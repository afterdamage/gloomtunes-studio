//! ADSR amplitude envelope.
//!
//! Attack is a linear ramp from 0 to 1. Decay and release are exponential approaches, the
//! shape of an RC circuit charging or discharging: each sample moves the level a fixed fraction
//! `1 - k` of the remaining distance to the target, with `k = exp(-1 / (tau * sample_rate))`.
//! The time constant is `tau = time / 4.6`, so the level is within 1 % (e^-4.6) of the target
//! after the set time. A linear decay sounds abrupt at the end; the exponential one sounds like a
//! natural decay because loudness is perceived roughly logarithmically.
//!
//! Release ends the envelope at -80 dB (1e-4), well before the level could become denormal.

/// Level treated as silence. -80 dB.
const FLOOR: f32 = 1e-4;
/// Number of time constants in the user-facing time (e^-4.6 ≈ 1 %).
const TIME_CONSTANTS: f32 = 4.6;

/// Precomputed envelope coefficients for one sample rate. Cheap to copy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdsrParams {
    attack_step: f32,
    decay_k: f32,
    sustain: f32,
    release_k: f32,
}

impl Default for AdsrParams {
    fn default() -> Self {
        Self::new(48_000.0, 0.0, 0.0, 1.0, 50.0)
    }
}

fn coefficient(sample_rate: f32, ms: f32) -> f32 {
    let frames = ms.max(0.0) * 0.001 * sample_rate;
    if frames < 1.0 {
        0.0 // instant
    } else {
        (-TIME_CONSTANTS / frames).exp()
    }
}

impl AdsrParams {
    /// Computes coefficients. Times are in milliseconds; `sustain` is clamped to 0..=1.
    pub fn new(
        sample_rate: f32,
        attack_ms: f32,
        decay_ms: f32,
        sustain: f32,
        release_ms: f32,
    ) -> Self {
        let sr = sample_rate.max(1.0);
        let attack_frames = attack_ms.max(0.0) * 0.001 * sr;
        Self {
            attack_step: if attack_frames < 1.0 {
                1.0
            } else {
                1.0 / attack_frames
            },
            decay_k: coefficient(sr, decay_ms),
            sustain: sustain.clamp(0.0, 1.0),
            release_k: coefficient(sr, release_ms),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Envelope state for one voice.
#[derive(Debug, Clone, Copy)]
pub struct Adsr {
    p: AdsrParams,
    stage: Stage,
    level: f32,
}

impl Default for Adsr {
    fn default() -> Self {
        Self {
            p: AdsrParams::default(),
            stage: Stage::Idle,
            level: 0.0,
        }
    }
}

impl Adsr {
    /// Starts the envelope from 0 with the given coefficients.
    pub fn trigger(&mut self, params: AdsrParams) {
        self.p = params;
        self.level = 0.0;
        self.stage = Stage::Attack;
    }

    /// Enters the release stage from the current level.
    pub fn release(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }

    /// Stops immediately.
    pub fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.level = 0.0;
    }

    /// False once the release (or a zero sustain) has reached silence.
    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    /// True after `release` was called.
    pub fn is_releasing(&self) -> bool {
        self.stage == Stage::Release
    }

    /// Current level without advancing.
    pub fn level(&self) -> f32 {
        self.level
    }

    /// Advances one sample and returns the level for it.
    #[inline]
    pub fn next_level(&mut self) -> f32 {
        let p = &self.p;
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.level += p.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level = p.sustain + (self.level - p.sustain) * p.decay_k;
                if (self.level - p.sustain).abs() < FLOOR {
                    self.level = p.sustain;
                    self.stage = if p.sustain < FLOOR {
                        Stage::Idle
                    } else {
                        Stage::Sustain
                    };
                }
            }
            Stage::Sustain => self.level = p.sustain,
            Stage::Release => {
                self.level *= p.release_k;
                if self.level < FLOOR {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(env: &mut Adsr, n: usize) -> Vec<f32> {
        (0..n).map(|_| env.next_level()).collect()
    }

    #[test]
    fn attack_is_linear_and_reaches_one_on_time() {
        let mut e = Adsr::default();
        e.trigger(AdsrParams::new(1000.0, 10.0, 0.0, 1.0, 0.0));
        let v = run(&mut e, 12);
        for (i, x) in v.iter().take(10).enumerate() {
            assert!((x - (i + 1) as f32 * 0.1).abs() < 1e-5, "{i} {x}");
        }
        assert_eq!(v[11], 1.0);
    }

    #[test]
    fn zero_attack_starts_at_full_level() {
        let mut e = Adsr::default();
        e.trigger(AdsrParams::new(48_000.0, 0.0, 0.0, 1.0, 10.0));
        assert_eq!(e.next_level(), 1.0);
    }

    #[test]
    fn decay_reaches_sustain_within_one_percent_on_time() {
        let sr = 48_000.0;
        let mut e = Adsr::default();
        e.trigger(AdsrParams::new(sr, 0.0, 100.0, 0.5, 100.0));
        let v = run(&mut e, 4800 + 1);
        // 1 % of the distance (0.5) is 0.005.
        assert!((v[4800] - 0.5).abs() <= 0.0051, "{}", v[4800]);
        assert!(
            (v[2400] - 0.5).abs() > 0.02,
            "should not be there at half the time"
        );
        assert!(v.windows(2).all(|w| w[1] <= w[0]));
    }

    #[test]
    fn release_falls_to_silence_and_goes_idle() {
        let sr = 48_000.0;
        let mut e = Adsr::default();
        e.trigger(AdsrParams::new(sr, 0.0, 0.0, 0.8, 50.0));
        run(&mut e, 100);
        assert_eq!(e.level(), 0.8);
        e.release();
        let v = run(&mut e, 2400);
        assert!(v[2399] <= 0.8 * 0.0101, "{}", v[2399]);
        run(&mut e, 48_000);
        assert!(!e.is_active());
        assert_eq!(e.level(), 0.0);
    }

    #[test]
    fn zero_sustain_ends_the_note_after_decay() {
        let mut e = Adsr::default();
        e.trigger(AdsrParams::new(48_000.0, 0.0, 20.0, 0.0, 0.0));
        run(&mut e, 48_000);
        assert!(!e.is_active());
    }

    #[test]
    fn release_during_attack_starts_from_current_level() {
        let mut e = Adsr::default();
        e.trigger(AdsrParams::new(1000.0, 10.0, 0.0, 1.0, 10.0));
        run(&mut e, 5);
        e.release();
        let x = e.next_level();
        assert!(x < 0.5 && x > 0.0, "{x}");
    }
}
