//! VST3 hosting (Prompt 18, D113), through the `vst3` crate's bindings (MIT or Apache-2.0,
//! generated from the MIT-licensed VST 3 SDK headers; no SDK needed to build).
//!
//! * [`Module`]: a `.vst3` bundle (or single file) loaded, with its entry calls and factory;
//!   listing its classes is how VST3 plugins are scanned (in the same child process as CLAP).
//! * [`Vst3Plugin`]: one instance on the main thread: the component (`IComponent` and
//!   `IAudioProcessor`) and its edit controller (`IEditController`, separate or the same
//!   object, connected through `IConnectionPoint`), state, parameters, activation, the run
//!   loop and the editor (`IPlugView` in one of our windows).
//! * `process::Vst3Processor`: the engine side.
//!
//! Plugin ids are the processor's class id in hex. VST3 parameters are normalized already, so
//! the document's mirror maps one to one.

#![allow(unsafe_code)]
// Enum constants have different integer types on different systems, so casts that are
// no-ops here are needed elsewhere.
#![allow(clippy::unnecessary_cast)]

mod com;
mod process;

use std::cell::RefCell;
use std::ffi::{c_char, c_void};
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gt_core::{PluginFormat, PluginKind, PluginParamInfo, MAX_PLUGIN_PARAMS};
use gt_engine::RENDER_QUANTUM;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper, Interface};

use crate::backend::{Activated, Instance};
use crate::catalog::PluginInfo;
use crate::host::Signals;
use crate::instance::FRAME;
use crate::presets::{self, Preset, Source};
use crate::processor::OUT_QUEUE;
use crate::window::HostWindow;
use com::{Frame, Handler, HostApp, MemStream, RunLoop};
use process::{Setup, Vst3Processor};

/// Header of our state blob: component stream and controller stream, each with its length.
const STATE_MAGIC: &[u8; 4] = b"GTV3";

pub(crate) fn guid_eq(a: &TUID, b: &[u8; 16]) -> bool {
    a.iter().zip(b).all(|(x, y)| *x as u8 == *y)
}

pub(crate) fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let max = dst.len() - 1;
    let mut n = 0;
    for (d, s) in dst.iter_mut().zip(src.encode_utf16()).take(max) {
        *d = s as TChar;
        n += 1;
    }
    dst[n] = 0;
}

fn wide(s: &[TChar]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end]).trim().to_owned()
}

fn narrow(s: &[c_char]) -> String {
    let bytes: Vec<u8> = s
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

/// A class id as text (32 hex digits), the plugin's id in the catalog and the document.
fn cid_hex(cid: &TUID) -> String {
    cid.iter().map(|&b| format!("{:02X}", b as u8)).collect()
}

fn cid_parse(id: &str) -> Option<TUID> {
    if id.len() != 32 || !id.is_ascii() {
        return None;
    }
    let mut out: TUID = [0; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&id[2 * i..2 * i + 2], 16).ok()? as c_char;
    }
    Some(out)
}

/// The binary inside a `.vst3` bundle for this system (or the file itself when it is not a
/// bundle): `Contents/x86_64-linux/<name>.so`, `Contents/x86_64-win/<name>.vst3`,
/// `Contents/MacOS/<name>`.
pub(crate) fn binary_path(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_owned());
    }
    let contents = path.join("Contents");
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let arch = std::env::consts::ARCH;
    let (dirs, file): (Vec<String>, String) = if cfg!(target_os = "macos") {
        (vec!["MacOS".to_owned()], stem)
    } else if cfg!(windows) {
        let win = match arch {
            "aarch64" => vec!["arm64-win".to_owned(), "arm64ec-win".to_owned()],
            "x86" => vec!["x86-win".to_owned()],
            _ => vec!["x86_64-win".to_owned()],
        };
        (win, format!("{stem}.vst3"))
    } else {
        (vec![format!("{arch}-linux")], format!("{stem}.so"))
    };
    for d in dirs {
        let dir = contents.join(d);
        let exact = dir.join(&file);
        if exact.is_file() {
            return Some(exact);
        }
        // Renamed bundles keep their binary's own name: take the one binary there.
        let found = std::fs::read_dir(&dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.is_file()
                    && (cfg!(target_os = "macos")
                        || p.extension().is_some_and(|e| {
                            e.eq_ignore_ascii_case(if cfg!(windows) { "vst3" } else { "so" })
                        }))
            });
        if found.is_some() {
            return found;
        }
    }
    None
}

type GetFactory = unsafe extern "system" fn() -> *mut IPluginFactory;

/// A loaded VST3 module.
pub(crate) struct Module {
    /// Released before the module's exit call (see `drop`).
    factory: ManuallyDrop<ComPtr<IPluginFactory>>,
    library: Option<libloading::Library>,
    exit: Option<unsafe extern "system" fn() -> bool>,
    #[cfg(target_os = "macos")]
    bundle: *mut c_void,
}

