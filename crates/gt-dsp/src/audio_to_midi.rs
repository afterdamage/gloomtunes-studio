//! Audio to MIDI (Prompt 32, D146).
//!
//! Melody mode tracks a monophonic pitch with YIN ([`crate::analysis::PitchDetector`]) every
//! [`HOP`] samples. A frame counts when its pitch is clear and it is no more than
//! [`GATE_DB`] below the loudest frame; the frame's note is its pitch rounded to a semitone,
//! cleaned by a 5-frame median. A run of frames on one note becomes a note (runs shorter than
//! [`MIN_NOTE_S`] are dropped), with a velocity from its loudest frame.
//!
//! Drum mode places a hit on every onset ([`crate::onsets::detect`]) and picks a key from the
//! hit's spectral centroid: kick (36) below 250 Hz, snare (38) below 2.5 kHz, else hi-hat (42).

use crate::analysis::PitchDetector;
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

/// Samples between pitch frames.
pub const HOP: usize = 256;
/// YIN window: periods up to this long (about 47 Hz at 48 kHz).
const WINDOW: usize = 1024;
/// Lowest and highest pitch tracked.
const MIN_HZ: f32 = 50.0;
const MAX_HZ: f32 = 2000.0;
/// Clarity a frame needs to count.
const CLARITY: f32 = 0.6;
/// Frames this far below the loudest one are silence.
pub const GATE_DB: f32 = 40.0;
/// Shortest note kept.
pub const MIN_NOTE_S: f64 = 0.05;

/// A note found in audio, in seconds from its start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetectedNote {
    /// Start.
    pub start: f64,
    /// Length.
    pub length: f64,
    /// MIDI key.
    pub key: u8,
    /// 0 to 1.
    pub velocity: f32,
}

fn rms_db(x: &[f32]) -> f32 {
    let p = x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32;
    10.0 * p.max(1e-12).log10()
}

fn velocity(db: f32) -> f32 {
    ((db + 50.0) / 50.0).clamp(0.05, 1.0)
}

/// The notes of a monophonic melody in `x`.
pub fn melody(x: &[f32], sample_rate: u32) -> Vec<DetectedNote> {
    let sr = sample_rate.max(1) as f32;
    let mut det = PitchDetector::new(WINDOW);
    let len = det.input_len();
    if x.len() < len {
        return Vec::new();
    }
    let frames = (x.len() - len) / HOP + 1;
    let mut keys: Vec<Option<u8>> = Vec::with_capacity(frames);
    let mut dbs = Vec::with_capacity(frames);
    for t in 0..frames {
        let w = &x[t * HOP..t * HOP + len];
        dbs.push(rms_db(&w[WINDOW / 2..WINDOW / 2 + WINDOW]));
        keys.push(
            det.detect(w, sr, MIN_HZ, MAX_HZ)
                .filter(|&(_, clarity)| clarity >= CLARITY)
                .map(|(hz, _)| {
                    (69.0 + 12.0 * (hz / 440.0).log2())
                        .round()
                        .clamp(0.0, 127.0) as u8
                }),
        );
    }
    let loudest = dbs.iter().copied().fold(f32::MIN, f32::max);
    for (k, &db) in keys.iter_mut().zip(&dbs) {
        if db < loudest - GATE_DB {
            *k = None;
        }
    }
    // A 5-frame median of the keys (silence sorts lowest) removes single-frame octave slips.
    let median: Vec<Option<u8>> = (0..frames)
        .map(|t| {
            let mut v: Vec<Option<u8>> = keys[t.saturating_sub(2)..(t + 3).min(frames)].to_vec();
            v.sort();
            v[v.len() / 2]
        })
        .collect();
    // A frame's pitch belongs to the middle of its window.
    let at = |t: usize| (t * HOP + len / 2) as f64 / f64::from(sample_rate.max(1));
    let mut out = Vec::new();
    let mut t = 0;
    while t < frames {
        let Some(key) = median[t] else {
            t += 1;
            continue;
        };
        let mut e = t;
        while e + 1 < frames && median[e + 1] == Some(key) {
            e += 1;
        }
        let start = at(t);
        let length = at(e + 1) - start;
        if length >= MIN_NOTE_S {
            let db = dbs[t..=e].iter().copied().fold(f32::MIN, f32::max);
            out.push(DetectedNote {
                start,
                length,
                key,
                velocity: velocity(db),
            });
        }
        t = e + 1;
    }
    out
}

