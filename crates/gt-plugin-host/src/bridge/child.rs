//! The bridge child: loads one plugin and serves the app over stdin/stdout and the shared
//! block, until the app says quit or goes away.

use std::io::{BufRead, Write};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gt_engine::{NoteExprKind, PluginProcessor, RENDER_QUANTUM};

use super::shm::{ev, Shm, Word};
use super::{decode, encode, Message, ParamDto, Reply, Request, SignalKind};
use crate::backend::{Entry, Instance};
use crate::catalog::PluginInfo;
use crate::host::Signals;
use crate::processor::ParamOut;

/// The longest the main loop sleeps between plugin main-thread work.
const TICK: Duration = Duration::from_millis(10);

/// Runs the child: call from `main` when started with [`super::BRIDGE_SWITCH`]. Returns the
/// exit code.
pub fn bridge_child_main() -> i32 {
    serve(&|info: &PluginInfo| Entry::load(&info.path, info.format))
}

enum Input {
    Request(Option<u64>, Request),
    Wake,
    Closed,
}

/// The output pipe, kept apart from what plugins print.
fn protocol_out() -> Box<dyn Write> {
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;
        // SAFETY: duplicates stdout for the protocol, then points stdout at stderr, so a
        // plugin's printing cannot break a line of the protocol; the new descriptor is owned
        // by the returned file.
        #[allow(unsafe_code)]
        unsafe {
            let fd = libc::dup(1);
            if fd >= 0 && libc::dup2(2, 1) >= 0 {
                return Box::new(std::fs::File::from_raw_fd(fd));
            }
        }
    }
    Box::new(std::io::stdout())
}

/// The audio thread and what it shares.
struct Running {
    shm: Arc<Shm>,
    thread: JoinHandle<()>,
    params: rtrb::Consumer<ParamOut>,
}

struct Child {
    out: Box<dyn Write>,
    signals: Arc<Signals>,
    // Field order: the instance goes before the entry (its code lives in the entry).
    instance: Option<Box<dyn Instance>>,
    entry: Option<Entry>,
    running: Option<Running>,
    editor_open: bool,
}

impl Child {
    fn send(&mut self, m: &Message) {
        let _ = self
            .out
            .write_all(encode(None, m).as_bytes())
            .and_then(|()| self.out.flush());
    }

    fn instance(&mut self) -> Result<&mut Box<dyn Instance>, String> {
        self.instance
            .as_mut()
            .ok_or_else(|| "no plugin loaded".to_owned())
    }

    fn handle(
        &mut self,
        req: Request,
        load: &dyn Fn(&PluginInfo) -> Result<Entry, String>,
    ) -> Result<Reply, String> {
        Ok(match req {
            Request::Load(info) => {
                let entry = load(&info)?;
                let instance = entry.create(&info, Arc::clone(&self.signals))?;
                let has_editor = instance.has_editor();
                self.instance = Some(instance);
                self.entry = Some(entry);
                Reply::Loaded { has_editor }
            }
            Request::ReadParams => {
                let (p, v) = self.instance()?.read_params();
                Reply::Params(p.iter().map(ParamDto::from).collect(), v)
            }
            Request::ValueText(i, v) => Reply::Text(self.instance()?.value_text(i as usize, v)),
            Request::SaveState => Reply::State(self.instance()?.save_state()),
            Request::LoadState(s) => {
                self.instance()?.load_state(&s)?;
                Reply::Done
            }
            Request::SetOffline(o) => {
                self.instance()?.set_offline(o);
                Reply::Done
            }
            Request::Activate { rate, shm } => {
                self.deactivate();
                let shm = Arc::new(Shm::open(&shm)?);
                let (processor, params) = self.instance()?.activate(rate)?;
                let latency = processor.latency();
                let s = Arc::clone(&shm);
                let thread = std::thread::Builder::new()
                    .name("plugin-bridge-audio".to_owned())
                    .spawn(move || audio_loop(&s, processor))
                    .map_err(|e| e.to_string())?;
                self.running = Some(Running {
                    shm,
                    thread,
                    params,
                });
                Reply::Activated { latency }
            }
            Request::Deactivate => {
                self.deactivate();
                Reply::Done
            }
            Request::OpenEditor(title) => {
                self.instance()?.open_editor(&title)?;
                Reply::Done
            }
            Request::CloseEditor => {
                self.instance()?.close_editor();
                Reply::Done
            }
            Request::Quit => Reply::Done,
            Request::HostChanged(i, v) => {
                self.instance()?.host_changed(i as usize, v);
                Reply::Done
            }
            Request::PluginReported(i, v) => {
                self.instance()?.plugin_reported(i as usize, v);
                Reply::Done
            }
        })
    }

