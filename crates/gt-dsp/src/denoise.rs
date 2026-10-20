//! Spectral noise reduction from a noise print (Prompt 32, D146).
//!
//! The noise print (a stretch of the recording with only the noise in it) gives the noise's
//! average magnitude in every frequency bin. The audio is then cut into overlapping Hann
//! windows (2048 samples, 75 % overlap); each bin is turned down where its magnitude is not
//! clearly above the noise's (`gain = 1 - over · noise / magnitude`, never below the floor that
//! `reduction_db` sets), the gains are smoothed over time against "musical noise" (isolated
//! bins flickering on and off), and the windows are added back together. Where the signal is
//! well above the noise the gain is 1 and it passes unchanged.

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

/// FFT size.
pub const N: usize = 2048;
/// Hop between windows (75 % overlap).
pub const HOP: usize = N / 4;
/// How far above the average noise magnitude a bin must be to pass untouched.
const OVER: f32 = 2.0;
/// Weight of the previous window's gain in the time smoothing.
const SMOOTH: f32 = 0.5;

/// The noise's average magnitude per bin.
#[derive(Debug, Clone, PartialEq)]
pub struct NoiseProfile {
    mag: Vec<f32>,
}

fn hann() -> Vec<f32> {
    (0..N)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos())
        .collect()
}

/// Calls `f` with the spectrum of every window of `x` (zero-padded at both ends), in order;
/// `f` may change the spectrum, and the changed windows are overlap-added into the result.
fn stft(x: &[f32], mut f: impl FnMut(&mut [Complex<f32>])) -> Vec<f32> {
    let w = hann();
    let mut planner = FftPlanner::new();
    let fwd = planner.plan_fft_forward(N);
    let inv = planner.plan_fft_inverse(N);
    let windows = x.len().div_ceil(HOP) + N / HOP;
    let mut out = vec![0.0_f32; (windows + 1) * HOP + N];
    let mut buf = vec![Complex::new(0.0, 0.0); N];
    for k in 0..windows {
        // Window k starts at x index k·HOP - (N - HOP).
        let start = (k * HOP) as isize - (N - HOP) as isize;
        for (i, b) in buf.iter_mut().enumerate() {
            let j = start + i as isize;
            let v = if j >= 0 {
                x.get(j as usize).copied().unwrap_or(0.0)
            } else {
                0.0
            };
            *b = Complex::new(v * w[i], 0.0);
        }
        fwd.process(&mut buf);
        f(&mut buf);
        inv.process(&mut buf);
        for (i, b) in buf.iter().enumerate() {
            out[k * HOP + i] += b.re * w[i];
        }
    }
    // Hann² at 75 % overlap sums to 1.5; the inverse FFT is unscaled.
    let scale = 1.0 / (1.5 * N as f32);
    out.drain(..N - HOP);
    out.truncate(x.len());
    for v in &mut out {
        *v *= scale;
    }
    out
}

impl NoiseProfile {
    /// Learns the noise from `print` (silence gives a profile that changes nothing).
    pub fn learn(print: &[f32]) -> Self {
        let mut mag = vec![0.0_f32; N / 2 + 1];
        let mut count = 0;
        stft(print, |s| {
            for (m, c) in mag.iter_mut().zip(s.iter()) {
                *m += c.norm();
            }
            count += 1;
        });
        for m in &mut mag {
            *m /= count.max(1) as f32;
        }
        Self { mag }
    }

    /// `x` with the noise turned down by up to `reduction_db` decibels.
    pub fn reduce(&self, x: &[f32], reduction_db: f32) -> Vec<f32> {
        let floor = 10f32.powf(-reduction_db.max(0.0) / 20.0);
        let mut prev = vec![1.0_f32; N / 2 + 1];
        stft(x, |s| {
            for k in 0..=N / 2 {
                let m = s[k].norm();
                let g = if m > 0.0 {
                    (1.0 - OVER * self.mag[k] / m).clamp(floor, 1.0)
                } else {
                    floor
                };
                let g = SMOOTH * prev[k] + (1.0 - SMOOTH) * g;
                prev[k] = g;
                s[k] *= g;
                // Keep the spectrum conjugate-symmetric so the output stays real.
                if k > 0 && k < N / 2 {
                    s[N - k] *= g;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uniform white noise from a fixed-seed generator.
    fn noise(n: usize, seed: u32, amp: f32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                amp * ((s >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0)
            })
            .collect()
    }

    fn power(x: &[f32]) -> f64 {
        x.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / x.len() as f64
    }

    #[test]
    fn a_profile_of_silence_changes_nothing() {
        let x: Vec<f32> = (0..10_000).map(|i| (i as f32 * 0.03).sin() * 0.5).collect();
        let y = NoiseProfile::learn(&vec![0.0; 8192]).reduce(&x, 30.0);
        let err = x
            .iter()
            .zip(&y)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(err < 1e-4, "{err}");
    }

    #[test]
    fn noise_reduction_raises_the_snr() {
        let sr = 48_000.0;
        let n = 96_000;
        // A melody of two tones over white noise at -14 dB relative to the tones.
        let clean: Vec<f32> = (0..n)
            .map(|i| {
                let f = if i < n / 2 { 440.0 } else { 660.0 };
                0.5 * (2.0 * std::f32::consts::PI * f * i as f32 / sr).sin()
            })
            .collect();
        let noisy: Vec<f32> = clean
            .iter()
            .zip(noise(n, 1, 0.2))
            .map(|(a, b)| a + b)
            .collect();
        // The print is another stretch of the same noise.
        let profile = NoiseProfile::learn(&noise(24_000, 7, 0.2));
        let out = profile.reduce(&noisy, 30.0);
        let snr = |y: &[f32]| {
            let resid: Vec<f32> = y.iter().zip(&clean).map(|(a, b)| a - b).collect();
            10.0 * (power(&clean) / power(&resid)).log10()
        };
        let (before, after) = (snr(&noisy), snr(&out));
        assert!(after > before + 8.0, "SNR {before:.1} dB -> {after:.1} dB");
    }
}
