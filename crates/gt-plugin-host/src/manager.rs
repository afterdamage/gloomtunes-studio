//! The plugin host: keeps the running plugin instances in step with the project document and
//! the engine, on the UI (main) thread.
//!
//! Each frame [`PluginHost::sync`] compares the document's plugins with the running ones:
//! new ones are loaded (state restored, parameters read into the document's mirror), removed
//! ones are taken out of the engine and destroyed once the engine has handed their processor
//! back, and parameter values edited in the document are sent to the engine. It also runs the
//! plugins' main-thread requests (callbacks, timers, file descriptors, restarts) and their
//! editor windows.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clack_extensions::gui::{GuiConfiguration, GuiSize, PluginGui};
use clack_host::prelude::PluginEntry;
use gt_core::{PluginInstanceId, PluginKind, PluginRef, Project};
use gt_engine::{EngineCommand, EngineHandle, PluginBox, MAX_PLUGINS};

use crate::catalog::{self, Catalog, PluginInfo};
use crate::guard::{CrashReport, Guard};
use crate::host::{Signals, Waker};
use crate::instance::LoadedPlugin;
use crate::processor::ParamOut;
use crate::window::HostWindow;

/// Saved states kept for plugins taken out of the project, so undo brings them back as they
/// were.
const STATE_CACHE: usize = 64;
/// Parameter changes sent to the engine per frame at most (the rest follow next frame).
const PARAMS_PER_FRAME: usize = 512;

/// How a plugin in the project is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginStatus {
    /// Loaded and running.
    Running,
    /// Loaded, waiting for the audio engine.
    Waiting,
    /// It reported an error or produced invalid output, and is bypassed.
    Failed,
    /// It could not be loaded; the reason. Its saved state is kept in the project.
    Unavailable(String),
}

/// A background scan of the plugin folders.
struct ScanJob {
    rx: Receiver<ScanMsg>,
    progress: (usize, usize, String),
}

enum ScanMsg {
    Progress(usize, usize, String),
    Done(Catalog),
}

/// An open plugin editor.
struct Editor {
    gui: PluginGui,
    /// Our window, for an embedded editor (none for a floating one).
    window: Option<HostWindow>,
    resizable: bool,
}

/// A running instance.
pub(crate) struct Live {
    plugin: LoadedPlugin,
    /// Entry in the engine's plugin table.
    slot: u8,
    /// The engine has its processor.
    in_engine: bool,
    /// A processor not accepted yet (the command queue was full).
    pending: Option<Box<PluginBox>>,
    /// Values last sent to the engine (or reported by the plugin).
    sent: Vec<f32>,
    out: Option<rtrb::Consumer<ParamOut>>,
    failed: bool,
    /// Deactivation is needed before the next activation (restart or new engine).
    restart: bool,
    editor: Option<Editor>,
}

/// What a sync changed in the document.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SyncOutcome {
    /// Parameter values or descriptions in the document were updated from plugins (not an
    /// undoable edit: the app should refresh what depends on them).
    pub mirrored: bool,
    /// A plugin said its state changed (from its editor): the project has unsaved changes.
    pub dirty: bool,
}

/// The plugin host.
pub struct PluginHost {
    dir: PathBuf,
    exe: PathBuf,
    waker: Waker,
    guard: Guard,
    catalog: Catalog,
    folders: Vec<PathBuf>,
    scan: Option<ScanJob>,
    entries: HashMap<PathBuf, PluginEntry>,
    pub(crate) live: BTreeMap<PluginInstanceId, Live>,
    unavailable: BTreeMap<PluginInstanceId, String>,
    pub(crate) retiring: Vec<LoadedPlugin>,
    exporting: Vec<LoadedPlugin>,
    states: VecDeque<(PluginInstanceId, Arc<[u8]>)>,
    sample_rate: Option<u32>,
    notices: Vec<String>,
}

