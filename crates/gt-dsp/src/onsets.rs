//! Transient (onset) detection and tempo estimation (Prompt 15 part 3, ARCHITECTURE.md D108).
//!
//! Onsets come from the spectral flux: the summed rise of the log-magnitude spectrum from one
//! frame to the next, which jumps when a new sound starts in any band. A peak counts as an
//! onset when it is the largest within ±[`PEAK_S`] and stands above an adaptive threshold
//! (the median flux around it plus a floor), so loud passages need a bigger jump than quiet
//! ones. Each onset is then placed to a few samples where the energy rises most.
//!
//! The tempo comes from the flux itself: its autocorrelation peaks at the beat period. Each
//! candidate period is scored together with its double and quadruple (bars repeat), weighted
//! toward 120 BPM so a loop of eighth notes is not read at twice its tempo, and a tempo that
//! makes the audio a whole number of beats long is preferred when it is within 0.3 %.

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

/// FFT size.
const N: usize = 1024;
/// Hop between frames.
const HOP: usize = 128;
/// Onsets closer than this to a larger one are merged into it (seconds).
const PEAK_S: f64 = 0.03;
/// Half-width of the threshold's median window (seconds).
const MEDIAN_S: f64 = 0.1;

/// Spectral flux of `x`, one value per [`HOP`] frames, frame `t` covering
/// `t·HOP - N..t·HOP` (silence before the start), normalised to a peak of 1.
pub fn flux(x: &[f32]) -> Vec<f32> {
    if x.is_empty() {
        return Vec::new();
    }
    let frames = x.len() / HOP + 1;
    let window: Vec<f32> = (0..N)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos())
        .collect();
    let fft = FftPlanner::new().plan_fft_forward(N);
    let mut buf = vec![Complex::new(0.0_f32, 0.0); N];
    let mut prev = vec![0.0_f32; N / 2];
    let mut out = Vec::with_capacity(frames);
    for t in 0..frames {
        let start = (t * HOP) as i64 - N as i64;
        for (i, (b, &w)) in buf.iter_mut().zip(&window).enumerate() {
            let k = start + i as i64;
            let v = if k >= 0 {
                x.get(k as usize).copied().unwrap_or(0.0)
            } else {
                0.0
            };
            *b = Complex::new(v * w, 0.0);
        }
        fft.process(&mut buf);
        let mut f = 0.0;
        for (p, b) in prev.iter_mut().zip(&buf[..N / 2]) {
            let m = (1.0 + 100.0 * b.norm()).ln();
            f += (m - *p).max(0.0);
            *p = m;
        }
        out.push(f);
    }
    let peak = out.iter().copied().fold(0.0_f32, f32::max);
    if peak > 0.0 {
        for v in &mut out {
            *v /= peak;
        }
    }
    out
}

/// Onsets in `x` (mono at `sample_rate`), as sample positions, in order. `sensitivity` from 0
/// (only the strongest hits) to 1 (every small change); 0.5 suits drum loops.
pub fn detect(x: &[f32], sample_rate: u32, sensitivity: f32) -> Vec<usize> {
    let f = flux(x);
    let fps = f64::from(sample_rate.max(1)) / HOP as f64;
    let peak_w = (PEAK_S * fps).round().max(1.0) as usize;
    let med_w = (MEDIAN_S * fps).round().max(1.0) as usize;
    let floor = 0.01 + 0.4 * (1.0 - sensitivity.clamp(0.0, 1.0)).powi(2);
    let mut scratch = Vec::with_capacity(2 * med_w + 1);
    let mut onsets = Vec::new();
    for t in 0..f.len() {
        let lo = t.saturating_sub(peak_w);
        let hi = (t + peak_w + 1).min(f.len());
        // The first of equal maxima wins.
        if f[lo..t].iter().any(|&v| v >= f[t]) || f[t + 1..hi].iter().any(|&v| v > f[t]) {
            continue;
        }
        scratch.clear();
        scratch.extend_from_slice(&f[t.saturating_sub(med_w)..(t + med_w + 1).min(f.len())]);
        scratch.sort_by(f32::total_cmp);
        let median = scratch[scratch.len() / 2];
        if f[t] > median + floor {
            onsets.push(refine(x, (t * HOP).saturating_sub(N)));
        }
    }
    onsets.dedup();
    onsets
}

