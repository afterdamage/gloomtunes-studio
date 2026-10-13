//! Multiband compressor and maximizer.

use super::{clamp_param, one_pole_coef, Biquad, BiquadCoeffs, Effect, FxContext, Limiter, Smooth};
use crate::{db_to_gain, gain_to_db};
use std::f32::consts::{FRAC_1_SQRT_2, TAU};

mod p {
    pub const BANDS: usize = 0;
    pub const X_LOW: usize = 1;
    pub const X_MID: usize = 2;
    pub const X_HIGH: usize = 3;
    /// Then threshold, ratio and gain for each of the four bands.
    pub const BAND0: usize = 4;
    pub const ATTACK: usize = 16;
    pub const RELEASE: usize = 17;
    pub const LIMITER: usize = 18;
    pub const DRIVE: usize = 19;
    pub const CEILING: usize = 20;
    pub const COUNT: usize = 21;
}

/// One Linkwitz-Riley 4th-order split: two Butterworth sections for each side. Its low and
/// high outputs add up to an all-pass, so the bands sum to a flat magnitude.
#[derive(Debug, Clone, Copy, Default)]
struct Split {
    lp: [Biquad; 2],
    hp: [Biquad; 2],
}

impl Split {
    fn tune(&mut self, f: f32, sr: f32) {
        let lp = BiquadCoeffs::low_pass(f, FRAC_1_SQRT_2, sr);
        let hp = BiquadCoeffs::high_pass(f, FRAC_1_SQRT_2, sr);
        for b in &mut self.lp {
            b.c = lp;
        }
        for b in &mut self.hp {
            b.c = hp;
        }
    }

    fn reset(&mut self) {
        for b in self.lp.iter_mut().chain(&mut self.hp) {
            b.reset();
        }
    }

    #[inline]
    fn low(&mut self, x: f32) -> f32 {
        let y = self.lp[0].process(x);
        self.lp[1].process(y)
    }

    #[inline]
    fn high(&mut self, x: f32) -> f32 {
        let y = self.hp[0].process(x);
        self.hp[1].process(y)
    }

    /// The same phase shift as the split, without splitting (for bands that skip it).
    #[inline]
    fn all_pass(&mut self, x: f32) -> f32 {
        self.low(x) + self.high(x)
    }
}

/// One channel's crossover network for up to three splits.
#[derive(Debug, Clone, Copy, Default)]
struct Network {
    splits: [Split; 3],
    /// `comp[i][j]`: band `i` passes through an all-pass copy of split `j > i`.
    comp: [[Split; 3]; 3],
}

/// Multiband compressor: Linkwitz-Riley crossovers cut the signal into 3 or 4 bands that add
/// back up flat, each band gets its own compressor (stereo-linked peak detector, threshold,
/// ratio, make-up gain), and the sum can go through the look-ahead limiter with input drive,
/// which makes it a maximizer.
#[derive(Debug, Clone)]
pub struct Multiband {
    sr: f32,
    four: bool,
    freqs: [f32; 3],
    nets: [Network; 2],
    threshold: [Smooth; 4],
    ratio: [f32; 4],
    gain: [Smooth; 4],
    attack: f32,
    release: f32,
    reduction: [f32; 4],
    limit: bool,
    limiter: Limiter,
}

