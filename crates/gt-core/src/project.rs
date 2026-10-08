//! The project document: channels, patterns and their notes (ARCHITECTURE.md §6).
//!
//! Plain data, edited on the UI thread only. The engine never sees these types; `gt-engine`
//! compiles them into flat, real-time-friendly snapshots.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::effects::{EffectKind, EffectSlot};
use crate::mixer::{Mixer, StripKind, FIRST_SEND, FX_SLOTS, INSERTS, MASTER};
use crate::modulation::{ModSourceKind, Modulator, ModulatorId, MAX_MODULATORS};
use crate::params::ParamId;
use crate::playlist::{AutoPoint, Automation, ClipKind, Curve, Playlist};
use crate::synth::{SynthParam, SynthPatch};
use crate::time::{TempoMap, TimeSigMap, PPQ};

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
    /// What makes the sound.
    pub instrument: Instrument,
    /// Mixer strip the channel plays into ([`crate::MASTER`] or an insert).
    pub insert: usize,
}

/// The sound source of a channel.
#[derive(Debug, Clone, PartialEq)]
pub enum Instrument {
    /// Plays a sample.
    Sampler(SamplerSettings),
    /// Gloom Synth.
    Synth(Box<SynthPatch>),
}

impl Channel {
    /// Sampler settings, if this is a sampler channel.
    pub fn sampler(&self) -> Option<&SamplerSettings> {
        match &self.instrument {
            Instrument::Sampler(s) => Some(s),
            Instrument::Synth(_) => None,
        }
    }

    /// Mutable sampler settings, if this is a sampler channel.
    pub fn sampler_mut(&mut self) -> Option<&mut SamplerSettings> {
        match &mut self.instrument {
            Instrument::Sampler(s) => Some(s),
            Instrument::Synth(_) => None,
        }
    }

    /// The synth patch, if this is a synth channel.
    pub fn synth(&self) -> Option<&SynthPatch> {
        match &self.instrument {
            Instrument::Synth(p) => Some(p),
            Instrument::Sampler(_) => None,
        }
    }

    /// Mutable synth patch, if this is a synth channel.
    pub fn synth_mut(&mut self) -> Option<&mut SynthPatch> {
        match &mut self.instrument {
            Instrument::Synth(p) => Some(p),
            Instrument::Sampler(_) => None,
        }
    }

