//! Channel slots: preallocated voice pools that play a sample or Gloom Synth.

use std::sync::Arc;

use gt_core::params::ChannelParam;
use gt_core::synth::{SynthParam, MOD_SLOTS};
use gt_core::{SampleData, ROOT_KEY};
use gt_dsp::{AdsrParams, GloomSynth, LinearRamp, SamplerVoice, VoiceRegion};

use crate::song::{ChannelParams, InstrumentKind, PatchValues};

/// Voices per channel. When all are busy, the oldest is stolen.
pub const VOICES_PER_CHANNEL: usize = 16;
/// Gain/pan smoothing time in seconds.
const GAIN_RAMP_S: f32 = 0.01;

/// Values set by automation or modulation. `None` means "use the document's".
#[derive(Debug, Clone, Copy)]
struct Controls {
    channel: [Option<f32>; ChannelParam::ALL.len()],
    synth: [Option<f32>; SynthParam::COUNT],
    mods: [Option<f32>; MOD_SLOTS],
    /// The synth's settings must be rebuilt before the next render.
    synth_dirty: bool,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            channel: [None; ChannelParam::ALL.len()],
            synth: [None; SynthParam::COUNT],
            mods: [None; MOD_SLOTS],
            synth_dirty: false,
        }
    }
}

/// One channel of the rack on the audio thread. Everything is allocated at engine creation:
/// each slot holds both a sampler voice pool and a Gloom Synth, and plays whichever its
/// parameters select.
pub(crate) struct ChannelSlot {
    synth: Box<GloomSynth>,
    sr: f32,
    /// The document's settings.
    params: ChannelParams,
    /// Automation and modulation on top of `params`.
    ctl: Controls,
    env: AdsrParams,
    sample: Option<Arc<SampleData>>,
    voices: [SamplerVoice; VOICES_PER_CHANNEL],
    gain_l: LinearRamp,
    gain_r: LinearRamp,
    ramp_frames: u32,
    /// Plugin table entry running this channel's instrument plugin.
    pub(crate) plugin: Option<u8>,
}

/// Equal-power pan law: -3 dB per side at centre, so a sound keeps its loudness as it moves.
fn pan_gains(gain: f32, pan: f32) -> (f32, f32) {
    let theta = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (gain * theta.cos(), gain * theta.sin())
}

impl ChannelSlot {
    pub(crate) fn new(sample_rate: f32) -> Self {
        let params = ChannelParams::default();
        Self {
            synth: Box::new(GloomSynth::new(sample_rate)),
            sr: sample_rate,
            params,
            ctl: Controls::default(),
            env: Self::env_for(sample_rate, &params),
            sample: None,
            voices: [SamplerVoice::new(sample_rate); VOICES_PER_CHANNEL],
            gain_l: LinearRamp::new(0.0),
            gain_r: LinearRamp::new(0.0),
            ramp_frames: (sample_rate * GAIN_RAMP_S) as u32,
            plugin: None,
        }
    }

    /// The instrument plugin instance this channel plays, if it is a plugin channel.
    pub(crate) fn plugin_instance(&self) -> Option<gt_core::PluginInstanceId> {
        (self.params.kind == InstrumentKind::Plugin)
            .then_some(self.params.plugin)
            .flatten()
    }

    /// True for a plugin channel (its notes and audio go through the plugin table).
    pub(crate) fn is_plugin(&self) -> bool {
        self.params.kind == InstrumentKind::Plugin
    }

    fn env_for(sr: f32, p: &ChannelParams) -> AdsrParams {
        let a = p.adsr;
        AdsrParams::new(sr, a.attack_ms, a.decay_ms, a.sustain, a.release_ms)
    }

    /// The document settings with automation and modulation applied.
    fn effective(&self) -> ChannelParams {
        let mut p = self.params;
        let c = |k: ChannelParam| self.ctl.channel[k as usize];
        p.gain = c(ChannelParam::Volume).unwrap_or(p.gain);
        p.pan = c(ChannelParam::Pan).unwrap_or(p.pan);
        p.pitch = c(ChannelParam::Pitch).unwrap_or(p.pitch);
        p.start = c(ChannelParam::Start).unwrap_or(p.start);
        p.end = c(ChannelParam::End).unwrap_or(p.end).max(p.start);
        p.adsr.attack_ms = c(ChannelParam::Attack).unwrap_or(p.adsr.attack_ms);
        p.adsr.decay_ms = c(ChannelParam::Decay).unwrap_or(p.adsr.decay_ms);
        p.adsr.sustain = c(ChannelParam::Sustain).unwrap_or(p.adsr.sustain);
        p.adsr.release_ms = c(ChannelParam::Release).unwrap_or(p.adsr.release_ms);
        p
    }

