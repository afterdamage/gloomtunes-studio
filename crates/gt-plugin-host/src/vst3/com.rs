//! The host's own VST3 objects: the host context (`IHostApplication`, which also makes
//! messages and attribute lists), the component handler (edits and restart requests from the
//! plugin), memory streams for state, and the editor frame with its run loop on Linux.
//!
//! Callbacks only record what happened ([`Signals`], the edit list, the run loop's
//! registrations); the plugin host acts on them in the UI frame, as with CLAP.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use vst3::ComRef;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComWrapper, Interface};

use super::guid_eq;
use crate::host::Signals;

/// The host context passed to `initialize`.
pub(crate) struct HostApp;

impl Class for HostApp {
    type Interfaces = (IHostApplication, IPlugInterfaceSupport);
}

impl IHostApplicationTrait for HostApp {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        match name.as_mut() {
            Some(n) => {
                super::copy_wstring("GloomTunes Studio", n);
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        let made = if guid_eq(&*cid, &IMessage::IID) {
            ComWrapper::new(Message::new()).to_com_ptr::<FUnknown>()
        } else if guid_eq(&*cid, &IAttributeList::IID) {
            ComWrapper::new(AttributeList::default()).to_com_ptr::<FUnknown>()
        } else {
            None
        };
        match made {
            Some(p) => {
                let raw = p.as_ptr();
                ((*(*raw).vtbl).queryInterface)(raw, iid, obj)
            }
            None => {
                *obj = std::ptr::null_mut();
                kNoInterface
            }
        }
    }
}

impl IPlugInterfaceSupportTrait for HostApp {
    unsafe fn isPlugInterfaceSupported(&self, iid: *const TUID) -> tresult {
        let Some(iid) = iid.as_ref() else {
            return kInvalidArgument;
        };
        let known = [
            IComponent::IID,
            IAudioProcessor::IID,
            IEditController::IID,
            IConnectionPoint::IID,
            IMidiMapping::IID,
            IUnitInfo::IID,
            IProgramListData::IID,
            IPlugView::IID,
        ];
        if known.iter().any(|k| guid_eq(iid, k)) {
            kResultTrue
        } else {
            kResultFalse
        }
    }
}

/// A message between a plugin's processor and controller.
pub(crate) struct Message {
    id: Mutex<Option<CString>>,
    attributes: ComWrapper<AttributeList>,
}

impl Message {
    fn new() -> Self {
        Self {
            id: Mutex::new(None),
            attributes: ComWrapper::new(AttributeList::default()),
        }
    }
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        self.id
            .lock()
            .map(|id| id.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()))
            .unwrap_or(std::ptr::null())
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        let id = (!id.is_null()).then(|| CStr::from_ptr(id).to_owned());
        if let Ok(mut m) = self.id.lock() {
            *m = id;
        }
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        // Not reference-counted for the caller (the SDK's convention).
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}

enum Attr {
    Int(i64),
    Float(f64),
    Str(Vec<u16>),
    Bin(Vec<u8>),
}

/// Named values carried by a message.
#[derive(Default)]
pub(crate) struct AttributeList {
    values: Mutex<HashMap<CString, Attr>>,
}

impl Class for AttributeList {
    type Interfaces = (IAttributeList,);
}

impl AttributeList {
    unsafe fn set(&self, id: *const c_char, v: Attr) -> tresult {
        if id.is_null() {
            return kInvalidArgument;
        }
        match self.values.lock() {
            Ok(mut m) => {
                m.insert(CStr::from_ptr(id).to_owned(), v);
                kResultOk
            }
            Err(_) => kInternalError,
        }
    }

    unsafe fn with<T>(&self, id: *const c_char, f: impl FnOnce(&Attr) -> Option<T>) -> Option<T> {
        if id.is_null() {
            return None;
        }
        let m = self.values.lock().ok()?;
        f(m.get(CStr::from_ptr(id))?)
    }
}

impl IAttributeListTrait for AttributeList {
    unsafe fn setInt(&self, id: *const c_char, value: i64) -> tresult {
        self.set(id, Attr::Int(value))
    }

