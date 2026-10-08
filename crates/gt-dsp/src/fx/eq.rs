//! 8-band parametric EQ.

use super::biquad::{Biquad, BiquadCoeffs};
use super::{clamp_param, Effect, FxContext, Smooth, CONTROL_FRAMES};
use crate::db_to_gain;

/// Number of bands.
pub const EQ_BANDS: usize = 8;
/// Parameters per band: type, frequency, gain, Q.
const PER_BAND: usize = 4;
/// Index of the output gain (after the bands).
const OUTPUT: usize = EQ_BANDS * PER_BAND;

/// Shape of one band. The value is the parameter value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandType {
    /// Not processed.
    Off = 0,
    /// Boost or cut around the frequency.
    Bell = 1,
    /// Boost or cut below the frequency.
    LowShelf = 2,
    /// Boost or cut above the frequency.
    HighShelf = 3,
    /// 12 dB/oct high-pass (removes lows).
    LowCut = 4,
    /// 12 dB/oct low-pass (removes highs).
    HighCut = 5,
    /// Narrow cut at the frequency.
    Notch = 6,
}

impl BandType {
    /// From a parameter value.
    pub fn from_value(v: f32) -> Self {
        match v.round() as i32 {
            1 => Self::Bell,
            2 => Self::LowShelf,
            3 => Self::HighShelf,
            4 => Self::LowCut,
            5 => Self::HighCut,
            6 => Self::Notch,
            _ => Self::Off,
        }
    }

    /// Whether the band's gain parameter does anything.
    pub fn uses_gain(self) -> bool {
        matches!(self, Self::Bell | Self::LowShelf | Self::HighShelf)
    }

    fn coeffs(self, freq: f32, gain_db: f32, q: f32, sr: f32) -> BiquadCoeffs {
        match self {
            Self::Off => BiquadCoeffs::IDENTITY,
            Self::Bell => BiquadCoeffs::bell(freq, gain_db, q, sr),
            Self::LowShelf => BiquadCoeffs::low_shelf(freq, gain_db, q, sr),
            Self::HighShelf => BiquadCoeffs::high_shelf(freq, gain_db, q, sr),
            Self::LowCut => BiquadCoeffs::high_pass(freq, q, sr),
            Self::HighCut => BiquadCoeffs::low_pass(freq, q, sr),
            Self::Notch => BiquadCoeffs::notch(freq, q, sr),
        }
    }
}

/// Response of one band at `freq` in dB, as the EQ computes it (for drawing the curve).
pub fn eq_band_response(
    kind: BandType,
    band_freq: f32,
    gain_db: f32,
    q: f32,
    sr: f32,
    freq: f32,
) -> f32 {
    if kind == BandType::Off {
        return 0.0;
    }
    kind.coeffs(band_freq, gain_db, q, sr)
        .magnitude_db(freq, sr)
}

#[derive(Debug, Clone, Copy)]
struct Band {
    kind: BandType,
    /// Smoothed in log2(Hz), so sweeps move evenly in pitch.
    log_freq: Smooth,
    gain: Smooth,
    q: Smooth,
    l: Biquad,
    r: Biquad,
}

impl Band {
    /// A bell at 0 dB or an "Off" band does nothing; skip it.
    fn is_flat(&self) -> bool {
        match self.kind {
            BandType::Off => true,
            k if k.uses_gain() => self.gain.is_settled() && self.gain.value() == 0.0,
            _ => false,
        }
    }
}

/// Eight biquad bands in series (bell, shelves, cuts, notch) and an output gain. Coefficients
/// are recomputed every 16 frames from smoothed frequency, gain and Q.
#[derive(Debug, Clone)]
pub struct ParamEq {
    sr: f32,
    bands: [Band; EQ_BANDS],
    output: Smooth,
}

impl ParamEq {
    /// A flat EQ.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let band = Band {
            kind: BandType::Off,
            log_freq: Smooth::new(1000.0_f32.log2(), 20.0, sr),
            gain: Smooth::new(0.0, 20.0, sr),
            q: Smooth::new(0.707, 20.0, sr),
            l: Biquad::default(),
            r: Biquad::default(),
        };
        Self {
            sr,
            bands: [band; EQ_BANDS],
            output: Smooth::new(1.0, 20.0, sr),
        }
    }

    fn update_coeffs(&mut self, frames: usize) {
        let sr = self.sr;
        for b in &mut self.bands {
            if b.kind == BandType::Off {
                continue;
            }
            let f = b.log_freq.advance(frames).exp2();
            let g = b.gain.advance(frames);
            let q = b.q.advance(frames);
            let c = b.kind.coeffs(f, g, q, sr);
            b.l.c = c;
            b.r.c = c;
        }
    }
}

