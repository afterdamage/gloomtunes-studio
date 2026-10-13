//! Pitch shifter and pitch correction (phase vocoder).

use std::f32::consts::{PI, TAU};
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use super::{clamp_param, DelayLine, Effect, FxContext, Smooth};
use crate::analysis::PitchDetector;

mod p {
    pub const MODE: usize = 0;
    pub const SEMITONES: usize = 1;
    pub const CENTS: usize = 2;
    pub const FORMANT: usize = 3;
    pub const ROOT: usize = 4;
    pub const SCALE: usize = 5;
    pub const SPEED: usize = 6;
    pub const MIX: usize = 7;
    pub const COUNT: usize = 8;
}

/// Frame size of the vocoder.
const N: usize = 2048;
/// Frames advance by a quarter of their size.
const OVERLAP: usize = 4;
const HOP: usize = N / OVERLAP;
/// Half-width, in bins, of the moving average that estimates the spectral envelope (about
/// 230 Hz at 48 kHz: wider than the harmonic spacing of a voice, narrower than its formants).
const ENVELOPE_BINS: usize = 10;

/// Scales for pitch correction, indexed by the `scale` parameter: chromatic, then the piano
/// roll's scales in its menu order (`gt_ui::views::ScaleKind`).
pub const SCALE_INTERVALS: [&[u8]; 10] = [
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
    &[0, 2, 4, 5, 7, 9, 11],
    &[0, 2, 3, 5, 7, 8, 10],
    &[0, 2, 3, 5, 7, 8, 11],
    &[0, 2, 3, 5, 7, 9, 10],
    &[0, 1, 3, 5, 7, 8, 10],
    &[0, 2, 4, 5, 7, 9, 10],
    &[0, 2, 4, 7, 9],
    &[0, 3, 5, 7, 10],
    &[0, 3, 5, 6, 7, 10],
];

/// Nearest note to MIDI pitch `m` (fractional) in `scale` on `root` (0 = C).
fn snap(m: f32, root: usize, scale: &[u8]) -> f32 {
    let base = m.round() as i32;
    (base - 6..=base + 6)
        .filter(|n| scale.contains(&((n - root as i32).rem_euclid(12) as u8)))
        .map(|n| n as f32)
        .min_by(|a, b| (a - m).abs().total_cmp(&(b - m).abs()))
        .unwrap_or(m)
}

/// One channel's vocoder state.
#[derive(Debug, Clone)]
struct Channel {
    input: Vec<f32>,
    output: Vec<f32>,
    acc: Vec<f32>,
    last_phase: Vec<f32>,
    sum_phase: Vec<f32>,
}

impl Channel {
    fn new() -> Self {
        Self {
            input: vec![0.0; N],
            output: vec![0.0; HOP],
            acc: vec![0.0; 2 * N],
            last_phase: vec![0.0; N / 2 + 1],
            sum_phase: vec![0.0; N / 2 + 1],
        }
    }

    fn reset(&mut self) {
        for v in [
            &mut self.input,
            &mut self.output,
            &mut self.acc,
            &mut self.last_phase,
            &mut self.sum_phase,
        ] {
            v.fill(0.0);
        }
    }
}

/// Pitch shifter: a phase vocoder (frames of 2048 with Hann windows, overlap 4). Each frame's
/// bins are measured for their true frequency from the phase advance since the last frame,
/// moved to the bin at `ratio` times their index and resynthesised with phases advanced at the
/// shifted frequency, so the length stays the same while the pitch moves (the classic method
/// as described by Bernsee). With the formant option the spectral envelope (a moving average
/// of the magnitudes) stays where it was and only the harmonics move under it, so a voice
/// keeps its vowel colour instead of sounding smaller or larger.
///
/// In correction mode a YIN detector reads each frame; the shift needed to land on the nearest
/// note of the chosen scale glides in with the speed time (0 snaps at once, the robotic sound).
/// Transposition applies on top. Latency is one frame (2048 frames).
pub struct PitchShifter {
    sr: f32,
    fwd: Arc<dyn Fft<f32>>,
    inv: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex<f32>>,
    buf: Vec<Complex<f32>>,
    window: Vec<f32>,
    ana_mag: Vec<f32>,
    ana_freq: Vec<f32>,
    syn_mag: Vec<f32>,
    syn_freq: Vec<f32>,
    env: Vec<f32>,
    prefix: Vec<f32>,
    mono: Vec<f32>,
    ch: [Channel; 2],
    fill: usize,
    detector: PitchDetector,
    correct: bool,
    semitones: f32,
    cents: f32,
    formant: bool,
    root: usize,
    scale: usize,
    speed_ms: f32,
    correction: f32,
    detected: f32,
    mix: Smooth,
    dry: [DelayLine; 2],
}

