//! Beat slicer (Prompt 23): where to cut a loop, on its transients or on a grid.

use crate::onsets;

/// Slice starts at every transient of `x` (mono at `sample_rate`), always including frame 0.
/// `sensitivity` as in [`onsets::detect`]. Slices shorter than `min_s` seconds merge into the
/// one before.
pub fn transient_slices(x: &[f32], sample_rate: u32, sensitivity: f32, min_s: f64) -> Vec<usize> {
    let min = (min_s.max(0.0) * f64::from(sample_rate)) as usize;
    let mut out = vec![0];
    for o in onsets::detect(x, sample_rate, sensitivity) {
        if o >= x.len() {
            continue;
        }
        let last = *out.last().unwrap_or(&0);
        if o >= last + min.max(1) {
            out.push(o);
        }
    }
    out
}

/// Slice starts every `1 / per_beat` of a beat for a loop `frames` long that lasts `beats`
/// beats.
pub fn grid_slices(frames: usize, beats: f64, per_beat: u32) -> Vec<usize> {
    let n = (beats * f64::from(per_beat.max(1))).round().max(1.0) as usize;
    (0..n)
        .map(|k| (k as f64 * frames as f64 / n as f64).round() as usize)
        .filter(|&s| s < frames)
        .collect()
}

/// The loop's length in beats: from its detected tempo, rounded to a whole number of beats
/// (a power of two when close), else 4 per 2 seconds at 120 BPM.
pub fn loop_beats(x: &[f32], sample_rate: u32) -> f64 {
    let seconds = x.len() as f64 / f64::from(sample_rate.max(1));
    let bpm = onsets::tempo(x, sample_rate).unwrap_or(120.0);
    let beats = (seconds * bpm / 60.0).round().max(1.0);
    let pow2 = 2f64.powf(beats.log2().round());
    if (beats - pow2).abs() / pow2 < 0.15 {
        pow2
    } else {
        beats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-second loop at 48 kHz with decaying noise bursts at `hits` (frames).
    fn loop_with(hits: &[usize]) -> Vec<f32> {
        let mut x = vec![0.0_f32; 96_000];
        let mut n = crate::Noise::new(9);
        for &h in hits {
            for (k, s) in x[h..].iter_mut().take(6000).enumerate() {
                *s += n.next_value() * 0.8 * (-(k as f32) / 1200.0).exp();
            }
        }
        x
    }

    #[test]
    fn transient_slices_land_on_the_hits() {
        let hits = [0, 12_000, 24_000, 33_000, 48_000, 60_000, 72_000, 84_000];
        let x = loop_with(&hits);
        let s = transient_slices(&x, 48_000, 0.5, 0.05);
        assert_eq!(s.len(), hits.len(), "{s:?}");
        for (got, want) in s.iter().zip(hits) {
            // Within 1 ms.
            assert!(got.abs_diff(want) <= 48, "{got} vs {want}");
        }
    }

    #[test]
    fn grid_slices_are_exact() {
        assert_eq!(
            grid_slices(96_000, 4.0, 2),
            (0..8).map(|k| k * 12_000).collect::<Vec<_>>()
        );
        assert_eq!(grid_slices(1000, 1.0, 3), [0, 333, 667]);
    }

    #[test]
    fn loop_length_in_beats() {
        // Eight hits in two seconds is a bar of eighths at 120 BPM: four beats.
        let hits: Vec<usize> = (0..8).map(|k| k * 12_000).collect();
        assert_eq!(loop_beats(&loop_with(&hits), 48_000), 4.0);
    }
}
