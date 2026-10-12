//! Round-trip latency measurement for the loopback calibration (Prompt 13).
//!
//! The engine plays [`BURSTS`] short tone bursts, one every [`period`] frames, and captures
//! the input over the same frames. With the output wired (or acoustically coupled) to the
//! input, each burst comes back after the round trip: output buffer, converters, cable or
//! air, input buffer. [`measure_delay`] finds that delay by cross-correlating the capture with
//! the burst, one search per burst, and takes the median. Correlation instead of a threshold
//! makes it robust to the interface filtering the burst, to a polarity flip and to noise.
//!
//! The burst is a Hann-windowed 2 kHz sine, 4 ms long: short enough to give one clear
//! correlation peak, and band-limited, so converters and cheap cables pass it whole.

use std::f32::consts::TAU;

/// Number of bursts played.
pub const BURSTS: usize = 8;
/// Frequency of the burst's sine.
const BURST_HZ: f32 = 2000.0;
/// Burst length in seconds.
const BURST_S: f32 = 0.004;
/// Bursts start this far apart, in seconds. Round trips longer than this cannot be measured.
pub const PERIOD_S: f32 = 0.35;
/// Peak level of the burst (about -6 dBFS).
pub const BURST_GAIN: f32 = 0.5;

/// Burst length in frames at `sample_rate`.
pub fn burst_len(sample_rate: f32) -> usize {
    (BURST_S * sample_rate).round().max(8.0) as usize
}

/// Frames between burst starts at `sample_rate`.
pub fn period(sample_rate: f32) -> usize {
    (PERIOD_S * sample_rate).round() as usize
}

/// Frames the whole measurement takes (all bursts plus one period for the last echo).
pub fn total_frames(sample_rate: f32) -> usize {
    period(sample_rate) * (BURSTS + 1)
}

/// Sample `i` of the burst (0 outside it). Real-time safe: computed, not looked up.
pub fn burst_sample(i: usize, sample_rate: f32) -> f32 {
    let len = burst_len(sample_rate);
    if i >= len {
        return 0.0;
    }
    let x = i as f32 / (len - 1) as f32;
    let window = 0.5 - 0.5 * (TAU * x).cos();
    BURST_GAIN * window * (TAU * BURST_HZ * i as f32 / sample_rate).sin()
}

/// The output signal of the measurement at frame `i` (from its start).
pub fn signal_at(i: usize, sample_rate: f32) -> f32 {
    let p = period(sample_rate);
    if i >= p * BURSTS {
        return 0.0;
    }
    burst_sample(i % p, sample_rate)
}

/// Why a measurement failed.
#[derive(Debug, Clone, PartialEq)]
pub enum CalibrationError {
    /// The capture is shorter than the measurement.
    TooShort,
    /// Nothing that looks like the bursts came back (no cable, input muted, wrong input).
    NoSignal,
    /// The bursts came back at different delays (noise, or an unstable stream).
    Inconsistent,
}

impl std::fmt::Display for CalibrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooShort => "the capture ended early",
            Self::NoSignal => {
                "no click came back: connect an output to the chosen input and turn it up"
            }
            Self::Inconsistent => "the clicks came back at different times; try again",
        })
    }
}

/// The round-trip delay in frames from the output frame where a burst starts to the captured
/// frame where it comes back. `captured[i]` must be the input read alongside output frame `i`
/// of [`signal_at`]. At least half the bursts must agree within two frames.
pub fn measure_delay(captured: &[f32], sample_rate: f32) -> Result<u32, CalibrationError> {
    let p = period(sample_rate);
    let len = burst_len(sample_rate);
    if captured.len() < total_frames(sample_rate) {
        return Err(CalibrationError::TooShort);
    }
    let burst: Vec<f32> = (0..len).map(|i| burst_sample(i, sample_rate)).collect();
    let energy: f32 = burst.iter().map(|b| b * b).sum();
    let mut delays = Vec::with_capacity(BURSTS);
    for k in 0..BURSTS {
        let base = k * p;
        let mut best = (0usize, 0.0_f32);
        let mut window_energy = 0.0_f32;
        for d in 0..p {
            let seg = &captured[base + d..base + d + len];
            let c: f32 = seg.iter().zip(&burst).map(|(x, b)| x * b).sum();
            if c.abs() > best.1 {
                best = (d, c.abs());
            }
            window_energy += seg[0] * seg[0];
        }
        // Normalised correlation at the peak: 1.0 is a clean, undistorted echo. Below a tenth
        // of the burst's own energy (relative to the input's level) it is noise.
        let seg = &captured[base + best.0..base + best.0 + len];
        let seg_energy: f32 = seg.iter().map(|x| x * x).sum();
        let norm = best.1 / (energy * seg_energy).sqrt().max(1e-12);
        let loud = seg_energy > 1e-3 * energy && seg_energy * p as f32 > window_energy * len as f32;
        if norm > 0.5 && loud {
            delays.push(best.0 as u32);
        }
    }
    if delays.len() < BURSTS / 2 {
        return Err(CalibrationError::NoSignal);
    }
    delays.sort_unstable();
    let median = delays[delays.len() / 2];
    let agree = delays.iter().filter(|&&d| d.abs_diff(median) <= 2).count();
    if agree < BURSTS / 2 {
        return Err(CalibrationError::Inconsistent);
    }
    Ok(median)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What an input sees through a loop with `delay` frames, `gain` and some noise.
    fn loopback(sr: f32, delay: usize, gain: f32, noise: f32) -> Vec<f32> {
        let n = total_frames(sr);
        let mut seed = 0x1234_5678_u32;
        (0..n)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let r = (seed as f32 / u32::MAX as f32 - 0.5) * 2.0 * noise;
                let s = i.checked_sub(delay).map_or(0.0, |j| signal_at(j, sr));
                s * gain + r
            })
            .collect()
    }

    #[test]
    fn finds_the_delay_exactly() {
        for (sr, delay) in [
            (44_100.0, 0),
            (48_000.0, 517),
            (96_000.0, 4096),
            (48_000.0, 9001),
        ] {
            let cap = loopback(sr, delay, 0.3, 0.0);
            assert_eq!(
                measure_delay(&cap, sr),
                Ok(delay as u32),
                "{sr} Hz, {delay}"
            );
        }
    }

    #[test]
    fn survives_noise_inversion_and_a_quiet_return() {
        let sr = 48_000.0;
        let cap: Vec<f32> = loopback(sr, 1234, -0.05, 0.01);
        assert_eq!(measure_delay(&cap, sr), Ok(1234));
    }

    #[test]
    fn silence_and_noise_are_refused() {
        let sr = 48_000.0;
        let silent = vec![0.0; total_frames(sr)];
        assert_eq!(measure_delay(&silent, sr), Err(CalibrationError::NoSignal));
        let noise = loopback(sr, 100, 0.0, 0.2);
        assert!(measure_delay(&noise, sr).is_err());
        assert_eq!(
            measure_delay(&silent[..100], sr),
            Err(CalibrationError::TooShort)
        );
    }

    #[test]
    fn burst_is_short_and_bounded() {
        let sr = 48_000.0;
        assert_eq!(burst_len(sr), 192);
        assert_eq!(burst_sample(0, sr), 0.0);
        assert_eq!(burst_sample(192, sr), 0.0);
        let peak = (0..192)
            .map(|i| burst_sample(i, sr).abs())
            .fold(0.0, f32::max);
        assert!(peak > 0.4 && peak <= BURST_GAIN);
        assert_eq!(signal_at(period(sr) * BURSTS + 5, sr), 0.0);
    }
}
