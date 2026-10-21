//! Drag and drop: browser rows dropped on the rack, the mixer or anywhere else, and files
//! dropped on the window from the file manager, each sent where the pointer is.

use std::path::PathBuf;

use gt_core::{DrumKit, EffectKind, EffectSlot, PluginKind, SampleSource, SynthPatch, FX_SLOTS};
use gt_project::presets;
use gt_ui::tr;
use gt_ui::views::{MixerDrop, RowItem};

use super::{GloomApp, MainView};
use crate::files::Then;

/// What a drop needs to know of the channel it landed on.
#[derive(Clone, Copy)]
struct Target {
    sampler: bool,
    drums: bool,
    synth: bool,
    insert: usize,
}

impl Target {
    fn of(c: &gt_core::Channel) -> Self {
        Self {
            sampler: c.sampler().is_some(),
            drums: c.drums().is_some(),
            synth: c.synth().is_some(),
            insert: c.insert,
        }
    }
}

impl GloomApp {
    /// A browser row dropped on channel `channel` (replace its sound or patch, or add the
    /// effect to its mixer track), or on the rack below the channels (`None`: a new channel).
    pub(super) fn drop_on_rack(&mut self, channel: Option<usize>, item: RowItem) {
        let ch = channel.and_then(|i| self.project.channels.get(i).map(|c| (i, Target::of(c))));
        match item {
            RowItem::Sample(src) => match ch {
                Some((i, c)) if c.sampler => {
                    self.rack.selected = i;
                    self.assign_sample(src);
                }
                Some((i, c)) if c.drums => {
                    if let SampleSource::File(path) = src {
                        self.rack.selected = i;
                        self.fill_pads(&[path]);
                    } else {
                        self.new_sampler(src);
                    }
                }
                _ => self.new_sampler(src),
            },
            RowItem::FactorySynth(_) | RowItem::UserSynth(_) => {
                let Some(patch) = self.patch_of(&item) else {
                    return;
                };
                match ch {
                    Some((i, c)) if c.synth => {
                        self.rack.selected = i;
                        self.load_synth(patch);
                    }
                    _ => self.new_synth(patch),
                }
            }
            RowItem::Kit(k) => {
                let Some((name, kit)) = DrumKit::factory_kits().into_iter().nth(k) else {
                    return;
                };
                match ch {
                    Some((i, c)) if c.drums => {
                        if let Some(d) = self.project.channels[i].drums_mut() {
                            *d = kit;
                        }
                        self.rack.selected = i;
                        self.push_sample(i);
                        self.pending_edit = Some("Load kit");
                    }
                    _ => {
                        if self.project.add_drum_synth_channel(name).is_none() {
                            self.toast(tr("The channel rack is full"));
                            return;
                        }
                        let i = self.project.channels.len() - 1;
                        if let Some(d) = self.project.channels[i].drums_mut() {
                            *d = kit;
                        }
                        self.rack.selected = i;
                        self.push_channels();
                        self.pending_edit = Some("Add drum kit");
                    }
                }
            }
            RowItem::Effect(key, preset) => match ch {
                Some((_, c)) => {
                    let strip = c.insert;
                    self.add_effect(strip, None, &key, preset);
                }
                None => self.toast(tr("Drop effects on a channel or a mixer track")),
            },
            RowItem::Plugin(p) => {
                let Some(info) = self.browse_plugin(p) else {
                    return;
                };
                self.patch_plugin_target = None;
                if info.kind() == PluginKind::Instrument {
                    self.plugin_slot = None;
                    self.add_plugin(&info, None);
                } else if let Some((_, c)) = ch {
                    let strip = c.insert;
                    self.add_plugin_effect(&info, strip, None);
                } else {
                    self.toast(tr("Drop effects on a channel or a mixer track"));
                }
            }
            RowItem::Project(path) => self.open_dropped_project(path),
            RowItem::Folder(dir) => self.open_folder(dir),
        }
    }

    /// A browser effect or plugin dropped on a mixer strip or one of its slots.
    pub(super) fn drop_on_mixer(&mut self, d: MixerDrop) {
        match d.item {
            RowItem::Effect(key, preset) => {
                self.add_effect(d.strip, d.slot, &key, preset);
            }
            RowItem::Plugin(p) => {
                let Some(info) = self.browse_plugin(p) else {
                    return;
                };
                if info.kind() == PluginKind::Effect {
                    self.patch_plugin_target = None;
                    self.add_plugin_effect(&info, d.strip, d.slot);
                } else {
                    self.toast(tr("Instrument plugins go on the channel rack"));
                }
            }
            _ => {}
        }
    }

    /// A browser row released where no view took it: used as a double-click would, except
    /// over the browser itself.
    pub(super) fn drop_anywhere(&mut self, item: RowItem, over_browser: bool) {
        if over_browser || matches!(item, RowItem::Folder(_)) {
            return;
        }
        self.on_browser(gt_ui::views::BrowserAction::Activate(item));
    }