impl PluginHost {
    /// Starts the host. `dir` is the `plugins` folder in the app's data folder; `exe` is this
    /// program (run with `--scan-clap` to scan); `waker` asks the UI for a frame. Returns what
    /// the last session left behind.
    pub fn new(dir: &Path, exe: PathBuf, waker: Waker) -> (Self, CrashReport) {
        let (guard, report) = Guard::open(dir);
        let folders = std::fs::read(dir.join("folders.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let host = Self {
            dir: dir.to_owned(),
            exe,
            waker,
            guard,
            catalog: Catalog::load(&dir.join("catalog.json")),
            folders,
            scan: None,
            entries: HashMap::new(),
            live: BTreeMap::new(),
            unavailable: BTreeMap::new(),
            retiring: Vec::new(),
            exporting: Vec::new(),
            states: VecDeque::new(),
            sample_rate: None,
            notices: Vec::new(),
        };
        (host, report)
    }

    // ---- Finding plugins ----

    /// The known plugins and plugin files.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Extra folders searched besides the standard ones.
    pub fn folders(&self) -> &[PathBuf] {
        &self.folders
    }

    /// Every folder searched.
    pub fn search_folders(&self) -> Vec<PathBuf> {
        let mut all = catalog::standard_folders();
        all.extend(self.folders.iter().cloned());
        all
    }

    /// Sets the extra folders (saved).
    pub fn set_folders(&mut self, folders: Vec<PathBuf>) {
        self.folders = folders;
        let _ = std::fs::create_dir_all(&self.dir);
        if let Ok(json) = serde_json::to_vec_pretty(&self.folders) {
            let _ = std::fs::write(self.dir.join("folders.json"), json);
        }
    }

    /// Scan progress: files done, files in total, the file being scanned.
    pub fn scanning(&self) -> Option<&(usize, usize, String)> {
        self.scan.as_ref().map(|s| &s.progress)
    }

    /// Scans the plugin folders in the background (new and changed files only).
    pub fn start_scan(&mut self) {
        if self.scan.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let folders = self.search_folders();
        let old = self.catalog.clone();
        let quarantined: Vec<(PathBuf, String)> = self
            .guard
            .quarantine()
            .map(|(p, r)| (p.to_owned(), format!("quarantined: {r}")))
            .collect();
        let exe = self.exe.clone();
        let cache = self.dir.join("catalog.json");
        let waker = Arc::clone(&self.waker);
        let spawned = std::thread::Builder::new()
            .name("plugin-scan".to_owned())
            .spawn(move || {
                let files = catalog::find_files(&folders);
                let skip = |p: &Path| {
                    quarantined
                        .iter()
                        .find(|(q, _)| q == p)
                        .map(|(_, r)| r.clone())
                };
                let scan = |p: &Path| catalog::scan_with_child(&exe, p);
                let progress = |done: usize, total: usize, file: &Path| {
                    let name = file
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let _ = tx.send(ScanMsg::Progress(done, total, name));
                    waker();
                };
                let cat = catalog::rescan(&old, &files, &skip, &scan, &progress);
                if let Err(e) = cat.save(&cache) {
                    log::warn!("cannot save the plugin list: {e}");
                }
                let _ = tx.send(ScanMsg::Done(cat));
                waker();
            });
        match spawned {
            Ok(_) => {
                self.scan = Some(ScanJob {
                    rx,
                    progress: (0, 0, String::new()),
                })
            }
            Err(e) => self.notices.push(format!("Cannot scan for plugins: {e}")),
        }
    }

    /// Quarantined plugin files with the reason.
    pub fn quarantine(&self) -> Vec<(PathBuf, String)> {
        self.guard
            .quarantine()
            .map(|(p, r)| (p.to_owned(), r.to_owned()))
            .collect()
    }

    /// Lets a quarantined file load again (and rescans).
    pub fn release(&mut self, path: &Path) {
        self.guard.release(path);
        self.catalog.files.retain(|f| f.path != path);
        self.unavailable.clear();
        self.start_scan();
    }

    /// Messages for the user since the last call.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    // ---- Instances ----

    /// Uses an already loaded entry for `path` (tests load the test plugin in process).
    #[cfg(test)]
    pub(crate) fn insert_entry(&mut self, path: &Path, entry: PluginEntry) {
        self.entries.insert(path.to_owned(), entry);
    }

    fn entry(&mut self, path: &Path, name: &str) -> Result<(), String> {
        if let Some(why) = self.guard.quarantined(path) {
            return Err(format!("{} is quarantined ({why})", path.display()));
        }
        if !self.entries.contains_key(path) {
            let loaded = self.guard.run(path, name, "loading", || {
                // SAFETY: loading runs the plugin file's initialization code. The in-flight
                // marker quarantines the file if that crashes the app.
                #[allow(unsafe_code)]
                unsafe {
                    PluginEntry::load(path)
                }
            });
            let entry = loaded.map_err(|e| format!("cannot load {}: {e}", path.display()))?;
            self.entries.insert(path.to_owned(), entry);
        }
        Ok(())
    }

    fn create(&mut self, info: &PluginInfo) -> Result<LoadedPlugin, String> {
        let signals = Signals::new(Arc::clone(&self.waker));
        self.entry(&info.path, &info.name)?;
        let entry = &self.entries[&info.path];
        self.guard.run(&info.path, &info.name, "creating", || {
            LoadedPlugin::create(entry, info, signals)
        })
    }

    /// The catalog entry for a plugin in the project, or one made from its saved path.
    fn info_for(&self, p: &PluginRef) -> PluginInfo {
        self.catalog
            .find(&p.id, &p.path)
            .cloned()
            .unwrap_or_else(|| PluginInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                vendor: p.vendor.clone(),
                version: String::new(),
                description: String::new(),
                features: vec![match p.kind {
                    PluginKind::Instrument => "instrument".to_owned(),
                    PluginKind::Effect => "audio-effect".to_owned(),
                }],
                path: p.path.clone(),
            })
    }

