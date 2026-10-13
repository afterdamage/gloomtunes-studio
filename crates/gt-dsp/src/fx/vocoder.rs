//! Channel vocoder.

use super::filter::{FilterMode, Svf, SvfCoeffs};
use super::{clamp_param, one_pole_coef, Effect, FxContext, Smooth};
use crate::{db_to_gain, Noise};
use std::f32::consts::TAU;

mod p {
    pub const BANDS: usize = 0;
    pub const CARRIER: usize = 1;
    pub const PITCH: usize = 2;
    pub const WIDTH: usize = 3;
    pub const ATTACK: usize = 4;
    pub const RELEASE: usize = 5;
    pub const SHIFT: usize = 6;
    pub const MIX: usize = 7;
    pub const OUTPUT: usize = 8;
    pub const COUNT: usize = 9;
}

/// Band counts, indexed by the `bands` parameter.
pub const BAND_COUNTS: [usize; 5] = [8, 12, 16, 24, 32];
const MAX_BANDS: usize = 32;
const LOWEST_HZ: f32 = 80.0;
const HIGHEST_HZ: f32 = 12_000.0;

/// Where the carrier comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Carrier {
    Saw,
    Noise,
    Sidechain,
}

/// One band: two band-pass stages for the modulator and two for the carrier, and the
/// modulator's envelope.
#[derive(Debug, Clone, Copy, Default)]
struct Band {
    m: [Svf; 2],
    c: [Svf; 2],
    env: [f32; 2],
}

/// Channel vocoder. The input (summed to mono) is the modulator: a bank of 8 to 32 band-pass
/// filters, spaced evenly in octaves from 80 Hz to 12 kHz, measures how loud each band is.
/// The carrier (a saw at the pitch knob, white noise, or the strip's sidechain) goes through
/// a matching bank, and each carrier band is played at the modulator band's level, so the
/// carrier "speaks" with the input's spectrum. Shift moves the carrier bank up or down
/// against the modulator bank (a formant shift); width sets how much neighbouring bands
/// overlap.
#[derive(Debug, Clone)]
pub struct Vocoder {
    sr: f32,
    count: usize,
    carrier: Carrier,
    width: f32,
    shift: f32,
    mod_coefs: [SvfCoeffs; MAX_BANDS],
    car_coefs: [SvfCoeffs; MAX_BANDS],
    bands: [Band; MAX_BANDS],
    attack: f32,
    release: f32,
    phase: f32,
    freq: Smooth,
    noise: Noise,
    mix: Smooth,
    output: Smooth,
}

impl Vocoder {
    /// 16 bands, saw carrier at 110 Hz.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let flat = SvfCoeffs::new(1000.0, 1.0, sr);
        let mut v = Self {
            sr,
            count: 16,
            carrier: Carrier::Saw,
            width: 1.0,
            shift: 0.0,
            mod_coefs: [flat; MAX_BANDS],
            car_coefs: [flat; MAX_BANDS],
            bands: [Band::default(); MAX_BANDS],
            attack: 0.0,
            release: 0.0,
            phase: 0.0,
            freq: Smooth::new(110.0, 20.0, sr),
            noise: Noise::new(0x5EED),
            mix: Smooth::new(1.0, 20.0, sr),
            output: Smooth::new(1.0, 20.0, sr),
        };
        v.set_param(p::ATTACK, 2.0);
        v.set_param(p::RELEASE, 50.0);
        v.retune();
        v
    }

    /// Centre frequency of band `b` of `n`.
    fn centre(&self, b: usize, n: usize) -> f32 {
        let top = HIGHEST_HZ.min(0.4 * self.sr);
        LOWEST_HZ * (top / LOWEST_HZ).powf(b as f32 / (n - 1).max(1) as f32)
    }

    fn retune(&mut self) {
        let n = self.count;
        let ratio = (self.shift / 12.0).exp2();
        let top = 0.45 * self.sr;
        // Width 1 makes each band as wide as the spacing between band centres.
        let spacing = (self.centre(1, n) / self.centre(0, n)).sqrt();
        let q = 1.0 / (self.width * (spacing - 1.0 / spacing));
        for b in 0..n {
            let f = self.centre(b, n);
            self.mod_coefs[b] = SvfCoeffs::new(f, q, self.sr);
            self.car_coefs[b] = SvfCoeffs::new((f * ratio).clamp(20.0, top), q, self.sr);
        }
    }

    fn coef(&self, ms: f32) -> f32 {
        one_pole_coef(1000.0 / (TAU * ms), self.sr)
    }
}

/// PolyBLEP correction for a saw's jump at phase 0.
#[inline]
fn blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        2.0 * x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + 2.0 * x + 1.0
    } else {
        0.0
    }
}

