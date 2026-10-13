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
//!
//! Every event sent is also kept in a [`RetroBuffer`], so what was just played can be turned
//! into a pattern even when nothing was recording (D110).

use std::sync::atomic::{fence, AtomicU64, Ordering};
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
    /// Another MIDI controller, 0 to 1 (plugins only).
    Cc {
        /// Controller number.
        cc: u8,
        /// Value.
        value: f32,
    },
    /// Polyphonic aftertouch on one key, 0 to 1.
    KeyPressure {
        /// MIDI key.
        key: u8,
        /// Pressure.
        value: f32,
    },
}

/// What a recorded controller value does, as a live event.
pub fn control_kind(kind: gt_core::ControlKind, value: f32) -> LiveKind {
    use gt_core::ControlKind as C;
    match kind {
        C::Bend => LiveKind::Bend(value),
        C::Sustain => LiveKind::Sustain(value >= 0.5),
        C::ModWheel => LiveKind::ModWheel(value),
        C::Pressure => LiveKind::Pressure(value),
        C::Cc(cc) => LiveKind::Cc { cc, value },
    }
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

/// Events the retroactive buffer holds: minutes of playing, or about a minute of a pitch
/// wheel moving without pause (2 MiB).
pub const RETRO_CAPACITY: usize = 1 << 17;

/// The last [`RETRO_CAPACITY`] live events with their arrival times, oldest overwritten first.
///
/// One writer at a time (the senders already share a lock); readers never lock. Each slot is
/// two atomics, the time and the packed event. The writer fills slot `head % capacity` and then
/// publishes `head + 1`; a reader copies the slots, re-reads `head` and keeps only the entries
/// no write can have reached meanwhile.
#[derive(Debug)]
pub struct RetroBuffer {
    time: Box<[AtomicU64]>,
    data: Box<[AtomicU64]>,
    head: AtomicU64,
}

impl RetroBuffer {
    /// An empty buffer for `capacity` events.
    pub fn new(capacity: usize) -> Self {
        let slots = || (0..capacity.max(1)).map(|_| AtomicU64::new(0)).collect();
        Self {
            time: slots(),
            data: slots(),
            head: AtomicU64::new(0),
        }
    }

    fn capacity(&self) -> u64 {
        self.time.len() as u64
    }

    /// Adds an event that arrived at `at_ns`. Callers must not push from two threads at once.
    pub fn push(&self, at_ns: u64, e: &LiveEvent) {
        let h = self.head.load(Ordering::Relaxed);
        let i = (h % self.capacity()) as usize;
        // A reader that sees the new slot contents also sees `head` at least at `h`, so it
        // knows the slot's old entry is gone.
        fence(Ordering::Release);
        self.time[i].store(at_ns, Ordering::Relaxed);
        self.data[i].store(pack(e), Ordering::Relaxed);
        self.head.store(h + 1, Ordering::Release);
    }

    /// Every event still held, oldest first, with its arrival time.
    pub fn snapshot(&self) -> Vec<(u64, LiveEvent)> {
        let cap = self.capacity();
        let end = self.head.load(Ordering::Acquire);
        let start = end.saturating_sub(cap);
        let mut out: Vec<(u64, u64)> = (start..end)
            .map(|k| {
                let i = (k % cap) as usize;
                (
                    self.time[i].load(Ordering::Relaxed),
                    self.data[i].load(Ordering::Relaxed),
                )
            })
            .collect();
        fence(Ordering::Acquire);
        // Entries the writer may have overwritten while they were copied.
        let now = self.head.load(Ordering::Relaxed);
        let valid = (now + 1).saturating_sub(cap).max(start);
        out.drain(..(valid - start).min(out.len() as u64) as usize);
        out.into_iter()
            .filter_map(|(t, d)| Some((t, unpack(d)?)))
            .collect()
    }
}

/// An event as 64 bits: kind in bits 0..8, port 8..16, channel 16..24, key 24..32 and the
/// value (velocity, bend, ...) as `f32` bits in 32..64.
fn pack(e: &LiveEvent) -> u64 {
    let (tag, key, value) = match e.kind {
        LiveKind::Note(n) => (1, n.key, n.velocity),
        LiveKind::Bend(v) => (2, 0, v),
        LiveKind::Sustain(d) => (3, 0, if d { 1.0 } else { 0.0 }),
        LiveKind::ModWheel(v) => (4, 0, v),
        LiveKind::Pressure(v) => (5, 0, v),
        LiveKind::KeyPressure { key, value } => (6, key, value),
        LiveKind::Cc { cc, value } => (7, cc, value),
    };
    tag | u64::from(e.port) << 8
        | u64::from(e.channel) << 16
        | u64::from(key) << 24
        | u64::from(value.to_bits()) << 32
}

fn unpack(d: u64) -> Option<LiveEvent> {
    let key = (d >> 24) as u8;
    let value = f32::from_bits((d >> 32) as u32);
    let kind = match d & 0xFF {
        1 => LiveKind::Note(LiveNote {
            key: key.min(127),
            velocity: value,
        }),
        2 => LiveKind::Bend(value),
        3 => LiveKind::Sustain(value > 0.5),
        4 => LiveKind::ModWheel(value),
        5 => LiveKind::Pressure(value),
        6 => LiveKind::KeyPressure { key, value },
        7 => LiveKind::Cc { cc: key, value },
        _ => return None,
    };
    Some(LiveEvent {
        kind,
        port: (d >> 8) as u8,
        channel: (d >> 16) as u8 & 0x0F,
        at_ns: 0,
    })
}

/// The sending end of the live queue, shared by every source of live events. Cloning shares
/// it. A new engine (new device or sample rate) is connected with [`LiveInput::connect`];
/// until then events are dropped (but still kept in the retroactive buffer).
#[derive(Debug, Clone)]
pub struct LiveInput {
    tx: Arc<Mutex<Option<Producer<LiveEvent>>>>,
    retro: Arc<RetroBuffer>,
}

impl Default for LiveInput {
    fn default() -> Self {
        Self {
            tx: Arc::default(),
            retro: Arc::new(RetroBuffer::new(RETRO_CAPACITY)),
        }
    }
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
        let e = event.into();
        let mut tx = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        // The lock makes this the only writer.
        let at = if e.at_ns == 0 { clock_ns() } else { e.at_ns };
        self.retro.push(at, &e);
        tx.as_mut().is_some_and(|p| p.push(e).is_ok())
    }

    /// What was played lately, oldest first, each event with its arrival time in `at_ns`.
    pub fn recent(&self) -> Vec<LiveEvent> {
        self.retro
            .snapshot()
            .into_iter()
            .map(|(at_ns, e)| LiveEvent { at_ns, ..e })
            .collect()
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

    fn note(key: u8) -> LiveEvent {
        LiveEvent {
            kind: LiveKind::Note(LiveNote::on(key, 0.5)),
            port: 3,
            channel: 9,
            at_ns: 0,
        }
    }

    #[test]
    fn the_retro_buffer_keeps_the_latest_events_in_order_after_wrapping() {
        let b = RetroBuffer::new(8);
        assert!(b.snapshot().is_empty());
        for k in 0..5 {
            b.push(100 + k, &note(k as u8));
        }
        let got = b.snapshot();
        assert_eq!(got.len(), 5);
        assert_eq!(got[0], (100, note(0)));
        for k in 5..21 {
            b.push(100 + k, &note(k as u8));
        }
        let got = b.snapshot();
        // Only the last 7 of 21: the slot the next write will reuse is not trusted.
        let times: Vec<u64> = got.iter().map(|e| e.0).collect();
        assert_eq!(times, (114..121).collect::<Vec<_>>());
        assert_eq!(got[6].1, note(20));
        // Every kind survives packing.
        for kind in [
            LiveKind::Bend(-0.25),
            LiveKind::Sustain(true),
            LiveKind::ModWheel(0.75),
            LiveKind::Pressure(0.5),
            LiveKind::KeyPressure {
                key: 61,
                value: 0.3,
            },
            LiveKind::Note(LiveNote::off(64)),
        ] {
            let e = LiveEvent { kind, ..note(0) };
            assert_eq!(unpack(pack(&e)), Some(e));
        }
    }

    #[test]
    fn the_retro_buffer_reads_consistently_while_written() {
        // The writer stores time = key * 1000 + k; a reader must never see a torn pair.
        let b = Arc::new(RetroBuffer::new(64));
        let w = Arc::clone(&b);
        let writer = std::thread::spawn(move || {
            for k in 0..200_000_u64 {
                let key = (k % 128) as u8;
                w.push(u64::from(key) * 1_000_000 + k, &note(key));
            }
        });
        let mut reads = 0;
        while !writer.is_finished() || reads == 0 {
            let got = b.snapshot();
            for pair in got.windows(2) {
                assert!(
                    pair[0].0 % 1_000_000 < pair[1].0 % 1_000_000,
                    "out of order"
                );
            }
            for (t, e) in got {
                let LiveKind::Note(n) = e.kind else {
                    panic!("{e:?}")
                };
                assert_eq!(u64::from(n.key), t / 1_000_000, "torn entry");
            }
            reads += 1;
        }
        writer.join().unwrap();
        assert_eq!(b.snapshot().len(), 63);
    }

    #[test]
    fn sending_keeps_events_even_without_an_engine() {
        let live = LiveInput::default();
        assert!(!live.send(LiveNote::on(60, 1.0)));
        let got = live.recent();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].port, KEYBOARD);
        assert!(got[0].at_ns > 0, "stamped on arrival");
    }
}