    fn free_slot(&self) -> Option<u8> {
        (0..MAX_PLUGINS as u8).find(|s| !self.live.values().any(|l| l.slot == *s))
    }

    fn make_live(&mut self, id: PluginInstanceId, plugin: LoadedPlugin) -> Result<(), String> {
        let slot = self
            .free_slot()
            .ok_or_else(|| format!("at most {MAX_PLUGINS} plugins can run at once"))?;
        self.live.insert(
            id,
            Live {
                plugin,
                slot,
                in_engine: false,
                pending: None,
                sent: Vec::new(),
                out: None,
                failed: false,
                restart: false,
                editor: None,
            },
        );
        Ok(())
    }

    /// Loads a plugin for adding to the project: returns its document entry (parameters
    /// read, default state). It starts running once the project holds it.
    pub fn instantiate(&mut self, info: &PluginInfo) -> Result<PluginRef, String> {
        let mut plugin = self.create(info)?;
        let (params, values) = plugin.read_params();
        let mut p = PluginRef::new(
            &info.id,
            &info.name,
            &info.vendor,
            info.kind(),
            info.path.clone(),
        );
        p.params = Arc::from(params);
        p.values = values;
        p.sanitize();
        self.make_live(p.instance, plugin)?;
        Ok(p)
    }

    /// Loads a project's plugin from its saved state.
    fn load(&mut self, p: &mut PluginRef) -> Result<(), String> {
        let info = self.info_for(p);
        let mut plugin = self.create(&info)?;
        let cached = self
            .states
            .iter()
            .rev()
            .find(|(id, _)| *id == p.instance)
            .map(|(_, s)| Arc::clone(s));
        let state = cached.unwrap_or_else(|| Arc::clone(&p.state));
        if !state.is_empty() {
            let r = self
                .guard
                .run(&info.path, &info.name, "restoring the state of", || {
                    plugin.load_state(&state)
                });
            if let Err(e) = r {
                self.notices.push(e);
            }
        }
        let (params, values) = plugin.read_params();
        p.params = Arc::from(params);
        p.values = values;
        p.path = info.path.clone();
        p.sanitize();
        self.make_live(p.instance, plugin)
    }

