//! Audio device panel: device, buffer size, sample rate and the test-tone switch.

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
    /// Sample rate of the running (or next) stream, if known.
    pub sample_rate: Option<u32>,
    /// Output channel count of the running stream.
    pub channels: Option<u16>,
    /// Frames delivered in the latest callback (may differ from the requested size).
    pub callback_frames: Option<u32>,
    /// True while the tone plays (or is fading in).
    pub running: bool,
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
    /// Start the test tone.
    Start,
    /// Stop the test tone.
    Stop,
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
            ui.label(m.sample_rate.map_or("–".into(), |sr| format!("{sr} Hz")));
            ui.end_row();

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
        let (label, act) = if m.running {
            ("■  Stop", AudioAction::Stop)
        } else {
            ("▶  Play 440 Hz", AudioAction::Start)
        };
        let button = egui::Button::new(RichText::new(label).color(if m.running {
            theme.accent
        } else {
            theme.text
        }))
        .min_size(egui::vec2(120.0, 24.0));
        if ui.add(button).clicked() {
            action = Some(act);
        }
    });
    if !m.status.is_empty() {
        ui.add_space(4.0);
        ui.add(egui::Label::new(dim(&m.status)).wrap());
    }

    action
}
