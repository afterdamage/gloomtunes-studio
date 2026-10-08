//! The project document: channels, patterns and their notes (ARCHITECTURE.md §6).
//!
//! Plain data, edited on the UI thread only. The engine never sees these types; `gt-engine`
//! compiles them into flat, real-time-friendly snapshots.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::time::PPQ;

/// Ticks per step of the channel rack: a 1/16 note.
pub const STEP_TICKS: i64 = PPQ / 4;
/// MIDI key that plays a sample at its original pitch (C5 in FL-style naming, MIDI 60).
pub const ROOT_KEY: u8 = 60;
/// Velocity of a newly placed step (100 of 127).
pub const DEFAULT_VELOCITY: f32 = 100.0 / 127.0;
/// Most channels a project can have (the engine preallocates this many slots).
pub const MAX_CHANNELS: usize = 64;

/// Stable identity of a channel. Never reused within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelId(pub u32);

/// Stable identity of a pattern. Never reused within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PatternId(pub u32);

/// The four original drum sounds that ship with the program (generated, CC0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltInSample {
    /// Kick drum.
    Kick,
    /// Snare drum.
    Snare,
    /// Closed hi-hat.
    Hat,
    /// Hand clap.
    Clap,
}

impl BuiltInSample {
    /// All built-in samples, in browser order.
    pub const ALL: [Self; 4] = [Self::Kick, Self::Snare, Self::Hat, Self::Clap];

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Kick => "Gloom Kick",
            Self::Snare => "Gloom Snare",
            Self::Hat => "Gloom Hat",
            Self::Clap => "Gloom Clap",
        }
    }
}

/// Where a channel's sample comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SampleSource {
    /// A built-in sound.
    BuiltIn(BuiltInSample),
    /// An audio file on disk.
    File(PathBuf),
}

impl SampleSource {
    /// Short display name: the built-in name, or the file name.
    pub fn display_name(&self) -> String {
        match self {
            Self::BuiltIn(b) => b.name().to_owned(),
            Self::File(p) => p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
        }
    }
}

/// How a sampler voice treats the sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopMode {
    /// Play from start to end once; note length is ignored.
    #[default]
    OneShot,
    /// Repeat the start..end region while the note is held, then release.
    Loop,
}

/// Envelope times in milliseconds; sustain is a level from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Adsr {
    /// Linear rise from 0 to 1.
    pub attack_ms: f32,
    /// Fall from 1 to the sustain level (reaches it within 1 % after this time).
    pub decay_ms: f32,
    /// Level held while the note is down.
    pub sustain: f32,
    /// Fall to silence after note-off (within 1 % after this time).
    pub release_ms: f32,
}

impl Default for Adsr {
    fn default() -> Self {
        Self {
            attack_ms: 0.0,
            decay_ms: 0.0,
            sustain: 1.0,
            release_ms: 50.0,
        }
    }
}

/// Settings of a sampler channel.
#[derive(Debug, Clone, PartialEq)]
pub struct SamplerSettings {
    /// The sample, or none (the channel is silent).
    pub sample: Option<SampleSource>,
    /// Transposition in semitones (fractional values detune).
    pub pitch: f32,
    /// Start point as a fraction of the sample length.
    pub start: f32,
    /// End point as a fraction of the sample length (greater than `start`).
    pub end: f32,
    /// One-shot or loop.
    pub loop_mode: LoopMode,
    /// Amplitude envelope.
    pub adsr: Adsr,
}

impl Default for SamplerSettings {
    fn default() -> Self {
        Self {
            sample: None,
            pitch: 0.0,
            start: 0.0,
            end: 1.0,
            loop_mode: LoopMode::OneShot,
            adsr: Adsr::default(),
        }
    }
}

/// One row of the channel rack.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    /// Identity.
    pub id: ChannelId,
    /// Display name.
    pub name: String,
    /// Linear gain, 0 to [`Channel::MAX_VOLUME`].
    pub volume: f32,
    /// Pan from -1 (left) to 1 (right).
    pub pan: f32,
    /// Muted.
    pub mute: bool,
    /// Soloed. When any channel is soloed, only soloed channels sound.
    pub solo: bool,
    /// Instrument settings.
    pub sampler: SamplerSettings,
}

