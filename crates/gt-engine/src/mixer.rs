//! The mixer on the audio thread: strips, effect slots, routing, meters and delay compensation
//! (ARCHITECTURE.md §7.1 and §7.7).
//!
//! Channels add their output into the `direct` buffer of the strip they are routed to. Each
//! quantum the strips are processed in the order computed on the UI side (every strip after all
//! strips that feed it, master last): input + direct → effect slots → fader, pan and polarity →
//! meters → output strip and send buses.

use std::sync::atomic::Ordering;

use gt_core::mixer::FIRST_SEND;
use gt_core::{EffectKind, EffectSlot, Mixer, FX_SLOTS, MASTER, SENDS, STRIPS};
use gt_dsp::fx::{
    Chorus, Compressor, Delay, Distortion, Effect, FxContext, Limiter, ParamEq, Reverb, StereoWidth,
};
use gt_dsp::LinearRamp;

use crate::{Telemetry, RENDER_QUANTUM};

const Q: usize = RENDER_QUANTUM;
/// Longest delay compensation per routing edge, in frames. Larger path differences are clamped
/// (the only built-in effect with latency, the limiter, needs 1.5 ms: 288 frames at 192 kHz).
pub const MAX_PDC_FRAMES: usize = 512;
/// Fader, pan and send changes glide over this time.
const RAMP_S: f32 = 0.01;
/// Effect bypass crossfade.
const BYPASS_S: f32 = 0.01;
/// RMS meter integration time.
const RMS_S: f32 = 0.3;

/// A boxed effect, created and configured on the UI thread with [`create_effect`].
pub struct EffectBox(pub Box<dyn Effect>);

impl std::fmt::Debug for EffectBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EffectBox({} params)", self.0.param_count())
    }
}

/// Builds the DSP for an effect slot at `sample_rate`, with the slot's parameter values applied
/// and smoothing settled. Allocates: call on the UI thread.
pub fn create_effect(slot: &EffectSlot, sample_rate: f32) -> EffectBox {
    let sr = sample_rate;
    let mut fx: Box<dyn Effect> = match slot.kind {
        EffectKind::Eq => Box::new(ParamEq::new(sr)),
        EffectKind::Compressor => Box::new(Compressor::new(sr)),
        EffectKind::Delay => Box::new(Delay::new(sr)),
        EffectKind::Reverb => Box::new(Reverb::new(sr)),
        EffectKind::Chorus => Box::new(Chorus::new(sr)),
        EffectKind::Distortion => Box::new(Distortion::new(sr)),
        EffectKind::Limiter => Box::new(Limiter::new(sr)),
        EffectKind::Width => Box::new(StereoWidth::new(sr)),
    };
    for (i, &v) in slot.params.iter().enumerate() {
        fx.set_param(i, v);
    }
    fx.reset();
    EffectBox(fx)
}

/// Engine settings of one strip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StripParams {
    /// Fader gain, 0 when muted or silenced by a solo.
    pub gain: f32,
    /// Balance, -1 to 1. Centre is unity on both sides; turning right lowers the left side.
    pub pan: f32,
    /// Flip polarity.
    pub invert: bool,
    /// Strip this one feeds (ignored on the master).
    pub output: u8,
    /// Post-fader send levels.
    pub sends: [f32; SENDS],
    /// Strip whose output is the sidechain key.
    pub sidechain: Option<u8>,
    /// Effect slots that are switched on.
    pub enabled: [bool; FX_SLOTS],
}

impl Default for StripParams {
    fn default() -> Self {
        Self {
            gain: 1.0,
            pan: 0.0,
            invert: false,
            output: MASTER as u8,
            sends: [0.0; SENDS],
            sidechain: None,
            enabled: [true; FX_SLOTS],
        }
    }
}

/// Engine settings of the whole mixer. Sent boxed with `EngineCommand::SetMixer`.
#[derive(Debug, Clone, PartialEq)]
pub struct MixerParams {
    /// By strip index.
    pub strips: [StripParams; STRIPS],
    /// Processing order: every strip after the strips that feed it, master last.
    pub order: [u8; STRIPS],
}

impl Default for MixerParams {
    fn default() -> Self {
        Self::from_mixer(&Mixer::new())
    }
}

