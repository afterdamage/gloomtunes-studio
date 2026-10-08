//! Metronome click voice.
//!
//! A short sine burst with a fast linear attack and an exponential decay, i.e. a damped
//! oscillator, the same shape a struck object produces. The decay is computed recursively
//! (`env *= k` each sample, with `k = exp(-1 / (tau * sample_rate))`), so each sample costs one
//! multiply. The voice switches itself off after `LENGTH_S`, before the envelope can reach the
//! denormal range (that would take hundreds of time constants).
//!
//! The oscillator starts at a cosine peak and the attack starts above zero, so the very first
//! sample of a click is non-zero. That makes onsets exactly measurable in tests, and the 0.5 ms
//! attack keeps the step small enough that it reads as part of the click, not as distortion.

use crate::SineOsc;

const ATTACK_S: f32 = 0.0005;
const DECAY_TAU_S: f32 = 0.012;
const LENGTH_S: f32 = 0.060;

/// A one-shot click voice. Retriggering restarts it.
#[derive(Debug, Clone)]
pub struct Click {
    sample_rate: f32,
    osc: SineOsc,
    attack_frames: u32,
    decay_k: f32,
    length_frames: u32,
    pos: u32,
    env: f32,
    gain: f32,
    active: bool,
}

impl Click {
    /// Creates an idle click voice.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        Self {
            sample_rate: sr,
            osc: SineOsc::new(sr, 1000.0),
            attack_frames: ((ATTACK_S * sr).round() as u32).max(1),
            decay_k: (-1.0 / (DECAY_TAU_S * sr)).exp(),
            length_frames: (LENGTH_S * sr).round() as u32,
            pos: 0,
            env: 0.0,
            gain: 0.0,
            active: false,
        }
    }

    /// Starts a click at `freq` Hz with peak amplitude `gain`.
    pub fn trigger(&mut self, freq: f32, gain: f32) {
        self.osc = SineOsc::new(self.sample_rate, freq);
        // Advance a quarter cycle so the burst starts at the cosine peak.
        self.osc.set_phase(0.25);
        self.pos = 0;
        self.env = 0.0;
        self.gain = gain;
        self.active = true;
    }

    /// True while the click is sounding.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Adds the click into `out` (mixing, not overwriting).
    pub fn add_to(&mut self, out: &mut [f32]) {
        for s in out {
            if !self.active {
                return;
            }
            if self.pos < self.attack_frames {
                self.env = (self.pos + 1) as f32 / self.attack_frames as f32;
            } else {
                self.env *= self.decay_k;
            }
            *s += self.osc.next_sample() * self.env * self.gain;
            self.pos += 1;
            if self.pos >= self.length_frames {
                self.active = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_is_non_zero_and_click_ends() {
        let mut c = Click::new(48_000.0);
        c.trigger(1000.0, 0.5);
        let mut buf = vec![0.0; 4800];
        c.add_to(&mut buf);
        assert!(buf[0] > 0.0);
        let len = (0.06 * 48_000.0) as usize;
        assert!(buf[len..].iter().all(|&s| s == 0.0));
        assert!(!c.is_active());
        let peak = buf.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!(peak <= 0.5 && peak > 0.4, "{peak}");
    }

    #[test]
    fn decays_by_tau() {
        let mut c = Click::new(48_000.0);
        c.trigger(1000.0, 1.0);
        let mut buf = vec![0.0; 2400];
        c.add_to(&mut buf);
        // Envelope after attack + one time constant should be about 1/e of the peak.
        let at = |t: f32| {
            let i = (t * 48_000.0) as usize;
            buf[i - 24..i + 24]
                .iter()
                .fold(0.0_f32, |m, s| m.max(s.abs()))
        };
        let ratio = at(0.0125) / at(0.0005 + 0.0005);
        assert!((ratio - (-1.0_f32).exp()).abs() < 0.05, "{ratio}");
    }

    #[test]
    fn mixes_into_existing_signal() {
        let mut c = Click::new(44_100.0);
        let mut buf = vec![0.25; 8];
        c.add_to(&mut buf);
        assert!(buf.iter().all(|&s| s == 0.25));
    }
}
