//! The browser's data (Prompt 25, D132): the file index built on a worker thread, the rows of
//! each tab, favourites and tags, and what using a row does.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

use gt_core::{EffectKind, EffectSlot, SampleSource, SynthPatch, Tick};
use gt_engine::EngineCommand;
use gt_plugin_host::PluginInfo;
use gt_project::index::{Index, ItemKind, Query};
use gt_project::presets;
use gt_ui::views::{BrowserAction, BrowserRow, BrowserTab, RowItem};

use super::{GloomApp, MainView};
use crate::files::Then;
use crate::library::list_folder;
use gt_ui::tr;

/// Rows shown at most (the list says how many matched in all).
const MAX_ROWS: usize = 1000;

/// The index and its worker.
#[derive(Default)]
pub(super) struct BrowseState {
    index: Index,
    building: Option<mpsc::Receiver<Index>>,
    /// The plugin list the Plugins tab's rows point into.
    plugins: Vec<PluginInfo>,
    /// The stretched preview being rendered.
    preview_key: Option<u64>,
}

/// Lowercase words of a search.
fn words(q: &str) -> Vec<String> {
    q.split_whitespace().map(str::to_lowercase).collect()
}

fn matches(words: &[String], text: &str) -> bool {
    let t = text.to_lowercase();
    words.iter().all(|w| t.contains(w.as_str()))
}

impl GloomApp {
    /// The folders the index walks: the user's, or the Music folder until they add one.
    fn browse_roots(&self) -> Vec<PathBuf> {
        if !self.settings.browser.roots.is_empty() {
            return self.settings.browser.roots.clone();
        }
        let music = crate::library::default_folder();
        if music.ends_with("Music") {
            vec![music]
        } else {
            Vec::new()
        }
    }

    /// Walks the root folders again on a worker thread.
    pub(super) fn start_index(&mut self) {
        let roots = self.browse_roots();
        self.browser.roots = roots.clone();
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("gt-browser-index".into())
            .spawn(move || {
                let _ = tx.send(Index::build(&roots));
            });
        match spawned {
            Ok(_) => {
                self.browse.building = Some(rx);
                self.browser.status = gt_ui::tr("Indexing folders…").to_owned();
            }
            Err(e) => self.browser.status = format!("Cannot index: {e}"),
        }
    }

    /// Picks up a finished index.
    pub(super) fn poll_index(&mut self) {
        let Some(rx) = &self.browse.building else {
            return;
        };
        let Ok(mut index) = rx.try_recv() else {
            return;
        };
        self.browse.building = None;
        for (key, tags) in &self.settings.browser.tags {
            index.set_tags(Path::new(key), tags);
        }
        let n = index.items.len();
        self.browser.status = format!(
            "{n} {}{} ({:.1} s)",
            gt_ui::tr("files indexed"),
            if index.truncated { "+" } else { "" },
            index.elapsed.as_secs_f32()
        );
        log::info!("browser index: {n} files in {:?}", index.elapsed);
        self.browse.index = index;
        self.refresh_browser();
    }

    /// True while a tempo-synced preview renders.
    pub(super) fn waiting_preview(&self) -> bool {
        self.browse.preview_key.is_some()
    }

    /// True while the index is being built.
    pub(super) fn indexing(&self) -> bool {
        self.browse.building.is_some()
    }

