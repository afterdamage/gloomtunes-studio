//! Transient shaper.

use super::{clamp_param, one_pole_coef, Effect, FxContext, Smooth};
use crate::{db_to_gain, gain_to_db};
use std::f32::consts::TAU;

mod p {
    pub const ATTACK: usize = 0;
    pub const SUSTAIN: usize = 1;
    pub const OUTPUT: usize = 2;
    pub const COUNT: usize = 3;
}

/// The envelope difference (dB) that counts as a full transient or a full tail.
const FULL_DB: f32 = 12.0;

/// An envelope follower with separate attack and release time constants.
#[derive(Debug, Clone, Copy)]
struct Follower {
    attack: f32,
    release: f32,
    value: f32,
}

impl Follower {
    /// A follower; a time of zero follows instantly.
    fn new(attack_ms: f32, release_ms: f32, sr: f32) -> Self {
        let c = |ms: f32| {
            if ms > 0.0 {
                one_pole_coef(1000.0 / (TAU * ms), sr)
            } else {
                1.0
            }
        };
        Self {
            attack: c(attack_ms),
            release: c(release_ms),
            value: 0.0,
        }
    }

    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        let c = if x > self.value {
            self.attack
        } else {
            self.release
        };
        self.value += (x - self.value) * c;
        self.value
    }
}

/// Transient shaper: level-independent, it compares envelope followers. At each hit a fast
/// follower runs ahead of a slow-attack copy of itself, and their difference (in dB) scales the
/// attack gain; as a note decays a slow-release copy lingers behind the fast follower, and that
/// difference scales the sustain gain. The copies follow the other way at once, so each
/// difference is zero outside its own phase.
#[derive(Debug, Clone)]
pub struct TransientShaper {
    attack: Smooth,
    sustain: Smooth,
    output: Smooth,
    fast: Follower,
    slow_attack: Follower,
    slow_release: Follower,
}

impl TransientShaper {
    /// A neutral shaper.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        Self {
            attack: Smooth::new(0.0, 20.0, sr),
            sustain: Smooth::new(0.0, 20.0, sr),
            output: Smooth::new(1.0, 20.0, sr),
            fast: Follower::new(0.5, 20.0, sr),
            slow_attack: Follower::new(20.0, 0.0, sr),
            slow_release: Follower::new(0.0, 300.0, sr),
        }
    }
}

impl Effect for TransientShaper {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            p::ATTACK => self.attack.set(clamp_param(v, -24.0, 24.0, 0.0)),
            p::SUSTAIN => self.sustain.set(clamp_param(v, -24.0, 24.0, 0.0)),
            p::OUTPUT => self
                .output
                .set(db_to_gain(clamp_param(v, -24.0, 12.0, 0.0))),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.attack.snap();
        self.sustain.snap();
        self.output.snap();
        for f in [
            &mut self.fast,
            &mut self.slow_attack,
            &mut self.slow_release,
        ] {
            f.value = 0.0;
        }
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &FxContext) {
        let db = |v: f32| gain_to_db(v.max(1e-6));
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let level = x.abs().max(y.abs());
            let fast = self.fast.tick(level);
            let hit = db(fast) - db(self.slow_attack.tick(fast));
            let tail = db(self.slow_release.tick(fast)) - db(fast);
            let attack = self.attack.next_value();
            let sustain = self.sustain.next_value();
            let gain_db = attack * (hit / FULL_DB).clamp(0.0, 1.0)
                + sustain * (tail / FULL_DB).clamp(0.0, 1.0);
            let g = db_to_gain(gain_db) * self.output.next_value();
            *x *= g;
            *y *= g;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{db, rms, run};

    const SR: f32 = 48_000.0;
    const MS: usize = 48;

    /// Four drum-like hits: a 200 Hz tone decaying with a 50 ms time constant, every 250 ms.
    fn hits() -> Vec<f32> {
        (0..1000 * MS)
            .map(|i| {
                let t = (i % (250 * MS)) as f32 / SR;
                0.8 * (TAU * 200.0 * t).sin() * (-t / 0.05).exp()
            })
            .collect()
    }

    /// Gain in dB over `ms` of the last hit.
    fn shaped(attack: f32, sustain: f32, ms: std::ops::Range<usize>) -> f32 {
        let mut s = TransientShaper::new(SR);
        s.set_param(p::ATTACK, attack);
        s.set_param(p::SUSTAIN, sustain);
        s.reset();
        let x = hits();
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut s, &mut l, &mut r, 120.0);
        let at = (750 + ms.start) * MS..(750 + ms.end) * MS;
        db(rms(&l[at.clone()]) / rms(&x[at]))
    }

    #[test]
    fn attack_lifts_the_hit_and_sustain_lifts_the_tail() {
        assert!(shaped(12.0, 0.0, 0..5) > 4.0, "{}", shaped(12.0, 0.0, 0..5));
        assert!(shaped(12.0, 0.0, 80..200).abs() < 1.0);
        assert!(shaped(-12.0, 0.0, 0..5) < -4.0);
        assert!(
            shaped(0.0, 12.0, 80..200) > 4.0,
            "{}",
            shaped(0.0, 12.0, 80..200)
        );
        assert!(shaped(0.0, 12.0, 0..5) < shaped(0.0, 12.0, 80..200) - 2.0);
        // Neutral settings change nothing at all.
        let mut s = TransientShaper::new(SR);
        let x = hits();
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut s, &mut l, &mut r, 120.0);
        assert_eq!(l, x);
    }
}