    /// The sample this channel plays, if any.
    pub fn sample(&self) -> Option<&SampleSource> {
        self.sampler().and_then(|s| s.sample.as_ref())
    }
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
    /// Master, inserts and send buses.
    pub mixer: Mixer,
    /// Tracks, clips and markers of the arrangement.
    pub playlist: Playlist,
    /// Tempo over the timeline.
    pub tempo: TempoMap,
    /// Time signatures over the timeline.
    pub signatures: TimeSigMap,
    /// LFOs and envelope followers on parameters, at most [`MAX_MODULATORS`].
    pub modulators: Vec<Modulator>,
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
            mixer: Mixer::new(),
            playlist: Playlist::new(),
            tempo: TempoMap::default(),
            signatures: TimeSigMap::default(),
            modulators: Vec::new(),
            next_id: 1,
        };
        let id = p.new_pattern();
        p.current_pattern = id;
        p
    }

    /// The starter project: the four built-in drums with a basic beat and a Gloom Synth bass line
    /// in "Pattern 1", two variations ("Intro", "Break") and a 12-bar arrangement of them.
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
        // A one-bar bass line on Gloom Synth: (step, length in steps, key, velocity).
        let bass_patch = SynthPatch::factory()
            .into_iter()
            .find(|x| x.name == "Gloom Bass")
            .unwrap_or_default();
        let bass = p
            .add_synth_channel("Gloom Bass", bass_patch)
            .expect("demo fits");
        let line: [(i64, i64, u8, f32); 6] = [
            (0, 2, 45, 0.9),
            (3, 1, 45, 0.6),
            (6, 2, 48, 0.8),
            (8, 2, 43, 0.9),
            (11, 1, 43, 0.6),
            (14, 2, 40, 0.8),
        ];
        p.current_pattern_mut().notes.insert(
            bass,
            line.iter()
                .map(|&(step, len, key, velocity)| Note {
                    start: step * STEP_TICKS,
                    length: len * STEP_TICKS,
                    key,
                    velocity,
                })
                .collect(),
        );
        p.demo_mix(bass);
        p.demo_arrangement(bass);
        p.demo_modulation();
        p
    }

    /// The hat drifts across the stereo field with a one-bar LFO, and the reverb bus ducks
    /// under the kick through an envelope follower.
    fn demo_modulation(&mut self) {
        let insert_of = |p: &Self, name: &str| {
            p.channels
                .iter()
                .find(|c| c.name == name)
                .map_or(MASTER, |c| c.insert)
        };
        let (hat, kick) = (insert_of(self, "Hat"), insert_of(self, "Kick"));
        if let Some(id) = self.add_modulator(
            ParamId::strip_pan(hat),
            ModSourceKind::Lfo {
                shape: crate::modulation::LfoShape::Sine,
                rate: crate::modulation::LfoRate::Sync(2),
                phase: 0.0,
            },
        ) {
            self.modulator_mut(id).expect("just added").amount = 0.3;
        }
        if let Some(id) = self.add_modulator(
            ParamId::strip_volume(FIRST_SEND),
            ModSourceKind::default_follower(kick),
        ) {
            self.modulator_mut(id).expect("just added").amount = -0.35;
        }
    }

    /// Attaches a modulator (amount 0.5, enabled) to `target`. `None` when the project already
    /// has [`MAX_MODULATORS`].
    pub fn add_modulator(&mut self, target: ParamId, source: ModSourceKind) -> Option<ModulatorId> {
        if self.modulators.len() >= MAX_MODULATORS {
            return None;
        }
        let id = ModulatorId(self.next_id());
        self.modulators.push(Modulator {
            id,
            target,
            source,
            amount: 0.5,
            enabled: true,
        });
        Some(id)
    }

    /// The modulator with this id.
    pub fn modulator_mut(&mut self, id: ModulatorId) -> Option<&mut Modulator> {
        self.modulators.iter_mut().find(|m| m.id == id)
    }

    /// Removes a modulator.
    pub fn remove_modulator(&mut self, id: ModulatorId) {
        self.modulators.retain(|m| m.id != id);
    }

    /// "Intro" (kick and hat) and "Break" (bass and hat) patterns, and an arrangement: intro,
    /// four bars of the full beat, the break with the delay bus swelling, the beat again.
    fn demo_arrangement(&mut self, bass: ChannelId) {
        const BAR: i64 = 4 * PPQ;
        let main = self.current_pattern;
        let ids: Vec<ChannelId> = self.channels.iter().map(|c| c.id).collect();
        let (kick, hat) = (ids[0], ids[2]);
        let keep = |p: &mut Self, id: PatternId, name: &str, chans: &[ChannelId]| {
            p.rename_pattern(id, name);
            if let Some(pat) = p.patterns.iter_mut().find(|x| x.id == id) {
                pat.notes.retain(|c, _| chans.contains(c));
            }
        };
        // Each clone lands right after the main pattern: create the break first.
        let brk = self.clone_pattern(main).expect("exists");
        keep(self, brk, "Break", &[bass, hat]);
        let intro = self.clone_pattern(main).expect("exists");
        keep(self, intro, "Intro", &[kick, hat]);
        self.current_pattern = main;

        let pl = &mut self.playlist;
        let names = ["Beat", "Intro", "Break", "Delay swell"];
        for (t, n) in pl.tracks.iter_mut().zip(names) {
            n.clone_into(&mut t.name);
        }
        let (t_main, t_intro, t_break, t_auto) = (
            pl.tracks[0].id,
            pl.tracks[1].id,
            pl.tracks[2].id,
            pl.tracks[3].id,
        );
        pl.add_clip(t_intro, 0, 2 * BAR, ClipKind::Pattern(intro));
        pl.add_clip(t_main, 2 * BAR, 4 * BAR, ClipKind::Pattern(main));
        pl.add_clip(t_break, 6 * BAR, 2 * BAR, ClipKind::Pattern(brk));
        pl.add_clip(t_main, 8 * BAR, 4 * BAR, ClipKind::Pattern(main));
        let delay = ParamId::strip_volume(FIRST_SEND + 1);
        let cutoff = ParamId::Synth {
            channel: bass,
            param: SynthParam::Cutoff,
        };
        let rest = delay.normalized(self);
        let hz = |f: f32| cutoff.info().to_normalized(f);
        let pl = &mut self.playlist;
        pl.add_clip(
            t_auto,
            6 * BAR,
            2 * BAR,
            ClipKind::Automation(Automation {
                target: delay,
                points: vec![
                    AutoPoint {
                        at: 0,
                        value: rest,
                        curve: Curve::Bezier(-0.6),
                    },
                    AutoPoint::new(2 * BAR - PPQ, 1.0),
                    AutoPoint::new(2 * BAR, rest),
                ],
            }),
        );
        // The bass filter opens over the drop, then snaps shut.
        pl.tracks[4].name = "Bass filter".to_owned();
        let t_filter = pl.tracks[4].id;
        pl.add_clip(
            t_filter,
            2 * BAR,
            4 * BAR,
            ClipKind::Automation(Automation {
                target: cutoff,
                points: vec![
                    AutoPoint {
                        at: 0,
                        value: hz(500.0),
                        curve: Curve::Smooth,
                    },
                    AutoPoint {
                        at: 3 * BAR,
                        value: hz(4000.0),
                        curve: Curve::Bezier(0.7),
                    },
                    AutoPoint::new(4 * BAR, hz(900.0)),
                ],
            }),
        );
        pl.add_marker(0, "Intro");
        pl.add_marker(2 * BAR, "Drop");
        pl.add_marker(6 * BAR, "Break");
    }

    /// The pattern with this id.
    pub fn pattern(&self, id: PatternId) -> Option<&Pattern> {
        self.patterns.iter().find(|p| p.id == id)
    }

    /// Mixer settings for the demo: each channel on its own insert (from `add_instrument`), a
    /// reverb send and a delay send, the bass ducked by the kick through a sidechain compressor,
    /// and a limiter on the master.
    fn demo_mix(&mut self, bass: ChannelId) {
        let insert_of = |p: &Self, name: &str| {
            p.channels
                .iter()
                .find(|c| c.name == name)
                .map_or(MASTER, |c| c.insert)
        };
        let (kick, snare, hat, clap) = (
            insert_of(self, "Kick"),
            insert_of(self, "Snare"),
            insert_of(self, "Hat"),
            insert_of(self, "Clap"),
        );
        let bass = self
            .channel_index(bass)
            .map_or(MASTER, |i| self.channels[i].insert);
        let reverb = FIRST_SEND;
        let delay = FIRST_SEND + 1;
        let m = &mut self.mixer;
        m.strips[reverb].name = "Reverb".to_owned();
        m.strips[reverb].slots[0] = Some(
            EffectSlot::new(EffectKind::Reverb)
                .with("decay", 1.8)
                .with("mix", 1.0)
                .with("damping", 5000.0),
        );
        m.strips[delay].name = "Delay".to_owned();
        m.strips[delay].slots[0] = Some(
            EffectSlot::new(EffectKind::Delay)
                .with("time", 6.0)
                .with("mix", 1.0)
                .with("feedback", 0.35),
        );
        m.strips[delay].volume = 0.7;
        m.strips[snare].sends[0] = 0.35;
        m.strips[clap].sends[0] = 0.45;
        m.strips[hat].sends[1] = 0.25;
        m.strips[hat].pan = 0.2;
        m.strips[bass].slots[0] = Some(
            EffectSlot::new(EffectKind::Eq)
                .with("b1.type", 4.0)
                .with("b1.freq", 35.0)
                .with("b4.freq", 400.0)
                .with("b4.gain", -3.0),
        );
        m.strips[bass].slots[1] = Some(
            EffectSlot::new(EffectKind::Compressor)
                .with("sidechain", 1.0)
                .with("threshold", -30.0)
                .with("ratio", 6.0)
                .with("attack", 1.0)
                .with("release", 140.0),
        );
        m.strips[bass].sidechain = Some(kick);
        m.strips[MASTER].slots[FX_SLOTS - 1] =
            Some(EffectSlot::new(EffectKind::Limiter).with("ceiling", -0.3));
    }

    fn next_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Adds a sampler channel at the bottom of the rack. `None` when the rack is full.
    pub fn add_channel(&mut self, name: &str, sample: Option<SampleSource>) -> Option<ChannelId> {
        self.add_instrument(
            name,
            Instrument::Sampler(SamplerSettings {
                sample,
                ..SamplerSettings::default()
            }),
        )
    }

    /// Adds a Gloom Synth channel at the bottom of the rack. `None` when the rack is full.
    pub fn add_synth_channel(&mut self, name: &str, patch: SynthPatch) -> Option<ChannelId> {
        self.add_instrument(name, Instrument::Synth(Box::new(patch)))
    }

    fn add_instrument(&mut self, name: &str, instrument: Instrument) -> Option<ChannelId> {
        if self.channels.len() >= MAX_CHANNELS {
            return None;
        }
        let id = ChannelId(self.next_id());
        // Each new channel gets the first insert no other channel uses, named after it.
        let insert = (1..=INSERTS)
            .find(|&i| self.channels.iter().all(|c| c.insert != i))
            .unwrap_or(MASTER);
        if insert != MASTER {
            let strip = &mut self.mixer.strips[insert];
            if strip.name == StripKind::Insert(insert).default_name() {
                strip.name = name.to_owned();
            }
        }
        self.channels.push(Channel {
            id,
            name: name.to_owned(),
            volume: Channel::DEFAULT_VOLUME,
            pan: 0.0,
            mute: false,
            solo: false,
            instrument,
            insert,
        });
        Some(id)
    }

    /// Removes a channel, its notes from every pattern and the modulators on its parameters.
    pub fn remove_channel(&mut self, id: ChannelId) {
        self.channels.retain(|c| c.id != id);
        self.modulators.retain(|m| m.target.channel() != Some(id));
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
    fn demo_mix_routes_each_channel_to_its_own_insert() {
        let p = Project::demo();
        let inserts: Vec<usize> = p.channels.iter().map(|c| c.insert).collect();
        assert_eq!(inserts, vec![1, 2, 3, 4, 5]);
        assert_eq!(p.mixer.strips[1].name, "Kick");
        assert_eq!(p.mixer.strips[5].sidechain, Some(1));
        assert!(p.mixer.processing_order().is_some());
        let mut copy = p.mixer.clone();
        copy.sanitize();
        assert_eq!(copy, p.mixer);
    }

    #[test]
    fn demo_has_drums_a_beat_and_a_synth_bass() {
        let p = Project::demo();
        assert_eq!(p.channels.len(), 5);
        assert!(p.channels[..4].iter().all(|c| c.sampler().is_some()));
        assert_eq!(
            p.channels[4].synth().map(|s| s.name.as_str()),
            Some("Gloom Bass")
        );
        assert_eq!(p.current_pattern().channel_notes(p.channels[4].id).len(), 6);
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
        let names = |p: &Project| {
            p.patterns
                .iter()
                .map(|x| x.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&p), ["Pattern 1", "Intro", "Break"]);
        let first = p.current_pattern;
        let copy = p.clone_pattern(first).unwrap();
        assert_eq!(p.patterns[1].name, "Pattern 1 copy");
        assert_eq!(p.patterns[1].notes, p.patterns[0].notes);
        let fresh = p.new_pattern();
        assert_eq!(p.patterns.last().unwrap().name, "Pattern 2");
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
        assert!(
            names(&p).contains(&"Pattern 1 copy 2".to_owned()),
            "{:?}",
            names(&p)
        );
    }

    #[test]
    fn demo_arrangement_uses_every_pattern() {
        let p = Project::demo();
        let pl = &p.playlist;
        assert_eq!(pl.song_end(), 12 * 4 * PPQ);
        for pat in &p.patterns {
            assert!(
                pl.clips.iter().any(|c| c.kind == ClipKind::Pattern(pat.id)),
                "{}",
                pat.name
            );
        }
        let intro = &p.patterns[1];
        assert_eq!(intro.notes.len(), 2, "kick and hat");
        assert_eq!(pl.markers.len(), 3);
        assert!(pl
            .clips
            .iter()
            .any(|c| matches!(&c.kind, ClipKind::Automation(a)
            if a.target.is_valid(&p))));
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
        assert_eq!(p.channels.len(), 4);
        assert!(!p.current_pattern().notes.contains_key(&id));
    }
}
