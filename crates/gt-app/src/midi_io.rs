//! MIDI input devices (midir), with hot-plug.
//!
//! A background thread lists the input ports once a second, connects to every new one the
//! user has not switched off and drops the ones that disappeared, on Windows and Linux alike
//! (neither backend reports plugging reliably, so polling is the portable way). Each
//! connection's callback runs on a MIDI thread owned by midir: notes, pitch bend, sustain, the
//! mod wheel and aftertouch go straight to the engine through the shared [`LiveInput`], stamped
//! with their arrival time and tagged with their port and channel (for routing); controller
//! moves also go to the UI thread (MIDI learn and bound parameters) with a repaint request.
//!
//! Each port gets a number the first time it is seen, kept while the app runs, so a route set
//! for a port still applies after it is unplugged and plugged back in.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use gt_engine::{LiveEvent, LiveInput, LiveKind, LiveNote};
use midir::{Ignore, MidiInput, MidiInputConnection};

/// How often the port list is checked.
const SCAN_EVERY: Duration = Duration::from_secs(1);
/// How often to retry when the MIDI system itself is unavailable (e.g. no ALSA sequencer).
const RETRY_EVERY: Duration = Duration::from_secs(10);
const CLIENT_NAME: &str = "GloomTunes Studio";

/// A controller (CC) message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiCc {
    /// MIDI channel 0 to 15.
    pub channel: u8,
    /// Controller number.
    pub cc: u8,
    /// Value 0 to 127.
    pub value: u8,
    /// When it arrived on [`gt_engine::clock_ns`]'s clock (0 until stamped), so recorded
    /// automation lands where a live note sent at the same moment would play (D111).
    pub at_ns: u64,
}

/// One input port as the settings panel shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiPortInfo {
    pub name: String,
    pub enabled: bool,
    pub connected: bool,
}

/// A decoded channel message.
#[derive(Debug, Clone, Copy, PartialEq)]
enum MidiIn {
    /// For the engine, on a MIDI channel.
    Live(LiveKind, u8),
    /// For the UI (learn, bound parameters).
    Cc(MidiCc),
    /// The mod wheel or sustain: both.
    Both(LiveKind, MidiCc),
    /// All notes off (CC 123) or all sound off (CC 120).
    AllOff(u8),
}

/// Decodes one message; anything that is not a note, controller, bend or aftertouch is
/// ignored.
fn decode(msg: &[u8]) -> Option<MidiIn> {
    let (&status, data) = msg.split_first()?;
    let channel = status & 0x0F;
    let unit = |v: u8| f32::from(v & 0x7F) / 127.0;
    let live = |k| Some(MidiIn::Live(k, channel));
    match (status & 0xF0, data) {
        (0x90, &[key, vel, ..]) if vel > 0 => {
            live(LiveKind::Note(LiveNote::on(key & 0x7F, unit(vel))))
        }
        (0x80 | 0x90, &[key, _, ..]) => live(LiveKind::Note(LiveNote::off(key & 0x7F))),
        (0xE0, &[lsb, msb, ..]) => {
            let v = (i32::from(msb & 0x7F) << 7 | i32::from(lsb & 0x7F)) - 8192;
            live(LiveKind::Bend((v as f32 / 8191.0).clamp(-1.0, 1.0)))
        }
        (0xD0, &[v, ..]) => live(LiveKind::Pressure(unit(v))),
        (0xA0, &[key, v, ..]) => live(LiveKind::KeyPressure {
            key: key & 0x7F,
            value: unit(v),
        }),
        (0xB0, &[120 | 123, ..]) => Some(MidiIn::AllOff(channel)),
        (0xB0, &[cc, value, ..]) => {
            let c = MidiCc {
                channel,
                cc: cc & 0x7F,
                value: value & 0x7F,
                at_ns: 0,
            };
            match c.cc {
                1 => Some(MidiIn::Both(LiveKind::ModWheel(unit(value)), c)),
                64 => Some(MidiIn::Both(LiveKind::Sustain(c.value >= 64), c)),
                _ => Some(MidiIn::Cc(c)),
            }
        }
        _ => None,
    }
}

