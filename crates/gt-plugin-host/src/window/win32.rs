//! Editor windows on Windows, with Win32.
//!
//! The window is created on the UI thread, whose message loop (run by winit for egui) also
//! dispatches this window's messages to [`wnd_proc`]. Closing and resizing are recorded in
//! the window's state and picked up by [`Native::poll`].

#![allow(unsafe_code)]

use std::cell::Cell;
use std::sync::OnceLock;

use clack_extensions::gui::Window;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW,
    LoadCursorW, RegisterClassExW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, CW_USEDEFAULT, GWLP_USERDATA, IDC_ARROW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
    SW_SHOWNORMAL, WINDOW_STYLE, WM_CLOSE, WM_NCDESTROY, WM_SIZE, WNDCLASSEXW, WS_CAPTION,
    WS_CLIPCHILDREN, WS_MINIMIZEBOX, WS_OVERLAPPEDWINDOW, WS_SYSMENU,
};

use super::WindowEvents;

/// Per-window state, owned by the window (freed on `WM_NCDESTROY`).
#[derive(Default)]
struct State {
    close: Cell<bool>,
    size: Cell<Option<(u32, u32)>>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

const CLASS: &str = "GloomTunesPluginWindow";

fn register_class() -> bool {
    static ATOM: OnceLock<u16> = OnceLock::new();
    *ATOM.get_or_init(|| {
        let name = wide(CLASS);
        // SAFETY: plain Win32 calls with valid, NUL-terminated strings; the class name is
        // copied by Windows.
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: GetModuleHandleW(std::ptr::null()),
                hIcon: std::ptr::null_mut(),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: name.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            RegisterClassExW(&class)
        }
    }) != 0
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: GWLP_USERDATA holds a `Box<State>` pointer set right after creation, or null.
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const State;
    match msg {
        WM_CLOSE if !state.is_null() => {
            // Closing is the host's decision: the plugin's editor must be destroyed first.
            unsafe { (*state).close.set(true) };
            return 0;
        }
        WM_SIZE if !state.is_null() => {
            let w = (lparam as u32) & 0xFFFF;
            let h = ((lparam as u32) >> 16) & 0xFFFF;
            if w > 0 && h > 0 {
                unsafe { (*state).size.set(Some((w, h))) };
            }
        }
        WM_NCDESTROY if !state.is_null() => unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(state as *mut State));
        },
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn style(resizable: bool) -> WINDOW_STYLE {
    if resizable {
        WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN
    } else {
        WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN
    }
}

/// Outer size for an inner (client) size.
fn outer(width: u32, height: u32, resizable: bool) -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    };
    // SAFETY: `r` is a valid RECT.
    unsafe { AdjustWindowRectEx(&mut r, style(resizable), 0, 0) };
    (r.right - r.left, r.bottom - r.top)
}

pub(super) struct Native {
    hwnd: HWND,
    state: *const State,
    resizable: bool,
}

impl Native {
    pub(super) fn open(
        title: &str,
        width: u32,
        height: u32,
        resizable: bool,
    ) -> Result<Self, String> {
        if !register_class() {
            return Err("cannot register the plugin window class".to_owned());
        }
        let class = wide(CLASS);
        let title = wide(title);
        let (w, h) = outer(width, height, resizable);
        // SAFETY: the class is registered; strings are NUL-terminated and outlive the call.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                style(resizable),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                w,
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err("cannot create the plugin window".to_owned());
        }
        let state = Box::into_raw(Box::<State>::default());
        // SAFETY: `hwnd` is our live window; the state is freed by `wnd_proc` on WM_NCDESTROY.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
            ShowWindow(hwnd, SW_SHOWNORMAL);
        }
        Ok(Self {
            hwnd,
            state,
            resizable,
        })
    }

    pub(super) fn clap_window(&self) -> Window<'static, 'static> {
        // SAFETY: the handle is a live window for as long as this `Native` exists, and the
        // host destroys the plugin's editor before dropping it.
        unsafe { Window::from_win32_hwnd(self.hwnd) }
    }

    pub(super) fn poll(&mut self) -> WindowEvents {
        // SAFETY: the state lives until the window is destroyed, which only `drop` does.
        let state = unsafe { &*self.state };
        WindowEvents {
            close: state.close.replace(false),
            resized: state.size.take(),
        }
    }

    pub(super) fn resize(&mut self, width: u32, height: u32, resizable: bool) {
        self.resizable = resizable;
        let (w, h) = outer(width, height, resizable);
        // SAFETY: `hwnd` is our live window.
        unsafe {
            SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                0,
                0,
                w,
                h,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        // Our own resize is not a user resize.
        // SAFETY: as in `poll`.
        unsafe { (*self.state).size.set(None) };
    }

    pub(super) fn raise(&mut self) {
        // SAFETY: `hwnd` is our live window.
        unsafe { SetForegroundWindow(self.hwnd) };
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        // SAFETY: `hwnd` is our live window; destroying it frees the state.
        unsafe { DestroyWindow(self.hwnd) };
    }
}
