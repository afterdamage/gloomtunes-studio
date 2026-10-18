//! The project document: channels, patterns and their notes (ARCHITECTURE.md §6).
//!
//! Plain data, edited on the UI thread only. The engine never sees these types; `gt-engine`
//! compiles them into flat, real-time-friendly snapshots.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::effects::{EffectKind, EffectSlot};
use crate::midi::ControlPoint;
use crate::mixer::{Mixer, StripKind, FX_SLOTS, MASTER};
use crate::modulation::{ModSourceKind, Modulator, ModulatorId, MAX_MODULATORS};
use crate::params::ParamId;
use crate::playlist::{AutoPoint, Automation, ClipKind, Curve, Playlist};
use crate::plugin::{PluginInstanceId, PluginOwner, PluginRef};
use crate::synth::{SynthParam, SynthPatch};
use crate::time::{TempoMap, TimeSigMap, PPQ};

/// Ticks per step of the channel rack: a 1/16 note.
pub const STEP_TICKS: i64 = PPQ / 4;
/// MIDI key that plays a sample at its original pitch (C5 in FL-style naming, MIDI 60).
pub const ROOT_KEY: u8 = 60;

use crate::sampler::{Interp, KeyZone, SamplerFilter, SamplerLfo};
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
    /// Interpolation quality (Prompt 23).
    pub interp: Interp,
    /// Play the region backwards.
    pub reverse: bool,
    /// Voice filter and its envelope.
    pub filter: SamplerFilter,
    /// The two LFOs.
    pub lfos: [SamplerLfo; 2],
    /// Key at which `sample` plays at its recorded pitch.
    pub root_key: u8,
    /// Keys `sample` answers.
    pub key_lo: u8,
    /// Highest key `sample` answers.
    pub key_hi: u8,
    /// Multi-sample zones; when there are any they replace `sample`.
    pub zones: Vec<KeyZone>,
    /// Zones answering the same key and velocity take turns instead of layering.
    pub round_robin: bool,
    /// One voice at a time.
    pub mono: bool,
    /// Mono only: a key pressed while another is held glides without restarting.
    pub legato: bool,
    /// Glide time of mono notes, ms (0: none).
    pub glide_ms: f32,
    /// Most voices sounding at once (1 to 16).
    pub voices: u8,
    /// Time-stretch mode: fit `sample` to this many beats at the song tempo, keeping its pitch
    /// (0: off; plays resampled).
    pub stretch_beats: f32,
    /// Beat slicer: length of the sliced loop in beats (0: not sliced). Its zones are the
    /// slices, on keys from [`crate::sampler::FIRST_SLICE_KEY`] up.
    pub slice_beats: f32,
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
            interp: Interp::default(),
            reverse: false,
            filter: SamplerFilter::default(),
            lfos: [SamplerLfo::default(); 2],
            root_key: ROOT_KEY,
            key_lo: 0,
            key_hi: 127,
            zones: Vec::new(),
            round_robin: false,
            mono: false,
            legato: false,
            glide_ms: 0.0,
            voices: SamplerSettings::MAX_VOICES,
            stretch_beats: 0.0,
            slice_beats: 0.0,
        }
    }
}

impl SamplerSettings {
    /// Voice pool size of a sampler channel.
    pub const MAX_VOICES: u8 = 16;
    /// Most zones in a multi-sample instrument.
    pub const MAX_ZONES: usize = 256;

    /// Every sample the channel uses: its main sample and its zones' samples.
    pub fn sources(&self) -> impl Iterator<Item = &SampleSource> {
        self.sample
            .iter()
            .chain(self.zones.iter().map(|z| &z.sample))
    }

    /// Every sample the channel uses, mutably.
    pub fn sources_mut(&mut self) -> impl Iterator<Item = &mut SampleSource> {
        self.sample
            .iter_mut()
            .chain(self.zones.iter_mut().map(|z| &mut z.sample))
    }

