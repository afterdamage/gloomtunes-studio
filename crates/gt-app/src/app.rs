//! The eframe application: owns the device layer, the project document and the transport
//! settings, and draws the UI.

use std::sync::atomic::Ordering;
use std::time::Duration;

use gt_core::{Project, SampleSource, TempoMap, Tick, MAX_CHANNELS, ROOT_KEY, STEP_TICKS};
use gt_engine::{ChannelParams, EngineCommand, LoopRegion, SongSnapshot, TransportState};
use gt_ui::views::{
    audio_panel, browser, channel_rack, sampler_panel, transport_bar, AudioAction, AudioPanelModel,
    BrowserAction, BrowserModel, PlayState, RackAction, RackState, RackView, SamplerPanelView,
    TransportAction, TransportModel,
};
use gt_ui::widgets::MeterBallistics;
use gt_ui::GloomTheme;

use crate::audio_io::AudioIo;
use crate::library::{default_folder, list_folder, Library};

/// Sample rate used for loading while no device is open.
const FALLBACK_RATE: u32 = 48_000;
/// Activity lights fall by this much per second.
const ACTIVITY_FALL_PER_S: f32 = 4.0;

pub struct GloomApp {
    theme: GloomTheme,
    audio: AudioIo,
    meter: MeterBallistics,
    transport: TransportModel,
    test_tone: bool,
    show_audio: bool,

    project: Project,
    rack: RackState,
    library: Library,
    browser: BrowserModel,
    /// Sound the user clicked in the browser, waiting to load before it can be previewed.
    preview_pending: Option<SampleSource>,
    /// Engine slots that may hold a sample (cleared when channels are removed).
    slots_in_use: usize,
    activity: [f32; MAX_CHANNELS],
}

impl GloomApp {
    pub fn new(ctx: &egui::Context) -> Self {
        let theme = GloomTheme::default();
        theme.apply(ctx);
        let mut app = Self {
            theme,
            audio: AudioIo::new(),
            meter: MeterBallistics::default(),
            transport: TransportModel::default(),
            test_tone: false,
            show_audio: false,
            project: Project::demo(),
            rack: RackState::default(),
            library: Library::new(),
            browser: BrowserModel::default(),
            preview_pending: None,
            slots_in_use: 0,
            activity: [0.0; MAX_CHANNELS],
        };
        app.open_folder(default_folder());
        app.request_channel_samples();
        app
    }

    fn rate(&self) -> u32 {
        self.audio
            .engine()
            .map_or(FALLBACK_RATE, |e| e.config().sample_rate)
    }

    fn open_folder(&mut self, dir: std::path::PathBuf) {
        match list_folder(&dir) {
            Ok(entries) => {
                self.browser.entries = entries;
                self.browser.error = None;
                self.browser.path_text = dir.display().to_string();
                self.browser.folder = dir;
            }
            Err(e) => self.browser.error = Some(e),
        }
    }

    /// Starts loading every sample the project uses.
    fn request_channel_samples(&mut self) {
        let rate = self.rate();
        for ch in &self.project.channels {
            if let Some(src) = &ch.sampler.sample {
                self.library.request(src, rate);
            }
        }
    }

    /// Sends channel `i`'s sample to the engine. If it is not loaded yet, the slot is silenced
    /// and the sample is sent when the load finishes, so a failed load never leaves the
    /// previous sample playing.
    fn push_sample(&mut self, i: usize) {
        let sample = match &self.project.channels[i].sampler.sample {
            None => None,
            Some(src) => match self.library.get(src) {
                Some(l) => Some(std::sync::Arc::clone(&l.data)),
                None => {
                    let rate = self.rate();
                    self.library.request(&src.clone(), rate);
                    None
                }
            },
        };
        self.send(EngineCommand::SetChannelSample {
            slot: i as u16,
            sample,
        });
    }