impl std::fmt::Debug for PitchShifter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PitchShifter({} st)", self.semitones)
    }
}

impl PitchShifter {
    /// No shift, correction off.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let mut planner = FftPlanner::new();
        let fwd = planner.plan_fft_forward(N);
        let inv = planner.plan_fft_inverse(N);
        let scratch = fwd
            .get_inplace_scratch_len()
            .max(inv.get_inplace_scratch_len());
        let latency = N;
        Self {
            sr,
            fwd,
            inv,
            scratch: vec![Complex::default(); scratch],
            buf: vec![Complex::default(); N],
            window: (0..N)
                .map(|i| 0.5 - 0.5 * (TAU * i as f32 / N as f32).cos())
                .collect(),
            ana_mag: vec![0.0; N / 2 + 1],
            ana_freq: vec![0.0; N / 2 + 1],
            syn_mag: vec![0.0; N / 2 + 1],
            syn_freq: vec![0.0; N / 2 + 1],
            env: vec![0.0; N / 2 + 1],
            prefix: vec![0.0; N / 2 + 2],
            mono: vec![0.0; N],
            ch: [Channel::new(), Channel::new()],
            fill: 0,
            detector: PitchDetector::new(N / 2),
            correct: false,
            semitones: 0.0,
            cents: 0.0,
            formant: false,
            root: 0,
            scale: 0,
            speed_ms: 20.0,
            correction: 0.0,
            detected: 0.0,
            mix: Smooth::new(1.0, 20.0, sr),
            dry: [DelayLine::new(latency + 4), DelayLine::new(latency + 4)],
        }
    }

    /// The pitch correction last heard, in Hz (0 when none).
    pub fn detected_hz(&self) -> f32 {
        self.detected
    }

    /// Updates the correction from the current input frame (in correction mode).
    fn track(&mut self) {
        if !self.correct {
            self.correction = 0.0;
            return;
        }
        for (m, (a, b)) in self
            .mono
            .iter_mut()
            .zip(self.ch[0].input.iter().zip(&self.ch[1].input))
        {
            *m = 0.5 * (a + b);
        }
        let target = match self.detector.detect(&self.mono, self.sr, 50.0, 1500.0) {
            Some((hz, _)) => {
                self.detected = hz;
                let m = 69.0 + 12.0 * (hz / 440.0).log2();
                snap(m, self.root, SCALE_INTERVALS[self.scale]) - m
            }
            None => {
                self.detected = 0.0;
                0.0
            }
        };
        let c = if self.speed_ms <= 0.0 {
            1.0
        } else {
            1.0 - (-(HOP as f32) / (self.speed_ms * 0.001 * self.sr)).exp()
        };
        self.correction += (target - self.correction) * c;
    }

    fn frame(&mut self, ch: usize, ratio: f32) {
        let expect = TAU * HOP as f32 / N as f32;
        let c = &mut self.ch[ch];
        for ((b, &x), &w) in self.buf.iter_mut().zip(&c.input).zip(&self.window) {
            *b = Complex::new(x * w, 0.0);
        }
        self.fwd
            .process_with_scratch(&mut self.buf, &mut self.scratch);
        // Analysis: each bin's magnitude and true frequency (in bins).
        for k in 0..=N / 2 {
            let (mag, phase) = self.buf[k].to_polar();
            let mut d = phase - c.last_phase[k] - k as f32 * expect;
            c.last_phase[k] = phase;
            d -= TAU * (d / TAU).round();
            self.ana_mag[k] = mag;
            self.ana_freq[k] = k as f32 + d * OVERLAP as f32 / TAU;
        }
        if self.formant {
            self.prefix[0] = 0.0;
            for k in 0..=N / 2 {
                self.prefix[k + 1] = self.prefix[k] + self.ana_mag[k];
            }
            for k in 0..=N / 2 {
                let (lo, hi) = (
                    k.saturating_sub(ENVELOPE_BINS),
                    (k + ENVELOPE_BINS).min(N / 2),
                );
                self.env[k] = (self.prefix[hi + 1] - self.prefix[lo]) / (hi + 1 - lo) as f32 + 1e-9;
            }
        }
        // Move every bin to `ratio` times its index.
        self.syn_mag.fill(0.0);
        self.syn_freq.fill(0.0);
        for k in 0..=N / 2 {
            let to = (k as f32 * ratio).round() as usize;
            if to > N / 2 {
                break;
            }
            let gain = if self.formant {
                self.env[to] / self.env[k]
            } else {
                1.0
            };
            self.syn_mag[to] += self.ana_mag[k] * gain;
            self.syn_freq[to] = self.ana_freq[k] * ratio;
        }
        // Synthesis: advance each bin's phase at its new frequency.
        for k in 0..=N / 2 {
            let d = (self.syn_freq[k] - k as f32) * TAU / OVERLAP as f32 + k as f32 * expect;
            c.sum_phase[k] = (c.sum_phase[k] + d) % (64.0 * PI);
            self.buf[k] = Complex::from_polar(self.syn_mag[k], c.sum_phase[k]);
        }
        for k in 1..N / 2 {
            self.buf[N - k] = self.buf[k].conj();
        }
        self.inv
            .process_with_scratch(&mut self.buf, &mut self.scratch);
        // Hann analysis and synthesis at overlap 4 sum to 1.5.
        let scale = 1.0 / (N as f32 * 1.5);
        for ((a, b), &w) in c.acc.iter_mut().zip(&self.buf).zip(&self.window) {
            *a += b.re * w * scale;
        }
        c.output.copy_from_slice(&c.acc[..HOP]);
        c.acc.copy_within(HOP.., 0);
        let len = c.acc.len();
        c.acc[len - HOP..].fill(0.0);
        c.input.copy_within(HOP.., 0);
    }
}