    /// Clamp every setting into range, replacing non-finite values with defaults.
    pub fn sanitize(&mut self) {
        let fin =
            |x: f32, lo: f32, hi: f32, d: f32| if x.is_finite() { x.clamp(lo, hi) } else { d };
        let s = self;
        s.pitch = fin(s.pitch, -48.0, 48.0, 0.0);
        s.start = fin(s.start, 0.0, 0.999, 0.0);
        s.end = fin(s.end, s.start + 0.001, 1.0, 1.0);
        let a = &mut s.adsr;
        a.attack_ms = fin(a.attack_ms, 0.0, 10_000.0, 0.0);
        a.decay_ms = fin(a.decay_ms, 0.0, 10_000.0, 0.0);
        a.sustain = fin(a.sustain, 0.0, 1.0, 1.0);
        a.release_ms = fin(a.release_ms, 0.0, 20_000.0, 50.0);
        let f = &mut s.filter;
        f.cutoff = fin(f.cutoff, 20.0, 20_000.0, 8000.0);
        f.resonance = fin(f.resonance, 0.0, 1.0, 0.2);
        f.env_amount = fin(f.env_amount, -8.0, 8.0, 0.0);
        let a = &mut f.env;
        a.attack_ms = fin(a.attack_ms, 0.0, 10_000.0, 0.0);
        a.decay_ms = fin(a.decay_ms, 0.0, 10_000.0, 300.0);
        a.sustain = fin(a.sustain, 0.0, 1.0, 0.0);
        a.release_ms = fin(a.release_ms, 0.0, 20_000.0, 200.0);
        for l in &mut s.lfos {
            l.rate = fin(l.rate, 0.01, 50.0, 4.0);
            l.amount = fin(l.amount, 0.0, 1.0, 0.25);
        }
        s.root_key = s.root_key.min(127);
        s.key_lo = s.key_lo.min(127);
        s.key_hi = s.key_hi.clamp(s.key_lo, 127);
        s.zones.truncate(SamplerSettings::MAX_ZONES);
        s.zones.iter_mut().for_each(KeyZone::sanitize);
        s.glide_ms = fin(s.glide_ms, 0.0, 5000.0, 0.0);
        s.voices = s.voices.clamp(1, SamplerSettings::MAX_VOICES);
        s.stretch_beats = fin(s.stretch_beats, 0.0, 256.0, 0.0);
        s.slice_beats = fin(s.slice_beats, 0.0, 256.0, 0.0);
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
    /// Pitch bend range in semitones, each way (Prompt 16).
    pub bend_range: f32,
    /// Swing of this channel's steps, 0 to 1; `None` follows the rack's swing (Prompt 27).
    pub swing: Option<f32>,
    /// Choke group, 1 to [`Channel::CHOKE_GROUPS`]: a note on this channel cuts the other
    /// channels of the group. 0 is none.
    pub choke: u8,
    /// Rack filter group ("" is none): the rack can show one group at a time.
    pub group: String,
    /// Root key, key range, mono and voice limit of non-sampler instruments.
    pub keys: crate::rack::KeyLimits,
}

/// The sound source of a channel.
#[derive(Debug, Clone, PartialEq)]
pub enum Instrument {
    /// Plays a sample.
    Sampler(SamplerSettings),
    /// Gloom Synth.
    Synth(Box<SynthPatch>),
    /// A third-party instrument plugin.
    Plugin(Box<PluginRef>),
    /// Drum pads (Prompt 23).
    Drums(Box<crate::pads::DrumKit>),
    /// Plays other channels (Prompt 27); makes no sound of its own.
    Layer(crate::rack::LayerSettings),
    /// Plays a sound pack's preset with macro knobs (Prompt 28).
    Player(Box<crate::player::Player>),
}

impl Channel {
    /// Sampler settings, if this is a sampler channel.
    pub fn sampler(&self) -> Option<&SamplerSettings> {
        match &self.instrument {
            Instrument::Sampler(s) => Some(s),
            _ => None,
        }
    }

    /// Mutable sampler settings, if this is a sampler channel.
    pub fn sampler_mut(&mut self) -> Option<&mut SamplerSettings> {
        match &mut self.instrument {
            Instrument::Sampler(s) => Some(s),
            _ => None,
        }
    }

    /// The synth patch, if this is a synth channel.
    pub fn synth(&self) -> Option<&SynthPatch> {
        match &self.instrument {
            Instrument::Synth(p) => Some(p),
            _ => None,
        }
    }

    /// Mutable synth patch, if this is a synth channel.
    pub fn synth_mut(&mut self) -> Option<&mut SynthPatch> {
        match &mut self.instrument {
            Instrument::Synth(p) => Some(p),
            _ => None,
        }
    }

    /// The drum kit, if this is a drum pad channel.
    pub fn drums(&self) -> Option<&crate::pads::DrumKit> {
        match &self.instrument {
            Instrument::Drums(k) => Some(k),
            _ => None,
        }
    }

    /// Mutable drum kit, if this is a drum pad channel.
    pub fn drums_mut(&mut self) -> Option<&mut crate::pads::DrumKit> {
        match &mut self.instrument {
            Instrument::Drums(k) => Some(k),
            _ => None,
        }
    }

    /// The preset player, if this is a player channel.
    pub fn player(&self) -> Option<&crate::player::Player> {
        match &self.instrument {
            Instrument::Player(p) => Some(p),
            _ => None,
        }
    }

    /// Mutable preset player, if this is a player channel.
    pub fn player_mut(&mut self) -> Option<&mut crate::player::Player> {
        match &mut self.instrument {
            Instrument::Player(p) => Some(p),
            _ => None,
        }
    }

    /// The instrument plugin, if this is a plugin channel.
    pub fn plugin(&self) -> Option<&PluginRef> {
        match &self.instrument {
            Instrument::Plugin(p) => Some(p),
            _ => None,
        }
    }

    /// Mutable instrument plugin, if this is a plugin channel.
    pub fn plugin_mut(&mut self) -> Option<&mut PluginRef> {
        match &mut self.instrument {
            Instrument::Plugin(p) => Some(p),
            _ => None,
        }
    }

    /// The sample this channel plays, if any.
    pub fn sample(&self) -> Option<&SampleSource> {
        self.sampler().and_then(|s| s.sample.as_ref())
    }

    /// Every sample the channel plays (a sampler's sample and key zones, a kit's pads).
    pub fn samples(&self) -> impl Iterator<Item = &SampleSource> {
        self.sampler()
            .into_iter()
            .flat_map(SamplerSettings::sources)
            .chain(
                self.drums()
                    .into_iter()
                    .flat_map(crate::pads::DrumKit::sources),
            )
            .chain(
                self.synth()
                    .into_iter()
                    .flat_map(|p| p.tables.iter().flatten()),
            )
            .chain(self.player().into_iter().flat_map(|p| p.base.sources()))
    }

