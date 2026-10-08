//! 24 dB/octave resonant low-pass in the style of the transistor ladder, as a zero-delay-feedback
//! (ZDF) filter.
//!
//! Four identical one-pole low-passes in series give the 24 dB/oct slope. Feeding the output back
//! to the input, inverted and scaled by `k`, makes the resonant peak at the cutoff; at `k = 4` the
//! loop gain there is 1 and the filter self-oscillates.
//!
//! Each one-pole is a trapezoidal ("TPT") integrator, the bilinear transform of an analog RC stage
//! with the cutoff pre-warped (`g = tan(π·fc / fs)`), so the cutoff is exact up to Nyquist. A
//! naive digital ladder puts a one-sample delay in the feedback path, which detunes the resonance
//! and becomes unstable at high cutoffs. Here the feedback equation is solved for the current
//! sample instead (Zavalishin, "The Art of VA Filter Design"): with `G = g / (1 + g)` the output
//! is `y = (G⁴·x + Σ) / (1 + k·G⁴)`, where `Σ` collects the four stage states. The loop is then
//! stable for every cutoff and every `k` up to 4 without oversampling.
//!
//! Drive saturates the input of the first stage with `tanh`, after the feedback is subtracted, as
//! in the analog circuit. That squashes the resonance at high levels and adds warmth; it can only
//! lower the loop gain, so it keeps the filter stable.
//!
//! The ladder loses bass as resonance rises (its DC gain is `1 / (1 + k)`); half of that loss is
//! made up with input gain `1 + k/2`, which keeps the level more even when sweeping resonance.

use core::f32::consts::PI;

/// Highest usable `k` (self-oscillation starts at 4).
pub const MAX_K: f32 = 4.0;

/// Pre-warped integrator gain for a cutoff in Hz (clamped to 10 Hz .. 0.45·fs).
#[inline]
pub fn ladder_g(cutoff_hz: f32, sample_rate: f32) -> f32 {
    let fc = cutoff_hz.clamp(10.0, 0.45 * sample_rate);
    (PI * fc / sample_rate).tan()
}

/// Feedback amount for a resonance in 0..1.
#[inline]
pub fn ladder_k(resonance: f32) -> f32 {
    resonance.clamp(0.0, 1.0) * MAX_K
}

/// Ladder filter state (four integrators).
#[derive(Debug, Clone, Copy, Default)]
pub struct Ladder {
    s: [f32; 4],
}

impl Ladder {
    /// Clears the state.
    pub fn reset(&mut self) {
        self.s = [0.0; 4];
    }

    /// Filters one sample. `g` from [`ladder_g`], `k` from [`ladder_k`], `drive` ≥ 1 (1 is
    /// clean).
    #[inline]
    pub fn process(&mut self, x: f32, g: f32, k: f32, drive: f32) -> f32 {
        let gg = g / (1.0 + g);
        let inv = 1.0 / (1.0 + g);
        let [s1, s2, s3, s4] = self.s;
        let g2 = gg * gg;
        let g3 = g2 * gg;
        let g4 = g2 * g2;
        let sigma = (g3 * s1 + g2 * s2 + gg * s3 + s4) * inv;
        let x = x * (1.0 + 0.5 * k);
        let y4 = (g4 * x + sigma) / (1.0 + k * g4);
        let mut u = x - k * y4;
        if drive > 1.0 {
            u = fast_tanh(u * drive) / drive.sqrt();
        }
        let stage = |input: f32, s: &mut f32| {
            let v = (input - *s) * gg;
            let y = v + *s;
            *s = y + v;
            y
        };
        let mut st = self.s;
        let y1 = stage(u, &mut st[0]);
        let y2 = stage(y1, &mut st[1]);
        let y3 = stage(y2, &mut st[2]);
        let y = stage(y3, &mut st[3]);
        self.s = st;
        y
    }
}

/// tanh approximation (Padé 3/2), exact at 0 and within 2 % up to |x| = 3, clamped beyond.
#[inline]
fn fast_tanh(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    let x2 = x * x;
    x * (27.0 + x2) / (27.0 + 9.0 * x2)
}