impl MixerParams {
    /// Engine settings for a document mixer. Mute and solo become a zero gain. A mixer whose
    /// routing has a cycle (which editing prevents) falls back to everything into the master.
    pub fn from_mixer(m: &Mixer) -> Self {
        let audible = m.audible();
        let order = m.processing_order();
        let mut strips = [StripParams::default(); STRIPS];
        for (i, (p, s)) in strips.iter_mut().zip(&m.strips).enumerate() {
            *p = StripParams {
                gain: if audible[i] { s.volume } else { 0.0 },
                pan: s.pan.clamp(-1.0, 1.0),
                invert: s.phase_invert,
                output: if order.is_some() && s.output < STRIPS {
                    s.output as u8
                } else {
                    MASTER as u8
                },
                sends: s.sends,
                sidechain: s.sidechain.filter(|_| order.is_some()).map(|x| x as u8),
                enabled: std::array::from_fn(|k| s.slots[k].as_ref().is_some_and(|x| x.enabled)),
            };
        }
        let mut out = [0_u8; STRIPS];
        match order {
            Some(o) => {
                for (d, s) in out.iter_mut().zip(o) {
                    *d = s as u8;
                }
            }
            None => {
                for (i, d) in out.iter_mut().enumerate() {
                    *d = ((i + 1) % STRIPS) as u8; // 1, 2, ..., 68, master
                }
                for p in &mut strips {
                    p.sends = [0.0; SENDS];
                }
            }
        }
        Self { strips, order: out }
    }
}

/// Balance law: centre leaves both sides at unity.
fn balance(gain: f32, pan: f32, invert: bool) -> (f32, f32) {
    let g = if invert { -gain } else { gain };
    let p = pan.clamp(-1.0, 1.0);
    (g * (1.0 - p.max(0.0)), g * (1.0 + p.min(0.0)))
}

/// A stereo delay line for delay compensation.
struct PdcDelay {
    l: Box<[f32]>,
    r: Box<[f32]>,
    pos: usize,
    delay: usize,
}

impl PdcDelay {
    fn new() -> Self {
        Self {
            l: vec![0.0; MAX_PDC_FRAMES].into_boxed_slice(),
            r: vec![0.0; MAX_PDC_FRAMES].into_boxed_slice(),
            pos: 0,
            delay: 0,
        }
    }

    fn set_delay(&mut self, d: usize) {
        let d = d.min(MAX_PDC_FRAMES - 1);
        if d != self.delay {
            self.delay = d;
            self.l.fill(0.0);
            self.r.fill(0.0);
        }
    }

    /// Delays the buffers in place by `delay` frames.
    fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if self.delay == 0 {
            return;
        }
        let n = self.l.len();
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let read = (self.pos + n - self.delay) % n;
            self.l[self.pos] = *x;
            self.r[self.pos] = *y;
            *x = self.l[read];
            *y = self.r[read];
            self.pos = (self.pos + 1) % n;
        }
    }
}

struct FxSlot {
    effect: Option<EffectBox>,
    enabled: bool,
    /// 1 when the effect is heard, 0 when bypassed.
    wet: LinearRamp,
}

struct Strip {
    /// Signal from other strips (already delay-compensated).
    in_l: [f32; Q],
    in_r: [f32; Q],
    /// Signal from channels.
    direct_l: [f32; Q],
    direct_r: [f32; Q],
    has_input: bool,
    /// This quantum's post-fader output (kept for sidechain keys).
    out_l: [f32; Q],
    out_r: [f32; Q],
    fx: [FxSlot; FX_SLOTS],
    params: StripParams,
    gain_l: LinearRamp,
    gain_r: LinearRamp,
    sends: [LinearRamp; SENDS],
    pdc_direct: PdcDelay,
    pdc_out: PdcDelay,
    pdc_sends: [PdcDelay; SENDS],
    mean_square: [f32; 2],
    fx_latency: usize,
}

impl Strip {
    fn new() -> Self {
        Self {
            in_l: [0.0; Q],
            in_r: [0.0; Q],
            direct_l: [0.0; Q],
            direct_r: [0.0; Q],
            has_input: false,
            out_l: [0.0; Q],
            out_r: [0.0; Q],
            fx: std::array::from_fn(|_| FxSlot {
                effect: None,
                enabled: true,
                wet: LinearRamp::new(1.0),
            }),
            params: StripParams::default(),
            gain_l: LinearRamp::new(1.0),
            gain_r: LinearRamp::new(1.0),
            sends: std::array::from_fn(|_| LinearRamp::new(0.0)),
            pdc_direct: PdcDelay::new(),
            pdc_out: PdcDelay::new(),
            pdc_sends: std::array::from_fn(|_| PdcDelay::new()),
            mean_square: [0.0; 2],
            fx_latency: 0,
        }
    }

