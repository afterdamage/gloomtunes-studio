//! The browser (Prompt 25, ARCHITECTURE.md D132): tabs for samples, instrument presets,
//! effect presets, plugins and projects, a search box over the app's file index, favourites
//! and user tags.
//!
//! Click a sound to hear it, double-click (or use the button) to load it into the selected
//! channel, or drag it onto a playlist track. Double-click a preset, plugin or project to use
//! it. The app fills the rows (searching its index on a worker-built list) and acts on the
//! returned actions; this view only shows them.

use std::path::PathBuf;

use egui::{RichText, Ui};
use gt_core::SampleSource;

use crate::i18n::tr;
use crate::GloomTheme;

/// One line of the folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserEntry {
    /// A sub-folder.
    Dir(String, PathBuf),
    /// A supported audio file.
    File(String, PathBuf),
}

/// The browser's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BrowserTab {
    /// Audio files and the built-in sounds.
    #[default]
    Samples,
    /// Gloom Synth presets, factory and the user's.
    Instruments,
    /// Effect presets.
    Effects,
    /// Plugins from the plugin catalog.
    Plugins,
    /// Project files under the root folders.
    Projects,
}

impl BrowserTab {
    /// Every tab, in order.
    pub const ALL: [BrowserTab; 5] = [
        BrowserTab::Samples,
        BrowserTab::Instruments,
        BrowserTab::Effects,
        BrowserTab::Plugins,
        BrowserTab::Projects,
    ];

    /// Tab label.
    pub fn label(self) -> &'static str {
        match self {
            BrowserTab::Samples => tr("Samples"),
            BrowserTab::Instruments => tr("Instruments"),
            BrowserTab::Effects => tr("Effects"),
            BrowserTab::Plugins => tr("Plugins"),
            BrowserTab::Projects => tr("Projects"),
        }
    }

    /// Stable name for the settings file.
    pub fn key(self) -> &'static str {
        match self {
            BrowserTab::Samples => "samples",
            BrowserTab::Instruments => "instruments",
            BrowserTab::Effects => "effects",
            BrowserTab::Plugins => "plugins",
            BrowserTab::Projects => "projects",
        }
    }

    /// The tab named `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.key() == key)
    }
}

/// What a row stands for.
#[derive(Debug, Clone, PartialEq)]
pub enum RowItem {
    /// A sound (built-in or a file).
    Sample(SampleSource),
    /// A folder to list.
    Folder(PathBuf),
    /// A factory synth preset, by index into `SynthPatch::factory()`.
    FactorySynth(usize),
    /// A synth preset file.
    UserSynth(PathBuf),
    /// An effect preset: effect key and preset index (`None`: the effect's defaults).
    Effect(String, Option<usize>),
    /// A plugin, by index into the catalog list the app keeps.
    Plugin(usize),
    /// A project file.
    Project(PathBuf),
}

/// One row of the list.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserRow {
    /// Favourites and tags are stored under this key (a path, or "synth:Name" and the like).
    pub key: String,
    /// Shown name.
    pub name: String,
    /// Second line: folder, vendor or engine.
    pub detail: String,
    /// Tempo from the name.
    pub bpm: Option<f32>,
    /// Key from the name.
    pub music_key: Option<String>,
    /// The user's tags.
    pub tags: Vec<String>,
    /// Starred.
    pub favourite: bool,
    /// What it is.
    pub item: RowItem,
}

/// What the browser shows. The app fills `rows` (and `entries` for folder browsing) when the
/// tab, the search or the folder changes.
#[derive(Debug, Clone, Default)]
pub struct BrowserModel {
    /// Open tab.
    pub tab: BrowserTab,
    /// Search text.
    pub query: String,
    /// Show only favourites.
    pub favourites_only: bool,
    /// The root folders searched.
    pub roots: Vec<PathBuf>,
    /// The listed folder (samples tab with an empty search).
    pub folder: PathBuf,
    /// Editable folder path for adding a root.
    pub path_text: String,
    /// Folder contents: folders first, then audio files.
    pub entries: Vec<BrowserEntry>,
    /// Rows to show (search results or the tab's list).
    pub rows: Vec<BrowserRow>,
    /// Matches in all (rows may be cut short).
    pub total: usize,
    /// Problem listing the folder.
    pub error: Option<String>,
    /// Index state ("Indexing…", "12,345 files").
    pub status: String,
    /// The row last clicked, by key.
    pub selected: Option<String>,
    /// The sound last clicked.
    pub selected_sample: Option<SampleSource>,
    /// Preview loops follow the project tempo.
    pub sync_preview: bool,
    /// Tag text being edited, for the row with this key.
    tag_edit: Option<(String, String)>,
}

