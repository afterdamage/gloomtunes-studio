//! Audio input and recording settings (Prompt 13): the input device, which of its channels
//! are offered as mono inputs or stereo pairs, monitoring, latency compensation and the
//! loopback measurement. Shown on the Audio and MIDI page under the output settings.

use egui::{Grid, RichText, Ui};
use gt_core::{AudioInput, MonitorMode};

use crate::widgets::level_meter;
use crate::GloomTheme;

/// How a pair of device channels (1-2, 3-4, ...) is offered to tracks and inserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PairMode {
    /// Both channels on their own and the pair together.
    #[default]
    Both,
    /// Two mono inputs.
    Mono,
    /// One stereo input.
    Stereo,
    /// Not offered.
    Off,
}

impl PairMode {
    /// Every mode, in menu order.
    pub const ALL: [Self; 4] = [Self::Both, Self::Mono, Self::Stereo, Self::Off];

    /// Menu name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Both => "Mono and stereo",
            Self::Mono => "Two mono",
            Self::Stereo => "Stereo pair",
            Self::Off => "Off",
        }
    }

    /// Settings-file key.
    pub fn key(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Mono => "mono",
            Self::Stereo => "stereo",
            Self::Off => "off",
        }
    }

    /// Parses [`PairMode::key`]; unknown text gives the default.
    pub fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|m| m.key() == key)
            .unwrap_or_default()
    }
}

/// The inputs offered for a device with `channels` channels, pairs set up as `modes` (missing
/// entries use the default). A last odd channel is a mono input unless its pair is off.
pub fn offered_inputs(channels: usize, modes: &[PairMode]) -> Vec<AudioInput> {
    let channels = channels.min(gt_core::MAX_INPUT_CHANNELS);
    let mut out = Vec::new();
    for pair in 0..channels.div_ceil(2) {
        let first = (pair * 2) as u8;
        let mode = modes.get(pair).copied().unwrap_or_default();
        let has_second = pair * 2 + 1 < channels;
        if !has_second {
            if mode != PairMode::Off {
                out.push(AudioInput::mono(first));
            }
            continue;
        }
        if matches!(mode, PairMode::Both | PairMode::Mono) {
            out.push(AudioInput::mono(first));
            out.push(AudioInput::mono(first + 1));
        }
        if matches!(mode, PairMode::Both | PairMode::Stereo) {
            out.push(AudioInput::stereo(first));
        }
    }
    out
}

/// A menu of `offered` inputs plus "No input" for `current`. An input the device does not
/// offer (another interface) stays selectable and is marked. Returns true when changed.
pub fn input_menu(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    width: f32,
    current: &mut Option<AudioInput>,
    offered: &[AudioInput],
) -> bool {
    let label = |i: Option<AudioInput>| match i {
        None => "No input".to_owned(),
        Some(i) if offered.contains(&i) => i.label(),
        Some(i) => format!("{} (missing)", i.label()),
    };
    let before = *current;
    egui::ComboBox::from_id_salt(id)
        .width(width)
        .selected_text(label(*current))
        .show_ui(ui, |ui| {
            ui.selectable_value(current, None, "No input");
            if let Some(c) = before.filter(|c| !offered.contains(c)) {
                ui.selectable_value(current, Some(c), label(Some(c)));
            }
            for &i in offered {
                ui.selectable_value(current, Some(i), i.label());
            }
        });
    *current != before
}

/// What the panel shows.
#[derive(Debug, Clone, Default)]
pub struct InputPanelModel {
    /// Input device names; index 0 is "No input".
    pub devices: Vec<String>,
    pub selected_device: usize,
    /// Channels of the open input stream (0: none open).
    pub channels: usize,
    /// Mode per channel pair.
    pub pairs: Vec<PairMode>,
    /// Level per input channel, dBFS.
    pub levels_db: Vec<f32>,
    pub monitor: MonitorMode,
    /// Latency the devices report, output plus input plus the engine's cushion, in ms.
    pub reported_ms: Option<f32>,
    /// Manual offset added to it, in ms (the calibration sets it).
    pub offset_ms: f32,
    /// Input channel the loopback measurement listens on.
    pub calibration_channel: usize,
    /// True while measuring.
    pub calibrating: bool,
    /// Times the input ran dry or was realigned since the stream opened.
    pub slips: u32,
    /// True when output and input are different devices (separate clocks).
    pub two_devices: bool,
    /// Status or result line.
    pub status: String,
}

/// User intents.
#[derive(Debug, Clone, PartialEq)]
pub enum InputAction {
    /// Pick an input device (0: none).
    SelectDevice(usize),
    /// Set how a channel pair is offered.
    SetPair(usize, PairMode),
    SetMonitor(MonitorMode),
    SetOffset(f32),
    SetCalibrationChannel(usize),
    /// Start (or cancel) the loopback measurement.
    Calibrate,
}

