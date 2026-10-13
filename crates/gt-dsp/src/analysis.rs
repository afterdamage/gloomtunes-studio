//! Signal analysis for meters and the analyzer: a lock-free tap the audio thread writes and
//! the UI reads, an FFT spectrum, stereo correlation and a pitch detector.
//!
//! Only [`Scope::push`] runs on the audio thread. [`Spectrum`] runs on the UI thread on a copy
//! of the signal. [`PitchDetector`] does not allocate after construction, so the pitch
//! shifter's correction mode uses it on the audio thread too.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// Frames a [`Scope`] keeps.
pub const SCOPE_FRAMES: usize = 8192;

/// The last [`SCOPE_FRAMES`] stereo frames of a signal. The audio thread pushes, any thread
/// reads; samples are atomics, so a read racing a write sees some old and some new samples
/// (harmless for a display) and never blocks either side.
#[derive(Debug)]
pub struct Scope {
    l: Box<[AtomicU32]>,
    r: Box<[AtomicU32]>,
    written: AtomicUsize,
    sample_rate: f32,
}

impl Scope {
    /// An empty tap for a signal at `sample_rate`.
    pub fn new(sample_rate: f32) -> Self {
        let zeros = || (0..SCOPE_FRAMES).map(|_| AtomicU32::new(0)).collect();
        Self {
            l: zeros(),
            r: zeros(),
            written: AtomicUsize::new(0),
            sample_rate,
        }
    }

    /// The signal's sample rate.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Frames written so far (wraps). Lets a reader tell whether anything new arrived.
    pub fn written(&self) -> usize {
        self.written.load(Ordering::Acquire)
    }

    /// Appends a block. Real-time safe.
    pub fn push(&self, l: &[f32], r: &[f32]) {
        let start = self.written.load(Ordering::Relaxed);
        for (i, (a, b)) in l.iter().zip(r).enumerate() {
            let at = (start.wrapping_add(i)) % SCOPE_FRAMES;
            self.l[at].store(a.to_bits(), Ordering::Relaxed);
            self.r[at].store(b.to_bits(), Ordering::Relaxed);
        }
        self.written
            .store(start.wrapping_add(l.len().min(r.len())), Ordering::Release);
    }

    /// Copies the latest `n` frames (at most [`SCOPE_FRAMES`]) into `l` and `r`, oldest first.
    pub fn read(&self, n: usize, l: &mut Vec<f32>, r: &mut Vec<f32>) {
        let n = n.min(SCOPE_FRAMES);
        let end = self.written.load(Ordering::Acquire);
        l.clear();
        r.clear();
        for i in 0..n {
            let at = end.wrapping_sub(n).wrapping_add(i) % SCOPE_FRAMES;
            l.push(f32::from_bits(self.l[at].load(Ordering::Relaxed)));
            r.push(f32::from_bits(self.r[at].load(Ordering::Relaxed)));
        }
    }
}

/// Magnitude spectrum with a Hann window, scaled so a full-scale sine reads 0 dB in its bin.
#[derive(Clone)]
pub struct Spectrum {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buf: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl std::fmt::Debug for Spectrum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Spectrum({})", self.window.len())
    }
}

impl Spectrum {
    /// A spectrum of `size` points (a power of two is fastest).
    pub fn new(size: usize) -> Self {
        let size = size.max(16);
        let fft = FftPlanner::new().plan_fft_forward(size);
        let window: Vec<f32> = (0..size)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / size as f32).cos())
            .collect();
        Self {
            scratch: vec![Complex::default(); fft.get_inplace_scratch_len()],
            fft,
            window,
            buf: vec![Complex::default(); size],
        }
    }

    /// Points per transform.
    pub fn size(&self) -> usize {
        self.window.len()
    }

    /// Level in dB of each bin `0..size/2` of the last `size` samples of `x` (zero-padded when
    /// shorter). Bin `k` is at `k * sample_rate / size` Hz.
    pub fn analyze(&mut self, x: &[f32], out_db: &mut Vec<f32>) {
        let n = self.size();
        let skip = x.len().saturating_sub(n);
        for (i, b) in self.buf.iter_mut().enumerate() {
            let v = x.get(skip + i).copied().unwrap_or(0.0);
            *b = Complex::new(v * self.window[i], 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.buf, &mut self.scratch);
        // A Hann window halves a sine's amplitude; a real sine splits over two half-bins.
        let scale = 4.0 / n as f32;
        out_db.clear();
        out_db.extend(
            self.buf[..n / 2]
                .iter()
                .map(|c| 20.0 * (c.norm() * scale).max(1e-10).log10()),
        );
    }
}

