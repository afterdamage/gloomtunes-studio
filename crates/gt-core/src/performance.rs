//! Performance mode (Prompt 31, D145).
//!
//! The playlist becomes a grid: a row per track, a cell per clip of that track (in timeline
//! order, pattern and audio clips). Launching a cell schedules it on the next bar or beat; each
//! track plays one clip at a time, looping it until another cell or a stop is launched on that
//! track. The launches are a list of `(track, clip, tick)`; [`Performance::clips`] lays them out
//! as ordinary clips, so playback, recording back into the arrangement and the tests all go
//! through the song compiler unchanged.

use crate::playlist::{Clip, ClipId, ClipKind, Playlist, TrackId};
use crate::project::Project;
use crate::time::{Tick, TimeSigMap};

/// Where a launch waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LaunchQuant {
    /// The next bar line.
    #[default]
    Bar,
    /// The next beat.
    Beat,
    /// At once.
    Off,
}

impl LaunchQuant {
    /// Display names, in [`LaunchQuant::ALL`] order.
    pub const NAMES: [&'static str; 3] = ["Bar", "Beat", "Off"];
    /// Every choice.
    pub const ALL: [Self; 3] = [Self::Bar, Self::Beat, Self::Off];

    /// Position in [`LaunchQuant::ALL`].
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&q| q == self).unwrap_or(0)
    }

    /// The first launch point at or after `t`.
    pub fn next(self, t: i64, sigs: &TimeSigMap) -> i64 {
        let t = t.max(0);
        match self {
            Self::Off => t,
            Self::Bar => {
                let bar = sigs.bar_of(t);
                let start = sigs.bar_start(bar);
                if start == t {
                    t
                } else {
                    sigs.bar_start(bar + 1)
                }
            }
            Self::Beat => {
                let bar = sigs.bar_of(t);
                let start = sigs.bar_start(bar);
                let beat = sigs.sig_of_bar(bar).beat_ticks().max(1);
                let k = (t - start + beat - 1).div_euclid(beat);
                // A bar that is not a whole number of beats ends early: its next bar line wins.
                (start + k * beat).min(sigs.bar_start(bar + 1))
            }
        }
    }
}

/// One launch: from `at` on, `track` plays `clip` (or nothing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Launch {
    /// The row.
    pub track: TrackId,
    /// The cell, or `None` to stop the track.
    pub clip: Option<ClipId>,
    /// Timeline tick it takes over at.
    pub at: i64,
}

/// A performance: its launches in time order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Performance {
    /// Where launches wait for.
    pub quant: LaunchQuant,
    /// Launches sorted by `at` (stable: a later launch at the same tick wins).
    pub launches: Vec<Launch>,
}

/// The cells of a track's row: its pattern and audio clips in timeline order.
pub fn grid_row(pl: &Playlist, track: TrackId) -> Vec<ClipId> {
    let mut v: Vec<&Clip> = pl
        .clips
        .iter()
        .filter(|c| c.track == track && !matches!(c.kind, ClipKind::Automation(_)))
        .collect();
    v.sort_by_key(|c| (c.start, c.id));
    v.into_iter().map(|c| c.id).collect()
}

impl Performance {
    /// Launches `clip` (or a stop) on `track` at the first launch point at or after `earliest`;
    /// launches on that track after it are dropped (they were queued and are replaced).
    /// Returns the tick it takes over at.
    pub fn launch(
        &mut self,
        track: TrackId,
        clip: Option<ClipId>,
        earliest: i64,
        sigs: &TimeSigMap,
    ) -> i64 {
        let at = self.quant.next(earliest, sigs);
        self.launches.retain(|l| l.track != track || l.at < at);
        self.launches.push(Launch { track, clip, at });
        self.launches.sort_by_key(|l| l.at);
        at
    }

    /// What `track` plays at `t`: the clip of its last launch at or before `t`.
    pub fn playing(&self, track: TrackId, t: i64) -> Option<ClipId> {
        self.launches
            .iter()
            .rev()
            .find(|l| l.track == track && l.at <= t)
            .and_then(|l| l.clip)
    }

    /// The launch waiting on `track` after `t`, if any.
    pub fn queued(&self, track: TrackId, t: i64) -> Option<Launch> {
        self.launches
            .iter()
            .find(|l| l.track == track && l.at > t)
            .copied()
    }

    /// The performance as timeline clips up to `end`: each launched clip repeated from its
    /// launch until the next launch on its track (or `end`), the last copy cut short. Clip ids
    /// are left at 0 (callers add them to a playlist).
    pub fn clips(&self, pl: &Playlist, end: i64) -> Vec<Clip> {
        let mut out = Vec::new();
        for (i, l) in self.launches.iter().enumerate() {
            let Some(src) = l.clip.and_then(|id| pl.clip(id)) else {
                continue;
            };
            let until = self.launches[i + 1..]
                .iter()
                .find(|n| n.track == l.track)
                .map_or(end, |n| n.at)
                .min(end);
            let len = src.length.max(1);
            let mut at = l.at;
            while at < until {
                out.push(Clip {
                    id: ClipId(0),
                    track: l.track,
                    start: at,
                    length: len.min(until - at),
                    offset: src.offset,
                    muted: false,
                    link: 0,
                    kind: src.kind.clone(),
                });
                at += len;
            }
        }
        out
    }
}

