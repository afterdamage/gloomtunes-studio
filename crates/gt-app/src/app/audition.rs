//! Hearing browser rows that are not sounds: a synth preset plays a phrase that suits it, a
//! drum kit a one-bar groove, an effect a demo loop through it, a project its first bars.
//! Each is a small project rendered offline on a worker thread (the same engine as export)
//! and played through the preview voice; renders are kept for the session.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

use gt_core::playlist::ClipKind;
use gt_core::{
    BuiltInSample, DrumKit, EffectKind, EffectSlot, Note, Project, SampleData, SampleSource,
    SynthPatch, FX_SLOTS, MASTER, PPQ, STEP_TICKS,
};
use gt_engine::EngineCommand;
use gt_ui::views::RowItem;

use super::GloomApp;

/// Renders kept at most.
const CACHE: usize = 48;

/// What a preview renders.
pub(super) enum Job {
    Synth(Box<SynthPatch>),
    Kit(Box<DrumKit>),
    Effect(Box<EffectSlot>),
    Project(PathBuf),
}

/// The previews being rendered and those done.
#[derive(Default)]
pub(super) struct Auditions {
    done: HashMap<String, Arc<SampleData>>,
    order: Vec<String>,
    pending: Option<(String, mpsc::Receiver<Result<SampleData, String>>)>,
}

impl Auditions {
    /// True while a render is on its way (the app keeps repainting to pick it up).
    pub(super) fn waiting(&self) -> bool {
        self.pending.is_some()
    }
}

impl GloomApp {
    /// Plays a preview of a preset, kit, effect or project row (rendered first if needed).
    pub(super) fn audition(&mut self, item: RowItem) {
        let rate = self.rate();
        let key = format!("{item:?}@{rate}");
        if let Some(d) = self.auditions.done.get(&key) {
            self.auditions.pending = None;
            self.send(EngineCommand::PreviewSample(Some(Arc::clone(d))));
            return;
        }
        let job = match &item {
            RowItem::FactorySynth(_) | RowItem::UserSynth(_) => match self.patch_of(&item) {
                Some(p) => Job::Synth(Box::new(p)),
                None => return,
            },
            RowItem::Kit(k) => match DrumKit::factory_kits().into_iter().nth(*k) {
                Some((_, kit)) => Job::Kit(Box::new(kit)),
                None => return,
            },
            RowItem::Effect(fx, preset) => {
                let Some(kind) = EffectKind::from_key(fx) else {
                    return;
                };
                let mut slot = EffectSlot::new(kind);
                if let Some(p) = preset.and_then(|i| kind.presets().get(i)) {
                    slot.apply_preset(p);
                }
                Job::Effect(Box::new(slot))
            }
            RowItem::Project(path) => Job::Project(path.clone()),
            _ => return,
        };
        // The last preview stops while this one renders.
        self.send(EngineCommand::PreviewSample(None));
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("preview-render".into())
            .spawn(move || {
                let _ = tx.send(render(job, rate));
            });
        if spawned.is_ok() {
            self.auditions.pending = Some((key, rx));
        }
    }

    /// Plays a preview render once it arrives.
    pub(super) fn poll_audition(&mut self) {
        let Some((key, rx)) = &self.auditions.pending else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("the preview stopped".to_owned()),
        };
        let key = key.clone();
        self.auditions.pending = None;
        match result {
            Ok(data) => {
                let data = Arc::new(data);
                let a = &mut self.auditions;
                if a.order.len() >= CACHE {
                    let old = a.order.remove(0);
                    a.done.remove(&old);
                }
                a.order.push(key.clone());
                a.done.insert(key, Arc::clone(&data));
                self.send(EngineCommand::PreviewSample(Some(data)));
            }
            Err(e) => self.toast(format!("No preview: {e}")),
        }
    }
}