impl Channel {
    /// Highest channel gain (+6 dB).
    pub const MAX_VOLUME: f32 = 2.0;
    /// Default channel gain (-4 dB), so a few full-scale drums hitting together stay below
    /// 0 dBFS (there is no master limiter yet).
    pub const DEFAULT_VOLUME: f32 = 0.63;
}

/// A note in a pattern. The step sequencer writes notes of one step at [`ROOT_KEY`]; the piano
/// roll (Step 4) will edit the same data freely.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Note {
    /// Start, in ticks from the pattern start.
    pub start: i64,
    /// Length in ticks (at least 1).
    pub length: i64,
    /// MIDI key; [`ROOT_KEY`] plays at the original pitch.
    pub key: u8,
    /// Velocity from 0 to 1.
    pub velocity: f32,
}

/// A pattern: notes for any number of channels, played as a loop of `steps` 1/16 notes.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    /// Identity.
    pub id: PatternId,
    /// Display name.
    pub name: String,
    /// Length in steps: 16 or 32.
    pub steps: u16,
    /// Notes per channel, each list sorted by start.
    pub notes: BTreeMap<ChannelId, Vec<Note>>,
}

impl Pattern {
    /// Allowed pattern lengths in steps.
    pub const STEP_COUNTS: [u16; 2] = [16, 32];

    /// Length in ticks: the step-grid length, extended to whole bars (4/4) so that every note
    /// fits. Notes drawn past the grid in the piano roll make the pattern longer, as in other
    /// pattern-based DAWs.
    pub fn length_ticks(&self) -> i64 {
        const BAR: i64 = 16 * STEP_TICKS;
        let grid = i64::from(self.steps) * STEP_TICKS;
        let end = self
            .notes
            .values()
            .flat_map(|v| v.iter())
            .map(|n| n.start + n.length.max(1))
            .max()
            .unwrap_or(0);
        grid.max((end + BAR - 1).div_euclid(BAR) * BAR)
    }

    /// The note that the step sequencer shows at `step` for `channel`, if any.
    pub fn step_note(&self, channel: ChannelId, step: u16) -> Option<&Note> {
        let at = i64::from(step) * STEP_TICKS;
        self.notes
            .get(&channel)?
            .iter()
            .find(|n| n.start == at && n.key == ROOT_KEY)
    }

    /// Turns a step on (with [`DEFAULT_VELOCITY`]) or off. Returns the new state.
    pub fn toggle_step(&mut self, channel: ChannelId, step: u16) -> bool {
        let at = i64::from(step) * STEP_TICKS;
        let notes = self.notes.entry(channel).or_default();
        if let Some(i) = notes
            .iter()
            .position(|n| n.start == at && n.key == ROOT_KEY)
        {
            notes.remove(i);
            false
        } else {
            let i = notes.partition_point(|n| n.start <= at);
            notes.insert(
                i,
                Note {
                    start: at,
                    length: STEP_TICKS,
                    key: ROOT_KEY,
                    velocity: DEFAULT_VELOCITY,
                },
            );
            true
        }
    }

    /// Sets the velocity of the note at `step` (clamped to 0..=1). No-op if the step is off.
    pub fn set_step_velocity(&mut self, channel: ChannelId, step: u16, velocity: f32) {
        let at = i64::from(step) * STEP_TICKS;
        if let Some(n) = self
            .notes
            .get_mut(&channel)
            .and_then(|v| v.iter_mut().find(|n| n.start == at && n.key == ROOT_KEY))
        {
            n.velocity = velocity.clamp(0.0, 1.0);
        }
    }

    /// Notes of `channel` (empty if none).
    pub fn channel_notes(&self, channel: ChannelId) -> &[Note] {
        self.notes.get(&channel).map_or(&[], Vec::as_slice)
    }
}

/// The whole document.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    /// Channel rack rows, in display order. At most [`MAX_CHANNELS`].
    pub channels: Vec<Channel>,
    /// Patterns, in creation order (never empty).
    pub patterns: Vec<Pattern>,
    /// The pattern being edited and played.
    pub current_pattern: PatternId,
    /// Channel-rack swing from 0 (straight) to 1: delays every second 1/16 step by up to half
    /// a step. About 0.67 gives a triplet feel.
    pub swing: f32,
    next_id: u32,
}

