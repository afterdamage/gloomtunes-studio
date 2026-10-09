//! MIDI and recording settings: input devices, count-in, metronome while recording, latency
//! compensation and the typing keyboard's octave.

use egui::{Grid, RichText, Ui};

use crate::GloomTheme;

/// One MIDI input port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiPortRow {
    /// Port name.
    pub name: String,
    /// The user wants it connected.
    pub enabled: bool,
    /// It is connected now.
    pub connected: bool,
}

/// Everything the panel shows and edits.
#[derive(Debug, Clone, PartialEq)]
pub struct MidiPanelModel {
    /// Input ports from the last scan.
    pub ports: Vec<MidiPortRow>,
    /// Why MIDI is unavailable, if it is.
    pub error: Option<String>,
    /// Bars counted in before recording from stop (0 to 2).
    pub count_in_bars: u8,
    /// Metronome on while recording, whatever the Click button says.
    pub record_click: bool,
    /// Output buffer latency (recorded notes move earlier by this), in ms.
    pub buffer_ms: f32,
    /// Extra latency compensation set by the user, in ms (may be negative).
    pub extra_ms: f32,
    /// Octave of the typing keyboard's bottom-row C.
    pub octave: i32,
}

impl Default for MidiPanelModel {
    fn default() -> Self {
        Self {
            ports: Vec::new(),
            error: None,
            count_in_bars: 1,
            record_click: true,
            buffer_ms: 0.0,
            extra_ms: 0.0,
            octave: 3,
        }
    }
}

/// User intents the model cannot hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiAction {
    /// Connect (true) or disconnect a port, by name.
    SetPortEnabled(String, bool),
}

/// Draws the panel; edits `m` in place.
pub fn midi_panel(ui: &mut Ui, theme: &GloomTheme, m: &mut MidiPanelModel) -> Vec<MidiAction> {
    let mut actions = Vec::new();
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    ui.label(RichText::new("MIDI inputs").strong());
    if let Some(e) = &m.error {
        ui.label(RichText::new(e.as_str()).color(theme.warn));
    } else if m.ports.is_empty() {
        ui.label(dim(
            "No MIDI inputs found. Plug one in: it is picked up within a second.",
        ));
    }
    for p in &mut m.ports {
        ui.horizontal(|ui| {
            if ui.checkbox(&mut p.enabled, p.name.as_str()).changed() {
                actions.push(MidiAction::SetPortEnabled(p.name.clone(), p.enabled));
            }
            if p.enabled && !p.connected {
                ui.label(RichText::new("cannot open").color(theme.warn));
            }
        });
    }
    ui.label(dim(
        "Notes play the selected channel. Right-click a knob or fader for MIDI learn.",
    ));
    ui.add_space(6.0);
    ui.label(RichText::new("Recording").strong());
    Grid::new("midi_panel")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(dim("Count-in"));
            ui.horizontal(|ui| {
                for (bars, label) in [(0, "Off"), (1, "1 bar"), (2, "2 bars")] {
                    ui.selectable_value(&mut m.count_in_bars, bars, label);
                }
            });
            ui.end_row();
            ui.label(dim("Metronome"));
            ui.checkbox(&mut m.record_click, "Click while recording");
            ui.end_row();
            ui.label(dim("Latency"));
            ui.horizontal(|ui| {
                ui.label(format!("output buffer {:.1} ms +", m.buffer_ms));
                ui.add(
                    egui::DragValue::new(&mut m.extra_ms)
                        .range(-100.0..=500.0)
                        .speed(0.5)
                        .suffix(" ms"),
                );
            })
            .response
            .on_hover_text(
                "Recorded notes move earlier by this much, because you hear (and play along \
                 with) the music this late. Add your interface's extra latency if notes land \
                 late",
            );
            ui.end_row();
            ui.label(dim("Typing keyboard"));
            ui.horizontal(|ui| {
                ui.label("bottom row starts at C");
                ui.add(
                    egui::DragValue::new(&mut m.octave)
                        .range(-1..=7)
                        .speed(0.05),
                );
            });
            ui.end_row();
        });
    actions
}