    /// True if the strip must run this quantum: it has input, an effect that may have a tail,
    /// or delay lines still holding audio.
    fn needs_processing(&self) -> bool {
        self.has_input
            || self.fx.iter().any(|f| f.effect.is_some())
            || self.pdc_direct.delay > 0
            || self.pdc_out.delay > 0
            || self.pdc_sends.iter().any(|d| d.delay > 0)
            || self.mean_square[0] > 1e-12
            || self.mean_square[1] > 1e-12
    }
}

/// The mixer state on the audio thread. All buffers and delay lines are allocated in `new`.
pub(crate) struct MixerEngine {
    strips: Vec<Strip>,
    order: [u8; STRIPS],
    ramp_frames: u32,
    bypass_frames: u32,
    rms_coef: f32,
    dry_l: [f32; Q],
    dry_r: [f32; Q],
    key_l: [f32; Q],
    key_r: [f32; Q],
    tmp_l: [f32; Q],
    tmp_r: [f32; Q],
    /// Total latency from a channel to the device, in frames.
    latency: usize,
}

impl MixerEngine {
    pub(crate) fn new(sample_rate: f32) -> Self {
        let defaults = MixerParams::default();
        Self {
            strips: (0..STRIPS).map(|_| Strip::new()).collect(),
            order: defaults.order,
            ramp_frames: (RAMP_S * sample_rate) as u32,
            bypass_frames: (BYPASS_S * sample_rate) as u32,
            rms_coef: 1.0 - (-(Q as f32) / (RMS_S * sample_rate)).exp(),
            dry_l: [0.0; Q],
            dry_r: [0.0; Q],
            key_l: [0.0; Q],
            key_r: [0.0; Q],
            tmp_l: [0.0; Q],
            tmp_r: [0.0; Q],
            latency: 0,
        }
    }

    /// Total latency to the device in frames.
    pub(crate) fn latency(&self) -> usize {
        self.latency
    }

    /// Applies new strip settings; gains and sends glide.
    pub(crate) fn set_params(&mut self, p: &MixerParams) {
        self.order = p.order;
        let (ramp, bypass) = (self.ramp_frames, self.bypass_frames);
        for (s, sp) in self.strips.iter_mut().zip(&p.strips) {
            s.params = *sp;
            let (l, r) = balance(sp.gain, sp.pan, sp.invert);
            s.gain_l.set_target(l, ramp);
            s.gain_r.set_target(r, ramp);
            for (ramp_k, &level) in s.sends.iter_mut().zip(&sp.sends) {
                ramp_k.set_target(level.clamp(0.0, 1.0), ramp);
            }
            for (slot, &on) in s.fx.iter_mut().zip(&sp.enabled) {
                if slot.enabled != on {
                    slot.enabled = on;
                    let latent = slot.effect.as_ref().is_some_and(|e| e.0.latency() > 0);
                    // A latent effect switches at once: crossfading would mix two time-shifted
                    // copies of the signal.
                    slot.wet
                        .set_target(if on { 1.0 } else { 0.0 }, if latent { 0 } else { bypass });
                }
            }
        }
        self.update_latency();
    }

    /// Puts an effect into a slot (or empties it) and returns the previous one.
    pub(crate) fn set_effect(
        &mut self,
        strip: usize,
        slot: usize,
        effect: Option<EffectBox>,
    ) -> Option<EffectBox> {
        let s = self.strips.get_mut(strip)?;
        let f = s.fx.get_mut(slot)?;
        let old = std::mem::replace(&mut f.effect, effect);
        f.wet = LinearRamp::new(if f.enabled { 1.0 } else { 0.0 });
        self.update_latency();
        old
    }

    /// Sets one effect parameter.
    pub(crate) fn set_effect_param(&mut self, strip: usize, slot: usize, index: usize, value: f32) {
        if let Some(e) = self
            .strips
            .get_mut(strip)
            .and_then(|s| s.fx.get_mut(slot))
            .and_then(|f| f.effect.as_mut())
        {
            e.0.set_param(index, value);
        }
    }

    /// Clears every effect's tail (on locate, so an echo of the old position does not linger).
    pub(crate) fn reset_effects(&mut self) {
        for s in &mut self.strips {
            for f in s.fx.iter_mut() {
                if let Some(e) = &mut f.effect {
                    e.0.reset();
                }
            }
        }
    }

    /// The `direct` input of strip `strip` (the master if out of range), for channels to add into.
    pub(crate) fn direct_mut(&mut self, strip: usize) -> (&mut [f32; Q], &mut [f32; Q]) {
        let s = if strip < STRIPS { strip } else { MASTER };
        let st = &mut self.strips[s];
        st.has_input = true;
        (&mut st.direct_l, &mut st.direct_r)
    }

