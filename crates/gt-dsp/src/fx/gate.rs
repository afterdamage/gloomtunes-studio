//! Noise gate and downward expander.

use super::{clamp_param, one_pole_coef, DelayLine, Effect, FxContext, Smooth};
use crate::{db_to_gain, gain_to_db};
use std::f32::consts::TAU;

mod p {
    pub const THRESHOLD: usize = 0;
    pub const RANGE: usize = 1;
    pub const RATIO: usize = 2;
    pub const ATTACK: usize = 3;
    pub const HOLD: usize = 4;
    pub const RELEASE: usize = 5;
    pub const LOOKAHEAD: usize = 6;
    pub const SIDECHAIN: usize = 7;
    pub const COUNT: usize = 8;
}

const MAX_LOOKAHEAD_MS: f32 = 10.0;
/// The level detector holds peaks and lets go this fast, so a low note's zero crossings do not
/// count as silence (hold covers the rest).
const DETECTOR_MS: f32 = 1.0;

/// Gate/expander: below the threshold the gain drops by `ratio - 1` dB for every dB under it
/// (a ratio of 20 or more is a gate), never further than `range`. The gain opens with the
/// attack time, stays open for the hold time once the level falls under the threshold, then
/// closes with the release time (one-pole times in linear gain). With look-ahead the audio is
/// delayed and the detector hears it early, so the gate is open before a hit.
#[derive(Debug, Clone)]
pub struct Gate {
    sr: f32,
    threshold: Smooth,
    range: Smooth,
    ratio: f32,
    attack: f32,
    release: f32,
    detector_release: f32,
    hold_frames: u32,
    lookahead: usize,
    use_sidechain: bool,
    left: DelayLine,
    right: DelayLine,
    level: f32,
    gain: f32,
    hold: u32,
}

impl Gate {
    /// A gate at -40 dB.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let cap = (MAX_LOOKAHEAD_MS * 0.001 * sr) as usize + 4;
        let mut g = Self {
            sr,
            threshold: Smooth::new(-40.0, 20.0, sr),
            range: Smooth::new(-80.0, 20.0, sr),
            ratio: 20.0,
            attack: 0.0,
            release: 0.0,
            detector_release: one_pole_coef(1000.0 / (TAU * DETECTOR_MS), sr),
            hold_frames: 0,
            lookahead: 0,
            use_sidechain: false,
            left: DelayLine::new(cap),
            right: DelayLine::new(cap),
            level: 0.0,
            gain: 0.0,
            hold: 0,
        };
        for (i, v) in [(p::ATTACK, 0.5), (p::HOLD, 20.0), (p::RELEASE, 100.0)] {
            g.set_param(i, v);
        }
        g.reset();
        g
    }

    fn coef(&self, ms: f32) -> f32 {
        one_pole_coef(1000.0 / (TAU * ms), self.sr)
    }
}

