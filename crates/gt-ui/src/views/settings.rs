//! The settings window's pages that are not about devices: keyboard shortcuts, the theme,
//! performance figures and privacy, plus the first-run wizard.

use egui::{Grid, Key, RichText, Ui};

use crate::keymap::{format_shortcut, shortcut_from_press, Command, Keymap};
use crate::theme::{color_to_hex, FONT_SIZE_RANGE, MAX_RADIUS};
use crate::views::{audio_panel, AudioAction, AudioPanelModel, CpuLoad, CPU_WARN};
use crate::GloomTheme;

/// Pages of the settings window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsPage {
    /// Audio device and MIDI.
    #[default]
    Audio,
    /// Keyboard shortcuts.
    Shortcuts,
    /// Colours and sizes.
    Theme,
    /// CPU, frame time, memory.
    Performance,
    /// Crash reports.
    Privacy,
}

impl SettingsPage {
    /// Every page, in tab order.
    pub const ALL: [SettingsPage; 5] = [
        SettingsPage::Audio,
        SettingsPage::Shortcuts,
        SettingsPage::Theme,
        SettingsPage::Performance,
        SettingsPage::Privacy,
    ];

    /// Tab label.
    pub fn label(self) -> &'static str {
        match self {
            SettingsPage::Audio => "Audio and MIDI",
            SettingsPage::Shortcuts => "Shortcuts",
            SettingsPage::Theme => "Theme",
            SettingsPage::Performance => "Performance",
            SettingsPage::Privacy => "Privacy",
        }
    }
}

/// Tabs along the top of the settings window.
pub fn settings_tabs(ui: &mut Ui, page: &mut SettingsPage) {
    ui.horizontal(|ui| {
        for p in SettingsPage::ALL {
            if ui.selectable_label(*page == p, p.label()).clicked() {
                *page = p;
            }
        }
    });
    ui.separator();
}

// ---------------------------------------------------------------------------------------------
// Shortcuts

/// Shortcut editor state: the command waiting for a key, and the filter text.
#[derive(Debug, Clone, Default)]
pub struct ShortcutEditorState {
    capturing: Option<Command>,
    filter: String,
}

impl ShortcutEditorState {
    /// True while the editor waits for a key press: the app must not run shortcuts then.
    pub fn capturing(&self) -> bool {
        self.capturing.is_some()
    }

    /// Stops waiting for a key (the window was closed).
    pub fn cancel(&mut self) {
        self.capturing = None;
    }
}

/// Lists every command with its shortcuts. "Set" waits for the next key press (Escape
/// cancels). Returns true when the keymap changed, and a notice when a shortcut moved from
/// another command.
pub fn shortcut_editor(
    ui: &mut Ui,
    theme: &GloomTheme,
    keymap: &mut Keymap,
    state: &mut ShortcutEditorState,
) -> (bool, Option<String>) {
    let mut changed = false;
    let mut notice = None;

    if let Some(cmd) = state.capturing {
        let press = ui.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => Some((*key, *modifiers)),
                _ => None,
            })
        });
        if let Some((key, modifiers)) = press {
            ui.input_mut(|i| i.consume_key(modifiers, key));
            state.capturing = None;
            if key != Key::Escape || !modifiers.is_none() {
                let s = shortcut_from_press(modifiers, key);
                if let Some(from) = keymap.bind(cmd, s) {
                    notice = Some(format!(
                        "{} now runs {} (it was {})",
                        format_shortcut(&s),
                        cmd.label(),
                        from.label()
                    ));
                }
                changed = true;
            }
        }
    }

    ui.horizontal(|ui| {
        ui.label(RichText::new("Filter").color(theme.text_dim));
        ui.add(egui::TextEdit::singleline(&mut state.filter).desired_width(160.0));
        if ui.button("Reset all").clicked() {
            *keymap = Keymap::default();
            changed = true;
        }
    });
    ui.add_space(4.0);
    let filter = state.filter.to_lowercase();
    egui::ScrollArea::vertical()
        .max_height(320.0)
        .show(ui, |ui| {
            Grid::new("shortcuts")
                .num_columns(3)
                .striped(true)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for cmd in Command::ALL {
                        if !filter.is_empty() && !cmd.label().to_lowercase().contains(&filter) {
                            continue;
                        }
                        ui.label(cmd.label());
                        let keys: Vec<String> =
                            keymap.get(cmd).iter().map(format_shortcut).collect();
                        let text = if state.capturing == Some(cmd) {
                            RichText::new("Press a key… (Esc cancels)").color(theme.accent)
                        } else if keys.is_empty() {
                            RichText::new("none").color(theme.text_dim)
                        } else {
                            RichText::new(keys.join(", ")).monospace()
                        };
                        ui.label(text);
                        ui.horizontal(|ui| {
                            if ui.small_button("Set").clicked() {
                                state.capturing = Some(cmd);
                            }
                            if ui
                                .add_enabled(!keys.is_empty(), egui::Button::new("Clear").small())
                                .clicked()
                            {
                                keymap.clear(cmd);
                                changed = true;
                            }
                            let is_default = keymap.get(cmd) == cmd.defaults().as_slice();
                            if ui
                                .add_enabled(!is_default, egui::Button::new("Default").small())
                                .clicked()
                            {
                                keymap.reset(cmd);
                                changed = true;
                            }
                        });
                        ui.end_row();
                    }
                });
        });
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "Keys inside the piano roll, playlist and rack (tools, arrows, Delete) and the \
             typing keyboard's notes are fixed.",
        )
        .color(theme.text_dim)
        .small(),
    );
    (changed, notice)
}