    fn push_all_params(&mut self) {
        for i in 0..self.project.channels.len() {
            let p =
                ChannelParams::from_channel(&self.project.channels[i], self.project.is_silenced(i));
            self.send(EngineCommand::SetChannelParams {
                slot: i as u16,
                params: Box::new(p),
            });
        }
    }

    fn push_song(&mut self) {
        let song = SongSnapshot::compile(&self.project);
        self.send(EngineCommand::SetSong(Box::new(song)));
    }

    /// Sends every channel's sample and settings, clears slots no longer used, and the song.
    fn push_channels(&mut self) {
        let n = self.project.channels.len();
        for slot in n..self.slots_in_use {
            self.send(EngineCommand::SetChannelSample {
                slot: slot as u16,
                sample: None,
            });
        }
        self.slots_in_use = n;
        for i in 0..n {
            self.push_sample(i);
        }
        self.push_all_params();
        self.push_song();
    }

    fn on_samples_ready(&mut self, ready: Vec<SampleSource>) {
        for src in ready {
            for i in 0..self.project.channels.len() {
                if self.project.channels[i].sampler.sample.as_ref() == Some(&src) {
                    self.push_sample(i);
                }
            }
            if self.preview_pending.as_ref() == Some(&src) {
                self.preview_pending = None;
                self.preview(&src);
            }
        }
    }

    fn preview(&mut self, src: &SampleSource) {
        match self.library.get(src) {
            Some(l) => {
                let data = std::sync::Arc::clone(&l.data);
                self.send(EngineCommand::PreviewSample(Some(data)));
            }
            None => {
                self.preview_pending = Some(src.clone());
                let rate = self.rate();
                self.library.request(src, rate);
            }
        }
    }

    fn on_browser(&mut self, action: BrowserAction) {
        match action {
            BrowserAction::Preview(src) => self.preview(&src),
            BrowserAction::Assign(src) => {
                let i = self.rack.selected;
                let Some(ch) = self.project.channels.get_mut(i) else {
                    return;
                };
                // Name a fresh "Sampler N" channel after its first sample.
                if ch.sampler.sample.is_none() && ch.name.starts_with("Sampler ") {
                    if let SampleSource::File(p) = &src {
                        if let Some(stem) = p.file_stem() {
                            ch.name = stem.to_string_lossy().into_owned();
                        }
                    }
                }
                ch.sampler.sample = Some(src);
                self.push_sample(i);
            }
            BrowserAction::Open(dir) => self.open_folder(dir),
        }
    }

    fn on_rack(&mut self, action: RackAction) {
        match action {
            RackAction::SongChanged => self.push_song(),
            RackAction::ParamsChanged => self.push_all_params(),
            RackAction::ChannelsChanged => self.push_channels(),
            RackAction::NoteOn(i) => self.send(EngineCommand::NoteOn {
                slot: i as u16,
                key: ROOT_KEY,
                velocity: gt_core::project::DEFAULT_VELOCITY,
            }),
            RackAction::NoteOff(i) => self.send(EngineCommand::NoteOff {
                slot: i as u16,
                key: ROOT_KEY,
            }),
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
        self.slots_in_use = 0; // a new engine starts with empty slots
        self.push_channels();
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
        let ready = self.library.poll();
        self.on_samples_ready(ready);

        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        // Read the engine's view of the transport and the channel activity.
        if let Some(engine) = self.audio.engine() {
            let t = engine.telemetry();
            self.transport.state = match t.transport_state() {
                TransportState::Stopped => PlayState::Stopped,
                TransportState::Playing => PlayState::Playing,
                TransportState::Paused => PlayState::Paused,
            };
            self.transport.position = t.position();
            self.transport.audio_online = true;
            for (a, peak) in self.activity.iter_mut().zip(&t.channel_peaks) {
                let p = peak.swap(0.0, Ordering::Relaxed);
                *a = (*a - ACTIVITY_FALL_PER_S * dt)
                    .max(p.min(1.0) * 1.5)
                    .max(0.0);
            }
        } else {
            self.transport.state = PlayState::Stopped;
            self.transport.audio_online = false;
            self.activity = [0.0; MAX_CHANNELS];
        }
        self.meter.update(self.audio.take_peak(), dt);

        // Space toggles play. Consumed before any widget runs, so a focused button does not
        // also react to it; left alone while a text field (e.g. typing a BPM) has focus.
        let space = !ctx.text_edit_focused()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space));

