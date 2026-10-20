//! MIDI output ports (Prompt 33, D148): sends what the engine queued for MIDI out channels,
//! clock and time code, each message at the time stamped on it.
//!
//! One thread owns every output connection. It lists the ports once a second, takes the
//! engine's messages as they come, keeps them in time order and sends each one when the
//! [`gt_engine::clock_ns`] clock reaches its time (sleeping in steps of at most a millisecond
//! in between). A channel's port is looked up by its slot in a table the UI keeps current;
//! clock and time code go to every port ticked for them. Connections open the first time a
//! port is needed and close when it disappears.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gt_engine::midi_out::{DEST_CLOCK, DEST_MTC};
use gt_engine::{clock_ns, MidiOutEvent};
use midir::{MidiOutput, MidiOutputConnection};
use rtrb::Consumer;

const CLIENT_NAME: &str = "GloomTunes Studio";
const SCAN_EVERY: Duration = Duration::from_secs(1);
/// Longest sleep between checks for due messages.
const STEP: Duration = Duration::from_millis(1);
/// Messages this close to their time are sent now.
const EARLY_NS: u64 = 300_000;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    /// The engine's queue (a new engine brings a new one).
    queue: Mutex<Option<Consumer<MidiOutEvent>>>,
    /// Port name by channel slot ("" or missing: nowhere).
    slots: Mutex<Vec<String>>,
    /// Ports that get clock, and time code.
    clock: Mutex<HashSet<String>>,
    mtc: Mutex<HashSet<String>>,
    /// Ports found by the last scan.
    ports: Mutex<Vec<String>>,
    /// Why MIDI output is unavailable, if it is.
    error: Mutex<Option<String>>,
}

/// The MIDI output side.
pub struct MidiOutIo {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl MidiOutIo {
    /// Starts the output thread.
    pub fn start() -> Self {
        let shared = Arc::new(Shared::default());
        let s = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("midi-out".to_owned())
            .spawn(move || run(&s))
            .map_err(|e| *lock(&shared.error) = Some(format!("cannot start MIDI out: {e}")))
            .ok();
        Self { shared, thread }
    }

    /// Takes messages from a new engine from now on.
    pub fn connect(&self, queue: Consumer<MidiOutEvent>) {
        *lock(&self.shared.queue) = Some(queue);
    }

    /// The output ports, as of the last scan.
    pub fn ports(&self) -> Vec<String> {
        lock(&self.shared.ports).clone()
    }

    /// Why MIDI output is unavailable, if it is.
    pub fn error(&self) -> Option<String> {
        lock(&self.shared.error).clone()
    }

    /// The port each channel slot plays.
    pub fn set_slots(&self, ports: Vec<String>) {
        let mut s = lock(&self.shared.slots);
        if *s != ports {
            *s = ports;
        }
    }

    /// The ports that get clock and time code.
    pub fn set_sync_ports(&self, clock: &[String], mtc: &[String]) {
        *lock(&self.shared.clock) = clock.iter().cloned().collect();
        *lock(&self.shared.mtc) = mtc.iter().cloned().collect();
    }
}

impl Drop for MidiOutIo {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Open connections by port name, opened on first use.
struct Ports {
    found: Vec<(String, midir::MidiOutputPort)>,
    open: HashMap<String, MidiOutputConnection>,
    /// Ports that failed to open, not retried until the next scan.
    failed: HashSet<String>,
}

impl Ports {
    fn send(&mut self, name: &str, msg: &[u8]) {
        if !self.open.contains_key(name) && !self.failed.contains(name) {
            let Some(port) = self.found.iter().find(|(n, _)| n == name).map(|(_, p)| p) else {
                return;
            };
            let opened = MidiOutput::new(CLIENT_NAME)
                .map_err(|e| e.to_string())
                .and_then(|o| {
                    o.connect(port, &format!("{CLIENT_NAME} out"))
                        .map_err(|e| e.to_string())
                });
            match opened {
                Ok(c) => {
                    log::info!("MIDI output opened: {name}");
                    self.open.insert(name.to_owned(), c);
                }
                Err(e) => {
                    log::warn!("cannot open MIDI output {name}: {e}");
                    self.failed.insert(name.to_owned());
                    return;
                }
            }
        }
        if let Some(c) = self.open.get_mut(name) {
            if let Err(e) = c.send(msg) {
                log::warn!("MIDI output {name}: {e}");
            }
        }
    }
}

fn run(shared: &Arc<Shared>) {
    let mut scanner: Option<MidiOutput> = None;
    let mut ports = Ports {
        found: Vec::new(),
        open: HashMap::new(),
        failed: HashSet::new(),
    };
    let mut pending: Vec<MidiOutEvent> = Vec::new();
    let mut next_scan = Instant::now();
    while !shared.stop.load(Ordering::Relaxed) {
        if Instant::now() >= next_scan {
            next_scan = Instant::now() + SCAN_EVERY;
            scan(shared, &mut scanner, &mut ports);
        }
        if let Some(q) = lock(&shared.queue).as_mut() {
            while let Ok(e) = q.pop() {
                // Mostly in order already: insert from the back.
                let at = pending.partition_point(|p| p.at_ns <= e.at_ns);
                pending.insert(at, e);
            }
        }
        let now = clock_ns();
        let due = pending.partition_point(|p| p.at_ns <= now + EARLY_NS);
        if due > 0 {
            let slots = lock(&shared.slots).clone();
            let clock = lock(&shared.clock).clone();
            let mtc = lock(&shared.mtc).clone();
            for e in pending.drain(..due) {
                match e.dest {
                    DEST_CLOCK => clock.iter().for_each(|p| ports.send(p, e.message())),
                    DEST_MTC => mtc.iter().for_each(|p| ports.send(p, e.message())),
                    slot => {
                        if let Some(p) = slots.get(usize::from(slot)) {
                            ports.send(p, e.message());
                        }
                    }
                }
            }
        }
        let wait = pending
            .first()
            .map_or(STEP, |p| {
                Duration::from_nanos(p.at_ns.saturating_sub(clock_ns() + EARLY_NS))
            })
            .min(STEP);
        std::thread::sleep(wait.max(Duration::from_micros(100)));
    }
    // Nothing keeps sounding on the devices after the app closes.
    for c in ports.open.values_mut() {
        for ch in 0..16u8 {
            let _ = c.send(&[0xB0 | ch, 123, 0]);
        }
    }
}

fn scan(shared: &Shared, scanner: &mut Option<MidiOutput>, ports: &mut Ports) {
    if scanner.is_none() {
        match MidiOutput::new(CLIENT_NAME) {
            Ok(s) => {
                *scanner = Some(s);
                *lock(&shared.error) = None;
            }
            Err(e) => {
                *lock(&shared.error) = Some(format!("MIDI output is not available: {e}"));
                return;
            }
        }
    }
    let Some(s) = scanner.as_ref() else {
        return;
    };
    let mut found: Vec<(String, midir::MidiOutputPort)> = Vec::new();
    for port in s.ports() {
        let Ok(base) = s.port_name(&port) else {
            continue;
        };
        let mut name = base.clone();
        let mut n = 2;
        while found.iter().any(|(s, _)| *s == name) {
            name = format!("{base} #{n}");
            n += 1;
        }
        found.push((name, port));
    }
    ports
        .open
        .retain(|name, _| found.iter().any(|(n, _)| n == name));
    ports.failed.clear();
    *lock(&shared.ports) = found.iter().map(|(n, _)| n.clone()).collect();
    ports.found = found;
}
