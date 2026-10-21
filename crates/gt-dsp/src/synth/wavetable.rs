//! Wavetables for the wavetable engine of Gloom Synth (Prompt 24).
//!
//! A table is a sequence of single-cycle frames, [`TABLE_LEN`] samples each. Every frame is
//! stored as [`MIPS`] band-limited copies (mip levels): level `l` keeps harmonics up to
//! `1024 >> l`, so a note picks the level whose top harmonic stays under Nyquist and the
//! oscillator does not alias. The copies are built from each frame's spectrum with an inverse
//! FFT, off the audio thread; the oscillator reads with linear interpolation inside a frame and
//! crossfades between neighbouring frames for the position.
//!
//! The factory tables are generated, not sampled: additive (shape morph, harmonic sweep,
//! drawbars), formant (vowels), pulse width, noise-derived spectra, a sync sweep and a
//! wavefolder. User tables come from single-cycle WAV files (one cycle, or frames of 2048).

use std::sync::{Arc, OnceLock};

use rustfft::num_complex::Complex32;
use rustfft::FftPlanner;

use crate::lfo::Noise;

/// Samples per frame.
pub const TABLE_LEN: usize = 2048;
/// Mip levels per frame (1024 harmonics down to 1).
pub const MIPS: usize = 11;
/// Most frames in a table.
pub const MAX_FRAMES: usize = 256;
/// Frames of each generated table.
const FACTORY_FRAMES: usize = 16;
/// Highest harmonic stored.
const HARMONICS: usize = TABLE_LEN / 2;
/// Stride of one mip level (with a guard sample for interpolation).
const STRIDE: usize = TABLE_LEN + 1;

/// Names of the factory tables, in order.
pub const FACTORY_TABLES: [&str; 8] = [
    "Basic Shapes",
    "Harmonic Sweep",
    "Organ Bars",
    "Vowels",
    "PWM",
    "Dust",
    "Sync Saw",
    "Fold",
];

/// How an oscillator bends its phase before reading the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Warp {
    /// Plain.
    #[default]
    Off,
    /// Hard sync: reads the cycle up to 8 times per period.
    Sync,
    /// Bends the phase (t^e): squeezes the cycle towards its end.
    Bend,
    /// Pulse-width warp: the first half of the cycle plays in a shorter part of the period.
    Pwm,
}

impl Warp {
    /// The warped phase of `t` (0 to 1) at `amount` (0 to 1).
    #[inline]
    pub fn apply(self, t: f32, amount: f32) -> f32 {
        match self {
            Warp::Off => t,
            Warp::Sync => super::common::fract(t * (1.0 + 7.0 * amount)),
            Warp::Bend => t.powf(1.0 + 4.0 * amount),
            Warp::Pwm => {
                let w = 0.5 - 0.48 * amount;
                if t < w {
                    0.5 * t / w
                } else {
                    0.5 + 0.5 * (t - w) / (1.0 - w)
                }
            }
        }
    }

    /// Steepest speed-up of the phase, to pick a mip level that keeps the warped cycle under
    /// Nyquist.
    #[inline]
    pub fn speed(self, amount: f32) -> f32 {
        match self {
            Warp::Off => 1.0,
            Warp::Sync => 1.0 + 7.0 * amount,
            Warp::Bend => 1.0 + 4.0 * amount,
            Warp::Pwm => 0.5 / (0.5 - 0.48 * amount),
        }
    }
}

/// The mip level for a phase increment (cycles per sample).
#[inline]
pub fn mip_for(inc: f32) -> usize {
    let h = inc.abs() * TABLE_LEN as f32;
    if h <= 1.0 {
        0
    } else {
        (h.log2().ceil() as usize).min(MIPS - 1)
    }
}

/// A band-limited wavetable.
#[derive(Debug, Clone)]
pub struct Wavetable {
    name: String,
    frames: usize,
    data: Vec<f32>,
}

/// One frame's spectrum: the cosine and sine amplitude of harmonics 1 to 1024.
type Spectrum = Vec<Complex32>;