impl Effect for Gate {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            p::THRESHOLD => self.threshold.set(clamp_param(v, -80.0, 0.0, -40.0)),
            p::RANGE => self.range.set(clamp_param(v, -80.0, 0.0, -80.0)),
            p::RATIO => self.ratio = clamp_param(v, 1.0, 20.0, 20.0),
            p::ATTACK => self.attack = self.coef(clamp_param(v, 0.01, 100.0, 0.5)),
            p::HOLD => {
                self.hold_frames = (clamp_param(v, 0.0, 1000.0, 20.0) * 0.001 * self.sr) as u32;
            }
            p::RELEASE => self.release = self.coef(clamp_param(v, 1.0, 2000.0, 100.0)),
            p::LOOKAHEAD => {
                let ms = clamp_param(v, 0.0, MAX_LOOKAHEAD_MS, 0.0);
                self.lookahead =
                    ((ms * 0.001 * self.sr).round() as usize).min(self.left.max_delay() as usize);
            }
            p::SIDECHAIN => self.use_sidechain = clamp_param(v, 0.0, 1.0, 0.0) >= 0.5,
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.threshold.snap();
        self.range.snap();
        self.left.clear();
        self.right.clear();
        self.level = 0.0;
        self.gain = db_to_gain(self.range.value());
        self.hold = 0;
    }

    fn latency(&self) -> usize {
        self.lookahead
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &FxContext) {
        let key = if self.use_sidechain {
            ctx.sidechain
        } else {
            None
        };
        for i in 0..l.len().min(r.len()) {
            let (kl, kr) = match key {
                Some((a, b)) => (
                    a.get(i).copied().unwrap_or(0.0),
                    b.get(i).copied().unwrap_or(0.0),
                ),
                None => (l[i], r[i]),
            };
            let peak = kl.abs().max(kr.abs());
            if peak > self.level {
                self.level = peak;
            } else {
                self.level += (peak - self.level) * self.detector_release;
            }
            let level_db = gain_to_db(self.level.max(1e-6));
            let threshold = self.threshold.next_value();
            let range = self.range.next_value();
            let target = if level_db >= threshold {
                self.hold = self.hold_frames;
                1.0
            } else {
                db_to_gain(((level_db - threshold) * (self.ratio - 1.0)).max(range))
            };
            if target > self.gain {
                self.gain += (target - self.gain) * self.attack;
            } else if self.hold > 0 {
                self.hold -= 1;
            } else {
                self.gain += (target - self.gain) * self.release;
            }
            self.left.push(l[i]);
            self.right.push(r[i]);
            l[i] = self.left.read_int(self.lookahead) * self.gain;
            r[i] = self.right.read_int(self.lookahead) * self.gain;
        }
    }

    fn meter(&self) -> f32 {
        -gain_to_db(self.gain.max(1e-6))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{db, rms, run, sine};

    const SR: f32 = 48_000.0;
    const MS: usize = 48;

    /// 100 ms of silence, 200 ms of a loud tone, then 400 ms of the same tone 60 dB down.
    fn burst() -> Vec<f32> {
        let loud = sine(1000.0, 0.5, SR, 700 * MS);
        (0..700 * MS)
            .map(|i| match i / MS {
                0..100 => 0.0,
                100..300 => loud[i],
                _ => loud[i] * 0.001,
            })
            .collect()
    }

    fn gated(setup: &[(usize, f32)]) -> (Vec<f32>, Vec<f32>) {
        let mut g = Gate::new(SR);
        for &(i, v) in setup {
            g.set_param(i, v);
        }
        g.reset();
        let x = burst();
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut g, &mut l, &mut r, 120.0);
        (x, l)
    }

    /// Gain in dB over `ms` (a range of milliseconds), output against input.
    fn gain(x: &[f32], y: &[f32], ms: std::ops::Range<usize>) -> f32 {
        let s = ms.start * MS..ms.end * MS;
        db(rms(&y[s.clone()]) / rms(&x[s]))
    }

    #[test]
    fn the_gate_opens_holds_and_closes_on_time() {
        let (x, y) = gated(&[(p::ATTACK, 1.0), (p::HOLD, 50.0), (p::RELEASE, 20.0)]);
        // Open within a few attack times of the hit, and fully open after that.
        assert!(gain(&x, &y, 105..300).abs() < 0.1);
        // The quiet tail passes while the gate holds (the detector needs a few ms to fall).
        assert!(
            gain(&x, &y, 310..345).abs() < 0.1,
            "{}",
            gain(&x, &y, 310..345)
        );
        // Then it closes: after hold plus five release times it is down by the full range.
        assert!(gain(&x, &y, 600..700) < -70.0, "{}", gain(&x, &y, 600..700));
        assert!((y[0..100 * MS]).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn an_expander_follows_its_ratio_and_range() {
        // Threshold -40 dB: the tail at -66 dBFS peak is 26 dB under; 2:1 takes 26 dB more,
        // and a -12 dB range stops it at -12.
        let fast = [(p::HOLD, 0.0), (p::RELEASE, 10.0)];
        let (x, y) = gated(&[fast[0], fast[1], (p::RATIO, 2.0), (p::RANGE, -80.0)]);
        let g = gain(&x, &y, 500..700);
        assert!((g + 26.0).abs() < 1.0, "{g}");
        let (x, y) = gated(&[fast[0], fast[1], (p::RATIO, 2.0), (p::RANGE, -12.0)]);
        let g = gain(&x, &y, 500..700);
        assert!((g + 12.0).abs() < 0.2, "{g}");
    }

    #[test]
    fn look_ahead_opens_before_the_hit() {
        let (x, y) = gated(&[(p::ATTACK, 1.0), (p::LOOKAHEAD, 5.0)]);
        assert_eq!(Gate::new(SR).latency(), 0);
        // The output is the input 5 ms later; its first millisecond already passes nearly
        // whole, where a gate without look-ahead cuts it.
        let late = 5 * MS;
        let onset = |y: &[f32], at: usize| db(rms(&y[at..at + MS]) / rms(&x[100 * MS..101 * MS]));
        assert!(
            onset(&y, 100 * MS + late) > -1.5,
            "{}",
            onset(&y, 100 * MS + late)
        );
        let (_, plain) = gated(&[(p::ATTACK, 1.0)]);
        assert!(
            onset(&plain, 100 * MS) < -2.0,
            "{}",
            onset(&plain, 100 * MS)
        );
    }

    #[test]
    fn a_sidechain_key_opens_the_gate() {
        let mut g = Gate::new(SR);
        g.set_param(p::SIDECHAIN, 1.0);
        g.reset();
        let quiet = sine(500.0, 0.001, SR, 64);
        let key = sine(500.0, 0.5, SR, 64);
        let mut out = 0.0;
        for _ in 0..100 {
            let (mut l, mut r) = (quiet.clone(), quiet.clone());
            let ctx = FxContext {
                bpm: 120.0,
                sidechain: Some((&key, &key)),
                beats: None,
            };
            g.process(&mut l, &mut r, &ctx);
            out = rms(&l) / rms(&quiet);
        }
        assert!((out - 1.0).abs() < 1e-3, "{out}");
    }
}