/// User intents raised by the browser.
#[derive(Debug, Clone, PartialEq)]
pub enum BrowserAction {
    /// Play the sound through the preview voice (`bpm`: its tempo, for tempo-synced preview).
    Preview(SampleSource, Option<f32>),
    /// Load the sound into the selected channel.
    Assign(SampleSource),
    /// List another folder.
    Open(PathBuf),
    /// The tab, the search or the favourites filter changed: fill the rows again.
    Refresh,
    /// Use a row: load the preset, add the effect or plugin, open the project.
    Activate(RowItem),
    /// Star or unstar the row with this key.
    ToggleFavourite(String),
    /// Set the tags of the row with this key.
    SetTags(String, Vec<String>),
    /// Search this folder too.
    AddRoot(PathBuf),
    /// Stop searching root `i`.
    RemoveRoot(usize),
    /// Walk the root folders again.
    Rescan,
}

/// Draws the browser. `target` names the channel that "Load" will fill.
pub fn browser(
    ui: &mut Ui,
    theme: &GloomTheme,
    m: &mut BrowserModel,
    target: Option<&str>,
) -> Vec<BrowserAction> {
    let mut actions = Vec::new();
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);

    ui.horizontal_wrapped(|ui| {
        for t in BrowserTab::ALL {
            if ui
                .add(egui::Button::selectable(m.tab == t, t.label()).small())
                .clicked()
                && m.tab != t
            {
                m.tab = t;
                actions.push(BrowserAction::Refresh);
            }
        }
    });
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(&mut m.query)
                .desired_width(ui.available_width() - 30.0)
                .hint_text(tr("Search (name, folder, 120bpm, key:am, #tag)")),
        );
        if r.changed() {
            actions.push(BrowserAction::Refresh);
        }
        let star = if m.favourites_only { "★" } else { "☆" };
        if ui
            .add(egui::Button::selectable(m.favourites_only, star))
            .on_hover_text(tr("Show only favourites"))
            .clicked()
        {
            m.favourites_only = !m.favourites_only;
            actions.push(BrowserAction::Refresh);
        }
    });
    if m.tab == BrowserTab::Samples {
        ui.horizontal(|ui| {
            let can_load = m.selected_sample.is_some() && target.is_some();
            let label = match target {
                Some(t) => format!("{} {t}", tr("Load into")),
                None => tr("Load").to_owned(),
            };
            if ui
                .add_enabled(can_load, egui::Button::new(label).truncate())
                .on_hover_text(tr(
                    "Load the selected sound into the selected channel (or double-click it)",
                ))
                .clicked()
            {
                if let Some(s) = m.selected_sample.clone() {
                    actions.push(BrowserAction::Assign(s));
                }
            }
        });
        ui.checkbox(&mut m.sync_preview, tr("Preview at project tempo"))
            .on_hover_text(tr(
                "Loops whose name gives a tempo (\"120bpm\") are previewed stretched to the \
                 project tempo",
            ));
    }
    if !m.status.is_empty() {
        ui.label(dim(&m.status).small());
    }
    ui.separator();

    let browsing = m.tab == BrowserTab::Samples && m.query.trim().is_empty() && !m.favourites_only;
    if browsing {
        folder_list(ui, theme, m, &mut actions);
    } else {
        row_list(ui, theme, m, &mut actions);
    }
    actions
}