impl Wavetable {
    /// Name shown in the panel.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Number of frames.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Builds a table from frame spectra (`spectra[f][h - 1]` = (cos, sin) amplitude of
    /// harmonic `h`). Each frame is normalized to a peak of 1.
    fn from_spectra(name: &str, spectra: &[Spectrum]) -> Self {
        let frames = spectra.len().clamp(1, MAX_FRAMES);
        let mut data = vec![0.0; frames * MIPS * STRIDE];
        let fft = FftPlanner::<f32>::new().plan_fft_inverse(TABLE_LEN);
        let mut buf = vec![Complex32::default(); TABLE_LEN];
        for (f, spec) in spectra.iter().take(frames).enumerate() {
            let mut gain = 1.0;
            for l in 0..MIPS {
                let top = (HARMONICS >> l).min(spec.len());
                buf.fill(Complex32::default());
                for h in 1..=top {
                    // x(t) = a·cos + b·sin  ⇔  X[h] = (a - jb)/2, X[N-h] = (a + jb)/2.
                    let c = spec[h - 1];
                    buf[h] = Complex32::new(0.5 * c.re, -0.5 * c.im);
                    buf[TABLE_LEN - h] += Complex32::new(0.5 * c.re, 0.5 * c.im);
                }
                fft.process(&mut buf);
                let out = &mut data[(f * MIPS + l) * STRIDE..][..STRIDE];
                if l == 0 {
                    let peak = buf.iter().map(|c| c.re.abs()).fold(0.0_f32, f32::max);
                    gain = if peak > 1e-9 { 1.0 / peak } else { 0.0 };
                }
                for (o, c) in out.iter_mut().zip(&buf) {
                    *o = c.re * gain;
                }
                out[TABLE_LEN] = out[0];
            }
        }
        Self {
            name: name.to_owned(),
            frames,
            data,
        }
    }

    /// Builds a table from single cycles of any length (each resampled to [`TABLE_LEN`]).
    pub fn from_cycles(name: &str, cycles: &[Vec<f32>]) -> Self {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(TABLE_LEN);
        let spectra: Vec<Spectrum> = cycles
            .iter()
            .map(|c| {
                let n = c.len().max(1);
                let mut buf: Vec<Complex32> = (0..TABLE_LEN)
                    .map(|i| {
                        let x = i as f32 * n as f32 / TABLE_LEN as f32;
                        let k = x as usize;
                        let fr = x - k as f32;
                        let a = c.get(k).copied().unwrap_or(0.0);
                        let b = c.get((k + 1) % n).copied().unwrap_or(0.0);
                        Complex32::new(a + (b - a) * fr, 0.0)
                    })
                    .collect();
                fft.process(&mut buf);
                let s = 2.0 / TABLE_LEN as f32;
                (1..=HARMONICS)
                    .map(|h| Complex32::new(buf[h].re * s, -buf[h].im * s))
                    .collect()
            })
            .collect();
        Self::from_spectra(name, &spectra)
    }

    /// A table from audio: frames of [`TABLE_LEN`] samples when the length is a multiple of
    /// it (at most [`MAX_FRAMES`]; longer files keep every n-th frame), else the whole file as
    /// one cycle. `None` for fewer than 8 samples.
    pub fn from_audio(name: &str, x: &[f32]) -> Option<Self> {
        if x.len() < 8 {
            return None;
        }
        let cycles: Vec<Vec<f32>> = if x.len() >= TABLE_LEN && x.len() % TABLE_LEN == 0 {
            let n = x.len() / TABLE_LEN;
            let step = n.div_ceil(MAX_FRAMES);
            x.chunks(TABLE_LEN)
                .step_by(step)
                .map(<[f32]>::to_vec)
                .collect()
        } else {
            vec![x.to_vec()]
        };
        Some(Self::from_cycles(name, &cycles))
    }

