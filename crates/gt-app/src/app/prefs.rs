//! Settings: the settings window, keyboard shortcuts, the first-run wizard, the crash-report
//! notice and the performance figures.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gt_engine::EngineCommand;
use gt_ui::views::{
    audio_panel, cpu_meter, first_run, perf_panel, privacy_panel, settings_tabs, shortcut_editor,
    theme_editor, AudioPanelModel, FirstRunAction, FirstRunModel, PerfAction, PerfModel,
    SettingsPage, ThemeEdit, TransportAction,
};
use gt_ui::{Command, GloomTheme};

use super::{GloomApp, MainView, Then};
use crate::crash;
use crate::settings::{MidiPrefs, Settings, ThemePrefs};

/// How long the CPU meter's peak tick and its red dropout warning stay up.
const PEAK_HOLD: Duration = Duration::from_secs(2);
const ALARM_HOLD: Duration = Duration::from_secs(3);

/// Performance figures gathered by the UI thread.
pub(super) struct Perf {
    started: Instant,
    pub(super) renderer: &'static str,
    /// Time from launch to the first frame.
    startup_ms: Option<f32>,
    /// CPU time of recent frames (eframe's measure: building the UI plus tessellation).
    frames: VecDeque<(Instant, f32)>,
    memory_mb: Option<f32>,
    memory_read: Option<Instant>,
    /// Highest audio load recently, and when it was reached.
    peak_hold: (f32, Instant),
    /// Overloads plus xruns last seen, and how long the meter stays red.
    dropouts_seen: u32,
    alarm_until: Option<Instant>,
    /// This frame's reading (the engine's peak resets when read, so it is read once).
    last: Option<gt_ui::views::CpuLoad>,
}

impl Perf {
    pub(super) fn new(started: Instant, renderer: &'static str) -> Self {
        Self {
            started,
            renderer,
            startup_ms: None,
            frames: VecDeque::new(),
            memory_mb: None,
            memory_read: None,
            peak_hold: (0.0, Instant::now()),
            dropouts_seen: 0,
            alarm_until: None,
            last: None,
        }
    }

    /// Once per frame, with eframe's CPU time of the previous frame in seconds.
    pub(super) fn frame(&mut self, cpu_seconds: Option<f32>) {
        let now = Instant::now();
        if self.startup_ms.is_none() {
            let ms = self.started.elapsed().as_secs_f32() * 1000.0;
            log::info!("startup: {ms:.0} ms to the first frame");
            self.startup_ms = Some(ms);
        }
        if let Some(s) = cpu_seconds {
            self.frames.push_back((now, s * 1000.0));
        }
        while self
            .frames
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(1))
        {
            self.frames.pop_front();
        }
    }

    /// Average and worst frame time over the last second, in ms.
    fn frame_ms(&self) -> (f32, f32) {
        if self.frames.is_empty() {
            return (0.0, 0.0);
        }
        let sum: f32 = self.frames.iter().map(|(_, ms)| ms).sum();
        let worst = self.frames.iter().map(|(_, ms)| *ms).fold(0.0, f32::max);
        (sum / self.frames.len() as f32, worst)
    }

    /// Resident memory, read at most once a second.
    fn memory_mb(&mut self) -> Option<f32> {
        if self
            .memory_read
            .is_none_or(|t| t.elapsed() > Duration::from_secs(1))
        {
            self.memory_read = Some(Instant::now());
            self.memory_mb = resident_mb();
        }
        self.memory_mb
    }

    /// Feeds a load reading to the meter: returns the held peak and whether a dropout
    /// happened within the last few seconds.
    fn meter(&mut self, load: Option<gt_ui::views::CpuLoad>) -> (f32, bool) {
        let now = Instant::now();
        self.last = load;
        if let Some(l) = load {
            if l.peak >= self.peak_hold.0 || now.duration_since(self.peak_hold.1) > PEAK_HOLD {
                self.peak_hold = (l.peak.max(l.load), now);
            }
            let dropouts = l.overloads + l.xruns;
            if dropouts > self.dropouts_seen {
                self.alarm_until = Some(now + ALARM_HOLD);
            }
            self.dropouts_seen = dropouts;
        }
        let alarm = self.alarm_until.is_some_and(|t| now < t);
        (self.peak_hold.0, alarm)
    }
}

