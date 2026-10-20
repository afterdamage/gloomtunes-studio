//! Render to audio clip, consolidate and freeze (Prompt 14, D105): offline renders of one
//! playlist track through the export renderer, so the result matches playback.
//!
//! The track plays alone (other tracks with sound muted, automation as in the mix) and the
//! engine listens to the track's mixer insert instead of the master ([`Tap`]):
//!
//! - **Render to audio clip** and **Consolidate** take what reaches the insert (before its
//!   effects), so the new clip plays back through the same effects.
//! - **Freeze** takes the insert's freeze point (after an insert's effects; before the master's,
//!   which belong to the whole mix), and the frozen render joins there while the track's clips
//!   are bypassed.
//!
//! Every sound of the track must reach its insert: audio clips always do; a pattern's channels
//! must play into the insert or into strips that lead to it (see [`check_routing`]).

use std::collections::HashMap;
use std::sync::Arc;

use gt_core::{
    AudioEdit, ClipId, ClipKind, Frozen, Project, SampleData, SampleSource, TrackId, MASTER, STRIPS,
};
use gt_engine::{PluginBox, Tap};

use crate::{render_one, trimmed, BitDepth, ExportError, ExportSettings, Progress, Range};

/// Longest release or effect tail a freeze keeps after the track's last clip, in seconds.
pub const FREEZE_TAIL_S: f64 = 30.0;
/// Longest tail of "Render to audio clip", in seconds.
pub const RENDER_TAIL_S: f32 = 10.0;
/// A freeze render ends after its last sample above this level (-140 dBFS).
const FREEZE_FLOOR: f32 = 1e-7;

/// One offline render of a track.
#[derive(Debug, Clone, PartialEq)]
pub struct Bounce {
    /// The track.
    pub track: TrackId,
    /// The clips that play; empty for every clip of the track.
    pub clips: Vec<ClipId>,
    /// First tick.
    pub start: i64,
    /// First tick after the range (the tail may run on).
    pub end: i64,
    /// Where the engine listens.
    pub tap: Tap,
    /// Keep rendering past `end` until the sound dies away (at most [`RENDER_TAIL_S`]).
    pub tail: bool,
}

/// Why a track cannot be rendered on its own.
pub fn check_routing(project: &Project, track: TrackId) -> Result<(), String> {
    let pl = &project.playlist;
    let t = pl.track(track).ok_or("no such track")?;
    let insert = pl.track_insert(t.id).min(STRIPS - 1);
    for c in pl.clips.iter().filter(|c| c.track == track) {
        let ClipKind::Pattern(id) = c.kind else {
            continue;
        };
        let Some(pat) = project.pattern(id) else {
            continue;
        };
        for (ch, notes) in &pat.notes {
            if notes.is_empty() {
                continue;
            }
            let Some(i) = project.channel_index(*ch) else {
                continue;
            };
            let channel = &project.channels[i];
            if !reaches(project, channel.insert, insert) {
                return Err(format!(
                    "\"{}\" plays into {}, which does not lead to this track's mixer strip ({}). \
                     Route the channel there, or set the track's strip to the master.",
                    channel.name,
                    strip_name(project, channel.insert),
                    strip_name(project, insert),
                ));
            }
        }
    }
    Ok(())
}

fn strip_name(project: &Project, strip: usize) -> String {
    project
        .mixer
        .strips
        .get(strip)
        .map_or_else(|| "the master".to_owned(), |s| s.name.clone())
}

/// True if sound entering strip `from` passes through strip `to` (following each strip's
/// output; every strip ends at the master).
fn reaches(project: &Project, from: usize, to: usize) -> bool {
    let mut s = from.min(STRIPS - 1);
    for _ in 0..=STRIPS {
        if s == to {
            return true;
        }
        if s == MASTER {
            return false;
        }
        s = project
            .mixer
            .strips
            .get(s)
            .map_or(MASTER, |st| st.output.min(STRIPS - 1));
    }
    false
}