/// Renders a preview at `rate`.
fn render(job: Job, rate: u32) -> Result<SampleData, String> {
    let (project, tail, normalize, bars) = match job {
        Job::Synth(patch) => (synth_project(*patch), 2.0, Some(-6.0), 1),
        Job::Kit(kit) => (kit_project(*kit), 1.0, Some(-6.0), 1),
        Job::Effect(slot) => (effect_project(*slot), 3.0, Some(-6.0), 2),
        Job::Project(path) => {
            let dir = crate::library::data_folder().join("embedded");
            let loaded = gt_project::file::load(&path, &dir).map_err(|e| e.to_string())?;
            (loaded.project, 1.0, None, 8)
        }
    };
    let song_end = project.playlist.song_end();
    if song_end <= 0 {
        return Err("it has nothing to play".to_owned());
    }
    let bar = 4 * PPQ;
    let settings = gt_export::ExportSettings {
        sample_rate: rate,
        depth: gt_export::BitDepth::Float32,
        dither: false,
        normalize_db: normalize,
        range: gt_export::Range::Ticks {
            start: 0,
            end: song_end.min(bars * bar),
        },
        tail: true,
        max_tail_seconds: tail,
        ..gt_export::ExportSettings::default()
    };
    let (samples, _) = gt_export::load_samples(&project, rate);
    let progress = gt_export::Progress::default();
    // Plugins are not loaded for a preview; their channels stay silent.
    let mut plugins = Vec::new();
    let out = gt_export::render(&project, &samples, &settings, &progress, &mut plugins)
        .map_err(|e| e.to_string())?;
    let audio = &out.first().ok_or("it has nothing to play")?.audio;
    let (l, r): (Vec<f32>, Vec<f32>) = audio.chunks_exact(2).map(|f| (f[0], f[1])).unzip();
    Ok(SampleData {
        sample_rate: rate,
        channels: vec![l, r],
    })
}

/// A one-bar pattern of `notes` (start step, length in steps, key, velocity) on channel
/// `ch`, placed once in the playlist.
fn one_bar(p: &mut Project, ch: gt_core::ChannelId, bars: i64, notes: &[(i64, i64, u8, f32)]) {
    let pat = p.current_pattern_mut();
    pat.steps = (16 * bars) as u16;
    let list = pat.notes.entry(ch).or_default();
    list.extend(
        notes
            .iter()
            .map(|&(at, len, key, v)| Note::new(at * STEP_TICKS, len * STEP_TICKS, key, v)),
    );
    list.sort_by_key(|n| (n.start, n.key));
    let id = p.current_pattern;
    if p.playlist.clips.is_empty() {
        let track = p.playlist.tracks[0].id;
        p.playlist
            .add_clip(track, 0, bars * 4 * PPQ, ClipKind::Pattern(id));
    }
}

/// What a synth preset plays, from its engine and name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Bass,
    Chords,
    Hit,
    Lead,
}

fn role(patch: &SynthPatch) -> Role {
    let name = patch.name.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    if has(&["kick", "snare", "hat", "clap", "perc", "thump", "tom"]) {
        Role::Hit
    } else if patch.engine() == 3 || has(&["bass", "sub", "808", "reese", "acid", "line"]) {
        Role::Bass
    } else if has(&[
        "pad", "choir", "string", "organ", "piano", "keys", "wind", "texture", "swell",
    ]) {
        Role::Chords
    } else {
        Role::Lead
    }
}

fn synth_project(patch: SynthPatch) -> Project {
    let mut p = Project::empty();
    let notes: &[(i64, i64, u8, f32)] = match role(&patch) {
        Role::Bass => &[
            (0, 3, 36, 0.9),
            (3, 1, 36, 0.7),
            (6, 2, 43, 0.85),
            (10, 2, 41, 0.8),
            (13, 3, 39, 0.8),
        ],
        Role::Chords => &[
            (0, 14, 60, 0.75),
            (0, 14, 63, 0.75),
            (0, 14, 67, 0.75),
            (0, 14, 70, 0.75),
        ],
        Role::Hit => &[
            (0, 2, 60, 0.9),
            (4, 2, 60, 0.7),
            (8, 2, 60, 0.9),
            (12, 2, 60, 0.7),
        ],
        Role::Lead => &[
            (0, 2, 60, 0.85),
            (2, 2, 63, 0.75),
            (4, 2, 67, 0.8),
            (6, 2, 72, 0.85),
            (8, 2, 70, 0.75),
            (10, 2, 67, 0.75),
            (12, 4, 63, 0.8),
        ],
    };
    let name = patch.name.clone();
    if let Some(ch) = p.add_synth_channel(&name, patch) {
        one_bar(&mut p, ch, 1, notes);
    }
    p
}

