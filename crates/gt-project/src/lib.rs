//! Editing, loading and saving of GloomTunes Studio projects.
//!
//! Step 3 added the sample loader; Step 4 adds undo/redo ([`History`]) and note operations.
//! Project persistence arrives in Step 9 (ARCHITECTURE.md §2.4).

#![forbid(unsafe_code)]

pub mod history;
pub mod loader;
pub mod ops;

pub use history::{Edit, History, Transaction};
pub use loader::{is_audio_file, load_sample, LoadError, AUDIO_EXTENSIONS};
