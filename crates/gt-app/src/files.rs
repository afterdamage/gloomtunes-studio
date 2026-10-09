//! File dialogs: open, save as, export, relink missing samples, crash recovery, "save
//! changes?" and MIDI file import and export. Each is a small egui window; the native file picker (rfd) is offered through a
//! Browse button next to a path field, so everything also works where no picker is available.
//!
//! Drawing returns a [`DialogAction`] for the app to carry out; the dialogs never touch the
//! project themselves.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{RichText, Ui};
use gt_export::{BitDepth, ExportSettings, Progress, Range, SAMPLE_RATES};
use gt_ui::GloomTheme;

/// What to do once the current document is saved (or its changes discarded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Then {
    /// Nothing more.
    Stay,
    /// Start a new project.
    New,
    /// Show the Open dialog.
    Open,
    /// Quit the program.
    Quit,
}

/// The export dialog's settings.
#[derive(Debug, Clone)]
pub struct ExportForm {
    pub path: String,
    pub rate: u32,
    pub depth: BitDepth,
    pub dither: bool,
    pub normalize: bool,
    pub normalize_db: f32,
    pub loop_only: bool,
    pub tail: bool,
    pub stems: bool,
    /// The running export's progress.
    pub running: Option<Arc<Progress>>,
    /// Result of the last export.
    pub message: Option<String>,
}

impl ExportForm {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path: path.display().to_string(),
            rate: 48_000,
            depth: BitDepth::Pcm24,
            dither: true,
            normalize: false,
            normalize_db: -0.3,
            loop_only: false,
            tail: true,
            stems: false,
            running: None,
            message: None,
        }
    }

    /// The settings, given the loop region in ticks.
    pub fn settings(&self, loop_region: (i64, i64)) -> ExportSettings {
        ExportSettings {
            sample_rate: self.rate,
            depth: self.depth,
            dither: self.dither && self.depth != BitDepth::Float32,
            normalize_db: self.normalize.then_some(self.normalize_db),
            range: if self.loop_only {
                Range::Ticks {
                    start: loop_region.0,
                    end: loop_region.1,
                }
            } else {
                Range::Song
            },
            tail: self.tail,
            max_tail_seconds: 10.0,
            stems: self.stems,
        }
    }
}

/// An open dialog.
#[derive(Debug, Clone)]
pub enum Dialog {
    Open {
        path: String,
        error: Option<String>,
    },
    SaveAs {
        path: String,
        embed: bool,
        error: Option<String>,
        then: Then,
    },
    Export(ExportForm),
    Relink {
        missing: Vec<PathBuf>,
        folder: String,
        message: Option<String>,
    },
    Recover {
        /// When the autosave was written, for display.
        when: String,
    },
    Unsaved {
        name: String,
        then: Then,
    },
    ImportMidi {
        path: String,
        /// Take the file's tempo and time signatures.
        timing: bool,
        error: Option<String>,
    },
    ExportMidi {
        path: String,
        /// The whole arrangement (true) or the current pattern.
        song: bool,
        error: Option<String>,
    },
}

/// What the user chose in a dialog.
#[derive(Debug, Clone)]
pub enum DialogAction {
    Open(PathBuf),
    SaveAs {
        path: PathBuf,
        embed: bool,
        then: Then,
    },
    Export {
        path: PathBuf,
    },
    CancelExport,
    RelinkSearch(PathBuf),
    RelinkFile {
        from: PathBuf,
        to: PathBuf,
    },
    Recover,
    DiscardRecovery,
    /// "Save changes?" answered: save first (`true`) or discard.
    Unsaved {
        save: bool,
        then: Then,
    },
    ImportMidi {
        path: PathBuf,
        timing: bool,
    },
    ExportMidi {
        path: PathBuf,
        song: bool,
    },
    Close,
}

fn path_row(ui: &mut Ui, path: &mut String, browse: impl FnOnce(&str) -> Option<PathBuf>) {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(path).desired_width(360.0));
        if ui.button("Browse…").clicked() {
            if let Some(p) = browse(path) {
                *path = p.display().to_string();
            }
        }
    });
}

fn start_dir(path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    let dir = if p.is_dir() { Some(p) } else { p.parent() };
    dir.filter(|d| d.is_dir()).map(Path::to_path_buf)
}

fn project_picker(path: &str) -> rfd::FileDialog {
    let mut d =
        rfd::FileDialog::new().add_filter("GloomTunes project", &[gt_project::file::EXTENSION]);
    if let Some(dir) = start_dir(path) {
        d = d.set_directory(dir);
    }
    d
}

fn midi_picker(path: &str) -> rfd::FileDialog {
    let mut d = rfd::FileDialog::new().add_filter("MIDI file", &["mid", "midi"]);
    if let Some(dir) = start_dir(path) {
        d = d.set_directory(dir);
    }
    d
}

