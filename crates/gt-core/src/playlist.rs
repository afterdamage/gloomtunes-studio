//! The playlist: tracks, clips and markers (ARCHITECTURE.md §6).
//!
//! A track is a lane for organisation, mute and solo; any clip can sit on any track. A clip
//! shows a window `[offset, offset + length)` of its source starting at `start` on the timeline:
//!
//! - a pattern clip repeats its pattern for as long as the clip is;
//! - an audio clip plays a sample file once, at its own speed, into the track's mixer strip;
//! - an automation clip moves one parameter (any [`ParamId`]) along its points.
//!
//! Slip edit changes `offset` only, so the content moves inside a clip that stays put. All
//! positions are ticks.

use crate::mixer::{MixerStrip, MASTER};
use crate::params::ParamId;
use crate::project::{PatternId, SampleSource};

/// Stable identity of a playlist track. Never reused within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackId(pub u32);

/// Stable identity of a clip. Never reused within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClipId(pub u32);

const MAX_VOLUME: f32 = MixerStrip::MAX_VOLUME;

/// Colours new tracks cycle through (muted tones that read on the dark theme).
pub const TRACK_COLORS: [[u8; 3]; 8] = [
    [138, 92, 196],
    [92, 132, 196],
    [72, 160, 150],
    [120, 168, 84],
    [196, 160, 72],
    [204, 116, 72],
    [196, 84, 112],
    [150, 150, 164],
];

/// Number of tracks a new playlist starts with.
pub const DEFAULT_TRACKS: usize = 8;

/// One lane of the playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// Identity.
    pub id: TrackId,
    /// Display name.
    pub name: String,
    /// Lane and clip colour (sRGB).
    pub color: [u8; 3],
    /// Silences every clip on the track.
    pub mute: bool,
    /// When any track is soloed, only soloed tracks play.
    pub solo: bool,
    /// Mixer strip that the track's audio clips play into ([`MASTER`] or an insert).
    pub insert: usize,
}

/// Shape of an automation segment, from one point to the next.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Curve {
    /// Stays at the point's value until the next point, then jumps.
    Hold,
    /// Straight line.
    #[default]
    Linear,
    /// S-curve (half a cosine): eases out of one point and into the next.
    Smooth,
    /// Single bend set by a tension from -1 to 1: positive rises fast then levels off,
    /// negative starts slowly; 0 is a straight line. A quadratic Bézier through both points.
    Bezier(f32),
}

impl Curve {
    /// Menu names, in the order of [`Curve::from_index`].
    pub const NAMES: [&'static str; 4] = ["Hold", "Linear", "Smooth", "Bézier"];

    /// Index into [`Curve::NAMES`].
    pub fn index(self) -> usize {
        match self {
            Self::Hold => 0,
            Self::Linear => 1,
            Self::Smooth => 2,
            Self::Bezier(_) => 3,
        }
    }

    /// The curve for a menu index (a new Bézier gets tension 0.5).
    pub fn from_index(i: usize) -> Self {
        match i {
            0 => Self::Hold,
            2 => Self::Smooth,
            3 => Self::Bezier(0.5),
            _ => Self::Linear,
        }
    }

    /// Fraction of the way from the first value to the second at `x` (0..1) of the segment.
    pub fn shape(self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Self::Hold => {
                if x < 1.0 {
                    0.0
                } else {
                    1.0
                }
            }
            Self::Linear => x,
            Self::Smooth => 0.5 - 0.5 * (std::f32::consts::PI * x).cos(),
            Self::Bezier(tension) => {
                // Control point (cx, cy) on the anti-diagonal: (0.5, 0.5) is a straight line.
                let t = if tension.is_finite() {
                    tension.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                let (cx, cy) = (0.5 * (1.0 - t), 0.5 * (1.0 + t));
                // Solve x(s) = (1 - 2cx)s² + 2cx·s for s, then evaluate y(s).
                let a = 1.0 - 2.0 * cx;
                let b = 2.0 * cx;
                let s = if a.abs() < 1e-6 {
                    x / b
                } else {
                    (-b + (b * b + 4.0 * a * x).max(0.0).sqrt()) / (2.0 * a)
                };
                let s = s.clamp(0.0, 1.0);
                2.0 * s * (1.0 - s) * cy + s * s
            }
        }
    }

    /// True if a piece cut out of the middle of a segment keeps this shape (so cutting at an
    /// arbitrary point needs no resampling).
    pub fn cuts_cleanly(self) -> bool {
        matches!(self, Self::Hold | Self::Linear) || matches!(self, Self::Bezier(t) if t == 0.0)
    }
}

/// A breakpoint of an automation clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoPoint {
    /// Position in ticks from the start of the clip's source (like a note in a pattern).
    pub at: i64,
    /// Normalized value, 0 to 1.
    pub value: f32,
    /// Shape of the segment from this point to the next.
    pub curve: Curve,
}

