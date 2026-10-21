//! The additive/spectral engine (Prompt 34, D153): up to [`PARTIALS`] sine partials per voice.
//!
//! Each partial's level comes from the harmonic editor (32 levels) or, by the morph amount,
//! from a resynthesized sample ([`Resynth`]: harmonic levels over time, analysed from a
//! recording), then a tilt in dB per octave, the even harmonics' level, and a spectral
//! filter that weighs each partial by its frequency (low/high/band-pass, notch, comb or vowel
//! formants). Stretch moves partial `k` to `k^(1 + 0.1 × stretch)` times the fundamental, for
//! bell and piano-like inharmonicity.
//!
//! Levels and frequencies are recomputed at control rate; each partial is a rotating phasor
//! (two multiplies and adds per sample, renormalized every control block) whose level ramps
//! across the block, so sweeps do not zipper. Partials at or above 0.45 × the sample rate are
//! left out.

use std::f32::consts::TAU;

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

/// Partials per voice.
pub const PARTIALS: usize = 64;
/// Editable harmonics (the rest come from resynthesis only).
pub const HARMONICS: usize = 32;
/// Most frames a resynthesis keeps.
pub const RESYNTH_FRAMES: usize = 64;

/// How the spectral filter weighs partials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpecFilter {
    /// No filter.
    #[default]
    Off,
    /// Partials above the frequency fade (steeper with a smaller width).
    LowPass,
    /// Partials below the frequency fade.
    HighPass,
    /// A band of `width` octaves around the frequency.
    BandPass,
    /// Everything but that band.
    Notch,
    /// Peaks at every multiple of the frequency.
    Comb,
    /// Three vowel formants (A, E, I, O, U along the vowel setting).
    Formant,
}

impl SpecFilter {
    /// The mode for a parameter value (0 to 6).
    pub fn from_index(i: u8) -> Self {
        [
            Self::Off,
            Self::LowPass,
            Self::HighPass,
            Self::BandPass,
            Self::Notch,
            Self::Comb,
            Self::Formant,
        ]
        .get(usize::from(i))
        .copied()
        .unwrap_or_default()
    }
}

/// The engine's settings, in plain units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdditiveSettings {
    /// Harmonic levels, 0 to 1.
    pub harmonics: [f32; HARMONICS],
    /// Brightness tilt, dB per octave.
    pub tilt_db: f32,
    /// Level of the even harmonics, 0 to 2.
    pub even: f32,
    /// Inharmonicity, -1 to 1.
    pub stretch: f32,
    /// 0: the harmonic editor, 1: the resynthesized sample.
    pub morph: f32,
    /// Where in the resynthesized sample a note starts, 0 to 1.
    pub position: f32,
    /// How fast a note moves through the resynthesized frames: 1 is the sample's own speed,
    /// 0 holds the position.
    pub scan: f32,
    /// Spectral filter.
    pub filter: SpecFilter,
    /// Its frequency, Hz.
    pub filter_hz: f32,
    /// Its width (or softness), octaves.
    pub width_oct: f32,
    /// Its resonance, 0 to 1.
    pub resonance: f32,
    /// Formant vowel, 0 (A) to 1 (U).
    pub vowel: f32,
}

impl Default for AdditiveSettings {
    fn default() -> Self {
        Self {
            // A saw, rounded as the document's defaults are.
            harmonics: std::array::from_fn(|k| (1e4 / (k + 1) as f32).round() / 1e4),
            tilt_db: 0.0,
            even: 1.0,
            stretch: 0.0,
            morph: 1.0,
            position: 0.0,
            scan: 1.0,
            filter: SpecFilter::Off,
            filter_hz: 2000.0,
            width_oct: 1.0,
            resonance: 0.0,
            vowel: 0.0,
        }
    }
}

/// Matrix modulation of the engine for one control block.
#[derive(Debug, Clone, Copy, Default)]
pub struct AdditiveMods {
    /// Filter frequency, octaves.
    pub cutoff_oct: f32,
    /// Tilt, dB per octave.
    pub tilt_db: f32,
    /// Resynthesis position, -1 to 1.
    pub position: f32,
    /// Morph, -1 to 1.
    pub morph: f32,
}