/// Adds the project extension if the name has none.
pub fn with_project_extension(p: PathBuf) -> PathBuf {
    if p.extension().is_none() {
        p.with_extension(gt_project::file::EXTENSION)
    } else {
        p
    }
}

/// Draws `dialog`. Returns what the user chose, if anything this frame.
pub fn show(ctx: &egui::Context, theme: &GloomTheme, dialog: &mut Dialog) -> Option<DialogAction> {
    let mut action = None;
    let mut open = true;
    let title = match dialog {
        Dialog::Open { .. } => "Open project",
        Dialog::SaveAs { .. } => "Save project as",
        Dialog::Export(_) => "Export audio",
        Dialog::Relink { .. } => "Missing samples",
        Dialog::Recover { .. } => "Recover unsaved work",
        Dialog::Unsaved { .. } => "Unsaved changes",
        Dialog::ImportMidi { .. } => "Import MIDI file",
        Dialog::ExportMidi { .. } => "Export MIDI file",
    };
    let dim = |t: &str| RichText::new(t).color(theme.text_dim);
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -40.0))
        .open(&mut open)
        .show(ctx, |ui| match dialog {
            Dialog::Open { path, error } => {
                ui.label(dim("Project file"));
                path_row(ui, path, |p| project_picker(p).pick_file());
                if let Some(e) = error {
                    ui.label(RichText::new(e.as_str()).color(theme.warn));
                }
                ui.horizontal(|ui| {
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Open").clicked() || enter {
                        action = Some(DialogAction::Open(PathBuf::from(path.trim())));
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(DialogAction::Close);
                    }
                });
            }
            Dialog::SaveAs {
                path,
                embed,
                error,
                then,
            } => {
                ui.label(dim("Save to"));
                path_row(ui, path, |p| project_picker(p).save_file());
                ui.checkbox(embed, "Embed samples").on_hover_text(
                    "Copy every audio file into the project so it opens on another \
                         computer. Makes the file larger.",
                );
                if let Some(e) = error {
                    ui.label(RichText::new(e.as_str()).color(theme.warn));
                }
                ui.horizontal(|ui| {
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Save").clicked() || enter {
                        action = Some(DialogAction::SaveAs {
                            path: with_project_extension(PathBuf::from(path.trim())),
                            embed: *embed,
                            then: *then,
                        });
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(DialogAction::Close);
                    }
                });
            }
            Dialog::Export(form) => action = export_form(ui, theme, form),
            Dialog::Relink {
                missing,
                folder,
                message,
            } => {
                ui.label(format!(
                    "{} audio file{} could not be found. Point to a folder to search it and its \
                     subfolders, or locate each file.",
                    missing.len(),
                    if missing.len() == 1 { "" } else { "s" }
                ));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(folder).desired_width(300.0));
                    if ui.button("Browse…").clicked() {
                        let mut d = rfd::FileDialog::new();
                        if let Some(dir) = start_dir(folder) {
                            d = d.set_directory(dir);
                        }
                        if let Some(p) = d.pick_folder() {
                            *folder = p.display().to_string();
                        }
                    }
                    if ui.button("Search").clicked() {
                        action = Some(DialogAction::RelinkSearch(PathBuf::from(folder.trim())));
                    }
                });
                if let Some(m) = message {
                    ui.label(RichText::new(m.as_str()).color(theme.text_dim));
                }
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for m in missing.iter() {
                            ui.horizontal(|ui| {
                                if ui.small_button("Locate…").clicked() {
                                    let name =
                                        m.file_name().map(|n| n.to_string_lossy().into_owned());
                                    let mut d = rfd::FileDialog::new()
                                        .add_filter("Audio", &gt_project::AUDIO_EXTENSIONS);
                                    if let Some(n) = name {
                                        d = d.set_file_name(n);
                                    }
                                    if let Some(to) = d.pick_file() {
                                        action = Some(DialogAction::RelinkFile {
                                            from: m.clone(),
                                            to,
                                        });
                                    }
                                }
                                ui.label(RichText::new(m.display().to_string()).small());
                            });
                        }
                    });
                if ui.button("Continue without them").clicked() {
                    action = Some(DialogAction::Close);
                }
            }
            Dialog::Recover { when } => {
                ui.label(format!(
                    "GloomTunes Studio did not close normally last time. An autosave from \
                     {when} is available."
                ));
                ui.horizontal(|ui| {
                    if ui.button("Recover").clicked() {
                        action = Some(DialogAction::Recover);
                    }
                    if ui.button("Discard").clicked() {
                        action = Some(DialogAction::DiscardRecovery);
                    }
                });
            }
            Dialog::ImportMidi {
                path,
                timing,
                error,
            } => {
                ui.label(dim("MIDI file"));
                path_row(ui, path, |p| midi_picker(p).pick_file());
                ui.checkbox(timing, "Use the file's tempo and time signature");
                ui.label(dim(
                    "Each part becomes a new Gloom Synth channel; the notes go into a new pattern.",
                ));
                if let Some(e) = error {
                    ui.label(RichText::new(e.as_str()).color(theme.warn));
                }
                ui.horizontal(|ui| {
                    if ui.button("Import").clicked() {
                        action = Some(DialogAction::ImportMidi {
                            path: PathBuf::from(path.trim()),
                            timing: *timing,
                        });
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(DialogAction::Close);
                    }
                });
            }
            Dialog::ExportMidi { path, song, error } => {
                ui.label(dim("Save to"));
                path_row(ui, path, |p| midi_picker(p).save_file());
                ui.horizontal(|ui| {
                    ui.selectable_value(song, true, "Whole song");
                    ui.selectable_value(song, false, "Current pattern");
                });
                ui.label(dim("Format 1: a tempo track, then one track per channel."));
                if let Some(e) = error {
                    ui.label(RichText::new(e.as_str()).color(theme.warn));
                }
                ui.horizontal(|ui| {
                    if ui.button("Export").clicked() {
                        let mut p = PathBuf::from(path.trim());
                        if p.extension().is_none() {
                            p.set_extension("mid");
                        }
                        action = Some(DialogAction::ExportMidi {
                            path: p,
                            song: *song,
                        });
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(DialogAction::Close);
                    }
                });
            }
            Dialog::Unsaved { name, then } => {
                ui.label(format!("Save the changes to \"{name}\"?"));
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        action = Some(DialogAction::Unsaved {
                            save: true,
                            then: *then,
                        });
                    }
                    if ui.button("Don't save").clicked() {
                        action = Some(DialogAction::Unsaved {
                            save: false,
                            then: *then,
                        });
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(DialogAction::Close);
                    }
                });
            }
        });
    if !open && action.is_none() {
        action = Some(match dialog {
            Dialog::Recover { .. } => DialogAction::DiscardRecovery,
            Dialog::Export(f) if f.running.is_some() => DialogAction::CancelExport,
            _ => DialogAction::Close,
        });
    }
    action
}