    /// Every sample the channel plays, mutably.
    pub fn samples_mut(&mut self) -> Box<dyn Iterator<Item = &mut SampleSource> + '_> {
        match &mut self.instrument {
            Instrument::Sampler(s) => Box::new(s.sources_mut()),
            Instrument::Drums(k) => Box::new(k.sources_mut()),
            Instrument::Synth(p) => Box::new(p.tables.iter_mut().flatten()),
            Instrument::Player(p) => Box::new(p.base.sources_mut()),
            Instrument::Plugin(_) | Instrument::Layer(_) => Box::new(std::iter::empty()),
        }
    }

    /// The layer settings, if this is a layer channel.
    pub fn layer(&self) -> Option<&crate::rack::LayerSettings> {
        match &self.instrument {
            Instrument::Layer(l) => Some(l),
            _ => None,
        }
    }

    /// Mutable layer settings, if this is a layer channel.
    pub fn layer_mut(&mut self) -> Option<&mut crate::rack::LayerSettings> {
        match &mut self.instrument {
            Instrument::Layer(l) => Some(l),
            _ => None,
        }
    }

    /// The swing this channel plays with, given the rack's.
    pub fn swing_or(&self, rack: f32) -> f32 {
        self.swing.unwrap_or(rack).clamp(0.0, 1.0)
    }
}

impl Channel {
    /// Highest channel gain (+6 dB).
    pub const MAX_VOLUME: f32 = 2.0;
    /// Default channel gain (-4 dB), so a few full-scale drums hitting together stay below
    /// 0 dBFS (there is no master limiter yet).
    pub const DEFAULT_VOLUME: f32 = 0.63;
    /// Default pitch bend range: a whole tone, the MIDI convention.
    pub const DEFAULT_BEND: f32 = 2.0;
    /// Largest pitch bend range.
    pub const MAX_BEND: f32 = 48.0;
    /// Number of choke groups.
    pub const CHOKE_GROUPS: u8 = 16;
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
    /// Stereo position, -1 (left) to 1 (right); 0 is centre.
    pub pan: f32,
    /// Release velocity, 0 to 1; 0.5 is neutral (instruments shorten their release above it
    /// and lengthen it below).
    pub release: f32,
    /// Fine pitch in cents, -100 to 100.
    pub fine: f32,
    /// Generic modulation values the instrument maps to destinations, 0 to 1; 0.5 is neutral.
    pub mod_x: f32,
    /// Second generic modulation value, 0 to 1; 0.5 is neutral.
    pub mod_y: f32,
    /// A slide note: instead of starting a voice it glides the previous note to its key over
    /// its own length.
    pub slide: bool,
    /// Portamento: the note starts at the previous note's pitch and glides to its own.
    pub porta: bool,
    /// Muted notes stay in the pattern but do not play.
    pub muted: bool,
    /// Colour group, 0 to [`Note::COLORS`] - 1.
    pub color: u8,
}

impl Note {
    /// Number of note colour groups.
    pub const COLORS: u8 = 16;
    /// Neutral release velocity and mod X/Y values.
    pub const NEUTRAL: f32 = 0.5;

    /// A plain note: centred, neutral expression values, no slide.
    pub fn new(start: i64, length: i64, key: u8, velocity: f32) -> Self {
        Self {
            start,
            length,
            key,
            velocity,
            pan: 0.0,
            release: Self::NEUTRAL,
            fine: 0.0,
            mod_x: Self::NEUTRAL,
            mod_y: Self::NEUTRAL,
            slide: false,
            porta: false,
            muted: false,
            color: 0,
        }
    }

    /// True when every per-note value besides start, length, key and velocity is neutral.
    pub fn is_plain(&self) -> bool {
        let n = Self::new(self.start, self.length, self.key, self.velocity);
        *self == n
    }

    /// Clamps every per-note value into its range (non-finite values become neutral).
    pub fn sanitize(&mut self) {
        let fix = |v: f32, lo: f32, hi: f32, neutral: f32| {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                neutral
            }
        };
        self.length = self.length.max(1);
        self.velocity = fix(self.velocity, 0.0, 1.0, DEFAULT_VELOCITY);
        self.pan = fix(self.pan, -1.0, 1.0, 0.0);
        self.release = fix(self.release, 0.0, 1.0, Self::NEUTRAL);
        self.fine = fix(self.fine, -100.0, 100.0, 0.0);
        self.mod_x = fix(self.mod_x, 0.0, 1.0, Self::NEUTRAL);
        self.mod_y = fix(self.mod_y, 0.0, 1.0, Self::NEUTRAL);
        self.color = self.color.min(Self::COLORS - 1);
    }
}