impl AutoPoint {
    /// A point with a linear segment after it.
    pub fn new(at: i64, value: f32) -> Self {
        Self {
            at,
            value,
            curve: Curve::Linear,
        }
    }
}

/// The source of an automation clip: a target and a line through points.
#[derive(Debug, Clone, PartialEq)]
pub struct Automation {
    /// What moves.
    pub target: ParamId,
    /// Breakpoints sorted by `at`; values between points are interpolated linearly, and the
    /// first and last values hold before and after.
    pub points: Vec<AutoPoint>,
}

impl Automation {
    /// Value at source position `at` (ticks from the source start).
    pub fn value_at(&self, at: f64) -> f32 {
        value_at(&self.points, at)
    }

    /// Inserts a point, keeping the list sorted; returns its index. A point that splits a
    /// segment takes that segment's curve, so the shape on either side keeps its kind.
    pub fn insert_point(&mut self, p: AutoPoint) -> usize {
        let i = self.points.partition_point(|q| q.at <= p.at);
        let curve = match i.checked_sub(1).and_then(|k| self.points.get(k)) {
            Some(prev) if i < self.points.len() => prev.curve,
            _ => p.curve,
        };
        self.points.insert(
            i,
            AutoPoint {
                at: p.at,
                value: p.value.clamp(0.0, 1.0),
                curve,
            },
        );
        i
    }
}

/// Value through sorted points, each segment shaped by its first point's curve; holds the end
/// values outside them.
pub fn value_at(points: &[AutoPoint], at: f64) -> f32 {
    let Some(first) = points.first() else {
        return 0.0;
    };
    let i = points.partition_point(|p| (p.at as f64) <= at);
    if i == 0 {
        return first.value;
    }
    let a = points[i - 1];
    let Some(b) = points.get(i) else {
        return a.value;
    };
    let span = (b.at - a.at) as f64;
    if span <= 0.0 {
        return b.value;
    }
    let f = ((at - a.at as f64) / span) as f32;
    a.value + (b.value - a.value) * a.curve.shape(f)
}

/// What a clip plays.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipKind {
    /// A pattern, repeated for the clip's length.
    Pattern(PatternId),
    /// A sample file, played once.
    Audio {
        /// The sound.
        source: SampleSource,
        /// Linear gain.
        gain: f32,
    },
    /// Automation of one parameter.
    Automation(Automation),
}

/// A clip on the playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    /// Identity.
    pub id: ClipId,
    /// Lane.
    pub track: TrackId,
    /// Timeline position of the clip's left edge.
    pub start: i64,
    /// Length in ticks (at least 1).
    pub length: i64,
    /// Source position at the left edge (slip edit), at least 0.
    pub offset: i64,
    /// A muted clip is drawn but not played.
    pub muted: bool,
    /// Source.
    pub kind: ClipKind,
}

impl Clip {
    /// Timeline position of the right edge.
    pub fn end(&self) -> i64 {
        self.start + self.length
    }
}

/// A named position on the timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    /// Position.
    pub at: i64,
    /// Label.
    pub name: String,
}

/// Tracks, clips and markers.
#[derive(Debug, Clone, PartialEq)]
pub struct Playlist {
    /// Lanes, top to bottom.
    pub tracks: Vec<Track>,
    /// Clips in no particular order.
    pub clips: Vec<Clip>,
    /// Markers sorted by position.
    pub markers: Vec<Marker>,
    next_id: u32,
}

impl Default for Playlist {
    fn default() -> Self {
        Self::new()
    }
}