/// The onset near the frame starting at `at`: first the 256-sample window whose energy rises
/// most over the one before it, then, around that, the sample where a 32-sample window's
/// energy jumps most.
fn refine(x: &[f32], at: usize) -> usize {
    // Energy of the first difference: a high-passed view, so the slow cycles of a bass
    // drum's tail do not pass for rises.
    let energy = |a: usize, b: usize| -> f32 {
        (a.max(1)..b.min(x.len()))
            .map(|i| (x[i] - x[i - 1]) * (x[i] - x[i - 1]))
            .sum()
    };
    let rise = |p: usize, w: usize| energy(p, p + w) - energy(p.saturating_sub(w), p);
    let best = |lo: usize, hi: usize, step: usize, w: usize| -> usize {
        (lo..hi.max(lo + 1))
            .step_by(step)
            .max_by(|&a, &b| rise(a, w).total_cmp(&rise(b, w)).then(b.cmp(&a)))
            .unwrap_or(lo)
    };
    let coarse = best(at, (at + N + HOP).min(x.len()), 8, 256);
    best(
        coarse.saturating_sub(256),
        (coarse + 128).min(x.len()),
        1,
        32,
    )
}

/// Tempo of `x` (mono at `sample_rate`) between 60 and 200 BPM, if it has a clear beat.
pub fn tempo(x: &[f32], sample_rate: u32) -> Option<f64> {
    let f = flux(x);
    let fps = f64::from(sample_rate.max(1)) / HOP as f64;
    if f.len() < (fps * 1.5) as usize {
        return None;
    }
    let mean = f.iter().sum::<f32>() / f.len() as f32;
    let d: Vec<f32> = f.iter().map(|v| v - mean).collect();
    let ac = |lag: usize| -> f64 {
        if lag >= d.len() {
            return 0.0;
        }
        let n = d.len() - lag;
        d[..n]
            .iter()
            .zip(&d[lag..])
            .map(|(a, b)| f64::from(a * b))
            .sum::<f64>()
            / n as f64
    };
    let lag_of = |bpm: f64| 60.0 / bpm * fps;
    let (min_lag, max_lag) = (lag_of(200.0).floor() as usize, lag_of(60.0).ceil() as usize);
    let score = |lag: usize| -> f64 {
        let bpm = 60.0 * fps / lag as f64;
        let prior = (-0.5 * ((bpm / 120.0).log2() / 0.7).powi(2)).exp();
        // The beat repeats at its multiples and splits into eighths at half of it.
        prior * (ac(lag) + 0.5 * ac(2 * lag) + 0.25 * ac(4 * lag) + 0.5 * ac(lag / 2))
    };
    let lag = (min_lag..=max_lag).max_by(|&a, &b| score(a).total_cmp(&score(b)))?;
    if ac(lag) <= 0.0 {
        return None;
    }
    // The period precisely: the autocorrelation peak near the largest multiple of it (up to
    // four) that fits in half the audio, refined with a parabola, divided back.
    let m = [4, 2, 1]
        .into_iter()
        .find(|m| m * lag + 4 < d.len() / 2)
        .unwrap_or(1);
    let around = m * lag;
    let peak = (around.saturating_sub(m).max(1)..=around + m)
        .max_by(|&a, &b| ac(a).total_cmp(&ac(b)))
        .unwrap_or(around);
    let (a, b, c) = (ac(peak - 1), ac(peak), ac(peak + 1));
    let den = a - 2.0 * b + c;
    let shift = if den < 0.0 {
        (0.5 * (a - c) / den).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    let bpm = 60.0 * fps * m as f64 / (peak as f64 + shift);
    // A loop is usually a whole number of beats: snap to that when it is close.
    let seconds = x.len() as f64 / f64::from(sample_rate.max(1));
    let beats = (seconds * bpm / 60.0).round().max(1.0);
    let whole = beats * 60.0 / seconds;
    Some(if (whole / bpm - 1.0).abs() < 0.003 {
        whole
    } else {
        bpm
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drums;

    const SR: u32 = 48_000;

    /// A drum loop: `bars` bars of 4/4 at `bpm` with kicks on 1 and 3, snares on 2 and 4 and
    /// hats on every eighth (quieter). Returns the audio and every hit's position.
    fn drum_loop(bpm: f64, bars: usize, hats: bool) -> (Vec<f32>, Vec<usize>) {
        let sr = SR as f32;
        let (kick, snare, hat) = (drums::kick(sr), drums::snare(sr), drums::hat(sr));
        let eighth = 30.0 / bpm * f64::from(SR);
        let len = (bars as f64 * 8.0 * eighth).round() as usize;
        let mut x = vec![0.0_f32; len];
        let mut hits = Vec::new();
        for e in 0..bars * 8 {
            let at = (e as f64 * eighth).round() as usize;
            let (sound, gain): (&[f32], f32) = match e % 8 {
                0 | 4 => (&kick, 0.9),
                2 | 6 => (&snare, 0.7),
                _ if hats => (&hat, 0.3),
                _ => continue,
            };
            hits.push(at);
            for (i, v) in sound.iter().enumerate() {
                if let Some(o) = x.get_mut(at + i) {
                    *o += v * gain;
                }
            }
        }
        (x, hits)
    }

    #[test]
    fn finds_every_hit_of_a_drum_loop_and_nothing_else() {
        for (bpm, hats) in [(95.0, true), (128.0, true), (140.0, false), (174.0, true)] {
            let (x, hits) = drum_loop(bpm, 2, hats);
            let found = detect(&x, SR, 0.5);
            assert_eq!(found.len(), hits.len(), "{bpm} BPM: {found:?} vs {hits:?}");
            for (f, h) in found.iter().zip(&hits) {
                let off = *f as i64 - *h as i64;
                // Within 2 ms.
                assert!(off.abs() <= 96, "{bpm} BPM: hit at {h} found at {f}");
            }
        }
    }

    #[test]
    fn a_quieter_setting_keeps_only_the_strong_hits() {
        let (x, _) = drum_loop(120.0, 2, true);
        let all = detect(&x, SR, 0.5).len();
        let strong = detect(&x, SR, 0.0).len();
        assert!(strong < all, "{strong} vs {all}");
        assert!(
            detect(&vec![0.0; SR as usize], SR, 1.0).is_empty(),
            "silence"
        );
    }

    #[test]
    fn reads_the_tempo_of_drum_loops() {
        for bpm in [95.0, 100.0, 120.0, 128.0, 140.0, 150.0] {
            let (x, _) = drum_loop(bpm, 4, true);
            let got = tempo(&x, SR).unwrap();
            assert!((got - bpm).abs() < 0.05, "{bpm} BPM read as {got}");
        }
        // Far from 120 the beat level is ambiguous (eighths at 86 BPM are quarters at 172):
        // the tempo is right up to an octave, and the clip menu has ×2 and ÷2 to fix it.
        for bpm in [80.0, 86.0, 174.0] {
            let (x, _) = drum_loop(bpm, 4, true);
            let got = tempo(&x, SR).unwrap();
            let folded = got * 2f64.powf(-(got / bpm).log2().round());
            assert!((folded - bpm).abs() < 0.05, "{bpm} BPM read as {got}");
        }
    }

    #[test]
    fn reads_the_tempo_without_a_whole_number_of_beats() {
        let (x, _) = drum_loop(110.0, 4, true);
        // Cut 0.3 beats off the end: the length no longer helps.
        let cut = x.len() - (0.3 * 60.0 / 110.0 * f64::from(SR)) as usize;
        let got = tempo(&x[..cut], SR).unwrap();
        assert!((got - 110.0).abs() < 1.0, "read as {got}");
    }
}
