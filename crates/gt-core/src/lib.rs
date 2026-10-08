//! Shared plain-data types for GloomTunes Studio: musical time, IDs and the project document.
//!
//! Step 2 defines musical time (ticks, tempo map, time signature). The document types described
//! in ARCHITECTURE.md §6 arrive with the steps that need them.

#![forbid(unsafe_code)]

pub mod time;

pub use time::{BarBeatTick, TempoMap, TempoMapError, TempoPoint, Tick, TimeSig, PPQ};