fn kit_project(kit: DrumKit) -> Project {
    let mut p = Project::empty();
    let Some(ch) = p.add_drum_synth_channel("Kit") else {
        return p;
    };
    if let Some(d) = p.channels.last_mut().and_then(|c| c.drums_mut()) {
        *d = kit;
    }
    let pad = |i: usize| DrumKit::key(0, i);
    let (kick, snare, hat, open, clap, low, high, perc) = (
        pad(0),
        pad(1),
        pad(2),
        pad(3),
        pad(4),
        pad(5),
        pad(6),
        pad(7),
    );
    let mut notes = vec![
        (0, 1, kick, 0.95),
        (7, 1, kick, 0.7),
        (8, 1, kick, 0.9),
        (4, 1, snare, 0.9),
        (12, 1, snare, 0.9),
        (12, 1, clap, 0.6),
        (14, 1, open, 0.7),
        (11, 1, perc, 0.6),
        (13, 1, low, 0.7),
        (15, 1, high, 0.7),
    ];
    notes.extend((0..14).step_by(2).map(|s| (s, 1, hat, 0.6)));
    one_bar(&mut p, ch, 1, &notes);
    p
}

/// Built-in drums and a pluck through the effect on the master.
fn effect_project(slot: EffectSlot) -> Project {
    let mut p = Project::empty();
    let drums = [
        (BuiltInSample::Kick, vec![0, 8, 16, 24]),
        (BuiltInSample::Snare, vec![4, 12, 20, 28]),
        (BuiltInSample::Hat, (2..32).step_by(4).collect()),
    ];
    for (b, steps) in drums {
        if let Some(ch) = p.add_channel(b.name(), Some(SampleSource::BuiltIn(b))) {
            let hits: Vec<_> = steps.into_iter().map(|s| (s, 1, 60, 0.85)).collect();
            one_bar(&mut p, ch, 2, &hits);
        }
    }
    let pluck = SynthPatch::factory()
        .into_iter()
        .find(|x| x.name == "Glass Pluck")
        .unwrap_or_default();
    if let Some(ch) = p.add_synth_channel("Pluck", pluck) {
        one_bar(
            &mut p,
            ch,
            2,
            &[
                (0, 2, 60, 0.8),
                (3, 2, 63, 0.7),
                (6, 2, 67, 0.75),
                (10, 2, 70, 0.7),
                (16, 2, 58, 0.8),
                (19, 2, 62, 0.7),
                (22, 2, 65, 0.75),
                (26, 4, 67, 0.7),
            ],
        );
    }
    // Every channel plays into the master, where the effect sits.
    for c in &mut p.channels {
        c.insert = MASTER;
    }
    p.mixer.strips[MASTER].slots[0] = Some(slot);
    p.mixer.strips[MASTER].slots[FX_SLOTS - 1] = None;
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loud(d: &SampleData) -> f32 {
        d.channels
            .iter()
            .flatten()
            .fold(0.0_f32, |m, x| m.max(x.abs()))
    }

    #[test]
    fn presets_kits_and_effects_render_a_short_preview() {
        let rate = 22_050;
        for patch in SynthPatch::factory().into_iter().step_by(17) {
            let name = patch.name.clone();
            let d = render(Job::Synth(Box::new(patch)), rate).unwrap();
            assert!(loud(&d) > 0.1, "{name}");
            // One bar at 120 BPM and at most two seconds of tail.
            assert!(d.frames() <= rate as usize * 4 + 64, "{name}");
        }
        let (_, kit) = DrumKit::factory_kits().remove(1);
        assert!(loud(&render(Job::Kit(Box::new(kit)), rate).unwrap()) > 0.1);
        let reverb = render(
            Job::Effect(Box::new(EffectSlot::new(EffectKind::Reverb))),
            rate,
        );
        assert!(loud(&reverb.unwrap()) > 0.1);
        assert!(render(Job::Project(PathBuf::from("/no/such.gloom")), rate).is_err());
    }

    #[test]
    fn presets_play_a_phrase_that_suits_them() {
        let by_name = |n: &str| {
            SynthPatch::factory()
                .into_iter()
                .find(|p| p.name == n)
                .unwrap()
        };
        assert_eq!(role(&by_name("Gloom Bass")), Role::Bass);
        assert_eq!(role(&by_name("Acid Line")), Role::Bass);
        assert_eq!(role(&by_name("Dark Pad")), Role::Chords);
        assert_eq!(role(&by_name("FM Kick")), Role::Hit);
        assert_eq!(role(&by_name("Ember Lead")), Role::Lead);
    }
}