/// Vowel formants (F1, F2, F3) in Hz, A E I O U.
const VOWELS: [[f32; 3]; 5] = [
    [800.0, 1150.0, 2900.0],
    [400.0, 1600.0, 2700.0],
    [350.0, 2300.0, 3000.0],
    [450.0, 800.0, 2830.0],
    [325.0, 700.0, 2530.0],
];
/// Formant bandwidths at width 1, Hz.
const FORMANT_BW: [f32; 3] = [90.0, 120.0, 160.0];

/// The spectral filter's gain at `f` Hz.
pub fn filter_gain(s: &AdditiveSettings, f: f32, cutoff_oct: f32) -> f32 {
    let shift = cutoff_oct.clamp(-10.0, 10.0).exp2();
    let fc = (s.filter_hz * shift).clamp(10.0, 40_000.0);
    let w = s.width_oct.clamp(0.05, 8.0);
    let x = (f.max(1.0) / fc).log2();
    let peak = 3.0 * s.resonance.clamp(0.0, 1.0) * (-(x / 0.15) * (x / 0.15)).exp();
    match s.filter {
        SpecFilter::Off => 1.0,
        SpecFilter::LowPass => 1.0 / (1.0 + (x * 8.0 / w).exp2()) + peak,
        SpecFilter::HighPass => 1.0 / (1.0 + (-x * 8.0 / w).exp2()) + peak,
        SpecFilter::BandPass => (-2.77 * (x / w) * (x / w)).exp() * (1.0 + peak),
        SpecFilter::Notch => 1.0 - (-2.77 * (x / w) * (x / w)).exp(),
        SpecFilter::Comb => (0.5 + 0.5 * (TAU * f / fc).cos()).powf(2.0 / w),
        SpecFilter::Formant => {
            let v = s.vowel.clamp(0.0, 1.0) * 4.0;
            let i = (v as usize).min(3);
            let t = v - i as f32;
            let mut g = 0.02_f32;
            for (j, bw) in FORMANT_BW.iter().enumerate() {
                let fj = (VOWELS[i][j] + (VOWELS[i + 1][j] - VOWELS[i][j]) * t) * shift;
                let d = (f - fj) / (bw * w);
                g += [1.0, 0.7, 0.4][j] * (-d * d).exp();
            }
            g.min(1.5)
        }
    }
}

/// A sample analysed into harmonic levels over time.
#[derive(Debug, Clone, PartialEq)]
pub struct Resynth {
    /// Name of the source, for display.
    pub name: String,
    /// Fundamental found in the sample, Hz.
    pub f0: f32,
    /// Levels of partials 1 to [`PARTIALS`], frame by frame (the loudest level is 1).
    pub frames: Vec<[f32; PARTIALS]>,
    /// Seconds between frames.
    pub frame_s: f32,
}

impl Resynth {
    /// Analyses mono audio: finds its pitch, then measures each harmonic's level in up to
    /// [`RESYNTH_FRAMES`] frames along it. `None` when it has no clear pitch.
    pub fn analyze(name: &str, x: &[f32], sr: f32) -> Option<Self> {
        if x.len() < 2048 || sr <= 0.0 {
            return None;
        }
        let f0 = pitch_of(x, sr)?;
        let n = 8192_usize;
        let win = n.min(x.len().next_power_of_two() / 2).max(1024);
        let count = (x.len() / (win / 2)).clamp(1, RESYNTH_FRAMES);
        let hop = if count > 1 {
            (x.len() - win) / (count - 1)
        } else {
            0
        };
        let fft = FftPlanner::new().plan_fft_forward(n);
        let hann: Vec<f32> = (0..win)
            .map(|i| 0.5 - 0.5 * (TAU * i as f32 / win as f32).cos())
            .collect();
        let mut buf = vec![Complex::default(); n];
        let mut frames = Vec::with_capacity(count);
        let bin_hz = sr / n as f32;
        for c in 0..count {
            let at = (c * hop).min(x.len().saturating_sub(win));
            for (i, b) in buf.iter_mut().enumerate() {
                let v = if i < win {
                    x.get(at + i).copied().unwrap_or(0.0) * hann[i]
                } else {
                    0.0
                };
                *b = Complex::new(v, 0.0);
            }
            fft.process(&mut buf);
            let mut levels = [0.0_f32; PARTIALS];
            for (k, l) in levels.iter_mut().enumerate() {
                let f = f0 * (k + 1) as f32;
                if f >= 0.5 * sr {
                    break;
                }
                // The strongest bin within a third of the harmonic spacing.
                let centre = f / bin_hz;
                let span = (0.33 * f0 / bin_hz).max(1.0);
                let lo = (centre - span).max(1.0) as usize;
                let hi = ((centre + span) as usize).min(n / 2 - 1);
                *l = (lo..=hi).map(|b| buf[b].norm()).fold(0.0, f32::max);
            }
            frames.push(levels);
        }
        let max = frames.iter().flatten().fold(0.0_f32, |m, &v| m.max(v));
        if max <= 0.0 {
            return None;
        }
        for f in &mut frames {
            for v in f.iter_mut() {
                *v /= max;
            }
        }
        Some(Self {
            name: name.to_owned(),
            f0,
            frames,
            frame_s: hop.max(1) as f32 / sr,
        })
    }

