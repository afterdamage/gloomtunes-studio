//! Editing, loading and saving of GloomTunes Studio projects.
//!
//! Step 3 adds the sample loader. The `Edit` command stack arrives in Step 4 and project
//! persistence in Step 9 (ARCHITECTURE.md §2.4).

#![forbid(unsafe_code)]

pub mod loader;

pub use loader::{is_audio_file, load_sample, LoadError, AUDIO_EXTENSIONS};