impl Effect for Vocoder {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            p::BANDS => {
                let i = clamp_param(v, 0.0, 4.0, 2.0).round() as usize;
                self.count = BAND_COUNTS[i];
                self.retune();
            }
            p::CARRIER => {
                self.carrier = match clamp_param(v, 0.0, 2.0, 0.0).round() as i32 {
                    1 => Carrier::Noise,
                    2 => Carrier::Sidechain,
                    _ => Carrier::Saw,
                };
            }
            p::PITCH => self.freq.set(clamp_param(v, 30.0, 1000.0, 110.0)),
            p::WIDTH => {
                self.width = clamp_param(v, 0.25, 2.0, 1.0);
                self.retune();
            }
            p::ATTACK => self.attack = self.coef(clamp_param(v, 0.1, 100.0, 2.0)),
            p::RELEASE => self.release = self.coef(clamp_param(v, 5.0, 1000.0, 50.0)),
            p::SHIFT => {
                self.shift = clamp_param(v, -12.0, 12.0, 0.0);
                self.retune();
            }
            p::MIX => self.mix.set(clamp_param(v, 0.0, 1.0, 1.0)),
            p::OUTPUT => self
                .output
                .set(db_to_gain(clamp_param(v, -24.0, 24.0, 0.0))),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.bands = [Band::default(); MAX_BANDS];
        self.phase = 0.0;
        self.freq.snap();
        self.mix.snap();
        self.output.snap();
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &FxContext) {
        let n = self.count;
        // Narrow bands pass less of a broad carrier; this keeps the level roughly even.
        let makeup = 4.0 * (n as f32 / 16.0 / self.width).sqrt();
        for i in 0..l.len().min(r.len()) {
            let m = 0.5 * (l[i] + r[i]);
            let c = match self.carrier {
                Carrier::Saw => {
                    let dt = self.freq.next_value() / self.sr;
                    self.phase += dt;
                    if self.phase >= 1.0 {
                        self.phase -= 1.0;
                    }
                    2.0 * self.phase - 1.0 - blep(self.phase, dt)
                }
                Carrier::Noise => self.noise.next_value(),
                Carrier::Sidechain => ctx.sidechain.map_or(0.0, |(a, b)| {
                    0.5 * (a.get(i).copied().unwrap_or(0.0) + b.get(i).copied().unwrap_or(0.0))
                }),
            };
            let mut out = 0.0;
            for (b, band) in self.bands.iter_mut().enumerate().take(n) {
                let mc = &self.mod_coefs[b];
                let x = band.m[0].tick(mc, m, FilterMode::BandPass);
                let x = band.m[1].tick(mc, x, FilterMode::BandPass);
                // Two followers in series: the second smooths the first one's ripple, which
                // would otherwise ring the carrier with sidebands.
                let mut level = x.abs();
                for e in &mut band.env {
                    let k = if level > *e {
                        self.attack
                    } else {
                        self.release
                    };
                    *e += (level - *e) * k;
                    level = *e;
                }
                let cc = &self.car_coefs[b];
                let y = band.c[0].tick(cc, c, FilterMode::BandPass);
                let y = band.c[1].tick(cc, y, FilterMode::BandPass);
                // Between two band centres the neighbours' 4-pole band-passes are half a turn
                // apart; flipping every other band makes them add instead of cancel.
                let y = if b % 2 == 1 { -y } else { y };
                out += y * level;
            }
            let wet = out * makeup * self.output.next_value();
            let mix = self.mix.next_value();
            l[i] = l[i] * (1.0 - mix) + wet * mix;
            r[i] = r[i] * (1.0 - mix) + wet * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{db, rms, run, sine, tone};

    const SR: f32 = 48_000.0;

    #[test]
    fn the_carrier_takes_the_modulators_spectrum() {
        for (bands, count) in BAND_COUNTS.into_iter().enumerate() {
            let mut v = Vocoder::new(SR);
            v.set_param(p::BANDS, bands as f32);
            v.reset();
            // A 1 kHz modulator: the saw's harmonics near 1 kHz come through, not those at 4 kHz.
            let x = sine(1000.0, 0.5, SR, 24_000);
            let (mut l, mut r) = (x.clone(), x.clone());
            run(&mut v, &mut l, &mut r, 120.0);
            let tail = &l[12_000..];
            let near = tone(tail, 990.0, SR);
            let far = tone(tail, 3960.0, SR);
            assert!(db(near / far) > 20.0, "{} bands: {}", count, db(near / far));
            assert!(rms(tail) > 0.01, "{} bands: {}", count, rms(tail));
        }
        // No modulator, no output.
        let mut v = Vocoder::new(SR);
        let (mut l, mut r) = (vec![0.0; 4800], vec![0.0; 4800]);
        run(&mut v, &mut l, &mut r, 120.0);
        assert!(rms(&l) < 1e-6);
    }

    #[test]
    fn a_sidechain_carrier_is_used_and_silence_without_one() {
        let mut v = Vocoder::new(SR);
        v.set_param(p::CARRIER, 2.0);
        v.reset();
        let x = sine(1000.0, 0.5, SR, 64);
        let key = sine(1000.0, 0.5, SR, 64);
        let mut level = 0.0;
        for _ in 0..200 {
            let (mut l, mut r) = (x.clone(), x.clone());
            let ctx = FxContext {
                bpm: 120.0,
                sidechain: Some((&key, &key)),
                beats: None,
            };
            v.process(&mut l, &mut r, &ctx);
            level = rms(&l);
        }
        assert!(level > 0.01, "{level}");
        let (mut l, mut r) = (x.clone(), x.clone());
        for _ in 0..200 {
            l.copy_from_slice(&x);
            r.copy_from_slice(&x);
            v.process(&mut l, &mut r, &FxContext::default());
        }
        assert!(rms(&l) < 1e-3, "{}", rms(&l));
    }
}