    /// The synth patch with automation and modulation applied.
    fn effective_patch(&self) -> PatchValues {
        let mut v = self.params.patch;
        for (x, c) in v.values.iter_mut().zip(&self.ctl.synth) {
            if let Some(c) = c {
                *x = *c;
            }
        }
        for (m, c) in v.mods.iter_mut().zip(&self.ctl.mods) {
            if let Some(c) = c {
                m.amount = *c;
            }
        }
        v
    }

    /// Points the gain ramps at the effective gain and pan over `frames`.
    fn retarget(&mut self, frames: u32) {
        let p = self.effective();
        let gain = if p.silenced { 0.0 } else { p.gain };
        let (l, r) = pan_gains(gain, p.pan);
        self.gain_l.set_target(l, frames);
        self.gain_r.set_target(r, frames);
    }

    /// Applies new settings. Gain and pan glide over 10 ms; the rest applies to new notes.
    pub(crate) fn set_params(&mut self, p: ChannelParams) {
        if p.kind != self.params.kind {
            // The other instrument's voices would never get their note-offs.
            self.kill_all();
        }
        self.params = p;
        if p.kind == InstrumentKind::Synth {
            self.synth.set_settings(&self.effective_patch().settings());
            self.ctl.synth_dirty = false;
        }
        self.env = Self::env_for(self.sr, &self.effective());
        self.retarget(self.ramp_frames);
    }

    /// Like `set_params`, but jumps straight to the new gain (for a channel that was idle).
    pub(crate) fn set_params_now(&mut self, p: ChannelParams) {
        self.set_params(p);
        self.gain_l = LinearRamp::new(self.gain_l.target());
        self.gain_r = LinearRamp::new(self.gain_r.target());
    }

    /// Sets a channel or sampler parameter from automation or modulation. Volume and pan ramp
    /// to the new value over `frames`; pitch retunes sounding voices; the rest applies to new
    /// notes.
    pub(crate) fn control(&mut self, param: ChannelParam, value: f32, frames: u32) {
        let old = self.effective();
        let slot = &mut self.ctl.channel[param as usize];
        if *slot == Some(value) {
            return;
        }
        *slot = Some(value);
        match param {
            ChannelParam::Volume | ChannelParam::Pan => self.retarget(frames),
            ChannelParam::Pitch => {
                let ratio = f64::from(((value - old.pitch) / 12.0).exp2());
                for v in self.voices.iter_mut().filter(|v| v.is_active()) {
                    v.scale_step(ratio);
                }
            }
            ChannelParam::Attack
            | ChannelParam::Decay
            | ChannelParam::Sustain
            | ChannelParam::Release => {
                self.env = Self::env_for(self.sr, &self.effective());
            }
            ChannelParam::Start | ChannelParam::End => {}
        }
    }

    /// Sets a Gloom Synth knob (plain value, `SynthParam` index) from automation or
    /// modulation. Takes effect at the next `flush_controls`.
    pub(crate) fn control_synth(&mut self, index: usize, value: f32) {
        if let Some(slot) = self.ctl.synth.get_mut(index) {
            if *slot != Some(value) {
                *slot = Some(value);
                self.ctl.synth_dirty = true;
            }
        }
    }

    /// Sets the amount of a Gloom Synth modulation matrix row.
    pub(crate) fn control_synth_mod(&mut self, row: usize, value: f32) {
        if let Some(slot) = self.ctl.mods.get_mut(row) {
            if *slot != Some(value) {
                *slot = Some(value);
                self.ctl.synth_dirty = true;
            }
        }
    }

    /// Hands changed synth controls to the synth (once per quantum, however many changed).
    pub(crate) fn flush_controls(&mut self) {
        if self.ctl.synth_dirty {
            self.ctl.synth_dirty = false;
            if self.params.kind == InstrumentKind::Synth {
                self.synth.set_settings(&self.effective_patch().settings());
            }
        }
    }

    /// Drops every automation and modulation value; the document's settings apply again.
    pub(crate) fn clear_controls(&mut self) {
        let had = self.ctl.channel.iter().any(Option::is_some)
            || self.ctl.synth.iter().any(Option::is_some)
            || self.ctl.mods.iter().any(Option::is_some);
        if !had {
            return;
        }
        let pitch = self.effective().pitch;
        self.ctl = Controls::default();
        let ratio = f64::from(((self.params.pitch - pitch) / 12.0).exp2());
        for v in self.voices.iter_mut().filter(|v| v.is_active()) {
            v.scale_step(ratio);
        }
        let p = self.params;
        self.set_params(p);
    }

    /// Swaps the sample, returning the old one (for the garbage queue). Playing voices stop:
    /// they were reading the old data.
    pub(crate) fn set_sample(&mut self, s: Option<Arc<SampleData>>) -> Option<Arc<SampleData>> {
        self.kill_all();
        std::mem::replace(&mut self.sample, s)
    }