    /// Fills the browser's rows for its tab, search and favourites filter.
    pub(super) fn refresh_browser(&mut self) {
        let words = words(&self.browser.query);
        let favs: HashSet<&str> = self
            .settings
            .browser
            .favourites
            .iter()
            .map(String::as_str)
            .collect();
        let fav_only = self.browser.favourites_only;
        let tags = &self.settings.browser.tags;
        let row = |key: String, name: String, detail: String, item: RowItem| BrowserRow {
            favourite: favs.contains(key.as_str()),
            tags: tags.get(&key).cloned().unwrap_or_default(),
            key,
            name,
            detail,
            bpm: None,
            music_key: None,
            item,
        };
        let keep = |r: &BrowserRow| {
            (!fav_only || r.favourite)
                && matches(
                    &words,
                    &format!("{} {} #{}", r.name, r.detail, r.tags.join(" #")),
                )
        };
        let mut rows = Vec::new();
        let tab = self.browser.tab;
        // Built-in sounds and other fixed rows.
        match tab {
            BrowserTab::Samples => {
                for b in gt_core::BuiltInSample::ALL {
                    let src = SampleSource::BuiltIn(b);
                    rows.push(row(
                        format!("builtin:{}", b.name()),
                        b.name().to_owned(),
                        gt_ui::tr("Built-in").to_owned(),
                        RowItem::Sample(src),
                    ));
                }
            }
            BrowserTab::Instruments => {
                for (i, p) in SynthPatch::factory().iter().enumerate() {
                    let engine = gt_core::synth::ENGINES[usize::from(p.engine().min(2))];
                    rows.push(row(
                        format!("synth:{}", p.name),
                        p.name.clone(),
                        format!("{engine} · {}", gt_ui::tr("factory")),
                        RowItem::FactorySynth(i),
                    ));
                }
                for (name, path) in &self.user_presets {
                    rows.push(row(
                        path.display().to_string(),
                        name.clone(),
                        gt_ui::tr("Your presets").to_owned(),
                        RowItem::UserSynth(path.clone()),
                    ));
                }
            }
            BrowserTab::Effects => {
                for kind in EffectKind::ALL {
                    rows.push(row(
                        format!("fx:{}", kind.key()),
                        kind.name().to_owned(),
                        gt_ui::tr("Effect").to_owned(),
                        RowItem::Effect(kind.key().to_owned(), None),
                    ));
                    for (i, p) in kind.presets().iter().enumerate() {
                        rows.push(row(
                            format!("fx:{}:{}", kind.key(), p.name),
                            p.name.to_owned(),
                            kind.name().to_owned(),
                            RowItem::Effect(kind.key().to_owned(), Some(i)),
                        ));
                    }
                }
            }
            BrowserTab::Plugins => {
                self.browse.plugins = self
                    .plugins
                    .catalog()
                    .plugins()
                    .into_iter()
                    .cloned()
                    .collect();
                for (i, p) in self.browse.plugins.iter().enumerate() {
                    let kind = match p.kind() {
                        gt_core::PluginKind::Instrument => gt_ui::tr("Instrument"),
                        gt_core::PluginKind::Effect => gt_ui::tr("Effect"),
                    };
                    rows.push(row(
                        format!("plugin:{}:{}", p.path.display(), p.id),
                        p.name.clone(),
                        format!("{kind} · {} · {}", p.vendor, p.format.label()),
                        RowItem::Plugin(i),
                    ));
                }
            }
            BrowserTab::Projects => {}
        }
        rows.retain(keep);
        // Files from the index.
        let kind = match tab {
            BrowserTab::Samples => Some(ItemKind::Sample),
            BrowserTab::Instruments => Some(ItemKind::SynthPreset),
            BrowserTab::Projects => Some(ItemKind::Project),
            _ => None,
        };
        let browsing = tab == BrowserTab::Samples && words.is_empty() && !fav_only;
        let mut total = rows.len();
        if let (Some(kind), false) = (kind, browsing) {
            let q = Query {
                text: self.browser.query.clone(),
                kind: Some(kind),
            };
            let index = &self.browse.index;
            let limit = if fav_only { usize::MAX } else { MAX_ROWS };
            for i in index.search(&q, limit) {
                let it = &index.items[i];
                let key = it.path.display().to_string();
                if fav_only && !favs.contains(key.as_str()) {
                    continue;
                }
                total += 1;
                if rows.len() >= MAX_ROWS {
                    continue;
                }
                let item = match kind {
                    ItemKind::Sample => RowItem::Sample(SampleSource::File(it.path.clone())),
                    ItemKind::SynthPreset => RowItem::UserSynth(it.path.clone()),
                    ItemKind::Project => RowItem::Project(it.path.clone()),
                };
                rows.push(BrowserRow {
                    favourite: favs.contains(key.as_str()),
                    tags: it.tags.clone(),
                    key,
                    name: it.name.clone(),
                    detail: it.folder.clone(),
                    bpm: it.bpm,
                    music_key: it.key.clone(),
                    item,
                });
            }
            if !fav_only && total >= MAX_ROWS {
                // The search stopped early; the count is a lower bound.
                total = total.max(MAX_ROWS + 1);
            }
        }
        rows.truncate(MAX_ROWS);
        self.browser.total = total.max(rows.len());
        self.browser.rows = rows;
    }