    /// The partial levels at `t` (0 to 1 of the frames), between frames.
    pub fn levels_at(&self, t: f32, out: &mut [f32; PARTIALS]) {
        let n = self.frames.len();
        if n == 0 {
            *out = [0.0; PARTIALS];
            return;
        }
        let pos = t.clamp(0.0, 1.0) * (n - 1) as f32;
        let i = (pos as usize).min(n - 1);
        let j = (i + 1).min(n - 1);
        let f = pos - i as f32;
        for (k, o) in out.iter_mut().enumerate() {
            *o = self.frames[i][k] + (self.frames[j][k] - self.frames[i][k]) * f;
        }
    }

    /// The length the frames cover, seconds.
    pub fn duration_s(&self) -> f32 {
        (self.frames.len().max(1) as f32 * self.frame_s).max(1e-3)
    }
}

/// The sample's pitch: the median of YIN estimates over its loudest stretch.
fn pitch_of(x: &[f32], sr: f32) -> Option<f32> {
    let w = 2048_usize.min(x.len() / 2);
    let mut det = crate::analysis::PitchDetector::new(w);
    let need = det.input_len();
    let mut found = Vec::new();
    let step = (x.len() / 12).max(need / 2);
    let (mut at, mut tried) = (0, 0);
    while at + need <= x.len() {
        tried += 1;
        if let Some((hz, clarity)) = det.detect(&x[at..at + need], sr, 30.0, 2000.0) {
            if clarity > 0.7 {
                found.push(hz);
            }
        }
        at += step;
    }
    // Pitched over at least half of the sample.
    if found.is_empty() || 2 * found.len() < tried {
        return None;
    }
    found.sort_by(f32::total_cmp);
    Some(found[found.len() / 2])
}

/// One voice's partials.
#[derive(Debug, Clone, Copy)]
pub struct AdditiveVoice {
    re: [f32; PARTIALS],
    im: [f32; PARTIALS],
    cos: [f32; PARTIALS],
    sin: [f32; PARTIALS],
    amp: [f32; PARTIALS],
    step: [f32; PARTIALS],
    /// Partials in use (the rest are silent).
    n: usize,
    /// Seconds since the note started (for scanning the resynthesis).
    elapsed: f32,
    fresh: bool,
}

impl Default for AdditiveVoice {
    fn default() -> Self {
        Self {
            re: [1.0; PARTIALS],
            im: [0.0; PARTIALS],
            cos: [1.0; PARTIALS],
            sin: [0.0; PARTIALS],
            amp: [0.0; PARTIALS],
            step: [0.0; PARTIALS],
            n: 0,
            elapsed: 0.0,
            fresh: true,
        }
    }
}

impl AdditiveVoice {
    /// A new note. A stolen voice keeps its phases and levels, so it does not click.
    pub fn start(&mut self, stolen: bool) {
        self.elapsed = 0.0;
        if !stolen {
            *self = Self::default();
        }
    }

