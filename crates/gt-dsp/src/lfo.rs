//! Low-frequency oscillator and a seeded noise source.
//!
//! LFO waves are generated naively (no band-limiting): at modulation rates the harmonics that
//! matter are far below Nyquist. Sample-and-hold picks a new random level at the start of each
//! cycle. Both use a small xorshift generator with a fixed seed, so a render is reproducible.

/// LFO waveform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LfoWave {
    /// Sine.
    #[default]
    Sine,
    /// Triangle.
    Triangle,
    /// Falling saw (1 to -1), the usual "ramp down" for wobbles.
    Saw,
    /// Square.
    Square,
    /// A new random level every cycle.
    SampleHold,
}

/// White noise from a 32-bit xorshift generator: uniform in -1..1, flat spectrum, deterministic
/// for a given seed.
#[derive(Debug, Clone, Copy)]
pub struct Noise {
    state: u32,
}

impl Noise {
    /// Creates a generator. A zero seed is replaced (xorshift would stay at 0).
    pub fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 0x9E37_79B9 } else { seed },
        }
    }

    /// Next value in -1..1.
    #[inline]
    pub fn next_value(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        (x >> 8) as f32 * (2.0 / (1u32 << 24) as f32) - 1.0
    }
}

/// A free-running LFO with output in -1..1.
#[derive(Debug, Clone, Copy)]
pub struct Lfo {
    phase: f32,
    held: f32,
    noise: Noise,
}

impl Lfo {
    /// Creates an LFO at phase 0.
    pub fn new(seed: u32) -> Self {
        let mut noise = Noise::new(seed);
        Self {
            phase: 0.0,
            held: noise.next_value(),
            noise,
        }
    }

    /// Restarts the cycle (on a new note).
    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.held = self.noise.next_value();
    }

    /// Value at the current phase.
    #[inline]
    pub fn value(&self, wave: LfoWave) -> f32 {
        let t = self.phase;
        match wave {
            LfoWave::Sine => (t * core::f32::consts::TAU).sin(),
            // Starts at 0 rising, like the sine.
            LfoWave::Triangle => {
                if t < 0.25 {
                    4.0 * t
                } else if t < 0.75 {
                    2.0 - 4.0 * t
                } else {
                    4.0 * t - 4.0
                }
            }
            LfoWave::Saw => 1.0 - 2.0 * t,
            LfoWave::Square => {
                if t < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            LfoWave::SampleHold => self.held,
        }
    }

    /// Advances by `cycles` (rate × time).
    #[inline]
    pub fn advance(&mut self, cycles: f32) {
        self.phase += cycles.max(0.0);
        if self.phase >= 1.0 {
            self.phase = self.phase.fract();
            self.held = self.noise.next_value();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_uniform_and_deterministic() {
        let mut a = Noise::new(7);
        let mut b = Noise::new(7);
        let v: Vec<f32> = (0..100_000).map(|_| a.next_value()).collect();
        assert!(v.iter().all(|x| (-1.0..1.0).contains(x)));
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        assert!(mean.abs() < 0.01, "{mean}");
        assert_eq!(b.next_value(), v[0]);
    }

    #[test]
    fn lfo_shapes() {
        let mut l = Lfo::new(1);
        assert_eq!(l.value(LfoWave::Triangle), 0.0);
        l.advance(0.25);
        assert!((l.value(LfoWave::Sine) - 1.0).abs() < 1e-6);
        assert!((l.value(LfoWave::Triangle) - 1.0).abs() < 1e-6);
        assert_eq!(l.value(LfoWave::Saw), 0.5);
        l.advance(0.5);
        assert_eq!(l.value(LfoWave::Square), -1.0);
        let held = l.value(LfoWave::SampleHold);
        l.advance(0.3); // wraps: new level
        assert_ne!(l.value(LfoWave::SampleHold), held);
    }
}
