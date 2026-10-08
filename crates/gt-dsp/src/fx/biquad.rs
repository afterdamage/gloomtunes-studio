//! Second-order IIR filter sections (the "RBJ Audio EQ Cookbook" designs).

use std::f32::consts::TAU;

/// Normalised biquad coefficients (`a0 = 1`):
/// `y[n] = b0·x[n] + b1·x[n−1] + b2·x[n−2] − a1·y[n−1] − a2·y[n−2]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BiquadCoeffs {
    /// Feed-forward.
    pub b0: f32,
    /// Feed-forward.
    pub b1: f32,
    /// Feed-forward.
    pub b2: f32,
    /// Feedback.
    pub a1: f32,
    /// Feedback.
    pub a2: f32,
}

impl Default for BiquadCoeffs {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Shared terms of the cookbook formulas: `w0 = 2π·f/fs`, `alpha = sin(w0)/(2Q)`.
fn terms(freq: f32, q: f32, sr: f32) -> (f32, f32, f32) {
    let f = freq.clamp(10.0, 0.49 * sr);
    let w0 = TAU * f / sr;
    let (s, c) = w0.sin_cos();
    (c, s / (2.0 * q.max(0.05)), s)
}

impl BiquadCoeffs {
    /// Passes the signal unchanged.
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn normalized(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Peaking ("bell") filter: `gain_db` at `freq`, width set by `q`.
    pub fn bell(freq: f32, gain_db: f32, q: f32, sr: f32) -> Self {
        let a = 10.0_f32.powf(gain_db / 40.0);
        let (c, alpha, _) = terms(freq, q, sr);
        Self::normalized(
            1.0 + alpha * a,
            -2.0 * c,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * c,
            1.0 - alpha / a,
        )
    }

    /// Low shelf: `gain_db` below `freq`; `q` = 0.707 gives the steepest shelf without overshoot.
    pub fn low_shelf(freq: f32, gain_db: f32, q: f32, sr: f32) -> Self {
        let a = 10.0_f32.powf(gain_db / 40.0);
        let (c, alpha, _) = terms(freq, q, sr);
        let k = 2.0 * a.sqrt() * alpha;
        Self::normalized(
            a * ((a + 1.0) - (a - 1.0) * c + k),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - k),
            (a + 1.0) + (a - 1.0) * c + k,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - k,
        )
    }

    /// High shelf: `gain_db` above `freq`.
    pub fn high_shelf(freq: f32, gain_db: f32, q: f32, sr: f32) -> Self {
        let a = 10.0_f32.powf(gain_db / 40.0);
        let (c, alpha, _) = terms(freq, q, sr);
        let k = 2.0 * a.sqrt() * alpha;
        Self::normalized(
            a * ((a + 1.0) + (a - 1.0) * c + k),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            a * ((a + 1.0) + (a - 1.0) * c - k),
            (a + 1.0) - (a - 1.0) * c + k,
            2.0 * ((a - 1.0) - (a + 1.0) * c),
            (a + 1.0) - (a - 1.0) * c - k,
        )
    }

    /// 12 dB/oct low-pass.
    pub fn low_pass(freq: f32, q: f32, sr: f32) -> Self {
        let (c, alpha, _) = terms(freq, q, sr);
        let b = (1.0 - c) / 2.0;
        Self::normalized(b, 1.0 - c, b, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    /// 12 dB/oct high-pass.
    pub fn high_pass(freq: f32, q: f32, sr: f32) -> Self {
        let (c, alpha, _) = terms(freq, q, sr);
        let b = (1.0 + c) / 2.0;
        Self::normalized(b, -(1.0 + c), b, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    /// Notch (band reject) at `freq`.
    pub fn notch(freq: f32, q: f32, sr: f32) -> Self {
        let (c, alpha, _) = terms(freq, q, sr);
        Self::normalized(1.0, -2.0 * c, 1.0, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    /// Magnitude response at `freq`, in dB. Evaluates `|H(e^{jw})|` directly.
    pub fn magnitude_db(&self, freq: f32, sr: f32) -> f32 {
        let w = TAU * freq / sr;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        let num = nr * nr + ni * ni;
        let den = (dr * dr + di * di).max(1e-30);
        10.0 * (num / den).max(1e-30).log10()
    }
}

/// One biquad section in transposed direct form II, which keeps rounding noise low and allows
/// changing coefficients between samples without blowing up.
#[derive(Debug, Clone, Copy, Default)]
pub struct Biquad {
    /// Current coefficients.
    pub c: BiquadCoeffs,
    s1: f32,
    s2: f32,
}

impl Biquad {
    /// Clears the state.
    pub fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }

    /// Filters one sample.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let c = &self.c;
        let y = c.b0 * x + self.s1;
        self.s1 = c.b1 * x - c.a1 * y + self.s2;
        self.s2 = c.b2 * x - c.a2 * y;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{db, rms, sine};

    fn measured_db(c: BiquadCoeffs, f: f32, sr: f32) -> f32 {
        let mut bq = Biquad {
            c,
            ..Biquad::default()
        };
        let x = sine(f, 0.5, sr, 48_000);
        let y: Vec<f32> = x.iter().map(|&v| bq.process(v)).collect();
        db(rms(&y[24_000..]) / rms(&x[24_000..]))
    }

    #[test]
    fn designs_match_their_magnitude_formula() {
        let sr = 48_000.0;
        for c in [
            BiquadCoeffs::bell(1000.0, 6.0, 1.0, sr),
            BiquadCoeffs::low_shelf(200.0, -9.0, 0.707, sr),
            BiquadCoeffs::high_shelf(5000.0, 4.0, 0.707, sr),
            BiquadCoeffs::low_pass(2000.0, 0.707, sr),
            BiquadCoeffs::high_pass(150.0, 0.707, sr),
            BiquadCoeffs::notch(3000.0, 4.0, sr),
        ] {
            for f in [60.0, 200.0, 1000.0, 3100.0, 9000.0] {
                let m = measured_db(c, f, sr);
                let e = c.magnitude_db(f, sr);
                assert!(
                    (m - e).abs() < 0.2,
                    "{c:?} at {f}: measured {m}, formula {e}"
                );
            }
        }
    }

    #[test]
    fn bell_hits_its_gain_at_the_centre() {
        let sr = 48_000.0;
        let c = BiquadCoeffs::bell(1000.0, 6.0, 1.0, sr);
        assert!((c.magnitude_db(1000.0, sr) - 6.0).abs() < 0.01);
        assert!(c.magnitude_db(50.0, sr).abs() < 0.1);
        let lp = BiquadCoeffs::low_pass(1000.0, 0.707, sr);
        // Two octaves above the cutoff a 12 dB/oct filter is about 24 dB down.
        assert!((lp.magnitude_db(4000.0, sr) + 24.0).abs() < 1.5);
    }
}