    /// Starts a note. `age` orders voices for stealing.
    pub(crate) fn note_on(&mut self, key: u8, velocity: f32, age: u64) {
        if self.params.kind == InstrumentKind::Plugin {
            return;
        }
        if self.params.kind == InstrumentKind::Synth {
            self.synth.note_on(key, velocity, age);
            return;
        }
        let Some(sample) = &self.sample else {
            return;
        };
        let frames = sample.frames();
        if frames == 0 {
            return;
        }
        let p = self.effective();
        let semis = p.pitch + f32::from(key) - f32::from(ROOT_KEY);
        let step =
            f64::from((semis / 12.0).exp2()) * f64::from(sample.sample_rate) / f64::from(self.sr);
        let region = VoiceRegion::from_fractions(frames, p.start, p.end, p.looped);
        let v = velocity.clamp(0.0, 1.0);
        // Velocity to amplitude: squared, a common curve that spreads soft hits more evenly
        // in loudness than a linear map.
        let gain = v * v;
        let idx = self
            .voices
            .iter()
            .position(|v| !v.is_active())
            .unwrap_or_else(|| {
                // Steal the oldest. A hard cut, acceptable for now (ROADMAP Step 3 limits).
                let mut oldest = 0;
                for (i, v) in self.voices.iter().enumerate() {
                    if v.age < self.voices[oldest].age {
                        oldest = i;
                    }
                }
                oldest
            });
        self.voices[idx].trigger(key, gain, step, region, self.env, age);
    }

    /// Releases held voices playing `key` (one-shot voices ignore it).
    pub(crate) fn note_off(&mut self, key: u8) {
        self.synth.note_off(key);
        for v in &mut self.voices {
            if v.is_held() && v.key() == key {
                v.release();
            }
        }
    }

    /// Releases every voice.
    pub(crate) fn release_all(&mut self) {
        self.synth.release_all();
        for v in &mut self.voices {
            v.release();
        }
    }

    /// Silences every voice at once.
    pub(crate) fn kill_all(&mut self) {
        self.synth.kill_all();
        for v in &mut self.voices {
            v.kill();
        }
    }

    /// Mixer strip this channel plays into.
    pub(crate) fn route(&self) -> u8 {
        self.params.route
    }

    /// True if any voice is sounding.
    pub(crate) fn is_active(&self) -> bool {
        self.voices.iter().any(SamplerVoice::is_active) || self.synth.is_active()
    }

    /// Adds the voices (dry, before channel gain) into the two buffers.
    pub(crate) fn render_voices(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        if self.synth.is_active() {
            self.synth.render(out_l, out_r);
        }
        let Some(sample) = &self.sample else {
            return;
        };
        let (l, r) = (sample.left(), sample.right());
        for v in &mut self.voices {
            if v.is_active() {
                v.render(l, r, out_l, out_r);
            }
        }
    }