fn export_form(ui: &mut Ui, theme: &GloomTheme, f: &mut ExportForm) -> Option<DialogAction> {
    let mut action = None;
    let running = f.running.clone();
    ui.add_enabled_ui(running.is_none(), |ui| {
        ui.label(RichText::new("File").color(theme.text_dim));
        path_row(ui, &mut f.path, |p| {
            let mut d = rfd::FileDialog::new().add_filter("WAV", &["wav"]);
            if let Some(dir) = start_dir(p) {
                d = d.set_directory(dir);
            }
            d.save_file()
        });
        egui::Grid::new("export_grid")
            .num_columns(2)
            .spacing(egui::vec2(10.0, 6.0))
            .show(ui, |ui| {
                ui.label("Range");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut f.loop_only, false, "Whole song");
                    ui.selectable_value(&mut f.loop_only, true, "Loop region");
                });
                ui.end_row();
                ui.label("Sample rate");
                egui::ComboBox::from_id_salt("export_rate")
                    .selected_text(format!("{} Hz", f.rate))
                    .show_ui(ui, |ui| {
                        for r in SAMPLE_RATES {
                            ui.selectable_value(&mut f.rate, r, format!("{r} Hz"));
                        }
                    });
                ui.end_row();
                ui.label("Format");
                ui.horizontal(|ui| {
                    for d in BitDepth::ALL {
                        ui.selectable_value(&mut f.depth, d, d.label());
                    }
                });
                ui.end_row();
                ui.label("");
                ui.add_enabled(
                    f.depth != BitDepth::Float32,
                    egui::Checkbox::new(&mut f.dither, "Dither (TPDF)"),
                )
                .on_hover_text(
                    "Adds a little noise before rounding to 16 or 24 bits, so quiet passages \
                     fade into noise instead of distortion",
                );
                ui.end_row();
                ui.label("");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut f.normalize, "Normalize to");
                    ui.add_enabled(
                        f.normalize,
                        egui::DragValue::new(&mut f.normalize_db)
                            .range(-24.0..=0.0)
                            .speed(0.1)
                            .suffix(" dBFS"),
                    );
                });
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut f.tail, "Render tails")
                    .on_hover_text("Keep going after the end until reverb and delays fade out");
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut f.stems, "One file per track (stems)")
                    .on_hover_text("Each playlist track with patterns or audio, through the mixer");
                ui.end_row();
            });
    });
    if let Some(p) = &running {
        ui.add(egui::ProgressBar::new(p.fraction()).show_percentage());
        if ui.button("Cancel").clicked() {
            action = Some(DialogAction::CancelExport);
        }
    } else {
        if let Some(m) = &f.message {
            ui.label(RichText::new(m.as_str()).color(theme.text_dim));
        }
        ui.horizontal(|ui| {
            if ui.button("Export").clicked() {
                action = Some(DialogAction::Export {
                    path: PathBuf::from(f.path.trim()),
                });
            }
            if ui.button("Close").clicked() {
                action = Some(DialogAction::Close);
            }
        });
    }
    action
}