impl Playlist {
    /// A playlist with [`DEFAULT_TRACKS`] empty tracks.
    pub fn new() -> Self {
        let mut p = Self {
            tracks: Vec::new(),
            clips: Vec::new(),
            markers: Vec::new(),
            next_id: 1,
        };
        for _ in 0..DEFAULT_TRACKS {
            p.add_track();
        }
        p
    }

    fn next_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Adds a track "Track N" at the bottom, with the next palette colour.
    pub fn add_track(&mut self) -> TrackId {
        let id = TrackId(self.next_id());
        let n = self.tracks.len();
        let name = (1..)
            .map(|k| format!("Track {k}"))
            .find(|s| self.tracks.iter().all(|t| &t.name != s))
            .unwrap_or_default();
        self.tracks.push(Track {
            id,
            name,
            color: TRACK_COLORS[n % TRACK_COLORS.len()],
            mute: false,
            solo: false,
            insert: MASTER,
        });
        id
    }

    /// Removes a track and its clips.
    pub fn remove_track(&mut self, id: TrackId) {
        self.tracks.retain(|t| t.id != id);
        self.clips.retain(|c| c.track != id);
    }

    /// Index of a track, top to bottom.
    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// The track with this id.
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// True if clips on `id` play: the track exists, is not muted, and no other track is
    /// soloed (unless it is soloed too).
    pub fn is_track_audible(&self, id: TrackId) -> bool {
        let any_solo = self.tracks.iter().any(|t| t.solo);
        self.track(id)
            .is_some_and(|t| !t.mute && (!any_solo || t.solo))
    }

    /// Adds a clip and returns its id. Length is at least 1 tick, offset at least 0.
    pub fn add_clip(&mut self, track: TrackId, start: i64, length: i64, kind: ClipKind) -> ClipId {
        let id = ClipId(self.next_id());
        self.clips.push(Clip {
            id,
            track,
            start,
            length: length.max(1),
            offset: 0,
            muted: false,
            kind,
        });
        id
    }

    /// The clip with this id.
    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    /// The clip with this id, mutably.
    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    /// Removes clips.
    pub fn remove_clips(&mut self, ids: &[ClipId]) {
        self.clips.retain(|c| !ids.contains(&c.id));
    }

    /// Cuts a clip in two at timeline position `at`. The right part continues the content
    /// exactly (its offset moves on by the left part's length). Returns the new right part, or
    /// `None` if `at` is not strictly inside the clip.
    pub fn split_clip(&mut self, id: ClipId, at: i64) -> Option<ClipId> {
        let c = self.clip(id)?.clone();
        if at <= c.start || at >= c.end() {
            return None;
        }
        let left = at - c.start;
        let new_id = ClipId(self.next_id());
        let right = Clip {
            id: new_id,
            start: at,
            length: c.length - left,
            offset: c.offset + left,
            ..c
        };
        if let Some(l) = self.clip_mut(id) {
            l.length = left;
        }
        self.clips.push(right);
        Some(new_id)
    }

    /// Copies clips so the copies start where the selection ends (the selection's span is
    /// rounded up to `grid` ticks when `grid > 0`). Returns the copies' ids.
    pub fn duplicate_clips(&mut self, ids: &[ClipId], grid: i64) -> Vec<ClipId> {
        let chosen: Vec<Clip> = self
            .clips
            .iter()
            .filter(|c| ids.contains(&c.id))
            .cloned()
            .collect();
        let (Some(lo), Some(hi)) = (
            chosen.iter().map(|c| c.start).min(),
            chosen.iter().map(Clip::end).max(),
        ) else {
            return Vec::new();
        };
        let mut span = hi - lo;
        if grid > 0 {
            span = (span + grid - 1).div_euclid(grid) * grid;
        }
        chosen
            .into_iter()
            .map(|c| {
                let id = ClipId(self.next_id());
                self.clips.push(Clip {
                    id,
                    start: c.start + span,
                    ..c
                });
                id
            })
            .collect()
    }

    /// Timeline position where the last clip ends (0 for an empty playlist).
    pub fn song_end(&self) -> i64 {
        self.clips.iter().map(Clip::end).max().unwrap_or(0).max(0)
    }

    /// Adds a marker, keeping the list sorted; returns its index.
    pub fn add_marker(&mut self, at: i64, name: &str) -> usize {
        let i = self.markers.partition_point(|m| m.at <= at);
        self.markers.insert(
            i,
            Marker {
                at,
                name: name.to_owned(),
            },
        );
        i
    }

