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

use gt_core::{ParamId, PluginInstanceId, PluginKind, PluginRef, Project};
use gt_engine::{EngineCommand, EngineHandle, PluginBox, MAX_PLUGINS};

use crate::backend::{Entry, Instance};
use crate::catalog::{self, Catalog, PluginInfo};
use crate::guard::{CrashReport, Guard};
use crate::host::{Signals, Waker};
use crate::presets::{self, Preset, Source};
use crate::processor::ParamOut;

/// Saved states kept for plugins taken out of the project, so undo brings them back as they
/// were.
const STATE_CACHE: usize = 64;
/// Parameter changes sent to the engine per frame at most (the rest follow next frame).
const PARAMS_PER_FRAME: usize = 512;
/// Parameter values written as text per frame at most (the rest follow next frame).
const TEXTS_PER_FRAME: usize = 256;

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

/// A running instance.
pub(crate) struct Live {
    plugin: Box<dyn Instance>,
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
    /// Parameter values as the plugin writes them, with the value each was written for.
    texts: Vec<(f32, Option<String>)>,
    /// Its presets: the plugin's own, then the user's (None until first asked for).
    presets: Option<Vec<Preset>>,
    /// Deactivation is needed before the next activation (restart or new engine).
    restart: bool,
}

/// What a sync changed in the document.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    /// Parameter values or descriptions in the document were updated from plugins (not an
    /// undoable edit: the app should refresh what depends on them).
    pub mirrored: bool,
    /// A plugin said its state changed (from its editor): the project has unsaved changes.
    pub dirty: bool,
    /// Parameters a plugin moved itself (its editor), for automation recording (D111).
    pub moved: Vec<ParamId>,
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
    entries: HashMap<PathBuf, Entry>,
    pub(crate) live: BTreeMap<PluginInstanceId, Live>,
    unavailable: BTreeMap<PluginInstanceId, String>,
    pub(crate) retiring: Vec<Box<dyn Instance>>,
    exporting: Vec<Box<dyn Instance>>,
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
    pub(crate) fn insert_entry(&mut self, path: &Path, entry: Entry) {
        self.entries.insert(path.to_owned(), entry);
    }

    fn entry(&mut self, info: &PluginInfo) -> Result<(), String> {
        let path = &info.path;
        if let Some(why) = self.guard.quarantined(path) {
            return Err(format!("{} is quarantined ({why})", path.display()));
        }
        if !self.entries.contains_key(path) {
            // The in-flight marker quarantines the file if its initialization crashes the app.
            let loaded = self.guard.run(path, &info.name, "loading", || {
                Entry::load(path, info.format)
            });
            let entry = loaded.map_err(|e| format!("cannot load {}: {e}", path.display()))?;
            self.entries.insert(path.to_owned(), entry);
        }
        Ok(())
    }

    fn create(&mut self, info: &PluginInfo) -> Result<Box<dyn Instance>, String> {
        let signals = Signals::new(Arc::clone(&self.waker));
        self.entry(info)?;
        let entry = &self.entries[&info.path];
        self.guard.run(&info.path, &info.name, "creating", || {
            entry.create(info, signals)
        })
    }

    /// The catalog entry for a plugin in the project, or one made from its saved path.
    fn info_for(&self, p: &PluginRef) -> PluginInfo {
        self.catalog
            .find(&p.id, &p.path)
            .cloned()
            .unwrap_or_else(|| PluginInfo {
                format: p.format,
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

    fn make_live(&mut self, id: PluginInstanceId, plugin: Box<dyn Instance>) -> Result<(), String> {
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
                texts: Vec::new(),
                presets: None,
                restart: false,
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
        p.format = info.format;
        p.params = Arc::from(params);
        p.values = values;
        mirror_outputs(&mut *plugin, &mut p);
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
        mirror_outputs(&mut *plugin, p);
        p.sanitize();
        self.make_live(p.instance, plugin)
    }

    fn retire(&mut self, id: PluginInstanceId, engine: Option<&mut EngineHandle>) {
        let Some(mut live) = self.live.remove(&id) else {
            return;
        };
        live.plugin.close_editor();
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
        self.refresh_texts(project);
        self.run_main_thread_work();
        self.poll_editors();

        self.retiring.retain_mut(|p| !p.try_deactivate());
        self.guard.set_in_use(
            self.live
                .values()
                .map(|l| l.plugin.info().path.clone())
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
            let s = Arc::clone(l.plugin.signals());
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
                l.texts.clear();
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
                let info = l.plugin.info();
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
                            processor,
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
                    l.plugin.host_changed(i, v);
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
            let Some(owner) = project.plugin_by_instance(id).map(|(o, _)| o) else {
                continue;
            };
            let Some(p) = project.plugin_by_instance_mut(id) else {
                continue;
            };
            // Changes the processor made (CLAP's output events, VST3's output parameter
            // changes) are already in the processor; editor edits (VST3) are not and go to
            // the engine with the next frame.
            let reported = std::iter::from_fn(|| rx.pop().ok())
                .map(|ParamOut { index, value }| (index as usize, value, true));
            let edits: Vec<(usize, f32, bool)> = l
                .plugin
                .take_edits()
                .into_iter()
                .map(|(i, v)| (i, v, false))
                .collect();
            let changes: Vec<(usize, f32, bool)> = reported.chain(edits).collect();
            for (i, value, in_processor) in changes {
                if let Some(v) = p.values.get_mut(i) {
                    let snapped = p.params[i].snap(value);
                    *v = snapped;
                    if in_processor {
                        if let Some(s) = l.sent.get_mut(i) {
                            *s = snapped;
                        }
                        l.plugin.plugin_reported(i, snapped);
                    }
                    out.mirrored = true;
                    let param = ParamId::plugin_param(owner, p, p.params[i].id);
                    if !out.moved.contains(&param) {
                        out.moved.push(param);
                    }
                }
            }
        }
    }

    /// Asks plugins how they write the values that changed.
    fn refresh_texts(&mut self, project: &Project) {
        let mut budget = TEXTS_PER_FRAME;
        for (&id, l) in self.live.iter_mut() {
            let Some((_, p)) = project.plugin_by_instance(id) else {
                continue;
            };
            l.texts.resize(p.values.len(), (f32::NAN, None));
            for (i, (&v, t)) in p.values.iter().zip(l.texts.iter_mut()).enumerate() {
                if budget == 0 {
                    return;
                }
                if t.0 != v {
                    *t = (v, l.plugin.value_text(i, v));
                    budget -= 1;
                }
            }
        }
    }

    /// Parameter values of plugin `id` as the plugin writes them ("-6.0 dB"), in the order of
    /// its parameters; None where the plugin does not say.
    pub fn param_texts(&self, id: PluginInstanceId) -> Vec<Option<String>> {
        self.live
            .get(&id)
            .map(|l| l.texts.iter().map(|(_, t)| t.clone()).collect())
            .unwrap_or_default()
    }

    // ---- Presets ----

    /// The presets of plugin `id`: the plugin's own, then the user's. Empty until
    /// [`Self::list_presets`] read them.
    pub fn presets(&self, id: PluginInstanceId) -> &[Preset] {
        self.listed_presets(id).unwrap_or_default()
    }

    /// The presets of plugin `id`, or None if they have not been read yet.
    pub fn listed_presets(&self, id: PluginInstanceId) -> Option<&[Preset]> {
        self.live.get(&id).and_then(|l| l.presets.as_deref())
    }

    /// Reads the preset list of plugin `id` (again).
    pub fn list_presets(&mut self, id: PluginInstanceId) {
        let Some(l) = self.live.get_mut(&id) else {
            return;
        };
        let info = l.plugin.info().clone();
        let plugin = &mut l.plugin;
        let mut list = self
            .guard
            .run(&info.path, &info.name, "listing the presets of", || {
                Ok::<_, String>(plugin.factory_presets())
            })
            .unwrap_or_default();
        list.extend(presets::list_user(&presets::user_dir(&self.dir, &info)));
        l.presets = Some(list);
    }

    /// Loads preset `index` (of [`Self::presets`]) into plugin `id`, and updates its document
    /// entry (values and state).
    pub fn load_preset(
        &mut self,
        project: &mut Project,
        id: PluginInstanceId,
        index: usize,
    ) -> Result<(), String> {
        let l = self.live.get_mut(&id).ok_or("the plugin is not loaded")?;
        let preset = l
            .presets
            .as_ref()
            .and_then(|p| p.get(index))
            .cloned()
            .ok_or("no such preset")?;
        let info = l.plugin.info().clone();
        let plugin = &mut l.plugin;
        self.guard.run(
            &info.path,
            &info.name,
            "loading a preset into",
            || match &preset.source {
                Source::User(path) => plugin.load_state(&presets::read_user(path)?),
                other => plugin.load_factory(other),
            },
        )?;
        let (params, values) = l.plugin.read_params();
        let state = l.plugin.save_state();
        // Everything goes to the processor again (a VST3 program change among it).
        l.sent.clear();
        l.texts.clear();
        if let Some(p) = project.plugin_by_instance_mut(id) {
            p.params = Arc::from(params);
            p.values = values;
            if let Some(s) = state {
                p.state = Arc::from(s);
            }
            p.sanitize();
        }
        Ok(())
    }

    /// Saves plugin `id`'s current state as user preset `name` (replacing one of that name).
    pub fn save_preset(&mut self, id: PluginInstanceId, name: &str) -> Result<(), String> {
        let l = self.live.get_mut(&id).ok_or("the plugin is not loaded")?;
        let info = l.plugin.info().clone();
        let state = l
            .plugin
            .save_state()
            .ok_or_else(|| format!("{} cannot save its state", info.name))?;
        presets::save_user(&presets::user_dir(&self.dir, &info), name, &state)?;
        self.list_presets(id);
        Ok(())
    }

    fn run_main_thread_work(&mut self) {
        let now = Instant::now();
        for l in self.live.values_mut() {
            l.plugin.main_thread_work(now);
        }
    }

    /// How soon the host wants another frame (timers, file descriptors, open editors).
    pub fn next_frame(&self) -> Option<Duration> {
        let now = Instant::now();
        let mut next = None::<Duration>;
        for l in self.live.values() {
            if let Some(w) = l.plugin.next_frame(now) {
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
        self.live.get(&id).is_some_and(|l| l.plugin.has_editor())
    }

    /// True if its editor is open.
    pub fn editor_open(&self, id: PluginInstanceId) -> bool {
        self.live.get(&id).is_some_and(|l| l.plugin.editor_open())
    }

    /// Opens (or raises) the plugin's editor: embedded in a window of ours, or floating in
    /// the plugin's own window if it cannot embed.
    pub fn open_editor(&mut self, id: PluginInstanceId, title: &str) -> Result<(), String> {
        let Some(l) = self.live.get_mut(&id) else {
            return Err("the plugin is not loaded".to_owned());
        };
        let info = l.plugin.info();
        let (path, name) = (info.path.clone(), info.name.clone());
        let plugin = &mut l.plugin;
        self.guard.run(&path, &name, "opening the editor of", || {
            plugin.open_editor(title)
        })
    }

    /// Closes the plugin's editor.
    pub fn close_editor(&mut self, id: PluginInstanceId) {
        if let Some(l) = self.live.get_mut(&id) {
            l.plugin.close_editor();
        }
    }

    fn poll_editors(&mut self) {
        for l in self.live.values_mut() {
            l.plugin.poll_editor();
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
                Ok((mut processor, _)) => {
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
            l.plugin.close_editor();
        }
        self.guard.clean_exit();
    }
}

/// Copies the plugin's extra outputs and whether it sends MIDI into its document entry.
fn mirror_outputs(plugin: &mut dyn Instance, p: &mut PluginRef) {
    let (names, midi) = plugin.outputs();
    p.set_output_names(names);
    p.sends_midi = midi;
}
