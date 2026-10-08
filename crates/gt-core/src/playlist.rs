//! The playlist: tracks, clips and markers (ARCHITECTURE.md §6).
//!
//! A track is a lane for organisation, mute and solo; any clip can sit on any track. A clip
//! shows a window `[offset, offset + length)` of its source starting at `start` on the timeline:
//!
//! - a pattern clip repeats its pattern for as long as the clip is;
//! - an audio clip plays a sample file once, at its own speed, into the track's mixer strip;
//! - an automation clip moves one mixer parameter along its points.
//!
//! Slip edit changes `offset` only, so the content moves inside a clip that stays put. All
//! positions are ticks.

use crate::effects::EffectKind;
use crate::mixer::{Mixer, MixerStrip, StripKind, FX_SLOTS, MASTER, STRIPS};
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

/// What an automation clip moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutoTarget {
    /// A strip's fader.
    StripVolume(usize),
    /// A strip's balance.
    StripPan(usize),
    /// One parameter of the effect in a slot. Ignored while the slot holds another kind.
    EffectParam {
        /// Strip index.
        strip: usize,
        /// Slot index.
        slot: usize,
        /// Effect the parameter belongs to.
        kind: EffectKind,
        /// Parameter index in [`EffectKind::params`].
        index: usize,
    },
}

impl AutoTarget {
    /// Display name, e.g. "Delay · Volume" or "Bass · EQ · B1 freq".
    pub fn name(&self, mixer: &Mixer) -> String {
        let strip = |i: usize| {
            mixer
                .strips
                .get(i)
                .map_or_else(|| StripKind::of(i).short(), |s| s.name.clone())
        };
        match *self {
            Self::StripVolume(s) => format!("{} · Volume", strip(s)),
            Self::StripPan(s) => format!("{} · Pan", strip(s)),
            Self::EffectParam {
                strip: s,
                kind,
                index,
                ..
            } => {
                let p = kind.params().get(index).map_or("?", |p| p.name);
                format!("{} · {} · {}", strip(s), kind.name(), p)
            }
        }
    }

    /// True if the target exists in `mixer` (the strip and slot exist and the slot holds the
    /// effect kind the target was made for).
    pub fn is_valid(&self, mixer: &Mixer) -> bool {
        match *self {
            Self::StripVolume(s) | Self::StripPan(s) => s < STRIPS,
            Self::EffectParam {
                strip,
                slot,
                kind,
                index,
            } => {
                strip < STRIPS
                    && slot < FX_SLOTS
                    && index < kind.params().len()
                    && mixer.strips[strip].slots[slot]
                        .as_ref()
                        .is_some_and(|s| s.kind == kind)
            }
        }
    }

    /// Normalized value (0..1) of the target's current setting in `mixer`.
    pub fn current(&self, mixer: &Mixer) -> f32 {
        match *self {
            Self::StripVolume(s) => mixer
                .strips
                .get(s)
                .map_or(0.0, |x| volume_to_norm(x.volume)),
            Self::StripPan(s) => mixer.strips.get(s).map_or(0.5, |x| (x.pan + 1.0) * 0.5),
            Self::EffectParam {
                strip,
                slot,
                kind,
                index,
            } => {
                let info = kind.params().get(index);
                let value = mixer
                    .strips
                    .get(strip)
                    .and_then(|s| s.slots.get(slot)?.as_ref())
                    .filter(|s| s.kind == kind)
                    .and_then(|s| s.params.get(index).copied());
                match (info, value) {
                    (Some(i), Some(v)) => i.to_normalized(v),
                    (Some(i), None) => i.to_normalized(i.default),
                    _ => 0.0,
                }
            }
        }
    }

    /// Text for a normalized value, e.g. "-3.2 dB", "L 40 %", "1.20 kHz".
    pub fn format(&self, t: f32) -> String {
        match *self {
            Self::StripVolume(_) => {
                let g = norm_to_volume(t);
                if g <= 1e-5 {
                    "-inf dB".to_owned()
                } else {
                    format!("{:+.1} dB", 20.0 * g.log10())
                }
            }
            Self::StripPan(_) => {
                let p = t * 2.0 - 1.0;
                if p.abs() < 0.005 {
                    "C".to_owned()
                } else if p < 0.0 {
                    format!("L {:.0} %", -p * 100.0)
                } else {
                    format!("R {:.0} %", p * 100.0)
                }
            }
            Self::EffectParam { kind, index, .. } => kind
                .params()
                .get(index)
                .map_or_else(String::new, |i| i.format(i.from_normalized(t))),
        }
    }
}