/// Resident set size from `/proc/self/status` (Linux only; elsewhere `None`).
fn resident_mb() -> Option<f32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb: f32 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kb / 1024.0)
}

impl GloomApp {
    /// Runs the commands whose shortcuts were pressed this frame. Called after the typing
    /// keyboard has taken its keys.
    pub(super) fn run_shortcuts(&mut self, ctx: &egui::Context) {
        if self.shortcut_state.capturing() {
            return;
        }
        let typing = ctx.text_edit_focused();
        for cmd in self.keymap.take_pressed(ctx, typing) {
            self.run_command(cmd);
        }
    }

    pub(super) fn run_command(&mut self, cmd: Command) {
        use super::FileCmd;
        let file = |c| (self.dialog.is_none()).then_some(c);
        let file_cmd = match cmd {
            Command::New => file(FileCmd::Request(Then::New)),
            Command::Open => file(FileCmd::Request(Then::Open)),
            Command::Save => file(FileCmd::Save),
            Command::SaveAs => file(FileCmd::SaveAs),
            Command::ExportAudio => file(FileCmd::Export),
            _ => None,
        };
        if let Some(c) = file_cmd {
            self.file_cmd(c);
            return;
        }
        match cmd {
            Command::PlayPause => self.on_transport(TransportAction::PlayPause),
            Command::Undo => self.undo(false),
            Command::Redo => self.undo(true),
            Command::ShowPlaylist => self.main_view = MainView::Playlist,
            Command::ShowRack => self.main_view = MainView::Rack,
            Command::ShowPianoRoll => self.main_view = MainView::PianoRoll,
            Command::ShowMixer => self.main_view = MainView::Mixer,
            Command::ToggleSongMode => {
                self.transport.song_mode = !self.transport.song_mode;
                self.push_song();
            }
            Command::ToggleTypingKeyboard => self.keyboard.on = !self.keyboard.on,
            Command::ToggleRecord => self.toggle_record(),
            Command::ToggleMetronome => {
                self.transport.metronome = !self.transport.metronome;
                self.send(EngineCommand::SetMetronome(self.transport.metronome));
            }
            Command::Settings => self.show_settings = !self.show_settings,
            Command::Plugins => self.show_plugins = !self.show_plugins,
            Command::New
            | Command::Open
            | Command::Save
            | Command::SaveAs
            | Command::ExportAudio => {}
        }
    }

    /// The menu text for `cmd`'s shortcut.
    pub(super) fn keys(&self, cmd: Command) -> String {
        self.keymap.text(cmd)
    }

    /// The audio panel's model with the live level.
    fn audio_model(&self) -> AudioPanelModel {
        AudioPanelModel {
            level_db: self.meter.level_db(),
            ..self.audio.panel_model(self.test_tone)
        }
    }

    /// The CPU meter in the transport bar; a click opens the performance page.
    pub(super) fn cpu_meter_ui(&mut self, ui: &mut egui::Ui, theme: &GloomTheme) {
        let load = self.audio.load();
        let (peak, alarm) = self.perf.meter(load);
        let tip = match load {
            Some(l) => format!(
                "Audio CPU {:.0} % (peak {:.0} %), {} late callbacks, {} device underruns. \
                 Click for details.",
                l.load * 100.0,
                peak * 100.0,
                l.overloads,
                l.xruns
            ),
            None => "No audio stream".to_owned(),
        };
        if cpu_meter(ui, theme, load, peak, alarm)
            .on_hover_text(tip)
            .clicked()
        {
            self.show_settings = true;
            self.settings_page = SettingsPage::Performance;
        }
    }

