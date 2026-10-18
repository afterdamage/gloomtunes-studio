//! File dialogs: open, save as, export, relink missing samples, crash recovery, "save
//! changes?" and MIDI file import and export. Each is a small egui window; the native file picker (rfd) is offered through a
//! Browse button next to a path field, so everything also works where no picker is available.
//!
//! Drawing returns a [`DialogAction`] for the app to carry out; the dialogs never touch the
//! project themselves.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{RichText, Ui};
use gt_export::video::{VideoSettings, VideoStyle};
use gt_export::{
    BitDepth, ExportSettings, Format, Mp3Mode, Progress, Range, MP3_BITRATES, SAMPLE_RATES,
};
use gt_ui::tr;
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
    /// Open the project the browser asked for (`GloomApp::pending_open`).
    OpenPending,
    /// Open factory demo `n` (`gt_core::demos::DEMOS`) as a new, unsaved project.
    Demo(usize),
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
    /// Stems per mixer insert instead of per track (Prompt 29).
    pub insert_stems: bool,
    /// One file per marker region.
    pub regions: bool,
    pub format: Format,
    /// Ogg Vorbis quality, 0 to 10.
    pub ogg_quality: f32,
    /// MP3: variable bitrate at `mp3_quality` instead of `mp3_kbps`.
    pub mp3_vbr: bool,
    pub mp3_kbps: u16,
    /// 0 (best) to 9.
    pub mp3_quality: u8,
    /// Also make an MP4 of the mix.
    pub video: bool,
    /// 0 spectrum, 1 waveform, 2 still image.
    pub video_style: usize,
    pub video_image: String,
    /// ffmpeg was found when the dialog opened.
    pub ffmpeg: bool,
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
            insert_stems: false,
            regions: false,
            format: Format::from_path(&path).unwrap_or_default(),
            ogg_quality: 6.0,
            mp3_vbr: false,
            mp3_kbps: 320,
            mp3_quality: 2,
            video: false,
            video_style: 0,
            video_image: String::new(),
            ffmpeg: gt_export::video::find_ffmpeg().is_some(),
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
            stem_kind: if self.insert_stems {
                gt_export::StemKind::Inserts
            } else {
                gt_export::StemKind::Tracks
            },
            regions: self.regions,
            format: self.format,
            ogg_quality: self.ogg_quality,
            mp3: if self.mp3_vbr {
                Mp3Mode::Vbr(self.mp3_quality)
            } else {
                Mp3Mode::Cbr(self.mp3_kbps)
            },
        }
    }

    /// The video to make with the export, in the theme's colours: only for a single mix file.
    pub fn video_settings(&self, theme: &GloomTheme) -> Option<VideoSettings> {
        if !self.video || !self.ffmpeg || self.stems || self.regions {
            return None;
        }
        let rgb = |c: egui::Color32| [c.r(), c.g(), c.b()];
        Some(VideoSettings {
            style: match self.video_style {
                0 => VideoStyle::Spectrum,
                1 => VideoStyle::Waveform,
                _ => VideoStyle::Image(PathBuf::from(self.video_image.trim())),
            },
            background: rgb(theme.bg_deep),
            accent: rgb(theme.accent),
            ..VideoSettings::default()
        })
    }

    /// Picks a format and gives the file name its extension.
    fn set_format(&mut self, format: Format) {
        self.format = format;
        let p = PathBuf::from(self.path.trim());
        if !self.path.trim().is_empty() {
            self.path = p.with_extension(format.extension()).display().to_string();
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
        /// Plugin files loaded when the last session ended.
        plugins: Vec<String>,
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
    /// Start a project from a template (Prompt 25).
    NewProject {
        templates: Vec<(String, PathBuf)>,
    },
    /// Keep the project as a template.
    SaveTemplate {
        name: String,
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
    /// Recover; `true` keeps the project's plugins from loading until retried.
    Recover(bool),
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
    /// Start from this template, or an empty project.
    NewFrom(Option<PathBuf>),
    SaveTemplate(String),
    DeleteTemplate(PathBuf),
    Close,
}

fn path_row(ui: &mut Ui, path: &mut String, browse: impl FnOnce(&str) -> Option<PathBuf>) {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(path).desired_width(360.0));
        if ui.button(tr("Browse…")).clicked() {
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
        Dialog::NewProject { .. } => "New project",
        Dialog::SaveTemplate { .. } => "Save as template",
    };
    let dim = |t: &str| RichText::new(t).color(theme.text_dim);
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -40.0))
        .open(&mut open)
        .show(ctx, |ui| match dialog {
            Dialog::Open { path, error } => {
                ui.label(dim(tr("Project file")));
                path_row(ui, path, |p| project_picker(p).pick_file());
                if let Some(e) = error {
                    ui.label(RichText::new(e.as_str()).color(theme.warn));
                }
                ui.horizontal(|ui| {
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button(tr("Open")).clicked() || enter {
                        action = Some(DialogAction::Open(PathBuf::from(path.trim())));
                    }
                    if ui.button(tr("Cancel")).clicked() {
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
                ui.label(dim(tr("Save to")));
                path_row(ui, path, |p| project_picker(p).save_file());
                ui.checkbox(embed, tr("Embed samples")).on_hover_text(
                    tr("Copy every audio file into the project so it opens on another \
                         computer. Makes the file larger."),
                );
                if let Some(e) = error {
                    ui.label(RichText::new(e.as_str()).color(theme.warn));
                }
                ui.horizontal(|ui| {
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button(tr("Save")).clicked() || enter {
                        action = Some(DialogAction::SaveAs {
                            path: with_project_extension(PathBuf::from(path.trim())),
                            embed: *embed,
                            then: *then,
                        });
                    }
                    if ui.button(tr("Cancel")).clicked() {
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
                    if ui.button(tr("Browse…")).clicked() {
                        let mut d = rfd::FileDialog::new();
                        if let Some(dir) = start_dir(folder) {
                            d = d.set_directory(dir);
                        }
                        if let Some(p) = d.pick_folder() {
                            *folder = p.display().to_string();
                        }
                    }
                    if ui.button(tr("Search")).clicked() {
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
                                if ui.small_button(tr("Locate…")).clicked() {
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
                if ui.button(tr("Continue without them")).clicked() {
                    action = Some(DialogAction::Close);
                }
            }
            Dialog::Recover { when, plugins } => {
                ui.label(format!(
                    "GloomTunes Studio did not close normally last time. An autosave from \
                     {when} is available."
                ));
                if !plugins.is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "These plugins were loaded at the time and may have caused it: {}",
                            plugins.join(", ")
                        ))
                        .small(),
                    );
                }
                ui.horizontal(|ui| {
                    if ui.button(tr("Recover")).clicked() {
                        action = Some(DialogAction::Recover(false));
                    }
                    if !plugins.is_empty()
                        && ui
                            .button(tr("Recover without plugins"))
                            .on_hover_text(
                                tr("Open it with its plugins switched off; retry each one from its panel"),
                            )
                            .clicked()
                    {
                        action = Some(DialogAction::Recover(true));
                    }
                    if ui.button(tr("Discard")).clicked() {
                        action = Some(DialogAction::DiscardRecovery);
                    }
                });
            }
            Dialog::ImportMidi {
                path,
                timing,
                error,
            } => {
                ui.label(dim(tr("MIDI file")));
                path_row(ui, path, |p| midi_picker(p).pick_file());
                ui.checkbox(timing, tr("Use the file's tempo and time signature"));
                ui.label(dim(
                    tr("Each part becomes a new Gloom Synth channel; the notes go into a new pattern."),
                ));
                if let Some(e) = error {
                    ui.label(RichText::new(e.as_str()).color(theme.warn));
                }
                ui.horizontal(|ui| {
                    if ui.button(tr("Import")).clicked() {
                        action = Some(DialogAction::ImportMidi {
                            path: PathBuf::from(path.trim()),
                            timing: *timing,
                        });
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        action = Some(DialogAction::Close);
                    }
                });
            }
            Dialog::ExportMidi { path, song, error } => {
                ui.label(dim(tr("Save to")));
                path_row(ui, path, |p| midi_picker(p).save_file());
                ui.horizontal(|ui| {
                    ui.selectable_value(song, true, tr("Whole song"));
                    ui.selectable_value(song, false, tr("Current pattern"));
                });
                ui.label(dim(tr("Format 1: a tempo track, then one track per channel.")));
                if let Some(e) = error {
                    ui.label(RichText::new(e.as_str()).color(theme.warn));
                }
                ui.horizontal(|ui| {
                    if ui.button(tr("Export")).clicked() {
                        let mut p = PathBuf::from(path.trim());
                        if p.extension().is_none() {
                            p.set_extension("mid");
                        }
                        action = Some(DialogAction::ExportMidi {
                            path: p,
                            song: *song,
                        });
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        action = Some(DialogAction::Close);
                    }
                });
            }
            Dialog::NewProject { templates } => {
                ui.label(dim(tr("Start from")));
                if ui
                    .add(egui::Button::new(tr("Empty project")).min_size(egui::vec2(260.0, 0.0)))
                    .clicked()
                {
                    action = Some(DialogAction::NewFrom(None));
                }
                for (name, path) in templates.iter() {
                    ui.horizontal(|ui| {
                        if ui
                            .add(egui::Button::new(name.as_str()).min_size(egui::vec2(230.0, 0.0)))
                            .on_hover_text(path.display().to_string())
                            .clicked()
                        {
                            action = Some(DialogAction::NewFrom(Some(path.clone())));
                        }
                        if ui
                            .small_button("×")
                            .on_hover_text(tr("Delete this template"))
                            .clicked()
                        {
                            action = Some(DialogAction::DeleteTemplate(path.clone()));
                        }
                    });
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new(tr("File > Save as template… adds the open project here."))
                        .small()
                        .color(theme.text_dim),
                );
                if ui.button(tr("Cancel")).clicked() {
                    action = Some(DialogAction::Close);
                }
            }
            Dialog::SaveTemplate { name, error } => {
                ui.label(dim(tr("Template name")));
                let r = ui.add(egui::TextEdit::singleline(name).desired_width(260.0));
                r.request_focus();
                ui.label(
                    RichText::new(tr("Samples are copied into the template, so it opens anywhere."))
                        .small()
                        .color(theme.text_dim),
                );
                if let Some(e) = error {
                    ui.label(RichText::new(e.as_str()).color(theme.warn));
                }
                ui.horizontal(|ui| {
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let ok = !template_name(name).is_empty();
                    if ui.add_enabled(ok, egui::Button::new(tr("Save"))).clicked() || enter && ok {
                        action = Some(DialogAction::SaveTemplate(name.clone()));
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        action = Some(DialogAction::Close);
                    }
                });
            }
            Dialog::Unsaved { name, then } => {
                ui.label(format!("Save the changes to \"{name}\"?"));
                ui.horizontal(|ui| {
                    if ui.button(tr("Save")).clicked() {
                        action = Some(DialogAction::Unsaved {
                            save: true,
                            then: *then,
                        });
                    }
                    if ui.button(tr("Don't save")).clicked() {
                        action = Some(DialogAction::Unsaved {
                            save: false,
                            then: *then,
                        });
                    }
                    if ui.button(tr("Cancel")).clicked() {
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
        ui.label(RichText::new(tr("File")).color(theme.text_dim));
        let format = f.format;
        path_row(ui, &mut f.path, |p| {
            let mut d =
                rfd::FileDialog::new().add_filter(format.label(), &[format.extension()]);
            if let Some(dir) = start_dir(p) {
                d = d.set_directory(dir);
            }
            d.save_file()
        });
        if let Some(fmt) = Format::from_path(Path::new(f.path.trim())) {
            f.format = fmt;
        }
        egui::Grid::new("export_grid")
            .num_columns(2)
            .spacing(egui::vec2(10.0, 6.0))
            .show(ui, |ui| {
                ui.label(tr("Range"));
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut f.loop_only, false, tr("Whole song"));
                    ui.selectable_value(&mut f.loop_only, true, tr("Loop region"));
                });
                ui.end_row();
                ui.label(tr("Sample rate"));
                egui::ComboBox::from_id_salt("export_rate")
                    .selected_text(format!("{} Hz", f.rate))
                    .show_ui(ui, |ui| {
                        for r in SAMPLE_RATES {
                            ui.selectable_value(&mut f.rate, r, format!("{r} Hz"));
                        }
                    });
                ui.end_row();
                ui.label(tr("Format"));
                ui.horizontal(|ui| {
                    for fmt in Format::ALL {
                        if ui
                            .add(egui::Button::selectable(f.format == fmt, fmt.label()))
                            .clicked()
                        {
                            f.set_format(fmt);
                        }
                    }
                });
                ui.end_row();
                match f.format {
                    Format::Wav | Format::Flac => {
                        ui.label(tr("Bit depth"));
                        ui.horizontal(|ui| {
                            for d in BitDepth::ALL {
                                // FLAC is integer only.
                                if f.format == Format::Wav || d != BitDepth::Float32 {
                                    ui.selectable_value(&mut f.depth, d, d.label());
                                }
                            }
                        });
                        if f.format == Format::Flac && f.depth == BitDepth::Float32 {
                            f.depth = BitDepth::Pcm24;
                        }
                    }
                    Format::Ogg => {
                        ui.label(tr("Quality"));
                        ui.add(egui::Slider::new(&mut f.ogg_quality, 0.0..=10.0).step_by(0.5))
                            .on_hover_text(tr(
                                "Variable bitrate: 3 is about 112 kbit/s, 6 about 192, 10 about 500",
                            ));
                    }
                    Format::Mp3 => {
                        ui.label(tr("Bitrate"));
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut f.mp3_vbr, false, tr("Constant"));
                            ui.selectable_value(&mut f.mp3_vbr, true, tr("Variable"));
                            if f.mp3_vbr {
                                ui.add(egui::Slider::new(&mut f.mp3_quality, 0..=9))
                                    .on_hover_text(tr(
                                        "0 is the best (about 245 kbit/s), 9 the smallest (about \
                                         65 kbit/s)",
                                    ));
                            } else {
                                egui::ComboBox::from_id_salt("mp3_kbps")
                                    .selected_text(format!("{} kbit/s", f.mp3_kbps))
                                    .show_ui(ui, |ui| {
                                        for k in MP3_BITRATES {
                                            ui.selectable_value(
                                                &mut f.mp3_kbps,
                                                k,
                                                format!("{k} kbit/s"),
                                            );
                                        }
                                    });
                            }
                        });
                    }
                }
                ui.end_row();
                ui.label("");
                ui.add_enabled(
                    !f.format.lossy() && f.depth != BitDepth::Float32,
                    egui::Checkbox::new(&mut f.dither, tr("Dither (TPDF)")),
                )
                .on_hover_text(tr(
                    "Adds a little noise before rounding to 16 or 24 bits, so quiet passages \
                     fade into noise instead of distortion",
                ));
                ui.end_row();
                ui.label("");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut f.normalize, tr("Normalize to"));
                    ui.add_enabled(
                        f.normalize,
                        egui::DragValue::new(&mut f.normalize_db)
                            .range(-24.0..=0.0)
                            .speed(0.1)
                            .suffix(tr(" dBFS")),
                    );
                });
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut f.tail, tr("Render tails"))
                    .on_hover_text(tr(
                        "Keep going after the end until reverb and delays fade out",
                    ));
                ui.end_row();
                ui.label("");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut f.stems, tr("Stems: one file per"))
                        .on_hover_text(tr(
                            "Per track: each playlist track with patterns or audio, through \
                             the whole mixer. Per insert: what each mixer insert outputs, \
                             after its effects and fader",
                        ));
                    ui.add_enabled_ui(f.stems, |ui| {
                        ui.selectable_value(&mut f.insert_stems, false, tr("track"));
                        ui.selectable_value(&mut f.insert_stems, true, tr("insert"));
                    });
                });
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut f.regions, tr("One file per marker region"))
                    .on_hover_text(tr(
                        "From each playlist marker to the next, named after the marker",
                    ));
                ui.end_row();
                ui.label(tr("Video"));
                ui.vertical(|ui| {
                    let single = !f.stems && !f.regions;
                    ui.add_enabled(
                        f.ffmpeg && single,
                        egui::Checkbox::new(&mut f.video, tr("Also make an MP4 video")),
                    )
                    .on_hover_text(tr(
                        "Made by ffmpeg next to the audio file. Not with stems or regions",
                    ));
                    if !f.ffmpeg {
                        ui.label(
                            RichText::new(tr(
                                "Needs ffmpeg: install it, or set GT_FFMPEG to its path",
                            ))
                            .small()
                            .color(theme.text_dim),
                        );
                    } else if f.video && single {
                        ui.horizontal(|ui| {
                            for (i, label) in
                                [tr("Spectrum"), tr("Waveform"), tr("Still image")]
                                    .into_iter()
                                    .enumerate()
                            {
                                ui.selectable_value(&mut f.video_style, i, label);
                            }
                        });
                        if f.video_style == 2 {
                            path_row(ui, &mut f.video_image, |p| {
                                let mut d = rfd::FileDialog::new()
                                    .add_filter(tr("Images"), &["png", "jpg", "jpeg", "bmp"]);
                                if let Some(dir) = start_dir(p) {
                                    d = d.set_directory(dir);
                                }
                                d.pick_file()
                            });
                        }
                    }
                });
                ui.end_row();
            });
    });
    if let Some(p) = &running {
        ui.add(egui::ProgressBar::new(p.fraction()).show_percentage());
        if ui.button(tr("Cancel")).clicked() {
            action = Some(DialogAction::CancelExport);
        }
    } else {
        if let Some(m) = &f.message {
            ui.label(RichText::new(m.as_str()).color(theme.text_dim));
        }
        ui.horizontal(|ui| {
            if ui.button(tr("Export")).clicked() {
                action = Some(DialogAction::Export {
                    path: PathBuf::from(f.path.trim()),
                });
            }
            if ui.button(tr("Close")).clicked() {
                action = Some(DialogAction::Close);
            }
        });
    }
    action
}

/// Where project templates are kept.
pub fn templates_folder() -> PathBuf {
    crate::library::data_folder().join("templates")
}

/// A template name made safe as a file name: path separators and other characters files
/// cannot have become spaces, and the ends are trimmed.
pub fn template_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .trim()
        .to_owned()
}

/// The templates in `dir`, by name.
pub fn list_templates(dir: &std::path::Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<(String, PathBuf)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case(gt_project::file::EXTENSION))
        })
        .filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), p)))
        .collect();
    v.sort_by_key(|(n, _)| n.to_lowercase());
    v
}

#[cfg(test)]
mod template_tests {
    use super::*;

    #[test]
    fn template_names_are_safe_and_listed_by_name() {
        assert_eq!(template_name("  Trap / 140: dark?  "), "Trap   140  dark");
        assert_eq!(template_name(".."), "");
        let dir = std::env::temp_dir().join(format!("gt-templates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["b.gloom", "A.gloom", "notes.txt"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let names: Vec<String> = list_templates(&dir).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["A", "b"]);
        assert!(list_templates(&dir.join("none")).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