    unsafe fn getInt(&self, id: *const c_char, value: *mut i64) -> tresult {
        match self.with(id, |a| match a {
            Attr::Int(v) => Some(*v),
            _ => None,
        }) {
            Some(v) if !value.is_null() => {
                *value = v;
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: *const c_char, value: f64) -> tresult {
        self.set(id, Attr::Float(value))
    }

    unsafe fn getFloat(&self, id: *const c_char, value: *mut f64) -> tresult {
        match self.with(id, |a| match a {
            Attr::Float(v) => Some(*v),
            _ => None,
        }) {
            Some(v) if !value.is_null() => {
                *value = v;
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: *const c_char, string: *const TChar) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut s = Vec::new();
        let mut p = string;
        while *p != 0 && s.len() < 1 << 20 {
            s.push(*p);
            p = p.add(1);
        }
        s.push(0);
        self.set(id, Attr::Str(s))
    }

    unsafe fn getString(&self, id: *const c_char, string: *mut TChar, size: u32) -> tresult {
        let chars = (size / 2) as usize;
        if string.is_null() || chars == 0 {
            return kInvalidArgument;
        }
        let ok = self.with(id, |a| match a {
            Attr::Str(s) => {
                // `s` ends with its NUL; a string too long is cut and terminated.
                let n = s.len().min(chars);
                std::ptr::copy_nonoverlapping(s.as_ptr(), string, n);
                if s.len() > chars {
                    *string.add(chars - 1) = 0;
                }
                Some(())
            }
            _ => None,
        });
        if ok.is_some() {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn setBinary(&self, id: *const c_char, data: *const c_void, size: u32) -> tresult {
        if data.is_null() && size > 0 {
            return kInvalidArgument;
        }
        let bytes = if size == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(data.cast::<u8>(), size as usize).to_vec()
        };
        self.set(id, Attr::Bin(bytes))
    }

    unsafe fn getBinary(
        &self,
        id: *const c_char,
        data: *mut *const c_void,
        size: *mut u32,
    ) -> tresult {
        if data.is_null() || size.is_null() {
            return kInvalidArgument;
        }
        // The pointer stays valid while the attribute is not replaced (the SDK's contract).
        match self.with(id, |a| match a {
            Attr::Bin(b) => Some((b.as_ptr().cast::<c_void>(), b.len() as u32)),
            _ => None,
        }) {
            Some((p, n)) => {
                *data = p;
                *size = n;
                kResultOk
            }
            None => kResultFalse,
        }
    }
}

/// Receives edits and requests from the plugin's controller.
pub(crate) struct Handler {
    pub(crate) signals: Arc<Signals>,
    /// Parameter edits from the editor (id, normalized), not yet sent to the processor.
    pub(crate) edits: Mutex<Vec<(u32, f64)>>,
}

impl Class for Handler {
    type Interfaces = (IComponentHandler, IComponentHandler2);
}

impl IComponentHandlerTrait for Handler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        if let Ok(mut e) = self.edits.lock() {
            if e.len() < 65_536 {
                e.push((id, value));
            }
        }
        self.signals.raise_flag(&self.signals.dirty);
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: i32) -> tresult {
        let flags = flags as u32;
        let has = |f: RestartFlags| flags & f as u32 != 0;
        if has(RestartFlags_::kReloadComponent)
            || has(RestartFlags_::kIoChanged)
            || has(RestartFlags_::kLatencyChanged)
        {
            // Buses and latency are read at activation: a restart picks them up.
            self.signals.raise_flag(&self.signals.restart);
        }
        if has(RestartFlags_::kParamValuesChanged) || has(RestartFlags_::kParamTitlesChanged) {
            self.signals.raise_flag(&self.signals.rescan_params);
        }
        kResultOk
    }
}

impl IComponentHandler2Trait for Handler {
    unsafe fn setDirty(&self, state: TBool) -> tresult {
        if state != 0 {
            self.signals.raise_flag(&self.signals.dirty);
        }
        kResultOk
    }

    unsafe fn requestOpenEditor(&self, _name: FIDString) -> tresult {
        kNotImplemented
    }

    unsafe fn startGroupEdit(&self) -> tresult {
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> tresult {
        kResultOk
    }
}

/// A stream over memory, for saving and restoring state.
#[derive(Default)]
pub(crate) struct MemStream {
    data: RefCell<Vec<u8>>,
    pos: Cell<usize>,
}

impl Class for MemStream {
    type Interfaces = (IBStream, ISizeableStream);
}

impl MemStream {
    pub(crate) fn new(data: Vec<u8>) -> ComWrapper<Self> {
        ComWrapper::new(Self {
            data: RefCell::new(data),
            pos: Cell::new(0),
        })
    }

    pub(crate) fn rewind(&self) {
        self.pos.set(0);
    }

    pub(crate) fn take(&self) -> Vec<u8> {
        std::mem::take(&mut self.data.borrow_mut())
    }
}

impl IBStreamTrait for MemStream {
    unsafe fn read(&self, buffer: *mut c_void, n: i32, read: *mut i32) -> tresult {
        let data = self.data.borrow();
        let pos = self.pos.get().min(data.len());
        let n = (n.max(0) as usize).min(data.len() - pos);
        if n > 0 {
            if buffer.is_null() {
                return kInvalidArgument;
            }
            std::ptr::copy_nonoverlapping(data.as_ptr().add(pos), buffer.cast::<u8>(), n);
        }
        self.pos.set(pos + n);
        if let Some(r) = read.as_mut() {
            *r = n as i32;
        }
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, n: i32, written: *mut i32) -> tresult {
        let n = n.max(0) as usize;
        if n > 0 && buffer.is_null() {
            return kInvalidArgument;
        }
        let mut data = self.data.borrow_mut();
        let pos = self.pos.get();
        if data.len() < pos + n {
            data.resize(pos + n, 0);
        }
        if n > 0 {
            std::ptr::copy_nonoverlapping(buffer.cast::<u8>(), data.as_mut_ptr().add(pos), n);
        }
        self.pos.set(pos + n);
        if let Some(w) = written.as_mut() {
            *w = n as i32;
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: i64, mode: i32, result: *mut i64) -> tresult {
        let len = self.data.borrow().len() as i64;
        let base = match mode as u32 {
            m if m == IBStream_::IStreamSeekMode_::kIBSeekSet as u32 => 0,
            m if m == IBStream_::IStreamSeekMode_::kIBSeekCur as u32 => self.pos.get() as i64,
            m if m == IBStream_::IStreamSeekMode_::kIBSeekEnd as u32 => len,
            _ => return kInvalidArgument,
        };
        let to = (base + pos).max(0);
        self.pos.set(to as usize);
        if let Some(r) = result.as_mut() {
            *r = to;
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut i64) -> tresult {
        match pos.as_mut() {
            Some(p) => {
                *p = self.pos.get() as i64;
                kResultOk
            }
            None => kInvalidArgument,
        }
    }
}

impl ISizeableStreamTrait for MemStream {
    unsafe fn getStreamSize(&self, size: *mut i64) -> tresult {
        match size.as_mut() {
            Some(s) => {
                *s = self.data.borrow().len() as i64;
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn setStreamSize(&self, size: i64) -> tresult {
        self.data.borrow_mut().resize(size.max(0) as usize, 0);
        kResultOk
    }
}

/// Timers and file descriptors the plugin registered with the run loop (Linux).
#[derive(Default)]
pub(crate) struct RunLoop {
    #[cfg(target_os = "linux")]
    pub(crate) fds: Vec<(vst3::ComPtr<Linux::IEventHandler>, i32)>,
    #[cfg(target_os = "linux")]
    pub(crate) timers: Vec<(vst3::ComPtr<Linux::ITimerHandler>, Duration, Instant)>,
}

impl RunLoop {
    /// Something is registered (the UI should keep polling).
    pub(crate) fn busy(&self) -> bool {
        #[cfg(target_os = "linux")]
        return !self.fds.is_empty() || !self.timers.is_empty();
        #[allow(unreachable_code)]
        false
    }
}

/// The editor's frame: resize requests and, on Linux, the run loop.
pub(crate) struct Frame {
    pub(crate) signals: Arc<Signals>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) run_loop: Rc<RefCell<RunLoop>>,
}

#[cfg(target_os = "linux")]
impl Class for Frame {
    type Interfaces = (IPlugFrame, Linux::IRunLoop);
}

#[cfg(not(target_os = "linux"))]
impl Class for Frame {
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for Frame {
    unsafe fn resizeView(&self, _view: *mut IPlugView, size: *mut ViewRect) -> tresult {
        let Some(r) = size.as_ref() else {
            return kInvalidArgument;
        };
        let (w, h) = ((r.right - r.left).max(1), (r.bottom - r.top).max(1));
        self.signals.request_resize(w as u32, h as u32);
        kResultTrue
    }
}

#[cfg(target_os = "linux")]
impl Linux::IRunLoopTrait for Frame {
    unsafe fn registerEventHandler(&self, handler: *mut Linux::IEventHandler, fd: i32) -> tresult {
        let Some(h) = ComRef::from_raw(handler) else {
            return kInvalidArgument;
        };
        self.run_loop.borrow_mut().fds.push((h.to_com_ptr(), fd));
        kResultOk
    }

    unsafe fn unregisterEventHandler(&self, handler: *mut Linux::IEventHandler) -> tresult {
        self.run_loop
            .borrow_mut()
            .fds
            .retain(|(h, _)| h.as_ptr() != handler);
        kResultOk
    }

    unsafe fn registerTimer(&self, handler: *mut Linux::ITimerHandler, ms: u64) -> tresult {
        let Some(h) = ComRef::from_raw(handler) else {
            return kInvalidArgument;
        };
        // Between 10 ms and 1 s: the UI frame loop runs them.
        let every = Duration::from_millis(ms.clamp(10, 1000));
        self.run_loop
            .borrow_mut()
            .timers
            .push((h.to_com_ptr(), every, Instant::now() + every));
        self.signals.wake();
        kResultOk
    }

    unsafe fn unregisterTimer(&self, handler: *mut Linux::ITimerHandler) -> tresult {
        self.run_loop
            .borrow_mut()
            .timers
            .retain(|(h, _, _)| h.as_ptr() != handler);
        kResultOk
    }
}