    /// Re-sorts markers after their positions were edited.
    pub fn sort_markers(&mut self) {
        self.markers.sort_by_key(|m| m.at);
    }

    /// Clips whose pattern no longer exists are dropped, clip lengths and offsets are put in
    /// range, automation points are sorted and clamped, and track routes fixed.
    pub fn sanitize(&mut self, pattern_exists: impl Fn(PatternId) -> bool) {
        let tracks: Vec<TrackId> = self.tracks.iter().map(|t| t.id).collect();
        self.clips.retain(|c| {
            tracks.contains(&c.track)
                && match &c.kind {
                    ClipKind::Pattern(p) => pattern_exists(*p),
                    _ => true,
                }
        });
        for c in &mut self.clips {
            c.length = c.length.max(1);
            c.offset = c.offset.max(0);
            if let ClipKind::Automation(a) = &mut c.kind {
                a.points.sort_by_key(|p| p.at);
                for p in &mut a.points {
                    p.value = if p.value.is_finite() {
                        p.value.clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    if let Curve::Bezier(t) = &mut p.curve {
                        *t = if t.is_finite() {
                            t.clamp(-1.0, 1.0)
                        } else {
                            0.0
                        };
                    }
                }
            }
            if let ClipKind::Audio { gain, .. } = &mut c.kind {
                *gain = if gain.is_finite() {
                    gain.clamp(0.0, MAX_VOLUME)
                } else {
                    1.0
                };
            }
        }
        for t in &mut self.tracks {
            if t.insert >= crate::mixer::FIRST_SEND {
                t.insert = MASTER;
            }
        }
        self.sort_markers();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::PatternId;

    fn pat() -> ClipKind {
        ClipKind::Pattern(PatternId(7))
    }

    #[test]
    fn new_playlist_has_named_coloured_tracks() {
        let p = Playlist::new();
        assert_eq!(p.tracks.len(), DEFAULT_TRACKS);
        assert_eq!(p.tracks[0].name, "Track 1");
        assert_ne!(p.tracks[0].color, p.tracks[1].color);
        let ids: std::collections::HashSet<_> = p.tracks.iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), DEFAULT_TRACKS);
    }

    #[test]
    fn split_keeps_content_continuous() {
        let mut p = Playlist::new();
        let t = p.tracks[0].id;
        let a = p.add_clip(t, 960, 3840, pat());
        p.clip_mut(a).unwrap().offset = 100;
        assert!(p.split_clip(a, 960).is_none(), "edge is not inside");
        assert!(p.split_clip(a, 960 + 3840).is_none());
        let b = p.split_clip(a, 2000).unwrap();
        let (l, r) = (p.clip(a).unwrap(), p.clip(b).unwrap());
        assert_eq!((l.start, l.length, l.offset), (960, 1040, 100));
        assert_eq!((r.start, r.length, r.offset), (2000, 2800, 1140));
        // The source position at any timeline tick is unchanged by the split.
        assert_eq!(r.offset - r.start, l.offset - l.start);
    }

    #[test]
    fn duplicate_places_copies_after_the_selection() {
        let mut p = Playlist::new();
        let t = p.tracks[0].id;
        let a = p.add_clip(t, 0, 3840, pat());
        let b = p.add_clip(t, 3840, 1000, pat());
        let copies = p.duplicate_clips(&[a, b], 3840);
        assert_eq!(copies.len(), 2);
        let starts: Vec<_> = copies.iter().map(|&c| p.clip(c).unwrap().start).collect();
        assert_eq!(starts, vec![7680, 7680 + 3840]);
        assert_eq!(p.song_end(), 7680 + 3840 + 1000);
        assert!(p.duplicate_clips(&[], 0).is_empty());
    }

    #[test]
    fn mute_and_solo_decide_audible_tracks() {
        let mut p = Playlist::new();
        let (a, b) = (p.tracks[0].id, p.tracks[1].id);
        assert!(p.is_track_audible(a));
        p.tracks[1].solo = true;
        assert!(!p.is_track_audible(a));
        assert!(p.is_track_audible(b));
        p.tracks[1].mute = true;
        assert!(!p.is_track_audible(b));
        assert!(!p.is_track_audible(TrackId(999)));
    }

