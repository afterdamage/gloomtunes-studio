//! Mixer effects.
//!
//! Every effect implements [`Effect`]: parameters are plain values (Hz, dB, ms...) set by index,
//! in the order of the matching table in `gt_core::effects`, and processing works in place on
//! planar stereo blocks of any length. Continuous parameters glide (one-pole, about 20 ms) so a
//! knob drag never clicks; choice parameters switch at once. Effects allocate only in `new`.

mod analyzer;
mod biquad;
mod bitcrusher;
mod chorus;
mod clipper;
mod compressor;
mod convolution;
mod delay;
mod distortion;
mod eq;
mod filter;
mod flanger;
mod gate;
mod limiter;
mod multiband;
mod oversample;
mod phaser;
mod pitch;
mod reverb;
mod smooth;
mod stutter;
mod transient;
mod vocoder;
mod width;

pub use analyzer::Analyzer;
pub use biquad::{Biquad, BiquadCoeffs};
pub use bitcrusher::Bitcrusher;
pub use chorus::Chorus;
pub use clipper::SoftClipper;
pub use compressor::Compressor;
pub use convolution::{generate_ir, ConvolutionReverb, Convolver, SplitConvolver};
pub use delay::{Delay, DELAY_DIVISIONS};
pub use distortion::Distortion;
pub use eq::{eq_band_response, BandType, ParamEq, EQ_BANDS};
pub use filter::{FilterMode, MultiFilter};
pub use flanger::Flanger;
pub use gate::Gate;
pub use limiter::Limiter;
pub use multiband::Multiband;
pub use oversample::{Downsampler2x, Upsampler2x};
pub use phaser::Phaser;
pub use pitch::{PitchShifter, SCALE_INTERVALS};
pub use reverb::Reverb;
pub use smooth::Smooth;
pub use stutter::{Stutter, STEPS};
pub use transient::TransientShaper;
pub use vocoder::Vocoder;
pub use width::StereoWidth;

/// Note values for tempo-synced modulation, in beats (matches `gt_core::effects::LFO_SYNC`).
pub const LFO_SYNC_BEATS: [f32; 9] = [
    0.125,     // 1/32
    0.25,      // 1/16
    0.5,       // 1/8
    2.0 / 3.0, // 1/4 triplet
    1.0,       // 1/4
    2.0,       // 1/2
    4.0,       // 1 bar
    8.0,       // 2 bars
    16.0,      // 4 bars
];

/// Rate in Hz of LFO note value `div` (an index into [`LFO_SYNC_BEATS`]) at `bpm`.
pub(crate) fn sync_hz(div: usize, bpm: f32) -> f32 {
    let beats = LFO_SYNC_BEATS[div.min(LFO_SYNC_BEATS.len() - 1)];
    bpm.clamp(10.0, 999.0) / 60.0 / beats
}

/// What an effect may need from the host besides its audio.
#[derive(Debug, Clone, Copy, Default)]
pub struct FxContext<'a> {
    /// Current tempo in BPM, for tempo-synced effects.
    pub bpm: f32,
    /// Key signal for effects with a sidechain input (the compressor), same length as the block.
    pub sidechain: Option<(&'a [f32], &'a [f32])>,
    /// Song position at the start of the block in quarter notes, while the transport plays.
    pub beats: Option<f64>,
}

/// A stereo insert effect. Real-time safe after construction.
pub trait Effect: Send {
    /// Number of parameters (the length of its `gt_core::effects` table).
    fn param_count(&self) -> usize;
    /// Sets parameter `index` to a plain value. Out-of-range values are clamped; an unknown index
    /// is ignored.
    fn set_param(&mut self, index: usize, value: f32);
    /// Clears all internal state (delay lines, envelopes) and jumps smoothed values to target.
    fn reset(&mut self);
    /// Delay the effect adds to the signal, in frames (for delay compensation).
    fn latency(&self) -> usize {
        0
    }
    /// Processes a block in place. `l` and `r` have the same length.
    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &FxContext);
    /// A value for the UI, such as gain reduction in dB (0 when the effect has none).
    fn meter(&self) -> f32 {
        0.0
    }
    /// Builds what the effect cannot build on the audio thread, such as a convolution reverb's
    /// impulse response from `user_ir` (left and right; right may be empty). Called on the UI
    /// thread after the parameters are set.
    fn prepare(&mut self, _user_ir: Option<(&[f32], &[f32])>) {}
    /// The tap a UI reads the signal from, for effects that show it (the analyzer).
    fn scope(&self) -> Option<std::sync::Arc<crate::analysis::Scope>> {
        None
    }
}

/// Frames between coefficient updates for effects that recompute filters from smoothed values.
pub(crate) const CONTROL_FRAMES: usize = 16;

/// Clamps a parameter value to `lo..=hi`, replacing non-finite input with `fallback`.
pub(crate) fn clamp_param(v: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v.clamp(lo, hi)
    } else {
        fallback
    }
}

/// One-pole low-pass coefficient for a cutoff frequency: `y += c·(x − y)`.
pub(crate) fn one_pole_coef(cutoff_hz: f32, sample_rate: f32) -> f32 {
    1.0 - (-std::f32::consts::TAU * cutoff_hz / sample_rate).exp()
}

/// A fixed-capacity mono delay line with fractional (linear) reads.
#[derive(Debug, Clone)]
pub(crate) struct DelayLine {
    buf: Vec<f32>,
    write: usize,
}

