//! MIDI and recording settings: input devices, input routing, count-in, metronome while
//! recording, latency compensation and the typing keyboard's octave.

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

/// What recording does to the notes already in the pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordMode {
    /// New notes join the old ones.
    #[default]
    Overdub,
    /// Where the playhead passes while recording, the recording channels' notes and
    /// controller data are cleared first.
    Replace,
}

impl RecordMode {
    /// Name in settings files.
    pub fn key(self) -> &'static str {
        match self {
            Self::Overdub => "overdub",
            Self::Replace => "replace",
        }
    }

    /// The mode named `key` (overdub for anything else).
    pub fn from_key(key: &str) -> Self {
        if key == "replace" {
            Self::Replace
        } else {
            Self::Overdub
        }
    }
}

/// What each pass around a looping pattern does while recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopPasses {
    /// Passes add up in the pattern.
    #[default]
    Merge,
    /// Each pass is kept as its own copy of the pattern ("… take 1", "… take 2"), and the next
    /// pass starts from the pattern as it was before recording.
    Stack,
}

impl LoopPasses {
    /// Name in settings files.
    pub fn key(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Stack => "stack",
        }
    }

    /// The choice named `key` (merge for anything else).
    pub fn from_key(key: &str) -> Self {
        if key == "stack" {
            Self::Stack
        } else {
            Self::Merge
        }
    }
}

/// How moving a control while recording in song mode writes automation (D111).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoMode {
    /// Writing starts when the control is touched and goes on with its last value after
    /// release, until the transport stops.
    #[default]
    Latch,
    /// Writing lasts while the control is held; on release the old automation plays again.
    Touch,
    /// Every control moved during the pass is written from where recording started to where
    /// it stopped.
    Write,
}

impl AutoMode {
    /// Every mode, in menu order.
    pub const ALL: [Self; 3] = [Self::Latch, Self::Touch, Self::Write];

    /// Name in settings files.
    pub fn key(self) -> &'static str {
        match self {
            Self::Latch => "latch",
            Self::Touch => "touch",
            Self::Write => "write",
        }
    }

    /// Menu name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Latch => "Latch",
            Self::Touch => "Touch",
            Self::Write => "Write",
        }
    }

    /// The mode named `key` (latch for anything else).
    pub fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|m| m.key() == key)
            .unwrap_or_default()
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Latch => "Writes from the first touch until the transport stops",
            Self::Touch => "Writes while you hold the control, then returns to the old automation",
            Self::Write => "Writes every control you move over the whole recorded stretch",
        }
    }
}

/// One input route: a port (and MIDI channel) that plays a fixed rack channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiRouteRow {
    /// Port name.
    pub port: String,
    /// MIDI channel 0 to 15, or `None` for all of the port's channels.
    pub channel: Option<u8>,
    /// Rack channel index.
    pub target: usize,
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
    /// Overdub or replace.
    pub mode: RecordMode,
    /// Merge or stack loop passes.
    pub passes: LoopPasses,
    /// How control moves are written as automation.
    pub automation: AutoMode,
    /// Input routes; anything not routed plays the selected channel.
    pub routes: Vec<MidiRouteRow>,
    /// Rack channel names, for route targets.
    pub channels: Vec<String>,
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
            mode: RecordMode::Overdub,
            passes: LoopPasses::Merge,
            automation: AutoMode::Latch,
            routes: Vec::new(),
            channels: Vec::new(),
        }
    }
}

/// User intents the model cannot hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiAction {
    /// Connect (true) or disconnect a port, by name.
    SetPortEnabled(String, bool),
    /// `routes` changed (they belong to the project).
    RoutesChanged,
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
        "Notes play the selected channel unless routed below. Right-click a knob or fader \
         for MIDI learn.",
    ));
    if routes(ui, m) {
        actions.push(MidiAction::RoutesChanged);
    }
    ui.add_space(6.0);
    ui.label(RichText::new("Recording").strong());
    Grid::new("midi_panel")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(dim("Mode"));
            ui.horizontal(|ui| {
                ui.selectable_value(&mut m.mode, RecordMode::Overdub, "Overdub")
                    .on_hover_text("Recorded notes join the ones already there");
                ui.selectable_value(&mut m.mode, RecordMode::Replace, "Replace")
                    .on_hover_text(
                        "Where the playhead passes, the recording channel's notes and controller \
                     data are cleared",
                    );
            });
            ui.end_row();
            ui.label(dim("Loop passes"));
            ui.horizontal(|ui| {
                ui.selectable_value(&mut m.passes, LoopPasses::Merge, "Merge")
                    .on_hover_text("Each pass around the pattern adds to it");
                ui.selectable_value(&mut m.passes, LoopPasses::Stack, "Stack")
                    .on_hover_text(
                        "Each pass around the pattern is kept as its own copy (\"take 1\", \
                     \"take 2\" …); the last pass stays in the pattern",
                    );
            });
            ui.end_row();
            ui.label(dim("Automation"));
            ui.horizontal(|ui| {
                for mode in AutoMode::ALL {
                    ui.selectable_value(&mut m.automation, mode, mode.name())
                        .on_hover_text(mode.hint());
                }
            });
            ui.end_row();
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

/// The route list. True when it changed.
fn routes(ui: &mut Ui, m: &mut MidiPanelModel) -> bool {
    let mut changed = false;
    let mut names: Vec<String> = m.ports.iter().map(|p| p.name.clone()).collect();
    for r in &m.routes {
        if !names.contains(&r.port) {
            names.push(r.port.clone());
        }
    }
    let channel_text =
        |c: Option<u8>| c.map_or("All channels".to_owned(), |c| format!("Ch {}", c + 1));
    let mut remove = None;
    for (i, r) in m.routes.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("route_port", i))
                .width(160.0)
                .selected_text(r.port.as_str())
                .show_ui(ui, |ui| {
                    for n in &names {
                        changed |= ui.selectable_value(&mut r.port, n.clone(), n).changed();
                    }
                });
            egui::ComboBox::from_id_salt(("route_ch", i))
                .width(90.0)
                .selected_text(channel_text(r.channel))
                .show_ui(ui, |ui| {
                    for c in std::iter::once(None).chain((0..16).map(Some)) {
                        changed |= ui
                            .selectable_value(&mut r.channel, c, channel_text(c))
                            .changed();
                    }
                });
            ui.label("plays");
            egui::ComboBox::from_id_salt(("route_target", i))
                .width(120.0)
                .selected_text(m.channels.get(r.target).map_or("?", String::as_str))
                .show_ui(ui, |ui| {
                    for (t, n) in m.channels.iter().enumerate() {
                        changed |= ui.selectable_value(&mut r.target, t, n).changed();
                    }
                });
            if ui.small_button("Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        m.routes.remove(i);
        changed = true;
    }
    let can_add = !names.is_empty() && !m.channels.is_empty();
    if ui
        .add_enabled(can_add, egui::Button::new("Add route"))
        .on_hover_text("Send a controller (or one of its MIDI channels) to a fixed channel, so two controllers can play two instruments")
        .clicked()
    {
        m.routes.push(MidiRouteRow {
            port: names[0].clone(),
            channel: None,
            target: 0,
        });
        changed = true;
    }
    changed
}