// ---------------------------------------------------------------------------------------------
// Theme

/// What the theme editor changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeEdit {
    /// Nothing.
    None,
    /// A value changed: apply and save.
    Changed,
}

/// Accent presets, every colour token and the two sizes. Edits `theme` in place.
pub fn theme_editor(ui: &mut Ui, theme: &mut GloomTheme) -> ThemeEdit {
    let mut changed = false;
    ui.label(RichText::new("Accent").color(theme.text_dim));
    if accent_swatches(ui, theme) {
        changed = true;
    }
    ui.add_space(6.0);
    let dim = theme.text_dim;
    Grid::new("theme_colors")
        .num_columns(4)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            let mut accent_edited = None;
            for (i, (name, color)) in theme.colors_mut().into_iter().enumerate() {
                ui.label(RichText::new(name.replace('_', " ")).color(dim));
                ui.horizontal(|ui| {
                    if ui.color_edit_button_srgba(color).changed() {
                        changed = true;
                        if name == "accent" {
                            accent_edited = Some(*color);
                        }
                    }
                    ui.label(RichText::new(color_to_hex(*color)).monospace().small());
                });
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
            if let Some(c) = accent_edited {
                theme.set_accent(c);
            }
        });
    ui.add_space(6.0);
    Grid::new("theme_sizes")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            ui.label(RichText::new("Text size").color(dim));
            changed |= ui
                .add(
                    egui::Slider::new(&mut theme.font_size, FONT_SIZE_RANGE.0..=FONT_SIZE_RANGE.1)
                        .step_by(0.5)
                        .fixed_decimals(1)
                        .suffix(" pt"),
                )
                .changed();
            ui.end_row();
            ui.label(RichText::new("Corner radius").color(dim));
            changed |= ui
                .add(egui::Slider::new(&mut theme.radius, 0..=MAX_RADIUS).suffix(" pt"))
                .changed();
            ui.end_row();
        });
    ui.add_space(6.0);
    if ui.button("Reset to Gloom").clicked() {
        *theme = GloomTheme::gloom();
        changed = true;
    }
    if changed {
        ThemeEdit::Changed
    } else {
        ThemeEdit::None
    }
}

/// A row of accent swatches; returns true when one was picked.
fn accent_swatches(ui: &mut Ui, theme: &mut GloomTheme) -> bool {
    let mut picked = None;
    ui.horizontal(|ui| {
        for (name, color) in GloomTheme::ACCENTS {
            let selected = theme.accent == color;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 18.0), egui::Sense::click());
            ui.painter().rect_filled(rect, theme.radius, color);
            if selected {
                ui.painter().rect_stroke(
                    rect.expand(2.0),
                    theme.radius,
                    egui::Stroke::new(1.5, theme.text),
                    egui::StrokeKind::Outside,
                );
            }
            if resp.on_hover_text(name).clicked() {
                picked = Some(color);
            }
        }
    });
    if let Some(c) = picked {
        theme.set_accent(c);
        true
    } else {
        false
    }
}

// ---------------------------------------------------------------------------------------------
// Performance

/// Figures for the performance page.
#[derive(Debug, Clone, Default)]
pub struct PerfModel {
    /// Audio callback load; `None` without a stream.
    pub cpu: Option<CpuLoad>,
    /// Highest load over the last few seconds.
    pub peak_hold: f32,
    /// Device buffer in ms.
    pub buffer_ms: Option<f32>,
    /// UI frame build time, average and worst over the last second, in ms.
    pub frame_ms: (f32, f32),
    /// Resident memory in MB, where the platform reports it.
    pub memory_mb: Option<f32>,
    /// Time from launch to the first frame, in ms.
    pub startup_ms: Option<f32>,
    /// Renderer in use (wgpu or OpenGL).
    pub renderer: String,
    /// The audio thread was refused real-time priority.
    pub realtime_denied: bool,
}