/// Magnitude response (linear) of the clean ladder at `freq` Hz: the analytic transfer function
/// of the same discrete filter, `H = c·H₁⁴ / (1 + k·H₁⁴)` with the TPT one-pole
/// `H₁(z) = g(1 + z⁻¹) / ((1 + g) + (g − 1)z⁻¹)` and input gain `c = 1 + k/2`. Used to draw the
/// filter curve.
pub fn ladder_response(cutoff_hz: f32, resonance: f32, sample_rate: f32, freq: f32) -> f32 {
    let g = f64::from(ladder_g(cutoff_hz, sample_rate));
    let k = f64::from(ladder_k(resonance));
    let w = std::f64::consts::TAU * f64::from(freq) / f64::from(sample_rate);
    // z⁻¹ = e^{-jw}
    let (zr, zi) = (w.cos(), -w.sin());
    // H1 = g(1 + z⁻¹) / ((1 + g) + (g - 1) z⁻¹)
    let (nr, ni) = (g * (1.0 + zr), g * zi);
    let (dr, di) = ((1.0 + g) + (g - 1.0) * zr, (g - 1.0) * zi);
    let den = dr * dr + di * di;
    let (h1r, h1i) = ((nr * dr + ni * di) / den, (ni * dr - nr * di) / den);
    // H1⁴
    let (h2r, h2i) = (h1r * h1r - h1i * h1i, 2.0 * h1r * h1i);
    let (h4r, h4i) = (h2r * h2r - h2i * h2i, 2.0 * h2r * h2i);
    // c·H4 / (1 + k·H4)
    let (dr, di) = (1.0 + k * h4r, k * h4i);
    let mag = (h4r * h4r + h4i * h4i).sqrt() / (dr * dr + di * di).sqrt();
    (mag * (1.0 + 0.5 * k)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steady-state gain of the filter for a sine at `freq`.
    fn measured_gain(cutoff: f32, res: f32, freq: f32) -> f32 {
        let sr = 48_000.0;
        let (g, k) = (ladder_g(cutoff, sr), ladder_k(res));
        let mut f = Ladder::default();
        let n = 48_000;
        // RMS over the second half (after the transient), times √2 for the amplitude.
        let mut sum = 0.0_f64;
        for i in 0..n {
            let x = (std::f64::consts::TAU * f64::from(freq) * i as f64 / f64::from(sr)).sin();
            let y = f.process(x as f32, g, k, 1.0);
            if i >= n / 2 {
                sum += f64::from(y) * f64::from(y);
            }
        }
        ((sum / (n / 2) as f64).sqrt() * std::f64::consts::SQRT_2) as f32
    }

    #[test]
    fn response_formula_matches_the_filter() {
        for (cutoff, res, freq) in [
            (1000.0, 0.0, 100.0),
            (1000.0, 0.0, 1000.0),
            (1000.0, 0.0, 4000.0),
            (1000.0, 0.8, 1000.0),
            (5000.0, 0.5, 3000.0),
            (200.0, 0.3, 800.0),
        ] {
            let m = measured_gain(cutoff, res, freq);
            let r = ladder_response(cutoff, res, 48_000.0, freq);
            let db = 20.0 * (m / r).log10();
            assert!(
                db.abs() < 0.3,
                "{cutoff} {res} {freq}: measured {m}, formula {r}"
            );
        }
    }

    #[test]
    fn slope_is_24_db_per_octave() {
        // Two and four octaves above a 500 Hz cutoff: about 24 dB apart.
        let a = ladder_response(500.0, 0.0, 48_000.0, 2000.0);
        let b = ladder_response(500.0, 0.0, 48_000.0, 4000.0);
        let db = 20.0 * (a / b).log10();
        assert!((db - 24.0).abs() < 1.5, "{db}");
    }

    #[test]
    fn resonance_peaks_at_cutoff() {
        let flat = ladder_response(1000.0, 0.0, 48_000.0, 1000.0);
        let peak = ladder_response(1000.0, 0.9, 48_000.0, 1000.0);
        assert!(peak > 3.0 * flat, "{flat} {peak}");
    }

    #[test]
    fn stays_bounded_at_full_resonance_and_high_cutoff() {
        let sr = 48_000.0;
        let mut f = Ladder::default();
        let (g, k) = (ladder_g(20_000.0, sr), ladder_k(1.0));
        let mut x = 1.0_f32;
        for i in 0..96_000 {
            if i % 50 == 0 {
                x = -x;
            }
            let y = f.process(x, g, k, 3.0);
            assert!(y.is_finite() && y.abs() < 20.0, "{i} {y}");
        }
    }

    #[test]
    fn tanh_approximation() {
        for x in [-2.5_f32, -1.0, -0.1, 0.0, 0.3, 1.0, 2.0] {
            assert!((fast_tanh(x) - x.tanh()).abs() < 0.025, "{x}");
        }
        assert_eq!(fast_tanh(10.0), fast_tanh(3.0));
    }
}