/// Draws the panel and returns what the user did.
pub fn input_panel(ui: &mut Ui, theme: &GloomTheme, m: &InputPanelModel) -> Option<InputAction> {
    let mut action = None;
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    ui.label(RichText::new("Audio input and recording").strong());
    Grid::new("input_panel")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(dim("Input device"));
            let current = m.devices.get(m.selected_device).map_or("", String::as_str);
            egui::ComboBox::from_id_salt("in_device")
                .width(280.0)
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (i, name) in m.devices.iter().enumerate() {
                        if ui.selectable_label(i == m.selected_device, name).clicked()
                            && i != m.selected_device
                        {
                            action = Some(InputAction::SelectDevice(i));
                        }
                    }
                });
            ui.end_row();

            if m.channels > 0 {
                ui.label(dim("Channels"));
                ui.vertical(|ui| {
                    for pair in 0..m.channels.div_ceil(2) {
                        ui.horizontal(|ui| {
                            let a = pair * 2;
                            let label = if a + 1 < m.channels {
                                format!("{}-{}", a + 1, a + 2)
                            } else {
                                format!("{}", a + 1)
                            };
                            ui.label(RichText::new(label).monospace());
                            let mode = m.pairs.get(pair).copied().unwrap_or_default();
                            egui::ComboBox::from_id_salt(("in_pair", pair))
                                .width(130.0)
                                .selected_text(mode.name())
                                .show_ui(ui, |ui| {
                                    for p in PairMode::ALL {
                                        if ui.selectable_label(p == mode, p.name()).clicked()
                                            && p != mode
                                        {
                                            action = Some(InputAction::SetPair(pair, p));
                                        }
                                    }
                                });
                            for c in a..(a + 2).min(m.channels) {
                                let db = m.levels_db.get(c).copied().unwrap_or(f32::NEG_INFINITY);
                                level_meter(ui, theme, db, egui::vec2(60.0, 8.0));
                            }
                        });
                    }
                });
                ui.end_row();
            }

            ui.label(dim("Monitoring"));
            egui::ComboBox::from_id_salt("in_monitor")
                .selected_text(m.monitor.name())
                .show_ui(ui, |ui| {
                    for mode in MonitorMode::ALL {
                        if ui
                            .selectable_label(mode == m.monitor, mode.name())
                            .clicked()
                        {
                            action = Some(InputAction::SetMonitor(mode));
                        }
                    }
                })
                .response
                .on_hover_text(
                    "How armed tracks hear their input. Auto: while stopped and while \
                     recording, not while playing a take back. Off: use your interface's \
                     direct monitoring.",
                );
            ui.end_row();

            ui.label(dim("Latency"));
            ui.horizontal(|ui| {
                let reported = m
                    .reported_ms
                    .map_or("–".to_owned(), |ms| format!("{ms:.1} ms reported"));
                ui.label(reported);
                let mut off = m.offset_ms;
                let r = ui
                    .add(
                        egui::DragValue::new(&mut off)
                            .range(-200.0..=200.0)
                            .speed(0.1)
                            .prefix("+ ")
                            .suffix(" ms"),
                    )
                    .on_hover_text(
                        "Added to the reported latency when placing recordings. The loopback \
                         measurement sets it.",
                    );
                if r.changed() {
                    action = Some(InputAction::SetOffset(off));
                }
            });
            ui.end_row();

            ui.label(dim("Measure"));
            ui.horizontal(|ui| {
                let mut ch = m.calibration_channel;
                egui::ComboBox::from_id_salt("in_calib")
                    .width(70.0)
                    .selected_text(format!("In {}", ch + 1))
                    .show_ui(ui, |ui| {
                        for c in 0..m.channels.max(1) {
                            ui.selectable_value(&mut ch, c, format!("In {}", c + 1));
                        }
                    });
                if ch != m.calibration_channel {
                    action = Some(InputAction::SetCalibrationChannel(ch));
                }
                let label = if m.calibrating {
                    "Cancel"
                } else {
                    "Loopback test"
                };
                if ui
                    .add_enabled(m.channels > 0, egui::Button::new(label))
                    .on_hover_text(
                        "Connect an output to this input with a cable (or hold the microphone \
                         to a speaker), then press: eight clicks are played and timed to set \
                         the offset.",
                    )
                    .clicked()
                {
                    action = Some(InputAction::Calibrate);
                }
            });
            ui.end_row();
        });
    if m.two_devices && m.channels > 0 {
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Input and output are different devices with their own clocks: \
                     GloomTunes keeps them aligned by skipping or repeating input, which can \
                     click. Use one device (or an aggregate device) for clean recordings.",
                )
                .color(theme.warn),
            )
            .wrap(),
        );
    }
    let mut status = m.status.clone();
    if m.slips > 0 {
        if !status.is_empty() {
            status.push_str("  ");
        }
        status.push_str(&format!("Input realigned {} times.", m.slips));
    }
    if !status.is_empty() {
        ui.add(egui::Label::new(dim(&status)).wrap());
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_offer_the_chosen_inputs() {
        let labels = |ch, modes: &[PairMode]| {
            offered_inputs(ch, modes)
                .into_iter()
                .map(AudioInput::label)
                .collect::<Vec<_>>()
        };
        assert_eq!(labels(2, &[]), ["In 1", "In 2", "In 1-2"]);
        assert_eq!(labels(1, &[]), ["In 1"]);
        assert_eq!(
            labels(5, &[PairMode::Mono, PairMode::Stereo, PairMode::Both]),
            ["In 1", "In 2", "In 3-4", "In 5"]
        );
        assert_eq!(
            labels(4, &[PairMode::Off, PairMode::Off]),
            Vec::<String>::new()
        );
        assert!(labels(0, &[]).is_empty());
        for k in PairMode::ALL {
            assert_eq!(PairMode::from_key(k.key()), k);
        }
    }
}
