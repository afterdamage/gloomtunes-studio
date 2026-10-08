//! The eframe application: owns the device layer, the transport settings and draws the UI.

use std::time::Duration;

use gt_core::{TempoMap, Tick};
use gt_engine::{EngineCommand, LoopRegion, TransportState};
use gt_ui::views::{
    audio_panel, transport_bar, AudioAction, AudioPanelModel, PlayState, TransportAction,
    TransportModel,
};
use gt_ui::widgets::MeterBallistics;
use gt_ui::GloomTheme;

use crate::audio_io::AudioIo;

pub struct GloomApp {
    theme: GloomTheme,
    audio: AudioIo,
    meter: MeterBallistics,
    transport: TransportModel,
    test_tone: bool,
}

impl GloomApp {
    pub fn new(ctx: &egui::Context) -> Self {
        let theme = GloomTheme::default();
        theme.apply(ctx);
        Self {
            theme,
            audio: AudioIo::new(),
            meter: MeterBallistics::default(),
            transport: TransportModel::default(),
            test_tone: false,
        }
    }

    /// Sends a command to the engine if audio is running. Settings live in `self.transport`,
    /// so a command lost while audio is down is re-sent when a new engine starts.
    fn send(&mut self, cmd: EngineCommand) {
        if let Some(engine) = self.audio.engine_mut() {
            if let Err(cmd) = engine.send(cmd) {
                log::warn!("engine command queue full; dropped {cmd:?}");
            }
        }
    }

    /// Brings a freshly created engine up to date with the current settings.
    fn sync_new_engine(&mut self) {
        self.send(EngineCommand::SetTempoMap(Box::new(TempoMap::constant(
            self.transport.bpm,
        ))));
        self.send(EngineCommand::SetTimeSig(self.transport.time_sig));
        self.send(EngineCommand::SetLoop(self.loop_region()));
        self.send(EngineCommand::SetMetronome(self.transport.metronome));
        self.send(EngineCommand::Locate(self.transport.position));
        self.send(EngineCommand::SetTestTone(self.test_tone));
    }

    fn loop_region(&self) -> LoopRegion {
        let bar = self.transport.time_sig.bar_ticks();
        let start = (self.transport.loop_start_bar - 1) * bar;
        LoopRegion {
            start: Tick(start),
            end: Tick(start + self.transport.loop_bars * bar),
            enabled: self.transport.loop_enabled,
        }
    }

    fn on_transport(&mut self, action: TransportAction) {
        match action {
            TransportAction::PlayPause => {
                let cmd = if self.transport.state == PlayState::Playing {
                    EngineCommand::Pause
                } else {
                    EngineCommand::Play
                };
                self.send(cmd);
            }
            TransportAction::Stop => self.send(EngineCommand::Stop),
            TransportAction::SetBpm(bpm) => {
                // Allocated here on the UI thread; the old map comes back as garbage.
                self.send(EngineCommand::SetTempoMap(Box::new(TempoMap::constant(
                    bpm,
                ))));
            }
            TransportAction::SetTimeSig(sig) => {
                self.send(EngineCommand::SetTimeSig(sig));
                // The loop is defined in bars, so its tick range depends on the signature.
                self.send(EngineCommand::SetLoop(self.loop_region()));
            }
            TransportAction::LoopChanged => self.send(EngineCommand::SetLoop(self.loop_region())),
            TransportAction::SetMetronome(on) => self.send(EngineCommand::SetMetronome(on)),
        }
    }

    fn on_audio(&mut self, action: AudioAction) {
        match action {
            AudioAction::ToggleTestTone => {
                self.test_tone = !self.test_tone;
                self.send(EngineCommand::SetTestTone(self.test_tone));
            }
            AudioAction::RestartAudio => self.audio.reopen(),
            AudioAction::SelectDevice(i) => self.audio.select_device(i),
            AudioAction::SelectBufferSize(n) => self.audio.select_buffer_size(n),
            AudioAction::Rescan => self.audio.rescan(),
        }
    }
}

impl eframe::App for GloomApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.audio.poll();
        if self.audio.take_fresh_engine() {
            self.sync_new_engine();
        }

        // Read the engine's view of the transport.
        if let Some(engine) = self.audio.engine() {
            let t = engine.telemetry();
            self.transport.state = match t.transport_state() {
                TransportState::Stopped => PlayState::Stopped,
                TransportState::Playing => PlayState::Playing,
                TransportState::Paused => PlayState::Paused,
            };
            self.transport.position = t.position();
            self.transport.audio_online = true;
        } else {
            self.transport.state = PlayState::Stopped;
            self.transport.audio_online = false;
        }

        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        self.meter.update(self.audio.take_peak(), dt);

        // Space toggles play. Consumed before any widget runs, so a focused button does not
        // also react to it; left alone while a text field (e.g. typing a BPM) has focus.
        let space = !ctx.text_edit_focused()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space));

        let mut transport_actions = egui::Panel::top("transport")
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(8))
            .show(ui, |ui| transport_bar(ui, &self.theme, &mut self.transport))
            .inner;
        if space {
            transport_actions.push(TransportAction::PlayPause);
        }
        for a in transport_actions {
            self.on_transport(a);
        }

        let model = AudioPanelModel {
            level_db: self.meter.level_db(),
            ..self.audio.panel_model(self.test_tone)
        };
        let audio_action = egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(12))
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Audio device").color(self.theme.text_dim));
                ui.add_space(4.0);
                audio_panel(ui, &self.theme, &model)
            })
            .inner;
        if let Some(a) = audio_action {
            self.on_audio(a);
        }

        // Repaint at display rate while sound or playback is running; idle otherwise.
        let playing = self.transport.state == PlayState::Playing;
        if playing || self.test_tone || self.meter.is_moving() {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if self.audio.engine().is_some() {
            // Keep stream housekeeping (reopen, errors) ticking slowly.
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}