    #[test]
    fn removing_a_track_removes_its_clips() {
        let mut p = Playlist::new();
        let t = p.tracks[2].id;
        p.add_clip(t, 0, 10, pat());
        p.add_clip(p.tracks[0].id, 0, 10, pat());
        p.remove_track(t);
        assert_eq!(p.tracks.len(), DEFAULT_TRACKS - 1);
        assert_eq!(p.clips.len(), 1);
        p.add_track();
        assert_eq!(
            p.tracks.last().unwrap().name,
            "Track 3",
            "first free number"
        );
    }

    #[test]
    fn automation_interpolates_and_holds() {
        let pts = [
            AutoPoint::new(0, 0.0),
            AutoPoint::new(100, 1.0),
            AutoPoint::new(200, 0.5),
        ];
        assert_eq!(value_at(&pts, -5.0), 0.0);
        assert_eq!(value_at(&pts, 50.0), 0.5);
        assert_eq!(value_at(&pts, 150.0), 0.75);
        assert_eq!(value_at(&pts, 900.0), 0.5);
        assert_eq!(value_at(&[], 3.0), 0.0);
        let mut a = Automation {
            target: crate::params::MASTER_VOLUME,
            points: pts.to_vec(),
        };
        a.points[1].curve = Curve::Hold;
        assert_eq!(a.insert_point(AutoPoint::new(150, 2.0)), 2);
        assert_eq!(a.points[2].value, 1.0, "clamped");
        assert_eq!(
            a.points[2].curve,
            Curve::Hold,
            "takes the split segment's curve"
        );
        assert_eq!(a.insert_point(AutoPoint::new(300, 0.0)), 4);
        assert_eq!(
            a.points[4].curve,
            Curve::Linear,
            "past the end: its own curve"
        );
    }

    #[test]
    fn curves_join_their_points_and_bend_the_right_way() {
        for c in [
            Curve::Linear,
            Curve::Smooth,
            Curve::Bezier(0.8),
            Curve::Bezier(-0.8),
            Curve::Bezier(0.0),
            Curve::Bezier(1.0),
            Curve::Bezier(-1.0),
        ] {
            assert!(c.shape(0.0).abs() < 1e-6, "{c:?}");
            assert!((c.shape(1.0) - 1.0).abs() < 1e-6, "{c:?}");
            // Monotonic.
            let mut last = 0.0;
            for k in 1..=100 {
                let y = c.shape(k as f32 / 100.0);
                assert!(y >= last - 1e-6, "{c:?} at {k}");
                last = y;
            }
        }
        assert_eq!(Curve::Hold.shape(0.99), 0.0);
        assert!((Curve::Smooth.shape(0.5) - 0.5).abs() < 1e-6);
        assert!(Curve::Smooth.shape(0.1) < 0.1, "eases out");
        assert!(
            (Curve::Bezier(0.0).shape(0.3) - 0.3).abs() < 1e-5,
            "0 is straight"
        );
        assert!(Curve::Bezier(0.8).shape(0.3) > 0.5, "positive rises fast");
        assert!(
            Curve::Bezier(-0.8).shape(0.3) < 0.15,
            "negative starts slowly"
        );
        let pts = [
            AutoPoint {
                at: 0,
                value: 0.2,
                curve: Curve::Hold,
            },
            AutoPoint::new(100, 0.8),
        ];
        assert_eq!(value_at(&pts, 99.0), 0.2);
        assert_eq!(value_at(&pts, 100.0), 0.8);
        for i in 0..4 {
            assert_eq!(Curve::from_index(i).index(), i);
        }
    }

    #[test]
    fn sanitize_drops_dangling_clips_and_fixes_ranges() {
        let mut p = Playlist::new();
        let t = p.tracks[0].id;
        let a = p.add_clip(t, 0, 10, pat());
        p.add_clip(t, 0, 10, ClipKind::Pattern(PatternId(1)));
        p.clip_mut(a).unwrap().offset = -5;
        p.tracks[0].insert = 67;
        p.add_marker(50, "b");
        p.add_marker(10, "a");
        p.sanitize(|id| id == PatternId(7));
        assert_eq!(p.clips.len(), 1);
        assert_eq!(p.clips[0].offset, 0);
        assert_eq!(p.tracks[0].insert, MASTER);
        assert_eq!(p.markers[0].name, "a");
    }
}
