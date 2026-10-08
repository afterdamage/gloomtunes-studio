//! Sample browser: the built-in sounds and one folder on disk.
//!
//! Click a sound to hear it, double-click (or use the button) to load it into the selected
//! channel, or drag it onto a playlist track. The app lists folders and loads files; this view
//! only shows them.

use std::path::PathBuf;

use egui::{RichText, Ui};
use gt_core::{BuiltInSample, SampleSource};

use crate::GloomTheme;

/// One line of the folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserEntry {
    /// A sub-folder.
    Dir(String, PathBuf),
    /// A supported audio file.
    File(String, PathBuf),
}

/// What the browser shows. The app fills `entries` when the folder changes.
#[derive(Debug, Clone, Default)]
pub struct BrowserModel {
    /// The listed folder.
    pub folder: PathBuf,
    /// Editable folder path.
    pub path_text: String,
    /// Folder contents: folders first, then audio files.
    pub entries: Vec<BrowserEntry>,
    /// Problem listing the folder.
    pub error: Option<String>,
    /// The sound last clicked.
    pub selected: Option<SampleSource>,
}

/// User intents raised by the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserAction {
    /// Play the sound through the preview voice.
    Preview(SampleSource),
    /// Load the sound into the selected channel.
    Assign(SampleSource),
    /// List another folder.
    Open(PathBuf),
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

    ui.horizontal(|ui| {
        let can_load = m.selected.is_some() && target.is_some();
        let label = match target {
            Some(t) => format!("Load into {t}"),
            None => "Load".to_owned(),
        };
        if ui
            .add_enabled(can_load, egui::Button::new(label).truncate())
            .on_hover_text("Load the selected sound into the selected channel (or double-click it)")
            .clicked()
        {
            if let Some(s) = m.selected.clone() {
                actions.push(BrowserAction::Assign(s));
            }
        }
    });
    ui.separator();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.label(dim("Built-in"));
            for b in BuiltInSample::ALL {
                entry(ui, m, b.name(), SampleSource::BuiltIn(b), &mut actions);
            }
            ui.add_space(6.0);
            ui.label(dim("Folder"));
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut m.path_text)
                        .desired_width(ui.available_width() - 34.0)
                        .hint_text("Folder path"),
                );
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Go").clicked() || enter {
                    actions.push(BrowserAction::Open(PathBuf::from(m.path_text.trim())));
                }
            });
            if let Some(parent) = m.folder.parent() {
                if ui
                    .selectable_label(false, dim(".. (up)"))
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
                        entry(ui, m, &name, SampleSource::File(path), &mut actions);
                    }
                }
            }
            if m.entries.is_empty() && m.error.is_none() {
                ui.label(dim("No folders or audio files here (wav, flac, mp3, ogg)."));
            }
        });
    actions
}

fn entry(
    ui: &mut Ui,
    m: &mut BrowserModel,
    name: &str,
    src: SampleSource,
    actions: &mut Vec<BrowserAction>,
) {
    let selected = m.selected.as_ref() == Some(&src);
    let r = ui
        .add(
            egui::Button::selectable(selected, name)
                .truncate()
                .sense(egui::Sense::click_and_drag()),
        )
        .on_hover_text(
            "Click to hear, double-click to load into the selected channel, drag onto a \
             playlist track for an audio clip",
        );
    if r.drag_started() {
        // Load it while it is being dragged, so the clip gets its length and waveform.
        m.selected = Some(src.clone());
        actions.push(BrowserAction::Preview(src.clone()));
    }
    r.dnd_set_drag_payload(src.clone());
    if r.double_clicked() {
        m.selected = Some(src.clone());
        actions.push(BrowserAction::Assign(src));
    } else if r.clicked() {
        m.selected = Some(src.clone());
        actions.push(BrowserAction::Preview(src));
    }
}