#[cfg(target_os = "macos")]
mod cf {
    use std::ffi::c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub(super) fn CFURLCreateFromFileSystemRepresentation(
            alloc: *const c_void,
            buf: *const u8,
            len: isize,
            is_dir: u8,
        ) -> *const c_void;
        pub(super) fn CFBundleCreate(alloc: *const c_void, url: *const c_void) -> *mut c_void;
        pub(super) fn CFRelease(cf: *const c_void);
    }
}

impl Module {
    /// Loads a `.vst3` bundle or file: runs its entry call and gets its factory.
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let bin = binary_path(path).ok_or("no binary for this system in the bundle")?;
        // SAFETY: loading runs the plugin's initialization code; callers run this in the scan
        // process or under the crash guard.
        #[cfg(target_os = "linux")]
        let (library, handle) = {
            let lib = unsafe { libloading::os::unix::Library::new(&bin) }
                .map_err(|e| format!("cannot load: {e}"))?;
            let handle = lib.into_raw();
            // SAFETY: the handle was just taken from a library.
            (
                libloading::Library::from(unsafe {
                    libloading::os::unix::Library::from_raw(handle)
                }),
                handle,
            )
        };
        #[cfg(not(target_os = "linux"))]
        let library =
            unsafe { libloading::Library::new(&bin) }.map_err(|e| format!("cannot load: {e}"))?;
        #[cfg(target_os = "macos")]
        let mut bundle = std::ptr::null_mut();
        // SAFETY: the symbols have the signatures the VST3 SDK defines for them.
        let exit = unsafe {
            #[cfg(target_os = "linux")]
            {
                type Entry = unsafe extern "system" fn(*mut c_void) -> bool;
                if let Ok(entry) = library.get::<Entry>(b"ModuleEntry\0") {
                    if !entry(handle) {
                        return Err("the plugin refused to start (ModuleEntry)".to_owned());
                    }
                }
                library
                    .get::<unsafe extern "system" fn() -> bool>(b"ModuleExit\0")
                    .ok()
                    .map(|s| *s)
            }
            #[cfg(windows)]
            {
                if let Ok(init) = library.get::<unsafe extern "system" fn() -> bool>(b"InitDll\0") {
                    if !init() {
                        return Err("the plugin refused to start (InitDll)".to_owned());
                    }
                }
                library
                    .get::<unsafe extern "system" fn() -> bool>(b"ExitDll\0")
                    .ok()
                    .map(|s| *s)
            }
            #[cfg(target_os = "macos")]
            {
                type Entry = unsafe extern "system" fn(*mut c_void) -> bool;
                let entry = library
                    .get::<Entry>(b"bundleEntry\0")
                    .or_else(|_| library.get::<Entry>(b"BundleEntry\0"));
                if let Ok(entry) = entry {
                    let bytes = path.as_os_str().as_encoded_bytes();
                    let url = cf::CFURLCreateFromFileSystemRepresentation(
                        std::ptr::null(),
                        bytes.as_ptr(),
                        bytes.len() as isize,
                        1,
                    );
                    if !url.is_null() {
                        bundle = cf::CFBundleCreate(std::ptr::null(), url);
                        cf::CFRelease(url);
                    }
                    if !entry(bundle) {
                        if !bundle.is_null() {
                            cf::CFRelease(bundle);
                        }
                        return Err("the plugin refused to start (bundleEntry)".to_owned());
                    }
                }
                library
                    .get::<unsafe extern "system" fn() -> bool>(b"bundleExit\0")
                    .or_else(|_| library.get(b"BundleExit\0"))
                    .ok()
                    .map(|s| *s)
            }
            #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
            None
        };
        // SAFETY: as above.
        let get = unsafe { library.get::<GetFactory>(b"GetPluginFactory\0") }
            .map_err(|_| "not a VST3 plugin (no GetPluginFactory)".to_owned())?;
        // SAFETY: the factory pointer is returned with a reference for us.
        let factory = unsafe { ComPtr::from_raw(get()) }.ok_or("the plugin has no factory")?;
        Ok(Self {
            factory: ManuallyDrop::new(factory),
            library: Some(library),
            exit,
            #[cfg(target_os = "macos")]
            bundle,
        })
    }

    /// A module whose factory function is linked into this program (tests).
    #[cfg(test)]
    pub(crate) fn from_factory(get: GetFactory) -> Self {
        // SAFETY: as in `load`.
        let factory = unsafe { ComPtr::from_raw(get()) }.expect("a factory");
        Self {
            factory: ManuallyDrop::new(factory),
            library: None,
            exit: None,
            #[cfg(target_os = "macos")]
            bundle: std::ptr::null_mut(),
        }
    }

    /// The plugins (audio module classes) the module offers.
    pub(crate) fn list(&self, path: &Path) -> Vec<PluginInfo> {
        let f = &self.factory;
        // SAFETY: plain calls into the factory with structs we own.
        unsafe {
            let mut finfo: PFactoryInfo = std::mem::zeroed();
            let vendor = if f.getFactoryInfo(&mut finfo) == kResultOk {
                narrow(&finfo.vendor)
            } else {
                String::new()
            };
            let f2 = f.cast::<IPluginFactory2>();
            let mut out = Vec::new();
            for i in 0..f.countClasses().clamp(0, 1024) {
                let (cid, category, name, subs, class_vendor, version) = match &f2 {
                    Some(f2) => {
                        let mut c: PClassInfo2 = std::mem::zeroed();
                        if f2.getClassInfo2(i, &mut c) != kResultOk {
                            continue;
                        }
                        (
                            c.cid,
                            narrow(&c.category),
                            narrow(&c.name),
                            narrow(&c.subCategories),
                            narrow(&c.vendor),
                            narrow(&c.version),
                        )
                    }
                    None => {
                        let mut c: PClassInfo = std::mem::zeroed();
                        if f.getClassInfo(i, &mut c) != kResultOk {
                            continue;
                        }
                        let name = narrow(&c.name);
                        (
                            c.cid,
                            narrow(&c.category),
                            name,
                            String::new(),
                            String::new(),
                            String::new(),
                        )
                    }
                };
                if category != "Audio Module Class" {
                    continue;
                }
                let subs: Vec<&str> = subs.split('|').filter(|s| !s.is_empty()).collect();
                let instrument = subs.iter().any(|s| s.eq_ignore_ascii_case("Instrument"));
                let mut features = vec![if instrument {
                    "instrument"
                } else {
                    "audio-effect"
                }
                .to_owned()];
                features.extend(
                    subs.iter()
                        .filter(|s| !matches!(s.to_lowercase().as_str(), "instrument" | "fx"))
                        .map(|s| s.to_lowercase()),
                );
                out.push(PluginInfo {
                    format: PluginFormat::Vst3,
                    id: cid_hex(&cid),
                    name,
                    vendor: if class_vendor.is_empty() {
                        vendor.clone()
                    } else {
                        class_vendor
                    },
                    version,
                    description: subs.join(" · "),
                    features,
                    path: path.to_owned(),
                });
            }
            out
        }
    }

    /// Creates an object of class `cid` with interface `I`.
    fn create<I: Interface>(&self, cid: &TUID) -> Option<ComPtr<I>> {
        let mut obj = std::ptr::null_mut();
        // SAFETY: the factory writes an object with a reference for us, or nothing.
        unsafe {
            let r =
                self.factory
                    .createInstance(cid.as_ptr(), I::IID.as_ptr() as FIDString, &mut obj);
            if r != kResultOk {
                return None;
            }
            ComPtr::from_raw(obj as *mut I)
        }
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        // SAFETY: the factory is released once, here, before the module's exit call; it is
        // not used again.
        unsafe { ManuallyDrop::drop(&mut self.factory) };
        if let Some(exit) = self.exit {
            // SAFETY: the module's own exit call, once, after its last object we hold.
            unsafe {
                exit();
            }
        }
        #[cfg(target_os = "macos")]
        if !self.bundle.is_null() {
            // SAFETY: we created the bundle reference.
            unsafe { cf::CFRelease(self.bundle) };
        }
        drop(self.library.take());
    }
}