struct Shared {
    stop: AtomicBool,
    /// Ports the user switched off, by name.
    disabled: Mutex<HashSet<String>>,
    /// Ports last seen, with whether they are connected.
    ports: Mutex<Vec<(String, bool)>>,
    /// Messages received (any port), for the activity light.
    activity: AtomicU32,
    /// Why MIDI is unavailable, if it is.
    error: Mutex<Option<String>>,
    /// Port names in the order they were first seen: a port's number is its index.
    numbers: Mutex<Vec<String>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The MIDI input side: owns the scanning thread and the controller queue.
pub struct MidiIo {
    shared: Arc<Shared>,
    cc: Receiver<MidiCc>,
    thread: Option<JoinHandle<()>>,
}

impl MidiIo {
    /// Starts scanning and connecting. Notes go to `live`; `ctx` is woken for controllers.
    pub fn start(live: LiveInput, ctx: egui::Context) -> Self {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            disabled: Mutex::new(HashSet::new()),
            ports: Mutex::new(Vec::new()),
            activity: AtomicU32::new(0),
            error: Mutex::new(None),
            numbers: Mutex::new(Vec::new()),
        });
        let (tx, rx) = channel();
        let s = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("midi-scan".to_owned())
            .spawn(move || scan_loop(&s, &live, &tx, &ctx))
            .map_err(|e| *lock(&shared.error) = Some(format!("cannot start MIDI: {e}")))
            .ok();
        Self {
            shared,
            cc: rx,
            thread,
        }
    }

    /// The input ports, as of the last scan.
    pub fn ports(&self) -> Vec<MidiPortInfo> {
        let disabled = lock(&self.shared.disabled).clone();
        lock(&self.shared.ports)
            .iter()
            .map(|(name, connected)| MidiPortInfo {
                enabled: !disabled.contains(name),
                connected: *connected,
                name: name.clone(),
            })
            .collect()
    }

    /// Switches a port on or off (applied at the next scan).
    pub fn set_enabled(&self, name: &str, on: bool) {
        let mut d = lock(&self.shared.disabled);
        if on {
            d.remove(name);
        } else {
            d.insert(name.to_owned());
        }
    }

    /// The ports the user switched off, by name (sorted, for the settings file).
    pub fn disabled_ports(&self) -> Vec<String> {
        let mut v: Vec<String> = lock(&self.shared.disabled).iter().cloned().collect();
        v.sort();
        v
    }

    /// Controller messages received since the last call.
    pub fn take_cc(&self) -> Vec<MidiCc> {
        self.cc.try_iter().collect()
    }

    /// The number live events from port `name` carry, once it has been seen.
    pub fn port_number(&self, name: &str) -> Option<usize> {
        lock(&self.shared.numbers).iter().position(|n| n == name)
    }

    /// Count of messages received so far (changes when anything arrives).
    pub fn activity(&self) -> u32 {
        self.shared.activity.load(Ordering::Relaxed)
    }

    /// Why MIDI input is unavailable, if it is.
    pub fn error(&self) -> Option<String> {
        lock(&self.shared.error).clone()
    }
}

impl Drop for MidiIo {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Keys held on one connection by MIDI channel, to release them on "all notes off" or
/// disconnect.
struct PortState {
    number: u8,
    held: [[bool; 128]; 16],
}

impl PortState {
    fn new(number: u8) -> Self {
        Self {
            number,
            held: [[false; 128]; 16],
        }
    }
}

fn scan_loop(shared: &Arc<Shared>, live: &LiveInput, tx: &Sender<MidiCc>, ctx: &egui::Context) {
    let mut scanner: Option<MidiInput> = None;
    let mut conns: HashMap<String, MidiInputConnection<PortState>> = HashMap::new();
    while !shared.stop.load(Ordering::Relaxed) {
        let mut wait = SCAN_EVERY;
        if scanner.is_none() {
            match MidiInput::new(CLIENT_NAME) {
                Ok(s) => {
                    scanner = Some(s);
                    *lock(&shared.error) = None;
                }
                Err(e) => {
                    *lock(&shared.error) = Some(format!("MIDI is not available: {e}"));
                    wait = RETRY_EVERY;
                }
            }
        }
        if let Some(scanner) = &scanner {
            // Names make ports recognizable across scans; identical devices get " #2" etc.
            let mut seen: Vec<(String, midir::MidiInputPort)> = Vec::new();
            for port in scanner.ports() {
                let Ok(base) = scanner.port_name(&port) else {
                    continue;
                };
                let mut name = base.clone();
                let mut n = 2;
                while seen.iter().any(|(s, _)| *s == name) {
                    name = format!("{base} #{n}");
                    n += 1;
                }
                seen.push((name, port));
            }
            let disabled = lock(&shared.disabled).clone();
            let gone: Vec<String> = conns
                .keys()
                .filter(|k| disabled.contains(*k) || !seen.iter().any(|(s, _)| s == *k))
                .cloned()
                .collect();
            for name in gone {
                if let Some(c) = conns.remove(&name) {
                    let (_, mut state) = c.close();
                    release_held(live, &mut state, None);
                    log::info!("MIDI input closed: {name}");
                }
            }
            for (name, port) in &seen {
                if conns.contains_key(name) || disabled.contains(name) {
                    continue;
                }
                let number = {
                    let mut numbers = lock(&shared.numbers);
                    numbers.iter().position(|n| n == name).unwrap_or_else(|| {
                        numbers.push(name.clone());
                        numbers.len() - 1
                    })
                };
                let number = u8::try_from(number).unwrap_or(u8::MAX - 1);
                match connect(port, number, shared, live, tx, ctx) {
                    Ok(c) => {
                        log::info!("MIDI input opened: {name}");
                        conns.insert(name.clone(), c);
                    }
                    Err(e) => log::warn!("cannot open MIDI input {name}: {e}"),
                }
            }
            *lock(&shared.ports) = seen
                .iter()
                .map(|(s, _)| (s.clone(), conns.contains_key(s)))
                .collect();
        }
        let mut waited = Duration::ZERO;
        while waited < wait && !shared.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
            waited += Duration::from_millis(50);
        }
    }
    for (_, c) in conns.drain() {
        let (_, mut state) = c.close();
        release_held(live, &mut state, None);
    }
}

