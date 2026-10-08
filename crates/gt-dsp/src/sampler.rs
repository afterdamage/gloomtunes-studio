//! Sample playback voice.
//!
//! The voice reads the sample at a fractional position that advances by `step` frames per
//! output frame: `step = 2^(semitones / 12) * sample_rate / engine_rate`. Between stored frames
//! it interpolates linearly. Linear interpolation is cheap and fine for drums and moderate
//! transposition; it attenuates the top octave slightly and, when pitching up, lets some
//! aliasing through because there is no low-pass before decimation. A windowed-sinc or
//! polyphase reader is a later quality option (ARCHITECTURE.md D23).
//!
//! One-shot voices play from start to end and ignore note-off; the last 2 ms before the end
//! point fade out, so trimming the end mid-waveform does not click. Loop voices repeat the
//! start..end region until note-off, then fade with the envelope's release.

use crate::{Adsr, AdsrParams};

/// End-of-sample fade for one-shot voices, in seconds.
const DECLICK_S: f64 = 0.002;

/// The part of the sample a voice plays, in sample frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceRegion {
    /// First frame played (and the loop start).
    pub start: f64,
    /// Frame where playback stops or wraps.
    pub end: f64,
    /// Loop the region while the note is held.
    pub looped: bool,
}

impl VoiceRegion {
    /// Converts fractional start/end points (0..1) into frames of a sample `frames` long.
    /// Guarantees `0 <= start < end <= frames` for a non-empty sample.
    pub fn from_fractions(frames: usize, start: f32, end: f32, looped: bool) -> Self {
        let len = frames as f64;
        let s = f64::from(start.clamp(0.0, 1.0)) * len;
        let e = f64::from(end.clamp(0.0, 1.0)) * len;
        let (s, e) = if e - s < 1.0 {
            // Degenerate: keep at least one frame.
            let s = s.min((len - 1.0).max(0.0));
            (s, (s + 1.0).min(len))
        } else {
            (s, e)
        };
        Self {
            start: s,
            end: e,
            looped,
        }
    }
}

/// One playing note of a sampler.
#[derive(Debug, Clone, Copy)]
pub struct SamplerVoice {
    active: bool,
    key: u8,
    pos: f64,
    step: f64,
    region: VoiceRegion,
    gain: f32,
    env: Adsr,
    declick_frames: f64,
    /// Trigger order, for stealing the oldest voice.
    pub age: u64,
}

impl SamplerVoice {
    /// Creates an idle voice for an engine running at `sample_rate`.
    pub fn new(sample_rate: f32) -> Self {
        Self {
            active: false,
            key: 0,
            pos: 0.0,
            step: 1.0,
            region: VoiceRegion {
                start: 0.0,
                end: 0.0,
                looped: false,
            },
            gain: 0.0,
            env: Adsr::default(),
            declick_frames: DECLICK_S * f64::from(sample_rate.max(1.0)),
            age: 0,
        }
    }

    /// Starts the voice. `step` is the read increment (see module docs), `gain` the linear
    /// amplitude (velocity curve already applied).
    pub fn trigger(
        &mut self,
        key: u8,
        gain: f32,
        step: f64,
        region: VoiceRegion,
        env: AdsrParams,
        age: u64,
    ) {
        self.active = region.end > region.start && step > 0.0;
        self.key = key;
        self.pos = region.start;
        self.step = step;
        self.region = region;
        self.gain = gain;
        self.env.trigger(env);
        self.age = age;
    }

    /// Note-off: loop voices enter their release, one-shot voices play on.
    pub fn release(&mut self) {
        if self.region.looped {
            self.env.release();
        }
    }

    /// Silences the voice immediately.
    pub fn kill(&mut self) {
        self.active = false;
        self.env.reset();
    }

    /// True while the voice produces sound.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// True while the voice is held (active and not releasing).
    pub fn is_held(&self) -> bool {
        self.active && !self.env.is_releasing()
    }

    /// The key that started the voice.
    pub fn key(&self) -> u8 {
        self.key
    }

