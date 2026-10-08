//! Musical time.
//!
//! Positions are integer [`Tick`]s at 960 per quarter note. Converting ticks to seconds needs the
//! [`TempoMap`]: a list of constant-tempo segments. Within a segment, time is linear in ticks:
//! `seconds = segment_start_seconds + (tick - segment_start_tick) * 60 / (bpm * PPQ)`.
//! Segment start times are precomputed exactly once, so a conversion never accumulates rounding
//! error, however far into the song it is.

use core::fmt;
use core::ops::{Add, Sub};

/// Ticks per quarter note. 960 divides evenly by 2, 3, 4, 5, 6, 8, 10, 12, 15, 16, 20, 24, 32 and
/// 64, so straight notes down to 1/256 and triplets down to 1/128 are exact integers.
pub const PPQ: i64 = 960;

/// Slowest and fastest tempo accepted, in BPM.
pub const MIN_BPM: f64 = 10.0;
/// See [`MIN_BPM`].
pub const MAX_BPM: f64 = 999.0;

/// A musical position or duration, in ticks (960 per quarter note).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Tick(pub i64);

impl Tick {
    /// Position zero: the start of bar 1.
    pub const ZERO: Tick = Tick(0);
}

impl Add for Tick {
    type Output = Tick;
    fn add(self, rhs: Tick) -> Tick {
        Tick(self.0 + rhs.0)
    }
}

impl Sub for Tick {
    type Output = Tick;
    fn sub(self, rhs: Tick) -> Tick {
        Tick(self.0 - rhs.0)
    }
}

/// A time signature such as 4/4 or 7/8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimeSig {
    /// Beats per bar.
    pub num: u8,
    /// Note value of one beat: 2, 4, 8 or 16.
    pub den: u8,
}

impl Default for TimeSig {
    fn default() -> Self {
        Self { num: 4, den: 4 }
    }
}

impl TimeSig {
    /// Creates a signature, clamping to a sane range (1..=32 beats; denominator 2, 4, 8 or 16).
    pub fn new(num: u8, den: u8) -> Self {
        let den = match den {
            0..=2 => 2,
            3..=4 => 4,
            5..=8 => 8,
            _ => 16,
        };
        Self {
            num: num.clamp(1, 32),
            den,
        }
    }

    /// Length of one beat in ticks (a quarter note is `PPQ`, an eighth is `PPQ / 2`).
    pub fn beat_ticks(self) -> i64 {
        PPQ * 4 / i64::from(self.den)
    }

    /// Length of one bar in ticks.
    pub fn bar_ticks(self) -> i64 {
        self.beat_ticks() * i64::from(self.num)
    }
}

impl fmt::Display for TimeSig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

/// A position split into bars, beats and ticks, for display. Bars and beats count from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarBeatTick {
    /// Bar number, from 1.
    pub bar: i64,
    /// Beat within the bar, from 1.
    pub beat: i64,
    /// Tick within the beat, from 0.
    pub tick: i64,
}

impl BarBeatTick {
    /// Splits `t` using a single time signature from tick 0. Negative positions count backwards
    /// from bar 1 (bar 0, -1, ...), as a count-in would.
    pub fn from_tick(t: Tick, sig: TimeSig) -> Self {
        let bar_len = sig.bar_ticks();
        let beat_len = sig.beat_ticks();
        let bar = t.0.div_euclid(bar_len);
        let in_bar = t.0.rem_euclid(bar_len);
        Self {
            bar: bar + 1,
            beat: in_bar / beat_len + 1,
            tick: in_bar % beat_len,
        }
    }
}

impl fmt::Display for BarBeatTick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{:03}", self.bar, self.beat, self.tick)
    }
}

/// A tempo change: from tick `at` onwards the tempo is `bpm` quarter notes per minute.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoPoint {
    /// Where the tempo takes effect.
    pub at: Tick,
    /// Quarter notes per minute.
    pub bpm: f64,
}