/// Releases the keys held on `channel` (all channels for `None`) and lifts the pedal.
fn release_held(live: &LiveInput, state: &mut PortState, channel: Option<u8>) {
    for (c, keys) in state.held.iter_mut().enumerate() {
        if channel.is_some_and(|ch| usize::from(ch) != c) {
            continue;
        }
        for (k, h) in keys.iter_mut().enumerate() {
            if std::mem::take(h) {
                live.send(LiveEvent::from_port(
                    LiveKind::Note(LiveNote::off(k as u8)),
                    state.number,
                    c as u8,
                ));
            }
        }
        live.send(LiveEvent::from_port(
            LiveKind::Sustain(false),
            state.number,
            c as u8,
        ));
    }
}

fn connect(
    port: &midir::MidiInputPort,
    number: u8,
    shared: &Arc<Shared>,
    live: &LiveInput,
    tx: &Sender<MidiCc>,
    ctx: &egui::Context,
) -> Result<MidiInputConnection<PortState>, String> {
    let mut input = MidiInput::new(CLIENT_NAME).map_err(|e| e.to_string())?;
    // SysEx, clock and active sensing are of no use here.
    input.ignore(Ignore::All);
    let (live, tx, ctx, shared) = (live.clone(), tx.clone(), ctx.clone(), Arc::clone(shared));
    input
        .connect(
            port,
            &format!("{CLIENT_NAME} in"),
            move |_stamp, msg, state: &mut PortState| {
                shared.activity.fetch_add(1, Ordering::Relaxed);
                let to_ui = |cc: MidiCc| {
                    let cc = MidiCc {
                        at_ns: gt_engine::clock_ns(),
                        ..cc
                    };
                    if tx.send(cc).is_ok() {
                        ctx.request_repaint();
                    }
                };
                match decode(msg) {
                    Some(MidiIn::Live(kind, channel)) => {
                        if let LiveKind::Note(n) = kind {
                            state.held[usize::from(channel)][usize::from(n.key)] = n.is_on();
                        }
                        live.send(LiveEvent::from_port(kind, state.number, channel));
                    }
                    Some(MidiIn::Cc(cc)) => to_ui(cc),
                    Some(MidiIn::Both(kind, cc)) => {
                        live.send(LiveEvent::from_port(kind, state.number, cc.channel));
                        to_ui(cc);
                    }
                    Some(MidiIn::AllOff(channel)) => release_held(&live, state, Some(channel)),
                    None => {}
                }
            },
            PortState::new(number),
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_decode_to_notes_controllers_and_expression() {
        let note = |n, ch| Some(MidiIn::Live(LiveKind::Note(n), ch));
        assert_eq!(decode(&[0x90, 60, 127]), note(LiveNote::on(60, 1.0), 0));
        // Note-on with velocity 0 is a note-off (running-status keyboards send these).
        assert_eq!(decode(&[0x93, 60, 0]), note(LiveNote::off(60), 3));
        assert_eq!(decode(&[0x80, 61, 40]), note(LiveNote::off(61), 0));
        assert_eq!(
            decode(&[0xB2, 74, 100]),
            Some(MidiIn::Cc(MidiCc {
                channel: 2,
                cc: 74,
                value: 100,
                at_ns: 0,
            }))
        );
        assert_eq!(decode(&[0xB5, 123, 0]), Some(MidiIn::AllOff(5)));
        // Pitch bend: centre, full up, full down.
        assert_eq!(
            decode(&[0xE1, 0, 64]),
            Some(MidiIn::Live(LiveKind::Bend(0.0), 1))
        );
        assert_eq!(
            decode(&[0xE0, 0x7F, 0x7F]),
            Some(MidiIn::Live(LiveKind::Bend(1.0), 0))
        );
        assert_eq!(
            decode(&[0xE0, 0, 0]),
            Some(MidiIn::Live(LiveKind::Bend(-1.0), 0))
        );
        assert!(matches!(
            decode(&[0xB0, 64, 127]),
            Some(MidiIn::Both(LiveKind::Sustain(true), _))
        ));
        assert!(matches!(
            decode(&[0xB0, 64, 10]),
            Some(MidiIn::Both(LiveKind::Sustain(false), _))
        ));
        assert!(matches!(
            decode(&[0xB0, 1, 127]),
            Some(MidiIn::Both(LiveKind::ModWheel(v), _)) if v == 1.0
        ));
        assert_eq!(
            decode(&[0xD4, 127]),
            Some(MidiIn::Live(LiveKind::Pressure(1.0), 4))
        );
        assert_eq!(
            decode(&[0xA0, 60, 0]),
            Some(MidiIn::Live(
                LiveKind::KeyPressure {
                    key: 60,
                    value: 0.0
                },
                0
            ))
        );
        // Program change, clock, short messages: ignored.
        assert_eq!(decode(&[0xC0, 5]), None);
        assert_eq!(decode(&[0xF8]), None);
        assert_eq!(decode(&[0x90, 60]), None);
        assert_eq!(decode(&[]), None);
    }
}
