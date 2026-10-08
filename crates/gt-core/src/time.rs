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

    /// Builds a map from any list of points: sorts them, clamps tempos into range, keeps the
    /// last of points that share a tick, drops points before tick 0 and adds 120 BPM at tick 0
    /// if nothing is there. Never fails.
    pub fn from_points_lossy(points: &[TempoPoint]) -> Self {
        let mut v: Vec<TempoPoint> = points
            .iter()
            .filter(|p| p.at.0 >= 0)
            .map(|p| TempoPoint {
                at: p.at,
                bpm: if p.bpm.is_finite() {
                    p.bpm.clamp(MIN_BPM, MAX_BPM)
                } else {
                    120.0
                },
            })
            .collect();
        v.sort_by_key(|p| p.at);
        let mut out: Vec<TempoPoint> = Vec::with_capacity(v.len() + 1);
        for p in v {
            match out.last_mut() {
                Some(last) if last.at == p.at => *last = p,
                _ => out.push(p),
            }
        }
        if out.first().is_none_or(|p| p.at != Tick::ZERO) {
            out.insert(
                0,
                TempoPoint {
                    at: Tick::ZERO,
                    bpm: 120.0,
                },
            );
        }
        Self::new(&out).unwrap_or_default()
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

/// A time-signature change: from bar `bar` (counted from 0) the signature is `sig`.
///
/// Changes sit on bar lines, so they are stored by bar rather than tick: when an earlier
/// signature changes, later changes stay on the same bar number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SigChange {
    /// Bar index from 0 (bar 1 on screen).
    pub bar: i64,
    /// Signature from that bar on.
    pub sig: TimeSig,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SigSegment {
    bar: i64,
    tick: i64,
    sig: TimeSig,
}

/// Time signatures over the whole timeline: bar lines, beats and bar:beat:tick positions.
///
/// Always starts at bar 0; before tick 0 the first signature continues backwards. Lookups are
/// binary searches over a few segments and never allocate, so the audio thread can use them.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeSigMap {
    segments: Vec<SigSegment>,
}

impl Default for TimeSigMap {
    fn default() -> Self {
        Self::constant(TimeSig::default())
    }
}

impl TimeSigMap {
    /// One signature for the whole song.
    pub fn constant(sig: TimeSig) -> Self {
        Self {
            segments: vec![SigSegment {
                bar: 0,
                tick: 0,
                sig,
            }],
        }
    }

    /// Builds a map from changes in any order. Changes before bar 0 are dropped, a later change
    /// on the same bar wins, a change that repeats the signature before it is merged, and bar 0
    /// defaults to 4/4 if no change is there.
    pub fn new(changes: &[SigChange]) -> Self {
        let mut sorted: Vec<SigChange> = changes.iter().copied().filter(|c| c.bar >= 0).collect();
        sorted.sort_by_key(|c| c.bar);
        let mut map = Self::constant(TimeSig::default());
        for c in sorted {
            let sig = TimeSig::new(c.sig.num, c.sig.den);
            let last = map.segments.last_mut().expect("never empty");
            if last.bar == c.bar {
                last.sig = sig;
                continue;
            }
            if last.sig == sig {
                continue;
            }
            let tick = last.tick + (c.bar - last.bar) * last.sig.bar_ticks();
            map.segments.push(SigSegment {
                bar: c.bar,
                tick,
                sig,
            });
        }
        // Merging may leave neighbours equal when bar 0 was overwritten.
        map.segments.dedup_by(|b, a| a.sig == b.sig);
        map
    }