/// The samples tab without a search: roots, then the listed folder.
fn folder_list(
    ui: &mut Ui,
    theme: &GloomTheme,
    m: &mut BrowserModel,
    actions: &mut Vec<BrowserAction>,
) {
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Built-in sounds and favourites come as rows.
            let rows = m.rows.clone();
            for row in &rows {
                row_ui(ui, theme, m, row, actions);
            }
            ui.add_space(6.0);
            ui.label(dim(tr("Folders")));
            let roots = m.roots.clone();
            for (i, root) in roots.iter().enumerate() {
                ui.horizontal(|ui| {
                    let name = root
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| root.display().to_string());
                    if ui
                        .selectable_label(m.folder == *root, RichText::new(format!("{name}/")))
                        .on_hover_text(root.display().to_string())
                        .clicked()
                    {
                        actions.push(BrowserAction::Open(root.clone()));
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text(tr("Stop searching this folder"))
                        .clicked()
                    {
                        actions.push(BrowserAction::RemoveRoot(i));
                    }
                });
            }
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut m.path_text)
                        .desired_width(ui.available_width() - 70.0)
                        .hint_text(tr("Folder path")),
                );
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui
                    .button(tr("Go"))
                    .on_hover_text(tr("List this folder"))
                    .clicked()
                    || enter
                {
                    actions.push(BrowserAction::Open(PathBuf::from(m.path_text.trim())));
                }
                if ui
                    .button("+")
                    .on_hover_text(tr("Add this folder to the searched folders"))
                    .clicked()
                {
                    actions.push(BrowserAction::AddRoot(PathBuf::from(m.path_text.trim())));
                }
            });
            if ui.small_button(tr("Rescan folders")).clicked() {
                actions.push(BrowserAction::Rescan);
            }
            ui.add_space(6.0);
            if m.folder.as_os_str().is_empty() {
                return;
            }
            ui.label(dim(&m.folder.display().to_string()));
            if let Some(parent) = m.folder.parent() {
                if ui
                    .selectable_label(false, dim(tr(".. (up)")))
                    .on_hover_text(parent.display().to_string())
                    .clicked()
                {
                    actions.push(BrowserAction::Open(parent.to_path_buf()));
                }
            }
            if let Some(e) = &m.error {
                ui.label(RichText::new(e).color(theme.warn));
            }
            let entries = m.entries.clone();
            for e in entries {
                match e {
                    BrowserEntry::Dir(name, path) => {
                        if ui
                            .selectable_label(
                                false,
                                RichText::new(format!("{name}/")).color(theme.text_dim),
                            )
                            .clicked()
                        {
                            actions.push(BrowserAction::Open(path));
                        }
                    }
                    BrowserEntry::File(name, path) => {
                        let key = path.display().to_string();
                        let bpm = bpm_in_name(&name);
                        let row = BrowserRow {
                            key,
                            name,
                            detail: String::new(),
                            bpm,
                            music_key: None,
                            tags: Vec::new(),
                            favourite: false,
                            item: RowItem::Sample(SampleSource::File(path)),
                        };
                        row_ui(ui, theme, m, &row, actions);
                    }
                }
            }
            if m.entries.is_empty() && m.error.is_none() {
                ui.label(dim(tr(
                    "No folders or audio files here (wav, flac, mp3, ogg).",
                )));
            }
        });
}

/// Tempo from a file name, for folder rows (the index has it for searched rows).
fn bpm_in_name(name: &str) -> Option<f32> {
    let lower = name.to_ascii_lowercase();
    let i = lower.find("bpm")?;
    let digits: String = lower[..i]
        .trim_end_matches([' ', '_', '-'])
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().ok().filter(|v| (40.0..=300.0).contains(v))
}

/// Search results or the tab's list, drawn only where visible.
fn row_list(
    ui: &mut Ui,
    theme: &GloomTheme,
    m: &mut BrowserModel,
    actions: &mut Vec<BrowserAction>,
) {
    if m.rows.is_empty() {
        let text = if m.query.trim().is_empty() && !m.favourites_only {
            tr("Nothing here yet.")
        } else {
            tr("Nothing matches.")
        };
        ui.label(RichText::new(text).color(theme.text_dim));
        return;
    }
    if m.total > m.rows.len() {
        ui.label(
            RichText::new(format!("{} {} {}", m.rows.len(), tr("shown of"), m.total))
                .small()
                .color(theme.text_dim),
        );
    }
    let row_h = ui.spacing().interact_size.y * 2.0 + 2.0;
    let rows = std::mem::take(&mut m.rows);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, row_h, rows.len(), |ui, range| {
            for row in &rows[range] {
                row_ui(ui, theme, m, row, actions);
            }
        });
    m.rows = rows;
}