/// An open editor.
struct Editor {
    view: ComPtr<IPlugView>,
    _frame: ComWrapper<Frame>,
    window: HostWindow,
    resizable: bool,
}

/// One VST3 plugin instance on the main thread.
pub(crate) struct Vst3Plugin {
    info: PluginInfo,
    signals: Arc<Signals>,
    _module: Rc<Module>,
    _host: ComWrapper<HostApp>,
    handler: ComWrapper<Handler>,
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: Option<ComPtr<IEditController>>,
    /// The controller is a separate object (it is initialized and connected by us).
    separate: bool,
    connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    /// Parameter ids in the document's order.
    ids: Arc<[u32]>,
    /// Their units ("dB"), shown after values the plugin writes without them.
    units: Vec<String>,
    /// The program-change parameter: its index, unit and number of steps.
    program: Option<(usize, i32, i32)>,
    offline: bool,
    active: bool,
    token: Arc<()>,
    run_loop: Rc<RefCell<RunLoop>>,
    editor: Option<Editor>,
}

impl Vst3Plugin {
    /// Creates an instance of `info` from a loaded module.
    pub(crate) fn create(
        module: &Rc<Module>,
        info: &PluginInfo,
        signals: Arc<Signals>,
    ) -> Result<Self, String> {
        let name = &info.name;
        let cid = cid_parse(&info.id).ok_or_else(|| format!("{name}: bad VST3 class id"))?;
        let component = module
            .create::<IComponent>(&cid)
            .ok_or_else(|| format!("cannot create {name}"))?;
        let host = ComWrapper::new(HostApp);
        let context = host
            .as_com_ref::<IHostApplication>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr() as *mut FUnknown);
        // SAFETY: calls into the plugin's component with live objects of ours.
        unsafe {
            if component.initialize(context) != kResultOk {
                return Err(format!("{name} failed to initialize"));
            }
        }
        let processor = match component.cast::<IAudioProcessor>() {
            Some(p) => p,
            None => {
                // SAFETY: initialized above.
                unsafe { component.terminate() };
                return Err(format!("{name} has no audio processor"));
            }
        };
        let handler = ComWrapper::new(Handler {
            signals: Arc::clone(&signals),
            edits: Mutex::new(Vec::new()),
        });
        let mut plugin = Self {
            info: info.clone(),
            signals,
            _module: Rc::clone(module),
            _host: host,
            handler,
            component,
            processor,
            controller: None,
            separate: false,
            connection: None,
            ids: Arc::from(Vec::new()),
            units: Vec::new(),
            program: None,
            offline: false,
            active: false,
            token: Arc::new(()),
            run_loop: Rc::new(RefCell::new(RunLoop::default())),
            editor: None,
        };
        plugin.attach_controller(module, context);
        Ok(plugin)
    }

    /// Finds or creates the edit controller, connects it and gives it the component's state.
    fn attach_controller(&mut self, module: &Module, context: *mut FUnknown) {
        // SAFETY: calls into the plugin with live objects; failures leave it without a
        // controller (no parameters or editor, but it still plays).
        unsafe {
            let controller = match self.component.cast::<IEditController>() {
                Some(c) => Some(c),
                None => {
                    let mut cid: TUID = [0; 16];
                    if self.component.getControllerClassId(&mut cid) == kResultOk {
                        module
                            .create::<IEditController>(&cid)
                            .filter(|c| c.initialize(context) == kResultOk)
                    } else {
                        None
                    }
                }
            };
            let Some(controller) = controller else {
                return;
            };
            self.separate = self.component.cast::<IEditController>().is_none();
            if self.separate {
                if let (Some(a), Some(b)) = (
                    self.component.cast::<IConnectionPoint>(),
                    controller.cast::<IConnectionPoint>(),
                ) {
                    a.connect(b.as_ptr());
                    b.connect(a.as_ptr());
                    self.connection = Some((a, b));
                }
            }
            if let Some(h) = self.handler.as_com_ref::<IComponentHandler>() {
                controller.setComponentHandler(h.as_ptr());
            }
            self.controller = Some(controller);
        }
        self.sync_controller();
    }

    /// Gives the controller the component's current state.
    fn sync_controller(&mut self) {
        let Some(c) = &self.controller else {
            return;
        };
        let stream = MemStream::new(Vec::new());
        let Some(s) = stream.as_com_ref::<IBStream>() else {
            return;
        };
        // SAFETY: plain calls with a stream we own.
        unsafe {
            if self.component.getState(s.as_ptr()) == kResultOk {
                stream.rewind();
                c.setComponentState(s.as_ptr());
            }
        }
    }

    /// The programs of the program-change parameter's unit, as presets.
    fn programs(&self) -> Vec<Preset> {
        let (Some(c), Some((index, unit, steps))) = (&self.controller, self.program) else {
            return Vec::new();
        };
        let Some(units) = c.cast::<IUnitInfo>() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        // SAFETY: plain queries with structs we own.
        unsafe {
            let mut list = None;
            for u in 0..units.getUnitCount().clamp(0, 4096) {
                let mut info: UnitInfo = std::mem::zeroed();
                if units.getUnitInfo(u, &mut info) == kResultOk && info.id == unit {
                    list = Some(info.programListId);
                }
            }
            let Some(list) = list.filter(|l| *l != kNoProgramListId) else {
                return Vec::new();
            };
            for k in 0..=steps.min(4096) {
                let mut name: String128 = std::mem::zeroed();
                if units.getProgramName(list, k, &mut name) != kResultOk {
                    continue;
                }
                out.push(Preset {
                    name: wide(&name),
                    user: false,
                    source: Source::Vst3Program {
                        index,
                        value: k as f32 / steps as f32,
                    },
                });
            }
        }
        out
    }

    fn save(&self, controller: bool) -> Option<Vec<u8>> {
        let stream = MemStream::new(Vec::new());
        let s = stream.as_com_ref::<IBStream>()?;
        // SAFETY: plain calls with a stream we own.
        let r = unsafe {
            if controller {
                self.controller.as_ref()?.getState(s.as_ptr())
            } else {
                self.component.getState(s.as_ptr())
            }
        };
        (r == kResultOk).then(|| stream.take())
    }

    /// Bus channel counts of one direction (audio).
    fn buses(&self, dir: BusDirections) -> Vec<usize> {
        // SAFETY: plain queries with structs we own.
        unsafe {
            let count = self
                .component
                .getBusCount(MediaTypes_::kAudio as MediaType, dir as BusDirection)
                .clamp(0, 64);
            (0..count)
                .map(|i| {
                    let mut b: BusInfo = std::mem::zeroed();
                    let ok = self.component.getBusInfo(
                        MediaTypes_::kAudio as MediaType,
                        dir as BusDirection,
                        i,
                        &mut b,
                    ) == kResultOk;
                    if ok {
                        b.channelCount.clamp(0, 32) as usize
                    } else {
                        0
                    }
                })
                .collect()
        }
    }

    fn midi_map(&self) -> [Option<u32>; 130] {
        let mut map = [None; 130];
        let Some(m) = self
            .controller
            .as_ref()
            .and_then(|c| c.cast::<IMidiMapping>())
        else {
            return map;
        };
        for (n, slot) in map.iter_mut().enumerate() {
            let mut id = 0;
            // SAFETY: a plain query.
            if unsafe { m.getMidiControllerAssignment(0, 0, n as i16, &mut id) } == kResultOk {
                *slot = Some(id);
            }
        }
        map
    }
}