/// User intents from the performance page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerfAction {
    /// Zero the overload and xrun counters.
    ResetCounters,
}

/// The performance page.
pub fn perf_panel(ui: &mut Ui, theme: &GloomTheme, m: &PerfModel) -> Option<PerfAction> {
    let mut action = None;
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    Grid::new("perf")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(dim("Audio CPU"));
            match m.cpu {
                Some(c) => {
                    let color = if c.load >= CPU_WARN {
                        theme.warn
                    } else {
                        theme.text
                    };
                    ui.label(
                        RichText::new(format!(
                            "{:.1} % now, {:.1} % peak (of the buffer time)",
                            c.load * 100.0,
                            m.peak_hold * 100.0
                        ))
                        .color(color),
                    );
                }
                None => {
                    ui.label("no audio stream");
                }
            }
            ui.end_row();

            ui.label(dim("Dropouts"));
            ui.horizontal(|ui| {
                let (over, xruns) = m.cpu.map_or((0, 0), |c| (c.overloads, c.xruns));
                let color = if over + xruns > 0 {
                    theme.warn
                } else {
                    theme.text
                };
                ui.label(
                    RichText::new(format!("{over} late callbacks, {xruns} device underruns"))
                        .color(color),
                );
                if ui.small_button("Reset").clicked() {
                    action = Some(PerfAction::ResetCounters);
                }
            });
            ui.end_row();

            ui.label(dim("Buffer"));
            ui.label(m.buffer_ms.map_or("–".into(), |ms| format!("{ms:.1} ms")));
            ui.end_row();

            ui.label(dim("Priority"));
            if m.realtime_denied {
                ui.label(
                    RichText::new("real-time priority refused by the system").color(theme.warn),
                )
                .on_hover_text(
                    "Audio still plays, but other programs can interrupt it. On Linux, \
                         add yourself to the audio group (or install rtkit); see README.",
                );
            } else {
                ui.label("real-time requested");
            }
            ui.end_row();

            ui.label(dim("UI frame"));
            ui.label(format!(
                "{:.1} ms average, {:.1} ms worst (last second)",
                m.frame_ms.0, m.frame_ms.1
            ));
            ui.end_row();

            ui.label(dim("Memory"));
            ui.label(
                m.memory_mb
                    .map_or("not reported on this platform".into(), |mb| {
                        format!("{mb:.0} MB resident")
                    }),
            );
            ui.end_row();

            ui.label(dim("Startup"));
            ui.label(
                m.startup_ms
                    .map_or("–".into(), |ms| format!("{ms:.0} ms to the first frame")),
            );
            ui.end_row();

            ui.label(dim("Renderer"));
            ui.label(&m.renderer);
            ui.end_row();
        });
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Late callbacks took longer than the audio they produced; each one is likely an \
             audible click. If they appear, raise the buffer size in Audio and MIDI.",
        )
        .color(theme.text_dim)
        .small(),
    );
    action
}

// ---------------------------------------------------------------------------------------------
// Privacy

/// The privacy page: the crash-report switch and where reports go. Returns the new value
/// when it changed.
pub fn privacy_panel(ui: &mut Ui, theme: &GloomTheme, enabled: bool, folder: &str) -> Option<bool> {
    let mut on = enabled;
    ui.checkbox(
        &mut on,
        "Save a crash report when GloomTunes closes unexpectedly",
    );
    ui.add_space(4.0);
    ui.add(
        egui::Label::new(
            RichText::new(format!(
                "Off by default. A report is a text file with the error message, where in the \
                 code it happened, the app version and your operating system. It holds no \
                 audio or project contents, though an error message can name a file. It stays \
                 on this computer ({folder}). At the next start you can read it and choose to \
                 open a pre-filled GitHub issue in your browser. Nothing is ever sent \
                 automatically."
            ))
            .color(theme.text_dim),
        )
        .wrap(),
    );
    (on != enabled).then_some(on)
}

// ---------------------------------------------------------------------------------------------
// First-run wizard

/// The wizard's page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FirstRunState {
    /// 0: audio, 1: look, 2: privacy, 3: ready.
    pub page: usize,
}