    fn retire(&mut self, id: PluginInstanceId, engine: Option<&mut EngineHandle>) {
        let Some(mut live) = self.live.remove(&id) else {
            return;
        };
        Self::close_editor_of(&mut live);
        if let Some(state) = live.plugin.save_state() {
            if self.states.len() >= STATE_CACHE {
                self.states.pop_front();
            }
            self.states.push_back((id, Arc::from(state)));
        }
        if live.in_engine {
            if let Some(e) = engine {
                let _ = e.send(EngineCommand::SetPlugin {
                    index: live.slot,
                    plugin: None,
                });
            }
        }
        drop(live.pending.take());
        self.retiring.push(live.plugin);
    }

    /// Status of the plugin with document id `id`.
    pub fn status(&self, id: PluginInstanceId) -> PluginStatus {
        if let Some(why) = self.unavailable.get(&id) {
            return PluginStatus::Unavailable(why.clone());
        }
        match self.live.get(&id) {
            Some(l) if l.failed => PluginStatus::Failed,
            Some(l) if l.in_engine => PluginStatus::Running,
            Some(_) => PluginStatus::Waiting,
            None => PluginStatus::Unavailable("not loaded".to_owned()),
        }
    }

    /// Keeps every plugin in `project` from loading (after a crash, to open a recovered
    /// project safely). Each shows as unavailable with `reason` until retried.
    pub fn hold(&mut self, project: &Project, reason: &str) {
        for (_, p) in project.plugins() {
            if !self.live.contains_key(&p.instance) {
                self.unavailable.insert(p.instance, reason.to_owned());
            }
        }
    }

    /// Runs a failed plugin again.
    pub fn retry(&mut self, id: PluginInstanceId, engine: Option<&mut EngineHandle>) {
        if self.unavailable.remove(&id).is_some() {
            return; // loaded again at the next sync
        }
        if let (Some(l), Some(e)) = (self.live.get_mut(&id), engine) {
            if e.send(EngineCommand::RetryPlugin(l.slot)).is_ok() {
                l.failed = false;
                // The engine ignored parameter changes while the plugin was bypassed (the one
                // that undoes the failure, say): send every value again.
                l.sent.clear();
            }
        }
    }

    /// Keeps everything in step; call once per UI frame. `sample_rate` is the engine's, and
    /// `fresh_engine` true right after the engine was (re)created.
    pub fn sync(
        &mut self,
        project: &mut Project,
        mut engine: Option<&mut EngineHandle>,
        fresh_engine: bool,
    ) -> SyncOutcome {
        let mut out = SyncOutcome::default();
        self.poll_scan();
        let rate = engine.as_ref().map(|e| e.config().sample_rate);
        if fresh_engine || rate != self.sample_rate {
            // The old engine is gone with the processors it held.
            for l in self.live.values_mut() {
                l.in_engine = false;
                l.pending = None;
                l.restart = true;
                l.failed = false;
            }
            self.sample_rate = rate;
        }

        // Retire instances the project no longer has.
        let wanted: Vec<PluginInstanceId> =
            project.plugins().iter().map(|(_, p)| p.instance).collect();
        let gone: Vec<PluginInstanceId> = self
            .live
            .keys()
            .filter(|id| !wanted.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.retire(id, engine.as_deref_mut());
        }
        self.unavailable.retain(|id, _| wanted.contains(id));

        // Load new ones.
        for id in wanted {
            if self.live.contains_key(&id) || self.unavailable.contains_key(&id) {
                continue;
            }
            let Some(p) = project.plugin_by_instance_mut(id) else {
                continue;
            };
            let mut copy = p.clone();
            match self.load(&mut copy) {
                Ok(()) => {
                    *project.plugin_by_instance_mut(id).expect("still there") = copy;
                    out.mirrored = true;
                }
                Err(e) => {
                    log::warn!("plugin {}: {e}", copy.name);
                    self.notices.push(format!("{}: {e}", copy.name));
                    self.unavailable.insert(id, e);
                }
            }
        }

        self.handle_signals(project, &mut out);
        if let Some(e) = engine {
            self.feed_engine(project, e);
            for l in self.live.values_mut() {
                l.failed = l.in_engine
                    && e.telemetry().plugin_failed[usize::from(l.slot)]
                        .load(std::sync::atomic::Ordering::Relaxed);
            }
        }
        self.read_outputs(project, &mut out);
        self.run_main_thread_work();
        self.poll_editors();

        self.retiring.retain_mut(|p| !p.try_deactivate());
        self.guard.set_in_use(
            self.live
                .values()
                .map(|l| l.plugin.info.path.clone())
                .collect(),
        );
        out
    }