/// Stereo correlation of two signals: +1 when identical (mono), 0 when unrelated, -1 when one
/// is the other inverted. Silence reads +1.
pub fn correlation(l: &[f32], r: &[f32]) -> f32 {
    let (mut lr, mut ll, mut rr) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (&a, &b) in l.iter().zip(r) {
        lr += f64::from(a) * f64::from(b);
        ll += f64::from(a) * f64::from(a);
        rr += f64::from(b) * f64::from(b);
    }
    let den = (ll * rr).sqrt();
    if den < 1e-12 {
        1.0
    } else {
        (lr / den).clamp(-1.0, 1.0) as f32
    }
}

/// MIDI note (69 = A4 at 440 Hz) nearest to `hz`, and how far `hz` is from it in cents.
pub fn note_of(hz: f32) -> (i32, f32) {
    let m = 69.0 + 12.0 * (hz.max(1e-3) / 440.0).log2();
    let n = m.round();
    (n as i32, (m - n) * 100.0)
}

/// Frequency of MIDI note `note` (may be fractional).
pub fn note_hz(note: f32) -> f32 {
    440.0 * ((note - 69.0) / 12.0).exp2()
}

/// Note name of a MIDI note, with the octave numbered so that 60 is C4.
pub fn note_name(note: i32) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!(
        "{}{}",
        NAMES[note.rem_euclid(12) as usize],
        note.div_euclid(12) - 1
    )
}

/// YIN pitch detection (de Cheveigné and Kawahara, 2002) with the difference function computed
/// by FFT, so a detection costs a few transforms instead of `window²` multiplies.
///
/// The difference `d(τ) = Σ (x[j] - x[j+τ])²` over a window of `W` samples expands to
/// `E(0) + E(τ) - 2·r(τ)`, where `E(τ)` is the energy of the window starting at `τ` (running
/// sums) and `r(τ)` the cross-correlation of the first window with the whole `2W` input (one
/// FFT product). YIN then normalises `d` by its running mean and takes the first dip under a
/// threshold, refined by a parabola through its neighbours.
#[derive(Clone)]
pub struct PitchDetector {
    w: usize,
    fwd: Arc<dyn Fft<f32>>,
    inv: Arc<dyn Fft<f32>>,
    a: Vec<Complex<f32>>,
    x: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    d: Vec<f32>,
    raw: Vec<f32>,
    energy: Vec<f32>,
}

impl std::fmt::Debug for PitchDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PitchDetector({})", self.w)
    }
}

/// YIN's dip threshold.
const YIN_THRESHOLD: f32 = 0.15;

impl PitchDetector {
    /// A detector with a window of `w` samples; it needs `2 * w` samples of input and finds
    /// periods up to `w` samples long.
    pub fn new(w: usize) -> Self {
        let w = w.max(16);
        let n = (4 * w).next_power_of_two();
        let mut planner = FftPlanner::new();
        let fwd = planner.plan_fft_forward(n);
        let inv = planner.plan_fft_inverse(n);
        let scratch = fwd
            .get_inplace_scratch_len()
            .max(inv.get_inplace_scratch_len());
        Self {
            w,
            fwd,
            inv,
            a: vec![Complex::default(); n],
            x: vec![Complex::default(); n],
            scratch: vec![Complex::default(); scratch],
            d: vec![0.0; w],
            raw: vec![0.0; w],
            energy: vec![0.0; 2 * w + 1],
        }
    }

    /// Samples of input [`Self::detect`] reads.
    pub fn input_len(&self) -> usize {
        2 * self.w
    }

