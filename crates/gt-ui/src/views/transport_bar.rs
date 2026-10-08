//! Transport bar: play/pause, stop, tempo, time signature, position, loop and metronome.

use egui::{DragValue, RichText, Ui};
use gt_core::{BarBeatTick, Tick, TimeSig};

use crate::GloomTheme;

/// Transport state as shown in the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayState {
    /// Stopped.
    #[default]
    Stopped,
    /// Playing.
    Playing,
    /// Paused.
    Paused,
}

/// Editable transport settings plus read-only playback state.
#[derive(Debug, Clone, PartialEq)]
pub struct TransportModel {
    /// Playback state from the engine.
    pub state: PlayState,
    /// Playhead from the engine.
    pub position: Tick,
    /// Tempo in BPM.
    pub bpm: f64,
    /// Time signature.
    pub time_sig: TimeSig,
    /// Loop on/off.
    pub loop_enabled: bool,
    /// First bar of the loop, from 1.
    pub loop_start_bar: i64,
    /// Loop length in bars.
    pub loop_bars: i64,
    /// Metronome on/off.
    pub metronome: bool,
    /// False when no audio stream is open (controls still edit settings).
    pub audio_online: bool,
}

impl Default for TransportModel {
    fn default() -> Self {
        Self {
            state: PlayState::Stopped,
            position: Tick::ZERO,
            bpm: 120.0,
            time_sig: TimeSig::default(),
            loop_enabled: false,
            loop_start_bar: 1,
            loop_bars: 4,
            metronome: true,
            audio_online: false,
        }
    }
}

/// User intents raised by the transport bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransportAction {
    /// Play, or pause while playing.
    PlayPause,
    /// Stop (twice: back to bar 1).
    Stop,
    /// New tempo.
    SetBpm(f64),
    /// New time signature.
    SetTimeSig(TimeSig),
    /// Loop on/off or region changed (the model holds the new values).
    LoopChanged,
    /// Metronome on/off.
    SetMetronome(bool),
}

/// Draws the bar. Edits change `m` in place and are reported as actions.
pub fn transport_bar(
    ui: &mut Ui,
    theme: &GloomTheme,
    m: &mut TransportModel,
) -> Vec<TransportAction> {
    let mut actions = Vec::new();
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    ui.horizontal(|ui| {
        let playing = m.state == PlayState::Playing;
        let icon = if playing { Icon::Pause } else { Icon::Play };
        if icon_button(ui, theme, icon, playing)
            .on_hover_text("Play / pause (Space)")
            .clicked()
        {
            actions.push(TransportAction::PlayPause);
        }
        if icon_button(ui, theme, Icon::Stop, false)
            .on_hover_text("Stop: back to where play started; again: back to bar 1")
            .clicked()
        {
            actions.push(TransportAction::Stop);
        }

        ui.separator();
        let pos = BarBeatTick::from_tick(m.position, m.time_sig);
        let colour = if m.audio_online {
            theme.accent
        } else {
            theme.text_dim
        };
        ui.label(
            RichText::new(pos.to_string())
                .monospace()
                .size(theme.font_size + 6.0)
                .color(colour),
        )
        .on_hover_text("Bar : beat : tick (960 ticks per quarter note)");

        ui.separator();
        ui.label(dim("BPM"));
        let mut bpm = m.bpm;
        let r = ui.add(
            DragValue::new(&mut bpm)
                .range(gt_core::time::MIN_BPM..=gt_core::time::MAX_BPM)
                .speed(0.1)
                .fixed_decimals(2),
        );
        if r.changed() && bpm != m.bpm {
            m.bpm = bpm;
            actions.push(TransportAction::SetBpm(bpm));
        }

        let mut num = i32::from(m.time_sig.num);
        let mut den = m.time_sig.den;
        ui.add(DragValue::new(&mut num).range(1..=32).speed(0.05));
        ui.label(dim("/"));
        egui::ComboBox::from_id_salt("time_sig_den")
            .width(36.0)
            .selected_text(den.to_string())
            .show_ui(ui, |ui| {
                for d in [2u8, 4, 8, 16] {
                    ui.selectable_value(&mut den, d, d.to_string());
                }
            });
        let sig = TimeSig::new(num as u8, den);
        if sig != m.time_sig {
            m.time_sig = sig;
            actions.push(TransportAction::SetTimeSig(sig));
        }

        ui.separator();
        if ui
            .add(egui::Button::selectable(m.loop_enabled, "Loop"))
            .on_hover_text("Loop the region below")
            .clicked()
        {
            m.loop_enabled = !m.loop_enabled;
            actions.push(TransportAction::LoopChanged);
        }
        ui.label(dim("bar"));
        let a = ui.add(
            DragValue::new(&mut m.loop_start_bar)
                .range(1..=9999)
                .speed(0.05),
        );
        ui.label(dim("for"));
        let b = ui.add(DragValue::new(&mut m.loop_bars).range(1..=999).speed(0.05));
        ui.label(dim("bars"));
        if a.changed() || b.changed() {
            actions.push(TransportAction::LoopChanged);
        }

        ui.separator();
        if ui
            .add(egui::Button::selectable(m.metronome, "Click"))
            .on_hover_text("Metronome")
            .clicked()
        {
            m.metronome = !m.metronome;
            actions.push(TransportAction::SetMetronome(m.metronome));
        }
    });
    actions
}

/// Transport icons, painted as shapes so they look the same with any font.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Icon {
    Play,
    Pause,
    Stop,
}

fn icon_button(ui: &mut Ui, theme: &GloomTheme, icon: Icon, active: bool) -> egui::Response {
    let resp = ui.add(egui::Button::new("").min_size(egui::vec2(32.0, 22.0)));
    let colour = if active { theme.accent } else { theme.text };
    let c = resp.rect.center();
    let p = ui.painter();
    match icon {
        Icon::Play => {
            let pts = vec![
                c + egui::vec2(-4.0, -6.0),
                c + egui::vec2(6.0, 0.0),
                c + egui::vec2(-4.0, 6.0),
            ];
            p.add(egui::Shape::convex_polygon(pts, colour, egui::Stroke::NONE));
        }
        Icon::Pause => {
            for dx in [-3.5, 3.5] {
                let r =
                    egui::Rect::from_center_size(c + egui::vec2(dx, 0.0), egui::vec2(3.0, 12.0));
                p.rect_filled(r, 0.0, colour);
            }
        }
        Icon::Stop => {
            p.rect_filled(
                egui::Rect::from_center_size(c, egui::vec2(10.0, 10.0)),
                1.0,
                colour,
            );
        }
    }
    resp
}