impl Instance for Vst3Plugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn signals(&self) -> &Arc<Signals> {
        &self.signals
    }

    fn read_params(&mut self) -> (Vec<PluginParamInfo>, Vec<f32>) {
        let Some(c) = &self.controller else {
            self.ids = Arc::from(Vec::new());
            return (Vec::new(), Vec::new());
        };
        let mut infos = Vec::new();
        let mut ids = Vec::new();
        let mut units = Vec::new();
        let mut values = Vec::new();
        self.program = None;
        // SAFETY: plain queries with structs we own.
        unsafe {
            let count = c.getParameterCount().clamp(0, MAX_PLUGIN_PARAMS as i32);
            for i in 0..count {
                let mut p: ParameterInfo = std::mem::zeroed();
                if c.getParameterInfo(i, &mut p) != kResultOk {
                    continue;
                }
                let flags = p.flags as u32;
                let has = |f: ParameterInfo_::ParameterFlags| flags & f as u32 != 0;
                use ParameterInfo_::ParameterFlags_::*;
                let default = (p.defaultNormalizedValue as f32).clamp(0.0, 1.0);
                infos.push(PluginParamInfo {
                    id: p.id,
                    name: wide(&p.title),
                    default,
                    steps: p.stepCount.clamp(0, 1_000_000) as u32,
                    automatable: has(kCanAutomate) && !has(kIsReadOnly),
                    hidden: has(kIsHidden),
                });
                if has(kIsProgramChange) && p.stepCount > 0 && self.program.is_none() {
                    self.program = Some((ids.len(), p.unitId, p.stepCount));
                }
                ids.push(p.id);
                units.push(wide(&p.units));
                let v = c.getParamNormalized(p.id) as f32;
                values.push(if v.is_finite() {
                    v.clamp(0.0, 1.0)
                } else {
                    default
                });
            }
        }
        self.ids = Arc::from(ids);
        self.units = units;
        (infos, values)
    }

    fn factory_presets(&mut self) -> Vec<Preset> {
        let mut out = self.programs();
        let class = self.info.id.to_ascii_uppercase();
        for folder in presets::vst3_folders(&self.info.vendor, &self.info.name) {
            for f in presets::files_in(&folder, Some("vstpreset")) {
                // Only files for this class (read the header, not the whole file).
                let mut head = [0u8; 40];
                let ok = std::fs::File::open(&f)
                    .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut head))
                    .is_ok();
                if !ok
                    || &head[..4] != b"VST3"
                    || head[8..40].to_ascii_uppercase() != *class.as_bytes()
                {
                    continue;
                }
                out.push(Preset {
                    name: f
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    user: false,
                    source: Source::Vst3File(f),
                });
            }
        }
        out
    }

    fn load_factory(&mut self, source: &Source) -> Result<(), String> {
        match source {
            Source::Vst3File(path) => {
                let bad = || format!("{} is not a preset for {}", path.display(), self.info.name);
                let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
                let (_, comp, cont) = presets::parse_vstpreset(&bytes).ok_or_else(bad)?;
                let mut state = STATE_MAGIC.to_vec();
                state.extend_from_slice(&(comp.len() as u32).to_le_bytes());
                state.extend_from_slice(&comp);
                state.extend_from_slice(&(cont.len() as u32).to_le_bytes());
                state.extend_from_slice(&cont);
                self.load_state(&state)
            }
            &Source::Vst3Program { index, value } => {
                let c = self.controller.as_ref().ok_or("no controller")?;
                let id = *self.ids.get(index).ok_or("no program parameter")?;
                // SAFETY: a plain call. The host sends the value to the processor next.
                unsafe { c.setParamNormalized(id, f64::from(value)) };
                Ok(())
            }
            _ => Err("not a VST3 preset".to_owned()),
        }
    }

    fn outputs(&mut self) -> (Vec<String>, bool) {
        let (audio, event) = (
            MediaTypes_::kAudio as MediaType,
            MediaTypes_::kEvent as MediaType,
        );
        let out = BusDirections_::kOutput as BusDirection;
        // SAFETY: plain queries with structs we own.
        unsafe {
            let c = &self.component;
            let count = c.getBusCount(audio, out).clamp(0, 64);
            let names = (1..count)
                .take(gt_core::MAX_EXTRA_OUTPUTS)
                .map(|i| {
                    let mut b: BusInfo = std::mem::zeroed();
                    let name = if c.getBusInfo(audio, out, i, &mut b) == kResultOk {
                        wide(&b.name)
                    } else {
                        String::new()
                    };
                    if name.is_empty() {
                        format!("Out {}", i + 1)
                    } else {
                        name
                    }
                })
                .collect();
            (names, c.getBusCount(event, out) > 0)
        }
    }

    fn value_text(&mut self, index: usize, value: f32) -> Option<String> {
        let c = self.controller.as_ref()?;
        let id = *self.ids.get(index)?;
        // SAFETY: a plain query with a buffer we own.
        let text = unsafe {
            let mut s: String128 = std::mem::zeroed();
            if c.getParamStringByValue(id, f64::from(value), &mut s) != kResultOk {
                return None;
            }
            wide(&s)
        };
        let unit = self.units.get(index).map_or("", |u| u.as_str());
        Some(if unit.is_empty() || text.contains(unit) {
            text
        } else {
            format!("{text} {unit}")
        })
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        let component = self.save(false)?;
        let controller = self.save(true).unwrap_or_default();
        let mut out = STATE_MAGIC.to_vec();
        out.extend_from_slice(&(component.len() as u32).to_le_bytes());
        out.extend_from_slice(&component);
        out.extend_from_slice(&(controller.len() as u32).to_le_bytes());
        out.extend_from_slice(&controller);
        Some(out)
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), String> {
        let bad = || format!("{} rejected its saved state", self.info.name);
        let part = |at: usize| -> Option<(&[u8], usize)> {
            let len = u32::from_le_bytes(state.get(at..at + 4)?.try_into().ok()?) as usize;
            Some((state.get(at + 4..at + 4 + len)?, at + 4 + len))
        };
        if state.get(..4) != Some(&STATE_MAGIC[..]) {
            return Err(bad());
        }
        let (component, next) = part(4).ok_or_else(bad)?;
        let controller = part(next).map_or(&[][..], |(c, _)| c);
        let stream = MemStream::new(component.to_vec());
        let s = stream.as_com_ref::<IBStream>().ok_or_else(bad)?;
        // SAFETY: plain calls with streams we own.
        unsafe {
            if self.component.setState(s.as_ptr()) != kResultOk {
                return Err(bad());
            }
            if let Some(c) = &self.controller {
                stream.rewind();
                c.setComponentState(s.as_ptr());
                if !controller.is_empty() {
                    let cs = MemStream::new(controller.to_vec());
                    if let Some(r) = cs.as_com_ref::<IBStream>() {
                        c.setState(r.as_ptr());
                    }
                }
            }
        }
        Ok(())
    }

    fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
    }

    fn activate(&mut self, sample_rate: u32) -> Result<Activated, String> {
        let name = self.info.name.clone();
        let inputs = self.buses(BusDirections_::kInput);
        let outputs = self.buses(BusDirections_::kOutput);
        // SAFETY: plain calls on an inactive component, as the VST3 activation sequence
        // requires.
        let latency = unsafe {
            let c = &self.component;
            let audio = MediaTypes_::kAudio as MediaType;
            let event = MediaTypes_::kEvent as MediaType;
            let (input, output) = (
                BusDirections_::kInput as BusDirection,
                BusDirections_::kOutput as BusDirection,
            );
            for (i, _) in inputs.iter().enumerate() {
                c.activateBus(audio, input, i as i32, u8::from(i == 0));
            }
            for (i, _) in outputs.iter().enumerate() {
                c.activateBus(audio, output, i as i32, 1);
            }
            let events_in = c.getBusCount(event, input);
            if events_in > 0 {
                c.activateBus(event, input, 0, 1);
            }
            if c.getBusCount(event, output) > 0 {
                c.activateBus(event, output, 0, 1);
            }
            let arrangement = |n: usize| {
                if n >= 2 {
                    SpeakerArr::kStereo
                } else {
                    SpeakerArr::kMono
                }
            };
            let mut ins: Vec<SpeakerArrangement> = inputs.iter().map(|&n| arrangement(n)).collect();
            let mut outs: Vec<SpeakerArrangement> =
                outputs.iter().map(|&n| arrangement(n)).collect();
            self.processor.setBusArrangements(
                ins.as_mut_ptr(),
                ins.len() as i32,
                outs.as_mut_ptr(),
                outs.len() as i32,
            );
            if self
                .processor
                .canProcessSampleSize(SymbolicSampleSizes_::kSample32 as i32)
                != kResultTrue
                && self
                    .processor
                    .canProcessSampleSize(SymbolicSampleSizes_::kSample32 as i32)
                    != kResultOk
            {
                return Err(format!("{name} cannot process 32-bit audio"));
            }
            let mut setup = ProcessSetup {
                processMode: if self.offline {
                    ProcessModes_::kOffline
                } else {
                    ProcessModes_::kRealtime
                } as i32,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
                maxSamplesPerBlock: RENDER_QUANTUM as i32,
                sampleRate: f64::from(sample_rate),
            };
            if self.processor.setupProcessing(&mut setup) != kResultOk {
                return Err(format!("cannot start {name} (setupProcessing)"));
            }
            if c.setActive(1) != kResultOk {
                return Err(format!("cannot start {name} (setActive)"));
            }
            self.active = true;
            (self.processor.getLatencySamples() as usize).min(1 << 20)
        };
        // SAFETY: a plain query.
        let note_input = unsafe {
            self.component.getBusCount(
                MediaTypes_::kEvent as MediaType,
                BusDirections_::kInput as BusDirection,
            ) > 0
        };
        let setup = Setup {
            inputs,
            outputs,
            note_input,
            effect: self.info.kind() == PluginKind::Effect,
            latency,
            sample_rate: f64::from(sample_rate),
            offline: self.offline,
            midi_map: self.midi_map(),
        };
        let (tx, rx) = rtrb::RingBuffer::new(OUT_QUEUE);
        let processor = Vst3Processor::new(
            self.processor.clone(),
            Arc::clone(&self.token),
            Arc::clone(&self.ids),
            setup,
            tx,
        );
        Ok((Box::new(processor), rx))
    }

    fn try_deactivate(&mut self) -> bool {
        if !self.active {
            return true;
        }
        if Arc::strong_count(&self.token) > 1 {
            return false;
        }
        // SAFETY: the engine has handed the processor back.
        unsafe {
            self.component.setActive(0);
        }
        self.active = false;
        true
    }

    fn main_thread_work(&mut self, _now: Instant) {
        #[cfg(target_os = "linux")]
        {
            use vst3::Steinberg::Linux::{IEventHandlerTrait, ITimerHandlerTrait};
            let now = _now;
            let (due, fds) = {
                let mut rl = self.run_loop.borrow_mut();
                let mut due = Vec::new();
                for (h, every, next) in rl.timers.iter_mut() {
                    if now >= *next {
                        due.push(h.clone());
                        *next = now + *every;
                    }
                }
                let fds: Vec<(i32, bool, bool)> =
                    rl.fds.iter().map(|&(_, fd)| (fd, true, false)).collect();
                (due, fds)
            };
            for h in due {
                // SAFETY: a registered handler (we hold a reference), on the main thread.
                unsafe { h.onTimer() };
            }
            for (fd, ..) in crate::host::poll_fds(&fds) {
                let handlers: Vec<_> = self
                    .run_loop
                    .borrow()
                    .fds
                    .iter()
                    .filter(|(_, f)| *f == fd)
                    .map(|(h, _)| h.clone())
                    .collect();
                for h in handlers {
                    // SAFETY: as above.
                    unsafe { h.onFDIsSet(fd) };
                }
            }
        }
    }

    fn next_frame(&self, _now: Instant) -> Option<Duration> {
        #[cfg(target_os = "linux")]
        let timer = self
            .run_loop
            .borrow()
            .timers
            .iter()
            .map(|t| t.2.saturating_duration_since(_now))
            .min();
        #[cfg(not(target_os = "linux"))]
        let timer = None::<Duration>;
        if self.editor.is_some() || self.run_loop.borrow().busy() {
            Some(timer.map_or(FRAME, |t| t.min(FRAME)))
        } else {
            timer
        }
    }

    fn host_changed(&mut self, index: usize, value: f32) {
        if let (Some(c), Some(&id)) = (&self.controller, self.ids.get(index)) {
            // SAFETY: a plain call on the main thread.
            unsafe { c.setParamNormalized(id, f64::from(value)) };
        }
    }

    fn plugin_reported(&mut self, index: usize, value: f32) {
        self.host_changed(index, value);
    }

    fn take_edits(&mut self) -> Vec<(usize, f32)> {
        let edits = self
            .handler
            .edits
            .lock()
            .map(|mut e| std::mem::take(&mut *e))
            .unwrap_or_default();
        edits
            .into_iter()
            .filter_map(|(id, v)| {
                let i = self.ids.iter().position(|&p| p == id)?;
                Some((i, (v as f32).clamp(0.0, 1.0)))
            })
            .collect()
    }

    fn has_editor(&self) -> bool {
        self.controller.is_some()
    }

    fn editor_open(&self) -> bool {
        self.editor.is_some()
    }

    fn open_editor(&mut self, title: &str) -> Result<(), String> {
        if let Some(ed) = self.editor.as_mut() {
            ed.window.raise();
            return Ok(());
        }
        let name = self.info.name.clone();
        let c = self
            .controller
            .as_ref()
            .ok_or_else(|| format!("{name} has no editor"))?;
        let platform = if cfg!(target_os = "linux") {
            kPlatformTypeX11EmbedWindowID
        } else if cfg!(windows) {
            kPlatformTypeHWND
        } else if cfg!(target_os = "macos") {
            kPlatformTypeNSView
        } else {
            return Err("plugin editors are not supported here".to_owned());
        };
        // SAFETY: plain calls on the main thread; the view, frame and window live in the
        // editor record until `close_editor` detaches the view.
        unsafe {
            let view = ComPtr::from_raw(c.createView(ViewType::kEditor))
                .ok_or_else(|| format!("{name} has no editor"))?;
            if view.isPlatformTypeSupported(platform) != kResultTrue {
                return Err(format!("{name}'s editor cannot run on this system"));
            }
            let mut rect: ViewRect = std::mem::zeroed();
            let (w, h) = if view.getSize(&mut rect) == kResultOk {
                (
                    (rect.right - rect.left).max(1) as u32,
                    (rect.bottom - rect.top).max(1) as u32,
                )
            } else {
                (640, 480)
            };
            let resizable = view.canResize() == kResultTrue;
            let window = HostWindow::open(title, w, h, resizable)
                .map_err(|e| format!("cannot open a window for {name}: {e}"))?;
            let frame = ComWrapper::new(Frame {
                signals: Arc::clone(&self.signals),
                run_loop: Rc::clone(&self.run_loop),
            });
            if let Some(f) = frame.as_com_ref::<IPlugFrame>() {
                view.setFrame(f.as_ptr());
            }
            if view.attached(window.raw_handle(), platform) != kResultOk {
                view.setFrame(std::ptr::null_mut());
                return Err(format!("{name} could not open its editor"));
            }
            self.editor = Some(Editor {
                view,
                _frame: frame,
                window,
                resizable,
            });
        }
        Ok(())
    }

    fn close_editor(&mut self) {
        if let Some(ed) = self.editor.take() {
            // SAFETY: detaches the view before its window goes.
            unsafe {
                ed.view.removed();
                ed.view.setFrame(std::ptr::null_mut());
            }
            drop(ed.view);
            drop(ed.window);
        }
    }

    fn poll_editor(&mut self) {
        let Some(ed) = self.editor.as_mut() else {
            return;
        };
        let rect = |w: u32, h: u32| ViewRect {
            left: 0,
            top: 0,
            right: w as i32,
            bottom: h as i32,
        };
        if let Some((w, h)) = self.signals.take_resize() {
            ed.window.resize(w, h, ed.resizable);
            let mut r = rect(w, h);
            // SAFETY: a live view.
            unsafe { ed.view.onSize(&mut r) };
        }
        let ev = ed.window.poll();
        if let Some((w, h)) = ev.resized {
            let mut r = rect(w, h);
            // SAFETY: a live view.
            unsafe {
                if ed.resizable {
                    ed.view.checkSizeConstraint(&mut r);
                    ed.view.onSize(&mut r);
                } else {
                    ed.view.getSize(&mut r);
                }
            }
            let (rw, rh) = (
                (r.right - r.left).max(1) as u32,
                (r.bottom - r.top).max(1) as u32,
            );
            if (rw, rh) != (w, h) {
                ed.window.resize(rw, rh, ed.resizable);
            }
        }
        if ev.close {
            self.close_editor();
        }
    }
}

impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        self.close_editor();
        // SAFETY: the teardown sequence of the VST3 SDK: deactivate, disconnect, terminate.
        unsafe {
            if self.active && Arc::strong_count(&self.token) == 1 {
                self.component.setActive(0);
            }
            if let Some((a, b)) = self.connection.take() {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if let Some(c) = self.controller.take() {
                c.setComponentHandler(std::ptr::null_mut());
                if self.separate {
                    c.terminate();
                }
            }
            self.component.terminate();
        }
    }
}

#[cfg(test)]
mod tests;
