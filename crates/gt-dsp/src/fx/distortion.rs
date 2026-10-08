//! Waveshaping distortion with anti-aliasing.

use super::{clamp_param, one_pole_coef, Effect, FxContext, Smooth};
use crate::db_to_gain;

mod p {
    pub const DRIVE: usize = 0;
    pub const SHAPE: usize = 1;
    pub const TONE: usize = 2;
    pub const OUTPUT: usize = 3;
    pub const MIX: usize = 4;
    pub const COUNT: usize = 5;
}

/// Transfer curves. The value is the parameter value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// `tanh`: smooth saturation, odd harmonics.
    Soft,
    /// Clips at ±1: harsh, buzzy.
    Hard,
    /// `sin`: folds loud parts back down, metallic.
    Fold,
    /// Offset `tanh`: asymmetric, adds even harmonics like a tube stage.
    Tube,
}

const TUBE_BIAS: f32 = 0.3;

/// `ln(cosh x)`, the antiderivative of `tanh`, without overflow for large `|x|`.
#[inline]
fn ln_cosh(x: f64) -> f64 {
    let a = x.abs();
    if a > 18.0 {
        a - std::f64::consts::LN_2
    } else {
        a.cosh().ln()
    }
}

impl Shape {
    fn from_value(v: f32) -> Self {
        match v.round() as i32 {
            1 => Self::Hard,
            2 => Self::Fold,
            3 => Self::Tube,
            _ => Self::Soft,
        }
    }

    #[inline]
    fn f(self, x: f32) -> f32 {
        match self {
            Self::Soft => x.tanh(),
            Self::Hard => x.clamp(-1.0, 1.0),
            Self::Fold => x.sin(),
            Self::Tube => (x + TUBE_BIAS).tanh() - TUBE_BIAS.tanh(),
        }
    }

    /// Antiderivative of `f`, in `f64`: the ADAA quotient subtracts two nearly equal values.
    #[inline]
    fn big_f(self, x: f64) -> f64 {
        let bias = f64::from(TUBE_BIAS);
        match self {
            Self::Soft => ln_cosh(x),
            Self::Hard => {
                if x.abs() <= 1.0 {
                    0.5 * x * x
                } else {
                    x.abs() - 0.5
                }
            }
            Self::Fold => -x.cos(),
            Self::Tube => ln_cosh(x + bias) - x * bias.tanh(),
        }
    }
}

/// Distortion: gain into a waveshaper, then a tone low-pass, a DC blocker and output level.
///
/// A waveshaper creates harmonics above Nyquist that fold back as inharmonic aliasing. Instead
/// of oversampling, this uses first-order antiderivative anti-aliasing (ADAA): the output is the
/// average of the curve between consecutive inputs, `(F(x₁) − F(x₀)) / (x₁ − x₀)`, where `F`
/// is the curve's antiderivative. That acts like a gentle low-pass on the generated harmonics
/// (aliasing is typically 10 to 20 dB lower) at the cost of half a sample of delay.
#[derive(Debug, Clone)]
pub struct Distortion {
    sr: f32,
    shape: Shape,
    drive: Smooth,
    tone_coef: f32,
    output: Smooth,
    mix: Smooth,
    /// Previous driven input and its antiderivative, per side.
    prev: [(f64, f64); 2],
    tone: [f32; 2],
    /// DC blocker state (x[n-1], y[n-1]) per side.
    dc: [(f32, f32); 2],
    dc_r: f32,
}

impl Distortion {
    /// Soft saturation, 12 dB drive.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        Self {
            sr,
            shape: Shape::Soft,
            drive: Smooth::new(db_to_gain(12.0), 20.0, sr),
            tone_coef: one_pole_coef(12_000.0, sr),
            output: Smooth::new(db_to_gain(-6.0), 20.0, sr),
            mix: Smooth::new(1.0, 20.0, sr),
            prev: [(0.0, Shape::Soft.big_f(0.0)); 2],
            tone: [0.0; 2],
            dc: [(0.0, 0.0); 2],
            // 10 Hz high-pass.
            dc_r: 1.0 - std::f32::consts::TAU * 10.0 / sr,
        }
    }

    #[inline]
    fn shape_adaa(&mut self, side: usize, x: f32) -> f32 {
        let (x0, f0) = self.prev[side];
        let x1 = f64::from(x);
        let f1 = self.shape.big_f(x1);
        let dx = x1 - x0;
        let y = if dx.abs() > 1e-6 {
            ((f1 - f0) / dx) as f32
        } else {
            self.shape.f((0.5 * (x1 + x0)) as f32)
        };
        self.prev[side] = (x1, f1);
        y
    }
}

