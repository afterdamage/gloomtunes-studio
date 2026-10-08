//! Shared plain-data types for GloomTunes Studio: musical time, IDs and the project document.
//!
//! Step 2 defined musical time (ticks, tempo map, time signature). Step 3 adds the first part
//! of the project document (channels, patterns, notes) and in-memory sample data. Step 5 adds
//! Gloom Synth patches; Step 6 the mixer and its effects; Step 7 the playlist; Step 8 the
//! parameter registry ([`ParamId`]), automation curves and modulators.

#![forbid(unsafe_code)]

pub mod effects;
pub mod mixer;
pub mod modulation;
pub mod param;
pub mod params;
pub mod peaks;
pub mod playlist;
pub mod project;
pub mod sample;
pub mod synth;
pub mod time;

pub use effects::{EffectKind, EffectSlot};
pub use mixer::{Mixer, MixerStrip, StripKind, FX_SLOTS, INSERTS, MASTER, SENDS, STRIPS};
pub use modulation::{LfoRate, LfoShape, ModSourceKind, Modulator, ModulatorId, MAX_MODULATORS};
pub use param::{ParamCurve, ParamInfo, ParamUnit};
pub use params::{ChannelParam, ParamId, StripParam, MASTER_VOLUME};
pub use peaks::Peaks;
pub use playlist::{
    AutoPoint, Automation, Clip, ClipId, ClipKind, Curve, Marker, Playlist, Track, TrackId,
};
pub use project::{
    Adsr, BuiltInSample, Channel, ChannelId, Instrument, LoopMode, Note, Pattern, PatternId,
    Project, SampleSource, SamplerSettings, MAX_CHANNELS, ROOT_KEY, STEP_TICKS,
};
pub use sample::SampleData;
pub use synth::{ModDest, ModSlot, ModSource, SynthParam, SynthPatch};
pub use time::{
    BarBeatTick, SigChange, TempoMap, TempoMapError, TempoPoint, Tick, TimeSig, TimeSigMap, PPQ,
};