/// The track alone: other tracks with sound muted, unsoloed, this track's own freeze lifted,
/// and only `clips` (if any) of its sound clips left.
fn isolated(project: &Project, b: &Bounce) -> Project {
    let mut p = project.clone();
    let pl = &mut p.playlist;
    let sounding: Vec<bool> = pl
        .tracks
        .iter()
        .map(|t| {
            pl.clips
                .iter()
                .any(|c| c.track == t.id && !matches!(c.kind, ClipKind::Automation(_)))
        })
        .collect();
    let audible: Vec<bool> = pl
        .tracks
        .iter()
        .map(|t| pl.is_track_audible(t.id))
        .collect();
    for (i, t) in pl.tracks.iter_mut().enumerate() {
        t.solo = false;
        if t.id == b.track {
            t.mute = false;
            t.frozen = None;
        } else {
            t.mute = sounding[i] || !audible[i];
        }
    }
    if !b.clips.is_empty() {
        pl.clips.retain(|c| {
            c.track != b.track
                || matches!(c.kind, ClipKind::Automation(_))
                || b.clips.contains(&c.id)
        });
        for c in &mut pl.clips {
            if b.clips.contains(&c.id) {
                c.muted = false;
            }
        }
    }
    p
}

/// Renders `b` at `rate` as interleaved stereo. `samples` holds every sound at `rate`
/// ([`crate::load_samples`]); `plugins` the project's plugins for offline use (handed back).
// The engine takes plugins boxed.
#[allow(clippy::vec_box)]
pub fn render(
    project: &Project,
    samples: &HashMap<SampleSource, Arc<SampleData>>,
    b: &Bounce,
    rate: u32,
    progress: &Progress,
    plugins: &mut Vec<Box<PluginBox>>,
) -> Result<Vec<f32>, ExportError> {
    if b.end <= b.start {
        return Err(ExportError::Empty);
    }
    let insert = project
        .playlist
        .track(b.track)
        .ok_or(ExportError::Empty)?
        .insert
        .min(STRIPS - 1);
    let p = trimmed(&isolated(project, b), b.end);
    let settings = ExportSettings {
        sample_rate: rate,
        depth: BitDepth::Float32,
        dither: false,
        normalize_db: None,
        range: Range::Ticks {
            start: b.start,
            end: b.end,
        },
        tail: b.tail,
        max_tail_seconds: RENDER_TAIL_S,
        stems: false,
        ..ExportSettings::default()
    };
    let audio = render_one(
        &p,
        samples,
        &settings,
        b.start,
        b.end,
        progress,
        (0, 1),
        plugins,
        Some((insert as u16, b.tap)),
    )?;
    progress
        .permille
        .store(1000, std::sync::atomic::Ordering::Relaxed);
    Ok(audio)
}

/// The span of a track's sound clips (all, or `clips`): (first start, last end).
fn span(project: &Project, track: TrackId, clips: &[ClipId]) -> Option<(i64, i64)> {
    let mut it = project.playlist.clips.iter().filter(|c| {
        c.track == track
            && !matches!(c.kind, ClipKind::Automation(_))
            && (clips.is_empty() || clips.contains(&c.id))
    });
    let first = it.next()?;
    Some(it.fold((first.start, first.end()), |(a, b), c| {
        (a.min(c.start), b.max(c.end()))
    }))
}

/// Freezing `track`: from the song start (so every instrument and effect starts exactly as
/// in playback) to [`FREEZE_TAIL_S`] after its last clip. Trim the result with
/// [`trim_silence`].
pub fn freeze_job(project: &Project, track: TrackId) -> Option<Bounce> {
    let (_, end) = span(project, track, &[])?;
    let tempo = &project.tempo;
    let end = tempo
        .seconds_to_tick(tempo.tick_to_seconds(end as f64) + FREEZE_TAIL_S)
        .ceil() as i64;
    Some(Bounce {
        track,
        clips: Vec::new(),
        start: 0,
        end,
        tap: Tap::Freeze,
        tail: false,
    })
}