    fn poll_scan(&mut self) {
        let Some(job) = self.scan.as_mut() else {
            return;
        };
        let mut done = None;
        for msg in job.rx.try_iter() {
            match msg {
                ScanMsg::Progress(a, b, f) => job.progress = (a, b, f),
                ScanMsg::Done(c) => done = Some(c),
            }
        }
        if let Some(c) = done {
            let count = c.plugins().len();
            self.catalog = c;
            self.scan = None;
            self.notices
                .push(format!("Plugin scan done: {count} plugins found"));
            // Plugins that were missing may be found now.
            self.unavailable.clear();
        }
    }

    fn handle_signals(&mut self, project: &mut Project, out: &mut SyncOutcome) {
        for (&id, l) in self.live.iter_mut() {
            let s = Arc::clone(&l.plugin.signals);
            if Signals::take(&s.callback) {
                l.plugin.instance.call_on_main_thread_callback();
            }
            if Signals::take(&s.restart) | Signals::take(&s.latency) {
                l.restart = true;
            }
            if Signals::take(&s.dirty) {
                out.dirty = true;
            }
            if Signals::take(&s.rescan_params) {
                let (params, values) = l.plugin.read_params();
                if let Some(p) = project.plugin_by_instance_mut(id) {
                    p.params = Arc::from(params);
                    p.values = values.clone();
                    p.sanitize();
                    out.mirrored = true;
                }
                l.sent = values;
            }
        }
    }

    fn feed_engine(&mut self, project: &Project, engine: &mut EngineHandle) {
        let Some(rate) = self.sample_rate else {
            return;
        };
        let mut budget = PARAMS_PER_FRAME;
        for (&id, l) in self.live.iter_mut() {
            if l.restart {
                // Take the processor out; the plugin can be deactivated once it is back.
                if l.in_engine {
                    let cmd = EngineCommand::SetPlugin {
                        index: l.slot,
                        plugin: None,
                    };
                    if engine.send(cmd).is_err() {
                        continue;
                    }
                    l.in_engine = false;
                }
                l.pending = None;
                if !l.plugin.try_deactivate() {
                    continue;
                }
                l.restart = false;
            }
            if !l.in_engine && l.pending.is_none() {
                if !l.plugin.try_deactivate() {
                    continue; // wait for the engine to hand the processor back
                }
                let info = &l.plugin.info;
                let (path, name) = (info.path.clone(), info.name.clone());
                let plugin = &mut l.plugin;
                let activated = self
                    .guard
                    .run(&path, &name, "starting", || plugin.activate(rate));
                match activated {
                    Ok((processor, rx)) => {
                        l.out = Some(rx);
                        l.pending = Some(Box::new(PluginBox {
                            instance: id,
                            processor: Box::new(processor),
                        }));
                        // Everything is sent fresh to the new processor.
                        l.sent.clear();
                    }
                    Err(e) => {
                        self.notices.push(e);
                        l.restart = true;
                        continue;
                    }
                }
            }
            if let Some(b) = l.pending.take() {
                match engine.send(EngineCommand::SetPlugin {
                    index: l.slot,
                    plugin: Some(b),
                }) {
                    Ok(()) => l.in_engine = true,
                    Err(EngineCommand::SetPlugin { plugin, .. }) => l.pending = plugin,
                    Err(_) => {}
                }
            }
            if !l.in_engine || l.failed {
                continue;
            }
            let Some(p) = project.plugin_by_instance(id).map(|(_, p)| p) else {
                continue;
            };
            l.sent.resize(p.values.len(), f32::NAN);
            for (i, (&v, sent)) in p.values.iter().zip(l.sent.iter_mut()).enumerate() {
                if budget == 0 {
                    break;
                }
                if v != *sent {
                    let cmd = EngineCommand::SetPluginParam {
                        index: l.slot,
                        param: i as u32,
                        value: v,
                    };
                    if engine.send(cmd).is_err() {
                        budget = 0;
                        break;
                    }
                    *sent = v;
                    budget -= 1;
                }
            }
        }
    }

