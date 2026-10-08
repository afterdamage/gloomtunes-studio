//! Transport bar: play/pause, stop, tempo, time signature, position, loop and metronome.

use egui::{DragValue, RichText, Ui};
use gt_core::{Tick, TimeSig, TimeSigMap};

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
    /// Tempo in BPM at the playhead.
    pub bpm: f64,
    /// Time signature at the playhead.
    pub time_sig: TimeSig,
    /// Song mode plays the playlist; pattern mode loops the current pattern.
    pub song_mode: bool,
    /// Loop on/off.
    pub loop_enabled: bool,
    /// First tick of the loop.
    pub loop_start: Tick,
    /// First tick after the loop.
    pub loop_end: Tick,
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
            song_mode: false,
            loop_enabled: false,
            loop_start: Tick(0),
            loop_end: Tick(4 * 3840),
            // Off by default now that the rack plays a beat; one click turns it on.
            metronome: false,
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
    /// New tempo at the playhead.
    SetBpm(f64),
    /// New time signature at the playhead.
    SetTimeSig(TimeSig),
    /// Switch between pattern and song mode (the model holds the new value).
    SetSongMode(bool),
    /// Loop on/off or region changed (the model holds the new values).
    LoopChanged,
    /// Metronome on/off.
    SetMetronome(bool),
}

/// Draws the bar. Edits change `m` in place and are reported as actions. `sigs` places bar
/// numbers.
pub fn transport_bar(
    ui: &mut Ui,
    theme: &GloomTheme,
    m: &mut TransportModel,
    sigs: &TimeSigMap,
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

        for (song, label, tip) in [
            (false, "Pat", "Pattern mode: loop the current pattern (L)"),
            (true, "Song", "Song mode: play the playlist (L)"),
        ] {
            if ui
                .add(egui::Button::selectable(m.song_mode == song, label))
                .on_hover_text(tip)
                .clicked()
                && m.song_mode != song
            {
                m.song_mode = song;
                actions.push(TransportAction::SetSongMode(song));
            }
        }

        ui.separator();
        let pos = sigs.bbt(m.position);
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
        ui.label(dim("BPM"))
            .on_hover_text("Tempo at the playhead; add changes in the playlist ruler");
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
        // The loop is shown in bars; editing here snaps it to bar lines (the playlist ruler
        // sets any region).
        let first = sigs.bar_of(m.loop_start.0);
        let mut start_bar = first + 1;
        let mut bars = (sigs.bar_of(m.loop_end.0 - 1) - first + 1).max(1);
        ui.label(dim("bar"));
        let a = ui.add(DragValue::new(&mut start_bar).range(1..=9999).speed(0.05));
        ui.label(dim("for"));
        let b = ui.add(DragValue::new(&mut bars).range(1..=999).speed(0.05));
        ui.label(dim("bars"));
        if a.changed() || b.changed() {
            m.loop_start = Tick(sigs.bar_start(start_bar - 1));
            m.loop_end = Tick(sigs.bar_start(start_bar - 1 + bars));
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
