//! CLAP and VST3 plugin hosting for GloomTunes Studio (Step 11, Prompt 18; ARCHITECTURE.md
//! §2.6).
//!
//! * [`catalog`]: the plugin search folders, scanning plugin files in a separate process, and
//!   the scan cache. A plugin that ships as both CLAP and VST3 is offered as CLAP.
//! * [`PluginHost`]: runs plugin instances on the UI thread in step with the project and the
//!   engine: loading with saved state, parameter sync both ways, restarts, main-thread
//!   callbacks, timers and file descriptors, editor windows, and fresh instances for export.
//! * The engine side is a [`gt_engine::PluginProcessor`] per instance; a plugin that fails or
//!   outputs invalid samples is bypassed by the engine.
//! * [`Guard`]: an in-flight marker around risky calls into plugin code; a crash there
//!   quarantines the plugin file at the next start.
//!
//! Plugins run in the app's process. Running them in a separate process was evaluated and
//! deferred (ARCHITECTURE.md D74); the guard, the bypass and the scan process are the safety
//! net instead.

#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
mod au;
mod backend;
pub mod catalog;
mod guard;
mod host;
mod instance;
mod manager;
pub mod presets;
mod processor;
#[cfg(test)]
mod tests;
mod vst3;
mod window;

pub use catalog::{Catalog, CatalogFile, PluginInfo, SCAN_SWITCH};
pub use guard::{CrashReport, Guard};
pub use host::Waker;
pub use manager::{PluginHost, PluginStatus, SyncOutcome};
pub use presets::Preset;