    pub(super) fn open_folder(&mut self, dir: PathBuf) {
        match list_folder(&dir) {
            Ok(entries) => {
                self.browser.entries = entries;
                self.browser.error = None;
                self.browser.path_text = dir.display().to_string();
                self.browser.folder = dir;
            }
            Err(e) => self.browser.error = Some(e),
        }
    }

    pub(super) fn on_browser(&mut self, action: BrowserAction) {
        match action {
            BrowserAction::Preview(src, bpm) => {
                self.preview_bpm = bpm.filter(|_| self.browser.sync_preview);
                self.preview(&src);
            }
            BrowserAction::Assign(src) => self.assign_sample(src),
            BrowserAction::Open(dir) => self.open_folder(dir),
            BrowserAction::Refresh => self.refresh_browser(),
            BrowserAction::Activate(item) => self.activate(item),
            BrowserAction::ToggleFavourite(key) => {
                let favs = &mut self.settings.browser.favourites;
                if let Some(i) = favs.iter().position(|k| *k == key) {
                    favs.remove(i);
                } else {
                    favs.push(key);
                }
                self.settings_dirty = true;
                self.refresh_browser();
            }
            BrowserAction::SetTags(key, tags) => {
                self.browse.index.set_tags(Path::new(&key), &tags);
                if tags.is_empty() {
                    self.settings.browser.tags.remove(&key);
                } else {
                    self.settings.browser.tags.insert(key, tags);
                }
                self.settings_dirty = true;
                self.refresh_browser();
            }
            BrowserAction::AddRoot(dir) => {
                if !dir.is_dir() {
                    self.toast(format!("Not a folder: {}", dir.display()));
                    return;
                }
                let mut roots = self.browse_roots();
                if !roots.contains(&dir) {
                    roots.push(dir);
                }
                self.settings.browser.roots = roots;
                self.settings_dirty = true;
                self.start_index();
            }
            BrowserAction::RemoveRoot(i) => {
                let mut roots = self.browse_roots();
                if i < roots.len() {
                    roots.remove(i);
                    self.settings.browser.roots = roots;
                    self.settings_dirty = true;
                    self.start_index();
                }
            }
            BrowserAction::Rescan => self.start_index(),
        }
    }

    fn assign_sample(&mut self, src: SampleSource) {
        let i = self.rack.selected;
        let Some(ch) = self.project.channels.get_mut(i) else {
            return;
        };
        let fresh = ch.sample().is_none() && ch.name.starts_with("Sampler ");
        let Some(sampler) = ch.sampler_mut() else {
            return; // a synth channel has no sample
        };
        sampler.sample = Some(src.clone());
        // Name a fresh "Sampler N" channel after its first sample.
        if fresh {
            if let SampleSource::File(p) = &src {
                if let Some(stem) = p.file_stem() {
                    ch.name = stem.to_string_lossy().into_owned();
                }
            }
        }
        self.push_sample(i);
        self.pending_edit = Some("Load sample");
    }

    /// Uses a preset, effect, plugin or project row.
    fn activate(&mut self, item: RowItem) {
        match item {
            RowItem::FactorySynth(i) => {
                if let Some(p) = SynthPatch::factory().into_iter().nth(i) {
                    self.load_synth(p);
                }
            }
            RowItem::UserSynth(path) => match presets::load(&path) {
                Ok(p) => self.load_synth(p),
                Err(e) => self.toast(format!("Cannot load {}: {e}", path.display())),
            },
            RowItem::Effect(key, preset) => {
                let Some(kind) = EffectKind::from_key(&key) else {
                    return;
                };
                let mut slot = EffectSlot::new(kind);
                if let Some(p) = preset.and_then(|i| kind.presets().get(i)) {
                    slot.apply_preset(p);
                }
                let Some((strip, k)) = self.effect_target() else {
                    self.toast(tr("This mixer track has no free effect slot"));
                    return;
                };
                self.project.mixer.strips[strip].slots[k] = Some(slot);
                self.mixer_state.selected = strip;
                self.mixer_state.slot = Some(k);
                self.show_view(MainView::Mixer);
                self.pending_edit = Some("Add effect");
            }
            RowItem::Plugin(i) => {
                if let Some(info) = self.browse.plugins.get(i).cloned() {
                    let target = self.effect_target();
                    if info.kind() == gt_core::PluginKind::Effect && target.is_none() {
                        self.toast(tr("This mixer track has no free effect slot"));
                        return;
                    }
                    self.add_plugin(&info, target);
                }
            }
            RowItem::Project(path) => {
                self.pending_open = Some(path);
                self.request(Then::OpenPending);
            }
            RowItem::Sample(src) => self.assign_sample(src),
            RowItem::Folder(dir) => self.open_folder(dir),
        }
    }

