//! Editor windows on Linux, with X11 (through XWayland on a Wayland desktop).

use clack_extensions::gui::Window;
use x11rb::connection::Connection;
use x11rb::properties::WmSizeHints;
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    StackMode, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::WindowEvents;

pub(super) struct Native {
    conn: RustConnection,
    window: u32,
    delete: u32,
    size: (u32, u32),
}

fn err(e: impl std::fmt::Display) -> String {
    format!("X11: {e}")
}

impl Native {
    pub(super) fn open(
        title: &str,
        width: u32,
        height: u32,
        resizable: bool,
    ) -> Result<Self, String> {
        let (conn, screen) = x11rb::connect(None).map_err(err)?;
        let root = conn.setup().roots[screen].root;
        let black = conn.setup().roots[screen].black_pixel;
        let window = conn.generate_id().map_err(err)?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            width.min(u32::from(u16::MAX)) as u16,
            height.min(u32::from(u16::MAX)) as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .background_pixel(black)
                .event_mask(EventMask::STRUCTURE_NOTIFY),
        )
        .map_err(err)?;
        let atom = |name: &str| -> Result<u32, String> {
            Ok(conn
                .intern_atom(false, name.as_bytes())
                .map_err(err)?
                .reply()
                .map_err(err)?
                .atom)
        };
        let protocols = atom("WM_PROTOCOLS")?;
        let delete = atom("WM_DELETE_WINDOW")?;
        let net_name = atom("_NET_WM_NAME")?;
        let utf8 = atom("UTF8_STRING")?;
        conn.change_property32(
            PropMode::REPLACE,
            window,
            protocols,
            AtomEnum::ATOM,
            &[delete],
        )
        .map_err(err)?;
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title.as_bytes(),
        )
        .map_err(err)?;
        conn.change_property8(PropMode::REPLACE, window, net_name, utf8, title.as_bytes())
            .map_err(err)?;
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"gloomtunes-plugin\0GloomTunes Studio\0",
        )
        .map_err(err)?;
        let me = Self {
            conn,
            window,
            delete,
            size: (width, height),
        };
        me.set_hints(width, height, resizable)?;
        me.conn.map_window(window).map_err(err)?;
        me.conn.flush().map_err(err)?;
        Ok(me)
    }

    fn set_hints(&self, width: u32, height: u32, resizable: bool) -> Result<(), String> {
        let mut hints = WmSizeHints::new();
        if !resizable {
            let s = (width as i32, height as i32);
            hints.min_size = Some(s);
            hints.max_size = Some(s);
        }
        hints
            .set_normal_hints(&self.conn, self.window)
            .map_err(err)?;
        Ok(())
    }

    pub(super) fn clap_window(&self) -> Window<'static, 'static> {
        Window::from_x11_handle(std::os::raw::c_ulong::from(self.window))
    }

    pub(super) fn poll(&mut self) -> WindowEvents {
        let mut out = WindowEvents::default();
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            match event {
                Event::ClientMessage(m) if m.data.as_data32()[0] == self.delete => {
                    out.close = true;
                }
                Event::ConfigureNotify(c) if c.window == self.window => {
                    let size = (u32::from(c.width), u32::from(c.height));
                    if size != self.size {
                        self.size = size;
                        out.resized = Some(size);
                    }
                }
                _ => {}
            }
        }
        out
    }

    pub(super) fn resize(&mut self, width: u32, height: u32, resizable: bool) {
        self.size = (width, height);
        let _ = self.set_hints(width, height, resizable);
        let _ = self.conn.configure_window(
            self.window,
            &ConfigureWindowAux::new().width(width).height(height),
        );
        let _ = self.conn.flush();
    }

    pub(super) fn raise(&mut self) {
        let _ = self.conn.configure_window(
            self.window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        );
        let _ = self.conn.flush();
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}
