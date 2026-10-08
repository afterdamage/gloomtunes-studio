//! Channel slots: preallocated voice pools that play a sample or Gloom Synth.

use std::sync::Arc;

use gt_core::{SampleData, ROOT_KEY};
use gt_dsp::{AdsrParams, GloomSynth, LinearRamp, SamplerVoice, VoiceRegion};

use crate::song::{ChannelParams, InstrumentKind};

/// Voices per channel. When all are busy, the oldest is stolen.
pub const VOICES_PER_CHANNEL: usize = 16;
/// Gain/pan smoothing time in seconds.
const GAIN_RAMP_S: f32 = 0.01;

/// One channel of the rack on the audio thread. Everything is allocated at engine creation:
/// each slot holds both a sampler voice pool and a Gloom Synth, and plays whichever its
/// parameters select.
pub(crate) struct ChannelSlot {
    synth: Box<GloomSynth>,
    sr: f32,
    params: ChannelParams,
    env: AdsrParams,
    sample: Option<Arc<SampleData>>,
    voices: [SamplerVoice; VOICES_PER_CHANNEL],
    gain_l: LinearRamp,
    gain_r: LinearRamp,
    ramp_frames: u32,
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
            env: Self::env_for(sample_rate, &params),
            sample: None,
            voices: [SamplerVoice::new(sample_rate); VOICES_PER_CHANNEL],
            gain_l: LinearRamp::new(0.0),
            gain_r: LinearRamp::new(0.0),
            ramp_frames: (sample_rate * GAIN_RAMP_S) as u32,
        }
    }

    fn env_for(sr: f32, p: &ChannelParams) -> AdsrParams {
        let a = p.adsr;
        AdsrParams::new(sr, a.attack_ms, a.decay_ms, a.sustain, a.release_ms)
    }

    /// Applies new settings. Gain and pan glide over 10 ms; the rest applies to new notes.
    pub(crate) fn set_params(&mut self, p: ChannelParams) {
        if p.kind != self.params.kind {
            // The other instrument's voices would never get their note-offs.
            self.kill_all();
        }
        if p.kind == InstrumentKind::Synth {
            self.synth.set_settings(&p.synth);
        }
        self.params = p;
        self.env = Self::env_for(self.sr, &p);
        let (l, r) = pan_gains(p.gain, p.pan);
        self.gain_l.set_target(l, self.ramp_frames);
        self.gain_r.set_target(r, self.ramp_frames);
    }

    /// Like `set_params`, but jumps straight to the new gain (for a channel that was idle).
    pub(crate) fn set_params_now(&mut self, p: ChannelParams) {
        self.set_params(p);
        let (l, r) = pan_gains(p.gain, p.pan);
        self.gain_l = LinearRamp::new(l);
        self.gain_r = LinearRamp::new(r);
    }

    /// Swaps the sample, returning the old one (for the garbage queue). Playing voices stop:
    /// they were reading the old data.
    pub(crate) fn set_sample(&mut self, s: Option<Arc<SampleData>>) -> Option<Arc<SampleData>> {
        self.kill_all();
        std::mem::replace(&mut self.sample, s)
    }

    /// Starts a note. `age` orders voices for stealing.
    pub(crate) fn note_on(&mut self, key: u8, velocity: f32, age: u64) {
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
        let p = &self.params;
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
}