impl DelayLine {
    /// A line that can delay by up to `capacity - 2` frames.
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            buf: vec![0.0; capacity.max(4)],
            write: 0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
    }

    /// Longest delay a read may ask for.
    pub(crate) fn max_delay(&self) -> f32 {
        (self.buf.len() - 2) as f32
    }

    /// Stores the next input sample.
    #[inline]
    pub(crate) fn push(&mut self, x: f32) {
        self.buf[self.write] = x;
        self.write += 1;
        if self.write == self.buf.len() {
            self.write = 0;
        }
    }

    /// The sample written `delay` frames before the most recent one (`delay` ≥ 0, fractional),
    /// linearly interpolated. Call after `push` for a delay of at least one sample.
    #[inline]
    pub(crate) fn read(&self, delay: f32) -> f32 {
        let n = self.buf.len();
        let d = delay.clamp(0.0, self.max_delay());
        let whole = d as usize;
        let frac = d - whole as f32;
        // Index of the most recent sample is write - 1.
        let i0 = (self.write + 2 * n - 1 - whole) % n;
        let i1 = if i0 == 0 { n - 1 } else { i0 - 1 };
        let a = self.buf[i0];
        a + (self.buf[i1] - a) * frac
    }

    /// The sample written exactly `delay` whole frames before the most recent one.
    #[inline]
    pub(crate) fn read_int(&self, delay: usize) -> f32 {
        let n = self.buf.len();
        let d = delay.min(n - 1);
        self.buf[(self.write + 2 * n - 1 - d) % n]
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    /// A sine block at `freq`, amplitude `amp`.
    pub fn sine(freq: f32, amp: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (std::f32::consts::TAU * freq * i as f32 / sr).sin())
            .collect()
    }

    /// RMS of a slice.
    pub fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// dB of an amplitude ratio.
    pub fn db(x: f32) -> f32 {
        20.0 * x.max(1e-12).log10()
    }

    /// Level of `freq` in `x` (a single DFT bin, Goertzel), as an amplitude.
    pub fn tone(x: &[f32], freq: f32, sr: f32) -> f32 {
        let w = std::f64::consts::TAU * f64::from(freq) / f64::from(sr);
        let (mut re, mut im) = (0.0_f64, 0.0_f64);
        for (i, &v) in x.iter().enumerate() {
            // A Hann window keeps leakage from strong neighbours out of the bin.
            let win = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / x.len() as f64).cos();
            re += f64::from(v) * win * (w * i as f64).cos();
            im -= f64::from(v) * win * (w * i as f64).sin();
        }
        ((re * re + im * im).sqrt() * 4.0 / x.len() as f64) as f32
    }

    /// Runs `fx` over stereo buffers in 64-frame blocks.
    pub fn run(fx: &mut dyn super::Effect, l: &mut [f32], r: &mut [f32], bpm: f32) {
        let ctx = super::FxContext {
            bpm,
            ..Default::default()
        };
        for (cl, cr) in l.chunks_mut(64).zip(r.chunks_mut(64)) {
            fx.process(cl, cr, &ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_line_reads_back_what_was_written() {
        let mut d = DelayLine::new(16);
        for i in 0..10 {
            d.push(i as f32);
        }
        assert_eq!(d.read(0.0), 9.0);
        assert_eq!(d.read(3.0), 6.0);
        assert_eq!(d.read(2.5), 6.5);
        assert_eq!(d.read_int(4), 5.0);
    }

    /// Every effect: silence stays silent-or-decays, extreme settings stay finite and bounded.
    #[test]
    fn every_effect_survives_extreme_settings() {
        let sr = 48_000.0;
        let effects: Vec<Box<dyn Effect>> = vec![
            Box::new(ParamEq::new(sr)),
            Box::new(Compressor::new(sr)),
            Box::new(Delay::new(sr)),
            Box::new(Reverb::new(sr)),
            Box::new(Chorus::new(sr)),
            Box::new(Distortion::new(sr)),
            Box::new(Limiter::new(sr)),
            Box::new(StereoWidth::new(sr)),
            Box::new(MultiFilter::new(sr)),
            Box::new(Gate::new(sr)),
            Box::new(Flanger::new(sr)),
            Box::new(Phaser::new(sr)),
            Box::new(Bitcrusher::new(sr)),
            Box::new(SoftClipper::new(sr)),
            Box::new(TransientShaper::new(sr)),
            Box::new(Multiband::new(sr)),
            Box::new(Analyzer::new(sr)),
            Box::new(ConvolutionReverb::new(sr)),
            Box::new(PitchShifter::new(sr)),
            Box::new(Vocoder::new(sr)),
            Box::new(Stutter::new(sr)),
        ];
        for mut fx in effects {
            for extreme in [f32::NEG_INFINITY, -1e9, 1e9, f32::NAN] {
                for i in 0..fx.param_count() {
                    fx.set_param(i, extreme);
                }
                fx.set_param(fx.param_count() + 3, 1.0); // ignored
                let mut l = test_util::sine(997.0, 1.0, sr, 24_000);
                let mut r = test_util::sine(301.0, 1.0, sr, 24_000);
                test_util::run(fx.as_mut(), &mut l, &mut r, 999.0);
                assert!(
                    l.iter().chain(&r).all(|x| x.is_finite() && x.abs() < 1e3),
                    "{} params at {extreme}",
                    fx.param_count()
                );
                fx.reset();
            }
        }
    }
}
