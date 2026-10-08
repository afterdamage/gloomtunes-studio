//! Band-limited oscillator waveforms with PolyBLEP and PolyBLAMP corrections.
//!
//! A naive saw or square jumps between -1 and 1 in a single sample. That step contains
//! harmonics far above Nyquist, and sampling folds them back down as inharmonic aliases, heard as
//! a gritty, metallic tone that gets worse with pitch. PolyBLEP ("polynomial band-limited step")
//! subtracts a short two-sample polynomial approximation of the difference between an ideal
//! band-limited step and a hard one at every jump. That removes most of the aliasing for the
//! cost of a few multiplications per discontinuity.
//!
//! A triangle has no jumps, only corners (jumps in slope). Its aliasing is much weaker (harmonics
//! fall at 12 dB per octave instead of 6) and is corrected the same way with PolyBLAMP, the
//! integrated version of the BLEP residual.

/// Oscillator waveform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wave {
    /// Pure sine (no correction needed).
    Sine,
    /// Triangle (PolyBLAMP corrected).
    Triangle,
    /// Rising saw (PolyBLEP corrected).
    #[default]
    Saw,
    /// Pulse with variable width (PolyBLEP corrected).
    Square,
}

/// PolyBLEP residual for an upward step of 2 (from -1 to 1) at phase 0. `t` is the phase in cycles (0..1) and
/// `dt` the phase increment per sample.
#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

/// PolyBLAMP residual for a corner at phase 0 (integral of the BLEP residual, in units of
/// `dt`).
#[inline]
fn poly_blamp(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt - 1.0;
        -x * x * x / 3.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt + 1.0;
        x * x * x / 3.0
    } else {
        0.0
    }
}

#[inline]
fn wrap(t: f32) -> f32 {
    if t >= 1.0 {
        t - 1.0
    } else if t < 0.0 {
        t + 1.0
    } else {
        t
    }
}

/// One sample of `wave` at phase `t` (cycles, 0..1) with phase increment `dt` (cycles per
/// sample, below 0.5). `pw` is the pulse width of the square (fraction of the cycle spent high).
/// Output is in -1..1.
#[inline]
pub fn blep_sample(wave: Wave, t: f32, dt: f32, pw: f32) -> f32 {
    let dt = dt.clamp(1e-7, 0.5);
    match wave {
        Wave::Sine => (t * core::f32::consts::TAU).sin(),
        Wave::Saw => 2.0 * t - 1.0 - poly_blep(t, dt),
        Wave::Square => {
            let pw = pw.clamp(0.02, 0.98);
            let naive = if t < pw { 1.0 } else { -1.0 };
            // Up-step at t = 0, down-step at t = pw. Both jumps are 2 high, which is the size
            // the residual above is scaled for (it runs from -1 to 1).
            naive + poly_blep(t, dt) - poly_blep(wrap(t - pw + 1.0), dt)
        }
        Wave::Triangle => {
            // Peak at t = 0, trough at t = 0.5. The slope changes by -8 per cycle at t = 0 and
            // by +8 at t = 0.5, i.e. by 8·dt per sample; the residual is scaled for a change of
            // 2 per sample.
            let naive = 4.0 * (t - 0.5).abs() - 1.0;
            naive - 4.0 * dt * (poly_blamp(t, dt) - poly_blamp(wrap(t + 0.5), dt))
        }
    }
}

/// A phase accumulator in cycles.
#[derive(Debug, Clone, Copy, Default)]
pub struct Phase {
    /// Current phase, 0..1.
    pub t: f32,
}

impl Phase {
    /// Returns the current phase and advances by `dt` cycles (wrapping at 1).
    #[inline]
    pub fn advance(&mut self, dt: f32) -> f32 {
        let t = self.t;
        let mut n = t + dt;
        if n >= 1.0 {
            n -= 1.0;
            if n >= 1.0 {
                n = n.fract();
            }
        }
        self.t = n;
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render `n` samples of a wave at `freq` / `sr`.
    fn render(wave: Wave, freq: f64, sr: f64, n: usize, naive: bool) -> Vec<f32> {
        let dt = (freq / sr) as f32;
        let mut ph = Phase::default();
        (0..n)
            .map(|_| {
                let t = ph.advance(dt);
                if naive {
                    match wave {
                        Wave::Saw => 2.0 * t - 1.0,
                        Wave::Square => {
                            if t < 0.5 {
                                1.0
                            } else {
                                -1.0
                            }
                        }
                        Wave::Triangle => 4.0 * (t - 0.5).abs() - 1.0,
                        Wave::Sine => (t * core::f32::consts::TAU).sin(),
                    }
                } else {
                    blep_sample(wave, t, dt, 0.5)
                }
            })
            .collect()
    }

    /// Power (dB, relative to the fundamental) of the aliases: harmonics above Nyquist folded
    /// back into the audio band. Measured with a Hann-windowed single-bin DFT at each folded
    /// frequency.
    fn alias_db(x: &[f32], freq: f64, sr: f64) -> f64 {
        use std::f64::consts::TAU;
        let n = x.len();
        let bin = |f: f64| {
            let (mut re, mut im) = (0.0, 0.0);
            for (i, &v) in x.iter().enumerate() {
                let w = 0.5 - 0.5 * (TAU * i as f64 / n as f64).cos();
                let ph = TAU * f * i as f64 / sr;
                re += f64::from(v) * w * ph.cos();
                im -= f64::from(v) * w * ph.sin();
            }
            re * re + im * im
        };
        let first_alias = (sr / 2.0 / freq) as usize + 1;
        let mut alias = 0.0;
        for h in first_alias..first_alias + 20 {
            let mut a = (h as f64 * freq) % sr;
            if a > sr / 2.0 {
                a = sr - a;
            }
            // Skip aliases that land on a real harmonic.
            if ((a / freq) - (a / freq).round()).abs() * freq > 40.0 {
                alias += bin(a);
            }
        }
        10.0 * (alias / bin(freq)).log10()
    }

    #[test]
    fn corrections_cut_aliasing() {
        // A high note, where aliasing is worst: about 2.5 kHz at 48 kHz.
        let (f, sr, n) = (2_489.0, 48_000.0, 8192);
        for (wave, min_gain_db) in [
            (Wave::Saw, 10.0),
            (Wave::Square, 10.0),
            (Wave::Triangle, 8.0),
        ] {
            let naive = alias_db(&render(wave, f, sr, n, true), f, sr);
            let blep = alias_db(&render(wave, f, sr, n, false), f, sr);
            assert!(
                blep < naive - min_gain_db,
                "{wave:?}: corrected {blep:.1} dB, naive {naive:.1} dB"
            );
        }
    }

    #[test]
    fn waves_stay_in_range_and_have_no_dc() {
        for wave in [Wave::Sine, Wave::Saw, Wave::Square, Wave::Triangle] {
            let v = render(wave, 110.0, 48_000.0, 48_000, false);
            assert!(v.iter().all(|x| x.abs() <= 1.05), "{wave:?}");
            let mean = v.iter().map(|&x| f64::from(x)).sum::<f64>() / v.len() as f64;
            assert!(mean.abs() < 0.01, "{wave:?} dc {mean}");
        }
    }

    #[test]
    fn phase_wraps() {
        let mut p = Phase { t: 0.9 };
        assert_eq!(p.advance(0.2), 0.9);
        assert!((p.t - 0.1).abs() < 1e-6);
    }
}