    fn read_outputs(&mut self, project: &mut Project, out: &mut SyncOutcome) {
        for (&id, l) in self.live.iter_mut() {
            let Some(rx) = l.out.as_mut() else {
                continue;
            };
            let Some(p) = project.plugin_by_instance_mut(id) else {
                continue;
            };
            while let Ok(ParamOut { index, value }) = rx.pop() {
                let i = index as usize;
                if let Some(v) = p.values.get_mut(i) {
                    let snapped = p.params[i].snap(value);
                    *v = snapped;
                    if let Some(s) = l.sent.get_mut(i) {
                        *s = snapped;
                    }
                    out.mirrored = true;
                }
            }
        }
    }

    fn run_main_thread_work(&mut self) {
        let now = Instant::now();
        for l in self.live.values_mut() {
            let inst = &mut l.plugin.instance;
            let (ext, due) = inst.access_handler(|h| h.due_timers(now));
            if let Some(ext) = ext {
                for id in due {
                    ext.on_timer(&inst.plugin_handle(), id);
                }
            }
            #[cfg(unix)]
            {
                let (ext, fds) = inst.access_handler(|h| (h.fd_ext.get(), h.fds.borrow().clone()));
                if let Some(ext) = ext {
                    for (fd, flags) in ready_fds(&fds) {
                        ext.on_fd(&inst.plugin_handle(), fd, flags);
                    }
                }
            }
        }
    }

    /// How soon the host wants another frame (timers, file descriptors, open editors).
    pub fn next_frame(&self) -> Option<Duration> {
        let now = Instant::now();
        let mut next = None::<Duration>;
        for l in self.live.values() {
            let t = l.plugin.instance.access_handler(|h| {
                #[cfg(unix)]
                let fds = !h.fds.borrow().is_empty();
                #[cfg(not(unix))]
                let fds = false;
                (h.next_timer(now), fds)
            });
            let mut want = t.0;
            if t.1 || l.editor.is_some() {
                want = Some(want.map_or(Duration::from_millis(16), |w| {
                    w.min(Duration::from_millis(16))
                }));
            }
            if let Some(w) = want {
                next = Some(next.map_or(w, |n| n.min(w)));
            }
        }
        if self.scan.is_some() || !self.retiring.is_empty() {
            next = Some(next.map_or(Duration::from_millis(100), |n| {
                n.min(Duration::from_millis(100))
            }));
        }
        next
    }

    // ---- Editors ----

    /// True if the plugin has an editor of its own.
    pub fn has_editor(&self, id: PluginInstanceId) -> bool {
        self.live
            .get(&id)
            .is_some_and(|l| l.plugin.gui_ext.is_some())
    }

    /// True if its editor is open.
    pub fn editor_open(&self, id: PluginInstanceId) -> bool {
        self.live.get(&id).is_some_and(|l| l.editor.is_some())
    }

    /// Opens (or raises) the plugin's editor: embedded in a window of ours, or floating in
    /// the plugin's own window if it cannot embed.
    pub fn open_editor(&mut self, id: PluginInstanceId, title: &str) -> Result<(), String> {
        let Some(l) = self.live.get_mut(&id) else {
            return Err("the plugin is not loaded".to_owned());
        };
        if let Some(ed) = l.editor.as_mut() {
            if let Some(w) = ed.window.as_mut() {
                w.raise();
            }
            return Ok(());
        }
        let gui = l
            .plugin
            .gui_ext
            .ok_or_else(|| format!("{} has no editor", l.plugin.info.name))?;
        let api = HostWindow::api().ok_or("plugin editors are not supported here")?;
        let embedded = GuiConfiguration {
            api_type: api,
            is_floating: false,
        };
        let floating = GuiConfiguration {
            api_type: api,
            is_floating: true,
        };
        let (path, name) = (l.plugin.info.path.clone(), l.plugin.info.name.clone());
        let plugin = &mut l.plugin;
        let editor = self.guard.run(&path, &name, "opening the editor of", || {
            open_editor(plugin, gui, embedded, floating, title)
        })?;
        l.editor = Some(editor);
        Ok(())
    }

