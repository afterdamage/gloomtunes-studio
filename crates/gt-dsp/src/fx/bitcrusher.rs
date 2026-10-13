//! Bitcrusher: bit-depth and sample-rate reduction.

use super::{clamp_param, Biquad, BiquadCoeffs, Effect, FxContext, Smooth};

mod p {
    pub const BITS: usize = 0;
    pub const RATE: usize = 1;
    pub const ANTI_ALIAS: usize = 2;
    pub const MIX: usize = 3;
    pub const COUNT: usize = 4;
}

/// Bitcrusher: the signal is held for `sample rate / rate` frames at a time (sample-rate
/// reduction, which folds everything above `rate / 2` back down as aliasing), then rounded to
/// `2^(bits-1)` steps per unit. The optional anti-alias filter (two Butterworth low-pass
/// sections at 0.45 × rate) removes what would fold, for a darker, cleaner lo-fi sound.
#[derive(Debug, Clone)]
pub struct Bitcrusher {
    sr: f32,
    bits: Smooth,
    rate: f32,
    anti_alias: bool,
    mix: Smooth,
    filters: [[Biquad; 2]; 2],
    phase: f32,
    held: [f32; 2],
}

impl Bitcrusher {
    /// 8 bits at 8 kHz.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let mut b = Self {
            sr,
            bits: Smooth::new(8.0, 20.0, sr),
            rate: 8000.0,
            anti_alias: true,
            mix: Smooth::new(1.0, 20.0, sr),
            filters: [[Biquad::default(); 2]; 2],
            phase: 0.0,
            held: [0.0; 2],
        };
        b.set_param(p::RATE, 8000.0);
        b
    }
}

impl Effect for Bitcrusher {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            p::BITS => self.bits.set(clamp_param(v, 1.0, 24.0, 8.0)),
            p::RATE => {
                self.rate = clamp_param(v, 100.0, 48_000.0, 8000.0);
                let fc = (0.45 * self.rate).min(0.45 * self.sr);
                let c = BiquadCoeffs::low_pass(fc, std::f32::consts::FRAC_1_SQRT_2, self.sr);
                for f in self.filters.iter_mut().flatten() {
                    f.c = c;
                }
            }
            p::ANTI_ALIAS => self.anti_alias = clamp_param(v, 0.0, 1.0, 1.0) >= 0.5,
            p::MIX => self.mix.set(clamp_param(v, 0.0, 1.0, 1.0)),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.bits.snap();
        self.mix.snap();
        for f in self.filters.iter_mut().flatten() {
            f.reset();
        }
        self.phase = 1.0;
        self.held = [0.0; 2];
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &FxContext) {
        let step = (self.rate / self.sr).min(1.0);
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let steps = (self.bits.next_value() - 1.0).exp2();
            let mix = self.mix.next_value();
            self.phase += step;
            let take = self.phase >= 1.0;
            if take {
                self.phase -= 1.0;
            }
            for (ch, s) in [x, y].into_iter().enumerate() {
                let mut v = *s;
                if self.anti_alias {
                    let [a, b] = &mut self.filters[ch];
                    v = b.process(a.process(v));
                }
                if take {
                    self.held[ch] = (v * steps).round() / steps;
                }
                *s = *s * (1.0 - mix) + self.held[ch] * mix;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{db, rms, run, sine};

    const SR: f32 = 48_000.0;

    fn crushed(setup: &[(usize, f32)], x: &[f32]) -> Vec<f32> {
        let mut b = Bitcrusher::new(SR);
        for &(i, v) in setup {
            b.set_param(i, v);
        }
        b.reset();
        let (mut l, mut r) = (x.to_vec(), x.to_vec());
        run(&mut b, &mut l, &mut r, 120.0);
        l
    }

    #[test]
    fn bits_and_rate_are_reduced() {
        let x = sine(110.0, 0.9, SR, 4800);
        let y = crushed(
            &[(p::BITS, 3.0), (p::RATE, 1000.0), (p::ANTI_ALIAS, 0.0)],
            &x,
        );
        // Three bits: every value is a multiple of 1/4.
        assert!(y.iter().all(|v| (v * 4.0).fract() == 0.0));
        // 1 kHz: each value is held for 48 samples.
        for run in y.chunks(48) {
            assert!(run.iter().all(|&v| v == run[0]));
        }
        // Mix at zero leaves the signal alone.
        assert_eq!(crushed(&[(p::MIX, 0.0)], &x), x);
    }

    #[test]
    fn the_anti_alias_filter_keeps_high_tones_from_folding() {
        // 10 kHz held at 8 kHz folds to 2 kHz at nearly full level; filtered first, it is gone.
        let x = sine(10_000.0, 0.5, SR, 24_000);
        let folded = |aa: f32| {
            let y = crushed(&[(p::BITS, 24.0), (p::ANTI_ALIAS, aa)], &x);
            db(rms(&y[12_000..]) / rms(&x[12_000..]))
        };
        assert!(folded(0.0) > -6.0, "{}", folded(0.0));
        assert!(folded(1.0) < -40.0, "{}", folded(1.0));
    }
}
