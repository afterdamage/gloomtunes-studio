//! The app side of a bridged plugin: the instance (control over the child's pipes) and the
//! processor the engine drives (shared memory).

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gt_core::PluginParamInfo;
use gt_engine::{NoteExprKind, PluginContext, PluginProcessor};

use super::shm::{ev, Shm, Word, MAX_EVENTS};
use super::{decode, encode, Message, Reply, Request, SignalKind, BRIDGE_SWITCH};
use crate::backend::{Activated, Instance};
use crate::catalog::PluginInfo;
use crate::host::Signals;
use crate::processor::ParamOut;

/// How long a block may take in the child before it counts as failed.
const BLOCK_TIMEOUT: Duration = Duration::from_millis(20);
/// The same while exporting (offline rendering may take its time).
const OFFLINE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the child may take to load a plugin, and to answer anything else.
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// New children after crashes, per instance.
const MAX_RESPAWNS: u32 = 3;
const PARAM_QUEUE: usize = 1024;
/// Blocks a child may stay behind before the plugin counts as failed (about a second).
const MAX_LATE: u32 = 1500;

/// How to start a bridge child.
#[derive(Debug, Clone)]
pub(crate) struct BridgeCommand {
    pub(crate) exe: PathBuf,
    pub(crate) args: Vec<OsString>,
    pub(crate) env: Vec<(OsString, OsString)>,
}

impl BridgeCommand {
    /// `exe --plugin-bridge`.
    pub(crate) fn app(exe: PathBuf) -> Self {
        Self {
            exe,
            args: vec![BRIDGE_SWITCH.into()],
            env: Vec::new(),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the child's reader thread shares with the instance.
#[derive(Default)]
struct Inbox {
    params: Mutex<Option<rtrb::Producer<ParamOut>>>,
    edits: Mutex<Vec<(usize, f32)>>,
    editor_open: AtomicBool,
}

/// One running child.
struct Process {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<(u64, Reply)>,
    next_id: u64,
}

impl Process {
    fn spawn(
        cmd: &BridgeCommand,
        inbox: &Arc<Inbox>,
        signals: &Arc<Signals>,
    ) -> Result<Self, String> {
        let mut child = Command::new(&cmd.exe)
            .args(&cmd.args)
            .envs(cmd.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("cannot start the plugin process: {e}"))?;
        let stdin = child.stdin.take().ok_or("no pipe to the plugin process")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("no pipe from the plugin process")?;
        let (tx, replies) = channel();
        let (inbox, signals) = (Arc::clone(inbox), Arc::clone(signals));
        std::thread::Builder::new()
            .name("plugin-bridge-in".to_owned())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    let Some((_, msg)) = decode::<Message>(&line) else {
                        continue;
                    };
                    match msg {
                        Message::Reply(id, r) => {
                            let _ = tx.send((id, r));
                        }
                        Message::Param(index, value) => {
                            if let Some(p) = lock(&inbox.params).as_mut() {
                                let _ = p.push(ParamOut { index, value });
                            }
                            signals.wake();
                        }
                        Message::Edit(i, v) => {
                            lock(&inbox.edits).push((i as usize, v));
                            signals.wake();
                        }
                        Message::EditorOpen(open) => {
                            inbox.editor_open.store(open, Ordering::Relaxed);
                            if !open {
                                signals.raise_flag(&signals.gui_closed);
                            }
                        }
                        Message::Signal(k) => signals.raise_flag(match k {
                            SignalKind::Restart => &signals.restart,
                            SignalKind::Latency => &signals.latency,
                            SignalKind::RescanParams => &signals.rescan_params,
                            SignalKind::Dirty => &signals.dirty,
                        }),
                    }
                }
                // The child is gone: let the UI notice soon.
                signals.wake();
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            child,
            stdin,
            replies,
            next_id: 0,
        })
    }

