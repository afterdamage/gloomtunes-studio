//! Soft clipper with oversampling.

use super::{clamp_param, Downsampler2x, Effect, FxContext, Smooth, Upsampler2x};
use crate::db_to_gain;
use std::f32::consts::FRAC_PI_2;

mod p {
    pub const DRIVE: usize = 0;
    pub const CURVE: usize = 1;
    pub const CEILING: usize = 2;
    pub const OVERSAMPLE: usize = 3;
    pub const OUTPUT: usize = 4;
    pub const COUNT: usize = 5;
}

/// Clipping curves, indexed by the `curve` parameter. Each maps the input (in units of the
/// ceiling) onto -1..1 with unity slope at zero, except the cubic, which is gentler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Curve {
    Tanh,
    Cubic,
    Arctan,
    Hard,
}

impl Curve {
    fn from_value(v: f32) -> Self {
        match v.round() as i32 {
            1 => Self::Cubic,
            2 => Self::Arctan,
            3 => Self::Hard,
            _ => Self::Tanh,
        }
    }

    #[inline]
    fn shape(self, u: f32) -> f32 {
        match self {
            Self::Tanh => u.tanh(),
            Self::Cubic => {
                let u = u.clamp(-1.0, 1.0);
                1.5 * u - 0.5 * u * u * u
            }
            Self::Arctan => (FRAC_PI_2 * u).atan() / FRAC_PI_2,
            Self::Hard => u.clamp(-1.0, 1.0),
        }
    }
}

/// One channel's resampling filters: 2x uses the outer pair, 4x both.
#[derive(Debug, Clone, Default)]
struct Chain {
    up1: Upsampler2x,
    up2: Upsampler2x,
    down2: Downsampler2x,
    down1: Downsampler2x,
}

/// Soft clipper: drive into a clipping curve that never exceeds the ceiling. Clipping makes
/// harmonics far above the input; at the plain rate those above Nyquist fold back as
/// inharmonic aliasing, so the curve runs at 2x or 4x the rate between half-band filters.
#[derive(Debug, Clone)]
pub struct SoftClipper {
    drive: Smooth,
    ceiling: Smooth,
    output: Smooth,
    curve: Curve,
    factor: usize,
    chains: [Chain; 2],
}

impl SoftClipper {
    /// Tanh at 0 dB drive, 2x oversampled.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        Self {
            drive: Smooth::new(1.0, 20.0, sr),
            ceiling: Smooth::new(1.0, 20.0, sr),
            output: Smooth::new(1.0, 20.0, sr),
            curve: Curve::Tanh,
            factor: 2,
            chains: Default::default(),
        }
    }
}

impl Effect for SoftClipper {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            p::DRIVE => self.drive.set(db_to_gain(clamp_param(v, 0.0, 24.0, 0.0))),
            p::CURVE => self.curve = Curve::from_value(clamp_param(v, 0.0, 3.0, 0.0)),
            p::CEILING => self
                .ceiling
                .set(db_to_gain(clamp_param(v, -24.0, 0.0, 0.0))),
            p::OVERSAMPLE => {
                self.factor = match clamp_param(v, 0.0, 2.0, 1.0).round() as i32 {
                    0 => 1,
                    1 => 2,
                    _ => 4,
                };
            }
            p::OUTPUT => self
                .output
                .set(db_to_gain(clamp_param(v, -24.0, 12.0, 0.0))),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.drive.snap();
        self.ceiling.snap();
        self.output.snap();
        self.chains = Default::default();
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &FxContext) {
        let curve = self.curve;
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let drive = self.drive.next_value();
            let ceiling = self.ceiling.next_value();
            let out = self.output.next_value();
            let clip = |v: f32| ceiling * curve.shape(v * drive / ceiling);
            for (ch, s) in [x, y].into_iter().enumerate() {
                let c = &mut self.chains[ch];
                let v = match self.factor {
                    1 => clip(*s),
                    2 => {
                        let [a, b] = c.up1.process(*s);
                        c.down1.process([clip(a), clip(b)])
                    }
                    _ => {
                        let [a, b] = c.up1.process(*s);
                        let [a0, a1] = c.up2.process(a);
                        let [b0, b1] = c.up2.process(b);
                        let lo = c.down2.process([clip(a0), clip(a1)]);
                        let hi = c.down2.process([clip(b0), clip(b1)]);
                        c.down1.process([lo, hi])
                    }
                };
                *s = v * out;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{db, run, sine, tone};

    const SR: f32 = 48_000.0;

    /// Clips a 7 kHz tone hard and returns (fundamental, loudest alias) in dB.
    fn aliasing(curve: f32, oversample: f32) -> (f32, f32) {
        let mut c = SoftClipper::new(SR);
        c.set_param(p::DRIVE, 18.0);
        c.set_param(p::CURVE, curve);
        c.set_param(p::OVERSAMPLE, oversample);
        c.reset();
        let x = sine(7000.0, 0.9, SR, 48_000);
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut c, &mut l, &mut r, 120.0);
        let tail = &l[24_000..];
        // Odd harmonics 35, 49, 63, 77 and 91 kHz fold to 13, 1, 15, 19 and 5 kHz.
        let worst = [13_000.0, 1000.0, 15_000.0, 19_000.0, 5000.0]
            .into_iter()
            .map(|f| db(tone(tail, f, SR)))
            .fold(f32::NEG_INFINITY, f32::max);
        (db(tone(tail, 7000.0, SR)), worst)
    }

    #[test]
    fn oversampling_keeps_aliasing_down() {
        for curve in [0.0, 2.0, 3.0] {
            let (f0, plain) = aliasing(curve, 0.0);
            let (g0, x4) = aliasing(curve, 2.0);
            assert!(plain - f0 > -40.0, "curve {curve}: plain {}", plain - f0);
            // The stated level: with 4x oversampling every alias stays 60 dB under the tone
            // (45 dB for the hard clip, whose harmonics fall off slowest).
            let limit = if curve == 3.0 { -45.0 } else { -60.0 };
            assert!(x4 - g0 < limit, "curve {curve}: 4x {}", x4 - g0);
            let (h0, x2) = aliasing(curve, 1.0);
            let gain = if curve == 3.0 { 0.0 } else { 10.0 };
            assert!(x2 - h0 < plain - f0 - gain, "curve {curve}: 2x {}", x2 - h0);
        }
    }

    #[test]
    fn the_ceiling_is_never_exceeded_and_quiet_signals_pass() {
        for curve in 0..4 {
            let mut c = SoftClipper::new(SR);
            c.set_param(p::DRIVE, 24.0);
            c.set_param(p::CURVE, curve as f32);
            c.set_param(p::CEILING, -6.0);
            c.set_param(p::OVERSAMPLE, 0.0);
            c.reset();
            let x = sine(100.0, 1.0, SR, 4800);
            let (mut l, mut r) = (x.clone(), x.clone());
            run(&mut c, &mut l, &mut r, 120.0);
            assert!(
                l.iter().all(|v| v.abs() <= db_to_gain(-6.0) + 1e-6),
                "{curve}"
            );
        }
        // A quiet tone through tanh at 0 dB drive is nearly untouched.
        let mut c = SoftClipper::new(SR);
        c.reset();
        let x = sine(1000.0, 0.01, SR, 9600);
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut c, &mut l, &mut r, 120.0);
        assert!(db(tone(&l[4800..], 1000.0, SR) / tone(&x[4800..], 1000.0, SR)).abs() < 0.01);
    }
}
