//! The mixer on the audio thread: strips, effect slots, routing, meters and delay compensation
//! (ARCHITECTURE.md §7.1 and §7.7).
//!
//! Channels add their output into the `direct` buffer of the strip they are routed to. Each
//! quantum the strips are processed in the order computed on the UI side (every strip after all
//! strips that feed it, master last): input + direct → effect slots → fader, pan and polarity →
//! meters → output strip and sends. Pre-fader sends leave after the effects, before the fader.
//!
//! The strip count follows the document (Prompt 29): strips beyond the engine's capacity
//! arrive built in [`EngineCommand::GrowMixer`](crate::EngineCommand::GrowMixer), which
//! [`crate::EngineHandle::send`] queues ahead of the first command that needs them, and are
//! never freed on the audio thread.

use std::sync::atomic::Ordering;

use gt_core::PluginInstanceId;
use gt_core::{EffectKind, EffectSlot, Mixer, PanLaw, FX_SLOTS, MASTER, MAX_SENDS, STRIPS};
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

/// Send slots per strip on the audio thread: twice the document's limit, so a send that was
/// replaced can fade out while its replacement fades in.
const SEND_SLOTS: usize = 2 * MAX_SENDS;

/// One send of a strip.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SendParams {
    /// Strip it feeds.
    pub to: u16,
    /// Level, 0 to 1.
    pub level: f32,
    /// Before the fader.
    pub pre: bool,
}

/// Engine settings of one strip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StripParams {
    /// Fader gain.
    pub gain: f32,
    /// False when muted or silenced by a solo: the strip outputs nothing.
    pub audible: bool,
    /// Pan, -1 to 1, split by the mixer's pan law.
    pub pan: f32,
    /// Flip polarity.
    pub invert: bool,
    /// Strip this one feeds (ignored on the master).
    pub output: u16,
    /// Sends; the first `send_count` are used.
    pub sends: [SendParams; MAX_SENDS],
    /// Number of sends.
    pub send_count: u8,
    /// Strip whose output is the sidechain key.
    pub sidechain: Option<u16>,
    /// Effect slots that are switched on.
    pub enabled: [bool; FX_SLOTS],
    /// Wet/dry mix of each slot (1 = only the effect).
    pub mix: [f32; FX_SLOTS],
    /// The plugin instance in each slot that holds a plugin.
    pub fx_plugin: [Option<PluginInstanceId>; FX_SLOTS],
}

impl StripParams {
    /// The used sends.
    pub fn sends(&self) -> &[SendParams] {
        &self.sends[..usize::from(self.send_count).min(MAX_SENDS)]
    }
}

