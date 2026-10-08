//! Editing, loading and saving of GloomTunes Studio projects.
//!
//! Step 3 added the sample loader; Step 4 undo/redo ([`History`]) and note operations; Step 5
//! Gloom Synth preset files ([`presets`]); Step 9 the project file ([`file`], [`migrate`]).

#![forbid(unsafe_code)]

pub mod file;
pub mod history;
pub mod loader;
pub mod migrate;
pub mod ops;
pub mod presets;

pub use history::{Edit, History, Transaction};
pub use loader::{is_audio_file, load_sample, LoadError, AUDIO_EXTENSIONS};