    fn send(&mut self, req: &Request) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        let line = encode(req.replies().then_some(id), req);
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|_| "the plugin process stopped".to_owned())?;
        Ok(id)
    }

    fn call(&mut self, req: &Request, timeout: Duration) -> Result<Reply, String> {
        let id = self.send(req)?;
        let until = Instant::now() + timeout;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.replies.recv_timeout(left) {
                Ok((got, Reply::Failed(e))) if got == id => return Err(e),
                Ok((got, r)) if got == id => return Ok(r),
                Ok(_) => continue, // a late reply to a request that timed out
                Err(RecvTimeoutError::Timeout) => {
                    // Hung: it will not be trusted again.
                    let _ = self.child.kill();
                    return Err("the plugin process stopped answering".to_owned());
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("the plugin process stopped".to_owned())
                }
            }
        }
    }

    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.send(&Request::Quit);
        let until = Instant::now() + Duration::from_millis(500);
        while Instant::now() < until {
            if !self.running() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A plugin instance running in a bridge child.
pub(crate) struct BridgeInstance {
    info: PluginInfo,
    signals: Arc<Signals>,
    cmd: BridgeCommand,
    inbox: Arc<Inbox>,
    process: Option<Process>,
    has_editor: bool,
    shm: Option<Arc<Shm>>,
    offline: bool,
    /// The last state saved or loaded, for a new child after a crash.
    state: Option<Vec<u8>>,
    respawns: u32,
    notice: Option<String>,
}

impl BridgeInstance {
    /// Starts a child and loads `info` in it.
    pub(crate) fn spawn(
        cmd: &BridgeCommand,
        info: &PluginInfo,
        signals: Arc<Signals>,
    ) -> Result<Self, String> {
        let mut b = Self {
            info: info.clone(),
            signals,
            cmd: cmd.clone(),
            inbox: Arc::new(Inbox::default()),
            process: None,
            has_editor: false,
            shm: None,
            offline: false,
            state: None,
            respawns: 0,
            notice: None,
        };
        b.start()?;
        Ok(b)
    }

    fn start(&mut self) -> Result<(), String> {
        let mut p = Process::spawn(&self.cmd, &self.inbox, &self.signals)?;
        match p.call(&Request::Load(self.info.clone()), LOAD_TIMEOUT)? {
            Reply::Loaded { has_editor } => self.has_editor = has_editor,
            r => return Err(format!("unexpected reply {r:?}")),
        }
        if let Some(s) = &self.state {
            p.call(&Request::LoadState(s.clone()), REPLY_TIMEOUT)?;
        }
        if self.offline {
            p.call(&Request::SetOffline(true), REPLY_TIMEOUT)?;
        }
        self.process = Some(p);
        Ok(())
    }

    fn call(&mut self, req: &Request) -> Result<Reply, String> {
        let p = self
            .process
            .as_mut()
            .ok_or_else(|| format!("{} is not running", self.info.name))?;
        p.call(req, REPLY_TIMEOUT)
    }

    /// A dead child: stop the engine waiting, and start a new one.
    fn check_child(&mut self) {
        let Some(p) = self.process.as_mut() else {
            return;
        };
        if p.running() {
            return;
        }
        let code = p.child.try_wait().ok().flatten();
        self.process = None;
        if let Some(shm) = &self.shm {
            shm.set(Word::Dead, 1);
        }
        let name = self.info.name.clone();
        log::warn!("the process of plugin {name} stopped ({code:?})");
        if self.respawns >= MAX_RESPAWNS {
            self.notice = Some(format!(
                "{name} crashed again in its own process and stays off (the app is fine)"
            ));
            return;
        }
        self.respawns += 1;
        match self.start() {
            Ok(()) => {
                self.notice = Some(format!(
                    "{name} crashed in its own process; it was started again"
                ));
                // The manager swaps in a processor of the new child.
                self.signals.raise_flag(&self.signals.restart);
            }
            Err(e) => self.notice = Some(format!("{name} crashed and cannot start again: {e}")),
        }
    }
}

impl Instance for BridgeInstance {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn signals(&self) -> &Arc<Signals> {
        &self.signals
    }

    fn read_params(&mut self) -> (Vec<PluginParamInfo>, Vec<f32>) {
        match self.call(&Request::ReadParams) {
            Ok(Reply::Params(p, v)) => (p.into_iter().map(Into::into).collect(), v),
            _ => (Vec::new(), Vec::new()),
        }
    }

    fn value_text(&mut self, index: usize, value: f32) -> Option<String> {
        match self.call(&Request::ValueText(index as u32, value)) {
            Ok(Reply::Text(t)) => t,
            _ => None,
        }
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        match self.call(&Request::SaveState) {
            Ok(Reply::State(s)) => {
                if s.is_some() {
                    self.state.clone_from(&s);
                }
                s
            }
            // Gone: the last state known.
            _ => self.state.clone(),
        }
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), String> {
        self.state = Some(state.to_vec());
        self.call(&Request::LoadState(state.to_vec())).map(drop)
    }

    fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
        let _ = self.call(&Request::SetOffline(offline));
    }

    fn activate(&mut self, sample_rate: u32) -> Result<Activated, String> {
        let shm = Arc::new(Shm::create()?);
        let reply = self.call(&Request::Activate {
            rate: sample_rate,
            shm: shm.path().to_owned(),
        })?;
        let Reply::Activated { latency } = reply else {
            return Err(format!("unexpected reply {reply:?}"));
        };
        // Both sides have it mapped: the name is no longer needed (Unix).
        shm.unlink();
        let (tx, rx) = rtrb::RingBuffer::new(PARAM_QUEUE);
        *lock(&self.inbox.params) = Some(tx);
        self.shm = Some(Arc::clone(&shm));
        let processor = BridgeProcessor {
            shm,
            queue: Box::new([[0; 4]; MAX_EVENTS]),
            events: 0,
            late: 0,
            reset: false,
            latency,
            seq: 0,
            timeout: if self.offline {
                OFFLINE_TIMEOUT
            } else {
                BLOCK_TIMEOUT
            },
        };
        Ok((Box::new(processor), rx))
    }

    fn try_deactivate(&mut self) -> bool {
        if let Some(shm) = &self.shm {
            if Arc::strong_count(shm) > 1 {
                return false;
            }
            if self.process.is_some() {
                let _ = self.call(&Request::Deactivate);
            }
            self.shm = None;
            *lock(&self.inbox.params) = None;
        }
        true
    }

    fn main_thread_work(&mut self, _now: Instant) {
        self.check_child();
    }

    fn next_frame(&self, _now: Instant) -> Option<Duration> {
        // Look for a dead child now and then.
        self.process.as_ref().map(|_| Duration::from_millis(100))
    }

    fn host_changed(&mut self, index: usize, value: f32) {
        if let Some(p) = self.process.as_mut() {
            let _ = p.send(&Request::HostChanged(index as u32, value));
        }
    }

    fn plugin_reported(&mut self, index: usize, value: f32) {
        if let Some(p) = self.process.as_mut() {
            let _ = p.send(&Request::PluginReported(index as u32, value));
        }
    }

    fn take_edits(&mut self) -> Vec<(usize, f32)> {
        std::mem::take(&mut *lock(&self.inbox.edits))
    }

    fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    fn has_editor(&self) -> bool {
        self.has_editor
    }

    fn editor_open(&self) -> bool {
        self.process.is_some() && self.inbox.editor_open.load(Ordering::Relaxed)
    }

    fn open_editor(&mut self, title: &str) -> Result<(), String> {
        self.call(&Request::OpenEditor(title.to_owned())).map(drop)
    }

    fn close_editor(&mut self) {
        if self.editor_open() {
            let _ = self.call(&Request::CloseEditor);
        }
    }

    fn poll_editor(&mut self) {}
}

