//! 2x up- and downsampling with polyphase IIR half-band filters.
//!
//! The half-band low-pass is two chains of first-order all-pass sections (in z⁻²), one per
//! polyphase branch, with coefficients from the elliptic design in Laurent de Soras' public
//! domain HIIR library (WTFPL). With [`COEFS`] sections the stopband is about 100 dB down from
//! 0.55 of the low rate's Nyquist; the passband is flat but not linear-phase (no reported
//! latency, a few samples of group delay near the top).

/// Number of all-pass sections (split over the two branches).
pub const COEFS: usize = 8;
/// Transition width as a fraction of the high rate: the filter is flat to 0.5 − this.
const TRANSITION: f64 = 0.05;

/// Elliptic half-band coefficients (HIIR's `PolyphaseIir2Designer`).
fn design() -> [f32; COEFS] {
    use std::f64::consts::PI;
    let k = ((1.0 - 2.0 * TRANSITION) * PI / 4.0).tan().powi(2);
    let kk = (1.0 - k * k).powf(0.25);
    let e = 0.5 * (1.0 - kk) / (1.0 + kk);
    let e4 = e.powi(4);
    let q = e * (1.0 + e4 * (2.0 + e4 * (15.0 + 150.0 * e4)));
    let order = (COEFS * 2 + 1) as f64;
    let mut out = [0.0; COEFS];
    for (index, c) in out.iter_mut().enumerate() {
        let c_ = (index + 1) as f64;
        let mut num = 0.0;
        let mut sign = 1.0;
        for i in 0..64 {
            let i = f64::from(i);
            let term = q.powf(i * (i + 1.0)) * ((i * 2.0 + 1.0) * c_ * PI / order).sin() * sign;
            num += term;
            sign = -sign;
            if term.abs() <= 1e-100 {
                break;
            }
        }
        let mut den = 0.0;
        let mut sign = -1.0;
        for i in 1..64 {
            let i = f64::from(i);
            let term = q.powf(i * i) * (i * 2.0 * c_ * PI / order).cos() * sign;
            den += term;
            sign = -sign;
            if term.abs() <= 1e-100 {
                break;
            }
        }
        let ww = num * q.powf(0.25) / (den + 0.5);
        let wwsq = ww * ww;
        let x = ((1.0 - wwsq * k) * (1.0 - wwsq / k)).sqrt() / (1.0 + wwsq);
        *c = ((1.0 - x) / (1.0 + x)) as f32;
    }
    out
}

/// One all-pass section: `y = c·(x − y₁) + x₁`.
#[derive(Debug, Clone, Copy, Default)]
struct Stage {
    x1: f32,
    y1: f32,
}

impl Stage {
    #[inline]
    fn tick(&mut self, c: f32, x: f32) -> f32 {
        let y = (x - self.y1) * c + self.x1;
        self.x1 = x;
        self.y1 = y;
        y
    }
}

/// Doubles the rate of one channel.
#[derive(Debug, Clone)]
pub struct Upsampler2x {
    coefs: [f32; COEFS],
    stages: [Stage; COEFS],
}

impl Default for Upsampler2x {
    fn default() -> Self {
        Self {
            coefs: design(),
            stages: [Stage::default(); COEFS],
        }
    }
}

impl Upsampler2x {
    /// Clears the filter state.
    pub fn reset(&mut self) {
        self.stages = [Stage::default(); COEFS];
    }

    /// The two high-rate samples for one input sample.
    #[inline]
    pub fn process(&mut self, x: f32) -> [f32; 2] {
        let (mut even, mut odd) = (x, x);
        for i in (0..COEFS).step_by(2) {
            even = self.stages[i].tick(self.coefs[i], even);
            odd = self.stages[i + 1].tick(self.coefs[i + 1], odd);
        }
        [even, odd]
    }
}

/// Halves the rate of one channel.
#[derive(Debug, Clone, Copy)]
pub struct Downsampler2x {
    coefs: [f32; COEFS],
    stages: [Stage; COEFS],
}

impl Default for Downsampler2x {
    fn default() -> Self {
        Self {
            coefs: design(),
            stages: [Stage::default(); COEFS],
        }
    }
}

impl Downsampler2x {
    /// Clears the filter state.
    pub fn reset(&mut self) {
        self.stages = [Stage::default(); COEFS];
    }

    /// One low-rate sample from two high-rate samples.
    #[inline]
    pub fn process(&mut self, x: [f32; 2]) -> f32 {
        let (mut a, mut b) = (x[1], x[0]);
        for i in (0..COEFS).step_by(2) {
            a = self.stages[i].tick(self.coefs[i], a);
            b = self.stages[i + 1].tick(self.coefs[i + 1], b);
        }
        0.5 * (a + b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{db, tone};

    const SR: f32 = 48_000.0;

    #[test]
    fn coefficients_are_stable_all_passes() {
        let c = design();
        assert!(c.iter().all(|&x| x > 0.0 && x < 1.0), "{c:?}");
        assert!(c.windows(2).all(|w| w[0] < w[1]), "{c:?}");
    }

    #[test]
    fn up_then_down_keeps_the_band_and_removes_images() {
        // A 5 kHz tone upsampled: the 96 kHz signal has the tone and no image at 43 kHz.
        let n = 9600;
        let x: Vec<f32> = (0..n)
            .map(|i| (std::f32::consts::TAU * 5000.0 * i as f32 / SR).sin())
            .collect();
        let mut up = Upsampler2x::default();
        let hi: Vec<f32> = x.iter().flat_map(|&v| up.process(v)).collect();
        let tail = &hi[hi.len() / 2..];
        assert!(db(tone(tail, 5000.0, 2.0 * SR)).abs() < 0.1);
        let image = db(tone(tail, 43_000.0, 2.0 * SR));
        assert!(image < -90.0, "{image}");
        // Back down: the tone survives.
        let mut down = Downsampler2x::default();
        let lo: Vec<f32> = hi.chunks(2).map(|c| down.process([c[0], c[1]])).collect();
        assert!(db(tone(&lo[n / 2..], 5000.0, SR)).abs() < 0.1);
        // A 40 kHz tone at the high rate would fold to 8 kHz; the filter removes it first.
        let hi: Vec<f32> = (0..2 * n)
            .map(|i| (std::f32::consts::TAU * 40_000.0 * i as f32 / (2.0 * SR)).sin())
            .collect();
        let mut down = Downsampler2x::default();
        let lo: Vec<f32> = hi.chunks(2).map(|c| down.process([c[0], c[1]])).collect();
        let folded = db(tone(&lo[n / 2..], 8000.0, SR));
        assert!(folded < -90.0, "{folded}");
    }
}