/// Fader position (0..1) to strip gain: `MixerStrip::MAX_VOLUME · t³` (unity at about 79 %).
pub fn norm_to_volume(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    MAX_VOLUME * t * t * t
}

/// Inverse of [`norm_to_volume`].
pub fn volume_to_norm(gain: f32) -> f32 {
    (gain.max(0.0) / MAX_VOLUME).cbrt().clamp(0.0, 1.0)
}

/// A breakpoint of an automation clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoPoint {
    /// Position in ticks from the start of the clip's source (like a note in a pattern).
    pub at: i64,
    /// Normalized value, 0 to 1.
    pub value: f32,
}

/// The source of an automation clip: a target and a line through points.
#[derive(Debug, Clone, PartialEq)]
pub struct Automation {
    /// What moves.
    pub target: AutoTarget,
    /// Breakpoints sorted by `at`; values between points are interpolated linearly, and the
    /// first and last values hold before and after.
    pub points: Vec<AutoPoint>,
}

impl Automation {
    /// Value at source position `at` (ticks from the source start).
    pub fn value_at(&self, at: f64) -> f32 {
        value_at(&self.points, at)
    }

    /// Inserts a point, keeping the list sorted; returns its index.
    pub fn insert_point(&mut self, p: AutoPoint) -> usize {
        let i = self.points.partition_point(|q| q.at <= p.at);
        self.points.insert(
            i,
            AutoPoint {
                at: p.at,
                value: p.value.clamp(0.0, 1.0),
            },
        );
        i
    }
}

/// Linear interpolation through sorted points; holds the end values outside them.
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
    a.value + (b.value - a.value) * f
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
    /// Automation of one mixer parameter.
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
            AutoPoint { at: 0, value: 0.0 },
            AutoPoint {
                at: 100,
                value: 1.0,
            },
            AutoPoint {
                at: 200,
                value: 0.5,
            },
        ];
        assert_eq!(value_at(&pts, -5.0), 0.0);
        assert_eq!(value_at(&pts, 50.0), 0.5);
        assert_eq!(value_at(&pts, 150.0), 0.75);
        assert_eq!(value_at(&pts, 900.0), 0.5);
        assert_eq!(value_at(&[], 3.0), 0.0);
        let mut a = Automation {
            target: AutoTarget::StripVolume(1),
            points: pts.to_vec(),
        };
        assert_eq!(
            a.insert_point(AutoPoint {
                at: 150,
                value: 2.0
            }),
            2
        );
        assert_eq!(a.points[2].value, 1.0, "clamped");
    }

    #[test]
    fn volume_law_round_trips() {
        for g in [0.0, 0.1, 0.5, 1.0, MAX_VOLUME] {
            assert!((norm_to_volume(volume_to_norm(g)) - g).abs() < 1e-5);
        }
        let t = AutoTarget::StripVolume(0);
        assert_eq!(t.format(volume_to_norm(1.0)), "+0.0 dB");
        assert_eq!(AutoTarget::StripPan(0).format(0.5), "C");
        assert_eq!(AutoTarget::StripPan(0).format(0.0), "L 100 %");
    }

    #[test]
    fn effect_targets_follow_the_slot() {
        let mut m = Mixer::new();
        let t = AutoTarget::EffectParam {
            strip: 3,
            slot: 1,
            kind: EffectKind::Delay,
            index: 0,
        };
        assert!(!t.is_valid(&m));
        m.strips[3].slots[1] = Some(crate::EffectSlot::new(EffectKind::Delay));
        assert!(t.is_valid(&m));
        assert!(t.name(&m).contains("Delay"));
        m.strips[3].slots[1] = Some(crate::EffectSlot::new(EffectKind::Chorus));
        assert!(!t.is_valid(&m));
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
