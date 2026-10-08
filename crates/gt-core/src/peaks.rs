//! Waveform peaks for drawing audio at any zoom.
//!
//! A pyramid of min/max pairs: level 0 holds one pair per [`PEAK_BASE`] frames, each level
//! above merges four pairs of the level below. Building costs one pass over the audio (done on
//! a worker thread); drawing a column of any width reads at most a few pairs from the coarsest
//! level that still resolves it.

use crate::sample::SampleData;

/// Frames per pair at level 0.
pub const PEAK_BASE: usize = 64;
/// Pairs merged per level step.
const FAN: usize = 4;

/// Min/max pyramid of a sample (both channels folded together).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Peaks {
    frames: usize,
    levels: Vec<Vec<(f32, f32)>>,
}

impl Peaks {
    /// Builds the pyramid. One pass over the data; allocates about 1/8 of the audio's size.
    pub fn build(data: &SampleData) -> Self {
        let frames = data.frames();
        let (l, r) = (data.left(), data.right());
        let mut base = Vec::with_capacity(frames.div_ceil(PEAK_BASE));
        for start in (0..frames).step_by(PEAK_BASE) {
            let end = (start + PEAK_BASE).min(frames);
            let mut mn = f32::INFINITY;
            let mut mx = f32::NEG_INFINITY;
            for (&a, &b) in l[start..end].iter().zip(&r[start..end]) {
                mn = mn.min(a).min(b);
                mx = mx.max(a).max(b);
            }
            base.push((mn, mx));
        }
        let mut levels = vec![base];
        while levels.last().is_some_and(|v| v.len() > 1) {
            let below = levels.last().expect("non-empty");
            let up = below
                .chunks(FAN)
                .map(|c| {
                    c.iter()
                        .fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), &(x, y)| {
                            (a.min(x), b.max(y))
                        })
                })
                .collect();
            levels.push(up);
        }
        Self { frames, levels }
    }

    /// Length of the analysed audio in frames.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Lowest and highest sample in frames `[lo, hi)`, or `None` outside the audio. Picks the
    /// coarsest level whose pairs are at most 1/16 of the range, so a query reads at most
    /// 66 pairs and overshoots the range by at most 1/8 of its width; ranges narrower than
    /// [`PEAK_BASE`] return the enclosing level-0 pair.
    pub fn range(&self, lo: f64, hi: f64) -> Option<(f32, f32)> {
        let lo = lo.max(0.0);
        let hi = hi.min(self.frames as f64);
        if hi <= lo || self.levels.is_empty() {
            return None;
        }
        let width = hi - lo;
        let mut level = 0;
        let mut bin = PEAK_BASE as f64;
        while level + 1 < self.levels.len() && bin * (FAN as f64) * 16.0 <= width {
            level += 1;
            bin *= FAN as f64;
        }
        let pairs = &self.levels[level];
        let a = (lo / bin).floor() as usize;
        let b = ((hi / bin).ceil() as usize).clamp(a + 1, pairs.len());
        let a = a.min(b - 1);
        Some(
            pairs[a..b]
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(p, q), &(x, y)| {
                    (p.min(x), q.max(y))
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_match_a_direct_scan() {
        let n = 100_003;
        let data: Vec<f32> = (0..n)
            .map(|i| ((i as f32) * 0.001).sin() * (i as f32 / n as f32))
            .collect();
        let s = SampleData::mono(48_000, data.clone());
        let p = Peaks::build(&s);
        assert_eq!(p.frames(), n);
        for (lo, hi) in [
            (0, n),
            (0, 64),
            (640, 6400),
            (50_000, 100_003),
            (12_800, 64_000),
        ] {
            let scan = |a: usize, b: usize| {
                data[a..b.min(n)]
                    .iter()
                    .fold((f32::INFINITY, f32::NEG_INFINITY), |(p, q), &x| {
                        (p.min(x), q.max(x))
                    })
            };
            let want = scan(lo, hi);
            let slack = ((hi - lo) / 8).max(PEAK_BASE);
            let outer = scan(lo.saturating_sub(slack), hi + slack);
            let got = p.range(lo as f64, hi as f64).unwrap();
            // The result covers the range and overshoots it by at most 1/8 of its width.
            assert!(
                got.0 <= want.0 && got.1 >= want.1,
                "{lo}..{hi}: {got:?} vs {want:?}"
            );
            assert!(
                got.0 >= outer.0 && got.1 <= outer.1,
                "{lo}..{hi}: {got:?} vs {outer:?}"
            );
        }
        assert!(p.range(n as f64, n as f64 + 10.0).is_none());
        assert!(p.range(-10.0, 0.0).is_none());
    }

    #[test]
    fn stereo_folds_both_sides_and_empty_is_safe() {
        let s = SampleData {
            sample_rate: 48_000,
            channels: vec![vec![0.5; 200], vec![-0.25; 200]],
        };
        assert_eq!(Peaks::build(&s).range(0.0, 200.0), Some((-0.25, 0.5)));
        let e = Peaks::build(&SampleData::default());
        assert_eq!(e.range(0.0, 10.0), None);
    }
}
