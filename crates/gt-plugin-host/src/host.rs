//! The host side of one plugin instance: the callbacks a CLAP plugin can make into us.
//!
//! Callbacks from any thread only set flags in [`Signals`] and wake the UI; the plugin host
//! acts on them on the main thread at the next frame. Main-thread callbacks (timers, file
//! descriptors) keep their registrations here for the host to run.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clack_extensions::audio_ports::{AudioPortRescanFlags, HostAudioPorts, HostAudioPortsImpl};
use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl};
use clack_extensions::latency::{HostLatency, HostLatencyImpl};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::note_ports::{
    HostNotePorts, HostNotePortsImpl, NoteDialects, NotePortRescanFlags,
};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
};
use clack_extensions::state::{HostState, HostStateImpl};
use clack_extensions::timer::{HostTimer, HostTimerImpl, PluginTimer, TimerId};
use clack_host::prelude::*;

/// Wakes the UI so it handles plugin requests soon (egui's `request_repaint`).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Requests a plugin made, picked up on the main thread.
pub(crate) struct Signals {
    pub(crate) callback: AtomicBool,
    pub(crate) restart: AtomicBool,
    pub(crate) rescan_params: AtomicBool,
    pub(crate) latency: AtomicBool,
    pub(crate) dirty: AtomicBool,
    pub(crate) gui_closed: AtomicBool,
    /// Requested editor size, packed with [`GuiSize::pack_to_u64`]; `u64::MAX` for none.
    pub(crate) gui_resize: AtomicU64,
    waker: Waker,
}

impl Signals {
    pub(crate) fn new(waker: Waker) -> Arc<Self> {
        Arc::new(Self {
            callback: AtomicBool::new(false),
            restart: AtomicBool::new(false),
            rescan_params: AtomicBool::new(false),
            latency: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
            gui_closed: AtomicBool::new(false),
            gui_resize: AtomicU64::new(u64::MAX),
            waker,
        })
    }

    fn raise(&self, flag: &AtomicBool) {
        flag.store(true, Ordering::Release);
        (self.waker)();
    }

    /// Takes a flag (true if it was set).
    pub(crate) fn take(flag: &AtomicBool) -> bool {
        flag.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn take_resize(&self) -> Option<GuiSize> {
        match self.gui_resize.swap(u64::MAX, Ordering::AcqRel) {
            u64::MAX => None,
            v => Some(GuiSize::unpack_from_u64(v)),
        }
    }
}

std::thread_local! {
    /// Set while this thread is inside a plugin's `process`, so plugin log messages from the
    /// audio thread are dropped instead of formatted and written (no I/O there).
    pub(crate) static IN_PROCESS: Cell<bool> = const { Cell::new(false) };
}

/// The handler types.
pub(crate) struct GtHost;

impl HostHandlers for GtHost {
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder
            .register::<HostLog>()
            .register::<HostParams>()
            .register::<HostState>()
            .register::<HostLatency>()
            .register::<HostGui>()
            .register::<HostTimer>()
            .register::<HostAudioPorts>()
            .register::<HostNotePorts>();
        #[cfg(unix)]
        builder.register::<clack_extensions::posix_fd::HostPosixFd>();
    }
}

/// Shared by every thread of one instance.
pub(crate) struct Shared {
    pub(crate) signals: Arc<Signals>,
    name: String,
}

impl Shared {
    pub(crate) fn new(signals: Arc<Signals>, name: &str) -> Self {
        Self {
            signals,
            name: name.to_owned(),
        }
    }
}

impl SharedHandler<'_> for Shared {
    fn request_restart(&self) {
        self.signals.raise(&self.signals.restart);
    }

    fn request_process(&self) {
        // The engine processes every plugin all the time.
    }

    fn request_callback(&self) {
        self.signals.raise(&self.signals.callback);
    }
}

impl HostLogImpl for Shared {
    fn log(&self, severity: LogSeverity, message: &str) {
        if IN_PROCESS.with(Cell::get) {
            return;
        }
        let level = match severity {
            LogSeverity::Debug => log::Level::Debug,
            LogSeverity::Info => log::Level::Info,
            LogSeverity::Warning => log::Level::Warn,
            _ => log::Level::Error,
        };
        log::log!(level, "plugin {}: {message}", self.name);
    }
}

impl HostParamsImplShared for Shared {
    fn request_flush(&self) {
        // Parameter events are delivered with the next block, which always comes.
    }
}

impl HostGuiImpl for Shared {
    fn resize_hints_changed(&self) {}

    fn request_resize(&self, new_size: GuiSize) -> Result<(), HostError> {
        self.signals
            .gui_resize
            .store(new_size.pack_to_u64(), Ordering::Release);
        (self.signals.waker)();
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn request_hide(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn closed(&self, _was_destroyed: bool) {
        self.signals.raise(&self.signals.gui_closed);
    }
}

/// A timer a plugin registered.
struct Timer {
    id: TimerId,
    every: Duration,
    next: Instant,
}

/// Main-thread state of one instance.
pub(crate) struct MainThread<'a> {
    shared: &'a Shared,
    timer_ext: Cell<Option<PluginTimer>>,
    timers: RefCell<Vec<Timer>>,
    next_timer: Cell<u32>,
    #[cfg(unix)]
    pub(crate) fd_ext: Cell<Option<clack_extensions::posix_fd::PluginPosixFd>>,
    #[cfg(unix)]
    pub(crate) fds: RefCell<Vec<(std::os::fd::RawFd, clack_extensions::posix_fd::FdFlags)>>,
}

impl<'a> MainThread<'a> {
    pub(crate) fn new(shared: &'a Shared) -> Self {
        Self {
            shared,
            timer_ext: Cell::new(None),
            timers: RefCell::new(Vec::new()),
            next_timer: Cell::new(1),
            #[cfg(unix)]
            fd_ext: Cell::new(None),
            #[cfg(unix)]
            fds: RefCell::new(Vec::new()),
        }
    }