    /// The pitch of the last `2 * w` samples of `x` between `min_hz` and `max_hz`, with a
    /// clarity from 0 to 1 (1 is a pure periodic signal). `None` when there is no clear pitch.
    pub fn detect(&mut self, x: &[f32], sr: f32, min_hz: f32, max_hz: f32) -> Option<(f32, f32)> {
        let w = self.w;
        if x.len() < 2 * w {
            return None;
        }
        let x = &x[x.len() - 2 * w..];
        let n = self.a.len();
        for i in 0..n {
            let v = x.get(i).copied().unwrap_or(0.0);
            self.x[i] = Complex::new(v, 0.0);
            self.a[i] = Complex::new(if i < w { v } else { 0.0 }, 0.0);
        }
        self.fwd
            .process_with_scratch(&mut self.a, &mut self.scratch);
        self.fwd
            .process_with_scratch(&mut self.x, &mut self.scratch);
        for (a, b) in self.a.iter_mut().zip(&self.x) {
            *a = a.conj() * b;
        }
        self.inv
            .process_with_scratch(&mut self.a, &mut self.scratch);
        let scale = 1.0 / n as f32;
        self.energy[0] = 0.0;
        for (i, &v) in x.iter().enumerate() {
            self.energy[i + 1] = self.energy[i] + v * v;
        }
        let e0 = self.energy[w];
        if e0 < 1e-8 * w as f32 {
            return None;
        }
        // Cumulative mean normalised difference, d'(0) = 1.
        let mut sum = 0.0;
        self.d[0] = 1.0;
        for tau in 1..w {
            let e_tau = self.energy[tau + w] - self.energy[tau];
            let diff = (e0 + e_tau - 2.0 * self.a[tau].re * scale).max(0.0);
            sum += diff;
            self.raw[tau] = diff;
            self.d[tau] = if sum > 0.0 {
                diff * tau as f32 / sum
            } else {
                1.0
            };
        }
        let lo = ((sr / max_hz.max(1.0)) as usize).clamp(2, w - 2);
        let hi = ((sr / min_hz.max(1.0)) as usize).clamp(lo + 1, w - 2);
        let mut tau = (lo..=hi).find(|&t| self.d[t] < YIN_THRESHOLD)?;
        while tau < hi && self.d[tau + 1] < self.d[tau] {
            tau += 1;
        }
        // The dip of the plain difference is the better-behaved parabola.
        let (y0, y1, y2) = (self.raw[tau - 1], self.raw[tau], self.raw[tau + 1]);
        let den = y0 - 2.0 * y1 + y2;
        let shift = if den.abs() > 1e-12 {
            (0.5 * (y0 - y2) / den).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        Some((
            sr / (tau as f32 + shift),
            (1.0 - self.d[tau]).clamp(0.0, 1.0),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn tone(f: f32, n: usize, harmonics: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                (1..=harmonics)
                    .map(|h| {
                        (std::f32::consts::TAU * f * h as f32 * i as f32 / SR).sin() / h as f32
                    })
                    .sum::<f32>()
                    * 0.3
            })
            .collect()
    }

    #[test]
    fn the_tuner_is_accurate_to_a_cent() {
        let mut det = PitchDetector::new(2048);
        // Pure sines and harmonic-rich tones from a low E to a high C, some between notes.
        for (f, harmonics) in [
            (41.2, 1),
            (82.41, 6),
            (110.0, 1),
            (196.0, 8),
            (261.63, 1),
            (440.0, 1),
            (445.0, 5),
            (659.26, 3),
            (987.77, 1),
            (2093.0, 1),
        ] {
            let x = tone(f, det.input_len(), harmonics);
            let (hz, clarity) = det.detect(&x, SR, 30.0, 4000.0).expect("pitch");
            let cents = 1200.0 * (hz / f).log2();
            assert!(
                cents.abs() < 1.0,
                "{f} Hz x{harmonics}: {hz} ({cents} cents)"
            );
            assert!(clarity > 0.9, "{f}: {clarity}");
        }
        assert!(det.detect(&vec![0.0; 4096], SR, 30.0, 4000.0).is_none());
        let mut noise = crate::Noise::new(7);
        let x: Vec<f32> = (0..4096).map(|_| noise.next_value()).collect();
        assert!(det.detect(&x, SR, 30.0, 4000.0).is_none());
    }

    #[test]
    fn notes_and_names() {
        assert_eq!(note_of(440.0), (69, 0.0));
        let (n, c) = note_of(445.0);
        assert_eq!(n, 69);
        assert!((c - 19.56).abs() < 0.01);
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(69), "A4");
        assert!((note_hz(60.0) - 261.626).abs() < 0.01);
    }

    #[test]
    fn spectrum_reads_a_full_scale_sine_at_0_db() {
        let mut s = Spectrum::new(4096);
        let bin = 100;
        let f = bin as f32 * SR / 4096.0;
        let x: Vec<f32> = (0..4096)
            .map(|i| (std::f32::consts::TAU * f * i as f32 / SR).sin())
            .collect();
        let mut out = Vec::new();
        s.analyze(&x, &mut out);
        assert_eq!(out.len(), 2048);
        assert!(out[bin].abs() < 0.01, "{}", out[bin]);
        assert!(out[bin + 10] < -80.0);
    }

    #[test]
    fn correlation_and_scope() {
        let a = tone(300.0, 4800, 1);
        let inv: Vec<f32> = a.iter().map(|v| -v).collect();
        let b = tone(437.0, 4800, 1);
        assert!((correlation(&a, &a) - 1.0).abs() < 1e-6);
        assert!((correlation(&a, &inv) + 1.0).abs() < 1e-6);
        assert!(correlation(&a, &b).abs() < 0.05);

        let s = Scope::new(SR);
        let ramp: Vec<f32> = (0..10_000).map(|i| i as f32).collect();
        for c in ramp.chunks(64) {
            s.push(c, c);
        }
        let (mut l, mut r) = (Vec::new(), Vec::new());
        s.read(100, &mut l, &mut r);
        assert_eq!(l, ramp[9900..]);
        assert_eq!(s.written(), 10_000);
    }
}