/// A pattern: notes for any number of channels, played as a loop of `steps` 1/16 notes.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    /// Identity.
    pub id: PatternId,
    /// Display name.
    pub name: String,
    /// Length of the step grid, 1 to [`Pattern::MAX_STEPS`].
    pub steps: u16,
    /// Colour in the pattern list and the playlist, 0 to [`Pattern::COLORS`] - 1 (0 is the
    /// track's colour; see [`Pattern::rgb`]).
    pub color: u8,
    /// Notes per channel, each list sorted by start.
    pub notes: BTreeMap<ChannelId, Vec<Note>>,
    /// Recorded controller data (bend, sustain, mod wheel, aftertouch) per channel, each list
    /// sorted by tick.
    pub controls: BTreeMap<ChannelId, Vec<ControlPoint>>,
    /// Pattern automation (D112): one line per parameter, points at ticks from the pattern
    /// start. It plays wherever the pattern plays, unless a playlist automation clip for the
    /// same parameter is playing.
    pub automation: Vec<crate::Automation>,
}

impl Pattern {
    /// Lengths offered in the length menu, in steps (any length from 1 to
    /// [`Self::MAX_STEPS`] can be typed).
    pub const STEP_COUNTS: [u16; 6] = [8, 12, 16, 32, 64, 128];
    /// Longest step grid.
    pub const MAX_STEPS: u16 = 512;
    /// Number of pattern colours.
    pub const COLORS: u8 = 16;

    /// Length in ticks: the step grid, or when notes run past it, whole bars (4/4) that fit
    /// every note. Notes drawn past the grid in the piano roll make the pattern longer, as in
    /// other pattern-based DAWs; a step note shifted late does not.
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
        if end <= grid + crate::rack::MAX_SHIFT {
            grid
        } else {
            (end + BAR - 1).div_euclid(BAR) * BAR
        }
    }

    /// The controller data of `channel`, sorted by tick.
    pub fn channel_controls(&self, channel: ChannelId) -> &[ControlPoint] {
        self.controls.get(&channel).map_or(&[], Vec::as_slice)
    }

    /// Removes the notes of `channel` that start in `[lo, hi)` and its controller data in
    /// that range (recording in replace mode). True when anything went.
    pub fn clear_range(&mut self, channel: ChannelId, lo: i64, hi: i64) -> bool {
        let mut gone = false;
        if let Some(v) = self.notes.get_mut(&channel) {
            let n = v.len();
            v.retain(|x| x.start < lo || x.start >= hi);
            gone |= v.len() != n;
        }
        if let Some(v) = self.controls.get_mut(&channel) {
            let n = v.len();
            v.retain(|x| x.tick < lo || x.tick >= hi);
            gone |= v.len() != n;
        }
        gone
    }

    /// The note that the step sequencer shows at `step` for `channel`, if any.
    pub fn step_note(&self, channel: ChannelId, step: u16) -> Option<&Note> {
        let at = i64::from(step) * STEP_TICKS;
        self.notes
            .get(&channel)?
            .iter()
            .find(|n| crate::rack::is_step(n, at))
    }

    /// Turns a step on (with [`DEFAULT_VELOCITY`]) or off. Returns the new state.
    pub fn toggle_step(&mut self, channel: ChannelId, step: u16) -> bool {
        let at = i64::from(step) * STEP_TICKS;
        let notes = self.notes.entry(channel).or_default();
        if let Some(i) = notes.iter().position(|n| crate::rack::is_step(n, at)) {
            notes.remove(i);
            false
        } else {
            let i = notes.partition_point(|n| n.start <= at);
            notes.insert(i, Note::new(at, STEP_TICKS, ROOT_KEY, DEFAULT_VELOCITY));
            true
        }
    }

    /// Sets the velocity of the note at `step` (clamped to 0..=1). No-op if the step is off.
    pub fn set_step_velocity(&mut self, channel: ChannelId, step: u16, velocity: f32) {
        let at = i64::from(step) * STEP_TICKS;
        if let Some(n) = self
            .notes
            .get_mut(&channel)
            .and_then(|v| v.iter_mut().find(|n| crate::rack::is_step(n, at)))
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
    /// MIDI controllers bound to parameters (MIDI learn).
    pub midi_map: Vec<crate::MidiBinding>,
    /// MIDI inputs that play fixed channels (the rest play the selected channel).
    pub midi_routes: Vec<crate::MidiRoute>,
    /// Title, artist, genre and notes (Prompt 25).
    pub info: ProjectInfo,
    next_id: u32,
}

/// What the user wrote about the project: saved with it, shown in the Project info window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectInfo {
    /// Song title.
    pub title: String,
    /// Artist name.
    pub artist: String,
    /// Genre.
    pub genre: String,
    /// Free text.
    pub notes: String,
}

