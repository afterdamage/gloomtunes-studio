//! Audio device panel: device, buffer size, sample rate, level and the device test tone, plus
//! the CPU load meter shown in the transport bar.

use egui::{Grid, RichText, Ui};

use crate::widgets::level_meter;
use crate::GloomTheme;

/// Buffer sizes offered to the user, in frames.
pub const BUFFER_SIZES: [u32; 5] = [64, 128, 256, 512, 1024];

/// Everything the panel shows. Filled in by the app from the device layer and the engine.
#[derive(Debug, Clone, Default)]
pub struct AudioPanelModel {
    /// Name of the audio host (WASAPI, ALSA, JACK, ...).
    pub host_name: String,
    /// Output device names. Index 0 is "system default".
    pub devices: Vec<String>,
    /// Index into `devices`.
    pub selected_device: usize,
    /// Requested buffer size in frames.
    pub buffer_size: u32,
    /// Standard sample rates the selected device supports.
    pub sample_rates: Vec<u32>,
    /// Requested sample rate (`None`: the device default).
    pub requested_rate: Option<u32>,
    /// Why exclusive mode is unavailable; `None` hides the row (platforms without it).
    pub exclusive_note: Option<String>,
    /// Sample rate of the running (or next) stream, if known.
    pub sample_rate: Option<u32>,
    /// Output channel count of the running stream.
    pub channels: Option<u16>,
    /// Frames delivered in the latest callback (may differ from the requested size).
    pub callback_frames: Option<u32>,
    /// True while an output stream is open.
    pub stream_open: bool,
    /// True while the test tone is on.
    pub test_tone: bool,
    /// Displayed meter level in dBFS.
    pub level_db: f32,
    /// Status or error line.
    pub status: String,
}

/// User intents raised by the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioAction {
    /// Pick a different output device (index into `devices`).
    SelectDevice(usize),
    /// Pick a different buffer size.
    SelectBufferSize(u32),
    /// Pick a sample rate (`None`: the device default).
    SelectSampleRate(Option<u32>),
    /// Turn the 440 Hz test tone on or off.
    ToggleTestTone,
    /// Close and reopen the output stream.
    RestartAudio,
    /// Re-scan output devices.
    Rescan,
}

/// Draws the panel and returns the action the user took this frame, if any.
pub fn audio_panel(ui: &mut Ui, theme: &GloomTheme, m: &AudioPanelModel) -> Option<AudioAction> {
    let mut action = None;
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);

    Grid::new("audio_panel")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(dim("Host"));
            ui.label(&m.host_name);
            ui.end_row();

            ui.label(dim("Output device"));
            ui.horizontal(|ui| {
                let current = m.devices.get(m.selected_device).map_or("", String::as_str);
                egui::ComboBox::from_id_salt("device")
                    .width(280.0)
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (i, name) in m.devices.iter().enumerate() {
                            if ui.selectable_label(i == m.selected_device, name).clicked()
                                && i != m.selected_device
                            {
                                action = Some(AudioAction::SelectDevice(i));
                            }
                        }
                    });
                if ui.small_button("Rescan").clicked() {
                    action = Some(AudioAction::Rescan);
                }
            });
            ui.end_row();

            ui.label(dim("Buffer size"));
            egui::ComboBox::from_id_salt("buffer")
                .selected_text(format!("{} frames", m.buffer_size))
                .show_ui(ui, |ui| {
                    for size in BUFFER_SIZES {
                        if ui
                            .selectable_label(size == m.buffer_size, format!("{size} frames"))
                            .clicked()
                            && size != m.buffer_size
                        {
                            action = Some(AudioAction::SelectBufferSize(size));
                        }
                    }
                });
            ui.end_row();

            ui.label(dim("Sample rate"));
            ui.horizontal(|ui| {
                let label =
                    |r: Option<u32>| r.map_or("Device default".into(), |r| format!("{r} Hz"));
                egui::ComboBox::from_id_salt("rate")
                    .selected_text(label(m.requested_rate))
                    .show_ui(ui, |ui| {
                        let options =
                            std::iter::once(None).chain(m.sample_rates.iter().copied().map(Some));
                        for r in options {
                            if ui
                                .selectable_label(r == m.requested_rate, label(r))
                                .clicked()
                                && r != m.requested_rate
                            {
                                action = Some(AudioAction::SelectSampleRate(r));
                            }
                        }
                    });
                if let Some(sr) = m.sample_rate {
                    ui.label(dim(&format!("running at {sr} Hz")));
                }
            });
            ui.end_row();

            if let Some(note) = &m.exclusive_note {
                ui.label(dim("Exclusive mode"));
                ui.add_enabled(false, egui::Checkbox::new(&mut false, "Unavailable"))
                    .on_disabled_hover_text(note);
                ui.end_row();
            }

            ui.label(dim("Callback"));
            let latency = match (m.callback_frames, m.sample_rate) {
                (Some(f), Some(sr)) if sr > 0 => {
                    format!("{f} frames ({:.1} ms)", f as f32 * 1000.0 / sr as f32)
                }
                _ => "–".into(),
            };
            let channels = m.channels.map_or(String::new(), |c| format!(", {c} ch"));
            ui.label(format!("{latency}{channels}"));
            ui.end_row();

            ui.label(dim("Level"));
            level_meter(ui, theme, m.level_db, egui::vec2(280.0, 10.0));
            ui.end_row();
        });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let tone = egui::Button::selectable(m.test_tone, "Test tone 440 Hz");
        if ui.add_enabled(m.stream_open, tone).clicked() {
            action = Some(AudioAction::ToggleTestTone);
        }
        if ui.button("Restart audio").clicked() {
            action = Some(AudioAction::RestartAudio);
        }
    });
    if !m.status.is_empty() {
        ui.add_space(4.0);
        ui.add(egui::Label::new(dim(&m.status)).wrap());
    }

    action
}

/// The audio callback's CPU load, as read from the engine.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuLoad {
    /// Smoothed load, 1.0 = the whole buffer time.
    pub load: f32,
    /// Worst single callback recently.
    pub peak: f32,
    /// Callbacks that took longer than their audio (likely dropouts) since the stream opened.
    pub overloads: u32,
    /// Underruns reported by the audio backend since the stream opened.
    pub xruns: u32,
}

/// Load at which the meter turns to the warning colour.
pub const CPU_WARN: f32 = 0.8;

/// A compact CPU meter for the transport bar: a bar with the smoothed load and a peak tick,
/// red while a dropout is recent (`alarm`). Returns the response so the app can open
/// the performance panel when it is clicked.
pub fn cpu_meter(
    ui: &mut Ui,
    theme: &GloomTheme,
    load: Option<CpuLoad>,
    peak_hold: f32,
    alarm: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(64.0, 16.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, theme.radius, theme.bg_deep);
    let text = match load {
        Some(l) => {
            let over = l.load >= CPU_WARN || alarm;
            let fill = if over { theme.warn } else { theme.accent_dim };
            let w = rect.width() * l.load.clamp(0.0, 1.0);
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min, egui::vec2(w, rect.height())),
                theme.radius,
                fill,
            );
            let x = rect.left() + rect.width() * peak_hold.clamp(0.0, 1.0);
            painter.vline(x, rect.y_range(), egui::Stroke::new(1.0, theme.text_dim));
            format!("CPU {:.0}%", l.load * 100.0)
        }
        None => "CPU –".to_owned(),
    };
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(theme.font_size - 2.0),
        theme.text,
    );
    painter.rect_stroke(
        rect,
        theme.radius,
        egui::Stroke::new(1.0, theme.stroke),
        egui::StrokeKind::Inside,
    );
    resp
}
