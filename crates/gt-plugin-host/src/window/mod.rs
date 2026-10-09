//! Native windows that hold plugin editors.
//!
//! The app's own window belongs to egui, so each plugin editor gets a separate top-level
//! window of ours, made with the platform's own API (X11 on Linux, Win32 on Windows), and the
//! plugin embeds its editor in it (`gui.set_parent`). Its events are polled once per UI frame
//! on the main thread, as CLAP requires.

#[cfg(target_os = "linux")]
mod x11;
#[cfg(target_os = "linux")]
use x11 as imp;

#[cfg(windows)]
mod win32;
#[cfg(windows)]
use win32 as imp;

use clack_extensions::gui::{GuiApiType, Window};

/// What happened to a window since the last poll.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WindowEvents {
    /// The user asked to close it.
    pub(crate) close: bool,
    /// Its inner size changed to this.
    pub(crate) resized: Option<(u32, u32)>,
}

/// A window holding one plugin editor.
pub(crate) struct HostWindow {
    #[cfg(any(target_os = "linux", windows))]
    inner: imp::Native,
}

impl HostWindow {
    /// The window API plugins embed into on this platform.
    pub(crate) fn api() -> Option<GuiApiType<'static>> {
        if cfg!(target_os = "linux") {
            Some(GuiApiType::X11)
        } else if cfg!(windows) {
            Some(GuiApiType::WIN32)
        } else {
            None
        }
    }

    /// Opens a window with an inner size of `width` × `height` pixels.
    #[allow(unused_variables)]
    pub(crate) fn open(
        title: &str,
        width: u32,
        height: u32,
        resizable: bool,
    ) -> Result<Self, String> {
        #[cfg(any(target_os = "linux", windows))]
        return Ok(Self {
            inner: imp::Native::open(title, width.max(1), height.max(1), resizable)?,
        });
        #[allow(unreachable_code)]
        Err("plugin windows are not supported on this system".to_owned())
    }

    /// The window as CLAP describes it, to pass to the plugin.
    pub(crate) fn clap_window(&self) -> Window<'static, 'static> {
        #[cfg(any(target_os = "linux", windows))]
        return self.inner.clap_window();
        #[allow(unreachable_code)]
        {
            unreachable!("no window can be opened on this system")
        }
    }

    /// Handles pending events.
    pub(crate) fn poll(&mut self) -> WindowEvents {
        #[cfg(any(target_os = "linux", windows))]
        return self.inner.poll();
        #[allow(unreachable_code)]
        WindowEvents::default()
    }

    /// Sets the inner size.
    #[allow(unused_variables)]
    pub(crate) fn resize(&mut self, width: u32, height: u32, resizable: bool) {
        #[cfg(any(target_os = "linux", windows))]
        self.inner.resize(width.max(1), height.max(1), resizable);
    }

    /// Brings the window to the front.
    pub(crate) fn raise(&mut self) {
        #[cfg(any(target_os = "linux", windows))]
        self.inner.raise();
    }
}