    /// Timers that are due now (their next time is moved on), with the plugin's timer
    /// extension to call them with.
    pub(crate) fn due_timers(&self, now: Instant) -> (Option<PluginTimer>, Vec<TimerId>) {
        let mut due = Vec::new();
        for t in self.timers.borrow_mut().iter_mut() {
            if now >= t.next {
                due.push(t.id);
                t.next = now + t.every;
            }
        }
        (self.timer_ext.get(), due)
    }

    /// Time until the next timer is due, if any are registered.
    pub(crate) fn next_timer(&self, now: Instant) -> Option<Duration> {
        self.timers
            .borrow()
            .iter()
            .map(|t| t.next.saturating_duration_since(now))
            .min()
    }
}

impl<'a> MainThreadHandler<'a> for MainThread<'a> {
    fn initialized(&self, instance: InitializedPluginHandle<'a>) {
        self.timer_ext.set(instance.get_extension());
        #[cfg(unix)]
        self.fd_ext.set(instance.get_extension());
    }
}

impl HostParamsImplMainThread for MainThread<'_> {
    fn rescan(&self, _flags: ParamRescanFlags) {
        self.shared
            .signals
            .raise(&self.shared.signals.rescan_params);
    }

    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

impl HostStateImpl for MainThread<'_> {
    fn mark_dirty(&self) {
        self.shared.signals.raise(&self.shared.signals.dirty);
    }
}

impl HostLatencyImpl for MainThread<'_> {
    fn changed(&self) {
        // Latency is read at activation; a change takes effect with the next restart.
        self.shared.signals.raise(&self.shared.signals.latency);
    }
}

impl HostTimerImpl for MainThread<'_> {
    fn register_timer(&self, period_ms: u32) -> Result<TimerId, HostError> {
        let id = TimerId(self.next_timer.get());
        self.next_timer.set(id.0.wrapping_add(1));
        // Between 10 ms and 1 s: the UI frame loop runs them.
        let every = Duration::from_millis(u64::from(period_ms.clamp(10, 1000)));
        self.timers.borrow_mut().push(Timer {
            id,
            every,
            next: Instant::now() + every,
        });
        (self.shared.signals.waker)();
        Ok(id)
    }

    fn unregister_timer(&self, timer_id: TimerId) -> Result<(), HostError> {
        let mut timers = self.timers.borrow_mut();
        let before = timers.len();
        timers.retain(|t| t.id != timer_id);
        if timers.len() < before {
            Ok(())
        } else {
            Err(HostError::Message("unknown timer"))
        }
    }
}

impl HostAudioPortsImpl for MainThread<'_> {
    fn is_rescan_flag_supported(&self, _flag: AudioPortRescanFlags) -> bool {
        false
    }

    fn rescan(&self, _flags: AudioPortRescanFlags) {
        // Ports are read at activation; a restart picks up the new ones.
        self.shared.signals.raise(&self.shared.signals.restart);
    }
}

impl HostNotePortsImpl for MainThread<'_> {
    fn supported_dialects(&self) -> NoteDialects {
        NoteDialects::CLAP | NoteDialects::MIDI
    }

    fn rescan(&self, _flags: NotePortRescanFlags) {
        self.shared.signals.raise(&self.shared.signals.restart);
    }
}

#[cfg(unix)]
mod fd {
    use super::MainThread;
    use clack_extensions::posix_fd::{FdFlags, HostPosixFdImpl};
    use clack_host::prelude::HostError;
    use std::os::fd::RawFd;

    impl HostPosixFdImpl for MainThread<'_> {
        fn register_fd(&self, fd: RawFd, flags: FdFlags) -> Result<(), HostError> {
            let mut fds = self.fds.borrow_mut();
            if fds.iter().any(|(f, _)| *f == fd) {
                return Err(HostError::Message("file descriptor already registered"));
            }
            fds.push((fd, flags));
            Ok(())
        }

        fn modify_fd(&self, fd: RawFd, flags: FdFlags) -> Result<(), HostError> {
            match self.fds.borrow_mut().iter_mut().find(|(f, _)| *f == fd) {
                Some(entry) => {
                    entry.1 = flags;
                    Ok(())
                }
                None => Err(HostError::Message("unknown file descriptor")),
            }
        }

        fn unregister_fd(&self, fd: RawFd) -> Result<(), HostError> {
            let mut fds = self.fds.borrow_mut();
            let before = fds.len();
            fds.retain(|(f, _)| *f != fd);
            if fds.len() < before {
                Ok(())
            } else {
                Err(HostError::Message("unknown file descriptor"))
            }
        }
    }
}
