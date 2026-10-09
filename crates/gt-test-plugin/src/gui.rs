//! GT Test Gain's editor on Linux: an X11 window embedded in the host's, showing the gain as
//! a bar. Clicking or dragging sets the gain. A host timer polls the window's events, the way
//! CLAP asks plugins to run their GUI on the host's main thread.

use std::sync::atomic::Ordering;

use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, PluginGuiImpl, Window};
use clack_extensions::timer::{HostTimer, PluginTimerImpl, TimerId};
use clack_plugin::prelude::*;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, Rectangle, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use crate::gain::GainMainThread;

const WIDTH: u16 = 320;
const HEIGHT: u16 = 80;
const BACKGROUND: u32 = 0x0016_1619;
const ACCENT: u32 = 0x009b_5de5;

/// An open editor.
pub(crate) struct Editor {
    conn: RustConnection,
    window: u32,
    gc: u32,
    timer: Option<(HostTimer, TimerId)>,
    drawn: f32,
}

fn x11_err<E>(_: E) -> PluginError {
    PluginError::Message("X11 error")
}

impl Editor {
    fn open(parent: u32) -> Result<Self, PluginError> {
        let (conn, _) = x11rb::connect(None).map_err(x11_err)?;
        let window = conn.generate_id().map_err(x11_err)?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            parent,
            0,
            0,
            WIDTH,
            HEIGHT,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .background_pixel(BACKGROUND)
                .event_mask(
                    EventMask::EXPOSURE | EventMask::BUTTON_PRESS | EventMask::BUTTON1_MOTION,
                ),
        )
        .map_err(x11_err)?;
        let gc = conn.generate_id().map_err(x11_err)?;
        conn.create_gc(gc, window, &CreateGCAux::new().foreground(ACCENT))
            .map_err(x11_err)?;
        conn.map_window(window).map_err(x11_err)?;
        conn.flush().map_err(x11_err)?;
        Ok(Self {
            conn,
            window,
            gc,
            timer: None,
            drawn: -1.0,
        })
    }

    fn draw(&mut self, gain: f32) {
        let w = (f32::from(WIDTH) * gain / 2.0)
            .round()
            .clamp(0.0, f32::from(WIDTH)) as u16;
        let _ = self.conn.clear_area(false, self.window, 0, 0, 0, 0);
        let _ = self.conn.poly_fill_rectangle(
            self.window,
            self.gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: w,
                height: HEIGHT,
            }],
        );
        let _ = self.conn.flush();
        self.drawn = gain;
    }

    /// Handles pending window events; returns a gain the user set, if any.
    fn poll(&mut self) -> Option<f32> {
        let mut set = None;
        let mut expose = false;
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            match event {
                Event::Expose(_) => expose = true,
                Event::ButtonPress(e) => set = Some(e.event_x),
                Event::MotionNotify(e) => set = Some(e.event_x),
                _ => {}
            }
        }
        if expose {
            self.drawn = -1.0;
        }
        set.map(|x| (f32::from(x) / f32::from(WIDTH) * 2.0).clamp(0.0, 2.0))
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}

impl PluginGuiImpl for GainMainThread<'_> {
    fn is_api_supported(&self, config: GuiConfiguration) -> bool {
        config.api_type == GuiApiType::X11 && !config.is_floating
    }

    fn get_preferred_api(&self) -> Option<GuiConfiguration<'_>> {
        Some(GuiConfiguration {
            api_type: GuiApiType::X11,
            is_floating: false,
        })
    }

    fn create(&self, config: GuiConfiguration) -> Result<(), PluginError> {
        if self.is_api_supported(config) {
            Ok(())
        } else {
            Err(PluginError::Message("unsupported window API"))
        }
    }

    fn destroy(&self) {
        if let Some(editor) = self.editor.borrow_mut().take() {
            if let Some((ext, id)) = editor.timer {
                let _ = ext.unregister_timer(&self.host, id);
            }
        }
    }

    fn set_scale(&self, _scale: f64) -> Result<(), PluginError> {
        Ok(())
    }

    fn get_size(&self) -> Option<GuiSize> {
        Some(GuiSize {
            width: u32::from(WIDTH),
            height: u32::from(HEIGHT),
        })
    }

    fn set_size(&self, _size: GuiSize) -> Result<(), PluginError> {
        Ok(())
    }

    fn set_parent(&self, window: Window) -> Result<(), PluginError> {
        let parent = window
            .as_x11_handle()
            .ok_or(PluginError::Message("not an X11 window"))?;
        let mut editor = Editor::open(parent as u32)?;
        let ext: Option<HostTimer> = self.host.get_extension();
        editor.timer = ext.and_then(|ext| Some((ext, ext.register_timer(&self.host, 30).ok()?)));
        *self.editor.borrow_mut() = Some(editor);
        Ok(())
    }

    fn set_transient(&self, _window: Window) -> Result<(), PluginError> {
        Err(PluginError::Message("floating windows are not supported"))
    }

    fn show(&self) -> Result<(), PluginError> {
        Ok(())
    }

    fn hide(&self) -> Result<(), PluginError> {
        Ok(())
    }
}

impl PluginTimerImpl for GainMainThread<'_> {
    fn on_timer(&self, _timer_id: TimerId) {
        let mut editor = self.editor.borrow_mut();
        let Some(editor) = editor.as_mut() else {
            return;
        };
        if let Some(gain) = editor.poll() {
            self.shared.gain.set(gain);
            self.shared.edited.store(true, Ordering::Relaxed);
        }
        let gain = self.shared.gain.get();
        if gain != editor.drawn {
            editor.draw(gain);
        }
    }
}