impl Effect for Distortion {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            p::DRIVE => self.drive.set(db_to_gain(clamp_param(v, 0.0, 48.0, 12.0))),
            p::SHAPE => {
                let s = Shape::from_value(clamp_param(v, 0.0, 3.0, 0.0));
                if s != self.shape {
                    self.shape = s;
                    // The stored antiderivative belongs to the old curve.
                    for p in &mut self.prev {
                        p.1 = s.big_f(p.0);
                    }
                }
            }
            p::TONE => {
                self.tone_coef = one_pole_coef(clamp_param(v, 500.0, 20_000.0, 12_000.0), self.sr);
            }
            p::OUTPUT => self
                .output
                .set(db_to_gain(clamp_param(v, -36.0, 12.0, -6.0))),
            p::MIX => self.mix.set(clamp_param(v, 0.0, 1.0, 1.0)),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.prev = [(0.0, self.shape.big_f(0.0)); 2];
        self.tone = [0.0; 2];
        self.dc = [(0.0, 0.0); 2];
        self.drive.snap();
        self.output.snap();
        self.mix.snap();
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &FxContext) {
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let drive = self.drive.next_value();
            let out = self.output.next_value();
            let mix = self.mix.next_value();
            for (side, s) in [x, y].into_iter().enumerate() {
                let shaped = self.shape_adaa(side, *s * drive);
                self.tone[side] += (shaped - self.tone[side]) * self.tone_coef;
                let (px, py) = self.dc[side];
                let t = self.tone[side];
                let hp = t - px + self.dc_r * py;
                self.dc[side] = (t, hp);
                *s += (hp * out - *s) * mix;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{run, sine};

    /// Power at `freq` via a single-bin DFT.
    fn bin_power(x: &[f32], freq: f32, sr: f32) -> f32 {
        let (mut re, mut im) = (0.0_f64, 0.0_f64);
        for (i, &v) in x.iter().enumerate() {
            let w = std::f64::consts::TAU * f64::from(freq) * i as f64 / f64::from(sr);
            // Hann window.
            let h = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / x.len() as f64).cos();
            re += f64::from(v) * h * w.cos();
            im += f64::from(v) * h * w.sin();
        }
        (re * re + im * im) as f32
    }

    #[test]
    fn hard_clip_is_bounded_and_adds_odd_harmonics() {
        let sr = 48_000.0;
        let mut d = Distortion::new(sr);
        d.set_param(p::SHAPE, 1.0);
        d.set_param(p::DRIVE, 24.0);
        d.set_param(p::OUTPUT, 0.0);
        d.set_param(p::TONE, 20_000.0);
        d.reset();
        let x = sine(200.0, 0.8, sr, 19_200);
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut d, &mut l, &mut r, 120.0);
        assert!(l[4800..].iter().all(|v| v.abs() < 1.2));
        let tail = &l[4800..];
        let h3 = bin_power(tail, 600.0, sr);
        let h2 = bin_power(tail, 400.0, sr);
        assert!(h3 > 1000.0 * h2, "{h3} {h2}");
    }

    #[test]
    fn adaa_lowers_aliasing() {
        // A 5 kHz tone hard-clipped: harmonics at 15, 25 (aliases to 23), 35 (→ 13) kHz...
        // Compare the 13 kHz alias with ADAA against a naive clipper.
        let sr = 48_000.0;
        let x = sine(5000.0, 0.9, sr, 16_384);
        let mut d = Distortion::new(sr);
        d.set_param(p::SHAPE, 1.0);
        d.set_param(p::DRIVE, 18.0);
        d.set_param(p::OUTPUT, 0.0);
        d.set_param(p::TONE, 20_000.0);
        d.reset();
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut d, &mut l, &mut r, 120.0);
        let g = db_to_gain(18.0);
        let naive: Vec<f32> = x.iter().map(|v| (v * g).clamp(-1.0, 1.0)).collect();
        let alias_adaa = bin_power(&l[2048..], 13_000.0, sr);
        let alias_naive = bin_power(&naive[2048..], 13_000.0, sr);
        let fund_adaa = bin_power(&l[2048..], 5000.0, sr);
        let fund_naive = bin_power(&naive[2048..], 5000.0, sr);
        let improvement = 10.0 * ((alias_naive / fund_naive) / (alias_adaa / fund_adaa)).log10();
        assert!(improvement > 6.0, "{improvement} dB");
    }

    #[test]
    fn tube_output_has_no_dc() {
        let sr = 48_000.0;
        let mut d = Distortion::new(sr);
        d.set_param(p::SHAPE, 3.0);
        d.set_param(p::DRIVE, 30.0);
        d.reset();
        let x = sine(110.0, 0.7, sr, 96_000);
        let (mut l, mut r) = (x.clone(), x);
        run(&mut d, &mut l, &mut r, 120.0);
        let tail = &l[48_000..];
        let mean = tail.iter().sum::<f32>() / tail.len() as f32;
        assert!(mean.abs() < 0.01, "{mean}");
    }
}