impl Multiband {
    /// Three bands at 120 Hz and 2.5 kHz, gentle settings, limiter on.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let mut m = Self {
            sr,
            four: false,
            freqs: [120.0, 1000.0, 2500.0],
            nets: [Network::default(); 2],
            threshold: [Smooth::new(-18.0, 20.0, sr); 4],
            ratio: [2.0; 4],
            gain: [Smooth::new(1.0, 20.0, sr); 4],
            attack: 0.0,
            release: 0.0,
            reduction: [0.0; 4],
            limit: true,
            limiter: Limiter::new(sr),
        };
        m.set_param(p::ATTACK, 10.0);
        m.set_param(p::RELEASE, 150.0);
        m.retune();
        m
    }

    /// The crossover frequencies in use, low to high.
    fn crossovers(&self) -> ([f32; 3], usize) {
        if self.four {
            (self.freqs, 3)
        } else {
            ([self.freqs[0], self.freqs[2], 0.0], 2)
        }
    }

    fn retune(&mut self) {
        let (f, n) = self.crossovers();
        // Keep the splits in order and away from Nyquist.
        let mut lo = 10.0;
        let mut fs = [0.0; 3];
        for (i, slot) in fs.iter_mut().enumerate().take(n) {
            *slot = f[i].max(lo * 1.25).min(0.45 * self.sr);
            lo = *slot;
        }
        for net in &mut self.nets {
            for (j, &f) in fs.iter().enumerate().take(n) {
                net.splits[j].tune(f, self.sr);
                for row in net.comp.iter_mut().take(j) {
                    row[j].tune(f, self.sr);
                }
            }
        }
    }

    /// Band index (0 to 3) of the `k`-th band in use.
    fn band(&self, k: usize) -> usize {
        if self.four || k < 2 {
            k
        } else {
            3
        }
    }
}