    /// Stops the audio thread (which drops the processor) and deactivates.
    fn deactivate(&mut self) {
        let Some(r) = self.running.take() else {
            return;
        };
        r.shm.set(Word::Quit, 1);
        r.shm.request(r.shm.get(Word::Request).wrapping_add(1));
        let _ = r.thread.join();
        if let Some(i) = self.instance.as_mut() {
            let until = Instant::now() + Duration::from_secs(2);
            while !i.try_deactivate() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    /// Plugin main-thread work, and what the plugin did that the app should hear about.
    fn tick(&mut self) {
        let Some(i) = self.instance.as_mut() else {
            return;
        };
        let now = Instant::now();
        i.main_thread_work(now);
        i.poll_editor();
        let open = i.editor_open();
        let edits = i.take_edits();
        let mut msgs: Vec<Message> = edits
            .into_iter()
            .map(|(k, v)| Message::Edit(k as u32, v))
            .collect();
        if let Some(r) = self.running.as_mut() {
            while let Ok(p) = r.params.pop() {
                msgs.push(Message::Param(p.index, p.value));
            }
        }
        let s = &self.signals;
        for (flag, kind) in [
            (&s.restart, SignalKind::Restart),
            (&s.latency, SignalKind::Latency),
            (&s.rescan_params, SignalKind::RescanParams),
            (&s.dirty, SignalKind::Dirty),
        ] {
            if Signals::take(flag) {
                msgs.push(Message::Signal(kind));
            }
        }
        if open != self.editor_open {
            self.editor_open = open;
            msgs.push(Message::EditorOpen(open));
        }
        for m in &msgs {
            self.send(m);
        }
    }

    fn next_wait(&self) -> Duration {
        self.instance
            .as_ref()
            .and_then(|i| i.next_frame(Instant::now()))
            .map_or(TICK, |w| w.min(TICK))
    }
}

/// Serves the app; `load` opens a plugin file (tests load the test plugin in process).
pub(crate) fn serve(load: &dyn Fn(&PluginInfo) -> Result<Entry, String>) -> i32 {
    let (tx, rx) = channel::<Input>();
    let wake: Sender<Input> = tx.clone();
    let waker: crate::Waker = Arc::new(move || {
        let _ = wake.send(Input::Wake);
    });
    let out = protocol_out();
    std::thread::Builder::new()
        .name("plugin-bridge-requests".to_owned())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if let Some((id, r)) = decode::<Request>(&line) {
                    if tx.send(Input::Request(id, r)).is_err() {
                        return;
                    }
                }
            }
            let _ = tx.send(Input::Closed);
        })
        .ok();
    let mut child = Child {
        out,
        signals: Signals::new(waker),
        instance: None,
        entry: None,
        running: None,
        editor_open: false,
    };
    loop {
        match rx.recv_timeout(child.next_wait()) {
            Ok(Input::Request(id, req)) => {
                let quit = matches!(req, Request::Quit);
                let reply = child.handle(req, load).unwrap_or_else(Reply::Failed);
                if let Some(id) = id {
                    child.send(&Message::Reply(id, reply));
                }
                if quit {
                    break;
                }
            }
            Ok(Input::Wake) | Err(RecvTimeoutError::Timeout) => {}
            Ok(Input::Closed) | Err(RecvTimeoutError::Disconnected) => break,
        }
        child.tick();
    }
    child.deactivate();
    if let Some(i) = child.instance.as_mut() {
        i.close_editor();
    }
    drop(child.instance.take());
    drop(child.entry.take());
    0
}

/// The child's audio thread: one block per request until told to quit.
fn audio_loop(shm: &Shm, mut p: Box<dyn PluginProcessor>) {
    let (mut l, mut r) = ([0f32; RENDER_QUANTUM], [0f32; RENDER_QUANTUM]);
    let mut seen = shm.get(Word::Request);
    let mut last = Instant::now();
    loop {
        shm.wait_request(seen, last.elapsed());
        let seq = shm.word(Word::Request as usize).load(Ordering::Acquire);
        if shm.get(Word::Quit) != 0 {
            break;
        }
        if seq == seen {
            continue;
        }
        seen = seq;
        last = Instant::now();
        if shm.get(Word::Reset) != 0 {
            p.reset();
        }
        for n in 0..(shm.get(Word::Events) as usize).min(super::shm::MAX_EVENTS) {
            let [kind, offset, a, b] = shm.event(n);
            let key = (a & 0x7F) as u8;
            let v = f32::from_bits(b);
            match kind {
                ev::NOTE_ON => p.note_on(offset, key, v),
                ev::NOTE_OFF => p.note_off(offset, key),
                ev::RELEASE_ALL => p.release_all(offset),
                ev::MIDI => p.midi(offset, [a as u8, (a >> 8) as u8, (a >> 16) as u8]),
                ev::KEY_PRESSURE => p.key_pressure(offset, key, v),
                ev::PARAM => p.set_param(a, v),
                ev::NOTE_EXPR => {
                    let kind = match a >> 8 {
                        0 => NoteExprKind::Tuning,
                        1 => NoteExprKind::Pressure,
                        _ => NoteExprKind::Timbre,
                    };
                    p.note_expression(offset, key, kind, v);
                }
                _ => {}
            }
        }
        let n = (shm.get(Word::Frames) as usize).min(RENDER_QUANTUM);
        let ctx = shm.context();
        shm.read_audio(&mut l[..n], &mut r[..n]);
        let ok = p.process(&mut l[..n], &mut r[..n], &ctx);
        shm.write_audio(&l[..n], &r[..n]);
        shm.set(Word::Ok, u32::from(ok));
        shm.finish(seq);
    }
    p.stop();
}
