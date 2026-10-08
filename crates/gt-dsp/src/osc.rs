//! Sine oscillator.
//!
//! A phase accumulator: each sample the phase advances by `freq / sample_rate` cycles and wraps
//! at 1.0; the output is `sin(2π · phase)`. A pure sine has no harmonics, so unlike saw or square
//! waves it cannot alias as long as `freq` is below Nyquist (`sample_rate / 2`).
//!
//! The phase is kept in `f64`: with `f32` the rounding error of the increment (about 1e-7 of a
//! cycle per sample) shows up as audible pitch error and drift on long notes.

use core::f64::consts::TAU;

/// Sine oscillator with a wrapped `f64` phase accumulator.
#[derive(Debug, Clone)]
pub struct SineOsc {
    sample_rate: f64,
    phase: f64,
    increment: f64,
}

impl SineOsc {
    /// Creates an oscillator at `freq` Hz for the given sample rate, starting at phase 0.
    pub fn new(sample_rate: f32, freq: f32) -> Self {
        let mut osc = Self {
            sample_rate: f64::from(sample_rate.max(1.0)),
            phase: 0.0,
            increment: 0.0,
        };
        osc.set_freq(freq);
        osc
    }

    /// Sets the frequency in Hz. Values outside `0..Nyquist` are clamped.
    #[inline]
    pub fn set_freq(&mut self, freq: f32) {
        let nyquist = self.sample_rate * 0.5;
        self.increment = f64::from(freq).clamp(0.0, nyquist) / self.sample_rate;
    }

    /// Restarts the waveform at phase 0 (a zero crossing, so no click when faded in).
    #[inline]
    pub fn reset(&mut self) {
        self.phase = 0.0;
    }

    /// Returns the next sample in `-1.0..=1.0`.
    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        let out = (self.phase * TAU).sin() as f32;
        self.phase += self.increment;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measures frequency from the interpolated times of the first and last upward zero
    /// crossings over `seconds` of output.
    fn measure(sample_rate: f32, freq: f32, seconds: f32) -> f64 {
        let mut osc = SineOsc::new(sample_rate, freq);
        let n = (sample_rate * seconds) as usize;
        let mut prev = osc.next_sample();
        let (mut first, mut last, mut cycles) = (None, 0.0_f64, 0_u32);
        for i in 1..n {
            let s = osc.next_sample();
            if prev < 0.0 && s >= 0.0 {
                // Linear interpolation between samples i-1 and i.
                let t = (i - 1) as f64 + f64::from(-prev / (s - prev));
                match first {
                    None => first = Some(t),
                    Some(_) => cycles += 1,
                }
                last = t;
            }
            prev = s;
        }
        f64::from(cycles) * f64::from(sample_rate) / (last - first.unwrap())
    }

    #[test]
    fn frequency_is_correct_at_common_rates() {
        for sr in [44_100.0, 48_000.0, 96_000.0] {
            let f = measure(sr, 440.0, 10.0);
            assert!((f - 440.0).abs() < 0.01, "sr {sr}: measured {f}");
        }
    }

    #[test]
    fn amplitude_is_unity() {
        let mut osc = SineOsc::new(48_000.0, 440.0);
        let peak = (0..48_000)
            .map(|_| osc.next_sample().abs())
            .fold(0.0, f32::max);
        assert!(peak > 0.999 && peak <= 1.0, "{peak}");
    }

    #[test]
    fn starts_at_zero_crossing() {
        let mut osc = SineOsc::new(48_000.0, 440.0);
        assert_eq!(osc.next_sample(), 0.0);
    }

    #[test]
    fn frequency_is_clamped_to_nyquist() {
        let mut osc = SineOsc::new(48_000.0, 1.0e6);
        for _ in 0..1000 {
            assert!(osc.next_sample().is_finite());
        }
    }
}