    /// The settings window: audio and MIDI, shortcuts, theme, performance, privacy.
    pub(super) fn settings_window(&mut self, ctx: &egui::Context, theme: &GloomTheme) {
        let mut open = true;
        let mut page = self.settings_page;
        egui::Window::new("Settings")
            .open(&mut open)
            .default_pos(ctx.content_rect().center() - egui::vec2(260.0, 240.0))
            .resizable(false)
            .collapsible(false)
            .default_width(520.0)
            .show(ctx, |ui| {
                settings_tabs(ui, &mut page);
                match page {
                    SettingsPage::Audio => {
                        let model = self.audio_model();
                        let action = audio_panel(ui, theme, &model);
                        ui.separator();
                        self.midi_settings(ui, theme);
                        if let Some(a) = action {
                            self.on_audio(a);
                        }
                    }
                    SettingsPage::Shortcuts => {
                        let (changed, notice) =
                            shortcut_editor(ui, theme, &mut self.keymap, &mut self.shortcut_state);
                        if changed {
                            self.settings_dirty = true;
                        }
                        if let Some(n) = notice {
                            self.toast(n);
                        }
                    }
                    SettingsPage::Theme => {
                        if theme_editor(ui, &mut self.theme) == ThemeEdit::Changed {
                            self.theme.apply(ctx);
                            self.settings_dirty = true;
                        }
                    }
                    SettingsPage::Performance => {
                        let model = PerfModel {
                            cpu: self.perf.last,
                            peak_hold: self.perf.peak_hold.0,
                            buffer_ms: self.audio.buffer_ms(),
                            frame_ms: self.perf.frame_ms(),
                            memory_mb: self.perf.memory_mb(),
                            startup_ms: self.perf.startup_ms,
                            renderer: self.perf.renderer.to_owned(),
                            realtime_denied: self.audio.realtime_denied(),
                        };
                        if perf_panel(ui, theme, &model) == Some(PerfAction::ResetCounters) {
                            if let Some(e) = self.audio.engine() {
                                e.telemetry().overloads.store(0, Ordering::Relaxed);
                                e.telemetry().xruns.store(0, Ordering::Relaxed);
                            }
                            self.perf.dropouts_seen = 0;
                        }
                        ctx.request_repaint_after(Duration::from_millis(250));
                    }
                    SettingsPage::Privacy => {
                        let folder = crash::folder().display().to_string();
                        if let Some(on) =
                            privacy_panel(ui, theme, self.settings.crash_reports, &folder)
                        {
                            self.set_crash_reports(on);
                        }
                    }
                }
            });
        self.settings_page = page;
        if page != SettingsPage::Shortcuts || !open {
            self.shortcut_state.cancel();
        }
        self.show_settings = open;
    }

    fn set_crash_reports(&mut self, on: bool) {
        self.settings.crash_reports = on;
        crash::set_enabled(on);
        self.settings_dirty = true;
    }

