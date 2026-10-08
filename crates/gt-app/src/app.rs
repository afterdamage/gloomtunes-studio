//! The eframe application: owns the device layer, the project document and the transport
//! settings, and draws the UI.

use std::sync::atomic::Ordering;
use std::time::Duration;

use gt_core::{
    EffectKind, Instrument, Project, SampleSource, TempoMap, Tick, FX_SLOTS, MAX_CHANNELS,
    ROOT_KEY, STEP_TICKS, STRIPS,
};
use gt_engine::{
    create_effect, ChannelParams, EngineCommand, LoopRegion, MixerParams, SongSnapshot,
    TransportState,
};
use gt_project::{ops, presets, History};
use gt_ui::views::{
    audio_panel, browser, channel_rack, mixer_view, piano_roll, sampler_panel, synth_panel,
    transport_bar, AudioAction, AudioPanelModel, BrowserAction, BrowserModel, MixerState,
    MixerView, PianoRollAction, PianoRollState, PianoRollView, PlayState, RackAction, RackState,
    RackView, SamplerPanelView, StripMeter, SynthPanelAction, SynthPanelView, TransportAction,
    TransportModel,
};
use gt_ui::widgets::MeterBallistics;
use gt_ui::GloomTheme;

use crate::audio_io::AudioIo;
use crate::library::{default_folder, list_folder, synth_preset_folder, Library};

/// Sample rate used for loading while no device is open.
const FALLBACK_RATE: u32 = 48_000;
/// Activity lights fall by this much per second.
const ACTIVITY_FALL_PER_S: f32 = 4.0;

/// What the central area shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MainView {
    Rack,
    PianoRoll,
    Mixer,
}

/// What the engine last received for one effect slot, so only differences are sent.
#[derive(Debug, Clone, PartialEq)]
struct SentEffect {
    kind: EffectKind,
    params: Vec<f32>,
}

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

    main_view: MainView,
    roll: PianoRollState,
    history: History,
    /// Label of the document change made since the last undo step, if any. Committed to the
    /// history once the gesture ends (no mouse button down, no text field focused).
    pending_edit: Option<&'static str>,
    humanize_seed: u64,

    /// Slot the engine copies into the oscilloscope buffer (the selected synth channel).
    scope_slot: Option<u16>,
    scope: Vec<f32>,
    preset_dir: std::path::PathBuf,
    user_presets: Vec<(String, std::path::PathBuf)>,
    preset_status: Option<String>,

    mixer_state: MixerState,
    /// Mixer settings and effects as last sent to the engine (none: send everything).
    sent_mixer: Option<MixerParams>,
    sent_fx: Vec<[Option<SentEffect>; FX_SLOTS]>,
    strip_meters: Vec<[MeterBallistics; 2]>,
    strip_view: Vec<StripMeter>,
    fx_meters: Vec<[f32; FX_SLOTS]>,
}

