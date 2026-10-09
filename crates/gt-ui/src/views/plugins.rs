//! Third-party plugins in the UI: the plugin browser, and the controls of a loaded plugin
//! (status, its editor, generic parameter sliders) shown for plugin channels and plugin
//! effects. The app owns the plugin host; these views only show what it reports and return
//! what the user asked for.

use std::sync::Arc;

use egui::{vec2, RichText, Ui};
use gt_core::{ParamId, PluginInstanceId, PluginKind, PluginOwner, PluginRef};

use crate::param_ui::param_menu;
use crate::GloomTheme;

/// How a plugin in the project is doing, as the plugin host reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    /// Loaded and running.
    Running,
    /// Loaded, waiting for the audio engine (no device open).
    Waiting,
    /// Bypassed after an error or invalid output.
    Failed,
    /// Not loaded, with the reason. Its saved settings stay in the project.
    Unavailable(String),
}

/// What the plugin controls show besides the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPanelView {
    /// Status.
    pub state: PluginState,
    /// The plugin has an editor of its own.
    pub has_editor: bool,
    /// Its editor is open.
    pub editor_open: bool,
}

/// What the user asked of a loaded plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginAction {
    /// Open (or raise) the plugin's editor.
    OpenEditor(PluginInstanceId),
    /// Close it.
    CloseEditor(PluginInstanceId),
    /// Run a bypassed plugin again, or try loading an unavailable one again.
    Retry(PluginInstanceId),
}

/// Status line, editor button and the parameter sliders of `plugin`, which sits at `owner`.
/// Returns true if a parameter changed.
pub fn plugin_controls(
    ui: &mut Ui,
    theme: &GloomTheme,
    owner: PluginOwner,
    plugin: &mut PluginRef,
    view: &PluginPanelView,
    actions: &mut Vec<PluginAction>,
) -> bool {
    let id = plugin.instance;
    ui.horizontal(|ui| {
        let (text, color) = match &view.state {
            PluginState::Running => ("Running".to_owned(), theme.text_dim),
            PluginState::Waiting => ("Waiting for audio".to_owned(), theme.text_dim),
            PluginState::Failed => ("Bypassed".to_owned(), theme.warn),
            PluginState::Unavailable(_) => ("Not loaded".to_owned(), theme.warn),
        };
        ui.label(
            RichText::new(format!("CLAP · {}", plugin.vendor))
                .small()
                .color(theme.text_dim),
        );
        ui.label(RichText::new(text).small().color(color));
        if matches!(
            view.state,
            PluginState::Failed | PluginState::Unavailable(_)
        ) && ui
            .small_button("Retry")
            .on_hover_text("Run the plugin again")
            .clicked()
        {
            actions.push(PluginAction::Retry(id));
        }
        if view.has_editor {
            let label = if view.editor_open {
                "Close editor"
            } else {
                "Open editor"
            };
            if ui
                .add(egui::Button::selectable(view.editor_open, label))
                .on_hover_text("The plugin's own window")
                .clicked()
            {
                actions.push(if view.editor_open {
                    PluginAction::CloseEditor(id)
                } else {
                    PluginAction::OpenEditor(id)
                });
            }
        }
    });
    let why = match &view.state {
        PluginState::Failed => Some("It reported an error or produced invalid audio."),
        PluginState::Unavailable(why) => Some(why.as_str()),
        _ => None,
    };
    if let Some(why) = why {
        ui.label(RichText::new(why).small().color(theme.warn));
    }
    plugin_params(ui, theme, owner, plugin)
}