    /// Computes delay compensation: each strip's input latency is the largest output latency
    /// of anything feeding it, and every edge is delayed by the difference, so all paths into
    /// a strip arrive aligned.
    fn update_latency(&mut self) {
        let mut in_lat = [0_usize; STRIPS];
        let mut out_lat = [0_usize; STRIPS];
        for s in &mut self.strips {
            s.fx_latency =
                s.fx.iter()
                    .filter(|f| f.enabled)
                    .filter_map(|f| f.effect.as_ref())
                    .map(|e| e.0.latency())
                    .sum();
        }
        for &s in &self.order {
            let s = usize::from(s);
            out_lat[s] = in_lat[s] + self.strips[s].fx_latency;
            if s != MASTER {
                let p = &self.strips[s].params;
                let t = usize::from(p.output).min(STRIPS - 1);
                in_lat[t] = in_lat[t].max(out_lat[s]);
                for (k, &level) in p.sends.iter().enumerate() {
                    if level > 0.0 {
                        in_lat[FIRST_SEND + k] = in_lat[FIRST_SEND + k].max(out_lat[s]);
                    }
                }
            }
        }
        for i in 0..STRIPS {
            let d = in_lat[i];
            let p = self.strips[i].params;
            let t = usize::from(p.output).min(STRIPS - 1);
            let out_delay = in_lat[t].saturating_sub(out_lat[i]);
            let send_delays: [usize; SENDS] =
                std::array::from_fn(|k| in_lat[FIRST_SEND + k].saturating_sub(out_lat[i]));
            let s = &mut self.strips[i];
            s.pdc_direct.set_delay(d);
            s.pdc_out.set_delay(if i == MASTER { 0 } else { out_delay });
            for (pd, dd) in s.pdc_sends.iter_mut().zip(send_delays) {
                pd.set_delay(dd);
            }
        }
        self.latency = out_lat[MASTER];
    }

    /// Runs every strip for one quantum and writes the master output into `out_l`/`out_r`.
    pub(crate) fn process(
        &mut self,
        out_l: &mut [f32; Q],
        out_r: &mut [f32; Q],
        bpm: f32,
        telemetry: &Telemetry,
    ) {
        for idx in 0..STRIPS {
            let s = usize::from(self.order[idx]).min(STRIPS - 1);
            self.process_strip(s, bpm, telemetry);
            let st = &mut self.strips[s];
            st.has_input = false;
            st.direct_l.fill(0.0);
            st.direct_r.fill(0.0);
            st.in_l.fill(0.0);
            st.in_r.fill(0.0);
            if s == MASTER {
                continue;
            }
            // Route: output edge, then sends (post-fader).
            let target = usize::from(st.params.output).min(STRIPS - 1);
            let carries =
                st.out_l.iter().chain(&st.out_r).any(|v| *v != 0.0) || st.pdc_out.delay > 0;
            self.tmp_l = st.out_l;
            self.tmp_r = st.out_r;
            st.pdc_out.process(&mut self.tmp_l, &mut self.tmp_r);
            let send_active: [bool; SENDS] =
                std::array::from_fn(|k| st.sends[k].target() > 0.0 || !st.sends[k].is_settled());
            if target != s {
                let t = &mut self.strips[target];
                add(&mut t.in_l, &self.tmp_l);
                add(&mut t.in_r, &self.tmp_r);
                t.has_input |= carries;
            }
            for (k, active) in send_active.into_iter().enumerate() {
                if !active || FIRST_SEND + k == s {
                    continue;
                }
                let st = &mut self.strips[s];
                for i in 0..Q {
                    let g = st.sends[k].next_value();
                    self.tmp_l[i] = st.out_l[i] * g;
                    self.tmp_r[i] = st.out_r[i] * g;
                }
                st.pdc_sends[k].process(&mut self.tmp_l, &mut self.tmp_r);
                let t = &mut self.strips[FIRST_SEND + k];
                add(&mut t.in_l, &self.tmp_l);
                add(&mut t.in_r, &self.tmp_r);
                t.has_input = true;
            }
        }
        let m = &self.strips[MASTER];
        *out_l = m.out_l;
        *out_r = m.out_r;
    }