    /// Effect `key` (with preset `preset`) into slot `slot` of strip `strip`, or its first
    /// free slot. False (with a notice) when there is none.
    pub(super) fn add_effect(
        &mut self,
        strip: usize,
        slot: Option<usize>,
        key: &str,
        preset: Option<usize>,
    ) -> bool {
        let Some(kind) = EffectKind::from_key(key) else {
            return false;
        };
        let strip = strip.min(self.project.mixer.inserts());
        let slots = &self.project.mixer.strips[strip].slots;
        let Some(k) = slot
            .filter(|&k| k < FX_SLOTS)
            .or_else(|| (0..FX_SLOTS).find(|&k| slots[k].is_none()))
        else {
            self.toast(tr("This mixer track has no free effect slot"));
            return false;
        };
        let mut fx = EffectSlot::new(kind);
        if let Some(p) = preset.and_then(|i| kind.presets().get(i)) {
            fx.apply_preset(p);
        }
        self.project.mixer.strips[strip].slots[k] = Some(fx);
        self.mixer_state.selected = strip;
        self.mixer_state.slot = Some(k);
        self.pending_edit = Some("Add effect");
        true
    }

    /// An effect plugin into slot `slot` of strip `strip`, or its first free slot.
    fn add_plugin_effect(
        &mut self,
        info: &gt_plugin_host::PluginInfo,
        strip: usize,
        slot: Option<usize>,
    ) {
        let strip = strip.min(self.project.mixer.inserts());
        let slots = &self.project.mixer.strips[strip].slots;
        let Some(k) = slot.or_else(|| (0..FX_SLOTS).find(|&k| slots[k].is_none())) else {
            self.toast(tr("This mixer track has no free effect slot"));
            return;
        };
        self.mixer_state.selected = strip;
        self.mixer_state.slot = Some(k);
        self.add_plugin(info, Some((strip, k)));
    }

    /// The synth patch of a preset row.
    pub(super) fn patch_of(&mut self, item: &RowItem) -> Option<SynthPatch> {
        match item {
            RowItem::FactorySynth(i) => SynthPatch::factory().into_iter().nth(*i),
            RowItem::UserSynth(path) => match presets::load(path) {
                Ok(p) => Some(p),
                Err(e) => {
                    self.toast(format!("Cannot load {}: {e}", path.display()));
                    None
                }
            },
            _ => None,
        }
    }

    /// A new sampler channel playing `src`, named after it.
    fn new_sampler(&mut self, src: SampleSource) {
        let name = match &src {
            SampleSource::File(p) => p.file_stem().map_or_else(
                || "Sampler".to_owned(),
                |s| s.to_string_lossy().into_owned(),
            ),
            SampleSource::BuiltIn(b) => b.name().to_owned(),
        };
        if self.project.add_channel(&name, Some(src)).is_none() {
            self.toast(tr("The channel rack is full"));
            return;
        }
        self.rack.selected = self.project.channels.len() - 1;
        self.push_channels();
        self.pending_edit = Some("Add sampler");
    }

    /// A new synth channel playing `patch`.
    pub(super) fn new_synth(&mut self, patch: SynthPatch) {
        let name = patch.name.clone();
        if self.project.add_synth_channel(&name, patch).is_none() {
            self.toast(tr("The channel rack is full"));
            return;
        }
        self.rack.selected = self.project.channels.len() - 1;
        self.push_channels();
        self.pending_edit = Some("Add synth");
    }

    fn open_dropped_project(&mut self, path: PathBuf) {
        self.pending_open = Some(path);
        self.request(Then::OpenPending);
    }

    /// Files dropped on the window from the file manager. A project opens; synth presets
    /// become synth channels; sounds (and folders of them) go where the pointer is: audio
    /// clips over the playlist, new sampler channels over the rack, otherwise the selected
    /// sampler (several as key zones) or drum pads, as before.
    pub(super) fn on_dropped_files(&mut self, paths: &[PathBuf]) {
        let ext = |p: &PathBuf| {
            p.extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default()
        };
        if let Some(project) = paths.iter().find(|p| ext(p) == gt_project::file::EXTENSION) {
            self.open_dropped_project(project.clone());
            return;
        }
        let mut files = Vec::new();
        for p in paths {
            if ext(p) == presets::PRESET_EXTENSION {
                self.drop_on_rack(None, RowItem::UserSynth(p.clone()));
            } else if p.is_dir() {
                files.extend(gt_project::list_audio_files(p));
            } else if gt_project::is_audio_file(p) {
                files.push(p.clone());
            }
        }
        if files.is_empty() {
            return;
        }
        let at = self.drop_view();
        match at {
            Some(MainView::Playlist) => {
                self.playlist
                    .dropped_files
                    .extend(files.into_iter().map(SampleSource::File));
            }
            Some(MainView::Rack) => {
                for f in files {
                    self.new_sampler(SampleSource::File(f));
                }
            }
            _ => {
                if self
                    .project
                    .channels
                    .get(self.rack.selected)
                    .is_some_and(|c| c.drums().is_some())
                {
                    self.fill_pads(&files);
                } else if self
                    .project
                    .channels
                    .get(self.rack.selected)
                    .is_some_and(|c| c.sampler().is_some())
                {
                    self.load_zones(&files);
                } else {
                    for f in files {
                        self.new_sampler(SampleSource::File(f));
                    }
                }
            }
        }
    }

    /// The docked view under the pointer (from last frame's layout), if any.
    fn drop_view(&self) -> Option<MainView> {
        let pos = self.last_pointer?;
        self.view_rects
            .iter()
            .find(|(_, r)| r.contains(pos))
            .map(|(v, _)| *v)
    }
}