    /// The sample at phase `t` (0 to 1) of frame position `pos` (0 to 1) from mip level `mip`.
    #[inline]
    pub fn read(&self, pos: f32, t: f32, mip: usize) -> f32 {
        let fpos = pos.clamp(0.0, 1.0) * (self.frames - 1) as f32;
        let f0 = (fpos as usize).min(self.frames - 1);
        let ff = fpos - f0 as f32;
        let x = super::common::fract(t) * TABLE_LEN as f32;
        let i = (x as usize).min(TABLE_LEN - 1);
        let fr = x - i as f32;
        let mip = mip.min(MIPS - 1);
        let at = |f: usize| {
            let d = &self.data[(f * MIPS + mip) * STRIDE + i..][..2];
            d[0] + (d[1] - d[0]) * fr
        };
        let a = at(f0);
        if ff > 0.0 && f0 + 1 < self.frames {
            a + (at(f0 + 1) - a) * ff
        } else {
            a
        }
    }
}

/// The factory tables.
#[derive(Debug)]
pub struct WtBank {
    /// In the order of [`FACTORY_TABLES`].
    pub tables: Vec<Wavetable>,
}

/// What a synth's wavetable oscillators read: the factory tables and each oscillator's user
/// table, if it has one (it replaces the factory choice).
#[derive(Debug, Clone)]
pub struct SynthTables {
    /// Factory tables.
    pub bank: Arc<WtBank>,
    /// User tables of oscillators 1 and 2.
    pub user: [Option<Arc<Wavetable>>; 2],
    /// The additive engine's resynthesized sample (Prompt 34).
    pub resynth: Option<Arc<super::additive::Resynth>>,
}

impl SynthTables {
    /// The factory tables and no user tables.
    pub fn factory() -> Self {
        Self {
            bank: factory_bank(),
            user: [None, None],
            resynth: None,
        }
    }

    /// The table oscillator `osc` reads with factory choice `table`.
    #[inline]
    pub fn get(&self, osc: usize, table: usize) -> Option<&Wavetable> {
        match self.user.get(osc) {
            Some(Some(t)) => Some(t),
            _ => self.bank.tables.get(table),
        }
    }
}

/// The factory tables, built on first use (about 12 MB; call off the audio thread).
pub fn factory_bank() -> Arc<WtBank> {
    static BANK: OnceLock<Arc<WtBank>> = OnceLock::new();
    Arc::clone(BANK.get_or_init(|| {
        Arc::new(WtBank {
            tables: (0..FACTORY_TABLES.len()).map(factory_table).collect(),
        })
    }))
}

/// Linear blend of two spectra.
fn mix(a: &Spectrum, b: &Spectrum, t: f32) -> Spectrum {
    a.iter().zip(b).map(|(x, y)| x + (y - x) * t).collect()
}

/// A spectrum with sine amplitude `f(h)` for every harmonic.
fn sines(f: impl Fn(usize) -> f32) -> Spectrum {
    (1..=HARMONICS).map(|h| Complex32::new(0.0, f(h))).collect()
}

/// Morph through `keys` across the frames.
fn morph(keys: &[Spectrum]) -> Vec<Spectrum> {
    (0..FACTORY_FRAMES)
        .map(|k| {
            let x = k as f32 / (FACTORY_FRAMES - 1) as f32 * (keys.len() - 1) as f32;
            let i = (x as usize).min(keys.len() - 2);
            mix(&keys[i], &keys[i + 1], x - i as f32)
        })
        .collect()
}