    /// Applies the smoothed channel gain and pan to `dry`, adds the result into `mix`, and
    /// returns the channel's peak level.
    pub(crate) fn mix_into(&mut self, dry: (&[f32], &[f32]), mix: (&mut [f32], &mut [f32])) -> f32 {
        let mut peak = 0.0_f32;
        for (((&dl, &dr), ml), mr) in dry
            .0
            .iter()
            .zip(dry.1)
            .zip(mix.0.iter_mut())
            .zip(mix.1.iter_mut())
        {
            let l = dl * self.gain_l.next_value();
            let r = dr * self.gain_r.next_value();
            *ml += l;
            *mr += r;
            peak = peak.max(l.abs()).max(r.abs());
        }
        peak
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pan_law_is_equal_power() {
        for pan in [-1.0, -0.3, 0.0, 0.5, 1.0] {
            let (l, r) = pan_gains(1.0, pan);
            assert!((l * l + r * r - 1.0).abs() < 1e-6, "{pan}");
        }
        let (l, r) = pan_gains(1.0, 0.0);
        assert!((l - r).abs() < 1e-7 && (l - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        let (l, r) = pan_gains(1.0, -1.0);
        assert!((l - 1.0).abs() < 1e-6 && r.abs() < 1e-6);
    }

    #[test]
    fn oldest_voice_is_stolen() {
        let mut ch = ChannelSlot::new(48_000.0);
        ch.set_sample(Some(Arc::new(SampleData::mono(48_000, vec![0.5; 48_000]))));
        for age in 0..VOICES_PER_CHANNEL as u64 + 3 {
            ch.note_on(60, 1.0, age);
        }
        let mut ages: Vec<_> = ch.voices.iter().map(|v| v.age).collect();
        ages.sort_unstable();
        assert_eq!(ages, (3..VOICES_PER_CHANNEL as u64 + 3).collect::<Vec<_>>());
    }

    #[test]
    fn synth_channels_play_without_a_sample() {
        let mut ch = ChannelSlot::new(48_000.0);
        ch.set_params_now(ChannelParams {
            kind: InstrumentKind::Synth,
            gain: 1.0,
            ..ChannelParams::default()
        });
        ch.note_on(60, 1.0, 1);
        let (mut l, mut r) = (vec![0.0; 2048], vec![0.0; 2048]);
        ch.render_voices(&mut l, &mut r);
        assert!(l.iter().any(|x| x.abs() > 0.01));
        // Switching back to the sampler silences the synth's voices.
        ch.set_params(ChannelParams::default());
        assert!(!ch.is_active());
    }

    #[test]
    fn pitch_and_rate_set_the_read_step() {
        // A 24 kHz sample on a 48 kHz engine, transposed up an octave, reads 1 frame per frame.
        let mut ch = ChannelSlot::new(48_000.0);
        let data: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        ch.set_sample(Some(Arc::new(SampleData::mono(24_000, data.clone()))));
        ch.set_params_now(ChannelParams {
            gain: 1.0,
            pitch: 12.0,
            ..ChannelParams::default()
        });
        ch.note_on(60, 1.0, 0);
        let (mut l, mut r) = (vec![0.0; 100], vec![0.0; 100]);
        ch.render_voices(&mut l, &mut r);
        assert_eq!(l[50], data[50]);
    }

    #[test]
    fn automated_pitch_retunes_sounding_voices() {
        let mut ch = ChannelSlot::new(48_000.0);
        let data: Vec<f32> = (0..48_000).map(|i| i as f32 / 48_000.0).collect();
        ch.set_sample(Some(Arc::new(SampleData::mono(48_000, data))));
        ch.set_params_now(ChannelParams {
            gain: 1.0,
            ..ChannelParams::default()
        });
        ch.note_on(60, 1.0, 0);
        let slope = |ch: &mut ChannelSlot| {
            let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
            ch.render_voices(&mut l, &mut r);
            (l[40] - l[20]) / 20.0 * 48_000.0
        };
        assert!((slope(&mut ch) - 1.0).abs() < 1e-3);
        ch.control(ChannelParam::Pitch, 12.0, 32);
        assert!(
            (slope(&mut ch) - 2.0).abs() < 1e-3,
            "an octave up reads twice as fast"
        );
        ch.clear_controls();
        assert!(
            (slope(&mut ch) - 1.0).abs() < 1e-3,
            "back to the document's pitch"
        );
    }

    #[test]
    fn synth_controls_apply_once_per_flush_and_clear() {
        let mut ch = ChannelSlot::new(48_000.0);
        let mut p = ChannelParams {
            kind: InstrumentKind::Synth,
            gain: 1.0,
            ..ChannelParams::default()
        };
        p.patch.values[SynthParam::Cutoff as usize] = 1000.0;
        ch.set_params_now(p);
        ch.control_synth(SynthParam::Cutoff as usize, 300.0);
        assert_eq!(
            ch.synth.settings().cutoff_hz,
            1000.0,
            "not before the flush"
        );
        ch.flush_controls();
        assert_eq!(ch.synth.settings().cutoff_hz, 300.0);
        // A document edit keeps the automated value on top.
        p.patch.values[SynthParam::Resonance as usize] = 0.9;
        ch.set_params(p);
        assert_eq!(ch.synth.settings().cutoff_hz, 300.0);
        assert_eq!(ch.synth.settings().resonance, 0.9);
        ch.clear_controls();
        assert_eq!(ch.synth.settings().cutoff_hz, 1000.0);
    }

    #[test]
    fn volume_control_ramps_over_the_given_frames_and_respects_mute() {
        let mut ch = ChannelSlot::new(48_000.0);
        ch.set_params_now(ChannelParams {
            gain: 1.0,
            ..ChannelParams::default()
        });
        ch.control(ChannelParam::Volume, 0.0, 32);
        let ones = vec![1.0; 32];
        let (mut l, mut r) = (vec![0.0; 32], vec![0.0; 32]);
        ch.mix_into((&ones, &ones), (&mut l, &mut r));
        assert!(l[0] > 0.6 && l[31].abs() < 1e-6, "{} {}", l[0], l[31]);
        ch.control(ChannelParam::Volume, 1.0, 0);
        ch.set_params(ChannelParams {
            gain: 1.0,
            silenced: true,
            ..ChannelParams::default()
        });
        let (mut l, mut r) = (vec![0.0; 1024], vec![0.0; 1024]);
        let ones = vec![1.0; 1024];
        ch.mix_into((&ones, &ones), (&mut l, &mut r));
        assert_eq!(l[1000], 0.0, "muted wins over automation");
    }
}
