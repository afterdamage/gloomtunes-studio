//! The real-time audio engine of GloomTunes Studio.
//!
//! The engine is split in two halves created together by [`create`]:
//!
//! - [`AudioProcessor`] is moved onto the audio thread (into the device callback, or onto an
//!   export thread) and only ever does real-time-safe work in [`AudioProcessor::process`].
//! - [`EngineHandle`] stays on the UI thread. It never waits for the audio thread.
//!
//! They talk through three wait-free `rtrb` queues (commands in; events and garbage out) and a
//! block of atomics ([`Telemetry`]). See ARCHITECTURE.md §4.
//!
//! This crate deliberately does not depend on any audio device library: the device layer in
//! `gt-app` calls `process`, and so can tests and offline export.

// Unsafe code will be needed later for FTZ/DAZ flags (ARCHITECTURE.md §7.6); until then, none.
#![deny(unsafe_code)]

mod atomic;
mod channel;
mod command;
mod mixer;
mod processor;
pub mod song;
pub mod transport;

pub use atomic::AtomicF32;
pub use channel::VOICES_PER_CHANNEL;
pub use command::{EngineCommand, EngineEvent, Garbage, LoopRegion, TransportState};
pub use gt_core::MAX_CHANNELS;
pub use mixer::{create_effect, EffectBox, MixerParams, StripParams, MAX_PDC_FRAMES};
pub use processor::{AudioProcessor, PREVIEW_GAIN, TEST_TONE_DBFS, TEST_TONE_HZ};
pub use song::{synth_settings, ChannelParams, InstrumentKind, NoteKind, SongEvent, SongSnapshot};

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use gt_core::{Tick, FX_SLOTS, STRIPS};
use rtrb::{Consumer, Producer, RingBuffer};

/// Frames per render quantum. The engine always renders in blocks of exactly this size, counted
/// from engine start, whatever the device buffer size is (ARCHITECTURE.md §5.3).
pub const RENDER_QUANTUM: usize = 64;
/// Fade time for test tone and output fades, in seconds.
pub const FADE_SECONDS: f32 = 0.02;

const COMMAND_CAPACITY: usize = 1024;
const EVENT_CAPACITY: usize = 256;
const GARBAGE_CAPACITY: usize = 512;

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
#[derive(Debug)]
pub struct Telemetry {
    /// Number of frames in the most recent `process` call (the actual device buffer size).
    pub last_block_frames: AtomicU32,
    /// Total number of `process` calls.
    pub blocks: AtomicU64,
    /// Highest absolute sample value seen since the UI last reset it (UI applies decay).
    pub peak: AtomicF32,
    /// True when the output has been faded out completely (safe to close the stream).
    pub silent: AtomicBool,
    /// Set by the device layer when the callback panicked; the engine output is muted.
    pub faulted: AtomicBool,
    /// [`TransportState`] as `u8`.
    pub transport_state: AtomicU8,
    /// Playhead in whole ticks, at the end of the last rendered quantum.
    pub position_ticks: AtomicI64,
    /// Engine frames rendered since creation.
    pub frames_rendered: AtomicU64,
    /// Events that could not be delivered (scheduler or event queue full). Should stay 0.
    pub events_dropped: AtomicU32,
    /// Peak output level per channel slot since the UI last reset it (activity lights).
    pub channel_peaks: [AtomicF32; MAX_CHANNELS],
    /// Oscilloscope ring: the last [`SCOPE_LEN`] samples of the scope channel (mono, before
    /// channel volume). Written in place; `scope_write` is the next index to be written.
    pub scope: [AtomicF32; SCOPE_LEN],
    /// Total samples written to `scope` (index = value % `SCOPE_LEN`).
    pub scope_write: AtomicU64,
    /// Level meters for every mixer strip, by strip index.
    pub meters: [MeterCell; STRIPS],
    /// Each effect's own meter (gain reduction in dB for dynamics), by strip and slot.
    pub fx_meters: [[AtomicF32; FX_SLOTS]; STRIPS],
    /// Delay from a channel to the device caused by effect latency, in frames.
    pub latency_frames: AtomicU32,
}

