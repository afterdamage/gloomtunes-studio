//! What players do live (MIDI keyboards and controllers, the computer keyboard) on its way to
//! the audio thread.
//!
//! Several threads send: one per connected MIDI device plus the UI thread for the computer
//! keyboard. They share one [`LiveInput`], whose lock is only ever taken by those senders; the
//! audio thread reads the other end of the queue without locking.
//!
//! Each event carries the time it arrived ([`clock_ns`]) and where it came from (input port
//! and MIDI channel). The audio thread plays it one device period after it arrived, at the
//! same place within the period, so notes keep their timing however the callbacks fall
//! (ARCHITECTURE.md D109), and routes it to a rack channel by its source ([`LiveRoutes`]).

use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use rtrb::Producer;

/// Capacity of the live queue: far more than anyone can play between two callbacks.
pub const LIVE_CAPACITY: usize = 1024;
/// MIDI input ports that can be routed one by one (later ports follow the selected channel).
pub const ROUTE_PORTS: usize = 16;
/// The source number of the computer keyboard and on-screen keys.
pub const KEYBOARD: u8 = u8::MAX;

/// Nanoseconds since the first call, on a monotonic clock shared by every thread.
pub fn clock_ns() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

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

/// What a live event does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LiveKind {
    /// A key going down or up.
    Note(LiveNote),
    /// Pitch bend, -1 to 1 (times the instrument's bend range).
    Bend(f32),
    /// Sustain pedal (CC 64) down or up.
    Sustain(bool),
    /// Mod wheel (CC 1), 0 to 1.
    ModWheel(f32),
    /// Channel aftertouch, 0 to 1.
    Pressure(f32),
    /// Polyphonic aftertouch on one key, 0 to 1.
    KeyPressure {
        /// MIDI key.
        key: u8,
        /// Pressure.
        value: f32,
    },
}

/// A live event with its source and arrival time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveEvent {
    /// What happens.
    pub kind: LiveKind,
    /// Input port index, or [`KEYBOARD`].
    pub port: u8,
    /// MIDI channel, 0 to 15.
    pub channel: u8,
    /// [`clock_ns`] when it arrived; 0 plays it as soon as possible.
    pub at_ns: u64,
}

impl LiveEvent {
    /// An event from a MIDI port that arrived now.
    pub fn from_port(kind: LiveKind, port: u8, channel: u8) -> Self {
        Self {
            kind,
            port,
            channel: channel & 0x0F,
            at_ns: clock_ns(),
        }
    }
}

impl From<LiveNote> for LiveEvent {
    /// A note from the computer keyboard, played as soon as possible.
    fn from(n: LiveNote) -> Self {
        Self {
            kind: LiveKind::Note(n),
            port: KEYBOARD,
            channel: 0,
            at_ns: 0,
        }
    }
}

/// Where each input port and MIDI channel plays: the selected rack channel (the default) or a
/// fixed one. Sent boxed with `EngineCommand::SetLiveRoutes`.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveRoutes {
    /// By `port * 16 + channel`: 0 for the selected channel, else rack slot + 1.
    slots: [u16; ROUTE_PORTS * 16],
}

impl Default for LiveRoutes {
    fn default() -> Self {
        Self {
            slots: [0; ROUTE_PORTS * 16],
        }
    }
}

impl LiveRoutes {
    /// Sends `port` and MIDI `channel` (or every channel of the port, for `None`) to rack
    /// `slot`, or back to the selected channel for `None`.
    pub fn set(&mut self, port: usize, channel: Option<u8>, slot: Option<u16>) {
        if port >= ROUTE_PORTS {
            return;
        }
        let v = slot.map_or(0, |s| s.saturating_add(1));
        match channel {
            Some(c) => self.slots[port * 16 + usize::from(c & 0x0F)] = v,
            None => self.slots[port * 16..port * 16 + 16].fill(v),
        }
    }

    /// The rack slot an event from `port`/`channel` plays, given the selected slot.
    pub fn slot(&self, port: u8, channel: u8, selected: Option<u16>) -> Option<u16> {
        let i = usize::from(port) * 16 + usize::from(channel & 0x0F);
        match self.slots.get(i).copied().unwrap_or(0) {
            0 => selected,
            s => Some(s - 1),
        }
    }
}

/// The sending end of the live queue, shared by every source of live events. Cloning shares
/// it. A new engine (new device or sample rate) is connected with [`LiveInput::connect`];
/// until then events are dropped.
#[derive(Debug, Clone, Default)]
pub struct LiveInput {
    tx: Arc<Mutex<Option<Producer<LiveEvent>>>>,
}

impl LiveInput {
    /// Sends future events to a new engine (from [`crate::EngineHandle::take_live_producer`]).
    pub fn connect(&self, producer: Producer<LiveEvent>) {
        *self.tx.lock().unwrap_or_else(PoisonError::into_inner) = Some(producer);
    }

    /// Drops events until the next `connect`.
    pub fn disconnect(&self) {
        *self.tx.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Queues an event. False when no engine is connected or the queue is full.
    pub fn send(&self, event: impl Into<LiveEvent>) -> bool {
        let mut tx = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        tx.as_mut().is_some_and(|p| p.push(event.into()).is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_send_ports_and_channels_to_fixed_slots() {
        let mut r = LiveRoutes::default();
        assert_eq!(r.slot(0, 3, Some(5)), Some(5), "selected by default");
        r.set(1, None, Some(2));
        r.set(1, Some(9), None);
        r.set(0, Some(3), Some(0));
        assert_eq!(r.slot(1, 0, Some(5)), Some(2));
        assert_eq!(
            r.slot(1, 9, Some(5)),
            Some(5),
            "channel 10 back to selected"
        );
        assert_eq!(r.slot(0, 3, None), Some(0));
        assert_eq!(r.slot(0, 4, None), None);
        assert_eq!(
            r.slot(KEYBOARD, 0, Some(7)),
            Some(7),
            "the keyboard follows selection"
        );
        r.set(ROUTE_PORTS, None, Some(1));
        assert_eq!(r.slot(ROUTE_PORTS as u8, 0, Some(7)), Some(7));
    }
}