        let theme = self.theme.clone();
        let mut transport_actions = egui::Panel::top("transport")
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let a = transport_bar(ui, &theme, &mut self.transport);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::selectable(self.show_audio, "Audio"))
                            .on_hover_text("Audio device settings")
                            .clicked()
                        {
                            self.show_audio = !self.show_audio;
                        }
                        gt_ui::widgets::level_meter(
                            ui,
                            &theme,
                            self.meter.level_db(),
                            egui::vec2(90.0, 8.0),
                        );
                    });
                    a
                })
                .inner
            })
            .inner;
        if space {
            transport_actions.push(TransportAction::PlayPause);
        }
        for a in transport_actions {
            self.on_transport(a);
        }

        if self.show_audio {
            let model = AudioPanelModel {
                level_db: self.meter.level_db(),
                ..self.audio.panel_model(self.test_tone)
            };
            let mut open = true;
            let action = egui::Window::new("Audio settings")
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .show(&ctx, |ui| audio_panel(ui, &theme, &model))
                .and_then(|r| r.inner.flatten());
            self.show_audio = open;
            if let Some(a) = action {
                self.on_audio(a);
            }
        }

        let target = self
            .project
            .channels
            .get(self.rack.selected)
            .map(|c| c.name.clone());
        let browser_actions = egui::Panel::left("browser")
            .resizable(true)
            .default_size(230.0)
            .min_size(160.0)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(8))
            .show(ui, |ui| {
                browser(ui, &theme, &mut self.browser, target.as_deref())
            })
            .inner;
        for a in browser_actions {
            self.on_browser(a);
        }

        let sampler_changed = egui::Panel::bottom("sampler")
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(8))
            .show(ui, |ui| {
                let Some(ch) = self.project.channels.get_mut(self.rack.selected) else {
                    ui.label(egui::RichText::new("No channel selected").color(theme.text_dim));
                    return false;
                };
                let loaded = ch.sampler.sample.as_ref().and_then(|s| self.library.get(s));
                let status = ch
                    .sampler
                    .sample
                    .as_ref()
                    .and_then(|s| self.library.status(s));
                let view = SamplerPanelView {
                    waveform: loaded.map(|l| l.overview.as_slice()),
                    seconds: loaded.map(|l| l.data.seconds()),
                    status: status.as_deref(),
                };
                sampler_panel(ui, &theme, ch, view)
            })
            .inner;
        if sampler_changed {
            self.push_all_params();
        }

        let playhead_step = (self.transport.state == PlayState::Playing).then(|| {
            let len = self.project.current_pattern().length_ticks().max(1);
            (self.transport.position.0.rem_euclid(len) / STEP_TICKS) as u16
        });
        let rack_actions = egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(10))
            .show(ui, |ui| {
                channel_rack(
                    ui,
                    &theme,
                    &mut self.project,
                    &mut self.rack,
                    RackView {
                        playhead_step,
                        activity: &self.activity,
                    },
                )
            })
            .inner;
        for a in rack_actions {
            self.on_rack(a);
        }

        // Repaint at display rate while anything moves; idle otherwise.
        let playing = self.transport.state == PlayState::Playing;
        let lights = self.activity.iter().any(|&a| a > 0.0);
        if playing || self.test_tone || self.meter.is_moving() || lights || self.library.is_busy() {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if self.audio.engine().is_some() {
            // Keep stream housekeeping (reopen, errors) ticking slowly.
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}
