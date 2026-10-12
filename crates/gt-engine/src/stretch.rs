//! Stretched audio clips (Prompt 15, ARCHITECTURE.md D106): which clips need the stretch
//! engine, the time map each one follows, and the cache key of its render.
//!
//! A clip in `TempoFollow` or `Manual` mode plays a render made by the stretch engine
//! ([`gt_dsp::stretch`]) instead of its source. The render starts at the clip's origin (the
//! song time where the source's first frame belongs) and runs at the engine's rate, so playing
//! it needs no resampling. Renders are built off the audio thread ([`StretchJob::render`]):
//! the app on a worker thread into a cache, the export before it starts. Both use the same
//! code, so the export matches playback. A clip whose render is not ready yet plays its
//! source resampled at the right speed (the pitch follows the speed until the render arrives).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use gt_core::{
    Clip, ClipKind, ClipStretch, Project, SampleData, SampleSource, StretchMode, Tick, PPQ,
};
use gt_dsp::stretch::{render, Signalsmith, TimeMap};

/// A stretch render to make: the clip's source through its time map.
#[derive(Debug, Clone)]
pub struct StretchJob {
    /// Cache key: changes whenever the render would.
    pub key: u64,
    /// The source as loaded (at the engine rate).
    pub source: SampleSource,
    /// Output frame (from the clip's origin) to source frame.
    pub map: TimeMap,
    /// Pitch shift.
    pub semitones: f32,
    /// Length of the render.
    pub frames: usize,
}

impl StretchJob {
    /// Makes the render (heavy: for a worker thread).
    pub fn render(&self, sample: &SampleData) -> SampleData {
        let rate = sample.sample_rate;
        let (l, r) = render(
            Box::new(Signalsmith::new(rate)),
            sample.left(),
            sample.right(),
            self.map.clone(),
            self.semitones,
            self.frames,
        );
        SampleData {
            sample_rate: rate,
            channels: vec![l, r],
        }
    }
}

/// Source frames per output frame of a clip that does not use the engine (resampling), or of
/// one whose render is missing (an approximation: the speed at the clip's start).
pub fn plain_speed(project: &Project, clip: &Clip, stretch: &ClipStretch) -> f64 {
    stretch.speed(project.tempo.bpm_at(Tick(clip.start - clip.offset)))
}

/// The cache key of the render `clip` needs, if it uses the stretch engine (cheap: for
/// compiling the song). `sample` is its source as loaded at the engine rate.
pub fn key(project: &Project, clip: &Clip, sample: &SampleData) -> Option<u64> {
    let ClipKind::Audio { source, edit, .. } = &clip.kind else {
        return None;
    };
    let st = edit.stretch.sanitized();
    if !st.uses_engine() {
        return None;
    }
    let mut h = DefaultHasher::new();
    source.hash(&mut h);
    sample.sample_rate.hash(&mut h);
    sample.frames().hash(&mut h);
    st.mode.key().hash(&mut h);
    match st.mode {
        StretchMode::Manual => {
            st.ratio.to_bits().hash(&mut h);
            st.semitones.to_bits().hash(&mut h);
        }
        StretchMode::TempoFollow => {
            st.bpm.to_bits().hash(&mut h);
            // The render depends on where the origin is and on the tempo map.
            (clip.start - clip.offset).hash(&mut h);
            for p in project.tempo.points() {
                p.at.0.hash(&mut h);
                p.bpm.to_bits().hash(&mut h);
            }
        }
        StretchMode::Off => {}
    }
    Some(h.finish())
}

/// The render `clip` needs, if it uses the stretch engine. `sample` is its source as loaded at
/// the engine rate.
pub fn job(project: &Project, clip: &Clip, sample: &SampleData) -> Option<StretchJob> {
    let key = key(project, clip, sample)?;
    let ClipKind::Audio { source, edit, .. } = &clip.kind else {
        return None;
    };
    let st = edit.stretch.sanitized();
    let rate = f64::from(sample.sample_rate.max(1));
    let src_frames = sample.frames() as f64;
    let tempo = &project.tempo;
    let origin_tick = (clip.start - clip.offset) as f64;
    let origin_s = tempo.tick_to_seconds(origin_tick);
    let (map, frames, semitones) = match st.mode {
        StretchMode::Manual => {
            let frames = (src_frames * st.ratio).ceil().max(1.0) as usize;
            (
                TimeMap::linear(1.0 / st.ratio, frames),
                frames,
                st.semitones,
            )
        }
        StretchMode::TempoFollow => {
            // Source beat b belongs at tick origin + b·PPQ.
            let beats = src_frames / rate * st.bpm / 60.0;
            let end_s = tempo.tick_to_seconds(origin_tick + beats * PPQ as f64);
            let frames = ((end_s - origin_s) * rate).ceil().max(1.0) as usize;
            let map = TimeMap::from_fn(frames, |o| {
                let tick = tempo.seconds_to_tick(origin_s + o / rate);
                (tick - origin_tick) / PPQ as f64 * 60.0 / st.bpm * rate
            });
            (map, frames, 0.0)
        }
        StretchMode::Off => return None,
    };
    Some(StretchJob {
        key,
        source: source.clone(),
        map,
        semitones,
        frames,
    })
}

/// Every render the project's audio clips need, one per key, for the sources `samples` has.
pub fn jobs(
    project: &Project,
    samples: impl Fn(&SampleSource) -> Option<Arc<SampleData>>,
) -> Vec<StretchJob> {
    let mut out: Vec<StretchJob> = Vec::new();
    for c in &project.playlist.clips {
        let ClipKind::Audio { source, .. } = &c.kind else {
            continue;
        };
        let Some(sample) = samples(source) else {
            continue;
        };
        if let Some(j) = job(project, c, &sample) {
            if !out.iter().any(|o| o.key == j.key) {
                out.push(j);
            }
        }
    }
    out
}