impl Project {
    /// A copy of the project whose playlist plays `perf` up to `end` instead of its clips
    /// (automation clips stay: they still move their parameters on the timeline).
    pub fn performance_project(&self, perf: &Performance, end: i64) -> Project {
        let mut p = self.clone();
        let clips = perf.clips(&self.playlist, end);
        p.playlist
            .clips
            .retain(|c| matches!(c.kind, ClipKind::Automation(_)));
        for c in clips {
            let id = p.playlist.add_clip(c.track, c.start, c.length, c.kind);
            if let Some(n) = p.playlist.clip_mut(id) {
                n.offset = c.offset;
            }
        }
        p
    }

    /// Records a performance back: a copy of the current arrangement named "Performance"
    /// whose pattern and audio clips are the performance up to `end`. It becomes current;
    /// returns its index (`None` when no more arrangements fit).
    pub fn record_performance(&mut self, perf: &Performance, end: i64) -> Option<usize> {
        let source = self.playlist.clone();
        let i = self.duplicate_arrangement(self.current_arrangement)?;
        let name = self.unique_arrangement_name("Performance");
        self.rename_arrangement(i, &name);
        self.playlist
            .clips
            .retain(|c| matches!(c.kind, ClipKind::Automation(_)));
        self.playlist.markers.clear();
        for c in perf.clips(&source, end) {
            let id = self.playlist.add_clip(c.track, c.start, c.length, c.kind);
            if let Some(n) = self.playlist.clip_mut(id) {
                n.offset = c.offset;
            }
        }
        Some(i)
    }
}

/// The tick a launch made now takes over at, so that it reaches the engine in time: `now`
/// plus a safety margin, rounded up to the launch point.
pub fn launch_point(quant: LaunchQuant, now: Tick, margin: i64, sigs: &TimeSigMap) -> i64 {
    quant.next(now.0 + margin.max(0), sigs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::TimeSig;
    use crate::PPQ;

    #[test]
    fn launch_points_are_the_next_bar_or_beat() {
        let sigs = TimeSigMap::constant(TimeSig::new(4, 4));
        let bar = 4 * PPQ;
        assert_eq!(LaunchQuant::Bar.next(0, &sigs), 0);
        assert_eq!(LaunchQuant::Bar.next(1, &sigs), bar);
        assert_eq!(LaunchQuant::Bar.next(bar, &sigs), bar);
        assert_eq!(LaunchQuant::Beat.next(PPQ + 1, &sigs), 2 * PPQ);
        assert_eq!(LaunchQuant::Beat.next(3 * PPQ, &sigs), 3 * PPQ);
        assert_eq!(LaunchQuant::Off.next(77, &sigs), 77);
        // 7/8 then 4/4: beats are eighths in the odd bar.
        let sigs = TimeSigMap::new(&[
            crate::SigChange {
                bar: 0,
                sig: TimeSig::new(7, 8),
            },
            crate::SigChange {
                bar: 1,
                sig: TimeSig::new(4, 4),
            },
        ]);
        assert_eq!(LaunchQuant::Beat.next(PPQ / 2 + 1, &sigs), PPQ);
        assert_eq!(LaunchQuant::Bar.next(1, &sigs), 7 * PPQ / 2);
    }

    #[test]
    fn each_track_plays_one_clip_and_loops_it_until_the_next_launch() {
        let mut p = Project::demo();
        let t = p.playlist.tracks[0].id;
        let row = grid_row(&p.playlist, t);
        assert!(row.len() >= 2, "the demo's first track has two clips");
        let (a, b) = (row[0], row[1]);
        let len_a = p.playlist.clip(a).unwrap().length;
        let sigs = p.signatures.clone();
        let mut perf = Performance::default();
        assert_eq!(perf.launch(t, Some(a), 0, &sigs), 0);
        // Queued for bar 3, then replaced by a launch of b for the same bar.
        let bar3 = sigs.bar_start(2);
        assert_eq!(perf.launch(t, None, bar3 - 5, &sigs), bar3);
        assert_eq!(perf.queued(t, 10).unwrap().clip, None);
        perf.launch(t, Some(b), bar3 - 5, &sigs);
        assert_eq!(perf.queued(t, 10).unwrap().clip, Some(b));
        assert_eq!(perf.playing(t, 10), Some(a));
        assert_eq!(perf.playing(t, bar3), Some(b));
        let end = bar3 + 3;
        let clips = perf.clips(&p.playlist, end);
        let on_t: Vec<_> = clips.iter().filter(|c| c.track == t).collect();
        // a tiles from 0 to bar 3, then b starts at bar 3 and is cut at the end.
        assert_eq!(on_t[0].start, 0);
        assert!(on_t
            .iter()
            .all(|c| c.start < end && c.start + c.length <= end));
        let a_end: i64 = on_t
            .iter()
            .filter(|c| c.start < bar3)
            .map(|c| c.start + c.length)
            .max()
            .unwrap();
        assert_eq!(a_end, bar3.min(len_a * ((bar3 + len_a - 1) / len_a)));
        assert_eq!(on_t.last().unwrap().start, bar3);
        // Recording makes a new current arrangement with those clips.
        let before = p.arrangement_count();
        let i = p.record_performance(&perf, end).unwrap();
        assert_eq!(p.arrangement_count(), before + 1);
        assert_eq!(p.current_arrangement, i);
        assert!(p
            .arrangement_names()
            .nth(i)
            .unwrap()
            .starts_with("Performance"));
        let rec: Vec<_> = p.playlist.clips.iter().filter(|c| c.track == t).collect();
        assert_eq!(rec.len(), on_t.len());
    }
}