    /// Recomputes every partial's frequency and level for the next `frames` samples.
    pub fn control(
        &mut self,
        s: &AdditiveSettings,
        resynth: Option<&Resynth>,
        f0: f32,
        sr: f32,
        frames: u32,
        m: AdditiveMods,
    ) {
        let mut base = [0.0_f32; PARTIALS];
        base[..HARMONICS].copy_from_slice(&s.harmonics);
        let morph = (s.morph + m.morph).clamp(0.0, 1.0);
        if let (Some(r), true) = (resynth, morph > 0.0) {
            let t = s.position + m.position + s.scan * self.elapsed / r.duration_s();
            let mut res = [0.0_f32; PARTIALS];
            r.levels_at(t, &mut res);
            for (b, r) in base.iter_mut().zip(res) {
                *b += (r - *b) * morph;
            }
        }
        let power: f32 = base.iter().map(|b| b * b).sum();
        let norm = if power > 1.0 {
            power.sqrt().recip()
        } else {
            1.0
        };
        let tilt = s.tilt_db + m.tilt_db;
        let exponent = 1.0 + 0.1 * s.stretch.clamp(-1.0, 1.0);
        let nyq = 0.45 * sr;
        let ctl = frames.max(1) as f32;
        let mut n = 0;
        #[allow(clippy::needless_range_loop)] // `k` indexes six parallel arrays
        for k in 0..PARTIALS {
            let h = (k + 1) as f32;
            let f = f0 * h.powf(exponent);
            let mut target = 0.0;
            if f < nyq && f > 0.0 {
                let mut g = base[k].max(0.0) * norm;
                if g > 0.0 {
                    g *= crate::db_to_gain(tilt * h.log2());
                    if k % 2 == 1 {
                        g *= s.even.clamp(0.0, 2.0);
                    }
                    g *= filter_gain(s, f, m.cutoff_oct);
                }
                target = g.min(4.0);
                let w = TAU * f / sr;
                self.cos[k] = w.cos();
                self.sin[k] = w.sin();
            }
            if self.fresh {
                self.amp[k] = target;
                self.step[k] = 0.0;
            } else {
                self.step[k] = (target - self.amp[k]) / ctl;
            }
            if target > 0.0 || self.amp[k] > 0.0 {
                n = k + 1;
            }
            // Keep the phasor on the unit circle.
            let mag2 = self.re[k] * self.re[k] + self.im[k] * self.im[k];
            let fix = 1.5 - 0.5 * mag2;
            self.re[k] *= fix;
            self.im[k] *= fix;
        }
        self.n = n;
        self.fresh = false;
        self.elapsed += ctl / sr;
    }

    /// The next sample.
    #[inline]
    pub fn tick(&mut self) -> f32 {
        let mut y = 0.0;
        for k in 0..self.n {
            let (re, im) = (self.re[k], self.im[k]);
            y += im * self.amp[k];
            self.re[k] = re * self.cos[k] - im * self.sin[k];
            self.im[k] = re * self.sin[k] + im * self.cos[k];
            self.amp[k] += self.step[k];
        }
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn run(v: &mut AdditiveVoice, s: &AdditiveSettings, r: Option<&Resynth>, n: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            v.control(s, r, 220.0, SR, 16, AdditiveMods::default());
            for _ in 0..16 {
                out.push(v.tick());
            }
        }
        out
    }

    /// Level of harmonic `k` of 220 Hz in `x`, by correlation.
    fn harmonic(x: &[f32], k: usize) -> f32 {
        let w = TAU * 220.0 * k as f32 / SR;
        let (mut c, mut s) = (0.0_f32, 0.0_f32);
        for (i, v) in x.iter().enumerate() {
            c += v * (w * i as f32).cos();
            s += v * (w * i as f32).sin();
        }
        2.0 * (c * c + s * s).sqrt() / x.len() as f32
    }

    #[test]
    fn harmonics_sound_at_their_levels() {
        let mut s = AdditiveSettings {
            harmonics: [0.0; HARMONICS],
            ..AdditiveSettings::default()
        };
        s.harmonics[0] = 1.0;
        s.harmonics[2] = 0.5;
        let mut v = AdditiveVoice::default();
        let x = run(&mut v, &s, None, 48_000);
        // Normalized by the total power 1.25.
        let n = 1.25_f32.sqrt();
        assert!(
            (harmonic(&x, 1) - 1.0 / n).abs() < 0.01,
            "{}",
            harmonic(&x, 1)
        );
        assert!((harmonic(&x, 3) - 0.5 / n).abs() < 0.01);
        assert!(harmonic(&x, 2) < 0.001);
        assert!(x.iter().all(|v| v.is_finite() && v.abs() < 2.0));
        // Even harmonics off, tilt down.
        let s = AdditiveSettings {
            even: 0.0,
            tilt_db: -6.0,
            ..AdditiveSettings::default()
        };
        let x = run(&mut AdditiveVoice::default(), &s, None, 48_000);
        assert!(harmonic(&x, 2) < 0.001 && harmonic(&x, 4) < 0.001);
        let r = harmonic(&x, 3) / harmonic(&x, 1);
        // A saw's third harmonic (1/3) a further 6 dB x log2(3) down.
        let want = (1.0 / 3.0) * crate::db_to_gain(-6.0 * 3f32.log2());
        assert!((r - want).abs() < 0.01, "{r} {want}");
    }

