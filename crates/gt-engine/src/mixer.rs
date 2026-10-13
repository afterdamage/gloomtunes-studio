//! The mixer on the audio thread: strips, effect slots, routing, meters and delay compensation
//! (ARCHITECTURE.md §7.1 and §7.7).
//!
//! Channels add their output into the `direct` buffer of the strip they are routed to. Each
//! quantum the strips are processed in the order computed on the UI side (every strip after all
//! strips that feed it, master last): input + direct → effect slots → fader, pan and polarity →
//! meters → output strip and send buses.

use std::sync::atomic::Ordering;

use gt_core::mixer::FIRST_SEND;
use gt_core::PluginInstanceId;
use gt_core::{EffectKind, EffectSlot, Mixer, FX_SLOTS, MASTER, SENDS, STRIPS};
use gt_dsp::fx::{
    Analyzer, Bitcrusher, Chorus, Compressor, ConvolutionReverb, Delay, Distortion, Effect,
    Flanger, FxContext, Gate, Limiter, MultiFilter, Multiband, ParamEq, Phaser, PitchShifter,
    Reverb, SoftClipper, StereoWidth, Stutter, TransientShaper, Vocoder,
};
use gt_dsp::LinearRamp;

use crate::plugin::{PluginContext, PluginTable};
use crate::{Telemetry, RENDER_QUANTUM};

const Q: usize = RENDER_QUANTUM;
/// Longest delay compensation per routing edge, in frames. Larger path differences are clamped
/// (the most latent built-in effect, the pitch shifter, needs 2048 frames). The lines are
/// allocated zeroed and an unused one is never written, so the memory of the many idle lines
/// is never touched.
pub const MAX_PDC_FRAMES: usize = 4096;
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
    create_effect_with(slot, sample_rate, None)
}

/// [`create_effect`] with the audio of the slot's file ([`EffectSlot::file`], loaded at
/// `sample_rate`), for effects that use one.
pub fn create_effect_with(
    slot: &EffectSlot,
    sample_rate: f32,
    file: Option<&gt_core::SampleData>,
) -> EffectBox {
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
        EffectKind::Filter => Box::new(MultiFilter::new(sr)),
        EffectKind::Gate => Box::new(Gate::new(sr)),
        EffectKind::Flanger => Box::new(Flanger::new(sr)),
        EffectKind::Phaser => Box::new(Phaser::new(sr)),
        EffectKind::Bitcrusher => Box::new(Bitcrusher::new(sr)),
        EffectKind::Clipper => Box::new(SoftClipper::new(sr)),
        EffectKind::Transient => Box::new(TransientShaper::new(sr)),
        EffectKind::Multiband => Box::new(Multiband::new(sr)),
        EffectKind::Analyzer => Box::new(Analyzer::new(sr)),
        EffectKind::Convolution => Box::new(ConvolutionReverb::new(sr)),
        EffectKind::PitchShift => Box::new(PitchShifter::new(sr)),
        EffectKind::Vocoder => Box::new(Vocoder::new(sr)),
        EffectKind::Stutter => Box::new(Stutter::new(sr)),
        // Plugins run from the plugin table (`EngineCommand::SetPlugin`), not from a box.
        EffectKind::Plugin => Box::new(Passthrough),
    };
    for (i, &v) in slot.params.iter().enumerate() {
        fx.set_param(i, v);
    }
    let channels = file.map(|f| f.channels.as_slice()).unwrap_or_default();
    let user = channels.first().map(|l| {
        let r = channels.get(1).unwrap_or(l);
        (l.as_slice(), r.as_slice())
    });
    fx.prepare(user);
    fx.reset();
    EffectBox(fx)
}

/// Stands in for an effect that runs elsewhere.
struct Passthrough;

