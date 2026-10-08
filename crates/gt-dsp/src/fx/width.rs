//! Stereo width.

use super::{clamp_param, Effect, FxContext, Smooth};
use crate::db_to_gain;

mod p {
    pub const WIDTH: usize = 0;
    pub const OUTPUT: usize = 1;
    pub const COUNT: usize = 2;
}

/// Mid/side width: the side signal `(L − R)/2` is scaled by the width (0 = mono, 100 % =
/// unchanged, 200 % = twice as wide), then turned back into left and right.
#[derive(Debug, Clone)]
pub struct StereoWidth {
    width: Smooth,
    output: Smooth,
}

impl StereoWidth {
    /// Unity width.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        Self {
            width: Smooth::new(1.0, 20.0, sr),
            output: Smooth::new(1.0, 20.0, sr),
        }
    }
}

impl Effect for StereoWidth {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            p::WIDTH => self.width.set(clamp_param(v, 0.0, 2.0, 1.0)),
            p::OUTPUT => self
                .output
                .set(db_to_gain(clamp_param(v, -24.0, 12.0, 0.0))),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.width.snap();
        self.output.snap();
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &FxContext) {
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let w = self.width.next_value();
            let g = self.output.next_value();
            let mid = 0.5 * (*x + *y);
            let side = 0.5 * (*x - *y) * w;
            *x = (mid + side) * g;
            *y = (mid - side) * g;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{run, sine};

    #[test]
    fn zero_is_mono_and_one_is_identity() {
        let sr = 48_000.0;
        let a = sine(300.0, 0.5, sr, 512);
        let b = sine(700.0, 0.3, sr, 512);
        let mut w = StereoWidth::new(sr);
        let (mut l, mut r) = (a.clone(), b.clone());
        run(&mut w, &mut l, &mut r, 120.0);
        for i in 0..512 {
            assert!((l[i] - a[i]).abs() < 1e-6 && (r[i] - b[i]).abs() < 1e-6);
        }
        w.set_param(p::WIDTH, 0.0);
        w.reset();
        let (mut l, mut r) = (a.clone(), b.clone());
        run(&mut w, &mut l, &mut r, 120.0);
        assert!(l.iter().zip(&r).all(|(x, y)| (x - y).abs() < 1e-6));
    }
}