impl Default for Project {
    fn default() -> Self {
        Self::empty()
    }
}

impl Project {
    /// A project with one empty pattern and no channels.
    pub fn empty() -> Self {
        let mut p = Self {
            channels: Vec::new(),
            patterns: Vec::new(),
            current_pattern: PatternId(0),
            swing: 0.0,
            next_id: 1,
        };
        let id = p.new_pattern();
        p.current_pattern = id;
        p
    }

    /// The starter project: the four built-in drums and a basic beat in "Pattern 1".
    pub fn demo() -> Self {
        let mut p = Self::empty();
        let [kick, snare, hat, clap] = BuiltInSample::ALL.map(|b| {
            let name = match b {
                BuiltInSample::Kick => "Kick",
                BuiltInSample::Snare => "Snare",
                BuiltInSample::Hat => "Hat",
                BuiltInSample::Clap => "Clap",
            };
            p.add_channel(name, Some(SampleSource::BuiltIn(b)))
                .expect("demo fits")
        });
        let pat = p.current_pattern_mut();
        for s in [0, 4, 8, 12] {
            pat.toggle_step(kick, s);
        }
        for s in [4, 12] {
            pat.toggle_step(snare, s);
        }
        for s in (2..16).step_by(4) {
            pat.toggle_step(hat, s);
        }
        pat.toggle_step(clap, 12);
        pat.set_step_velocity(clap, 12, 0.6);
        p
    }

    fn next_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Adds a sampler channel at the bottom of the rack. `None` when the rack is full.
    pub fn add_channel(&mut self, name: &str, sample: Option<SampleSource>) -> Option<ChannelId> {
        if self.channels.len() >= MAX_CHANNELS {
            return None;
        }
        let id = ChannelId(self.next_id());
        self.channels.push(Channel {
            id,
            name: name.to_owned(),
            volume: Channel::DEFAULT_VOLUME,
            pan: 0.0,
            mute: false,
            solo: false,
            sampler: SamplerSettings {
                sample,
                ..SamplerSettings::default()
            },
        });
        Some(id)
    }

    /// Removes a channel and its notes from every pattern.
    pub fn remove_channel(&mut self, id: ChannelId) {
        self.channels.retain(|c| c.id != id);
        for p in &mut self.patterns {
            p.notes.remove(&id);
        }
    }

    /// Index of a channel in the rack.
    pub fn channel_index(&self, id: ChannelId) -> Option<usize> {
        self.channels.iter().position(|c| c.id == id)
    }

    /// True if the channel at `index` should be silent because of mute or another channel's
    /// solo.
    pub fn is_silenced(&self, index: usize) -> bool {
        let Some(c) = self.channels.get(index) else {
            return true;
        };
        let any_solo = self.channels.iter().any(|c| c.solo);
        c.mute || (any_solo && !c.solo)
    }

    /// Creates an empty 16-step pattern named "Pattern N" and returns its id (does not select it).
    pub fn new_pattern(&mut self) -> PatternId {
        let id = PatternId(self.next_id());
        let name = self.unique_pattern_name("Pattern");
        self.patterns.push(Pattern {
            id,
            name,
            steps: 16,
            notes: BTreeMap::new(),
        });
        id
    }

    /// Copies a pattern (notes, length) under a new name, inserted after the original.
    pub fn clone_pattern(&mut self, id: PatternId) -> Option<PatternId> {
        let i = self.patterns.iter().position(|p| p.id == id)?;
        let new_id = PatternId(self.next_id());
        let name = self.unique_pattern_name(&format!("{} copy", self.patterns[i].name));
        let copy = Pattern {
            id: new_id,
            name,
            ..self.patterns[i].clone()
        };
        self.patterns.insert(i + 1, copy);
        Some(new_id)
    }

    /// Renames a pattern. Empty names are ignored.
    pub fn rename_pattern(&mut self, id: PatternId, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if let Some(p) = self.patterns.iter_mut().find(|p| p.id == id) {
            name.clone_into(&mut p.name);
        }
    }

    /// Makes `id` the current pattern, if it exists.
    pub fn select_pattern(&mut self, id: PatternId) {
        if self.patterns.iter().any(|p| p.id == id) {
            self.current_pattern = id;
        }
    }