impl Effect for PitchShifter {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            p::MODE => self.correct = clamp_param(v, 0.0, 1.0, 0.0) >= 0.5,
            p::SEMITONES => self.semitones = clamp_param(v, -24.0, 24.0, 0.0).round(),
            p::CENTS => self.cents = clamp_param(v, -100.0, 100.0, 0.0),
            p::FORMANT => self.formant = clamp_param(v, 0.0, 1.0, 0.0) >= 0.5,
            p::ROOT => self.root = clamp_param(v, 0.0, 11.0, 0.0).round() as usize,
            p::SCALE => {
                let last = (SCALE_INTERVALS.len() - 1) as f32;
                self.scale = clamp_param(v, 0.0, last, 0.0).round() as usize;
            }
            p::SPEED => self.speed_ms = clamp_param(v, 0.0, 500.0, 20.0),
            p::MIX => self.mix.set(clamp_param(v, 0.0, 1.0, 1.0)),
            _ => {}
        }
    }

    fn reset(&mut self) {
        for c in &mut self.ch {
            c.reset();
        }
        for d in &mut self.dry {
            d.clear();
        }
        self.fill = 0;
        self.correction = 0.0;
        self.detected = 0.0;
        self.mix.snap();
    }

    fn latency(&self) -> usize {
        N
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &FxContext) {
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let at = N - HOP + self.fill;
            self.ch[0].input[at] = *x;
            self.ch[1].input[at] = *y;
            self.dry[0].push(*x);
            self.dry[1].push(*y);
            let (wl, wr) = (self.ch[0].output[self.fill], self.ch[1].output[self.fill]);
            self.fill += 1;
            if self.fill == HOP {
                self.fill = 0;
                self.track();
                let ratio = ((self.semitones + self.cents / 100.0 + self.correction) / 12.0)
                    .exp2()
                    .clamp(0.25, 4.0);
                self.frame(0, ratio);
                self.frame(1, ratio);
            }
            let mix = self.mix.next_value();
            let (dl, dr) = (self.dry[0].read_int(N), self.dry[1].read_int(N));
            *x = dl * (1.0 - mix) + wl * mix;
            *y = dr * (1.0 - mix) + wr * mix;
        }
    }

    fn meter(&self) -> f32 {
        self.detected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{run, sine, tone};

    const SR: f32 = 48_000.0;

    fn shifted(x: &[f32], setup: &[(usize, f32)]) -> Vec<f32> {
        let mut s = PitchShifter::new(SR);
        for &(i, v) in setup {
            s.set_param(i, v);
        }
        s.reset();
        let (mut l, mut r) = (x.to_vec(), x.to_vec());
        run(&mut s, &mut l, &mut r, 120.0);
        l
    }

    fn pitch_of(y: &[f32]) -> f32 {
        let mut d = PitchDetector::new(2048);
        d.detect(&y[y.len() - 4096..], SR, 40.0, 4000.0).unwrap().0
    }

    fn cents(a: f32, b: f32) -> f32 {
        1200.0 * (a / b).log2()
    }

    #[test]
    fn shifts_land_within_a_few_cents() {
        let x = sine(440.0, 0.5, SR, 48_000);
        for (st, ct) in [
            (7.0, 0.0),
            (-12.0, 0.0),
            (12.0, 0.0),
            (3.0, 50.0),
            (-5.0, 0.0),
        ] {
            let y = shifted(&x, &[(p::SEMITONES, st), (p::CENTS, ct)]);
            let want = 440.0 * ((st + ct / 100.0) / 12.0).exp2();
            let got = pitch_of(&y);
            assert!(
                cents(got, want).abs() < 3.0,
                "{st} st {ct} c: {got} vs {want}"
            );
        }
        // No shift: the output is the input, a frame late, at the same level.
        let y = shifted(&x, &[]);
        let lat = N;
        let err = (lat + N..48_000)
            .map(|i| (y[i] - x[i - lat]).abs())
            .fold(0.0, f32::max);
        assert!(err < 0.01, "{err}");
    }

    #[test]
    fn correction_snaps_to_the_scale() {
        // 270 Hz is a little sharp of C4 (261.6) and a little flat of C#4 (277.2).
        let x = sine(270.0, 0.5, SR, 48_000);
        let chromatic = shifted(&x, &[(p::MODE, 1.0), (p::SPEED, 0.0)]);
        assert!(cents(pitch_of(&chromatic), 277.18).abs() < 5.0);
        let c_major = shifted(&x, &[(p::MODE, 1.0), (p::SPEED, 0.0), (p::SCALE, 1.0)]);
        assert!(cents(pitch_of(&c_major), 261.63).abs() < 5.0);
        // In D major (no C natural, C# is the 7th) it goes up again.
        let d_major = shifted(
            &x,
            &[
                (p::MODE, 1.0),
                (p::SPEED, 0.0),
                (p::SCALE, 1.0),
                (p::ROOT, 2.0),
            ],
        );
        assert!(cents(pitch_of(&d_major), 277.18).abs() < 5.0);
        assert_eq!(snap(60.4, 0, SCALE_INTERVALS[1]), 60.0);
        assert_eq!(snap(60.9, 0, SCALE_INTERVALS[1]), 60.0);
        assert_eq!(snap(61.1, 0, SCALE_INTERVALS[1]), 62.0);
    }

    #[test]
    fn formant_mode_keeps_the_spectral_peak_in_place() {
        // A 200 Hz tone whose 5th harmonic (1 kHz) is loudest: a crude vowel.
        let x: Vec<f32> = (0..48_000)
            .map(|i| {
                (1..=12)
                    .map(|h| {
                        let a = 1.0 / (1.0 + ((h as f32 - 5.0) / 1.5).powi(2));
                        a * (TAU * 200.0 * h as f32 * i as f32 / SR).sin()
                    })
                    .sum::<f32>()
                    * 0.1
            })
            .collect();
        let peak = |y: &[f32]| {
            let tail = &y[24_000..];
            (1..=24)
                .map(|h| h as f32 * 200.0)
                .max_by(|&a, &b| tone(tail, a, SR).total_cmp(&tone(tail, b, SR)))
                .unwrap()
        };
        assert_eq!(peak(&x), 1000.0);
        // An octave up, harmonics at 400 Hz steps: plain shifting moves the peak to 2 kHz,
        // the formant option keeps it near 1 kHz.
        assert_eq!(peak(&shifted(&x, &[(p::SEMITONES, 12.0)])), 2000.0);
        let kept = peak(&shifted(&x, &[(p::SEMITONES, 12.0), (p::FORMANT, 1.0)]));
        assert!((800.0..=1200.0).contains(&kept), "{kept}");
    }
}