fn row_ui(
    ui: &mut Ui,
    theme: &GloomTheme,
    m: &mut BrowserModel,
    row: &BrowserRow,
    actions: &mut Vec<BrowserAction>,
) {
    let selected = m.selected.as_deref() == Some(row.key.as_str());
    let mut badges = String::new();
    if let Some(b) = row.bpm {
        badges.push_str(&format!(" · {b:.0} bpm"));
    }
    if let Some(k) = &row.music_key {
        badges.push_str(&format!(" · {k}"));
    }
    for t in &row.tags {
        badges.push_str(&format!(" #{t}"));
    }
    ui.horizontal(|ui| {
        let star = if row.favourite { "★" } else { "☆" };
        if ui
            .add(
                egui::Button::new(RichText::new(star).color(if row.favourite {
                    theme.accent
                } else {
                    theme.text_dim
                }))
                .frame(false),
            )
            .on_hover_text(tr("Favourite"))
            .clicked()
        {
            actions.push(BrowserAction::ToggleFavourite(row.key.clone()));
        }
        let r = ui
            .add(
                egui::Button::selectable(selected, &row.name)
                    .truncate()
                    .sense(egui::Sense::click_and_drag()),
            )
            .on_hover_text(hover_text(row));
        let sample = match &row.item {
            RowItem::Sample(s) => Some(s.clone()),
            _ => None,
        };
        if let Some(src) = &sample {
            if r.drag_started() {
                // Load it while it is being dragged, so the clip gets its length and waveform.
                m.selected = Some(row.key.clone());
                m.selected_sample = Some(src.clone());
                actions.push(BrowserAction::Preview(src.clone(), row.bpm));
            }
            r.dnd_set_drag_payload(src.clone());
        }
        if r.double_clicked() {
            m.selected = Some(row.key.clone());
            match &row.item {
                RowItem::Sample(s) => actions.push(BrowserAction::Assign(s.clone())),
                RowItem::Folder(p) => actions.push(BrowserAction::Open(p.clone())),
                other => actions.push(BrowserAction::Activate(other.clone())),
            }
        } else if r.clicked() {
            m.selected = Some(row.key.clone());
            match &row.item {
                RowItem::Sample(s) => {
                    m.selected_sample = Some(s.clone());
                    actions.push(BrowserAction::Preview(s.clone(), row.bpm));
                }
                RowItem::Folder(p) => actions.push(BrowserAction::Open(p.clone())),
                _ => {}
            }
        }
        r.context_menu(|ui| {
            if ui.button(tr("Edit tags…")).clicked() {
                m.tag_edit = Some((row.key.clone(), row.tags.join(", ")));
                ui.close();
            }
            let fav = if row.favourite {
                tr("Remove from favourites")
            } else {
                tr("Add to favourites")
            };
            if ui.button(fav).clicked() {
                actions.push(BrowserAction::ToggleFavourite(row.key.clone()));
                ui.close();
            }
            if !matches!(row.item, RowItem::Sample(_) | RowItem::Folder(_))
                && ui.button(tr("Use")).clicked()
            {
                actions.push(BrowserAction::Activate(row.item.clone()));
                ui.close();
            }
        });
    });
    if !row.detail.is_empty() || !badges.is_empty() {
        ui.label(
            RichText::new(format!("    {}{badges}", row.detail))
                .small()
                .color(theme.text_dim),
        );
    }
    if let Some((key, text)) = &mut m.tag_edit {
        if *key == row.key {
            let mut done = false;
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(text)
                        .desired_width(ui.available_width() - 40.0)
                        .hint_text(tr("Tags, separated by commas")),
                );
                r.request_focus();
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.small_button(tr("OK")).clicked() || enter {
                    let tags = parse_tags(text);
                    actions.push(BrowserAction::SetTags(key.clone(), tags));
                    done = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = true;
                }
            });
            if done {
                m.tag_edit = None;
            }
        }
    }
}

fn hover_text(row: &BrowserRow) -> String {
    match &row.item {
        RowItem::Sample(_) => tr(
            "Click to hear, double-click to load into the selected channel, drag onto a \
             playlist track for an audio clip; right-click for tags",
        )
        .to_owned(),
        RowItem::Folder(_) => tr("Open this folder").to_owned(),
        RowItem::FactorySynth(_) | RowItem::UserSynth(_) => {
            tr("Double-click to load into the selected synth channel (or a new one)").to_owned()
        }
        RowItem::Effect(..) => {
            tr("Double-click to add to the selected mixer track's first free slot").to_owned()
        }
        RowItem::Plugin(_) => tr(
            "Double-click to add: an instrument as a new channel, an effect to the selected \
             mixer track",
        )
        .to_owned(),
        RowItem::Project(p) => format!("{}\n{}", tr("Double-click to open"), p.display()),
    }
}

/// Tags typed as "punchy, Dark , 808" become ["punchy", "Dark", "808"] (empty ones and
/// repeats dropped).
pub fn parse_tags(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in text
        .split(',')
        .map(|t| t.trim().trim_start_matches('#').trim())
    {
        if !t.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(t)) {
            out.push(t.to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_parse_and_tabs_round_trip() {
        assert_eq!(
            parse_tags(" punchy, Dark ,, #808, dark"),
            vec!["punchy", "Dark", "808"]
        );
        for t in BrowserTab::ALL {
            assert_eq!(BrowserTab::from_key(t.key()), Some(t));
        }
        assert_eq!(bpm_in_name("Loop_128bpm"), Some(128.0));
        assert_eq!(bpm_in_name("Kick"), None);
    }
}
