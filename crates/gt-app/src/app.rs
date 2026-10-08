//! The eframe application: owns the device layer and draws the UI.

use std::time::Duration;

use gt_ui::views::{audio_panel, AudioAction, AudioPanelModel};
use gt_ui::widgets::MeterBallistics;
use gt_ui::GloomTheme;

use crate::audio_io::AudioIo;

pub struct GloomApp {
    theme: GloomTheme,
    audio: AudioIo,
    meter: MeterBallistics,
}

impl GloomApp {
    pub fn new(ctx: &egui::Context) -> Self {
        let theme = GloomTheme::default();
        theme.apply(ctx);
        Self {
            theme,
            audio: AudioIo::new(),
            meter: MeterBallistics::default(),
        }
    }
}

impl eframe::App for GloomApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.audio.poll();

        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        self.meter.update(self.audio.take_peak(), dt);

        let model = AudioPanelModel {
            level_db: self.meter.level_db(),
            ..self.audio.panel_model()
        };

        let action = egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(12))
            .show(ui, |ui| {
                ui.heading("GloomTunes Studio");
                ui.add_space(8.0);
                audio_panel(ui, &self.theme, &model)
            })
            .inner;

        match action {
            Some(AudioAction::Start) => self.audio.start(),
            Some(AudioAction::Stop) => self.audio.stop(),
            Some(AudioAction::SelectDevice(i)) => self.audio.select_device(i),
            Some(AudioAction::SelectBufferSize(n)) => self.audio.select_buffer_size(n),
            Some(AudioAction::Rescan) => self.audio.rescan(),
            None => {}
        }

        // Keep the meter and stream state fresh while audio runs; idle otherwise.
        if self.audio.is_active() || self.meter.is_moving() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}