/// Why a tempo map could not be built.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TempoMapError {
    /// No points were given.
    Empty,
    /// The first point is not at tick 0.
    FirstPointNotAtZero,
    /// Two points share a tick, or points are out of order.
    NotStrictlyIncreasing,
    /// A tempo outside `MIN_BPM..=MAX_BPM`, or not finite.
    TempoOutOfRange(f64),
}

impl fmt::Display for TempoMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "tempo map has no points"),
            Self::FirstPointNotAtZero => write!(f, "first tempo point must be at tick 0"),
            Self::NotStrictlyIncreasing => write!(f, "tempo points must be strictly increasing"),
            Self::TempoOutOfRange(b) => {
                write!(f, "tempo {b} BPM is outside {MIN_BPM}..={MAX_BPM}")
            }
        }
    }
}

impl std::error::Error for TempoMapError {}

/// Segment with its precomputed start time.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    start_tick: i64,
    start_seconds: f64,
    /// Seconds per tick: `60 / (bpm * PPQ)`.
    seconds_per_tick: f64,
    bpm: f64,
}

/// Step-wise tempo over the whole timeline. Always has a point at tick 0.
///
/// Before tick 0 (count-in) the first tempo continues backwards.
#[derive(Debug, Clone, PartialEq)]
pub struct TempoMap {
    segments: Vec<Segment>,
}

impl Default for TempoMap {
    fn default() -> Self {
        Self::constant(120.0)
    }
}

impl TempoMap {
    /// A single tempo for the whole song. The tempo is clamped into `MIN_BPM..=MAX_BPM`.
    pub fn constant(bpm: f64) -> Self {
        let bpm = if bpm.is_finite() {
            bpm.clamp(MIN_BPM, MAX_BPM)
        } else {
            120.0
        };
        Self {
            segments: vec![Segment {
                start_tick: 0,
                start_seconds: 0.0,
                seconds_per_tick: 60.0 / (bpm * PPQ as f64),
                bpm,
            }],
        }
    }

    /// Builds a map from tempo points. The first must be at tick 0; ticks strictly increase.
    pub fn new(points: &[TempoPoint]) -> Result<Self, TempoMapError> {
        let first = points.first().ok_or(TempoMapError::Empty)?;
        if first.at != Tick::ZERO {
            return Err(TempoMapError::FirstPointNotAtZero);
        }
        let mut segments: Vec<Segment> = Vec::with_capacity(points.len());
        for p in points {
            if !p.bpm.is_finite() || !(MIN_BPM..=MAX_BPM).contains(&p.bpm) {
                return Err(TempoMapError::TempoOutOfRange(p.bpm));
            }
            let start_seconds = match segments.last() {
                None => 0.0,
                Some(prev) => {
                    if p.at.0 <= prev.start_tick {
                        return Err(TempoMapError::NotStrictlyIncreasing);
                    }
                    prev.start_seconds + (p.at.0 - prev.start_tick) as f64 * prev.seconds_per_tick
                }
            };
            segments.push(Segment {
                start_tick: p.at.0,
                start_seconds,
                seconds_per_tick: 60.0 / (p.bpm * PPQ as f64),
                bpm: p.bpm,
            });
        }
        Ok(Self { segments })
    }