/// One slider per visible parameter, with a filter for plugins that have many. Returns true if
/// a value changed.
fn plugin_params(
    ui: &mut Ui,
    theme: &GloomTheme,
    owner: PluginOwner,
    plugin: &mut PluginRef,
) -> bool {
    let params = Arc::clone(&plugin.params);
    let visible = params.iter().filter(|p| !p.hidden).count();
    if visible == 0 {
        ui.label(
            RichText::new("This plugin has no parameters to show here.")
                .small()
                .color(theme.text_dim),
        );
        return false;
    }
    let filter_id = egui::Id::new(("gt_plugin_filter", plugin.instance));
    let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
    if visible > 12 {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Find").small().color(theme.text_dim));
            ui.add(egui::TextEdit::singleline(&mut filter).desired_width(160.0));
        });
        ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));
    }
    let needle = filter.to_lowercase();
    let mut changed = false;
    egui::ScrollArea::vertical()
        .id_salt(("gt_plugin_params", plugin.instance))
        .max_height(260.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            egui::Grid::new(("gt_plugin_grid", plugin.instance))
                .num_columns(2)
                .spacing(vec2(8.0, 2.0))
                .show(ui, |ui| {
                    ui.spacing_mut().slider_width = 170.0;
                    for (i, info) in params.iter().enumerate() {
                        if info.hidden || !info.name.to_lowercase().contains(&needle) {
                            continue;
                        }
                        fixed_label(ui, 130.0, RichText::new(&info.name).small());
                        let mut v = plugin.values.get(i).copied().unwrap_or(info.default);
                        let r = if info.steps > 0 && info.steps <= 128 {
                            let mut s = (v * info.steps as f32).round() as u32;
                            let r = ui.add_enabled(
                                info.automatable,
                                egui::Slider::new(&mut s, 0..=info.steps),
                            );
                            if r.changed() {
                                v = s as f32 / info.steps as f32;
                            }
                            r
                        } else {
                            ui.add_enabled(
                                info.automatable,
                                egui::Slider::new(&mut v, 0.0..=1.0)
                                    .custom_formatter(|x, _| format!("{:.1} %", x * 100.0)),
                            )
                        };
                        let r = if info.automatable {
                            r.on_hover_text(format!(
                                "{} (default {:.1} %). Right-click to automate or MIDI learn",
                                info.name,
                                info.default * 100.0
                            ))
                        } else {
                            r.on_disabled_hover_text(
                                "The plugin does not let the host change this; use its editor",
                            )
                        };
                        if info.automatable {
                            param_menu(theme, &r, ParamId::plugin_param(owner, plugin, info.id));
                        }
                        if r.changed() && plugin.set_value(info.id, v) {
                            changed = true;
                        }
                        ui.end_row();
                    }
                });
        });
    changed
}

/// A left-aligned label in a column of fixed width, cut with "…" when longer.
fn fixed_label(ui: &mut Ui, width: f32, text: RichText) -> egui::Response {
    ui.allocate_ui_with_layout(
        vec2(width, 16.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            ui.add(egui::Label::new(text).truncate())
        },
    )
    .inner
}

/// One plugin in the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEntry {
    /// Display name.
    pub name: String,
    /// Vendor.
    pub vendor: String,
    /// Instrument or effect.
    pub kind: PluginKind,
    /// The plugin's description and file, for the tooltip.
    pub details: String,
}

/// What the plugin browser shows.
#[derive(Debug, Clone, Copy)]
pub struct PluginBrowserView<'a> {
    /// Known plugins, sorted by name.
    pub plugins: &'a [PluginEntry],
    /// Scan progress: files done, files in total, the file being scanned.
    pub scanning: Option<&'a (usize, usize, String)>,
    /// Folders searched as standard, then the user's own.
    pub standard_folders: &'a [String],
    /// The user's own folders.
    pub folders: &'a [String],
    /// Quarantined plugin files, with the reason.
    pub quarantine: &'a [(String, String)],
    /// Plugin files that could not be scanned, with the reason.
    pub failed: &'a [(String, String)],
    /// Where an effect goes ("Insert 3, slot 2"), none if no mixer slot is chosen.
    pub effect_target: Option<&'a str>,
}

/// Browser state that is not part of the document.
#[derive(Debug, Clone, Default)]
pub struct PluginBrowserState {
    /// Search text.
    pub search: String,
    /// Kind filter.
    pub kind: Option<PluginKind>,
    /// Folder being typed.
    pub new_folder: String,
}

/// What the user asked of the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginBrowserAction {
    /// Add plugin `i` of the list (an instrument as a new channel, an effect to the target
    /// slot).
    Add(usize),
    /// Scan the folders again.
    Rescan,
    /// Search this folder too.
    AddFolder(String),
    /// Stop searching the user's folder `i`.
    RemoveFolder(usize),
    /// Let quarantined file `i` load again.
    Release(usize),
}

