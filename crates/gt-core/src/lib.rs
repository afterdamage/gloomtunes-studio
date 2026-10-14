//! Shared plain-data types for GloomTunes Studio: musical time, IDs and the project document.
//!
//! Step 2 defined musical time (ticks, tempo map, time signature). Step 3 adds the first part
//! of the project document (channels, patterns, notes) and in-memory sample data. Step 5 adds
//! Gloom Synth patches; Step 6 the mixer and its effects; Step 7 the playlist; Step 8 the
//! parameter registry ([`ParamId`]), automation curves and modulators; Step 10 MIDI learn
//! bindings ([`MidiBinding`]); Step 11 third-party plugins ([`PluginRef`]); Prompt 13 audio
//! inputs and recorded audio ([`AudioInput`], [`Gap`]).

#![forbid(unsafe_code)]

pub mod autorec;
pub mod effects;
pub mod midi;
pub mod mixer;
pub mod modulation;
pub mod note_tools;
pub mod pads;
pub mod param;
pub mod params;
pub mod peaks;
pub mod playlist;
pub mod plugin;
pub mod project;
pub mod recording;
pub mod sample;
pub mod sampler;
pub mod synth;
pub mod time;

pub use autorec::{record_automation, record_pattern_automation, thin, THIN_TOLERANCE};
pub use effects::{EffectKind, EffectSlot};
pub use midi::{
    ControlKind, ControlPoint, MidiBinding, MidiRoute, MAX_LEARN_CC, MAX_MIDI_BINDINGS,
};
pub use mixer::{Mixer, MixerStrip, StripKind, FX_SLOTS, INSERTS, MASTER, SENDS, STRIPS};
pub use modulation::{
    LfoRate, LfoShape, MidiControl, ModSourceKind, Modulator, ModulatorId, MAX_MODULATORS,
};
pub use pads::{DrumKit, Pad, PadLayer};
pub use param::{ParamCurve, ParamInfo, ParamUnit};
pub use params::{ChannelParam, ParamId, StripParam, MASTER_VOLUME};
pub use peaks::Peaks;
pub use playlist::{
    AudioEdit, AudioTake, AutoPoint, Automation, Clip, ClipId, ClipKind, ClipStretch, Curve, Fade,
    FadeCurve, Frozen, Marker, Playlist, StretchMode, Track, TrackId, WarpMarker,
};
pub use plugin::{
    PluginFormat, PluginInstanceId, PluginKind, PluginOutput, PluginOwner, PluginParamInfo,
    PluginRef, MAX_EXTRA_OUTPUTS, MAX_PLUGIN_PARAMS,
};
pub use project::{
    Adsr, BuiltInSample, Channel, ChannelId, Instrument, LoopMode, Note, Pattern, PatternId,
    Project, ProjectInfo, SampleSource, SamplerSettings, MAX_CHANNELS, ROOT_KEY, STEP_TICKS,
};
pub use recording::{AudioInput, Gap, MonitorMode, MAX_INPUT_CHANNELS};
pub use sample::SampleData;
pub use sampler::{Interp, KeyZone, LfoTarget, SamplerFilter, SamplerFilterMode, SamplerLfo};
pub use synth::{ModDest, ModSlot, ModSource, SynthParam, SynthPatch};
pub use time::{
    BarBeatTick, SigChange, TempoMap, TempoMapError, TempoPoint, Tick, TimeSig, TimeSigMap, PPQ,
};
