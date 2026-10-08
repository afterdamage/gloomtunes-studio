//! Feed-forward compressor with an optional sidechain key.

use super::{clamp_param, Effect, FxContext, Smooth};
use crate::db_to_gain;

/// Parameter indexes (see `gt_core::effects`).
mod p {
    pub const THRESHOLD: usize = 0;
    pub const RATIO: usize = 1;
    pub const ATTACK: usize = 2;
    pub const RELEASE: usize = 3;
    pub const KNEE: usize = 4;
    pub const MAKEUP: usize = 5;
    pub const MIX: usize = 6;
    pub const SIDECHAIN: usize = 7;
    pub const COUNT: usize = 8;
}

/// Compressor: the level of the key (the input itself, or the sidechain when enabled and
/// connected) is measured as the louder of the two channels in dB; levels above the threshold
/// are reduced by the ratio, with a soft knee. The gain reduction follows with separate attack
/// and release times (smoothing in the dB domain, so release sounds even), then makeup gain and
/// a dry/wet mix for parallel compression.
#[derive(Debug, Clone)]
pub struct Compressor {
    sr: f32,
    threshold: Smooth,
    ratio: Smooth,
    knee: f32,
    makeup: Smooth,
    mix: Smooth,
    attack_coef: f32,
    release_coef: f32,
    use_sidechain: bool,
    /// Current gain reduction in dB (≥ 0).
    reduction: f32,
}

fn time_coef(ms: f32, sr: f32) -> f32 {
    1.0 - (-1.0 / (ms * 0.001 * sr).max(1.0)).exp()
}

impl Compressor {
    /// A compressor at its default settings.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        Self {
            sr,
            threshold: Smooth::new(-18.0, 20.0, sr),
            ratio: Smooth::new(4.0, 20.0, sr),
            knee: 6.0,
            makeup: Smooth::new(1.0, 20.0, sr),
            mix: Smooth::new(1.0, 20.0, sr),
            attack_coef: time_coef(10.0, sr),
            release_coef: time_coef(120.0, sr),
            use_sidechain: false,
            reduction: 0.0,
        }
    }

    /// Static curve: gain reduction in dB for a key level in dB.
    fn curve(level_db: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
        let over = level_db - threshold;
        let slope = 1.0 - 1.0 / ratio;
        if knee > 0.0 && over.abs() <= knee * 0.5 {
            // Quadratic blend across the knee, continuous in value and slope.
            let t = over + knee * 0.5;
            slope * t * t / (2.0 * knee)
        } else if over > 0.0 {
            slope * over
        } else {
            0.0
        }
    }
}

impl Effect for Compressor {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            p::THRESHOLD => self.threshold.set(clamp_param(v, -60.0, 0.0, -18.0)),
            p::RATIO => self.ratio.set(clamp_param(v, 1.0, 20.0, 4.0)),
            p::ATTACK => self.attack_coef = time_coef(clamp_param(v, 0.1, 200.0, 10.0), self.sr),
            p::RELEASE => {
                self.release_coef = time_coef(clamp_param(v, 5.0, 2000.0, 120.0), self.sr);
            }
            p::KNEE => self.knee = clamp_param(v, 0.0, 24.0, 6.0),
            p::MAKEUP => self.makeup.set(db_to_gain(clamp_param(v, 0.0, 24.0, 0.0))),
            p::MIX => self.mix.set(clamp_param(v, 0.0, 1.0, 1.0)),
            p::SIDECHAIN => self.use_sidechain = clamp_param(v, 0.0, 1.0, 0.0) >= 0.5,
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.reduction = 0.0;
        self.threshold.snap();
        self.ratio.snap();
        self.makeup.snap();
        self.mix.snap();
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
            let level = kl.abs().max(kr.abs());
            let level_db = 20.0 * level.max(1e-6).log10();
            let target = Self::curve(
                level_db,
                self.threshold.next_value(),
                self.ratio.next_value(),
                self.knee,
            );
            let coef = if target > self.reduction {
                self.attack_coef
            } else {
                self.release_coef
            };
            self.reduction += (target - self.reduction) * coef;
            let wet = db_to_gain(-self.reduction) * self.makeup.next_value();
            let mix = self.mix.next_value();
            let g = 1.0 - mix + mix * wet;
            l[i] *= g;
            r[i] *= g;
        }
    }

    fn meter(&self) -> f32 {
        self.reduction
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{db, rms, run, sine};

    #[test]
    fn steady_tone_follows_the_static_curve() {
        let sr = 48_000.0;
        let mut c = Compressor::new(sr);
        c.set_param(p::THRESHOLD, -20.0);
        c.set_param(p::RATIO, 4.0);
        c.set_param(p::KNEE, 0.0);
        c.set_param(p::RELEASE, 300.0);
        c.reset();
        // A square wave keeps the peak detector steady (a sine would ripple at twice its rate).
        let x: Vec<f32> = (0..96_000)
            .map(|i| if (i / 50) % 2 == 0 { 0.5 } else { -0.5 })
            .collect();
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut c, &mut l, &mut r, 120.0);
        // Input -6 dBFS is 14 dB over: 4:1 leaves 3.5 dB over, so -16.5 dBFS.
        let out = db(rms(&l[48_000..]));
        assert!((out + 16.5).abs() < 0.2, "{out}");
        assert!((c.meter() - 10.5).abs() < 0.2);
    }

    #[test]
    fn below_threshold_is_untouched() {
        let mut c = Compressor::new(48_000.0);
        let x = sine(200.0, 0.05, 48_000.0, 9600); // -26 dBFS peak, threshold -18, knee 6
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut c, &mut l, &mut r, 120.0);
        for (a, b) in l.iter().zip(&x) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn sidechain_key_ducks_the_input() {
        let sr = 48_000.0;
        let mut c = Compressor::new(sr);
        c.set_param(p::SIDECHAIN, 1.0);
        c.set_param(p::THRESHOLD, -30.0);
        c.set_param(p::RATIO, 20.0);
        let key = vec![0.9_f32; 64];
        let quiet = sine(300.0, 0.1, sr, 9600);
        let (mut l, mut r) = (quiet.clone(), quiet.clone());
        let ctx = FxContext {
            bpm: 120.0,
            sidechain: Some((&key, &key)),
        };
        for (a, b) in l.chunks_mut(64).zip(r.chunks_mut(64)) {
            c.process(a, b, &ctx);
        }
        // The quiet input alone is far below threshold, yet it is pushed down by the key.
        let ratio = rms(&l[4800..]) / rms(&quiet[4800..]);
        assert!(db(ratio) < -20.0, "{}", db(ratio));
        // Without the sidechain switch, the key is ignored (and the quiet input is below a
        // normal threshold).
        c.set_param(p::SIDECHAIN, 0.0);
        c.set_param(p::THRESHOLD, -12.0);
        c.reset();
        let (mut l, mut r) = (quiet.clone(), quiet.clone());
        for (a, b) in l.chunks_mut(64).zip(r.chunks_mut(64)) {
            c.process(a, b, &ctx);
        }
        assert!(db(rms(&l[4800..]) / rms(&quiet[4800..])).abs() < 0.1);
    }
}