    /// The current pattern.
    pub fn current_pattern(&self) -> &Pattern {
        self.patterns
            .iter()
            .find(|p| p.id == self.current_pattern)
            .unwrap_or(&self.patterns[0])
    }

    /// The current pattern, mutably.
    pub fn current_pattern_mut(&mut self) -> &mut Pattern {
        let i = self
            .patterns
            .iter()
            .position(|p| p.id == self.current_pattern)
            .unwrap_or(0);
        &mut self.patterns[i]
    }

    fn unique_pattern_name(&self, base: &str) -> String {
        let taken = |n: &str| self.patterns.iter().any(|p| p.name == n);
        if base == "Pattern" {
            return (1..)
                .map(|k| format!("Pattern {k}"))
                .find(|n| !taken(n))
                .unwrap_or_default();
        }
        if !taken(base) {
            return base.to_owned();
        }
        (2..)
            .map(|k| format!("{base} {k}"))
            .find(|n| !taken(n))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_has_four_channels_and_a_beat() {
        let p = Project::demo();
        assert_eq!(p.channels.len(), 4);
        let pat = p.current_pattern();
        assert_eq!(pat.channel_notes(p.channels[0].id).len(), 4);
        assert!(pat.step_note(p.channels[1].id, 4).is_some());
        assert!(pat.step_note(p.channels[1].id, 5).is_none());
    }

    #[test]
    fn toggling_keeps_notes_sorted() {
        let mut p = Project::empty();
        let c = p.add_channel("c", None).unwrap();
        let pat = p.current_pattern_mut();
        for s in [9, 3, 15, 0] {
            assert!(pat.toggle_step(c, s));
        }
        let starts: Vec<_> = pat.channel_notes(c).iter().map(|n| n.start).collect();
        assert_eq!(starts, vec![0, 3 * 240, 9 * 240, 15 * 240]);
        assert!(!pat.toggle_step(c, 9));
        assert_eq!(pat.channel_notes(c).len(), 3);
    }

    #[test]
    fn patterns_clone_rename_select() {
        let mut p = Project::demo();
        let first = p.current_pattern;
        let copy = p.clone_pattern(first).unwrap();
        assert_eq!(p.patterns[1].name, "Pattern 1 copy");
        assert_eq!(p.patterns[1].notes, p.patterns[0].notes);
        let fresh = p.new_pattern();
        assert_eq!(p.patterns[2].name, "Pattern 2");
        p.rename_pattern(copy, "  Fill ");
        assert_eq!(p.patterns[1].name, "Fill");
        p.rename_pattern(copy, "   ");
        assert_eq!(p.patterns[1].name, "Fill");
        p.select_pattern(fresh);
        assert!(p.current_pattern().notes.is_empty());
        p.select_pattern(PatternId(999));
        assert_eq!(p.current_pattern, fresh);
        // Clones of clones get distinct names.
        p.clone_pattern(first).unwrap();
        p.clone_pattern(first).unwrap();
        let names: Vec<_> = p.patterns.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"Pattern 1 copy 2"), "{names:?}");
    }

    #[test]
    fn length_grows_to_whole_bars_around_notes() {
        let mut p = Project::empty();
        let c = p.add_channel("c", None).unwrap();
        let pat = p.current_pattern_mut();
        assert_eq!(pat.length_ticks(), 16 * STEP_TICKS);
        pat.notes.entry(c).or_default().push(Note {
            start: 16 * STEP_TICKS + 100,
            length: 240,
            key: 64,
            velocity: 0.5,
        });
        assert_eq!(pat.length_ticks(), 32 * STEP_TICKS);
        pat.steps = 32;
        assert_eq!(pat.length_ticks(), 32 * STEP_TICKS);
    }

    #[test]
    fn solo_silences_others() {
        let mut p = Project::demo();
        assert!(!p.is_silenced(0));
        p.channels[1].solo = true;
        assert!(p.is_silenced(0));
        assert!(!p.is_silenced(1));
        p.channels[1].mute = true;
        assert!(p.is_silenced(1));
    }

    #[test]
    fn removing_a_channel_removes_its_notes() {
        let mut p = Project::demo();
        let id = p.channels[0].id;
        p.remove_channel(id);
        assert_eq!(p.channels.len(), 3);
        assert!(!p.current_pattern().notes.contains_key(&id));
    }
}