impl Effect for Passthrough {
    fn param_count(&self) -> usize {
        0
    }
    fn set_param(&mut self, _index: usize, _value: f32) {}
    fn reset(&mut self) {}
    fn process(&mut self, _l: &mut [f32], _r: &mut [f32], _ctx: &FxContext) {}
}

/// Engine settings of one strip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StripParams {
    /// Fader gain.
    pub gain: f32,
    /// False when muted or silenced by a solo: the strip outputs nothing.
    pub audible: bool,
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
    /// Wet/dry mix of each slot (1 = only the effect).
    pub mix: [f32; FX_SLOTS],
    /// The plugin instance in each slot that holds a plugin.
    pub fx_plugin: [Option<PluginInstanceId>; FX_SLOTS],
}

impl Default for StripParams {
    fn default() -> Self {
        Self {
            gain: 1.0,
            audible: true,
            pan: 0.0,
            invert: false,
            output: MASTER as u8,
            sends: [0.0; SENDS],
            sidechain: None,
            enabled: [true; FX_SLOTS],
            mix: [1.0; FX_SLOTS],
            fx_plugin: [None; FX_SLOTS],
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
    /// Engine settings for a document mixer. Mute and solo clear `audible`. A mixer whose
    /// routing has a cycle (which editing prevents) falls back to everything into the master.
    pub fn from_mixer(m: &Mixer) -> Self {
        let audible = m.audible();
        let order = m.processing_order();
        let mut strips = [StripParams::default(); STRIPS];
        for (i, (p, s)) in strips.iter_mut().zip(&m.strips).enumerate() {
            *p = StripParams {
                gain: s.volume,
                audible: audible[i],
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
                mix: std::array::from_fn(|k| s.slots[k].as_ref().map_or(1.0, |x| x.mix)),
                fx_plugin: std::array::from_fn(|k| {
                    s.slots[k]
                        .as_ref()
                        .and_then(|x| x.plugin.as_ref())
                        .map(|p| p.instance)
                }),
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
pub(crate) struct PdcDelay {
    l: Box<[f32]>,
    r: Box<[f32]>,
    pos: usize,
    delay: usize,
}

impl PdcDelay {
    pub(crate) fn new() -> Self {
        Self {
            l: vec![0.0; MAX_PDC_FRAMES].into_boxed_slice(),
            r: vec![0.0; MAX_PDC_FRAMES].into_boxed_slice(),
            pos: 0,
            delay: 0,
        }
    }

    pub(crate) fn set_delay(&mut self, d: usize) {
        let d = d.min(MAX_PDC_FRAMES - 1);
        if d != self.delay {
            self.delay = d;
            // Only the `d` frames read next can hold stale audio.
            let n = self.l.len();
            for i in 0..d {
                let at = (self.pos + n - d + i) % n;
                self.l[at] = 0.0;
                self.r[at] = 0.0;
            }
        }
    }

    /// Delays the buffers in place by `delay` frames.
    pub(crate) fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
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
    /// Plugin table entry of the plugin in this slot (it takes the place of `effect`).
    plugin: Option<u8>,
    plugin_latency: usize,
    enabled: bool,
    /// 1 when the effect is heard, 0 when bypassed.
    wet: LinearRamp,
    /// The slot's wet/dry mix.
    mix: LinearRamp,
    /// The dry signal delayed by the effect's latency, so a partial mix lines up.
    dry_delay: PdcDelay,
}

impl FxSlot {
    fn latency(&self) -> usize {
        if self.plugin.is_some() {
            self.plugin_latency
        } else {
            self.effect.as_ref().map_or(0, |e| e.0.latency())
        }
    }

    /// True when the slot is heard: switched on with some of the effect in the mix.
    fn active(&self) -> bool {
        self.enabled && self.mix.target() > 0.0
    }
}

struct Strip {
    /// Signal from other strips (already delay-compensated).
    in_l: [f32; Q],
    in_r: [f32; Q],
    /// Signal from channels.
    direct_l: [f32; Q],
    direct_r: [f32; Q],
    /// Signal from plugin instruments, already aligned to `inst_latency` (D115).
    latent_l: [f32; Q],
    latent_r: [f32; Q],
    /// Largest latency of the plugin instruments playing into this strip; the direct signal
    /// is delayed by it so both line up.
    inst_latency: usize,
    pdc_inst: PdcDelay,
    /// Frozen track audio, joining at the strip's freeze point (see [`freeze_post_fx`]).
    frozen_l: [f32; Q],
    frozen_r: [f32; Q],
    has_input: bool,
    /// This quantum's post-fader output (kept for sidechain keys).
    out_l: [f32; Q],
    out_r: [f32; Q],
    fx: [FxSlot; FX_SLOTS],
    params: StripParams,
    /// Fader and balance set by automation; they replace `params` until the document's value
    /// for that strip changes.
    auto_gain: Option<f32>,
    auto_pan: Option<f32>,
    auto_sends: [Option<f32>; SENDS],
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
            latent_l: [0.0; Q],
            latent_r: [0.0; Q],
            inst_latency: 0,
            pdc_inst: PdcDelay::new(),
            frozen_l: [0.0; Q],
            frozen_r: [0.0; Q],
            has_input: false,
            out_l: [0.0; Q],
            out_r: [0.0; Q],
            fx: std::array::from_fn(|_| FxSlot {
                effect: None,
                plugin: None,
                plugin_latency: 0,
                enabled: true,
                wet: LinearRamp::new(1.0),
                mix: LinearRamp::new(1.0),
                dry_delay: PdcDelay::new(),
            }),
            params: StripParams::default(),
            auto_gain: None,
            auto_pan: None,
            auto_sends: [None; SENDS],
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

    /// Points the fader and balance ramps at the current settings.
    fn retarget(&mut self, frames: u32) {
        let p = &self.params;
        let gain = if p.audible {
            self.auto_gain.unwrap_or(p.gain)
        } else {
            0.0
        };
        let (l, r) = balance(gain, self.auto_pan.unwrap_or(p.pan), p.invert);
        self.gain_l.set_target(l, frames);
        self.gain_r.set_target(r, frames);
    }

    /// True if the strip must run this quantum: it has input, an effect that may have a tail,
    /// or delay lines still holding audio.
    fn needs_processing(&self) -> bool {
        self.has_input
            || self
                .fx
                .iter()
                .any(|f| f.effect.is_some() || f.plugin.is_some())
            || self.pdc_direct.delay > 0
            || self.pdc_inst.delay > 0
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
    /// Offline renders: the strip and point whose signal replaces the master output.
    tap: Option<(usize, Tap)>,
    tap_l: [f32; Q],
    tap_r: [f32; Q],
}

/// Where an offline render listens to a strip instead of the master output (render to
/// audio, consolidate and freeze, D105).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tap {
    /// What reaches the strip, before its effects.
    Input,
    /// The strip's freeze point: after the effects of an insert, before the master's effects
    /// (those belong to the whole mix). Frozen audio joins here.
    Freeze,
}

/// True when frozen audio on `strip` joins after the strip's effects (every insert) rather
/// than before them (the master).
pub fn freeze_post_fx(strip: usize) -> bool {
    strip != MASTER
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
            tap: None,
            tap_l: [0.0; Q],
            tap_r: [0.0; Q],
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
            // A document edit of an automated value takes over from the automation.
            if sp.gain != s.params.gain {
                s.auto_gain = None;
            }
            if sp.pan != s.params.pan {
                s.auto_pan = None;
            }
            for (k, auto) in s.auto_sends.iter_mut().enumerate() {
                if sp.sends[k] != s.params.sends[k] {
                    *auto = None;
                }
            }
            s.params = *sp;
            s.retarget(ramp);
            for ((ramp_k, &level), auto) in s.sends.iter_mut().zip(&sp.sends).zip(&s.auto_sends) {
                ramp_k.set_target(auto.unwrap_or(level).clamp(0.0, 1.0), ramp);
            }
            for ((slot, &on), &mix) in s.fx.iter_mut().zip(&sp.enabled).zip(&sp.mix) {
                let latent = slot.latency() > 0;
                if slot.enabled != on {
                    slot.enabled = on;
                    // A latent effect switches at once: crossfading would mix two time-shifted
                    // copies of the signal.
                    slot.wet
                        .set_target(if on { 1.0 } else { 0.0 }, if latent { 0 } else { bypass });
                }
                let mix = if mix.is_finite() {
                    mix.clamp(0.0, 1.0)
                } else {
                    1.0
                };
                if mix != slot.mix.target() {
                    // Turning a latent slot's mix to or from zero changes the strip's latency,
                    // so that step happens at once too; the dry path is aligned otherwise.
                    let jump = latent && (mix == 0.0 || slot.mix.target() == 0.0);
                    slot.mix.set_target(mix, if jump { 0 } else { ramp });
                }
            }
        }
        self.update_latency();
    }

    /// Sets a strip's fader gain and balance from automation or modulation (`None` leaves
    /// that one alone); the fader ramps to it over `frames`.
    pub(crate) fn automate(
        &mut self,
        strip: usize,
        gain: Option<f32>,
        pan: Option<f32>,
        frames: u32,
    ) {
        let Some(s) = self.strips.get_mut(strip) else {
            return;
        };
        let changed = gain.is_some_and(|g| s.auto_gain != Some(g))
            || pan.is_some_and(|p| s.auto_pan != Some(p));
        if changed {
            if gain.is_some() {
                s.auto_gain = gain;
            }
            if pan.is_some() {
                s.auto_pan = pan;
            }
            s.retarget(frames);
        }
    }

    /// Sets a send level from automation or modulation; it ramps there over `frames`.
    pub(crate) fn automate_send(&mut self, strip: usize, send: usize, level: f32, frames: u32) {
        let Some(s) = self.strips.get_mut(strip) else {
            return;
        };
        let (Some(auto), Some(ramp)) = (s.auto_sends.get_mut(send), s.sends.get_mut(send)) else {
            return;
        };
        if *auto != Some(level) {
            *auto = Some(level);
            ramp.set_target(level.clamp(0.0, 1.0), frames);
        }
    }

    /// Drops all automation overrides of faders, balances and sends.
    pub(crate) fn clear_automation(&mut self) {
        let ramp = self.ramp_frames;
        for s in &mut self.strips {
            if s.auto_gain.is_some() || s.auto_pan.is_some() {
                s.auto_gain = None;
                s.auto_pan = None;
                s.retarget(ramp);
            }
            for k in 0..SENDS {
                if s.auto_sends[k].take().is_some() {
                    s.sends[k].set_target(s.params.sends[k].clamp(0.0, 1.0), ramp);
                }
            }
        }
    }

    /// Peak level of a strip's output in the last quantum (post-fader).
    pub(crate) fn strip_peak(&self, strip: usize) -> f32 {
        self.strips.get(strip).map_or(0.0, |s| {
            s.out_l
                .iter()
                .chain(&s.out_r)
                .fold(0.0_f32, |m, x| m.max(x.abs()))
        })
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

    /// Sets one effect parameter. Returns true if that changed the effect's latency (the gate's
    /// look-ahead), in which case delay compensation has been recomputed.
    pub(crate) fn set_effect_param(
        &mut self,
        strip: usize,
        slot: usize,
        index: usize,
        value: f32,
    ) -> bool {
        let Some(e) = self
            .strips
            .get_mut(strip)
            .and_then(|s| s.fx.get_mut(slot))
            .and_then(|f| f.effect.as_mut())
        else {
            return false;
        };
        let before = e.0.latency();
        e.0.set_param(index, value);
        if e.0.latency() == before {
            return false;
        }
        self.update_latency();
        true
    }

    /// Points every slot at the plugin table entry running its plugin (after the mixer
    /// settings or the table changed) and recomputes delay compensation.
    pub(crate) fn resolve_plugins(&mut self, table: &PluginTable) {
        for s in &mut self.strips {
            for (k, f) in s.fx.iter_mut().enumerate() {
                let index = s.params.fx_plugin[k].and_then(|id| table.find(id));
                if index != f.plugin {
                    f.plugin = index;
                    f.wet = LinearRamp::new(if f.enabled { 1.0 } else { 0.0 });
                }
                f.plugin_latency = table.latency(index);
            }
        }
        self.update_latency();
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

    /// The frozen-audio input of strip `strip` (the master if out of range).
    pub(crate) fn frozen_mut(&mut self, strip: usize) -> (&mut [f32; Q], &mut [f32; Q]) {
        let s = if strip < STRIPS { strip } else { MASTER };
        let st = &mut self.strips[s];
        st.has_input = true;
        (&mut st.frozen_l, &mut st.frozen_r)
    }

    /// Delay compensation applied to what channels send into `strip`, in frames.
    pub(crate) fn input_delay(&self, strip: usize) -> usize {
        self.strips
            .get(strip)
            .map_or(0, |s| s.pdc_direct.delay + s.pdc_inst.delay)
    }

    /// The input of strip `strip` for a plugin instrument whose signal is already delayed to
    /// the strip's instrument latency (see [`Self::set_instrument_latency`]).
    pub(crate) fn latent_mut(&mut self, strip: usize) -> (&mut [f32; Q], &mut [f32; Q]) {
        let s = if strip < STRIPS { strip } else { MASTER };
        let st = &mut self.strips[s];
        st.has_input = true;
        (&mut st.latent_l, &mut st.latent_r)
    }

    /// The instrument latency of strip `strip` (0 if out of range).
    pub(crate) fn instrument_latency(&self, strip: usize) -> usize {
        self.strips.get(strip).map_or(0, |s| s.inst_latency)
    }

    /// Sets each strip's instrument latency (the largest latency of the plugin instruments
    /// routed to it) and recomputes delay compensation when it changed (true then).
    pub(crate) fn set_instrument_latency(&mut self, lat: &[usize; STRIPS]) -> bool {
        let lat = lat.map(|l| l.min(MAX_PDC_FRAMES - 1));
        let changed = self
            .strips
            .iter()
            .zip(&lat)
            .any(|(s, l)| s.inst_latency != *l);
        if changed {
            for (s, l) in self.strips.iter_mut().zip(lat) {
                s.inst_latency = l;
            }
            self.update_latency();
        }
        changed
    }

    /// Makes [`Self::process`] output a strip's signal instead of the master (offline only).
    pub(crate) fn set_tap(&mut self, tap: Option<(usize, Tap)>) {
        self.tap = tap.map(|(s, t)| (s.min(STRIPS - 1), t));
    }

    /// Computes delay compensation: each strip's input latency is the largest output latency
    /// of anything feeding it, and every edge is delayed by the difference, so all paths into
    /// a strip arrive aligned.
    fn update_latency(&mut self) {
        // A strip's input is late by its instruments' latency at least.
        let mut in_lat: [usize; STRIPS] = std::array::from_fn(|i| self.strips[i].inst_latency);
        let mut out_lat = [0_usize; STRIPS];
        for s in &mut self.strips {
            s.fx_latency =
                s.fx.iter()
                    .filter(|f| f.active())
                    .map(FxSlot::latency)
                    .sum();
            for f in &mut s.fx {
                let d = f.latency();
                f.dry_delay.set_delay(d);
            }
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
            s.pdc_inst.set_delay(s.inst_latency);
            s.pdc_direct.set_delay(d - s.inst_latency);
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
        plugins: &mut PluginTable,
        ctx: &PluginContext,
        telemetry: &Telemetry,
    ) {
        for idx in 0..STRIPS {
            let s = usize::from(self.order[idx]).min(STRIPS - 1);
            self.process_strip(s, bpm, plugins, ctx, telemetry);
            let st = &mut self.strips[s];
            st.has_input = false;
            st.direct_l.fill(0.0);
            st.direct_r.fill(0.0);
            st.latent_l.fill(0.0);
            st.latent_r.fill(0.0);
            st.frozen_l.fill(0.0);
            st.frozen_r.fill(0.0);
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
        if self.tap.is_some() {
            *out_l = self.tap_l;
            *out_r = self.tap_r;
            return;
        }
        let m = &self.strips[MASTER];
        *out_l = m.out_l;
        *out_r = m.out_r;
    }

    fn process_strip(
        &mut self,
        s: usize,
        bpm: f32,
        plugins: &mut PluginTable,
        pctx: &PluginContext,
        telemetry: &Telemetry,
    ) {
        let tap = match self.tap {
            Some((t, point)) if t == s => {
                self.tap_l = [0.0; Q];
                self.tap_r = [0.0; Q];
                Some(point)
            }
            _ => None,
        };
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
        st.pdc_inst.process(&mut l, &mut r);
        add(&mut l, &st.latent_l);
        add(&mut r, &st.latent_r);
        st.pdc_direct.process(&mut l, &mut r);
        add(&mut l, &st.in_l);
        add(&mut r, &st.in_r);
        if tap == Some(Tap::Input) {
            self.tap_l = l;
            self.tap_r = r;
        }
        let post = freeze_post_fx(s);
        if !post {
            add(&mut l, &st.frozen_l);
            add(&mut r, &st.frozen_r);
            if tap == Some(Tap::Freeze) {
                self.tap_l = l;
                self.tap_r = r;
            }
        }
        let ctx = FxContext {
            bpm,
            sidechain: has_key.then_some((&self.key_l[..], &self.key_r[..])),
            beats: pctx.playing.then_some(pctx.beats),
        };
        for (k, slot) in st.fx.iter_mut().enumerate() {
            if slot.effect.is_none() && slot.plugin.is_none() {
                continue;
            }
            let wet_settled = slot.wet.is_settled();
            if wet_settled && slot.wet.value() == 0.0 {
                continue;
            }
            // A slot mixed fully dry is skipped exactly like a bypassed one.
            let mix_settled = slot.mix.is_settled();
            if mix_settled && slot.mix.value() == 0.0 {
                continue;
            }
            let blend = !wet_settled || !mix_settled || slot.mix.value() < 1.0;
            // A latent slot's dry line runs all the time, so it is full when a blend starts.
            if blend || slot.latency() > 0 {
                self.dry_l = l;
                self.dry_r = r;
                slot.dry_delay.process(&mut self.dry_l, &mut self.dry_r);
            }
            let mut meter = 0.0;
            if slot.plugin.is_some() {
                plugins.run_effect(slot.plugin, &mut l, &mut r, pctx, telemetry);
            } else if let Some(fx) = &mut slot.effect {
                fx.0.process(&mut l, &mut r, &ctx);
                meter = fx.0.meter();
            }
            if blend {
                for i in 0..Q {
                    let w = slot.wet.next_value() * slot.mix.next_value();
                    l[i] = self.dry_l[i] + (l[i] - self.dry_l[i]) * w;
                    r[i] = self.dry_r[i] + (r[i] - self.dry_r[i]) * w;
                }
            }
            telemetry.fx_meters[s][k].store(meter, Ordering::Relaxed);
        }
        if post {
            add(&mut l, &st.frozen_l);
            add(&mut r, &st.frozen_r);
            if tap == Some(Tap::Freeze) {
                self.tap_l = l;
                self.tap_r = r;
            }
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
                beats: None,
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