    /// Adds the voice into `out_l` / `out_r` (same length), reading `left` / `right` (the same
    /// slice twice for a mono sample).
    pub fn render(&mut self, left: &[f32], right: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let len = left.len().min(right.len());
        let end = self.region.end.min(len as f64);
        let start = self.region.start;
        for (ol, or) in out_l.iter_mut().zip(out_r.iter_mut()) {
            if !self.active {
                return;
            }
            if self.pos >= end {
                if self.region.looped && end > start {
                    self.pos = start + (self.pos - end) % (end - start);
                } else {
                    self.active = false;
                    return;
                }
            }
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            let j = if i + 1 < end as usize {
                i + 1
            } else if self.region.looped {
                start as usize
            } else {
                len // past the end reads as silence
            };
            let read = |d: &[f32]| {
                let a = d.get(i).copied().unwrap_or(0.0);
                let b = d.get(j).copied().unwrap_or(0.0);
                a + (b - a) * frac
            };
            let mut g = self.gain * self.env.next_level();
            if !self.region.looped {
                let remaining = (end - self.pos) / self.step;
                if remaining < self.declick_frames {
                    g *= (remaining / self.declick_frames) as f32;
                }
            }
            *ol += read(left) * g;
            *or += read(right) * g;
            if !self.env.is_active() {
                self.active = false;
            }
            self.pos += self.step;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn flat_env() -> AdsrParams {
        AdsrParams::new(SR, 0.0, 0.0, 1.0, 10.0)
    }

    fn render(v: &mut SamplerVoice, data: &[f32], frames: usize) -> Vec<f32> {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        v.render(data, data, &mut l, &mut r);
        assert_eq!(l, r);
        l
    }

    fn ramp(n: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32 / n as f32).collect()
    }

    #[test]
    fn unity_step_reproduces_the_sample() {
        let data = ramp(4800);
        let mut v = SamplerVoice::new(SR);
        let region = VoiceRegion::from_fractions(data.len(), 0.0, 1.0, false);
        v.trigger(60, 1.0, 1.0, region, flat_env(), 0);
        let out = render(&mut v, &data, 5000);
        // Exact until the 2 ms end fade (96 frames).
        let fade = (0.002 * SR) as usize;
        assert_eq!(&out[..4800 - fade], &data[..4800 - fade]);
        assert!(out[4799] < 0.02);
        assert!(out[4800..].iter().all(|&s| s == 0.0));
        assert!(!v.is_active());
    }

    #[test]
    fn octave_up_plays_twice_as_fast() {
        let data = ramp(4800);
        let mut v = SamplerVoice::new(SR);
        let region = VoiceRegion::from_fractions(data.len(), 0.0, 1.0, false);
        v.trigger(72, 1.0, 2.0, region, flat_env(), 0);
        let out = render(&mut v, &data, 4800);
        assert_eq!(out[100], data[200]);
        assert!(out[2400..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn fractional_step_interpolates() {
        let data = vec![0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut v = SamplerVoice::new(1.0); // no declick at this rate
        v.trigger(
            60,
            1.0,
            0.5,
            VoiceRegion::from_fractions(8, 0.0, 1.0, false),
            AdsrParams::new(1.0, 0.0, 0.0, 1.0, 0.0),
            0,
        );
        let out = render(&mut v, &data, 5);
        assert_eq!(out, vec![0.0, 0.5, 1.0, 0.5, 0.0]);
    }

    #[test]
    fn start_and_end_points_trim() {
        let data = ramp(1000);
        let mut v = SamplerVoice::new(1.0);
        let region = VoiceRegion::from_fractions(1000, 0.25, 0.5, false);
        v.trigger(
            60,
            1.0,
            1.0,
            region,
            AdsrParams::new(1.0, 0.0, 0.0, 1.0, 0.0),
            0,
        );
        let out = render(&mut v, &data, 400);
        assert_eq!(out[0], data[250]);
        assert_eq!(out[248], data[498]);
        assert!(out[250..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn one_shot_ignores_note_off() {
        let data = vec![0.5; 4800];
        let mut v = SamplerVoice::new(SR);
        v.trigger(
            60,
            1.0,
            1.0,
            VoiceRegion::from_fractions(4800, 0.0, 1.0, false),
            flat_env(),
            0,
        );
        render(&mut v, &data, 100);
        v.release();
        let out = render(&mut v, &data, 100);
        assert!(out.iter().all(|&s| s == 0.5));
    }

    #[test]
    fn loop_repeats_while_held_then_releases() {
        let data = ramp(100);
        let mut v = SamplerVoice::new(SR);
        v.trigger(
            60,
            1.0,
            1.0,
            VoiceRegion::from_fractions(100, 0.5, 1.0, true),
            flat_env(),
            0,
        );
        let out = render(&mut v, &data, 200);
        assert_eq!(out[0], data[50]);
        assert_eq!(out[50], data[50]);
        assert_eq!(out[150], data[50]);
        v.release();
        let tail = render(&mut v, &data, 4800);
        assert!(tail[0] > 0.0);
        assert!(!v.is_active());
        assert_eq!(tail[4799], 0.0);
    }

    #[test]
    fn degenerate_regions_are_safe() {
        let r = VoiceRegion::from_fractions(10, 0.9, 0.1, false);
        assert!(r.end > r.start && r.end <= 10.0);
        let r = VoiceRegion::from_fractions(0, 0.0, 1.0, true);
        let mut v = SamplerVoice::new(SR);
        v.trigger(60, 1.0, 1.0, r, flat_env(), 0);
        assert!(!v.is_active());
        let out = render(&mut v, &[], 16);
        assert!(out.iter().all(|&s| s == 0.0));
    }
}
