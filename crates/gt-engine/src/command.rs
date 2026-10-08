//! Messages between the UI thread and the audio thread (ARCHITECTURE.md §4).

use gt_core::{TempoMap, Tick, TimeSig};

/// Loop region in ticks. Playback wraps from `end` back to `start` while `enabled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopRegion {
    /// First tick inside the loop.
    pub start: Tick,
    /// First tick after the loop.
    pub end: Tick,
    /// Whether playback wraps.
    pub enabled: bool,
}

impl Default for LoopRegion {
    fn default() -> Self {
        Self {
            start: Tick(0),
            end: Tick(4 * gt_core::PPQ * 4),
            enabled: false,
        }
    }
}

/// UI → engine. Applied at the start of the next render quantum.
///
/// Every variant is small (checked at compile time below); large payloads travel boxed, with the
/// allocation done on the UI thread and the old value returned through the garbage queue.
#[derive(Debug)]
pub enum EngineCommand {
    /// Start playing from the current position (or resume from pause).
    Play,
    /// Stop and hold the current position.
    Pause,
    /// Stop and return to where playback last started; when already stopped, return to bar 1.
    Stop,
    /// Move the playhead.
    Locate(Tick),
    /// Set the loop region. Regions shorter than a 1/16 note are treated as disabled.
    SetLoop(LoopRegion),
    /// Replace the tempo map. The playhead keeps its musical position.
    SetTempoMap(Box<TempoMap>),
    /// Set the time signature (used for beats, bars and the metronome).
    SetTimeSig(TimeSig),
    /// Metronome clicks while playing.
    SetMetronome(bool),
    /// The 440 Hz device test tone from Step 1.
    SetTestTone(bool),
    /// Fade the whole output to silence (before closing the device). Telemetry `silent` turns
    /// true once done. `FadeIn` reverses it.
    FadeOut,
    /// Undo `FadeOut`.
    FadeIn,
}

// Keep commands cheap to copy through the queue.
const _: () = assert!(core::mem::size_of::<EngineCommand>() <= 32);

/// Transport state as seen by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum TransportState {
    /// Not playing; position is where Stop put it.
    #[default]
    Stopped = 0,
    /// Playing.
    Playing = 1,
    /// Not playing; position held.
    Paused = 2,
}

impl TransportState {
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Playing,
            2 => Self::Paused,
            _ => Self::Stopped,
        }
    }
}

/// Engine → UI notifications. Plain `Copy` data, so the audio thread never frees anything when
/// the queue is full and an event has to be dropped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EngineEvent {
    /// The transport changed state or jumped.
    TransportChanged {
        /// New state.
        state: TransportState,
        /// Position in ticks at the change.
        position: Tick,
    },
}

/// Things the engine is done with, handed back so they are freed on the UI thread.
#[derive(Debug)]
pub enum Garbage {
    /// A replaced tempo map.
    TempoMap(Box<TempoMap>),
}