    /// Loads `patch` into the selected synth channel, or a new synth channel.
    fn load_synth(&mut self, patch: SynthPatch) {
        let i = self.rack.selected;
        if let Some(p) = self.project.channels.get_mut(i).and_then(|c| c.synth_mut()) {
            *p = patch;
            self.push_all_params();
            self.pending_edit = Some("Load preset");
            return;
        }
        let name = patch.name.clone();
        if self.project.add_synth_channel(&name, patch).is_none() {
            self.toast(tr("The channel rack is full"));
            return;
        }
        self.rack.selected = self.project.channels.len() - 1;
        self.push_channels();
        self.pending_edit = Some("Add synth");
    }

    /// Plays `data` through the preview voice, stretched to the project tempo when the sound
    /// has a tempo (the render comes from the stretch worker; the plain sound plays until it
    /// is ready only if the tempos already match).
    pub(super) fn preview_data(&mut self, src: &SampleSource, data: Arc<gt_core::SampleData>) {
        let song_bpm = self.project.tempo.bpm_at(Tick(0));
        let Some(bpm) = self.preview_bpm.map(f64::from) else {
            self.browse.preview_key = None;
            self.send(EngineCommand::PreviewSample(Some(data)));
            return;
        };
        let speed = song_bpm / bpm;
        if (speed - 1.0).abs() < 0.005 || data.frames() == 0 {
            self.browse.preview_key = None;
            self.send(EngineCommand::PreviewSample(Some(data)));
            return;
        }
        let job = preview_job(src, &data, speed);
        self.browse.preview_key = Some(job.key);
        if let Some(r) = self.stretch.preview(job, data) {
            self.browse.preview_key = None;
            self.send(EngineCommand::PreviewSample(Some(r)));
        }
    }

    /// Plays the stretched preview once its render arrives.
    pub(super) fn poll_preview(&mut self) {
        let Some(key) = self.browse.preview_key else {
            return;
        };
        if let Some(r) = self.stretch.get(key) {
            self.browse.preview_key = None;
            self.send(EngineCommand::PreviewSample(Some(r)));
        }
    }

    /// The browser tab and filter for the settings file.
    pub(super) fn collect_browser_prefs(&mut self) {
        let b = &mut self.settings.browser;
        b.tab = self.browser.tab.key().to_owned();
        b.sync_preview = self.browser.sync_preview;
    }

    /// Sets up the browser from the settings at start.
    pub(super) fn init_browser(&mut self) {
        let b = &self.settings.browser;
        self.browser.tab = BrowserTab::from_key(&b.tab).unwrap_or_default();
        self.browser.sync_preview = b.sync_preview;
        let start = self
            .browse_roots()
            .into_iter()
            .next()
            .unwrap_or_else(crate::library::default_folder);
        self.open_folder(start);
        self.start_index();
        self.refresh_browser();
    }

    /// Whether `v` is in a window of its own: focus it; else show it in the main window.
    pub(super) fn show_view(&mut self, v: MainView) {
        if self.layout.is_floating(v) {
            self.focus_view = Some(v);
        } else {
            self.layout.view = v;
        }
    }
}

/// The stretch job of a tempo-synced preview: `speed` times faster, at the same pitch.
fn preview_job(
    src: &SampleSource,
    data: &gt_core::SampleData,
    speed: f64,
) -> gt_engine::stretch::StretchJob {
    let frames = ((data.frames() as f64 / speed).ceil() as usize).max(1);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "preview".hash(&mut h);
    src.hash(&mut h);
    data.sample_rate.hash(&mut h);
    data.frames().hash(&mut h);
    frames.hash(&mut h);
    gt_engine::stretch::StretchJob {
        key: h.finish(),
        source: src.clone(),
        map: gt_dsp::stretch::TimeMap::linear(speed, frames),
        semitones: 0.0,
        frames,
    }
}