/// What the wizard shows besides the audio panel.
#[derive(Debug, Clone)]
pub struct FirstRunModel<'a> {
    /// The audio device panel's model.
    pub audio: &'a AudioPanelModel,
    /// Crash reports on.
    pub crash_reports: bool,
    /// Plugins found so far, and whether a scan is running.
    pub plugins_found: usize,
    /// A plugin scan is running.
    pub scanning: bool,
}

/// User intents from the wizard.
#[derive(Debug, Clone, PartialEq)]
pub enum FirstRunAction {
    /// The audio panel on page 1 did something.
    Audio(AudioAction),
    /// The theme changed (accent or text size).
    Theme,
    /// Crash reports switched.
    CrashReports(bool),
    /// Done: keep the demo song (true) or start empty.
    Finish {
        /// Keep the demo song open.
        demo: bool,
    },
}

/// Pages of the wizard. Every choice applies at once (the test tone must sound on the chosen
/// device); "Skip" keeps the defaults.
pub fn first_run(
    ui: &mut Ui,
    theme: &mut GloomTheme,
    state: &mut FirstRunState,
    m: &FirstRunModel<'_>,
) -> Vec<FirstRunAction> {
    let mut out = Vec::new();
    let pages = ["Sound", "Look", "Privacy", "Ready"];
    ui.horizontal(|ui| {
        for (i, p) in pages.iter().enumerate() {
            let color = if i == state.page {
                theme.accent
            } else {
                theme.text_dim
            };
            ui.label(RichText::new(format!("{}. {p}", i + 1)).color(color));
        }
    });
    ui.separator();
    let dim = theme.text_dim;
    let text = |ui: &mut Ui, s: &str| {
        ui.add(egui::Label::new(RichText::new(s).color(dim)).wrap());
    };
    match state.page {
        0 => {
            text(
                ui,
                "Welcome to GloomTunes Studio. Pick the output you want to hear and press Test \
                 tone. A smaller buffer means less delay when you play; raise it if you hear \
                 clicks.",
            );
            ui.add_space(6.0);
            if let Some(a) = audio_panel(ui, theme, m.audio) {
                out.push(FirstRunAction::Audio(a));
            }
        }
        1 => {
            text(
                ui,
                "Choose an accent colour and a text size. Settings > Theme has the rest.",
            );
            ui.add_space(6.0);
            if accent_swatches(ui, theme) {
                out.push(FirstRunAction::Theme);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Text size").color(dim));
                let r = ui.add(
                    egui::Slider::new(&mut theme.font_size, FONT_SIZE_RANGE.0..=FONT_SIZE_RANGE.1)
                        .step_by(0.5)
                        .fixed_decimals(1)
                        .suffix(" pt"),
                );
                // Applied when the drag ends, so the slider does not move under the pointer.
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    out.push(FirstRunAction::Theme);
                }
            });
        }
        2 => {
            if let Some(on) =
                privacy_panel(ui, theme, m.crash_reports, "the GloomTunes data folder")
            {
                out.push(FirstRunAction::CrashReports(on));
            }
        }
        _ => {
            let plugins = if m.scanning {
                format!(
                    "Looking for CLAP plugins… {} found so far.",
                    m.plugins_found
                )
            } else {
                format!(
                    "{} CLAP plugin{} found. Add folders in the plugin browser ({}+P).",
                    m.plugins_found,
                    if m.plugins_found == 1 { "" } else { "s" },
                    crate::keymap::COMMAND_KEY
                )
            };
            text(ui, &plugins);
            ui.add_space(4.0);
            text(
                ui,
                "The demo song shows the channel rack, piano roll, playlist and mixer at work. \
                 Press Space to play it. Every setting from this wizard is in Settings.",
            );
        }
    }
    ui.add_space(8.0);
    ui.separator();
    ui.horizontal(|ui| {
        if state.page > 0 && ui.button("Back").clicked() {
            state.page -= 1;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if state.page + 1 < pages.len() {
                if ui
                    .button(RichText::new("Next").color(theme.accent))
                    .clicked()
                {
                    state.page += 1;
                }
                if ui
                    .button(RichText::new("Skip").color(theme.text_dim))
                    .on_hover_text("Keep the defaults and open the demo song")
                    .clicked()
                {
                    out.push(FirstRunAction::Finish { demo: true });
                }
            } else {
                if ui
                    .button(RichText::new("Open the demo song").color(theme.accent))
                    .clicked()
                {
                    out.push(FirstRunAction::Finish { demo: true });
                }
                if ui.button("Start empty").clicked() {
                    out.push(FirstRunAction::Finish { demo: false });
                }
            }
        });
    });
    out
}