    /// The tempo points this map was built from.
    pub fn points(&self) -> impl Iterator<Item = TempoPoint> + '_ {
        self.segments.iter().map(|s| TempoPoint {
            at: Tick(s.start_tick),
            bpm: s.bpm,
        })
    }

    /// Tempo in effect at `t`.
    pub fn bpm_at(&self, t: Tick) -> f64 {
        self.segment_for_tick(t.0 as f64).bpm
    }

    /// Seconds from tick 0 to the (possibly fractional) tick `t`. Negative before tick 0.
    #[inline]
    pub fn tick_to_seconds(&self, t: f64) -> f64 {
        let s = self.segment_for_tick(t);
        s.start_seconds + (t - s.start_tick as f64) * s.seconds_per_tick
    }

    /// The (fractional) tick reached `seconds` after tick 0. Inverse of [`Self::tick_to_seconds`].
    #[inline]
    pub fn seconds_to_tick(&self, seconds: f64) -> f64 {
        let i = self
            .segments
            .partition_point(|s| s.start_seconds <= seconds)
            .saturating_sub(1);
        let s = &self.segments[i];
        s.start_tick as f64 + (seconds - s.start_seconds) / s.seconds_per_tick
    }

    #[inline]
    fn segment_for_tick(&self, t: f64) -> &Segment {
        // Binary search: no allocation, O(log n), fine on the audio thread.
        let i = self
            .segments
            .partition_point(|s| (s.start_tick as f64) <= t)
            .saturating_sub(1);
        &self.segments[i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_divisions_are_exact() {
        for div in [4, 6, 12, 64] {
            assert_eq!(PPQ % div, 0, "1/{div} of a quarter");
        }
    }

    #[test]
    fn constant_tempo_conversion() {
        let m = TempoMap::constant(120.0);
        // 120 BPM: one quarter note = 0.5 s.
        assert_eq!(m.tick_to_seconds(960.0), 0.5);
        assert_eq!(m.tick_to_seconds(3840.0), 2.0);
        assert_eq!(m.seconds_to_tick(2.0), 3840.0);
    }

    #[test]
    fn piecewise_tempo_conversion() {
        // 120 BPM for 2 bars (4 s), then 60 BPM.
        let m = TempoMap::new(&[
            TempoPoint {
                at: Tick(0),
                bpm: 120.0,
            },
            TempoPoint {
                at: Tick(7680),
                bpm: 60.0,
            },
        ])
        .unwrap();
        assert_eq!(m.tick_to_seconds(7680.0), 4.0);
        assert_eq!(m.tick_to_seconds(7680.0 + 960.0), 5.0);
        assert_eq!(m.seconds_to_tick(5.0), 8640.0);
        assert_eq!(m.bpm_at(Tick(7679)), 120.0);
        assert_eq!(m.bpm_at(Tick(7680)), 60.0);
    }

    #[test]
    fn round_trip_over_long_spans() {
        let m = TempoMap::new(&[
            TempoPoint {
                at: Tick(0),
                bpm: 133.0,
            },
            TempoPoint {
                at: Tick(12_345),
                bpm: 87.5,
            },
            TempoPoint {
                at: Tick(99_999),
                bpm: 174.0,
            },
        ])
        .unwrap();
        for t in [-500.0, 0.0, 1.0, 12_344.5, 12_345.0, 50_000.0, 10_000_000.0] {
            let back = m.seconds_to_tick(m.tick_to_seconds(t));
            assert!((back - t).abs() < 1e-6, "{t} -> {back}");
        }
    }

    #[test]
    fn rejects_bad_maps() {
        assert_eq!(TempoMap::new(&[]), Err(TempoMapError::Empty));
        let p = |at, bpm| TempoPoint { at: Tick(at), bpm };
        assert_eq!(
            TempoMap::new(&[p(10, 120.0)]),
            Err(TempoMapError::FirstPointNotAtZero)
        );
        assert_eq!(
            TempoMap::new(&[p(0, 120.0), p(0, 90.0)]),
            Err(TempoMapError::NotStrictlyIncreasing)
        );
        assert!(matches!(
            TempoMap::new(&[p(0, 5000.0)]),
            Err(TempoMapError::TempoOutOfRange(_))
        ));
        assert_eq!(TempoMap::constant(f64::NAN).bpm_at(Tick(0)), 120.0);
    }

    #[test]
    fn bar_beat_tick_display() {
        let four = TimeSig::default();
        assert_eq!(BarBeatTick::from_tick(Tick(0), four).to_string(), "1:1:000");
        assert_eq!(
            BarBeatTick::from_tick(Tick(960 * 5 + 7), four).to_string(),
            "2:2:007"
        );
        let seven_eight = TimeSig::new(7, 8);
        assert_eq!(seven_eight.beat_ticks(), 480);
        assert_eq!(seven_eight.bar_ticks(), 3360);
        assert_eq!(
            BarBeatTick::from_tick(Tick(3360 + 480 * 6), seven_eight).to_string(),
            "2:7:000"
        );
        assert_eq!(
            BarBeatTick::from_tick(Tick(-960), four).to_string(),
            "0:4:000"
        );
    }
}