fn factory_table(index: usize) -> Wavetable {
    use std::f32::consts::PI;
    let name = FACTORY_TABLES[index];
    let frame_t = |k: usize| k as f32 / (FACTORY_FRAMES - 1) as f32;
    match index {
        // Sine, triangle, saw, square.
        0 => {
            let sine = sines(|h| if h == 1 { 1.0 } else { 0.0 });
            let tri = sines(|h| {
                if h % 2 == 1 {
                    let s = if (h / 2) % 2 == 0 { 1.0 } else { -1.0 };
                    s * 8.0 / (PI * PI * (h * h) as f32)
                } else {
                    0.0
                }
            });
            let saw = sines(|h| 2.0 / (PI * h as f32));
            let square = sines(|h| {
                if h % 2 == 1 {
                    4.0 / (PI * h as f32)
                } else {
                    0.0
                }
            });
            Wavetable::from_spectra(name, &morph(&[sine, tri, saw, square]))
        }
        // Harmonics added one octave band at a time: 1, then up to 2, 4, ... 1024 (1/h).
        1 => {
            let frames: Vec<Spectrum> = (0..FACTORY_FRAMES)
                .map(|k| {
                    let top = 2f32.powf(frame_t(k) * 10.0);
                    sines(|h| {
                        let h = h as f32;
                        if h <= top {
                            1.0 / h
                        } else {
                            0.0
                        }
                    })
                })
                .collect();
            Wavetable::from_spectra(name, &frames)
        }
        // Drawbar registrations on harmonics 1, 2, 3, 4, 5, 6, 8.
        2 => {
            const BARS: [usize; 7] = [1, 2, 3, 4, 5, 6, 8];
            const REGS: [[f32; 7]; 5] = [
                [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                [1.0, 0.8, 0.0, 0.6, 0.0, 0.0, 0.0],
                [1.0, 1.0, 0.8, 0.0, 0.0, 0.0, 0.5],
                [0.8, 0.6, 0.0, 0.6, 0.4, 0.4, 0.8],
                [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            ];
            let keys: Vec<Spectrum> = REGS
                .iter()
                .map(|r| {
                    sines(|h| {
                        BARS.iter()
                            .position(|&b| b == h)
                            .map_or(0.0, |i| r[i] * r[i])
                    })
                })
                .collect();
            Wavetable::from_spectra(name, &morph(&keys))
        }
        // Vowel formants A, E, I, O, U over a 110 Hz voice.
        3 => {
            const F0: f32 = 110.0;
            const VOWELS: [[(f32, f32); 3]; 5] = [
                [(800.0, 1.0), (1150.0, 0.5), (2900.0, 0.25)],
                [(400.0, 1.0), (2000.0, 0.4), (2600.0, 0.3)],
                [(300.0, 1.0), (2300.0, 0.3), (3000.0, 0.25)],
                [(450.0, 1.0), (800.0, 0.6), (2830.0, 0.1)],
                [(325.0, 1.0), (700.0, 0.4), (2530.0, 0.08)],
            ];
            let keys: Vec<Spectrum> = VOWELS
                .iter()
                .map(|formants| {
                    // A glottal-like source (1/h) shaped by three resonances 90 Hz wide.
                    sines(|h| {
                        let f = F0 * h as f32;
                        let shape: f32 = formants
                            .iter()
                            .map(|&(c, g)| g / (1.0 + ((f - c) / 90.0).powi(2)))
                            .sum();
                        shape / (h as f32).sqrt()
                    })
                })
                .collect();
            Wavetable::from_spectra(name, &morph(&keys))
        }
        // A pulse narrowing from 50 % to 2 %.
        4 => {
            let frames: Vec<Spectrum> = (0..FACTORY_FRAMES)
                .map(|k| {
                    let w = 0.5 - 0.48 * frame_t(k);
                    (1..=HARMONICS)
                        .map(|h| {
                            let h = h as f32;
                            let a = 2.0 / (PI * h) * (PI * h * w).sin();
                            // Cosine series of a pulse centred on w/2.
                            let ph = -2.0 * PI * h * w / 2.0;
                            Complex32::new(a * ph.cos(), -a * ph.sin())
                        })
                        .collect()
                })
                .collect();
            Wavetable::from_spectra(name, &frames)
        }
        // Noise-derived: random magnitudes and phases with a 1/h tilt, four keys morphed.
        5 => {
            let mut n = Noise::new(0x51ab_2f07);
            let keys: Vec<Spectrum> = (0..4)
                .map(|k| {
                    let tilt = 0.6 + 0.3 * k as f32;
                    (1..=HARMONICS)
                        .map(|h| {
                            let m = (0.5 + 0.5 * n.next_value()) / (h as f32).powf(tilt);
                            let ph = PI * n.next_value();
                            Complex32::new(m * ph.cos(), m * ph.sin())
                        })
                        .collect()
                })
                .collect();
            Wavetable::from_spectra(name, &morph(&keys))
        }
        // A saw hard-synced at 1 to 6 times the period.
        6 => {
            let cycles: Vec<Vec<f32>> = (0..FACTORY_FRAMES)
                .map(|k| {
                    let r = 1.0 + 5.0 * frame_t(k);
                    (0..TABLE_LEN)
                        .map(|i| {
                            let x = i as f32 / TABLE_LEN as f32 * r;
                            2.0 * (x - x.floor()) - 1.0
                        })
                        .collect()
                })
                .collect();
            Wavetable::from_cycles(name, &cycles)
        }
        // A sine through a sine wavefolder, drive 1 to 8.
        _ => {
            let cycles: Vec<Vec<f32>> = (0..FACTORY_FRAMES)
                .map(|k| {
                    let g = 1.0 + 7.0 * frame_t(k);
                    (0..TABLE_LEN)
                        .map(|i| {
                            let s = (2.0 * PI * i as f32 / TABLE_LEN as f32).sin();
                            (g * s * PI * 0.5).sin()
                        })
                        .collect()
                })
                .collect();
            Wavetable::from_cycles(name, &cycles)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_levels_keep_harmonics_under_nyquist() {
        for inc in [0.0001_f32, 0.001, 0.01, 0.05, 0.2, 0.45] {
            let mip = mip_for(inc);
            let top = (HARMONICS >> mip) as f32;
            assert!(top * inc <= 0.5 + 1e-6 || mip == MIPS - 1, "{inc} {mip}");
        }
        assert_eq!(mip_for(1.0 / 4096.0), 0);
    }

    #[test]
    fn a_saw_spectrum_reads_back_as_a_saw_and_mips_are_band_limited() {
        let saw = sines(|h| 2.0 / (std::f32::consts::PI * h as f32));
        let t = Wavetable::from_spectra("saw", &[saw]);
        // Mip 10 keeps only the fundamental: a sine (normalized to 1 at mip 0).
        let peak0 = (0..TABLE_LEN)
            .map(|i| t.read(0.0, i as f32 / TABLE_LEN as f32, 0).abs())
            .fold(0.0, f32::max);
        assert!((peak0 - 1.0).abs() < 1e-3);
        let s = t.read(0.0, 0.25, MIPS - 1);
        let s2 = t.read(0.0, 0.75, MIPS - 1);
        assert!(s > 0.0 && (s + s2).abs() < 1e-3, "{s} {s2}");
    }

    #[test]
    fn cycles_and_audio_import() {
        let cyc: Vec<f32> = (0..600)
            .map(|i| (std::f32::consts::TAU * i as f32 / 600.0).sin())
            .collect();
        let t = Wavetable::from_audio("one", &cyc).unwrap();
        assert_eq!(t.frames(), 1);
        assert!((t.read(0.0, 0.25, 0) - 1.0).abs() < 1e-2);
        let multi = vec![0.5_f32; TABLE_LEN * 3];
        assert_eq!(Wavetable::from_audio("m", &multi).unwrap().frames(), 3);
        assert!(Wavetable::from_audio("x", &[0.0; 4]).is_none());
    }

    #[test]
    fn warps_stay_in_range() {
        for w in [Warp::Off, Warp::Sync, Warp::Bend, Warp::Pwm] {
            for k in 0..100 {
                let t = k as f32 / 100.0;
                let y = w.apply(t, 0.7);
                assert!((0.0..=1.0).contains(&y), "{w:?} {t} {y}");
            }
        }
        assert!((Warp::Pwm.apply(0.02, 1.0) - 0.5).abs() < 1e-6);
    }
}