impl Effect for ParamEq {
    fn param_count(&self) -> usize {
        OUTPUT + 1
    }

    fn set_param(&mut self, index: usize, value: f32) {
        if index == OUTPUT {
            self.output
                .set(db_to_gain(clamp_param(value, -24.0, 24.0, 0.0)));
            return;
        }
        let Some(b) = self.bands.get_mut(index / PER_BAND) else {
            return;
        };
        match index % PER_BAND {
            0 => {
                let kind = BandType::from_value(clamp_param(value, 0.0, 6.0, 0.0));
                if kind != b.kind {
                    b.kind = kind;
                    // A new shape starts from clean state rather than the old filter's memory.
                    b.l.reset();
                    b.r.reset();
                    b.log_freq.snap();
                    b.gain.snap();
                    b.q.snap();
                }
            }
            1 => b
                .log_freq
                .set(clamp_param(value, 20.0, 20_000.0, 1000.0).log2()),
            2 => b.gain.set(clamp_param(value, -24.0, 24.0, 0.0)),
            _ => b.q.set(clamp_param(value, 0.1, 18.0, 0.707)),
        }
    }

    fn reset(&mut self) {
        for b in &mut self.bands {
            b.l.reset();
            b.r.reset();
            b.log_freq.snap();
            b.gain.snap();
            b.q.snap();
        }
        self.output.snap();
        self.update_coeffs(0);
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &FxContext) {
        for (cl, cr) in l
            .chunks_mut(CONTROL_FRAMES)
            .zip(r.chunks_mut(CONTROL_FRAMES))
        {
            self.update_coeffs(cl.len());
            for b in &mut self.bands {
                if b.is_flat() {
                    continue;
                }
                for (x, y) in cl.iter_mut().zip(cr.iter_mut()) {
                    *x = b.l.process(*x);
                    *y = b.r.process(*y);
                }
            }
            if !(self.output.is_settled() && self.output.value() == 1.0) {
                for (x, y) in cl.iter_mut().zip(cr.iter_mut()) {
                    let g = self.output.next_value();
                    *x *= g;
                    *y *= g;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{db, rms, run, sine};

    #[test]
    fn flat_eq_is_transparent() {
        let mut eq = ParamEq::new(48_000.0);
        for b in 0..EQ_BANDS {
            eq.set_param(b * PER_BAND, BandType::Bell as i32 as f32);
        }
        eq.reset();
        let x = sine(440.0, 0.5, 48_000.0, 4096);
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut eq, &mut l, &mut r, 120.0);
        assert_eq!(l, x);
    }

    #[test]
    fn bands_add_up_to_the_drawn_curve() {
        let sr = 48_000.0;
        let mut eq = ParamEq::new(sr);
        let bands = [
            (BandType::LowCut, 60.0, 0.0, 0.707),
            (BandType::Bell, 400.0, -6.0, 1.2),
            (BandType::HighShelf, 6000.0, 4.0, 0.707),
        ];
        for (i, &(k, f, g, q)) in bands.iter().enumerate() {
            eq.set_param(i * PER_BAND, k as i32 as f32);
            eq.set_param(i * PER_BAND + 1, f);
            eq.set_param(i * PER_BAND + 2, g);
            eq.set_param(i * PER_BAND + 3, q);
        }
        eq.set_param(OUTPUT, -3.0);
        eq.reset();
        for f in [40.0, 400.0, 1500.0, 10_000.0] {
            let x = sine(f, 0.25, sr, 48_000);
            let (mut l, mut r) = (x.clone(), x.clone());
            run(&mut eq, &mut l, &mut r, 120.0);
            let measured = db(rms(&l[24_000..]) / rms(&x[24_000..]));
            let drawn: f32 = bands
                .iter()
                .map(|&(k, bf, g, q)| eq_band_response(k, bf, g, q, sr, f))
                .sum::<f32>()
                - 3.0;
            assert!(
                (measured - drawn).abs() < 0.3,
                "{f} Hz: {measured} vs {drawn}"
            );
            eq.reset();
        }
    }

    #[test]
    fn gain_changes_glide() {
        let sr = 48_000.0;
        let mut eq = ParamEq::new(sr);
        eq.set_param(OUTPUT, -24.0);
        let mut l = vec![1.0; 64];
        let mut r = vec![1.0; 64];
        run(&mut eq, &mut l, &mut r, 120.0);
        // After 64 samples of a 20 ms glide the gain has barely moved.
        assert!(l[63] > 0.9, "{}", l[63]);
    }
}