    /// The first-run wizard, until it is finished.
    pub(super) fn first_run_window(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.wizard.take() else {
            return;
        };
        let audio = self.audio_model();
        let model = FirstRunModel {
            audio: &audio,
            crash_reports: self.settings.crash_reports,
            plugins_found: self.plugins.catalog().plugins().len(),
            scanning: self.plugins.scanning().is_some(),
        };
        let mut theme = self.theme.clone();
        let actions = egui::Window::new("Welcome to GloomTunes Studio")
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 120.0))
            .show(ctx, |ui| first_run(ui, &mut theme, &mut state, &model))
            .and_then(|r| r.inner)
            .unwrap_or_default();
        self.wizard = Some(state);
        for a in actions {
            match a {
                FirstRunAction::Audio(a) => self.on_audio(a),
                FirstRunAction::Theme => {
                    self.theme = theme.clone();
                    self.theme.apply(ctx);
                    self.settings_dirty = true;
                }
                FirstRunAction::CrashReports(on) => self.set_crash_reports(on),
                FirstRunAction::Finish { demo } => {
                    if self.test_tone {
                        self.test_tone = false;
                        self.send(EngineCommand::SetTestTone(false));
                    }
                    if !demo {
                        self.replace_project(gt_core::Project::empty(), None);
                    }
                    self.settings.first_run_done = true;
                    self.settings_dirty = true;
                    self.wizard = None;
                }
            }
        }
        // The text-size slider edits the copy while it is dragged and applies when released;
        // keep the dragged value so the slider follows the pointer.
        self.theme.font_size = theme.font_size;
    }

    /// The notice about a crash report saved last time.
    pub(super) fn crash_window(&mut self, ctx: &egui::Context, theme: &GloomTheme) {
        let Some((path, text)) = self.crash_report.as_ref() else {
            return;
        };
        let mut close = false;
        let mut delete = false;
        let mut open_issue = false;
        let mut open_folder = false;
        egui::Window::new("GloomTunes closed unexpectedly")
            .collapsible(false)
            .default_pos(ctx.content_rect().center() - egui::vec2(280.0, 160.0))
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "A crash report was saved ({}). Nothing has been sent. To help fix \
                             it, open a GitHub issue with the report filled in: you can read \
                             and edit it there before submitting.",
                            path.display()
                        ))
                        .color(theme.text_dim),
                    )
                    .wrap(),
                );
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(text.as_str()).monospace().small(),
                            )
                            .wrap(),
                        );
                    });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    open_issue = ui
                        .button(egui::RichText::new("Report on GitHub…").color(theme.accent))
                        .clicked();
                    open_folder = ui.button("Open folder").clicked();
                    delete = ui.button("Delete report").clicked();
                    close = ui.button("Later").clicked();
                });
            });
        if open_issue {
            match crash::open_external(&crash::issue_url(text)) {
                Ok(()) => self.toast("Opened GitHub in your browser; submit the issue there"),
                Err(e) => self.toast(format!("Cannot open the browser: {e}")),
            }
        }
        if open_folder {
            if let Err(e) = crash::open_external(&crash::folder().display().to_string()) {
                self.toast(format!("Cannot open the folder: {e}"));
            }
        }
        if delete {
            for p in crash::reports(&crash::folder()) {
                let _ = std::fs::remove_file(p);
            }
            self.crash_report = None;
        }
        if close {
            self.crash_report = None;
        }
    }

    /// The newest saved crash report, if any.
    pub(super) fn find_crash_report() -> Option<(PathBuf, String)> {
        let path = crash::reports(&crash::folder()).pop()?;
        let text = std::fs::read_to_string(&path).ok()?;
        Some((path, text))
    }

    /// Copies the current preferences into `self.settings`.
    fn collect_settings(&mut self) {
        let s = &mut self.settings;
        s.audio = self.audio.prefs();
        let m = &self.midi_model;
        s.midi = MidiPrefs {
            disabled_ports: self.midi.disabled_ports(),
            count_in_bars: m.count_in_bars,
            record_click: m.record_click,
            extra_ms: m.extra_ms,
            octave: m.octave,
        };
        s.set_keymap(&self.keymap);
        s.theme = ThemePrefs::from_theme(&self.theme);
    }

    /// Saves the settings now (at exit).
    pub(super) fn save_settings_now(&mut self) {
        self.collect_settings();
        if let Err(e) = self.settings.save(&Settings::path()) {
            log::warn!("cannot save settings: {e}");
        }
    }

    /// Saves the settings when something changed and no gesture is in progress, and always
    /// when `force`. The audio and MIDI preferences are compared each time, since
    /// they change through several paths.
    pub(super) fn save_settings(&mut self, ctx: &egui::Context, force: bool) {
        let due = self.settings_checked.elapsed() > Duration::from_secs(1);
        if !force && (!due || ctx.input(|i| i.pointer.any_down())) {
            return;
        }
        self.settings_checked = Instant::now();
        let before = self.settings.clone();
        self.collect_settings();
        if force || self.settings_dirty || self.settings != before {
            self.settings_dirty = false;
            if let Err(e) = self.settings.save(&Settings::path()) {
                log::warn!("cannot save settings: {e}");
            }
        }
    }
}