impl Default for StripParams {
    fn default() -> Self {
        Self {
            gain: 1.0,
            audible: true,
            pan: 0.0,
            invert: false,
            output: MASTER as u16,
            sends: [SendParams::default(); MAX_SENDS],
            send_count: 0,
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
    /// By strip index, 2 to [`STRIPS`].
    pub strips: Vec<StripParams>,
    /// Processing order of every strip: each after the strips that feed it, master last.
    pub order: Vec<u16>,
    /// How pan knobs split the signal.
    pub pan_law: PanLaw,
}

/// Mixer strips built on the UI thread for an engine that needs more.
pub struct NewStrips(pub(crate) Vec<Box<Strip>>);

impl NewStrips {
    /// `n` new strips.
    pub(crate) fn new(n: usize) -> Self {
        Self((0..n).map(|_| Box::new(Strip::new())).collect())
    }
}

impl std::fmt::Debug for NewStrips {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NewStrips({})", self.0.len())
    }
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
        let n = m.strips.len().min(STRIPS);
        let audible = m.audible();
        let order = m.processing_order();
        let ok = order.is_some();
        let mut strips = vec![StripParams::default(); n];
        for (i, (p, s)) in strips.iter_mut().zip(&m.strips).enumerate() {
            let mut sends = [SendParams::default(); MAX_SENDS];
            let mut send_count = 0;
            if ok && i != MASTER {
                for (d, x) in sends.iter_mut().zip(s.sends.iter().filter(|x| x.to < n)) {
                    *d = SendParams {
                        to: x.to as u16,
                        level: x.level,
                        pre: x.pre,
                    };
                    send_count += 1;
                }
            }
            *p = StripParams {
                gain: s.volume,
                audible: audible[i],
                pan: s.pan.clamp(-1.0, 1.0),
                invert: s.phase_invert,
                output: if ok && s.output < n {
                    s.output as u16
                } else {
                    MASTER as u16
                },
                sends,
                send_count,
                sidechain: s.sidechain.filter(|&x| ok && x < n).map(|x| x as u16),
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
        let order = match order {
            Some(o) => o.into_iter().map(|s| s as u16).collect(),
            // Everything into the master: 1, 2, ..., n - 1, master.
            None => (1..n).chain([MASTER]).map(|s| s as u16).collect(),
        };
        Self {
            strips,
            order,
            pan_law: m.pan_law,
        }
    }
}

/// Fader gain, pan and polarity to left and right gains.
fn balance(law: PanLaw, gain: f32, pan: f32, invert: bool) -> (f32, f32) {
    let g = if invert { -gain } else { gain };
    let (l, r) = law.gains(pan);
    (g * l, g * r)
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

    /// Silences the line.
    pub(crate) fn clear(&mut self) {
        self.l.fill(0.0);
        self.r.fill(0.0);
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

/// One send on the audio thread. A slot keeps its target while it fades out.
struct SendSlot {
    /// Strip and pre-fader flag; `None` when the slot is free.
    to: Option<(u16, bool)>,
    /// Document level.
    level: f32,
    /// Level set by automation, until the document's level changes.
    auto: Option<f32>,
    ramp: LinearRamp,
    pdc: PdcDelay,
}

impl SendSlot {
    fn new() -> Self {
        Self {
            to: None,
            level: 0.0,
            auto: None,
            ramp: LinearRamp::new(0.0),
            pdc: PdcDelay::new(),
        }
    }

    fn active(&self) -> bool {
        self.to.is_some() && (self.ramp.target() > 0.0 || !self.ramp.is_settled())
    }
}

/// A strip on the audio thread (built on the UI thread).
pub(crate) struct Strip {
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
    /// This quantum's signal after the effects, before the fader (for pre-fader sends).
    pre_l: [f32; Q],
    pre_r: [f32; Q],
    /// 1 while the strip is heard, for pre-fader sends.
    on: LinearRamp,
    /// The output the strip fed before its output last changed, and the crossfade to the new
    /// one (0 to 1).
    old_output: Option<u16>,
    xfade: LinearRamp,
    fx: [FxSlot; FX_SLOTS],
    params: StripParams,
    /// Fader and balance set by automation; they replace `params` until the document's value
    /// for that strip changes.
    auto_gain: Option<f32>,
    auto_pan: Option<f32>,
    gain_l: LinearRamp,
    gain_r: LinearRamp,
    sends: [SendSlot; SEND_SLOTS],
    pdc_direct: PdcDelay,
    pdc_out: PdcDelay,
    /// Delays the sidechain key to line up with this strip's input.
    pdc_key: PdcDelay,
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
            pre_l: [0.0; Q],
            pre_r: [0.0; Q],
            on: LinearRamp::new(1.0),
            old_output: None,
            xfade: LinearRamp::new(1.0),
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
            gain_l: LinearRamp::new(1.0),
            gain_r: LinearRamp::new(1.0),
            sends: std::array::from_fn(|_| SendSlot::new()),
            pdc_direct: PdcDelay::new(),
            pdc_out: PdcDelay::new(),
            pdc_key: PdcDelay::new(),
            mean_square: [0.0; 2],
            fx_latency: 0,
        }
    }

    /// Points the fader and balance ramps at the current settings.
    fn retarget(&mut self, law: PanLaw, frames: u32) {
        let p = &self.params;
        let gain = if p.audible {
            self.auto_gain.unwrap_or(p.gain)
        } else {
            0.0
        };
        let (l, r) = balance(law, gain, self.auto_pan.unwrap_or(p.pan), p.invert);
        self.gain_l.set_target(l, frames);
        self.gain_r.set_target(r, frames);
        self.on
            .set_target(if p.audible { 1.0 } else { 0.0 }, frames);
    }

    /// Matches the send slots to the document's sends: a kept send glides to its new level, a
    /// new one fades in from silence in a free slot, a dropped one fades out and frees its
    /// slot once silent.
    fn retarget_sends(&mut self, frames: u32) {
        let p = self.params;
        let want = p.sends();
        for slot in &mut self.sends {
            if slot
                .to
                .is_some_and(|t| !want.iter().any(|w| (w.to, w.pre) == t))
            {
                slot.level = 0.0;
                slot.auto = None;
                slot.ramp.set_target(0.0, frames);
            }
        }
        for w in want {
            let key = (w.to, w.pre);
            let level = if w.level.is_finite() {
                w.level.clamp(0.0, 1.0)
            } else {
                0.0
            };
            let at = self
                .sends
                .iter()
                .position(|s| s.to == Some(key))
                .or_else(|| {
                    self.sends
                        .iter()
                        .position(|s| s.to.is_none())
                        .or_else(|| self.sends.iter().position(|s| !s.active()))
                });
            let Some(at) = at else {
                continue;
            };
            let slot = &mut self.sends[at];
            if slot.to != Some(key) {
                slot.to = Some(key);
                slot.ramp = LinearRamp::new(0.0);
                slot.auto = None;
                slot.level = f32::NAN;
            }
            if slot.level != level {
                slot.level = level;
                slot.auto = None;
            }
            slot.ramp.set_target(slot.auto.unwrap_or(level), frames);
        }
    }

    /// Frees send slots that have faded out.
    fn free_silent_sends(&mut self) {
        for slot in &mut self.sends {
            if slot.to.is_some() && slot.ramp.is_settled() && slot.ramp.target() == 0.0 {
                let kept = self
                    .params
                    .sends()
                    .iter()
                    .any(|w| Some((w.to, w.pre)) == slot.to);
                if !kept {
                    slot.to = None;
                }
            }
        }
    }

    /// Forgets the audio and automation it holds (a strip that comes back into use after
    /// inserts were removed). Its effects stay until the app replaces them.
    fn clear(&mut self) {
        for b in [
            &mut self.in_l,
            &mut self.in_r,
            &mut self.direct_l,
            &mut self.direct_r,
            &mut self.latent_l,
            &mut self.latent_r,
            &mut self.frozen_l,
            &mut self.frozen_r,
            &mut self.out_l,
            &mut self.out_r,
            &mut self.pre_l,
            &mut self.pre_r,
        ] {
            b.fill(0.0);
        }
        for d in [
            &mut self.pdc_inst,
            &mut self.pdc_direct,
            &mut self.pdc_out,
            &mut self.pdc_key,
        ] {
            d.clear();
        }
        for slot in &mut self.sends {
            slot.to = None;
            slot.auto = None;
            slot.level = 0.0;
            slot.ramp = LinearRamp::new(0.0);
            slot.pdc.clear();
        }
        for f in &mut self.fx {
            f.dry_delay.clear();
            if let Some(e) = &mut f.effect {
                e.0.reset();
            }
        }
        self.old_output = None;
        self.xfade = LinearRamp::new(1.0);
        self.auto_gain = None;
        self.auto_pan = None;
        self.has_input = false;
        self.mean_square = [0.0; 2];
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
            || self.sends.iter().any(|d| d.pdc.delay > 0)
            || self.mean_square[0] > 1e-12
            || self.mean_square[1] > 1e-12
    }
}

/// The mixer state on the audio thread. All buffers and delay lines are allocated in `new`.
pub(crate) struct MixerEngine {
    /// Every strip the engine has (reserved to [`STRIPS`]); the first `live` are in use.
    strips: Vec<Box<Strip>>,
    live: usize,
    order: Vec<u16>,
    pan_law: PanLaw,
    /// Scratch for [`Self::update_latency`].
    in_lat: Box<[usize; STRIPS]>,
    out_lat: Box<[usize; STRIPS]>,
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
        let mut strips = Vec::with_capacity(STRIPS);
        strips.extend(defaults.strips.iter().map(|_| Box::new(Strip::new())));
        let mut order = Vec::with_capacity(STRIPS);
        order.extend_from_slice(&defaults.order);
        Self {
            live: strips.len(),
            strips,
            order,
            pan_law: PanLaw::Zero,
            in_lat: Box::new([0; STRIPS]),
            out_lat: Box::new([0; STRIPS]),
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

    /// Number of strips the engine has, for [`MixerParams::build_missing`].
    pub(crate) fn capacity(&self) -> usize {
        self.strips.len()
    }

    /// Takes the strips out of `new` (they are kept, never freed here).
    pub(crate) fn grow(&mut self, new: &mut NewStrips) {
        for s in new.0.drain(..) {
            if self.strips.len() < STRIPS {
                self.strips.push(s);
            } else {
                // Cannot happen (the handle builds at most STRIPS); dropping would free here.
                std::mem::forget(s);
            }
        }
    }

    /// Applies new strip settings; gains and sends glide, a changed output crossfades.
    pub(crate) fn set_params(&mut self, p: &MixerParams) {
        let live = p.strips.len().min(self.strips.len());
        for s in &mut self.strips[self.live.min(live)..live] {
            s.clear();
        }
        self.live = live;
        self.order.clear();
        self.order.extend(
            p.order
                .iter()
                .copied()
                .filter(|&s| usize::from(s) < live)
                .take(STRIPS),
        );
        self.pan_law = p.pan_law;
        let (ramp, bypass) = (self.ramp_frames, self.bypass_frames);
        let law = self.pan_law;
        for (i, (s, sp)) in self.strips.iter_mut().zip(&p.strips).enumerate() {
            // A document edit of an automated value takes over from the automation.
            if sp.gain != s.params.gain {
                s.auto_gain = None;
            }
            if sp.pan != s.params.pan {
                s.auto_pan = None;
            }
            let mut sp = *sp;
            if usize::from(sp.output) >= live || usize::from(sp.output) == i {
                sp.output = MASTER as u16;
            }
            let n = usize::from(sp.send_count).min(MAX_SENDS);
            sp.send_count = sp.sends[..n]
                .iter()
                .take_while(|x| usize::from(x.to) < live && usize::from(x.to) != i)
                .count() as u8;
            let sounding = s.out_l.iter().chain(&s.out_r).any(|v| *v != 0.0);
            if i != MASTER && sp.output != s.params.output && sounding {
                // Crossfade from the old output to the new one (a silent strip just switches).
                s.old_output = Some(s.params.output).filter(|&o| usize::from(o) < live);
                s.xfade = LinearRamp::new(0.0);
                s.xfade.set_target(1.0, ramp);
            }
            s.params = sp;
            s.retarget(law, ramp);
            s.retarget_sends(ramp);
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
            s.retarget(self.pan_law, frames);
        }
    }

    /// Sets the level of the send from `strip` to `target` (pre- and post-fader alike) from
    /// automation or modulation; it ramps there over `frames`.
    pub(crate) fn automate_send(&mut self, strip: usize, target: usize, level: f32, frames: u32) {
        let Some(s) = self.strips.get_mut(strip) else {
            return;
        };
        let wanted = |t: (u16, bool)| {
            usize::from(t.0) == target && s.params.sends().iter().any(|w| (w.to, w.pre) == t)
        };
        for slot in &mut s.sends {
            if slot.to.is_some_and(wanted) && slot.auto != Some(level) {
                slot.auto = Some(level);
                slot.ramp.set_target(level.clamp(0.0, 1.0), frames);
            }
        }
    }

    /// Drops all automation overrides of faders, balances and sends.
    pub(crate) fn clear_automation(&mut self) {
        let ramp = self.ramp_frames;
        for s in &mut self.strips {
            if s.auto_gain.is_some() || s.auto_pan.is_some() {
                s.auto_gain = None;
                s.auto_pan = None;
                s.retarget(self.pan_law, ramp);
            }
            for slot in &mut s.sends {
                if slot.auto.take().is_some() {
                    slot.ramp.set_target(slot.level, ramp);
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

    /// Puts an effect into a slot (or empties it) and returns the previous one (or the new one,
    /// when there is no such slot, so it is not freed here).
    pub(crate) fn set_effect(
        &mut self,
        strip: usize,
        slot: usize,
        effect: Option<EffectBox>,
    ) -> Option<EffectBox> {
        let Some(f) = self.strips.get_mut(strip).and_then(|s| s.fx.get_mut(slot)) else {
            return effect;
        };
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
        let s = if strip < self.live { strip } else { MASTER };
        let st = &mut self.strips[s];
        st.has_input = true;
        (&mut st.direct_l, &mut st.direct_r)
    }

    /// The frozen-audio input of strip `strip` (the master if out of range).
    pub(crate) fn frozen_mut(&mut self, strip: usize) -> (&mut [f32; Q], &mut [f32; Q]) {
        let s = if strip < self.live { strip } else { MASTER };
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
        let s = if strip < self.live { strip } else { MASTER };
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
        let mut changed = false;
        for (s, &l) in self.strips.iter_mut().zip(lat) {
            let l = l.min(MAX_PDC_FRAMES - 1);
            changed |= s.inst_latency != l;
            s.inst_latency = l;
        }
        if changed {
            self.update_latency();
        }
        changed
    }

    /// Makes [`Self::process`] output a strip's signal instead of the master (offline only).
    pub(crate) fn set_tap(&mut self, tap: Option<(usize, Tap)>) {
        self.tap = tap.map(|(s, t)| (s.min(self.live - 1), t));
    }

    /// Computes delay compensation: each strip's input latency is the largest output latency
    /// of anything feeding it (outputs, sends and its sidechain key), and every edge is delayed
    /// by the difference, so all paths into a strip arrive aligned.
    fn update_latency(&mut self) {
        let live = self.live;
        let (in_lat, out_lat) = (&mut *self.in_lat, &mut *self.out_lat);
        for (i, s) in self.strips[..live].iter_mut().enumerate() {
            // A strip's input is late by its instruments' latency at least.
            in_lat[i] = s.inst_latency;
            out_lat[i] = 0;
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
            // The key comes from a strip processed earlier, so its latency is known.
            if let Some(k) = self.strips[s].params.sidechain.map(usize::from) {
                if k < live {
                    in_lat[s] = in_lat[s].max(out_lat[k]);
                }
            }
            out_lat[s] = in_lat[s] + self.strips[s].fx_latency;
            if s != MASTER {
                let st = &self.strips[s];
                let t = usize::from(st.params.output).min(live - 1);
                in_lat[t] = in_lat[t].max(out_lat[s]);
                for slot in &st.sends {
                    if let Some((to, _)) = slot.to {
                        let to = usize::from(to).min(live - 1);
                        in_lat[to] = in_lat[to].max(out_lat[s]);
                    }
                }
            }
        }
        for i in 0..live {
            let d = in_lat[i];
            let s = &mut self.strips[i];
            let t = usize::from(s.params.output).min(live - 1);
            s.pdc_inst.set_delay(s.inst_latency);
            s.pdc_direct.set_delay(d - s.inst_latency);
            s.pdc_out.set_delay(if i == MASTER {
                0
            } else {
                in_lat[t].saturating_sub(out_lat[i])
            });
            for slot in &mut s.sends {
                if let Some((to, _)) = slot.to {
                    let to = usize::from(to).min(live - 1);
                    slot.pdc.set_delay(in_lat[to].saturating_sub(out_lat[i]));
                }
            }
            let key = s.params.sidechain.map_or(0, |k| {
                let k = usize::from(k).min(live - 1);
                d.saturating_sub(out_lat[k])
            });
            s.pdc_key.set_delay(key);
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
        let live = self.live;
        for idx in 0..self.order.len() {
            let s = usize::from(self.order[idx]).min(live - 1);
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
            // Route: output edge (crossfading from the old one after a change), then sends.
            let target = usize::from(st.params.output).min(live - 1);
            let carries =
                st.out_l.iter().chain(&st.out_r).any(|v| *v != 0.0) || st.pdc_out.delay > 0;
            self.tmp_l = st.out_l;
            self.tmp_r = st.out_r;
            st.pdc_out.process(&mut self.tmp_l, &mut self.tmp_r);
            let old = st
                .old_output
                .map(usize::from)
                .filter(|&o| o < live && o != s && o != target);
            if let Some(o) = old {
                for i in 0..Q {
                    let x = st.xfade.next_value();
                    self.dry_l[i] = self.tmp_l[i] * (1.0 - x);
                    self.dry_r[i] = self.tmp_r[i] * (1.0 - x);
                    self.tmp_l[i] *= x;
                    self.tmp_r[i] *= x;
                }
                if st.xfade.is_settled() {
                    st.old_output = None;
                }
                let t = &mut self.strips[o];
                add(&mut t.in_l, &self.dry_l);
                add(&mut t.in_r, &self.dry_r);
                t.has_input |= carries;
            } else {
                st.old_output = None;
            }
            if target != s {
                let t = &mut self.strips[target];
                add(&mut t.in_l, &self.tmp_l);
                add(&mut t.in_r, &self.tmp_r);
                t.has_input |= carries;
            }
            for k in 0..SEND_SLOTS {
                let st = &mut self.strips[s];
                let slot = &mut st.sends[k];
                let Some((to, pre)) = slot.to.filter(|_| slot.active()) else {
                    continue;
                };
                let to = usize::from(to);
                if to >= live || to == s {
                    continue;
                }
                let (src_l, src_r) = if pre {
                    (&st.pre_l, &st.pre_r)
                } else {
                    (&st.out_l, &st.out_r)
                };
                for i in 0..Q {
                    let g = slot.ramp.next_value();
                    self.tmp_l[i] = src_l[i] * g;
                    self.tmp_r[i] = src_r[i] * g;
                }
                slot.pdc.process(&mut self.tmp_l, &mut self.tmp_r);
                let t = &mut self.strips[to];
                add(&mut t.in_l, &self.tmp_l);
                add(&mut t.in_r, &self.tmp_r);
                t.has_input = true;
            }
            self.strips[s].free_silent_sends();
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
            st.pre_l = [0.0; Q];
            st.pre_r = [0.0; Q];
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
            Some(k) if k < self.live && k != s => {
                self.key_l = self.strips[k].out_l;
                self.key_r = self.strips[k].out_r;
                true
            }
            _ => false,
        };
        let st = &mut self.strips[s];
        if has_key {
            st.pdc_key.process(&mut self.key_l, &mut self.key_r);
        }
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
        if st
            .sends
            .iter()
            .any(|x| x.active() && x.to.is_some_and(|t| t.1))
        {
            let sign = if st.params.invert { -1.0 } else { 1.0 };
            for i in 0..Q {
                let g = st.on.next_value() * sign;
                st.pre_l[i] = l[i] * g;
                st.pre_r[i] = r[i] * g;
            }
        } else {
            st.on.set_target(st.on.target(), 0);
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
        let z = PanLaw::Zero;
        assert_eq!(balance(z, 1.0, 0.0, false), (1.0, 1.0));
        assert_eq!(balance(z, 1.0, 1.0, false), (0.0, 1.0));
        assert_eq!(balance(z, 0.5, -0.5, true), (-0.5, -0.25));
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
