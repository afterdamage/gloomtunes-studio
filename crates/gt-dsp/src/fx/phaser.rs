//! Phaser.

use super::flanger::lfo_hz;
use super::{clamp_param, Effect, FxContext, Smooth, CONTROL_FRAMES};
use std::f32::consts::{PI, TAU};

mod p {
    pub const STAGES: usize = 0;
    pub const RATE: usize = 1;
    pub const SYNC: usize = 2;
    pub const DEPTH: usize = 3;
    pub const CENTER: usize = 4;
    pub const FEEDBACK: usize = 5;
    pub const SPREAD: usize = 6;
    pub const MIX: usize = 7;
    pub const COUNT: usize = 8;
}

const MAX_STAGES: usize = 12;
/// How far the LFO sweeps the centre at full depth, in octaves each way.
const SWEEP_OCTAVES: f32 = 2.0;

/// Phaser: 2 to 12 first-order all-pass stages in series. Each turns the phase from 0 at DC to
/// -180° at Nyquist, -90° at its break frequency; mixed half and half with the dry signal, the
/// frequencies where the chain reaches an odd multiple of -180° cancel, giving `stages / 2`
/// notches. A sine LFO (free or tempo-synced) sweeps the break frequency.
#[derive(Debug, Clone)]
pub struct Phaser {
    sr: f32,
    stages: usize,
    rate: f32,
    sync: usize,
    depth: Smooth,
    center: Smooth,
    feedback: Smooth,
    spread: Smooth,
    mix: Smooth,
    phase: f32,
    coef: [f32; 2],
    /// Per channel and stage: last input and last output.
    state: [[(f32, f32); MAX_STAGES]; 2],
    last: [f32; 2],
    countdown: usize,
}

impl Phaser {
    /// A four-stage phaser.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        Self {
            sr,
            stages: 4,
            rate: 0.5,
            sync: 0,
            depth: Smooth::new(0.7, 20.0, sr),
            center: Smooth::new(800.0, 20.0, sr),
            feedback: Smooth::new(0.0, 20.0, sr),
            spread: Smooth::new(0.25, 20.0, sr),
            mix: Smooth::new(0.5, 20.0, sr),
            phase: 0.0,
            coef: [0.0; 2],
            state: [[(0.0, 0.0); MAX_STAGES]; 2],
            last: [0.0; 2],
            countdown: 0,
        }
    }

    /// All-pass coefficient for a break frequency.
    fn allpass(f: f32, sr: f32) -> f32 {
        let t = (PI * (f / sr).clamp(1e-5, 0.49)).tan();
        (t - 1.0) / (t + 1.0)
    }

    fn update(&mut self, bpm: f32) {
        let n = CONTROL_FRAMES;
        let depth = self.depth.advance(n);
        let center = self.center.advance(n);
        let offset = self.spread.advance(n) * 0.5;
        for (ch, c) in self.coef.iter_mut().enumerate() {
            let lfo = (TAU * (self.phase - offset * ch as f32)).sin();
            let f = center * (lfo * depth * SWEEP_OCTAVES).exp2();
            *c = Self::allpass(f.clamp(20.0, 0.45 * self.sr), self.sr);
        }
        self.phase = (self.phase + lfo_hz(self.rate, self.sync, bpm) * n as f32 / self.sr).fract();
    }
}

impl Effect for Phaser {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            p::STAGES => self.stages = 2 + 2 * clamp_param(v, 0.0, 5.0, 1.0).round() as usize,
            p::RATE => self.rate = clamp_param(v, 0.02, 10.0, 0.5),
            p::SYNC => self.sync = clamp_param(v, 0.0, 9.0, 0.0).round() as usize,
            p::DEPTH => self.depth.set(clamp_param(v, 0.0, 1.0, 0.7)),
            p::CENTER => self.center.set(clamp_param(v, 50.0, 8000.0, 800.0)),
            p::FEEDBACK => self.feedback.set(clamp_param(v, -0.95, 0.95, 0.0)),
            p::SPREAD => self.spread.set(clamp_param(v, 0.0, 1.0, 0.25)),
            p::MIX => self.mix.set(clamp_param(v, 0.0, 1.0, 0.5)),
            _ => {}
        }
    }

    fn reset(&mut self) {
        for s in [
            &mut self.depth,
            &mut self.center,
            &mut self.feedback,
            &mut self.spread,
            &mut self.mix,
        ] {
            s.snap();
        }
        self.state = [[(0.0, 0.0); MAX_STAGES]; 2];
        self.last = [0.0; 2];
        self.phase = 0.0;
        self.countdown = 0;
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &FxContext) {
        let stages = self.stages;
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            if self.countdown == 0 {
                self.update(ctx.bpm);
                self.countdown = CONTROL_FRAMES;
            }
            self.countdown -= 1;
            let fb = self.feedback.next_value();
            let mix = self.mix.next_value();
            for (ch, s) in [x, y].into_iter().enumerate() {
                let a = self.coef[ch];
                let mut v = *s + fb * self.last[ch];
                for (x1, y1) in &mut self.state[ch][..stages] {
                    let out = a * v + *x1 - a * *y1;
                    *x1 = v;
                    *y1 = out;
                    v = out;
                }
                self.last[ch] = v;
                *s += (v - *s) * mix;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{db, rms, run, sine};

    const SR: f32 = 48_000.0;

    fn level(stages: f32, hz: f32) -> f32 {
        let mut f = Phaser::new(SR);
        f.set_param(p::STAGES, stages);
        f.set_param(p::DEPTH, 0.0);
        f.set_param(p::CENTER, 1000.0);
        f.reset();
        let x = sine(hz, 0.25, SR, 24_000);
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut f, &mut l, &mut r, 120.0);
        db(rms(&l[12_000..]) / rms(&x[12_000..]))
    }

    /// Where an `n`-stage chain broken at `fc` cancels: each stage turns
    /// `2·atan(tan(πf/sr) / tan(πfc/sr))`, so notch `m` is at `tan(πf/sr) = tan(πfc/sr) ·
    /// tan((2m+1)π / 2n)`.
    fn notch(n: usize, m: usize, fc: f32) -> f32 {
        let t = (PI * fc / SR).tan() * ((2 * m + 1) as f32 * PI / (2 * n) as f32).tan();
        t.atan() * SR / PI
    }

    #[test]
    fn notches_sit_where_the_phase_adds_to_half_turns() {
        for (choice, n) in [(0.0, 2), (1.0, 4), (2.0, 6), (5.0, 12)] {
            for m in 0..n / 2 {
                let f = notch(n, m, 1000.0);
                if f > 0.45 * SR {
                    continue;
                }
                assert!(level(choice, f) < -30.0, "{n} stages, notch {m} at {f} Hz");
            }
        }
        // Halfway between the two notches of four stages the chain is back in phase.
        assert!(level(1.0, 1000.0).abs() < 0.1);
        assert!(level(1.0, notch(4, 0, 1000.0)) < -30.0);
    }
}