    fn close_editor_of(l: &mut Live) {
        if let Some(ed) = l.editor.take() {
            ed.gui.destroy(&l.plugin.instance.plugin_handle());
            drop(ed.window);
        }
    }

    /// Closes the plugin's editor.
    pub fn close_editor(&mut self, id: PluginInstanceId) {
        if let Some(l) = self.live.get_mut(&id) {
            Self::close_editor_of(l);
        }
    }

    fn poll_editors(&mut self) {
        for l in self.live.values_mut() {
            let Some(ed) = l.editor.as_mut() else {
                continue;
            };
            let h = l.plugin.instance.plugin_handle();
            let mut close = Signals::take(&l.plugin.signals.gui_closed);
            if let Some(size) = l.plugin.signals.take_resize() {
                if let Some(w) = ed.window.as_mut() {
                    w.resize(size.width, size.height, ed.resizable);
                    let _ = ed.gui.set_size(&h, size);
                }
            }
            if let Some(w) = ed.window.as_mut() {
                let ev = w.poll();
                close |= ev.close;
                if let Some((width, height)) = ev.resized {
                    let want = GuiSize { width, height };
                    if ed.resizable {
                        let size = ed.gui.adjust_size(&h, want).unwrap_or(want);
                        let _ = ed.gui.set_size(&h, size);
                        if size != want {
                            w.resize(size.width, size.height, true);
                        }
                    } else if let Some(size) = ed.gui.get_size(&h) {
                        if size != want {
                            w.resize(size.width, size.height, false);
                        }
                    }
                }
            }
            if close {
                Self::close_editor_of(l);
            }
        }
    }

    // ---- Saving and export ----

    /// Copies every running plugin's current state into the project (before saving).
    pub fn store_states(&mut self, project: &mut Project) {
        for (&id, l) in self.live.iter_mut() {
            if let (Some(p), Some(state)) =
                (project.plugin_by_instance_mut(id), l.plugin.save_state())
            {
                p.state = Arc::from(state);
            }
        }
    }

    /// Fresh instances of every plugin in the project, in their current state and switched to
    /// offline rendering, for an export at `sample_rate`. Call [`Self::finish_export`] when the
    /// export is over. Plugins that cannot be loaded are left out (silent or bypassed).
    pub fn export_processors(
        &mut self,
        project: &Project,
        sample_rate: u32,
    ) -> Vec<Box<PluginBox>> {
        self.finish_export();
        let mut out = Vec::new();
        for (_, p) in project.plugins() {
            let id = p.instance;
            let state = self.live.get_mut(&id).and_then(|l| l.plugin.save_state());
            let info = self.info_for(p);
            let mut plugin = match self.create(&info) {
                Ok(pl) => pl,
                Err(e) => {
                    self.notices.push(format!("Export: {e}"));
                    continue;
                }
            };
            let state = state.map(Arc::from).unwrap_or_else(|| Arc::clone(&p.state));
            if !state.is_empty() {
                let _ = self
                    .guard
                    .run(&info.path, &info.name, "restoring the state of", || {
                        plugin.load_state(&state)
                    });
            }
            let _ = plugin.read_params();
            plugin.set_offline(true);
            match self.guard.run(&info.path, &info.name, "starting", || {
                plugin.activate(sample_rate)
            }) {
                Ok((processor, _)) => {
                    let mut processor: Box<dyn gt_engine::PluginProcessor> = Box::new(processor);
                    // Values edited since the state was saved.
                    for (i, &v) in p.values.iter().enumerate() {
                        processor.set_param(i as u32, v);
                    }
                    out.push(Box::new(PluginBox {
                        instance: id,
                        processor,
                    }));
                    self.exporting.push(plugin);
                }
                Err(e) => self.notices.push(format!("Export: {e}")),
            }
        }
        out
    }

