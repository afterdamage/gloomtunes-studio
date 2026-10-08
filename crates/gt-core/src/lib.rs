//! Shared plain-data types for GloomTunes Studio: musical time, IDs and the project document.
//!
//! Step 2 defined musical time (ticks, tempo map, time signature). Step 3 adds the first part
//! of the project document (channels, patterns, notes) and in-memory sample data. Step 5 adds
//! Gloom Synth patches; Step 6 the mixer and its effects.

#![forbid(unsafe_code)]

pub mod effects;
pub mod mixer;
pub mod param;
pub mod project;
pub mod sample;
pub mod synth;
pub mod time;

pub use effects::{EffectKind, EffectSlot};
pub use mixer::{Mixer, MixerStrip, StripKind, FX_SLOTS, INSERTS, MASTER, SENDS, STRIPS};
pub use param::{ParamCurve, ParamInfo, ParamUnit};
pub use project::{
    Adsr, BuiltInSample, Channel, ChannelId, Instrument, LoopMode, Note, Pattern, PatternId,
    Project, SampleSource, SamplerSettings, MAX_CHANNELS, ROOT_KEY, STEP_TICKS,
};
pub use sample::SampleData;
pub use synth::{ModDest, ModSlot, ModSource, SynthParam, SynthPatch};
pub use time::{BarBeatTick, TempoMap, TempoMapError, TempoPoint, Tick, TimeSig, PPQ};
