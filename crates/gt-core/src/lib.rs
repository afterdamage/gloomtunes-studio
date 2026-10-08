//! Shared plain-data types for GloomTunes Studio: musical time, IDs and the project document.
//!
//! Step 2 defined musical time (ticks, tempo map, time signature). Step 3 adds the first part
//! of the project document (channels, patterns, notes) and in-memory sample data.

#![forbid(unsafe_code)]

pub mod project;
pub mod sample;
pub mod time;

pub use project::{
    Adsr, BuiltInSample, Channel, ChannelId, LoopMode, Note, Pattern, PatternId, Project,
    SampleSource, SamplerSettings, MAX_CHANNELS, ROOT_KEY, STEP_TICKS,
};
pub use sample::SampleData;
pub use time::{BarBeatTick, TempoMap, TempoMapError, TempoPoint, Tick, TimeSig, PPQ};