/// Drum hits in `x`: one note per onset, keyed by its brightness.
pub fn drums(x: &[f32], sample_rate: u32, sensitivity: f32) -> Vec<DetectedNote> {
    const N: usize = 2048;
    let sr = f64::from(sample_rate.max(1));
    let fft = FftPlanner::new().plan_fft_forward(N);
    let mut buf = vec![Complex::new(0.0_f32, 0.0); N];
    crate::onsets::detect(x, sample_rate, sensitivity)
        .into_iter()
        .map(|o| {
            let end = (o + N).min(x.len());
            let w = &x[o..end];
            for (i, b) in buf.iter_mut().enumerate() {
                let hann = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos();
                *b = Complex::new(w.get(i).copied().unwrap_or(0.0) * hann, 0.0);
            }
            fft.process(&mut buf);
            let (mut num, mut den) = (0.0_f64, 0.0_f64);
            for (k, c) in buf.iter().enumerate().take(N / 2).skip(1) {
                let m = f64::from(c.norm());
                num += m * k as f64 * sr / N as f64;
                den += m;
            }
            let centroid = if den > 0.0 { num / den } else { 0.0 };
            let key = if centroid < 250.0 {
                36
            } else if centroid < 2500.0 {
                38
            } else {
                42
            };
            let peak = w.iter().fold(0.0_f32, |m, v| m.max(v.abs()));
            DetectedNote {
                start: o as f64 / sr,
                length: 0.1,
                key,
                velocity: velocity(20.0 * peak.max(1e-6).log10() - 3.0),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const SR: u32 = 48_000;

    /// Notes as (key, start s, length s), rendered as sines with 5 ms ramps and a few
    /// harmonics, like a simple synth line.
    fn render(notes: &[(u8, f64, f64)], seconds: f64) -> Vec<f32> {
        let mut x = vec![0.0_f32; (seconds * f64::from(SR)) as usize];
        for &(key, start, len) in notes {
            let hz = 440.0 * 2f32.powf((f32::from(key) - 69.0) / 12.0);
            let a = (start * f64::from(SR)) as usize;
            let n = (len * f64::from(SR)) as usize;
            let ramp = 240;
            for i in 0..n {
                let env = (i.min(n - 1 - i) as f32 / ramp as f32).min(1.0);
                let ph = 2.0 * PI * hz * i as f32 / SR as f32;
                x[a + i] +=
                    0.4 * env * (ph.sin() + 0.3 * (2.0 * ph).sin() + 0.1 * (3.0 * ph).sin());
            }
        }
        x
    }

    #[test]
    fn a_synthesized_melody_comes_back_as_its_notes() {
        // A scale and a leap, with short gaps, from A2 to C6.
        let keys = [45, 52, 57, 60, 62, 64, 65, 67, 69, 72, 76, 84];
        let notes: Vec<(u8, f64, f64)> = keys
            .iter()
            .enumerate()
            .map(|(i, &k)| (k, 0.1 + i as f64 * 0.3, 0.25))
            .collect();
        let x = render(&notes, 0.2 + keys.len() as f64 * 0.3);
        let got = melody(&x, SR);
        assert_eq!(got.len(), notes.len(), "{got:?}");
        for (g, &(key, start, len)) in got.iter().zip(&notes) {
            assert_eq!(g.key, key, "{g:?}");
            assert!((g.start - start).abs() < 0.03, "{g:?} vs {start}");
            assert!((g.length - len).abs() < 0.05, "{g:?} vs {len}");
        }
    }

    #[test]
    fn quiet_notes_get_lower_velocities_and_silence_gives_nothing() {
        let mut x = render(&[(60, 0.1, 0.3)], 1.0);
        let quiet = render(&[(60, 0.5, 0.3)], 1.0);
        for (a, b) in x.iter_mut().zip(&quiet) {
            *a += 0.25 * b;
        }
        let got = melody(&x, SR);
        assert_eq!(got.len(), 2);
        assert!(got[0].velocity > got[1].velocity);
        assert!(melody(&vec![0.0; 48_000], SR).is_empty());
    }

    #[test]
    fn drum_hits_land_on_their_onsets_with_kick_snare_and_hat_keys() {
        let mut x = vec![0.0_f32; SR as usize * 2];
        let mut seed = 9_u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        };
        // Kick: a decaying 60 Hz sine. Snare: decaying noise through a two-pole low-pass. Hat:
        // decaying raw noise, differentiated (bright).
        let hits = [(0.2, 36), (0.7, 38), (1.2, 42), (1.6, 36)];
        for &(t, key) in &hits {
            let a = (t * f64::from(SR)) as usize;
            let (mut lp, mut lp2) = (0.0_f32, 0.0_f32);
            let mut last = 0.0_f32;
            for i in 0..9600 {
                let env = (-(i as f32) / 2400.0).exp();
                let v = match key {
                    36 => (2.0 * PI * 60.0 * i as f32 / SR as f32).sin(),
                    38 => {
                        lp += 0.08 * (rnd() - lp);
                        lp2 += 0.08 * (lp - lp2);
                        6.0 * lp2
                    }
                    _ => {
                        let n = rnd();
                        let d = n - last;
                        last = n;
                        0.5 * d
                    }
                };
                x[a + i] += 0.8 * env * v;
            }
        }
        let got = drums(&x, SR, 0.5);
        assert_eq!(got.len(), hits.len(), "{got:?}");
        for (g, &(t, key)) in got.iter().zip(&hits) {
            assert!((g.start - t).abs() < 0.01, "{g:?}");
            assert_eq!(g.key, key, "{g:?}");
        }
    }
}