/// The plugin browser (shown in a window by the app).
pub fn plugin_browser(
    ui: &mut Ui,
    theme: &GloomTheme,
    state: &mut PluginBrowserState,
    view: PluginBrowserView<'_>,
) -> Vec<PluginBrowserAction> {
    let mut actions = Vec::new();
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.search)
                .hint_text("Search")
                .desired_width(170.0),
        );
        for (kind, label) in [
            (None, "All"),
            (Some(PluginKind::Instrument), "Instruments"),
            (Some(PluginKind::Effect), "Effects"),
        ] {
            if ui
                .add(egui::Button::selectable(state.kind == kind, label))
                .clicked()
            {
                state.kind = kind;
            }
        }
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| match view.scanning {
                Some((done, total, file)) => {
                    ui.spinner();
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("Scanning {done}/{total} {file}"))
                                .small()
                                .color(theme.text_dim),
                        )
                        .truncate(),
                    );
                }
                None => {
                    if ui
                        .button("Rescan")
                        .on_hover_text("Look for new and changed plugin files")
                        .clicked()
                    {
                        actions.push(PluginBrowserAction::Rescan);
                    }
                }
            },
        );
    });
    ui.separator();

    let needle = state.search.to_lowercase();
    let shown: Vec<usize> = (0..view.plugins.len())
        .filter(|&i| {
            let p = &view.plugins[i];
            state.kind.is_none_or(|k| k == p.kind)
                && (p.name.to_lowercase().contains(&needle)
                    || p.vendor.to_lowercase().contains(&needle))
        })
        .collect();
    egui::ScrollArea::vertical()
        .id_salt("gt_plugin_list")
        .max_height(280.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if view.plugins.is_empty() {
                ui.label(
                    RichText::new(if view.scanning.is_some() {
                        "Looking for CLAP plugins…"
                    } else {
                        "No CLAP plugins found. Install some, or add the folder they are in below."
                    })
                    .color(theme.text_dim),
                );
            }
            egui::Grid::new("gt_plugin_rows")
                .num_columns(4)
                .striped(true)
                .spacing(vec2(8.0, 3.0))
                .show(ui, |ui| {
                    for &i in &shown {
                        let p = &view.plugins[i];
                        fixed_label(ui, 210.0, RichText::new(&p.name).strong())
                            .on_hover_text(&p.details);
                        fixed_label(
                            ui,
                            120.0,
                            RichText::new(&p.vendor).small().color(theme.text_dim),
                        );
                        let (kind, tip) = match p.kind {
                            PluginKind::Instrument => (
                                "Instrument",
                                "Add as a new channel in the channel rack".to_owned(),
                            ),
                            PluginKind::Effect => (
                                "Effect",
                                view.effect_target.map_or_else(
                                    || "Select a mixer strip to add effects".to_owned(),
                                    |t| format!("Add to {t}"),
                                ),
                            ),
                        };
                        ui.label(RichText::new(kind).small().color(theme.text_dim));
                        let can = p.kind == PluginKind::Instrument || view.effect_target.is_some();
                        if ui
                            .add_enabled(can, egui::Button::new("Add"))
                            .on_hover_text(&tip)
                            .on_disabled_hover_text(&tip)
                            .clicked()
                        {
                            actions.push(PluginBrowserAction::Add(i));
                        }
                        ui.end_row();
                    }
                });
        });

    egui::CollapsingHeader::new(RichText::new("Folders").small())
        .id_salt("gt_plugin_folders")
        .show(ui, |ui| {
            for f in view.standard_folders {
                ui.label(RichText::new(f).small().color(theme.text_dim));
            }
            for (i, f) in view.folders.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(f).small());
                    if ui.small_button("Remove").clicked() {
                        actions.push(PluginBrowserAction::RemoveFolder(i));
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut state.new_folder)
                        .hint_text("Another folder with .clap files")
                        .desired_width(240.0),
                );
                if ui
                    .add_enabled(
                        !state.new_folder.trim().is_empty(),
                        egui::Button::new("Add"),
                    )
                    .clicked()
                {
                    actions.push(PluginBrowserAction::AddFolder(
                        state.new_folder.trim().to_owned(),
                    ));
                    state.new_folder.clear();
                }
            });
        });
    if !view.quarantine.is_empty() || !view.failed.is_empty() {
        let n = view.quarantine.len() + view.failed.len();
        egui::CollapsingHeader::new(
            RichText::new(format!("Problems ({n})"))
                .small()
                .color(theme.warn),
        )
        .id_salt("gt_plugin_problems")
        .show(ui, |ui| {
            for (i, (file, why)) in view.quarantine.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(file).small()).on_hover_text(why);
                    if ui
                        .small_button("Allow again")
                        .on_hover_text(format!("{why}. Load it again anyway?"))
                        .clicked()
                    {
                        actions.push(PluginBrowserAction::Release(i));
                    }
                });
            }
            for (file, why) in view.failed {
                ui.label(
                    RichText::new(format!("{file}: {why}"))
                        .small()
                        .color(theme.text_dim),
                );
            }
        });
    }
    actions
}