    fn process_strip(&mut self, s: usize, bpm: f32, telemetry: &Telemetry) {
        if !self.strips[s].needs_processing() {
            let st = &mut self.strips[s];
            st.out_l = [0.0; Q];
            st.out_r = [0.0; Q];
            st.gain_l.set_target(st.gain_l.target(), 0);
            st.gain_r.set_target(st.gain_r.target(), 0);
            let cell = &telemetry.meters[s];
            cell.rms[0].store(0.0, Ordering::Relaxed);
            cell.rms[1].store(0.0, Ordering::Relaxed);
            return;
        }
        // Sidechain key: the source strip's output from this quantum (processed earlier).
        let key = self.strips[s].params.sidechain.map(usize::from);
        let has_key = match key {
            Some(k) if k < STRIPS && k != s => {
                self.key_l = self.strips[k].out_l;
                self.key_r = self.strips[k].out_r;
                true
            }
            _ => false,
        };
        let st = &mut self.strips[s];
        let (mut l, mut r) = (st.direct_l, st.direct_r);
        st.pdc_direct.process(&mut l, &mut r);
        add(&mut l, &st.in_l);
        add(&mut r, &st.in_r);
        let ctx = FxContext {
            bpm,
            sidechain: has_key.then_some((&self.key_l[..], &self.key_r[..])),
        };
        for (k, slot) in st.fx.iter_mut().enumerate() {
            let Some(fx) = &mut slot.effect else {
                continue;
            };
            let wet_settled = slot.wet.is_settled();
            if wet_settled && slot.wet.value() == 0.0 {
                continue;
            }
            if wet_settled {
                fx.0.process(&mut l, &mut r, &ctx);
            } else {
                self.dry_l = l;
                self.dry_r = r;
                fx.0.process(&mut l, &mut r, &ctx);
                for i in 0..Q {
                    let w = slot.wet.next_value();
                    l[i] = self.dry_l[i] + (l[i] - self.dry_l[i]) * w;
                    r[i] = self.dry_r[i] + (r[i] - self.dry_r[i]) * w;
                }
            }
            telemetry.fx_meters[s][k].store(fx.0.meter(), Ordering::Relaxed);
        }
        let (mut peak_l, mut peak_r, mut sq_l, mut sq_r) = (0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32);
        for i in 0..Q {
            let a = l[i] * st.gain_l.next_value();
            let b = r[i] * st.gain_r.next_value();
            // Guard the rest of the mix against a misbehaving effect.
            let a = if a.is_finite() { a } else { 0.0 };
            let b = if b.is_finite() { b } else { 0.0 };
            st.out_l[i] = a;
            st.out_r[i] = b;
            peak_l = peak_l.max(a.abs());
            peak_r = peak_r.max(b.abs());
            sq_l += a * a;
            sq_r += b * b;
        }
        let k = self.rms_coef;
        st.mean_square[0] += (sq_l / Q as f32 - st.mean_square[0]) * k;
        st.mean_square[1] += (sq_r / Q as f32 - st.mean_square[1]) * k;
        for m in &mut st.mean_square {
            if *m < 1e-14 {
                *m = 0.0;
            }
        }
        let cell = &telemetry.meters[s];
        cell.peak[0].fetch_max(peak_l, Ordering::Relaxed);
        cell.peak[1].fetch_max(peak_r, Ordering::Relaxed);
        cell.rms[0].store(st.mean_square[0].sqrt(), Ordering::Relaxed);
        cell.rms[1].store(st.mean_square[1].sqrt(), Ordering::Relaxed);
    }
}

#[inline]
fn add(dst: &mut [f32; Q], src: &[f32; Q]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d += s;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_keeps_unity_at_centre() {
        assert_eq!(balance(1.0, 0.0, false), (1.0, 1.0));
        assert_eq!(balance(1.0, 1.0, false), (0.0, 1.0));
        assert_eq!(balance(0.5, -0.5, true), (-0.5, -0.25));
    }

    /// The DSP and the document tables must agree on every effect's parameter count, and the
    /// defaults must produce finite output.
    #[test]
    fn effect_tables_match_the_dsp() {
        for kind in EffectKind::ALL {
            let slot = EffectSlot::new(kind);
            let mut fx = create_effect(&slot, 48_000.0);
            assert_eq!(fx.0.param_count(), kind.params().len(), "{}", kind.key());
            let mut l: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
            let mut r = l.clone();
            let ctx = FxContext {
                bpm: 120.0,
                sidechain: None,
            };
            for (a, b) in l.chunks_mut(64).zip(r.chunks_mut(64)) {
                fx.0.process(a, b, &ctx);
            }
            assert!(
                l.iter().chain(&r).all(|v| v.is_finite() && v.abs() < 4.0),
                "{}",
                kind.key()
            );
        }
    }
}
