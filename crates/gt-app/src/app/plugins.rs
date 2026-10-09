//! CLAP plugins: the app side (the plugin browser, adding plugins, their panels' requests and
//! the per-frame sync with the plugin host).

use std::path::PathBuf;

use gt_core::{EffectSlot, PluginKind, PluginOwner, FX_SLOTS, STRIPS};
use gt_plugin_host::PluginInfo;
use gt_ui::views::{
    plugin_browser, PluginAction, PluginBrowserAction, PluginBrowserView, PluginEntry,
};
use gt_ui::GloomTheme;

use super::{GloomApp, MainView};

impl GloomApp {
    /// Once per frame, after the views: keeps the running plugins in step with the document
    /// and the engine, and runs their main-thread work.
    pub(super) fn sync_plugins(&mut self, ctx: &egui::Context) {
        let fresh = std::mem::take(&mut self.fresh_engine);
        let out = self
            .plugins
            .sync(&mut self.project, self.audio.engine_mut(), fresh);
        if out.dirty {
            // Changed in the plugin's own editor: unsaved, but not an undo step.
            self.edits += 1;
        }
        if let Some(msg) = self.plugins.take_notices().pop() {
            self.toast(msg);
        }
        if let Some(next) = self.plugins.next_frame() {
            ctx.request_repaint_after(next);
        }
    }

    /// Where the browser adds an effect: the slot chosen with "Plugin…", else the open slot
    /// of the mixer's selected strip, else its first empty slot.
    fn effect_target(&self) -> Option<(usize, usize)> {
        if self.plugin_slot.is_some() {
            return self.plugin_slot;
        }
        let strip = self.mixer_state.selected.min(STRIPS - 1);
        let slots = &self.project.mixer.strips[strip].slots;
        self.mixer_state
            .slot
            .or_else(|| (0..FX_SLOTS).find(|&k| slots[k].is_none()))
            .map(|k| (strip, k))
    }

    fn slot_name(&self, (strip, slot): (usize, usize)) -> String {
        format!(
            "{}, slot {}",
            self.project.mixer.strips[strip].name,
            slot + 1
        )
    }

    pub(super) fn on_plugin_action(&mut self, action: PluginAction) {
        match action {
            PluginAction::OpenEditor(id) => {
                let Some((owner, p)) = self.project.plugin_by_instance(id) else {
                    return;
                };
                let place = match owner {
                    PluginOwner::Channel(c) => self
                        .project
                        .channel_index(c)
                        .map_or_else(String::new, |i| self.project.channels[i].name.clone()),
                    PluginOwner::Effect { strip, slot } => self.slot_name((strip, slot)),
                };
                let title = format!("{} · {place}", p.name);
                if let Err(e) = self.plugins.open_editor(id, &title) {
                    self.toast(format!("Cannot open the editor: {e}"));
                }
            }
            PluginAction::CloseEditor(id) => self.plugins.close_editor(id),
            PluginAction::Retry(id) => self.plugins.retry(id, self.audio.engine_mut()),
        }
    }

    /// The plugin browser window.
    pub(super) fn plugin_window(&mut self, ctx: &egui::Context, theme: &GloomTheme) {
        let infos: Vec<PluginInfo> = self
            .plugins
            .catalog()
            .plugins()
            .into_iter()
            .cloned()
            .collect();
        let entries: Vec<PluginEntry> = infos
            .iter()
            .map(|p| PluginEntry {
                name: p.name.clone(),
                vendor: p.vendor.clone(),
                kind: p.kind(),
                details: [
                    p.description.as_str(),
                    &format!("{} {}", p.id, p.version),
                    &p.path.display().to_string(),
                ]
                .iter()
                .filter(|s| !s.trim().is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("\n"),
            })
            .collect();
        let quarantine = self.plugins.quarantine();
        let quarantine_rows: Vec<(String, String)> = quarantine
            .iter()
            .map(|(p, r)| (p.display().to_string(), r.clone()))
            .collect();
        let failed: Vec<(String, String)> = self
            .plugins
            .catalog()
            .files
            .iter()
            .filter_map(|f| Some((f.path.display().to_string(), f.error.clone()?)))
            .collect();
        let standard: Vec<String> = gt_plugin_host::catalog::standard_folders()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let folders: Vec<String> = self
            .plugins
            .folders()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let target = self.effect_target();
        let target_name = target.map(|t| self.slot_name(t));
        let scanning = self.plugins.scanning().cloned();
        let view = PluginBrowserView {
            plugins: &entries,
            scanning: scanning.as_ref(),
            standard_folders: &standard,
            folders: &folders,
            quarantine: &quarantine_rows,
            failed: &failed,
            effect_target: target_name.as_deref(),
        };
        let mut open = true;
        let actions = egui::Window::new("Plugins")
            .open(&mut open)
            .default_width(560.0)
            .default_pos(ctx.content_rect().center() - egui::vec2(280.0, 160.0))
            .collapsible(false)
            .show(ctx, |ui| {
                plugin_browser(ui, theme, &mut self.plugin_browser, view)
            })
            .and_then(|r| r.inner)
            .unwrap_or_default();
        if !open {
            self.show_plugins = false;
            self.plugin_slot = None;
        }
        for a in actions {
            match a {
                PluginBrowserAction::Add(i) => {
                    if let Some(info) = infos.get(i) {
                        self.add_plugin(info, target);
                    }
                }
                PluginBrowserAction::Rescan => self.plugins.start_scan(),
                PluginBrowserAction::AddFolder(f) => {
                    let mut all = self.plugins.folders().to_vec();
                    all.push(PathBuf::from(f));
                    self.plugins.set_folders(all);
                    self.plugins.start_scan();
                }
                PluginBrowserAction::RemoveFolder(i) => {
                    let mut all = self.plugins.folders().to_vec();
                    if i < all.len() {
                        all.remove(i);
                        self.plugins.set_folders(all);
                    }
                }
                PluginBrowserAction::Release(i) => {
                    if let Some((path, _)) = quarantine.get(i) {
                        self.plugins.release(path);
                    }
                }
            }
        }
    }

    /// Loads `info` and adds it: an instrument as a new channel, an effect to `target`.
    fn add_plugin(&mut self, info: &PluginInfo, target: Option<(usize, usize)>) {
        let kind = info.kind();
        if kind == PluginKind::Effect && target.is_none() {
            return;
        }
        let plugin = match self.plugins.instantiate(info) {
            Ok(p) => p,
            Err(e) => {
                self.toast(e);
                return;
            }
        };
        match (kind, target) {
            (PluginKind::Instrument, _) => {
                if self
                    .project
                    .add_plugin_channel(&info.name, plugin)
                    .is_none()
                {
                    self.toast("The channel rack is full");
                    return;
                }
                self.rack.selected = self.project.channels.len() - 1;
                if self.main_view == MainView::Mixer {
                    self.main_view = MainView::Rack;
                }
                self.push_channels();
                self.pending_edit = Some("Add plugin");
            }
            (PluginKind::Effect, Some((strip, slot))) => {
                self.project.mixer.strips[strip].slots[slot] = Some(EffectSlot::plugin(plugin));
                self.mixer_state.selected = strip;
                self.mixer_state.slot = Some(slot);
                self.main_view = MainView::Mixer;
                self.pending_edit = Some("Add plugin effect");
            }
            (PluginKind::Effect, None) => return,
        }
        self.toast(format!("Added {}", info.name));
        self.show_plugins = false;
        self.plugin_slot = None;
    }
}
