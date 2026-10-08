//! Decibel conversions.
//!
//! Amplitude in dB is `20 * log10(gain)`, so -6.02 dB halves the amplitude and -12 dB is a gain
//! of about 0.2512. dBFS means "relative to digital full scale", where a sample of 1.0 is 0 dBFS.

/// Converts a level in dB to a linear amplitude factor.
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// Converts a linear amplitude factor to dB. Returns `f32::NEG_INFINITY` for zero.
#[inline]
pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.abs().log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minus_twelve_db() {
        assert!((db_to_gain(-12.0) - 0.251_188_64).abs() < 1e-6);
    }

    #[test]
    fn round_trip() {
        for db in [-60.0, -24.0, -12.0, -6.0, 0.0, 6.0] {
            assert!((gain_to_db(db_to_gain(db)) - db).abs() < 1e-4, "{db}");
        }
    }

    #[test]
    fn unity_and_silence() {
        assert_eq!(db_to_gain(0.0), 1.0);
        assert_eq!(gain_to_db(0.0), f32::NEG_INFINITY);
    }
}
