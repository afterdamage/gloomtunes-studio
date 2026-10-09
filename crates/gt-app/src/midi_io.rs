//! MIDI input devices (midir), with hot-plug.
//!
//! A background thread lists the input ports once a second, connects to every new one the
//! user has not switched off and drops the ones that disappeared, on Windows and Linux alike
//! (neither backend reports plugging reliably, so polling is the portable way). Each
//! connection's callback runs on a MIDI thread owned by midir: notes go straight to the
//! engine through the shared [`LiveInput`], controller moves go to the UI thread (MIDI learn
//! and bound parameters) with a repaint request.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use gt_engine::{LiveInput, LiveNote};
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
    Note(LiveNote),
    Cc(MidiCc),
    /// All notes off (CC 123) or all sound off (CC 120).
    AllOff,
}

/// Decodes one message; anything that is not a note or controller is ignored.
fn decode(msg: &[u8]) -> Option<MidiIn> {
    let (&status, data) = msg.split_first()?;
    let channel = status & 0x0F;
    match (status & 0xF0, data) {
        (0x90, &[key, vel, ..]) if vel > 0 => Some(MidiIn::Note(LiveNote::on(
            key & 0x7F,
            f32::from(vel & 0x7F) / 127.0,
        ))),
        (0x80 | 0x90, &[key, _, ..]) => Some(MidiIn::Note(LiveNote::off(key & 0x7F))),
        (0xB0, &[120 | 123, ..]) => Some(MidiIn::AllOff),
        (0xB0, &[cc, value, ..]) => Some(MidiIn::Cc(MidiCc {
            channel,
            cc: cc & 0x7F,
            value: value & 0x7F,
        })),
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

    /// Controller messages received since the last call.
    pub fn take_cc(&self) -> Vec<MidiCc> {
        self.cc.try_iter().collect()
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

/// Keys held on one connection, to release them on "all notes off" or disconnect.
struct PortState {
    held: [bool; 128],
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
                    let (_, state) = c.close();
                    release_held(live, &state);
                    log::info!("MIDI input closed: {name}");
                }
            }
            for (name, port) in &seen {
                if conns.contains_key(name) || disabled.contains(name) {
                    continue;
                }
                match connect(port, shared, live, tx, ctx) {
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
        let (_, state) = c.close();
        release_held(live, &state);
    }
}

fn release_held(live: &LiveInput, state: &PortState) {
    for (k, &h) in state.held.iter().enumerate() {
        if h {
            live.send(LiveNote::off(k as u8));
        }
    }
}

fn connect(
    port: &midir::MidiInputPort,
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
                match decode(msg) {
                    Some(MidiIn::Note(n)) => {
                        state.held[usize::from(n.key)] = n.is_on();
                        live.send(n);
                    }
                    Some(MidiIn::Cc(cc)) => {
                        if tx.send(cc).is_ok() {
                            ctx.request_repaint();
                        }
                    }
                    Some(MidiIn::AllOff) => {
                        release_held(&live, state);
                        state.held = [false; 128];
                    }
                    None => {}
                }
            },
            PortState { held: [false; 128] },
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_decode_to_notes_and_controllers() {
        assert_eq!(
            decode(&[0x90, 60, 127]),
            Some(MidiIn::Note(LiveNote::on(60, 1.0)))
        );
        // Note-on with velocity 0 is a note-off (running-status keyboards send these).
        assert_eq!(
            decode(&[0x93, 60, 0]),
            Some(MidiIn::Note(LiveNote::off(60)))
        );
        assert_eq!(
            decode(&[0x80, 61, 40]),
            Some(MidiIn::Note(LiveNote::off(61)))
        );
        assert_eq!(
            decode(&[0xB2, 74, 100]),
            Some(MidiIn::Cc(MidiCc {
                channel: 2,
                cc: 74,
                value: 100
            }))
        );
        assert_eq!(decode(&[0xB0, 123, 0]), Some(MidiIn::AllOff));
        // Pitch bend, program change, clock, short messages: ignored.
        assert_eq!(decode(&[0xE0, 0, 64]), None);
        assert_eq!(decode(&[0xC0, 5]), None);
        assert_eq!(decode(&[0xF8]), None);
        assert_eq!(decode(&[0x90, 60]), None);
        assert_eq!(decode(&[]), None);
    }
}
