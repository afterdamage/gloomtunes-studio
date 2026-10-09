//! Notes played live (MIDI keyboards, the computer keyboard) on their way to the audio thread.
//!
//! Several threads play notes: one per connected MIDI device plus the UI thread for the
//! computer keyboard. They share one [`LiveInput`], whose lock is only ever taken by those
//! senders; the audio thread reads the other end of the queue without locking.

use std::sync::{Arc, Mutex, PoisonError};

use rtrb::Producer;

/// Capacity of the live-note queue: far more than anyone can play between two callbacks.
pub const LIVE_CAPACITY: usize = 512;

/// One key going down or up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveNote {
    /// MIDI key, 0 to 127.
    pub key: u8,
    /// Velocity from 0 to 1 for a key going down; 0 for a key going up.
    pub velocity: f32,
}

impl LiveNote {
    /// A key going down. A velocity of 0 is raised to the smallest audible one, because
    /// 0 means "up".
    pub fn on(key: u8, velocity: f32) -> Self {
        Self {
            key: key.min(127),
            velocity: velocity.clamp(1.0 / 127.0, 1.0),
        }
    }

    /// A key going up.
    pub fn off(key: u8) -> Self {
        Self {
            key: key.min(127),
            velocity: 0.0,
        }
    }

    /// True for a key going down.
    pub fn is_on(&self) -> bool {
        self.velocity > 0.0
    }
}

/// The sending end of the live-note queue, shared by every source of live notes. Cloning
/// shares it. A new engine (new device or sample rate) is connected with
/// [`LiveInput::connect`]; until then notes are dropped.
#[derive(Debug, Clone, Default)]
pub struct LiveInput {
    tx: Arc<Mutex<Option<Producer<LiveNote>>>>,
}

impl LiveInput {
    /// Sends future notes to a new engine (from [`crate::EngineHandle::take_live_producer`]).
    pub fn connect(&self, producer: Producer<LiveNote>) {
        *self.tx.lock().unwrap_or_else(PoisonError::into_inner) = Some(producer);
    }

    /// Drops notes until the next `connect`.
    pub fn disconnect(&self) {
        *self.tx.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Queues a note. False when no engine is connected or the queue is full.
    pub fn send(&self, note: LiveNote) -> bool {
        let mut tx = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        tx.as_mut().is_some_and(|p| p.push(note).is_ok())
    }
}