    /// Releases the export's instances (once their processors are gone).
    pub fn finish_export(&mut self) {
        self.retiring.append(&mut self.exporting);
    }

    /// Closes every editor and records a clean exit. Instances are destroyed when the host is
    /// dropped (after the engine).
    pub fn shutdown(&mut self) {
        for l in self.live.values_mut() {
            Self::close_editor_of(l);
        }
        self.guard.clean_exit();
    }
}

fn open_editor(
    plugin: &mut LoadedPlugin,
    gui: PluginGui,
    embedded: GuiConfiguration,
    floating: GuiConfiguration,
    title: &str,
) -> Result<Editor, String> {
    let h = plugin.instance.plugin_handle();
    let name = plugin.info.name.clone();
    if gui.is_api_supported(&h, embedded) {
        gui.create(&h, embedded)
            .map_err(|e| format!("{name} could not open its editor: {e}"))?;
        let _ = gui.set_scale(&h, 1.0);
        let size = gui.get_size(&h).unwrap_or(GuiSize {
            width: 640,
            height: 480,
        });
        let resizable = gui.can_resize(&h);
        let window = match HostWindow::open(title, size.width, size.height, resizable) {
            Ok(w) => w,
            Err(e) => {
                gui.destroy(&h);
                return Err(format!("cannot open a window for {name}: {e}"));
            }
        };
        // SAFETY: the window lives in the editor record and is dropped only after
        // `gui.destroy` (see `close_editor_of`).
        #[allow(unsafe_code)]
        let parented = unsafe { gui.set_parent(&h, window.clap_window()) };
        if let Err(e) = parented {
            gui.destroy(&h);
            return Err(format!("{name} could not open its editor: {e}"));
        }
        let _ = gui.show(&h);
        Ok(Editor {
            gui,
            window: Some(window),
            resizable,
        })
    } else if gui.is_api_supported(&h, floating) {
        gui.create(&h, floating)
            .map_err(|e| format!("{name} could not open its editor: {e}"))?;
        if let Ok(t) = std::ffi::CString::new(title) {
            gui.suggest_title(&h, &t);
        }
        let _ = gui.show(&h);
        Ok(Editor {
            gui,
            window: None,
            resizable: false,
        })
    } else {
        Err(format!("{name}'s editor cannot run on this system"))
    }
}

/// File descriptors that are ready, with what they are ready for (a zero-timeout poll).
#[cfg(unix)]
fn ready_fds(
    fds: &[(std::os::fd::RawFd, clack_extensions::posix_fd::FdFlags)],
) -> Vec<(std::os::fd::RawFd, clack_extensions::posix_fd::FdFlags)> {
    use clack_extensions::posix_fd::FdFlags;
    if fds.is_empty() {
        return Vec::new();
    }
    let mut polls: Vec<libc::pollfd> = fds
        .iter()
        .map(|&(fd, flags)| libc::pollfd {
            fd,
            events: (if flags.contains(FdFlags::READ) {
                libc::POLLIN
            } else {
                0
            }) | (if flags.contains(FdFlags::WRITE) {
                libc::POLLOUT
            } else {
                0
            }) | (if flags.contains(FdFlags::ERROR) {
                libc::POLLERR
            } else {
                0
            }),
            revents: 0,
        })
        .collect();
    // SAFETY: `polls` is a valid array of pollfd of the given length; zero timeout.
    #[allow(unsafe_code)]
    let n = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, 0) };
    if n <= 0 {
        return Vec::new();
    }
    polls
        .iter()
        .filter(|p| p.revents != 0)
        .map(|p| {
            let mut f = FdFlags::empty();
            if p.revents & libc::POLLIN != 0 {
                f |= FdFlags::READ;
            }
            if p.revents & libc::POLLOUT != 0 {
                f |= FdFlags::WRITE;
            }
            if p.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                f |= FdFlags::ERROR;
            }
            (p.fd, f)
        })
        .collect()
}