    /// The changes this map holds, bar 0 first.
    pub fn changes(&self) -> impl Iterator<Item = SigChange> + '_ {
        self.segments.iter().map(|s| SigChange {
            bar: s.bar,
            sig: s.sig,
        })
    }

    #[inline]
    fn segment_for_tick(&self, t: i64) -> &SigSegment {
        let i = self
            .segments
            .partition_point(|s| s.tick <= t)
            .saturating_sub(1);
        &self.segments[i]
    }

    #[inline]
    fn segment_for_bar(&self, bar: i64) -> &SigSegment {
        let i = self
            .segments
            .partition_point(|s| s.bar <= bar)
            .saturating_sub(1);
        &self.segments[i]
    }

    /// Signature in effect at `t`.
    pub fn sig_at(&self, t: Tick) -> TimeSig {
        self.segment_for_tick(t.0).sig
    }

    /// Signature of bar `bar` (from 0).
    pub fn sig_of_bar(&self, bar: i64) -> TimeSig {
        self.segment_for_bar(bar).sig
    }

    /// Tick where bar `bar` (from 0; negative before the song) starts.
    pub fn bar_start(&self, bar: i64) -> i64 {
        let s = self.segment_for_bar(bar);
        s.tick + (bar - s.bar) * s.sig.bar_ticks()
    }

    /// Bar (from 0) containing tick `t`.
    pub fn bar_of(&self, t: i64) -> i64 {
        let s = self.segment_for_tick(t);
        s.bar + (t - s.tick).div_euclid(s.sig.bar_ticks())
    }

    /// Splits `t` into bar, beat and tick, following signature changes.
    pub fn bbt(&self, t: Tick) -> BarBeatTick {
        let s = self.segment_for_tick(t.0);
        let rel = t.0 - s.tick;
        let bar = rel.div_euclid(s.sig.bar_ticks());
        let in_bar = rel.rem_euclid(s.sig.bar_ticks());
        let beat = s.sig.beat_ticks();
        BarBeatTick {
            bar: s.bar + bar + 1,
            beat: in_bar / beat + 1,
            tick: in_bar % beat,
        }
    }

    /// Calls `f(tick, downbeat)` for every beat in `lo..=hi`, in order. No allocation.
    pub fn for_each_beat(&self, lo: i64, hi: i64, mut f: impl FnMut(i64, bool)) {
        if hi < lo {
            return;
        }
        let mut i = self
            .segments
            .partition_point(|s| s.tick <= lo)
            .saturating_sub(1);
        let mut t = lo;
        while t <= hi {
            let s = self.segments[i];
            let end = self.segments.get(i + 1).map_or(i64::MAX, |n| n.tick);
            let beat = s.sig.beat_ticks();
            let bar = s.sig.bar_ticks();
            // First beat at or after `t` in this segment.
            let mut b = s.tick + (t - s.tick).div_euclid(beat) * beat;
            if b < t {
                b += beat;
            }
            while b <= hi && b < end {
                f(b, (b - s.tick).rem_euclid(bar) == 0);
                b += beat;
            }
            if end > hi {
                break;
            }
            t = end;
            i += 1;
        }
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

    #[test]
    fn sig_map_places_bars_and_beats() {
        // Two bars of 4/4, then 7/8 from bar 2 (0-based), then 3/4 from bar 4.
        let m = TimeSigMap::new(&[
            SigChange {
                bar: 4,
                sig: TimeSig::new(3, 4),
            },
            SigChange {
                bar: 2,
                sig: TimeSig::new(7, 8),
            },
        ]);
        assert_eq!(m.changes().count(), 3);
        assert_eq!(m.bar_start(2), 2 * 3840);
        assert_eq!(m.bar_start(3), 2 * 3840 + 3360);
        assert_eq!(m.bar_start(4), 2 * 3840 + 2 * 3360);
        assert_eq!(m.bar_start(5), 2 * 3840 + 2 * 3360 + 2880);
        assert_eq!(m.bar_start(-1), -3840);
        for bar in -2..8 {
            assert_eq!(m.bar_of(m.bar_start(bar)), bar);
            assert_eq!(m.bar_of(m.bar_start(bar + 1) - 1), bar);
        }
        assert_eq!(m.sig_at(Tick(2 * 3840)), TimeSig::new(7, 8));
        assert_eq!(m.bbt(Tick(2 * 3840 + 480 * 6)).to_string(), "3:7:000");
        let mut beats = Vec::new();
        m.for_each_beat(2 * 3840 - 960, 2 * 3840 + 960, |t, down| {
            beats.push((t, down))
        });
        assert_eq!(
            beats,
            vec![(6720, false), (7680, true), (8160, false), (8640, false)]
        );
        // Every bar line of the 3/4 section is a downbeat.
        let start = m.bar_start(4);
        let mut downs = Vec::new();
        m.for_each_beat(start, start + 3 * 2880 - 1, |t, d| {
            if d {
                downs.push(t);
            }
        });
        assert_eq!(downs, vec![start, start + 2880, start + 5760]);
    }

    #[test]
    fn sig_map_normalises_changes() {
        let four = TimeSig::default();
        let m = TimeSigMap::new(&[
            SigChange {
                bar: -3,
                sig: TimeSig::new(5, 4),
            },
            SigChange {
                bar: 0,
                sig: TimeSig::new(3, 4),
            },
            SigChange {
                bar: 2,
                sig: TimeSig::new(3, 4),
            },
            SigChange { bar: 6, sig: four },
        ]);
        let got: Vec<_> = m.changes().collect();
        assert_eq!(
            got,
            vec![
                SigChange {
                    bar: 0,
                    sig: TimeSig::new(3, 4)
                },
                SigChange { bar: 6, sig: four }
            ]
        );
        assert_eq!(TimeSigMap::new(&[]), TimeSigMap::constant(four));
    }

    #[test]
    fn lossy_tempo_maps_are_always_valid() {
        let p = |at, bpm| TempoPoint { at: Tick(at), bpm };
        let m =
            TempoMap::from_points_lossy(&[p(960, 90.0), p(-5, 60.0), p(960, 100.0), p(0, 5000.0)]);
        let pts: Vec<_> = m.points().collect();
        assert_eq!(pts, vec![p(0, MAX_BPM), p(960, 100.0)]);
        assert_eq!(
            TempoMap::from_points_lossy(&[p(480, 90.0)]).bpm_at(Tick(0)),
            120.0
        );
    }
}
