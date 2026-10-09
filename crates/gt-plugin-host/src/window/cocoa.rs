//! Editor windows on macOS, with AppKit.
//!
//! The window is created on the main thread, whose event loop (run by winit for egui) also
//! handles this window's events. Instead of a delegate, [`Native::poll`] compares the window's
//! state with the last poll: a window the user closed is no longer visible (it is kept alive,
//! not released, so the plugin's view can be removed first), and a changed content size is
//! a resize.

#![allow(unsafe_code)]

use clack_extensions::gui::Window;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSView, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::WindowEvents;

pub(super) struct Native {
    window: Retained<NSWindow>,
    view: Retained<NSView>,
    size: (u32, u32),
    closed: bool,
}

fn style(resizable: bool) -> NSWindowStyleMask {
    let mut s =
        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
    if resizable {
        s |= NSWindowStyleMask::Resizable;
    }
    s
}

fn content_size(view: &NSView) -> (u32, u32) {
    let f = view.frame();
    (
        f.size.width.round().max(1.0) as u32,
        f.size.height.round().max(1.0) as u32,
    )
}

impl Native {
    pub(super) fn open(
        title: &str,
        width: u32,
        height: u32,
        resizable: bool,
    ) -> Result<Self, String> {
        let mtm = MainThreadMarker::new()
            .ok_or("plugin windows can only be opened on the main thread")?;
        let rect = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(f64::from(width), f64::from(height)),
        );
        // SAFETY: a plain NSWindow init on the main thread (checked above) with a valid rect
        // and style. Buffered backing is the only kind modern macOS supports.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect,
                style(resizable),
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: we own the window through `Retained` and release it ourselves; closing it
        // must not free it while the plugin's view may still be inside.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(title));
        let view = window
            .contentView()
            .ok_or("the window has no content view")?;
        window.center();
        window.makeKeyAndOrderFront(None);
        let size = content_size(&view);
        Ok(Self {
            window,
            view,
            size,
            closed: false,
        })
    }

    pub(super) fn clap_window(&self) -> Window<'static, 'static> {
        // SAFETY: the view belongs to our window, which outlives the plugin's editor (it is
        // dropped only after `gui.destroy`).
        unsafe { Window::from_cocoa_nsview(Retained::as_ptr(&self.view) as *mut _) }
    }

    pub(super) fn poll(&mut self) -> WindowEvents {
        let mut ev = WindowEvents::default();
        if !self.closed && !self.window.isVisible() && !self.window.isMiniaturized() {
            self.closed = true;
            ev.close = true;
        }
        let size = content_size(&self.view);
        if size != self.size {
            self.size = size;
            ev.resized = Some(size);
        }
        ev
    }

    pub(super) fn resize(&mut self, width: u32, height: u32, resizable: bool) {
        self.window.setStyleMask(style(resizable));
        self.window
            .setContentSize(NSSize::new(f64::from(width), f64::from(height)));
        // The plugin asked for this size, so it is not reported back as a user resize.
        self.size = content_size(&self.view);
    }

    pub(super) fn raise(&mut self) {
        if self.window.isMiniaturized() {
            self.window.deminiaturize(None);
        }
        self.window.makeKeyAndOrderFront(None);
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        if !self.closed {
            self.window.close();
        }
    }
}