/// The engine's side of a bridged plugin. Events are written straight into the shared
/// block; `process` hands the block over and waits for it.
pub(crate) struct BridgeProcessor {
    shm: Arc<Shm>,
    /// Events for the next block.
    queue: Box<[[u32; 4]; MAX_EVENTS]>,
    events: usize,
    /// Blocks since the child fell behind (0: in step).
    late: u32,
    reset: bool,
    latency: usize,
    seq: u32,
    timeout: Duration,
}

impl BridgeProcessor {
    fn push(&mut self, e: [u32; 4]) {
        if let Some(slot) = self.queue.get_mut(self.events) {
            *slot = e;
            self.events += 1;
        }
    }
}

impl PluginProcessor for BridgeProcessor {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &PluginContext) -> bool {
        let shm = &*self.shm;
        if shm.get(Word::Dead) != 0 {
            return false;
        }
        if shm.done() != self.seq {
            // Still on a late block: dry, unless it never ends.
            self.late += 1;
            return self.late < MAX_LATE;
        }
        self.late = 0;
        let n = l.len().min(r.len()).min(gt_engine::RENDER_QUANTUM);
        for (k, e) in self.queue[..self.events].iter().enumerate() {
            shm.write_event(k, *e);
        }
        shm.set(Word::Frames, n as u32);
        shm.set(Word::Events, self.events as u32);
        shm.set(Word::Reset, u32::from(std::mem::take(&mut self.reset)));
        shm.write_context(ctx);
        shm.write_audio(&l[..n], &r[..n]);
        self.events = 0;
        self.seq = self.seq.wrapping_add(1);
        shm.request(self.seq);
        let start = Instant::now();
        let mut spins = 0u32;
        while shm.done() != self.seq {
            spins += 1;
            if spins < 4000 {
                std::hint::spin_loop();
                continue;
            }
            if shm.get(Word::Dead) != 0 {
                return false;
            }
            if start.elapsed() > self.timeout {
                // Late: this block stays dry; the next ones too, without waiting, until the
                // child catches up (or the app finds it dead).
                self.late = 1;
                return true;
            }
            std::thread::yield_now();
        }
        shm.read_audio(&mut l[..n], &mut r[..n]);
        shm.get(Word::Ok) == 1
    }

    fn note_on(&mut self, offset: u32, key: u8, velocity: f32) {
        self.push([ev::NOTE_ON, offset, u32::from(key), velocity.to_bits()]);
    }

    fn note_off(&mut self, offset: u32, key: u8) {
        self.push([ev::NOTE_OFF, offset, u32::from(key), 0]);
    }

    fn release_all(&mut self, offset: u32) {
        self.push([ev::RELEASE_ALL, offset, 0, 0]);
    }

    fn midi(&mut self, offset: u32, msg: [u8; 3]) {
        let m = u32::from(msg[0]) | u32::from(msg[1]) << 8 | u32::from(msg[2]) << 16;
        self.push([ev::MIDI, offset, m, 0]);
    }

    fn key_pressure(&mut self, offset: u32, key: u8, value: f32) {
        self.push([ev::KEY_PRESSURE, offset, u32::from(key), value.to_bits()]);
    }

    fn note_expression(&mut self, offset: u32, key: u8, kind: NoteExprKind, value: f32) {
        let k = match kind {
            NoteExprKind::Tuning => 0,
            NoteExprKind::Pressure => 1,
            NoteExprKind::Timbre => 2,
        };
        self.push([
            ev::NOTE_EXPR,
            offset,
            k << 8 | u32::from(key),
            value.to_bits(),
        ]);
    }

    fn set_param(&mut self, index: u32, value: f32) {
        self.push([ev::PARAM, 0, index, value.to_bits()]);
    }

    fn reset(&mut self) {
        self.reset = true;
    }

    fn latency(&self) -> usize {
        self.latency
    }
}