impl ProjectInfo {
    /// True when nothing is filled in.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
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
            midi_map: Vec::new(),
            midi_routes: Vec::new(),
            info: ProjectInfo::default(),
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
                .map(|&(step, len, key, velocity)| {
                    Note::new(step * STEP_TICKS, len * STEP_TICKS, key, velocity)
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
            ParamId::strip_volume(crate::mixer::DEMO_REVERB),
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
        let delay = ParamId::strip_volume(crate::mixer::DEMO_DELAY);
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
        let (reverb, delay) = self.mixer.fx_returns();
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
        m.add_send(snare, reverb, 0.35, false);
        m.add_send(clap, reverb, 0.45, false);
        m.add_send(hat, delay, 0.25, false);
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

    /// The next identity the project will hand out (saved so ids are never reused).
    pub fn id_counter(&self) -> u32 {
        self.next_id
    }

    /// Restores the identity counter after loading. It never goes at or below an id already
    /// in use, whatever the file says.
    pub fn set_id_counter(&mut self, next: u32) {
        let used = self
            .channels
            .iter()
            .map(|c| c.id.0)
            .chain(self.patterns.iter().map(|p| p.id.0))
            .chain(self.modulators.iter().map(|m| m.id.0))
            .max()
            .unwrap_or(0);
        self.next_id = next.max(used + 1);
    }

    /// Repairs a project read from a file: every value in range, no dangling references, at
    /// least one pattern, ids unique. Everything the engine relies on holds afterwards.
    pub fn sanitize(&mut self) {
        let fin =
            |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        self.mixer.sanitize();
        let inserts = self.mixer.inserts();
        // Channels: unique ids, at most MAX_CHANNELS, values in range.
        let mut seen = std::collections::BTreeSet::new();
        self.channels.retain(|c| seen.insert(c.id));
        self.channels.truncate(MAX_CHANNELS);
        for c in &mut self.channels {
            c.volume = fin(c.volume, 0.0, Channel::MAX_VOLUME, Channel::DEFAULT_VOLUME);
            c.pan = fin(c.pan, -1.0, 1.0, 0.0);
            if c.insert > inserts {
                c.insert = MASTER;
            }
            match &mut c.instrument {
                Instrument::Sampler(s) => s.sanitize(),
                Instrument::Player(p) => {
                    p.base.sanitize();
                    p.sanitize();
                }
                Instrument::Synth(p) => p.sanitize(),
                Instrument::Plugin(p) => p.sanitize(),
                Instrument::Drums(k) => k.sanitize(inserts),
                Instrument::Layer(_) => {}
            }
            c.swing = c.swing.filter(|s| s.is_finite()).map(|s| s.clamp(0.0, 1.0));
            c.choke = c.choke.min(Channel::CHOKE_GROUPS);
            c.keys.sanitize();
        }
        // Layers play existing, non-layer channels other than themselves, each once.
        let plain: std::collections::BTreeSet<ChannelId> = self
            .channels
            .iter()
            .filter(|c| c.layer().is_none())
            .map(|c| c.id)
            .collect();
        for c in &mut self.channels {
            if let Instrument::Layer(l) = &mut c.instrument {
                let mut seen = std::collections::BTreeSet::new();
                l.children.retain(|k| plain.contains(k) && seen.insert(*k));
                l.children
                    .truncate(crate::rack::LayerSettings::MAX_CHILDREN);
            }
        }
        // Patterns: unique ids, valid lengths, notes of existing channels only, sorted.
        let mut seen = std::collections::BTreeSet::new();
        self.patterns.retain(|p| seen.insert(p.id));
        let channels: std::collections::BTreeSet<ChannelId> =
            self.channels.iter().map(|c| c.id).collect();
        for p in &mut self.patterns {
            p.steps = p.steps.clamp(1, Pattern::MAX_STEPS);
            p.color = p.color.min(Pattern::COLORS - 1);
            p.notes
                .retain(|ch, v| channels.contains(ch) && !v.is_empty());
            for v in p.notes.values_mut() {
                for n in v.iter_mut() {
                    n.start = n.start.max(0);
                    n.length = n.length.max(1);
                    n.key = n.key.min(127);
                    n.velocity = fin(n.velocity, 0.0, 1.0, DEFAULT_VELOCITY);
                }
                v.sort_by_key(|n| n.start);
            }
            p.controls
                .retain(|ch, v| channels.contains(ch) && !v.is_empty());
            for v in p.controls.values_mut() {
                v.retain(|c| c.value.is_finite());
                for c in v.iter_mut() {
                    c.tick = c.tick.max(0);
                    c.value = c.value.clamp(c.kind.min(), 1.0);
                }
                v.sort_by_key(|c| c.tick);
            }
        }
        if self.patterns.is_empty() {
            let id = self.new_pattern();
            self.current_pattern = id;
        }
        if !self.patterns.iter().any(|p| p.id == self.current_pattern) {
            self.current_pattern = self.patterns[0].id;
        }
        self.swing = fin(self.swing, 0.0, 1.0, 0.0);
        let patterns: Vec<PatternId> = self.patterns.iter().map(|p| p.id).collect();
        self.playlist.sanitize(|id| patterns.contains(&id));
        for t in &mut self.playlist.tracks {
            if t.insert > inserts {
                t.insert = MASTER;
            }
        }
        // Modulators: unique ids, valid targets kept even if they no longer resolve (a deleted
        // effect slot), at most MAX_MODULATORS.
        let mut seen = std::collections::BTreeSet::new();
        self.modulators.retain(|m| seen.insert(m.id));
        self.modulators.truncate(MAX_MODULATORS);
        for m in &mut self.modulators {
            m.sanitize();
        }
        self.sanitize_midi();
        self.make_plugin_instances_unique();
        let next = self.next_id;
        self.set_id_counter(next);
        let next = self.playlist.id_counter();
        self.playlist.set_id_counter(next);
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

    /// Adds a preset player channel (Prompt 28) at the bottom of the rack. `None` when the
    /// rack is full.
    pub fn add_player_channel(
        &mut self,
        name: &str,
        player: crate::player::Player,
    ) -> Option<ChannelId> {
        self.add_instrument(name, Instrument::Player(Box::new(player)))
    }

    /// Adds a Gloom Synth channel at the bottom of the rack. `None` when the rack is full.
    pub fn add_synth_channel(&mut self, name: &str, patch: SynthPatch) -> Option<ChannelId> {
        self.add_instrument(name, Instrument::Synth(Box::new(patch)))
    }

    /// Adds an empty drum pad channel at the bottom of the rack. `None` when the rack is full.
    pub fn add_drum_channel(&mut self, name: &str) -> Option<ChannelId> {
        self.add_instrument(name, Instrument::Drums(Box::default()))
    }

    /// Adds a drum synth channel (drum pads playing [`crate::DrumKit::synth_kit`]) at the
    /// bottom of the rack. `None` when the rack is full.
    pub fn add_drum_synth_channel(&mut self, name: &str) -> Option<ChannelId> {
        self.add_instrument(
            name,
            Instrument::Drums(Box::new(crate::DrumKit::synth_kit())),
        )
    }

    /// Adds a layer channel (no children yet) at the bottom of the rack. `None` when the rack
    /// is full.
    pub fn add_layer_channel(&mut self, name: &str) -> Option<ChannelId> {
        self.add_instrument(
            name,
            Instrument::Layer(crate::rack::LayerSettings::default()),
        )
    }

    /// Adds a channel playing an instrument plugin at the bottom of the rack. `None` when the
    /// rack is full.
    pub fn add_plugin_channel(&mut self, name: &str, plugin: PluginRef) -> Option<ChannelId> {
        self.add_instrument(name, Instrument::Plugin(Box::new(plugin)))
    }

    /// Every plugin in the project with where it sits: channels in rack order, then effect
    /// slots by strip and slot.
    pub fn plugins(&self) -> Vec<(PluginOwner, &PluginRef)> {
        let mut out: Vec<(PluginOwner, &PluginRef)> = self
            .channels
            .iter()
            .filter_map(|c| Some((PluginOwner::Channel(c.id), c.plugin()?)))
            .collect();
        for (strip, s) in self.mixer.strips.iter().enumerate() {
            for (slot, fx) in s.slots.iter().enumerate() {
                if let Some(p) = fx.as_ref().and_then(|f| f.plugin.as_deref()) {
                    out.push((PluginOwner::Effect { strip, slot }, p));
                }
            }
        }
        out
    }

    /// The plugin at `owner`, if there is one.
    pub fn plugin(&self, owner: PluginOwner) -> Option<&PluginRef> {
        match owner {
            PluginOwner::Channel(id) => self.channels[self.channel_index(id)?].plugin(),
            PluginOwner::Effect { strip, slot } => self
                .mixer
                .strips
                .get(strip)?
                .slots
                .get(slot)?
                .as_ref()?
                .plugin
                .as_deref(),
        }
    }

    /// Mutable plugin at `owner`.
    pub fn plugin_mut(&mut self, owner: PluginOwner) -> Option<&mut PluginRef> {
        match owner {
            PluginOwner::Channel(id) => {
                let i = self.channel_index(id)?;
                self.channels[i].plugin_mut()
            }
            PluginOwner::Effect { strip, slot } => self
                .mixer
                .strips
                .get_mut(strip)?
                .slots
                .get_mut(slot)?
                .as_mut()?
                .plugin
                .as_deref_mut(),
        }
    }

    /// The plugin with a given instance id and where it sits.
    pub fn plugin_by_instance(&self, id: PluginInstanceId) -> Option<(PluginOwner, &PluginRef)> {
        self.plugins().into_iter().find(|(_, p)| p.instance == id)
    }

    /// The plugin entry for a running instance, to update it.
    pub fn plugin_by_instance_mut(&mut self, id: PluginInstanceId) -> Option<&mut PluginRef> {
        let (owner, _) = self.plugin_by_instance(id)?;
        self.plugin_mut(owner)
    }

    /// Loads a mixer-strip preset into strip `i`, or only its effect chain with `chain_only`
    /// (Prompt 29). Plugins in it get new instances. False if there is no such strip.
    pub fn apply_strip_preset(
        &mut self,
        i: usize,
        p: &crate::StripPreset,
        chain_only: bool,
    ) -> bool {
        let Some(s) = self.mixer.strips.get_mut(i) else {
            return false;
        };
        if chain_only {
            s.slots = p.slots.clone();
        } else {
            s.apply_preset(p);
        }
        for fx in s.slots.iter_mut().flatten() {
            fx.sanitize();
            if let Some(pl) = fx.plugin.as_deref_mut() {
                pl.instance = PluginInstanceId::fresh();
            }
        }
        true
    }

    /// Links playlist track `track` to mixer strip `strip` so they show side by side: the
    /// track's audio plays into the strip, an unnamed strip takes the track's name, and the
    /// strip takes the track's colour (Prompt 29).
    pub fn link_track(&mut self, strip: usize, track: crate::TrackId) -> bool {
        let Some(t) = self.playlist.tracks.iter_mut().find(|t| t.id == track) else {
            return false;
        };
        let Some(s) = self.mixer.strips.get_mut(strip) else {
            return false;
        };
        t.insert = strip;
        if strip != MASTER && s.name == StripKind::of(strip).default_name() {
            s.name = t.name.clone();
        }
        s.color = Some(t.color);
        true
    }

    /// The playlist tracks linked to (playing into) mixer strip `strip`.
    pub fn tracks_of_strip(&self, strip: usize) -> impl Iterator<Item = &crate::Track> {
        self.playlist
            .tracks
            .iter()
            .filter(move |t| t.insert == strip)
    }

    /// Gives every plugin its own instance id: a copied channel or slot must not share the
    /// running plugin of the original.
    pub fn make_plugin_instances_unique(&mut self) {
        let mut seen = std::collections::HashSet::new();
        let mut fix = |p: &mut PluginRef| {
            if !seen.insert(p.instance) {
                p.instance = PluginInstanceId::fresh();
            }
        };
        for c in &mut self.channels {
            if let Some(p) = c.plugin_mut() {
                fix(p);
            }
        }
        for s in &mut self.mixer.strips {
            for fx in s.slots.iter_mut().flatten() {
                if let Some(p) = fx.plugin.as_deref_mut() {
                    fix(p);
                }
            }
        }
    }

    fn add_instrument(&mut self, name: &str, instrument: Instrument) -> Option<ChannelId> {
        if self.channels.len() >= MAX_CHANNELS {
            return None;
        }
        let id = ChannelId(self.next_id());
        // Each new channel gets the first insert no other channel uses, named after it.
        let insert = (1..=self.mixer.inserts())
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
            bend_range: Channel::DEFAULT_BEND,
            swing: None,
            choke: 0,
            group: String::new(),
            keys: crate::rack::KeyLimits::default(),
        });
        Some(id)
    }

    /// Removes a channel, its notes from every pattern and the modulators on its parameters.
    pub fn remove_channel(&mut self, id: ChannelId) {
        self.channels.retain(|c| c.id != id);
        self.modulators.retain(|m| m.target.channel() != Some(id));
        self.midi_routes.retain(|r| r.target != id);
        for c in &mut self.channels {
            if let Some(l) = c.layer_mut() {
                l.children.retain(|&k| k != id);
            }
        }
        for p in &mut self.patterns {
            p.notes.remove(&id);
            p.controls.remove(&id);
        }
    }

    /// Appends a mixer insert; `None` when there are [`crate::MAX_INSERTS`].
    pub fn add_insert(&mut self) -> Option<usize> {
        self.mixer.add_insert()
    }

    /// Deletes mixer insert `i` (Prompt 29). Inserts after it move down one, and everything
    /// that names a strip follows: channels, pads and plugin outputs, tracks, routing,
    /// automation, modulators and MIDI bindings. What played into `i` now plays into the
    /// master; automation, modulators and bindings of `i` itself are removed. False when `i`
    /// is not an insert or is the last one.
    pub fn remove_insert(&mut self, i: usize) -> bool {
        if !self.mixer.remove_insert(i) {
            return false;
        }
        let strip = |s: usize| Mixer::remap(i, s);
        let to = |s: usize| strip(s).unwrap_or(MASTER);
        for c in &mut self.channels {
            c.insert = to(c.insert);
            if let Some(k) = c.drums_mut() {
                for p in k.banks.iter_mut().flatten() {
                    p.output = p.output.and_then(strip);
                }
            }
            if let Some(p) = c.plugin_mut() {
                for o in &mut p.outputs {
                    o.route = o
                        .route
                        .and_then(|r| strip(usize::from(r)))
                        .map(|r| r as u16);
                }
            }
        }
        for t in &mut self.playlist.tracks {
            t.insert = to(t.insert);
        }
        let keep = |a: &mut crate::Automation| match a.target.remap_strips(strip) {
            Some(t) => {
                a.target = t;
                true
            }
            None => false,
        };
        self.playlist.clips.retain_mut(|c| match &mut c.kind {
            ClipKind::Automation(a) => keep(a),
            _ => true,
        });
        for p in &mut self.patterns {
            p.automation.retain_mut(keep);
        }
        self.modulators.retain_mut(|m| {
            if let crate::ModSourceKind::Follower { strip: s, .. } = &mut m.source {
                *s = to(*s);
            }
            match m.target.remap_strips(strip) {
                Some(t) => {
                    m.target = t;
                    true
                }
                None => false,
            }
        });
        self.midi_map
            .retain_mut(|b| match b.param.remap_strips(strip) {
                Some(t) => {
                    b.param = t;
                    true
                }
                None => false,
            });
        true
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
            color: 0,
            notes: BTreeMap::new(),
            controls: BTreeMap::new(),
            automation: Vec::new(),
        });
        id
    }

    /// Splits pattern `id` into one pattern per channel with notes, named "<pattern> -
    /// <channel>" and placed after it. Each playlist clip of the pattern is replaced by one
    /// clip per new pattern at the same place, on the clip's track and the tracks below it
    /// (added at the bottom when the playlist runs out). Returns the new patterns, in rack
    /// order; none when the pattern has notes of fewer than two channels.
    pub fn split_pattern_by_channel(&mut self, id: PatternId) -> Vec<PatternId> {
        let Some(i) = self.patterns.iter().position(|p| p.id == id) else {
            return Vec::new();
        };
        let src = self.patterns[i].clone();
        let used: Vec<&Channel> = self
            .channels
            .iter()
            .filter(|c| {
                !src.channel_notes(c.id).is_empty() || !src.channel_controls(c.id).is_empty()
            })
            .collect();
        if used.len() < 2 {
            return Vec::new();
        }
        let names: Vec<(ChannelId, String)> = used.iter().map(|c| (c.id, c.name.clone())).collect();
        let mut out = Vec::new();
        for (k, (ch, cname)) in names.iter().enumerate() {
            let new_id = PatternId(self.next_id());
            let name = self.unique_pattern_name(&format!("{} - {cname}", src.name));
            let mut p = Pattern {
                id: new_id,
                name,
                notes: BTreeMap::new(),
                controls: BTreeMap::new(),
                automation: Vec::new(),
                ..src.clone()
            };
            if let Some(v) = src.notes.get(ch) {
                p.notes.insert(*ch, v.clone());
            }
            if let Some(v) = src.controls.get(ch) {
                p.controls.insert(*ch, v.clone());
            }
            // Pattern automation stays with the first part.
            if k == 0 {
                p.automation = src.automation.clone();
            }
            self.patterns.insert(i + 1 + k, p);
            out.push(new_id);
        }
        // The playlist: each clip of the pattern becomes one clip per part.
        let clips: Vec<crate::Clip> = self
            .playlist
            .clips
            .iter()
            .filter(|c| c.kind == crate::ClipKind::Pattern(id))
            .cloned()
            .collect();
        for clip in clips {
            let Some(row) = self.playlist.tracks.iter().position(|t| t.id == clip.track) else {
                continue;
            };
            self.playlist.clips.retain(|c| c.id != clip.id);
            for (k, &pid) in out.iter().enumerate() {
                while self.playlist.tracks.len() <= row + k {
                    self.playlist.add_track();
                }
                let track = self.playlist.tracks[row + k].id;
                let cid = self.playlist.add_clip(
                    track,
                    clip.start,
                    clip.length,
                    crate::ClipKind::Pattern(pid),
                );
                if let Some(c) = self.playlist.clips.iter_mut().find(|c| c.id == cid) {
                    c.offset = clip.offset;
                    c.muted = clip.muted;
                }
            }
        }
        out
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

    /// `base`, or `base 2`, `base 3` … whichever no pattern has yet.
    pub fn unique_pattern_name(&self, base: &str) -> String {
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
        pat.notes
            .entry(c)
            .or_default()
            .push(Note::new(16 * STEP_TICKS + 100, 240, 64, 0.5));
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

    #[test]
    fn removing_an_insert_renumbers_everything_that_names_a_strip() {
        let mut p = Project::demo();
        let (reverb, delay) = (crate::mixer::DEMO_REVERB, crate::mixer::DEMO_DELAY);
        let kick = p.channels[0].insert;
        let hat = p.channels.iter().find(|c| c.name == "Hat").unwrap().insert;
        assert!(p.mixer.strips[hat].sends.iter().any(|s| s.to == delay));
        let mods = p.modulators.len();
        p.midi_map.push(crate::MidiBinding {
            channel: 0,
            cc: 7,
            param: ParamId::strip_volume(delay),
        });
        assert!(p.remove_insert(reverb));
        assert_eq!(p.mixer.inserts(), delay - 1);
        // The delay moved down into the reverb's place, with everything pointing at it.
        assert_eq!(p.mixer.strips[reverb].name, "Delay");
        assert!(p.mixer.strips[hat].sends.iter().any(|s| s.to == reverb));
        assert!(p
            .mixer
            .strips
            .iter()
            .all(|s| s.sends.iter().all(|x| x.to < delay)));
        assert_eq!(
            p.midi_map.last().unwrap().param,
            ParamId::strip_volume(reverb)
        );
        // The follower on the reverb's volume went with it.
        assert_eq!(p.modulators.len(), mods - 1);
        assert_eq!(p.channels[0].insert, kick);
        assert!(!p.remove_insert(MASTER));
        p.sanitize();
        assert!(p.mixer.processing_order().is_some());
    }

    #[test]
    fn strip_presets_and_track_links() {
        let mut p = Project::demo();
        let (reverb, delay) = (crate::mixer::DEMO_REVERB, crate::mixer::DEMO_DELAY);
        let preset = p.mixer.strips[reverb].preset();
        let routing = (
            p.mixer.strips[delay].output,
            p.mixer.strips[delay].sends.clone(),
        );
        assert!(p.apply_strip_preset(delay, &preset, false));
        assert_eq!(p.mixer.strips[delay].name, "Reverb");
        assert_eq!(p.mixer.strips[delay].slots, p.mixer.strips[reverb].slots);
        assert_eq!(
            (
                p.mixer.strips[delay].output,
                p.mixer.strips[delay].sends.clone()
            ),
            routing
        );
        let mut chain = preset.clone();
        chain.name = "x".into();
        assert!(p.apply_strip_preset(3, &chain, true));
        assert_ne!(p.mixer.strips[3].name, "x");
        assert!(!p.apply_strip_preset(999, &chain, true));

        let t = p.playlist.tracks[7].id;
        let free = p.add_insert().unwrap();
        assert!(p.link_track(free, t));
        assert_eq!(
            p.tracks_of_strip(free).map(|t| t.id).collect::<Vec<_>>(),
            vec![t]
        );
        assert_eq!(p.mixer.strips[free].name, p.playlist.tracks[7].name);
        assert_eq!(p.mixer.strips[free].color, Some(p.playlist.tracks[7].color));
    }
}