impl Effect for Multiband {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        let coef = |ms: f32, sr: f32| one_pole_coef(1000.0 / (TAU * ms), sr);
        match index {
            p::BANDS => {
                self.four = clamp_param(v, 0.0, 1.0, 0.0) >= 0.5;
                self.retune();
            }
            p::X_LOW | p::X_MID | p::X_HIGH => {
                let (lo, hi, def) = [
                    (20.0, 1000.0, 120.0),
                    (200.0, 5000.0, 1000.0),
                    (1000.0, 16_000.0, 2500.0),
                ][index - p::X_LOW];
                self.freqs[index - p::X_LOW] = clamp_param(v, lo, hi, def);
                self.retune();
            }
            i if (p::BAND0..p::BAND0 + 12).contains(&i) => {
                let (b, k) = ((i - p::BAND0) / 3, (i - p::BAND0) % 3);
                match k {
                    0 => self.threshold[b].set(clamp_param(v, -60.0, 0.0, -18.0)),
                    1 => self.ratio[b] = clamp_param(v, 1.0, 20.0, 2.0),
                    _ => self.gain[b].set(db_to_gain(clamp_param(v, -24.0, 24.0, 0.0))),
                }
            }
            p::ATTACK => self.attack = coef(clamp_param(v, 0.1, 200.0, 10.0), self.sr),
            p::RELEASE => self.release = coef(clamp_param(v, 5.0, 2000.0, 150.0), self.sr),
            p::LIMITER => self.limit = clamp_param(v, 0.0, 1.0, 1.0) >= 0.5,
            p::DRIVE => self.limiter.set_param(0, clamp_param(v, 0.0, 24.0, 0.0)),
            p::CEILING => self.limiter.set_param(1, clamp_param(v, -12.0, 0.0, -0.3)),
            _ => {}
        }
    }

    fn reset(&mut self) {
        for net in &mut self.nets {
            for s in net.splits.iter_mut().chain(net.comp.iter_mut().flatten()) {
                s.reset();
            }
        }
        for s in self.threshold.iter_mut().chain(&mut self.gain) {
            s.snap();
        }
        self.reduction = [0.0; 4];
        self.limiter.reset();
    }

    fn latency(&self) -> usize {
        if self.limit {
            self.limiter.latency()
        } else {
            0
        }
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &FxContext) {
        let n = if self.four { 3 } else { 2 };
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            // Split both channels.
            let mut bands = [[0.0_f32; 4]; 2];
            for ((s, net), out) in [*x, *y].into_iter().zip(&mut self.nets).zip(&mut bands) {
                let mut rest = s;
                for (i, band) in out.iter_mut().enumerate().take(n) {
                    let mut b = net.splits[i].low(rest);
                    rest = net.splits[i].high(rest);
                    for comp in &mut net.comp[i][i + 1..n] {
                        b = comp.all_pass(b);
                    }
                    *band = b;
                }
                out[n] = rest;
            }
            // Compress each band (stereo-linked) and sum.
            let (mut ol, mut or) = (0.0, 0.0);
            for (k, (&bl, &br)) in bands[0].iter().zip(&bands[1]).enumerate().take(n + 1) {
                let b = self.band(k);
                let level = gain_to_db(bl.abs().max(br.abs()).max(1e-6));
                let over = (level - self.threshold[b].next_value()).max(0.0);
                let target = over * (1.0 - 1.0 / self.ratio[b]);
                let c = if target > self.reduction[b] {
                    self.attack
                } else {
                    self.release
                };
                self.reduction[b] += (target - self.reduction[b]) * c;
                let g = db_to_gain(-self.reduction[b]) * self.gain[b].next_value();
                ol += bl * g;
                or += br * g;
            }
            *x = ol;
            *y = or;
        }
        if self.limit {
            self.limiter.process(l, r, ctx);
        }
    }

    fn meter(&self) -> f32 {
        let bands = self.reduction.iter().fold(0.0_f32, |m, &v| m.max(v));
        if self.limit {
            bands.max(self.limiter.meter())
        } else {
            bands
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{db, rms, run, sine};

    const SR: f32 = 48_000.0;

    fn neutral(four: bool) -> Multiband {
        let mut m = Multiband::new(SR);
        m.set_param(p::BANDS, if four { 1.0 } else { 0.0 });
        m.set_param(p::LIMITER, 0.0);
        for b in 0..4 {
            m.set_param(p::BAND0 + 3 * b + 1, 1.0);
        }
        m.reset();
        m
    }

    #[test]
    fn the_crossovers_sum_flat() {
        for four in [false, true] {
            let mut worst = 0.0_f32;
            for f in [
                30.0, 90.0, 120.0, 160.0, 500.0, 1000.0, 1400.0, 2500.0, 4000.0, 12_000.0,
            ] {
                let mut m = neutral(four);
                let x = sine(f, 0.25, SR, 24_000);
                let (mut l, mut r) = (x.clone(), x.clone());
                run(&mut m, &mut l, &mut r, 120.0);
                worst = worst.max(db(rms(&l[12_000..]) / rms(&x[12_000..])).abs());
            }
            assert!(worst < 0.05, "four bands {four}: {worst} dB");
        }
        // An impulse comes out with the same total energy (an all-pass, not a copy).
        let mut m = neutral(true);
        let mut l = vec![0.0; 8192];
        l[0] = 1.0;
        let mut r = l.clone();
        run(&mut m, &mut l, &mut r, 120.0);
        let energy: f32 = l.iter().map(|v| v * v).sum();
        assert!((energy - 1.0).abs() < 1e-3, "{energy}");
    }

    #[test]
    fn only_the_loud_band_is_compressed() {
        let mut m = neutral(false);
        // Low band: threshold -30 dB, 10:1.
        m.set_param(p::BAND0, -30.0);
        m.set_param(p::BAND0 + 1, 10.0);
        m.reset();
        let low: Vec<f32> = sine(60.0, 0.5, SR, 48_000);
        let high: Vec<f32> = sine(6000.0, 0.5, SR, 48_000);
        let x: Vec<f32> = low.iter().zip(&high).map(|(a, b)| a + b).collect();
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut m, &mut l, &mut r, 120.0);
        let tail = &l[24_000..];
        let at = |f: f32| crate::fx::test_util::tone(tail, f, SR);
        // The 6 kHz tone is untouched, the 60 Hz one is pulled down by about 21 dB.
        assert!(db(at(6000.0) / 0.5).abs() < 0.3, "{}", db(at(6000.0) / 0.5));
        assert!(db(at(60.0) / 0.5) < -15.0, "{}", db(at(60.0) / 0.5));
        assert!(m.meter() > 15.0);
    }

    #[test]
    fn the_limiter_holds_the_ceiling() {
        let mut m = Multiband::new(SR);
        m.set_param(p::DRIVE, 12.0);
        m.set_param(p::CEILING, -1.0);
        m.reset();
        assert!(m.latency() > 0);
        let x = sine(200.0, 0.9, SR, 24_000);
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut m, &mut l, &mut r, 120.0);
        assert!(l.iter().all(|v| v.abs() <= db_to_gain(-1.0) + 1e-4));
        m.set_param(p::LIMITER, 0.0);
        assert_eq!(m.latency(), 0);
    }
}