/// One strip's meter. The engine raises `peak` with `fetch_max` (the UI swaps it back to 0
/// when it reads, and applies decay and hold) and stores the RMS level, integrated over about
/// 300 ms, every quantum. Linear amplitude, left and right.
#[derive(Debug, Default)]
pub struct MeterCell {
    /// Highest sample since the UI last read it.
    pub peak: [AtomicF32; 2],
    /// Smoothed RMS level.
    pub rms: [AtomicF32; 2],
}

/// Length of the oscilloscope ring buffer in samples.
pub const SCOPE_LEN: usize = 4096;

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            last_block_frames: AtomicU32::default(),
            blocks: AtomicU64::default(),
            peak: AtomicF32::default(),
            silent: AtomicBool::default(),
            faulted: AtomicBool::default(),
            transport_state: AtomicU8::default(),
            position_ticks: AtomicI64::default(),
            frames_rendered: AtomicU64::default(),
            events_dropped: AtomicU32::default(),
            channel_peaks: std::array::from_fn(|_| AtomicF32::default()),
            scope: std::array::from_fn(|_| AtomicF32::default()),
            scope_write: AtomicU64::default(),
            meters: std::array::from_fn(|_| MeterCell::default()),
            fx_meters: std::array::from_fn(|_| std::array::from_fn(|_| AtomicF32::default())),
            latency_frames: AtomicU32::default(),
        }
    }
}

impl Telemetry {
    /// Copies the newest `out.len()` scope samples (oldest first). Samples may tear between
    /// quanta, which only matters for a picture.
    pub fn read_scope(&self, out: &mut [f32]) {
        let n = out.len().min(SCOPE_LEN);
        let end = self.scope_write.load(Ordering::Relaxed);
        let start = end.saturating_sub(n as u64);
        for (i, o) in out.iter_mut().enumerate().take(n) {
            *o = self.scope[((start + i as u64) % SCOPE_LEN as u64) as usize]
                .load(Ordering::Relaxed);
        }
    }

    /// The transport state.
    pub fn transport_state(&self) -> TransportState {
        TransportState::from_u8(self.transport_state.load(Ordering::Relaxed))
    }

    /// The playhead position.
    pub fn position(&self) -> Tick {
        Tick(self.position_ticks.load(Ordering::Relaxed))
    }
}

/// The UI-thread half of the engine.
#[derive(Debug)]
pub struct EngineHandle {
    commands: Producer<EngineCommand>,
    events: Consumer<EngineEvent>,
    garbage: Consumer<Garbage>,
    telemetry: Arc<Telemetry>,
    config: EngineConfig,
}

impl EngineHandle {
    /// Queues a command for the audio thread. Never blocks; if the queue is full the command is
    /// handed back so the caller can retry later.
    pub fn send(&mut self, cmd: EngineCommand) -> Result<(), EngineCommand> {
        self.commands
            .push(cmd)
            .map_err(|rtrb::PushError::Full(c)| c)
    }

    /// Calls `f` for every event the engine has posted since the last call.
    pub fn poll_events(&mut self, mut f: impl FnMut(EngineEvent)) {
        while let Ok(e) = self.events.pop() {
            f(e);
        }
    }

    /// Frees everything the engine has handed back. Call once per UI frame.
    pub fn collect_garbage(&mut self) -> usize {
        let mut n = 0;
        while let Ok(g) = self.garbage.pop() {
            drop(g);
            n += 1;
        }
        n
    }

    /// Read-only access to the published telemetry.
    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    /// The configuration this engine was created with.
    pub fn config(&self) -> EngineConfig {
        self.config
    }
}

/// Creates both halves of an engine. Call on the UI thread: this allocates.
pub fn create(config: EngineConfig) -> (EngineHandle, AudioProcessor) {
    let telemetry = Arc::new(Telemetry::default());
    let (cmd_tx, cmd_rx) = RingBuffer::new(COMMAND_CAPACITY);
    let (evt_tx, evt_rx) = RingBuffer::new(EVENT_CAPACITY);
    let (gc_tx, gc_rx) = RingBuffer::new(GARBAGE_CAPACITY);
    let processor = AudioProcessor::new(config, Arc::clone(&telemetry), cmd_rx, evt_tx, gc_tx);
    let handle = EngineHandle {
        commands: cmd_tx,
        events: evt_rx,
        garbage: gc_rx,
        telemetry,
        config,
    };
    (handle, processor)
}