impl GloomApp {
    pub fn new(ctx: &egui::Context) -> Self {
        let theme = GloomTheme::default();
        theme.apply(ctx);
        let project = Project::demo();
        let history = History::new(&project);
        let mut app = Self {
            theme,
            audio: AudioIo::new(),
            meter: MeterBallistics::default(),
            transport: TransportModel::default(),
            test_tone: false,
            show_audio: false,
            project,
            rack: RackState::default(),
            library: Library::new(),
            browser: BrowserModel::default(),
            preview_pending: None,
            slots_in_use: 0,
            activity: [0.0; MAX_CHANNELS],
            main_view: MainView::Rack,
            roll: PianoRollState::default(),
            history,
            pending_edit: None,
            humanize_seed: 0x9E37_79B9_7F4A_7C15,
            scope_slot: None,
            scope: vec![0.0; gt_engine::SCOPE_LEN],
            preset_dir: synth_preset_folder(),
            user_presets: Vec::new(),
            preset_status: None,
            mixer_state: MixerState::default(),
            sent_mixer: None,
            sent_fx: vec![Default::default(); STRIPS],
            strip_meters: vec![Default::default(); STRIPS],
            strip_view: vec![StripMeter::default(); STRIPS],
            fx_meters: vec![[0.0; FX_SLOTS]; STRIPS],
        };
        app.refresh_presets();
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
            if let Some(src) = ch.sample() {
                self.library.request(src, rate);
            }
        }
    }

    /// Sends channel `i`'s sample to the engine. If it is not loaded yet, the slot is silenced
    /// and the sample is sent when the load finishes, so a failed load never leaves the
    /// previous sample playing.
    fn push_sample(&mut self, i: usize) {
        let sample = match self.project.channels[i].sample() {
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
                if self.project.channels[i].sample() == Some(&src) {
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
                let fresh = ch.sample().is_none() && ch.name.starts_with("Sampler ");
                let Some(sampler) = ch.sampler_mut() else {
                    return; // a synth channel has no sample
                };
                sampler.sample = Some(src.clone());
                // Name a fresh "Sampler N" channel after its first sample.
                if fresh {
                    if let SampleSource::File(p) = &src {
                        if let Some(stem) = p.file_stem() {
                            ch.name = stem.to_string_lossy().into_owned();
                        }
                    }
                }
                self.push_sample(i);
                self.pending_edit = Some("Load sample");
            }
            BrowserAction::Open(dir) => self.open_folder(dir),
        }
    }

    fn refresh_presets(&mut self) {
        self.user_presets = presets::list(&self.preset_dir)
            .into_iter()
            .map(|p| {
                let name = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (name, p)
            })
            .collect();
    }

    fn on_synth_panel(&mut self, action: SynthPanelAction) {
        let i = self.rack.selected;
        match action {
            SynthPanelAction::Changed => {
                self.push_all_params();
                self.pending_edit = Some("Synth settings");
            }
            SynthPanelAction::LoadFile(path) => match presets::load(&path) {
                Ok(patch) => {
                    if let Some(p) = self.project.channels.get_mut(i).and_then(|c| c.synth_mut()) {
                        *p = patch;
                        self.preset_status = None;
                        self.push_all_params();
                        self.pending_edit = Some("Load preset");
                    }
                }
                Err(e) => self.preset_status = Some(e.to_string()),
            },
            SynthPanelAction::Save => {
                let Some(patch) = self.project.channels.get(i).and_then(|c| c.synth()) else {
                    return;
                };
                self.preset_status = Some(match presets::save(&self.preset_dir, patch) {
                    Ok(path) => format!("Saved {}", path.display()),
                    Err(e) => format!("Could not save: {e}"),
                });
                self.refresh_presets();
            }
        }
    }

    /// Points the oscilloscope at the selected channel when it is a synth.
    fn update_scope_slot(&mut self) {
        let want = self
            .project
            .channels
            .get(self.rack.selected)
            .filter(|c| c.synth().is_some())
            .map(|_| self.rack.selected as u16);
        if want != self.scope_slot {
            self.scope_slot = want;
            self.send(EngineCommand::SetScopeChannel(want));
        }
    }

    fn on_rack(&mut self, action: RackAction) {
        match action {
            RackAction::SongChanged => {
                self.push_song();
                self.pending_edit = Some("Edit pattern");
            }
            RackAction::ParamsChanged => {
                self.push_all_params();
                self.pending_edit = Some("Channel settings");
            }
            RackAction::ChannelsChanged => {
                self.push_channels();
                self.pending_edit = Some("Add or remove channel");
            }
            RackAction::OpenPianoRoll(_) => self.main_view = MainView::PianoRoll,
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

    fn on_roll(&mut self, action: PianoRollAction) {
        let slot = self.rack.selected as u16;
        match action {
            PianoRollAction::Changed => {
                self.tidy_roll_notes();
                self.push_song();
                self.pending_edit = Some("Edit notes");
            }
            PianoRollAction::AuditionOn { key, velocity } => self.send(EngineCommand::NoteOn {
                slot,
                key,
                velocity,
            }),
            PianoRollAction::AuditionOff { key } => {
                self.send(EngineCommand::NoteOff { slot, key });
            }
            PianoRollAction::Quantize | PianoRollAction::Humanize => {
                let Some(id) = self.project.channels.get(self.rack.selected).map(|c| c.id) else {
                    return;
                };
                let roll = &mut self.roll;
                let notes = self
                    .project
                    .current_pattern_mut()
                    .notes
                    .entry(id)
                    .or_default();
                roll.selected.resize(notes.len(), false);
                if action == PianoRollAction::Quantize {
                    ops::quantize(
                        notes,
                        &roll.selected,
                        roll.snap.ticks(),
                        roll.quantize_strength,
                    );
                } else {
                    self.humanize_seed = self.humanize_seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
                    ops::humanize(
                        notes,
                        &roll.selected,
                        roll.humanize_ticks,
                        roll.humanize_velocity,
                        self.humanize_seed,
                    );
                }
                ops::sort_with_selection(notes, &mut roll.selected);
                self.tidy_roll_notes();
                self.push_song();
                self.pending_edit = Some(if action == PianoRollAction::Quantize {
                    "Quantize"
                } else {
                    "Humanize"
                });
            }
        }
    }

    /// Drops an emptied note list, so "no notes" has a single representation.
    fn tidy_roll_notes(&mut self) {
        let pat = self.project.current_pattern_mut();
        pat.notes.retain(|_, v| !v.is_empty());
    }

    /// Records the pending change as an undo step once the gesture has ended.
    fn commit_if_idle(&mut self, ctx: &egui::Context) {
        let Some(label) = self.pending_edit else {
            return;
        };
        let busy = ctx.input(|i| i.pointer.any_down())
            || ctx.text_edit_focused()
            || self.roll.is_dragging();
        if !busy {
            self.history.commit(&self.project, label);
            self.pending_edit = None;
        }
    }

    /// Ends a piano-roll gesture in progress and silences its audition.
    fn cancel_roll_gesture(&mut self) {
        if let Some(a) = self.roll.cancel() {
            self.on_roll(a);
        }
    }

    fn undo(&mut self, redo: bool) {
        // The roll's drag holds indices into the notes that are about to be replaced.
        self.cancel_roll_gesture();
        // Finish the current gesture first so it becomes its own step.
        if let Some(label) = self.pending_edit.take() {
            self.history.commit(&self.project, label);
        }
        let done = if redo {
            self.history.redo(&mut self.project)
        } else {
            self.history.undo(&mut self.project)
        };
        if done.is_some() {
            self.roll.selected.clear();
            let n = self.project.channels.len();
            self.rack.selected = self.rack.selected.min(n.saturating_sub(1));
            self.push_channels();
        }
    }

    /// Sends a command to the engine if audio is running. Settings live in `self.transport`,
    /// so a command lost while audio is down is re-sent when a new engine starts.
    fn send(&mut self, cmd: EngineCommand) {
        self.try_send(cmd);
    }

    /// Like `send`, but reports whether the engine got the command.
    fn try_send(&mut self, cmd: EngineCommand) -> bool {
        let Some(engine) = self.audio.engine_mut() else {
            return false;
        };
        match engine.send(cmd) {
            Ok(()) => true,
            Err(cmd) => {
                log::warn!("engine command queue full; dropped {cmd:?}");
                false
            }
        }
    }

    /// Brings the engine's mixer in line with the document: the strip settings when they
    /// changed, a new effect where a slot's kind changed, and single parameters otherwise.
    /// Runs every frame, so edits, undo and loading all take the same path. Effects that stay
    /// in place keep their state (reverb tails ring on).
    fn sync_mixer(&mut self) {
        if self.audio.engine().is_none() {
            return;
        }
        let params = MixerParams::from_mixer(&self.project.mixer);
        if self.sent_mixer.as_ref() != Some(&params)
            && self.try_send(EngineCommand::SetMixer(Box::new(params.clone())))
        {
            self.sent_mixer = Some(params);
        }
        let rate = self.rate() as f32;
        for strip in 0..STRIPS {
            for k in 0..FX_SLOTS {
                let want = self.project.mixer.strips[strip].slots[k].clone();
                let have = self.sent_fx[strip][k].clone();
                match (want, have) {
                    (None, None) => {}
                    (None, Some(_)) => {
                        if self.try_send(EngineCommand::SetEffect {
                            strip: strip as u8,
                            slot: k as u8,
                            effect: None,
                        }) {
                            self.sent_fx[strip][k] = None;
                        }
                    }
                    (Some(w), Some(h)) if w.kind == h.kind => {
                        for (i, (&a, &b)) in w.params.iter().zip(&h.params).enumerate() {
                            if a != b
                                && self.try_send(EngineCommand::SetEffectParam {
                                    strip: strip as u8,
                                    slot: k as u8,
                                    index: i as u8,
                                    value: a,
                                })
                            {
                                if let Some(s) = self.sent_fx[strip][k].as_mut() {
                                    s.params[i] = a;
                                }
                            }
                        }
                    }
                    (Some(w), _) => {
                        let effect = create_effect(&w, rate);
                        if self.try_send(EngineCommand::SetEffect {
                            strip: strip as u8,
                            slot: k as u8,
                            effect: Some(effect),
                        }) {
                            self.sent_fx[strip][k] = Some(SentEffect {
                                kind: w.kind,
                                params: w.params,
                            });
                        }
                    }
                }
            }
        }
    }

    /// Reads the strip and effect meters from the engine and applies peak ballistics.
    fn read_meters(&mut self, dt: f32) {
        let Some(engine) = self.audio.engine() else {
            self.strip_view.fill(StripMeter::default());
            return;
        };
        let t = engine.telemetry();
        let db = |x: f32| {
            if x > 1e-6 {
                (20.0 * x.log10()).max(-60.0)
            } else {
                -60.0
            }
        };
        for i in 0..STRIPS {
            let cell = &t.meters[i];
            for side in 0..2 {
                let peak = cell.peak[side].swap(0.0, Ordering::Relaxed);
                self.strip_meters[i][side].update(peak, dt);
                self.strip_view[i].peak_db[side] = self.strip_meters[i][side].level_db();
                self.strip_view[i].rms_db[side] = db(cell.rms[side].load(Ordering::Relaxed));
            }
            for k in 0..FX_SLOTS {
                self.fx_meters[i][k] = t.fx_meters[i][k].load(Ordering::Relaxed);
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
        self.send(EngineCommand::SetScopeChannel(self.scope_slot));
        // A new engine has an empty mixer.
        self.sent_mixer = None;
        self.sent_fx = vec![Default::default(); STRIPS];
    }

    /// Draws the piano roll for the selected channel of the current pattern.
    fn show_roll(
        &mut self,
        ui: &mut egui::Ui,
        theme: &GloomTheme,
        playhead: Option<i64>,
    ) -> Vec<PianoRollAction> {
        let Some(ch) = self.project.channels.get(self.rack.selected) else {
            ui.label(egui::RichText::new("Add a channel to write notes").color(theme.text_dim));
            return Vec::new();
        };
        let (id, name) = (ch.id, ch.name.clone());
        let bar_ticks = self.transport.time_sig.bar_ticks();
        let pat = self.project.current_pattern_mut();
        self.roll.set_target(pat.id.0, id.0);
        let pattern_len = pat.length_ticks();
        // Take the edited list out so the other channels can be borrowed as ghosts.
        let mut notes = pat.notes.remove(&id).unwrap_or_default();
        let ghosts: Vec<&[gt_core::Note]> = pat.notes.values().map(Vec::as_slice).collect();
        let actions = piano_roll(
            ui,
            theme,
            &mut self.roll,
            PianoRollView {
                notes: &mut notes,
                ghosts,
                pattern_len,
                bar_ticks,
                playhead,
                channel_name: &name,
            },
        );
        if !notes.is_empty() {
            pat.notes.insert(id, notes);
        }
        actions
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

        // Undo/redo and view switching, unless a text field wants the keys. Ctrl+Shift+Z is
        // checked before Ctrl+Z because egui ignores extra Shift when matching.
        if !ctx.text_edit_focused() {
            use egui::{Key, Modifiers};
            let key = |m, k| ctx.input_mut(|i| i.consume_key(m, k));
            if key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) || key(Modifiers::COMMAND, Key::Y)
            {
                self.undo(true);
            } else if key(Modifiers::COMMAND, Key::Z) {
                self.undo(false);
            }
            if key(Modifiers::NONE, Key::F6) {
                self.main_view = MainView::Rack;
            }
            if key(Modifiers::NONE, Key::F7) {
                self.main_view = MainView::PianoRoll;
            }
            if key(Modifiers::NONE, Key::F9) {
                self.main_view = MainView::Mixer;
            }
        }

        let theme = self.theme.clone();
        let mut undo_clicked = None;
        let undo_tip = self
            .history
            .undo_label()
            .map(|l| format!("Undo {l} (Ctrl+Z)"));
        let redo_tip = self
            .history
            .redo_label()
            .map(|l| format!("Redo {l} (Ctrl+Shift+Z)"));
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
                        let redo = ui.add_enabled(redo_tip.is_some(), egui::Button::new("Redo"));
                        if redo
                            .on_hover_text(redo_tip.as_deref().unwrap_or("Nothing to redo"))
                            .clicked()
                        {
                            undo_clicked = Some(true);
                        }
                        let undo = ui.add_enabled(undo_tip.is_some(), egui::Button::new("Undo"));
                        if undo
                            .on_hover_text(undo_tip.as_deref().unwrap_or("Nothing to undo"))
                            .clicked()
                        {
                            undo_clicked = Some(false);
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
        if let Some(redo) = undo_clicked {
            self.undo(redo);
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
            .filter(|c| c.sampler().is_some())
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

        // The oscilloscope follows the selected synth channel.
        self.update_scope_slot();
        if self.scope_slot.is_some() {
            if let Some(engine) = self.audio.engine() {
                engine.telemetry().read_scope(&mut self.scope);
            }
        }
        let sample_rate = self.rate() as f32;
        let show_instrument = self.main_view != MainView::Mixer;
        let (sampler_changed, synth_actions) = if !show_instrument {
            (false, Vec::new())
        } else {
            egui::Panel::bottom("instrument")
                .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(8))
                .show(ui, |ui| {
                    let Some(ch) = self.project.channels.get_mut(self.rack.selected) else {
                        ui.label(egui::RichText::new("No channel selected").color(theme.text_dim));
                        return (false, Vec::new());
                    };
                    match &mut ch.instrument {
                        Instrument::Sampler(s) => {
                            let loaded = s.sample.as_ref().and_then(|x| self.library.get(x));
                            let status = s.sample.as_ref().and_then(|x| self.library.status(x));
                            let view = SamplerPanelView {
                                waveform: loaded.map(|l| l.overview.as_slice()),
                                seconds: loaded.map(|l| l.data.seconds()),
                                status: status.as_deref(),
                            };
                            (sampler_panel(ui, &theme, &mut ch.name, s, view), Vec::new())
                        }
                        Instrument::Synth(patch) => {
                            let view = SynthPanelView {
                                scope: &self.scope,
                                user_presets: &self.user_presets,
                                sample_rate,
                                status: self.preset_status.as_deref(),
                            };
                            (false, synth_panel(ui, &theme, &mut ch.name, patch, view))
                        }
                    }
                })
                .inner
        };
        if sampler_changed {
            self.push_all_params();
            self.pending_edit = Some("Sampler settings");
        }
        for a in synth_actions {
            self.on_synth_panel(a);
        }

        let playing = self.transport.state == PlayState::Playing;
        let pattern_pos = playing.then(|| {
            let len = self.project.current_pattern().length_ticks().max(1);
            self.transport.position.0.rem_euclid(len)
        });
        if self.main_view == MainView::Mixer {
            self.read_meters(dt);
        }
        let mut mixer_changed = false;
        let (rack_actions, roll_actions) = egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (v, label, tip) in [
                        (MainView::Rack, "Channel rack", "Step sequencer (F6)"),
                        (
                            MainView::PianoRoll,
                            "Piano roll",
                            "Notes of the selected channel (F7)",
                        ),
                        (MainView::Mixer, "Mixer", "Inserts, sends and effects (F9)"),
                    ] {
                        if ui
                            .add(egui::Button::selectable(self.main_view == v, label))
                            .on_hover_text(tip)
                            .clicked()
                        {
                            self.main_view = v;
                        }
                    }
                });
                ui.separator();
                match self.main_view {
                    MainView::Rack => {
                        let a = channel_rack(
                            ui,
                            &theme,
                            &mut self.project,
                            &mut self.rack,
                            RackView {
                                playhead_step: pattern_pos.map(|t| (t / STEP_TICKS) as u16),
                                activity: &self.activity,
                            },
                        );
                        (a, Vec::new())
                    }
                    MainView::PianoRoll => (Vec::new(), self.show_roll(ui, &theme, pattern_pos)),
                    MainView::Mixer => {
                        let latency = self
                            .audio
                            .engine()
                            .map_or(0, |e| e.telemetry().latency_frames.load(Ordering::Relaxed));
                        let view = MixerView {
                            meters: &self.strip_view,
                            fx_meters: &self.fx_meters,
                            sample_rate,
                            latency_ms: latency as f32 * 1000.0 / sample_rate,
                        };
                        if mixer_view(
                            ui,
                            &theme,
                            &mut self.project.mixer,
                            &mut self.mixer_state,
                            view,
                        ) {
                            mixer_changed = true;
                        }
                        (Vec::new(), Vec::new())
                    }
                }
            })
            .inner;
        for a in rack_actions {
            self.on_rack(a);
        }
        for a in roll_actions {
            self.on_roll(a);
        }
        if mixer_changed {
            self.pending_edit = Some("Mixer");
        }
        self.sync_mixer();
        if self.main_view != MainView::PianoRoll {
            // Hidden mid-gesture (F6, tab click): the roll never sees the release.
            self.cancel_roll_gesture();
        }
        self.commit_if_idle(&ctx);

        // Repaint at display rate while anything moves; idle otherwise.
        let lights = self.activity.iter().any(|&a| a > 0.0)
            || self.main_view == MainView::Mixer
                && self
                    .strip_meters
                    .iter()
                    .flatten()
                    .any(MeterBallistics::is_moving);
        if playing || self.test_tone || self.meter.is_moving() || lights || self.library.is_busy() {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if self.audio.engine().is_some() {
            // Keep stream housekeeping (reopen, errors) ticking slowly.
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}
