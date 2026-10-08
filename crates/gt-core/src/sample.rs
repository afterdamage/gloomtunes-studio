//! Decoded audio held in memory.

/// A decoded sample: one `Vec<f32>` per channel (planar), all the same length.
///
/// Built off the audio thread (by the loader or a generator) and shared with the engine as an
/// `Arc<SampleData>`; the engine only reads it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SampleData {
    /// Sample rate the data is stored at, in Hz.
    pub sample_rate: u32,
    /// Planar channel data. Empty for a silent, zero-length sample.
    pub channels: Vec<Vec<f32>>,
}

impl SampleData {
    /// A mono sample.
    pub fn mono(sample_rate: u32, data: Vec<f32>) -> Self {
        Self {
            sample_rate,
            channels: vec![data],
        }
    }

    /// Length in frames.
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// Length in seconds.
    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.sample_rate.max(1))
    }

    /// Left channel (the only channel of a mono sample).
    pub fn left(&self) -> &[f32] {
        self.channels.first().map_or(&[], Vec::as_slice)
    }

    /// Right channel; for a mono sample this is the same data as [`SampleData::left`].
    pub fn right(&self) -> &[f32] {
        self.channels
            .get(1)
            .or_else(|| self.channels.first())
            .map_or(&[], Vec::as_slice)
    }

    /// Min/max envelope for drawing a waveform overview with `bins` columns (left channel).
    pub fn overview(&self, bins: usize) -> Vec<(f32, f32)> {
        let data = self.left();
        if data.is_empty() || bins == 0 {
            return Vec::new();
        }
        (0..bins)
            .map(|b| {
                let lo = b * data.len() / bins;
                let hi = ((b + 1) * data.len() / bins).max(lo + 1).min(data.len());
                data[lo..hi]
                    .iter()
                    .fold((0.0_f32, 0.0_f32), |(mn, mx), &s| (mn.min(s), mx.max(s)))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_right_is_left() {
        let s = SampleData::mono(48_000, vec![0.5, -0.5]);
        assert_eq!(s.frames(), 2);
        assert_eq!(s.right(), s.left());
        assert_eq!(s.overview(2), vec![(0.0, 0.5), (-0.5, 0.0)]);
    }

    #[test]
    fn empty_is_safe() {
        let s = SampleData::default();
        assert_eq!(s.frames(), 0);
        assert!(s.left().is_empty() && s.right().is_empty());
        assert!(s.overview(8).is_empty());
    }
}