/// Consolidating `clips` (all on one track) into one clip covering their span.
pub fn consolidate_job(project: &Project, clips: &[ClipId]) -> Option<Bounce> {
    let track = project.playlist.clip(*clips.first()?)?.track;
    if clips
        .iter()
        .any(|&id| project.playlist.clip(id).map(|c| c.track) != Some(track))
    {
        return None;
    }
    let (start, end) = span(project, track, clips)?;
    Some(Bounce {
        track,
        clips: clips.to_vec(),
        start,
        end,
        tap: Tap::Input,
        tail: false,
    })
}

/// Rendering a track to a new audio clip: the selected `clips` of the track (their span), or,
/// with none selected, everything on the track between `range` (e.g. the loop), with the tail.
pub fn render_job(
    project: &Project,
    track: TrackId,
    clips: &[ClipId],
    range: Option<(i64, i64)>,
) -> Option<Bounce> {
    let (start, end) = if clips.is_empty() {
        range.filter(|(a, b)| b > a)?
    } else {
        span(project, track, clips)?
    };
    Some(Bounce {
        track,
        clips: clips.to_vec(),
        start,
        end,
        tap: Tap::Input,
        tail: true,
    })
}

/// Drops the end of a freeze render once it stays below -140 dBFS.
pub fn trim_silence(audio: &mut Vec<f32>) {
    let last = audio.iter().rposition(|x| x.abs() > FREEZE_FLOOR);
    let frames = last.map_or(0, |i| i / 2 + 1);
    audio.truncate(2 * frames);
}

/// Length in ticks of `frames` of audio at `rate` starting at tick `start` (rounded up).
pub fn ticks_for(project: &Project, start: i64, frames: usize, rate: u32) -> i64 {
    let tempo = &project.tempo;
    let s = tempo.tick_to_seconds(start as f64) + frames as f64 / f64::from(rate.max(1));
    (tempo.seconds_to_tick(s).ceil() as i64 - start).max(1)
}

fn audio_clip(source: SampleSource) -> ClipKind {
    ClipKind::Audio {
        source,
        gain: 1.0,
        gaps: Vec::new(),
        takes: Vec::new(),
        edit: AudioEdit::default(),
    }
}

/// Freezes `track` with its render. Returns false if the track is gone.
pub fn apply_freeze(project: &mut Project, track: TrackId, source: SampleSource) -> bool {
    let Some(t) = project.playlist.track_mut(track) else {
        return false;
    };
    t.frozen = Some(Frozen { source, start: 0 });
    true
}

/// Replaces `clips` with one audio clip of `length` ticks at the start of `b`.
pub fn apply_consolidate(
    project: &mut Project,
    b: &Bounce,
    source: SampleSource,
    length: i64,
) -> Option<ClipId> {
    project.playlist.track(b.track)?;
    project.playlist.remove_clips(&b.clips);
    Some(
        project
            .playlist
            .add_clip(b.track, b.start, length, audio_clip(source)),
    )
}

/// Puts a render on a new track right below `b.track` (same mixer strip) and mutes what it
/// replaces: the selected clips, or the track's sound clips in the range. Returns the new track
/// and clip.
pub fn apply_render(
    project: &mut Project,
    b: &Bounce,
    source: SampleSource,
    length: i64,
) -> Option<(TrackId, ClipId)> {
    let pl = &mut project.playlist;
    let at = pl.track_index(b.track)?;
    let (name, insert) = {
        let t = &pl.tracks[at];
        (format!("{} (render)", t.name), t.insert)
    };
    let id = pl.add_track();
    let mut t = pl.tracks.pop()?;
    t.name = name;
    t.insert = insert;
    pl.tracks.insert(at + 1, t);
    for c in &mut pl.clips {
        let hit = if b.clips.is_empty() {
            c.track == b.track && c.start < b.end && c.end() > b.start
        } else {
            b.clips.contains(&c.id)
        };
        if hit && !matches!(c.kind, ClipKind::Automation(_)) {
            c.muted = true;
        }
    }
    let clip = pl.add_clip(id, b.start, length, audio_clip(source));
    Some((id, clip))
}
