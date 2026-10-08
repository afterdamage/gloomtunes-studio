//! Stereo chorus.

use super::{clamp_param, DelayLine, Effect, FxContext, Smooth};
use std::f32::consts::TAU;

const MAX_MS: f32 = 50.0;

mod p {
    pub const RATE: usize = 0;
    pub const DEPTH: usize = 1;
    pub const DELAY: usize = 2;
    pub const SPREAD: usize = 3;
    pub const MIX: usize = 4;
    pub const COUNT: usize = 5;
}

/// Chorus: each side reads its input from a short delay whose length is swept by a sine LFO,
/// which detunes the copy slightly up and down; mixed with the dry signal it thickens the sound.
/// The right LFO runs `spread × 180°` behind the left, which widens the image.
#[derive(Debug, Clone)]
pub struct Chorus {
    sr: f32,
    left: DelayLine,
    right: DelayLine,
    phase: f32,
    rate_hz: f32,
    depth: Smooth,
    base: Smooth,
    spread: Smooth,
    mix: Smooth,
}

impl Chorus {
    /// A gentle default chorus.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let cap = (MAX_MS * 0.001 * sr) as usize + 8;
        let ms = |v: f32| v * 0.001 * sr;
        Self {
            sr,
            left: DelayLine::new(cap),
            right: DelayLine::new(cap),
            phase: 0.0,
            rate_hz: 0.6,
            depth: Smooth::new(ms(3.0), 20.0, sr),
            base: Smooth::new(ms(12.0), 50.0, sr),
            spread: Smooth::new(0.5, 20.0, sr),
            mix: Smooth::new(0.5, 20.0, sr),
        }
    }
}

impl Effect for Chorus {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        let ms = 0.001 * self.sr;
        match index {
            p::RATE => self.rate_hz = clamp_param(v, 0.05, 8.0, 0.6),
            p::DEPTH => self.depth.set(clamp_param(v, 0.0, 10.0, 3.0) * ms),
            p::DELAY => self.base.set(clamp_param(v, 1.0, 30.0, 12.0) * ms),
            p::SPREAD => self.spread.set(clamp_param(v, 0.0, 1.0, 0.5)),
            p::MIX => self.mix.set(clamp_param(v, 0.0, 1.0, 0.5)),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.left.clear();
        self.right.clear();
        self.phase = 0.0;
        self.depth.snap();
        self.base.snap();
        self.spread.snap();
        self.mix.snap();
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &FxContext) {
        let inc = self.rate_hz / self.sr;
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            self.left.push(*x);
            self.right.push(*y);
            let depth = self.depth.next_value();
            let base = self.base.next_value();
            let offset = self.spread.next_value() * 0.5;
            let ml = (TAU * self.phase).sin();
            let mr = (TAU * (self.phase - offset)).sin();
            self.phase += inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            // The base delay is at least one depth, so the read never reaches "now".
            let dl = (base + depth * ml).max(1.0);
            let dr = (base + depth * mr).max(1.0);
            let wl = self.left.read(dl);
            let wr = self.right.read(dr);
            let mix = self.mix.next_value();
            *x += (wl - *x) * mix;
            *y += (wr - *y) * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{rms, run, sine};

    #[test]
    fn dry_mix_is_identity_and_wet_moves_the_pitch() {
        let sr = 48_000.0;
        let mut c = Chorus::new(sr);
        c.set_param(p::MIX, 0.0);
        c.reset();
        let x = sine(440.0, 0.5, sr, 9600);
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut c, &mut l, &mut r, 120.0);
        assert_eq!(l, x);

        c.set_param(p::MIX, 1.0);
        c.set_param(p::SPREAD, 1.0);
        c.reset();
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut c, &mut l, &mut r, 120.0);
        // Level is kept, but the two sides now differ (opposite LFO phases).
        assert!((rms(&l[4800..]) - rms(&x[4800..])).abs() < 0.05);
        let diff = l
            .iter()
            .zip(&r)
            .skip(4800)
            .map(|(a, b)| (a - b).abs())
            .sum::<f32>();
        assert!(diff > 10.0, "{diff}");
    }
}