    #[test]
    fn the_spectral_filter_shapes_partials() {
        let mut s = AdditiveSettings {
            filter: SpecFilter::LowPass,
            filter_hz: 700.0,
            width_oct: 0.5,
            ..AdditiveSettings::default()
        };
        assert!(filter_gain(&s, 300.0, 0.0) > 0.9);
        assert!(filter_gain(&s, 2000.0, 0.0) < 0.01);
        // An octave of cutoff modulation moves it.
        assert!(filter_gain(&s, 1200.0, 1.0) > 0.9);
        s.filter = SpecFilter::HighPass;
        assert!(filter_gain(&s, 300.0, 0.0) < 0.01);
        s.filter = SpecFilter::BandPass;
        s.width_oct = 1.0;
        assert!((filter_gain(&s, 700.0, 0.0) - 1.0).abs() < 1e-3);
        assert!(filter_gain(&s, 2800.0, 0.0) < 0.01);
        s.filter = SpecFilter::Notch;
        assert!(filter_gain(&s, 700.0, 0.0) < 1e-3);
        s.filter = SpecFilter::Comb;
        assert!(filter_gain(&s, 1400.0, 0.0) > 0.99);
        assert!(filter_gain(&s, 1050.0, 0.0) < 0.01);
        s.filter = SpecFilter::Formant;
        s.vowel = 0.0; // A: 800 Hz, not 2000
        assert!(filter_gain(&s, 800.0, 0.0) > 0.9);
        assert!(filter_gain(&s, 2000.0, 0.0) < 0.1);
        // The band-pass sweep reaches the sound.
        let s = AdditiveSettings {
            filter: SpecFilter::BandPass,
            filter_hz: 220.0 * 5.0,
            width_oct: 0.3,
            ..AdditiveSettings::default()
        };
        let x = run(&mut AdditiveVoice::default(), &s, None, 48_000);
        assert!(harmonic(&x, 5) > 4.0 * harmonic(&x, 1));
    }

    #[test]
    fn resynthesis_recovers_a_samples_harmonics_and_their_change() {
        // Two seconds at 110 Hz: harmonics 1 and 2 at first, crossfading to 1 and 3.
        let n = 96_000;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / SR;
                let w = TAU * 110.0 * t;
                let fade = i as f32 / n as f32;
                w.sin() + 0.6 * (1.0 - fade) * (2.0 * w).sin() + 0.6 * fade * (3.0 * w).sin()
            })
            .collect();
        let r = Resynth::analyze("test", &x, SR).unwrap();
        assert!((r.f0 - 110.0).abs() < 1.0, "{}", r.f0);
        assert!(r.frames.len() > 8);
        assert!((r.duration_s() - 2.0).abs() < 0.3, "{}", r.duration_s());
        let first = r.frames[0];
        let last = r.frames[r.frames.len() - 1];
        assert!((first[1] / first[0] - 0.6).abs() < 0.1, "{first:?}");
        assert!(first[2] < 0.1 && last[1] < 0.1);
        assert!((last[2] / last[0] - 0.6).abs() < 0.1, "{last:?}");
        // Played back at 220 Hz with the editor out of the way, the second harmonic leads at
        // the start and the third at the end.
        let s = AdditiveSettings {
            morph: 1.0,
            scan: 1.0,
            ..AdditiveSettings::default()
        };
        let mut v = AdditiveVoice::default();
        let y = run(&mut v, &s, Some(&r), 2 * 48_000);
        let (a, b) = (&y[2400..12_000], &y[84_000..93_600]);
        assert!(harmonic(a, 2) > 3.0 * harmonic(a, 3));
        assert!(harmonic(b, 3) > 3.0 * harmonic(b, 2));
        // No pitch, no resynthesis.
        let mut seed = 0x1234_5678_u32;
        let noise: Vec<f32> = (0..48_000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed >> 8) as f32 / 8_388_608.0 - 1.0
            })
            .collect();
        assert!(Resynth::analyze("noise", &noise, SR).is_none());
    }

    #[test]
    fn partials_above_nyquist_are_left_out() {
        let s = AdditiveSettings::default();
        let mut v = AdditiveVoice::default();
        v.control(&s, None, 2000.0, SR, 16, AdditiveMods::default());
        // 0.45 x 48 kHz / 2 kHz: ten partials.
        assert_eq!(v.n, 10);
    }
}
