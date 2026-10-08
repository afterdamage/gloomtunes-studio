//! The real-time audio engine of GloomTunes Studio.
//!
//! The engine is split in two halves created together by [`create`]:
//!
//! - [`AudioProcessor`] is moved onto the audio thread (into the device callback, or onto an
//!   export thread) and only ever does real-time-safe work in [`AudioProcessor::process`].
//! - [`EngineHandle`] stays on the UI thread. It never waits for the audio thread.
//!
//! This crate deliberately does not depend on any audio device library: the device layer in
//! `gt-app` calls `process`, and so can tests and offline export.
//!
//! Step 1 scope: the processor plays a 440 Hz test tone at -12 dBFS with click-free start/stop
//! fades. Control uses a few atomics; Step 2 replaces them with the `rtrb` command queue described
//! in ARCHITECTURE.md §4.

// Unsafe code will be needed later for FTZ/DAZ flags (ARCHITECTURE.md §7.6); until then, none.
#![deny(unsafe_code)]

mod atomic;
mod processor;

pub use atomic::AtomicF32;
pub use processor::{AudioProcessor, TEST_TONE_DBFS, TEST_TONE_HZ};

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// Fade time for start/stop, in seconds. Long enough to be click-free, short enough to feel instant.
pub const FADE_SECONDS: f32 = 0.02;

/// Static configuration of an engine instance. A new device or sample rate means a new engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    /// Device sample rate in Hz.
    pub sample_rate: u32,
    /// Number of interleaved output channels the device expects.
    pub out_channels: usize,
}

/// Values the audio thread publishes for the UI. All plain atomics: written with `Relaxed`
/// ordering on the audio thread, read at any time by the UI. No locks, no queues.
#[derive(Debug, Default)]
pub struct Telemetry {
    /// Number of frames in the most recent `process` call (the actual device buffer size).
    pub last_block_frames: AtomicU32,
    /// Total number of `process` calls.
    pub blocks: AtomicU64,
    /// Highest absolute sample value seen since the UI last reset it (UI applies decay).
    pub peak: AtomicF32,
    /// True when output is exactly silent and the tone is stopped (safe to close the stream).
    pub silent: AtomicBool,
    /// Set by the device layer when the callback panicked; the engine output is muted.
    pub faulted: AtomicBool,
}

/// Control flags written by the UI and read by the audio thread.
#[derive(Debug, Default)]
pub(crate) struct Control {
    pub(crate) tone_on: AtomicBool,
}

/// State shared between the two halves.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    pub(crate) control: Control,
    pub(crate) telemetry: Telemetry,
}

/// The UI-thread half of the engine.
#[derive(Debug, Clone)]
pub struct EngineHandle {
    shared: Arc<Shared>,
    config: EngineConfig,
}

impl EngineHandle {
    /// Fades the test tone in.
    pub fn start_tone(&self) {
        self.shared.telemetry.silent.store(false, Ordering::Relaxed);
        self.shared.control.tone_on.store(true, Ordering::Relaxed);
    }

    /// Fades the test tone out. [`Telemetry::silent`] becomes true once the fade has finished.
    pub fn stop_tone(&self) {
        self.shared.control.tone_on.store(false, Ordering::Relaxed);
    }

    /// True if the tone is requested to play (it may still be fading).
    pub fn tone_requested(&self) -> bool {
        self.shared.control.tone_on.load(Ordering::Relaxed)
    }

    /// Read-only access to the published telemetry.
    pub fn telemetry(&self) -> &Telemetry {
        &self.shared.telemetry
    }

    /// The configuration this engine was created with.
    pub fn config(&self) -> EngineConfig {
        self.config
    }
}

/// Creates both halves of an engine. Call on the UI thread: this allocates.
pub fn create(config: EngineConfig) -> (EngineHandle, AudioProcessor) {
    let shared = Arc::new(Shared::default());
    shared.telemetry.silent.store(true, Ordering::Relaxed);
    let processor = AudioProcessor::new(config, Arc::clone(&shared));
    (EngineHandle { shared, config }, processor)
}
