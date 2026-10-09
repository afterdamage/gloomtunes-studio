//! The eframe application: owns the device layer, the project document and the transport
//! settings, and draws the UI.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_core::{
    ClipKind, EffectKind, Instrument, ModSourceKind, Modulator, ParamId, PluginInstanceId,
    PluginKind, Project, SampleSource, SigChange, TempoPoint, Tick, TimeSigMap, FX_SLOTS,
    MAX_CHANNELS, ROOT_KEY, STEP_TICKS, STRIPS,
};
use gt_engine::{
    create_effect, ChannelParams, EngineCommand, LoopRegion, MixerParams, ModPlan, ParamDest,
    SongSnapshot, TransportState,
};
use gt_plugin_host::{PluginHost, PluginStatus};
use gt_project::{ops, presets, History};
use gt_ui::param_ui::{self, ParamMarks, ParamRequest};
use gt_ui::views::{
    add_automation, audio_panel, browser, channel_rack, mixer_view, modulators_panel, piano_roll,
    playlist, plugin_controls, sampler_panel, synth_panel, transport_bar, AudioAction,
    AudioPanelModel, BrowserAction, BrowserModel, MidiPanelModel, MixerState, MixerView,
    ModulatorsState, PianoRollAction, PianoRollState, PianoRollView, PlayState, PlaylistAction,
    PlaylistState, PlaylistView, PluginBrowserState, PluginPanelView, PluginState, RackAction,
    RackState, RackView, SamplerPanelView, StripMeter, SynthPanelAction, SynthPanelView,
    TransportAction, TransportModel,
};
use gt_ui::widgets::MeterBallistics;
use gt_ui::GloomTheme;

mod midi;
mod plugins;

use crate::audio_io::AudioIo;
use crate::files::{self, Dialog, DialogAction, ExportForm, Then};
use crate::library::{data_folder, default_folder, list_folder, synth_preset_folder, Library};

/// Sample rate used for loading while no device is open.
const FALLBACK_RATE: u32 = 48_000;
/// Activity lights fall by this much per second.
const ACTIVITY_FALL_PER_S: f32 = 4.0;

/// What the central area shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MainView {
    Playlist,
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

    playlist: PlaylistState,
    /// Effect parameters the last song automates and the last modulation plan modulates. The
    /// engine sets them every control period; when neither drives one any more, its document
    /// value is sent again.
    lane_fx: Vec<(usize, usize, usize)>,
    mod_fx: Vec<(usize, usize, usize)>,
    /// What the last modulation plan was built from (none: send it).
    sent_mods: Option<Vec<(Modulator, Option<ParamDest>, f32)>>,
    show_modulators: bool,
    modulators: ModulatorsState,

    /// Where the project was opened from or last saved to.
    doc_path: Option<PathBuf>,
    /// Embed samples on plain Save (the last choice made in Save as).
    embed_samples: bool,
    /// Undo steps recorded so far, and the count at the last save and autosave: the project
    /// has unsaved changes while they differ.
    edits: u64,
    saved_edits: u64,
    autosaved_edits: u64,
    last_autosave: Instant,
    dialog: Option<Dialog>,
    /// Remembered export settings.
    export_form: Option<ExportForm>,
    export_job: Option<std::thread::JoinHandle<Result<Vec<PathBuf>, gt_export::ExportError>>>,
    /// A short message shown at the bottom right, and when it appeared.
    toast: Option<(String, Instant)>,
    /// The user confirmed quitting (or there was nothing to save).
    quit_confirmed: bool,
    window_title: String,

    /// Live notes from MIDI devices and the typing keyboard, to the engine.
    live: gt_engine::LiveInput,
    midi: crate::midi_io::MidiIo,
    keyboard: crate::live::TypingKeyboard,
    recorder: crate::live::Recorder,
    /// The take's channel and the pattern length when it started.
    take: Option<(gt_core::ChannelId, i64)>,
    /// MIDI and recording settings (shown in the Audio and MIDI window).
    midi_model: MidiPanelModel,
    /// Live channel slot last sent to the engine (outer none: send it).
    sent_live_slot: Option<Option<u16>>,
    /// Parameter waiting for a MIDI controller to move (MIDI learn).
    learning: Option<ParamId>,
    /// MIDI activity count last seen.
    midi_seen: u32,

    /// CLAP plugins: finding, loading and running them, and their editor windows. Declared
    /// after `audio`, so the engine (holding their processors) goes first at exit.
    plugins: PluginHost,
    /// Plugin files in use when the last session ended unexpectedly.
    crashed_plugins: Vec<PathBuf>,
    show_plugins: bool,
    plugin_browser: PluginBrowserState,
    /// Mixer slot the plugin browser adds effects to (chosen with the slot's "Plugin…").
    plugin_slot: Option<(usize, usize)>,
    /// A new engine started this frame: the plugin host starts its plugins in it.
    fresh_engine: bool,
}

/// What the plugin panels show for plugin `id`.
fn plugin_view(host: &PluginHost, id: PluginInstanceId) -> PluginPanelView {
    PluginPanelView {
        state: match host.status(id) {
            PluginStatus::Running => PluginState::Running,
            PluginStatus::Waiting => PluginState::Waiting,
            PluginStatus::Failed => PluginState::Failed,
            PluginStatus::Unavailable(why) => PluginState::Unavailable(why),
        },
        has_editor: host.has_editor(id),
        editor_open: host.editor_open(id),
    }
}

/// How often unsaved work is written to the recovery file.
const AUTOSAVE_EVERY: Duration = Duration::from_secs(60);

/// The strip a new envelope follower listens to: insert 1 (the demo's kick), or insert 2 when
/// the target is on insert 1 itself.
fn follower_source(target: &ParamId) -> usize {
    match *target {
        ParamId::Strip { strip: 1, .. } | ParamId::Effect { strip: 1, .. } => 2,
        _ => 1,
    }
}

/// Effect parameter (strip, slot, index) behind a destination.
fn effect_of(dest: &ParamDest) -> Option<(usize, usize, usize)> {
    match *dest {
        ParamDest::Effect { strip, slot, index } => {
            Some((usize::from(strip), usize::from(slot), usize::from(index)))
        }
        _ => None,
    }
}

impl GloomApp {
    pub fn new(ctx: &egui::Context, open: Option<PathBuf>) -> Self {
        let theme = GloomTheme::default();
        theme.apply(ctx);
        let project = Project::demo();
        let history = History::new(&project);
        let live = gt_engine::LiveInput::default();
        let waker_ctx = ctx.clone();
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("gloomtunes"));
        let (plugins, crash) = PluginHost::new(
            &data_folder().join("plugins"),
            exe,
            Arc::new(move || waker_ctx.request_repaint()),
        );
        let mut app = Self {
            theme,
            audio: AudioIo::new(),
            meter: MeterBallistics::default(),
            // The demo opens on its arrangement, ready to play.
            transport: TransportModel {
                song_mode: true,
                ..TransportModel::default()
            },
            test_tone: false,
            show_audio: false,
            project,
            rack: RackState::default(),
            library: Library::new(),
            browser: BrowserModel::default(),
            preview_pending: None,
            slots_in_use: 0,
            activity: [0.0; MAX_CHANNELS],
            main_view: MainView::Playlist,
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
            playlist: PlaylistState::default(),
            lane_fx: Vec::new(),
            mod_fx: Vec::new(),
            sent_mods: None,
            show_modulators: false,
            modulators: ModulatorsState::default(),
            doc_path: None,
            embed_samples: false,
            edits: 0,
            saved_edits: 0,
            autosaved_edits: 0,
            last_autosave: Instant::now(),
            dialog: None,
            export_form: None,
            export_job: None,
            toast: None,
            quit_confirmed: false,
            window_title: String::new(),
            live: live.clone(),
            midi: crate::midi_io::MidiIo::start(live, ctx.clone()),
            keyboard: crate::live::TypingKeyboard::default(),
            recorder: crate::live::Recorder::default(),
            take: None,
            midi_model: MidiPanelModel::default(),
            sent_live_slot: None,
            learning: None,
            midi_seen: 0,
            plugins,
            crashed_plugins: crash.in_use.clone(),
            show_plugins: false,
            plugin_browser: PluginBrowserState::default(),
            plugin_slot: None,
            fresh_engine: false,
        };
        if let Some((path, plugin, action)) = &crash.quarantined {
            app.toast(format!(
                "{plugin} was switched off: GloomTunes closed while {action} it ({}). \
                 Allow it again in Plugins.",
                path.display()
            ));
        }
        app.plugins.start_scan();
        app.refresh_presets();
        app.open_folder(default_folder());
        app.request_channel_samples();
        app.check_recovery();
        if let Some(path) = open {
            app.open_path(&path);
        }
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

    /// Starts loading every sample the project uses (channels and audio clips).
    fn request_channel_samples(&mut self) {
        let rate = self.rate();
        for ch in &self.project.channels {
            if let Some(src) = ch.sample() {
                self.library.request(src, rate);
            }
        }
        self.request_clip_audio();
    }

    fn request_clip_audio(&mut self) {
        let rate = self.rate();
        for c in &self.project.playlist.clips {
            if let ClipKind::Audio { source, .. } = &c.kind {
                self.library.request(source, rate);
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

    /// Sends what plays: the current pattern (pattern mode) or the playlist (song mode). Audio
    /// clips whose sound is still loading join when it arrives.
    fn push_song(&mut self) {
        let song = if self.transport.song_mode {
            self.request_clip_audio();
            let lib = &self.library;
            SongSnapshot::compile_song(&self.project, |src| {
                lib.get(src).map(|l| std::sync::Arc::clone(&l.data))
            })
        } else {
            SongSnapshot::compile(&self.project)
        };
        let automated: Vec<(usize, usize, usize)> = song
            .automation
            .iter()
            .filter_map(|l| effect_of(&l.dest))
            .collect();
        if self.try_send(EngineCommand::SetSong(Box::new(song))) {
            let old = std::mem::replace(&mut self.lane_fx, automated);
            self.release_fx(old);
        }
        // The song's end is the loop in song mode.
        self.send(EngineCommand::SetLoop(self.loop_region()));
    }

    /// Sends the tempo and time-signature maps, then the song (audio clip times depend on the
    /// tempo).
    fn push_timing(&mut self) {
        self.send(EngineCommand::SetTempoMap(Box::new(
            self.project.tempo.clone(),
        )));
        self.send(EngineCommand::SetSignatures(Box::new(
            self.project.signatures.clone(),
        )));
        self.push_song();
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
        let clips_waiting = self.transport.song_mode
            && ready.iter().any(|src| {
                self.project
                    .playlist
                    .clips
                    .iter()
                    .any(|c| matches!(&c.kind, ClipKind::Audio { source, .. } if source == src))
            });
        if clips_waiting {
            self.push_song();
        }
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
            RackAction::AddPlugin => {
                self.show_plugins = true;
                self.plugin_browser.kind = Some(PluginKind::Instrument);
                self.plugin_slot = None;
            }
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
        // A recording take is one undo step, committed when it ends.
        let busy = ctx.input(|i| i.pointer.any_down())
            || self.recorder.armed
            || ctx.text_edit_focused()
            || self.roll.is_dragging()
            || self.playlist.is_dragging();
        if !busy {
            if self.history.commit(&self.project, label) {
                self.edits += 1;
            }
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
        self.playlist.cancel();
        // Finish the current gesture first so it becomes its own step.
        if let Some(label) = self.pending_edit.take() {
            if self.history.commit(&self.project, label) {
                self.edits += 1;
            }
        }
        let done = if redo {
            self.history.redo(&mut self.project)
        } else {
            self.history.undo(&mut self.project)
        };
        if done.is_some() {
            self.edits += 1;
            self.roll.selected.clear();
            let n = self.project.channels.len();
            self.rack.selected = self.rack.selected.min(n.saturating_sub(1));
            self.push_channels();
            self.push_timing();
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

    /// Effect parameters in `old` that neither the song nor a modulator drives any more go
    /// back to their document values on the next mixer sync. (Channel and strip controls are
    /// reset by the engine itself.)
    fn release_fx(&mut self, old: Vec<(usize, usize, usize)>) {
        for (strip, slot, index) in old {
            if self.lane_fx.contains(&(strip, slot, index))
                || self.mod_fx.contains(&(strip, slot, index))
            {
                continue;
            }
            if let Some(Some(e)) = self.sent_fx.get_mut(strip).and_then(|s| s.get_mut(slot)) {
                if let Some(v) = e.params.get_mut(index) {
                    *v = f32::NAN;
                }
            }
        }
    }

    /// Sends the modulation plan when a modulator, a modulated parameter's value or where it
    /// lives (channel order, effect slots) changed since the last one.
    fn sync_modulation(&mut self) {
        if self.audio.engine().is_none() {
            return;
        }
        let p = &self.project;
        let key: Vec<(Modulator, Option<ParamDest>, f32)> = p
            .modulators
            .iter()
            .map(|m| (*m, ParamDest::resolve(&m.target, p), m.target.normalized(p)))
            .collect();
        if self.sent_mods.as_ref() == Some(&key) {
            return;
        }
        let plan = ModPlan::compile(p);
        let modulated: Vec<(usize, usize, usize)> = plan
            .targets
            .iter()
            .filter_map(|t| effect_of(&t.dest))
            .collect();
        if self.try_send(EngineCommand::SetModulation(Box::new(plan))) {
            self.sent_mods = Some(key);
            let old = std::mem::replace(&mut self.mod_fx, modulated);
            self.release_fx(old);
        }
    }

    /// Which parameters automation clips and enabled modulators drive, for the markers.
    fn param_marks(&self) -> ParamMarks {
        let mut marks = ParamMarks::default();
        for c in &self.project.playlist.clips {
            if let ClipKind::Automation(a) = &c.kind {
                marks.automated.insert(a.target);
            }
        }
        for m in self.project.modulators.iter().filter(|m| m.enabled) {
            marks.modulated.insert(m.target);
        }
        for b in &self.project.midi_map {
            marks
                .midi
                .insert(b.param, format!("CC {}, ch {}", b.cc, b.channel + 1));
        }
        marks
    }

    /// Carries out a choice from a control's right-click menu.
    fn on_param_request(&mut self, req: ParamRequest) {
        match req {
            ParamRequest::Automate(target) => {
                let sigs = &self.project.signatures;
                let start = sigs.bar_start(sigs.bar_of(self.transport.position.0));
                add_automation(&mut self.project, &mut self.playlist, target, start);
                self.main_view = MainView::Playlist;
                // The clip only plays in song mode.
                self.transport.song_mode = true;
                self.push_song();
                self.pending_edit = Some("Create automation clip");
            }
            ParamRequest::AddLfo(target) | ParamRequest::AddFollower(target) => {
                let source = if matches!(req, ParamRequest::AddLfo(_)) {
                    ModSourceKind::default_lfo()
                } else {
                    ModSourceKind::default_follower(follower_source(&target))
                };
                if let Some(id) = self.project.add_modulator(target, source) {
                    if let (ModSourceKind::Follower { .. }, Some(m)) =
                        (source, self.project.modulator_mut(id))
                    {
                        // Followers mostly duck: louder source, lower target.
                        m.amount = -0.5;
                    }
                    self.pending_edit = Some("Add modulator");
                }
                self.show_modulators = true;
                self.modulators.focus = Some(target);
            }
            ParamRequest::MidiLearn(target) => self.start_learning(target),
            ParamRequest::ForgetMidi(target) => {
                if self.project.forget_midi(target) {
                    self.pending_edit = Some("Forget MIDI controller");
                }
            }
            ParamRequest::ShowModulators(target) => {
                self.show_modulators = true;
                self.modulators.focus = Some(target);
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
        self.send(EngineCommand::SetTempoMap(Box::new(
            self.project.tempo.clone(),
        )));
        self.send(EngineCommand::SetSignatures(Box::new(
            self.project.signatures.clone(),
        )));
        self.send(EngineCommand::SetLoop(self.loop_region()));
        self.send(EngineCommand::SetMetronome(self.transport.metronome));
        self.send(EngineCommand::Locate(self.transport.position));
        self.send(EngineCommand::SetTestTone(self.test_tone));
        self.slots_in_use = 0; // a new engine starts with empty slots
        if let Some(p) = self.audio.engine_mut().and_then(|e| e.take_live_producer()) {
            self.live.connect(p);
        }
        self.sent_live_slot = None;
        self.push_channels();
        self.send(EngineCommand::SetScopeChannel(self.scope_slot));
        // A new engine has an empty mixer.
        self.sent_mixer = None;
        self.sent_fx = vec![Default::default(); STRIPS];
        self.lane_fx.clear();
        self.mod_fx.clear();
        self.sent_mods = None;
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
        let bar_ticks = self.project.signatures.sig_of_bar(0).bar_ticks();
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

    /// The loop the engine plays: the user's loop when on; in song mode without one, the
    /// whole song (from bar 1 to the end of the last clip, whole bars).
    fn loop_region(&self) -> LoopRegion {
        let t = &self.transport;
        if t.loop_enabled || !t.song_mode {
            return LoopRegion {
                start: t.loop_start,
                end: t.loop_end,
                enabled: t.loop_enabled,
            };
        }
        let sigs = &self.project.signatures;
        let end = self.project.playlist.song_end();
        let end = sigs.bar_start(sigs.bar_of(end - 1) + 1);
        LoopRegion {
            start: Tick(0),
            end: Tick(end),
            enabled: end > 0,
        }
    }

    /// Where the current pattern is playing, in pattern ticks: the transport position in
    /// pattern mode; in song mode, inside a clip of the current pattern under the playhead.
    fn pattern_position(&self) -> Option<i64> {
        self.pattern_tick_at(self.transport.position.0)
    }

    /// [`Self::pattern_position`] for song position `pos`.
    fn pattern_tick_at(&self, pos: i64) -> Option<i64> {
        let len = self.project.current_pattern().length_ticks().max(1);
        if !self.transport.song_mode {
            return Some(pos.rem_euclid(len));
        }
        let pl = &self.project.playlist;
        pl.clips
            .iter()
            .find(|c| {
                c.kind == ClipKind::Pattern(self.project.current_pattern)
                    && !c.muted
                    && pl.is_track_audible(c.track)
                    && c.start <= pos
                    && pos < c.end()
            })
            .map(|c| (pos - c.start + c.offset).rem_euclid(len))
    }

    fn on_playlist(&mut self, action: PlaylistAction) {
        match action {
            PlaylistAction::Changed => {
                self.push_song();
                self.pending_edit = Some("Edit playlist");
            }
            PlaylistAction::TimingChanged => {
                self.push_timing();
                self.pending_edit = Some("Tempo or time signature");
            }
            PlaylistAction::Locate(t) => self.send(EngineCommand::Locate(Tick(t))),
            PlaylistAction::SetLoop { start, end } => {
                self.transport.loop_start = Tick(start);
                self.transport.loop_end = Tick(end);
                self.transport.loop_enabled = true;
                self.send(EngineCommand::SetLoop(self.loop_region()));
            }
            PlaylistAction::Load(src) => {
                let rate = self.rate();
                self.library.request(&src, rate);
            }
            PlaylistAction::EditPattern(id) => {
                self.project.select_pattern(id);
                self.main_view = MainView::PianoRoll;
                self.push_song();
            }
        }
    }

    /// Sets the tempo of the segment the playhead is in.
    fn set_bpm_at_playhead(&mut self, bpm: f64) {
        let pos = self.transport.position;
        let mut pts: Vec<TempoPoint> = self.project.tempo.points().collect();
        let i = pts.partition_point(|p| p.at <= pos).saturating_sub(1);
        pts[i].bpm = bpm;
        self.project.tempo = gt_core::TempoMap::from_points_lossy(&pts);
        self.push_timing();
        self.pending_edit = Some("Tempo");
    }

    /// Sets the time signature in effect at the playhead.
    fn set_sig_at_playhead(&mut self, sig: gt_core::TimeSig) {
        let bar = self.project.signatures.bar_of(self.transport.position.0);
        let mut ch: Vec<SigChange> = self.project.signatures.changes().collect();
        let i = ch.partition_point(|c| c.bar <= bar).saturating_sub(1);
        ch[i].sig = sig;
        self.project.signatures = TimeSigMap::new(&ch);
        self.push_timing();
        self.pending_edit = Some("Time signature");
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
            TransportAction::Stop => {
                self.send(EngineCommand::Stop);
                self.end_take();
            }
            TransportAction::ToggleRecord => self.toggle_record(),
            TransportAction::ToggleTypingKeyboard => self.keyboard.on = !self.keyboard.on,
            TransportAction::SetBpm(bpm) => self.set_bpm_at_playhead(bpm),
            TransportAction::SetTimeSig(sig) => self.set_sig_at_playhead(sig),
            TransportAction::SetSongMode(_) => self.push_song(),
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

/// The project file: new, open, save, export, autosave and recovery.
impl GloomApp {
    fn is_dirty(&self) -> bool {
        self.edits != self.saved_edits || self.pending_edit.is_some()
    }

    /// The project's name: its file name without extension, or "Untitled".
    fn doc_name(&self) -> String {
        self.doc_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map_or_else(
                || "Untitled".to_owned(),
                |s| s.to_string_lossy().into_owned(),
            )
    }

    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    fn recovery_dir(&self) -> PathBuf {
        data_folder().join("recovery")
    }

    /// Replaces the document: stops playback, clears undo history (it lives only within a
    /// session and a document) and sends everything to the engine.
    fn replace_project(&mut self, project: gt_core::Project, path: Option<PathBuf>) {
        self.send(EngineCommand::Stop);
        self.send(EngineCommand::Locate(Tick(0)));
        self.recorder = crate::live::Recorder::default();
        self.take = None;
        self.learning = None;
        self.cancel_roll_gesture();
        self.playlist.cancel();
        self.pending_edit = None;
        self.project = project;
        self.history = History::new(&self.project);
        self.edits = 0;
        self.saved_edits = 0;
        self.autosaved_edits = 0;
        self.doc_path = path;
        self.rack.selected = 0;
        self.roll.selected.clear();
        self.playlist = PlaylistState::default();
        self.modulators = ModulatorsState::default();
        self.sent_mods = None;
        self.transport.position = Tick(0);
        self.request_channel_samples();
        self.push_channels();
        self.push_timing();
    }

    /// Opens a project file, reporting problems in the Open dialog or the relink dialog.
    fn open_path(&mut self, path: &std::path::Path) {
        match gt_project::file::load(path, &data_folder().join("embedded")) {
            Ok(loaded) => {
                for w in &loaded.warnings {
                    log::warn!("{}: {w}", path.display());
                }
                let missing = loaded.missing.clone();
                self.replace_project(loaded.project, Some(path.to_path_buf()));
                self.embed_samples = loaded.extracted > 0;
                self.dialog = None;
                let mut msg = format!("Opened {}", self.doc_name());
                if !loaded.warnings.is_empty() {
                    msg.push_str(&format!(
                        " ({} item{} repaired, see the log)",
                        loaded.warnings.len(),
                        if loaded.warnings.len() == 1 { "" } else { "s" }
                    ));
                }
                self.toast(msg);
                if !missing.is_empty() {
                    let folder = path
                        .parent()
                        .map_or_else(default_folder, std::path::Path::to_path_buf);
                    self.dialog = Some(Dialog::Relink {
                        missing,
                        folder: folder.display().to_string(),
                        message: None,
                    });
                }
            }
            Err(e) => {
                self.dialog = Some(Dialog::Open {
                    path: path.display().to_string(),
                    error: Some(e.to_string()),
                });
            }
        }
    }

    /// Saves to `path`. Returns false (with the reason in the Save as dialog) if it failed.
    fn save_to(&mut self, path: PathBuf, embed: bool, then: Then) -> bool {
        // An edit still in progress belongs to the saved state.
        if let Some(label) = self.pending_edit.take() {
            if self.history.commit(&self.project, label) {
                self.edits += 1;
            }
        }
        self.plugins.store_states(&mut self.project);
        match gt_project::file::save(
            &self.project,
            &path,
            gt_project::file::SaveOptions {
                embed_samples: embed,
            },
        ) {
            Ok(report) => {
                self.doc_path = Some(path);
                self.embed_samples = embed;
                self.saved_edits = self.edits;
                let mut msg = format!("Saved {}", self.doc_name());
                if report.embedded > 0 {
                    msg.push_str(&format!(" with {} embedded samples", report.embedded));
                }
                if !report.unreadable.is_empty() {
                    msg.push_str(&format!(
                        "; {} sample file(s) could not be read to embed",
                        report.unreadable.len()
                    ));
                }
                self.toast(msg);
                self.dialog = None;
                self.then(then);
                true
            }
            Err(e) => {
                self.dialog = Some(Dialog::SaveAs {
                    path: path.display().to_string(),
                    embed,
                    error: Some(format!("Could not save: {e}")),
                    then,
                });
                false
            }
        }
    }

    /// Save: to the current file, or ask where.
    fn save(&mut self, then: Then) {
        match self.doc_path.clone() {
            Some(p) => {
                self.save_to(p, self.embed_samples, then);
            }
            None => self.save_as(then),
        }
    }

    fn save_as(&mut self, then: Then) {
        let path = self.doc_path.clone().unwrap_or_else(|| {
            default_folder().join(format!("Untitled.{}", gt_project::file::EXTENSION))
        });
        self.dialog = Some(Dialog::SaveAs {
            path: path.display().to_string(),
            embed: self.embed_samples,
            error: None,
            then,
        });
    }

    /// New, Open or Quit: asks to save unsaved changes first.
    fn request(&mut self, then: Then) {
        if self.is_dirty() {
            self.dialog = Some(Dialog::Unsaved {
                name: self.doc_name(),
                then,
            });
        } else {
            self.then(then);
        }
    }

    fn then(&mut self, then: Then) {
        match then {
            Then::Stay => {}
            Then::New => self.replace_project(gt_core::Project::empty(), None),
            Then::Open => {
                let dir = self
                    .doc_path
                    .as_ref()
                    .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
                    .unwrap_or_else(default_folder);
                self.dialog = Some(Dialog::Open {
                    path: format!("{}{}", dir.display(), std::path::MAIN_SEPARATOR),
                    error: None,
                });
            }
            Then::Quit => self.quit_confirmed = true,
        }
    }

    fn show_export(&mut self) {
        let form = self.export_form.clone().unwrap_or_else(|| {
            let dir = self
                .doc_path
                .as_ref()
                .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
                .unwrap_or_else(default_folder);
            ExportForm::new(dir.join(format!("{}.wav", self.doc_name())))
        });
        self.dialog = Some(Dialog::Export(form));
    }

    fn start_export(&mut self, path: PathBuf) {
        let Some(Dialog::Export(form)) = &mut self.dialog else {
            return;
        };
        let t = &self.transport;
        let settings = form.settings((t.loop_start.0, t.loop_end.0));
        if gt_export::range_ticks(&self.project, settings.range).is_none() {
            form.message = Some(if form.loop_only {
                "The loop region is empty or has no clips in it.".to_owned()
            } else {
                "The playlist is empty: add clips first.".to_owned()
            });
            return;
        }
        let progress = Arc::new(gt_export::Progress::default());
        form.running = Some(Arc::clone(&progress));
        form.message = None;
        let project = self.project.clone();
        // Fresh plugin instances in their current state, rendering offline.
        let mut plugins = self
            .plugins
            .export_processors(&project, settings.sample_rate);
        self.export_job = Some(std::thread::spawn(move || {
            gt_export::export(&project, &settings, &path, &progress, &mut plugins)
        }));
    }

    /// Picks up a finished export.
    fn poll_export(&mut self) {
        if !self.export_job.as_ref().is_some_and(|j| j.is_finished()) {
            return;
        }
        let Some(job) = self.export_job.take() else {
            return;
        };
        self.plugins.finish_export();
        let result = job
            .join()
            .unwrap_or_else(|_| Err(gt_export::ExportError::Io(std::io::Error::other("crashed"))));
        let msg = match result {
            Ok(files) if files.len() == 1 => format!("Exported {}", files[0].display()),
            Ok(files) => format!(
                "Exported {} stems to {}",
                files.len(),
                files[0]
                    .parent()
                    .map_or_else(String::new, |p| p.display().to_string())
            ),
            Err(e) => format!("Export failed: {e}"),
        };
        if let Some(Dialog::Export(form)) = &mut self.dialog {
            form.running = None;
            form.message = Some(msg.clone());
            self.export_form = Some(form.clone());
        }
        self.toast(msg);
    }

    fn on_dialog(&mut self, action: DialogAction) {
        match action {
            DialogAction::Open(path) => {
                let path = files::with_project_extension(path);
                self.open_path(&path);
            }
            DialogAction::SaveAs { path, embed, then } => {
                self.save_to(path, embed, then);
            }
            DialogAction::Export { path } => self.start_export(path),
            DialogAction::ImportMidi { path, timing } => self.import_midi(&path, timing),
            DialogAction::ExportMidi { path, song } => self.export_midi(&path, song),
            DialogAction::CancelExport => {
                if let Some(Dialog::Export(form)) = &self.dialog {
                    if let Some(p) = &form.running {
                        p.cancel.store(true, Ordering::Relaxed);
                    }
                }
            }
            DialogAction::RelinkSearch(dir) => {
                let Some(Dialog::Relink { missing, .. }) = &self.dialog else {
                    return;
                };
                let found = gt_project::file::find_by_name(&dir, missing, 6);
                let n = found.len();
                for (from, to) in &found {
                    gt_project::file::relink(&mut self.project, from, to);
                }
                if n > 0 {
                    self.pending_edit = Some("Relink samples");
                    self.request_channel_samples();
                    self.push_channels();
                }
                if let Some(Dialog::Relink {
                    missing, message, ..
                }) = &mut self.dialog
                {
                    missing.retain(|m| !found.contains_key(m));
                    *message = Some(format!("Found {n} in {}.", dir.display()));
                    if missing.is_empty() {
                        self.dialog = None;
                        self.toast(format!(
                            "Relinked {n} sample{}",
                            if n == 1 { "" } else { "s" }
                        ));
                    }
                }
            }
            DialogAction::RelinkFile { from, to } => {
                gt_project::file::relink(&mut self.project, &from, &to);
                self.pending_edit = Some("Relink samples");
                self.request_channel_samples();
                self.push_channels();
                if let Some(Dialog::Relink { missing, .. }) = &mut self.dialog {
                    missing.retain(|m| *m != from);
                    if missing.is_empty() {
                        self.dialog = None;
                    }
                }
            }
            DialogAction::Recover(without_plugins) => self.recover(without_plugins),
            DialogAction::DiscardRecovery => {
                self.clear_recovery();
                self.dialog = None;
            }
            DialogAction::Unsaved { save, then } => {
                self.dialog = None;
                if save {
                    self.save(then);
                } else {
                    self.then(then);
                }
            }
            DialogAction::Close => {
                if let Some(Dialog::Export(form)) = self.dialog.take() {
                    self.export_form = Some(form);
                }
            }
        }
    }

    /// At start-up: if the last session did not exit cleanly and left an autosave, offer it.
    /// Then mark this session as running.
    fn check_recovery(&mut self) {
        let dir = self.recovery_dir();
        let lock = dir.join("session.lock");
        let autosave = dir.join(format!("autosave.{}", gt_project::file::EXTENSION));
        if lock.is_file() && autosave.is_file() {
            let when = std::fs::metadata(&autosave)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map_or_else(
                    || "the last session".to_owned(),
                    |d| match d.as_secs() {
                        s if s < 120 => format!("{s} seconds ago"),
                        s if s < 7200 => format!("{} minutes ago", s / 60),
                        s => format!("{} hours ago", s / 3600),
                    },
                );
            let plugins = self
                .crashed_plugins
                .iter()
                .map(|p| {
                    p.file_stem().map_or_else(
                        || p.display().to_string(),
                        |s| s.to_string_lossy().into_owned(),
                    )
                })
                .collect();
            self.dialog = Some(Dialog::Recover { when, plugins });
        }
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(&lock, std::process::id().to_string());
        }
    }

    fn recover(&mut self, without_plugins: bool) {
        let dir = self.recovery_dir();
        let autosave = dir.join(format!("autosave.{}", gt_project::file::EXTENSION));
        let original = std::fs::read_to_string(dir.join("autosave-path.txt"))
            .ok()
            .map(|s| PathBuf::from(s.trim()))
            .filter(|p| !p.as_os_str().is_empty());
        match gt_project::file::load(&autosave, &data_folder().join("embedded")) {
            Ok(loaded) => {
                let missing = loaded.missing.clone();
                self.replace_project(loaded.project, original);
                if without_plugins {
                    self.plugins.hold(
                        &self.project,
                        "switched off after the crash; Retry to load it",
                    );
                }
                // Recovered work is not saved anywhere yet.
                self.edits = 1;
                self.dialog = None;
                self.toast("Recovered the autosaved project; save it to keep it");
                if !missing.is_empty() {
                    self.dialog = Some(Dialog::Relink {
                        missing,
                        folder: default_folder().display().to_string(),
                        message: None,
                    });
                }
            }
            Err(e) => {
                self.dialog = None;
                self.toast(format!("Could not recover: {e}"));
            }
        }
    }

    fn clear_recovery(&self) {
        let dir = self.recovery_dir();
        let _ = std::fs::remove_file(dir.join(format!("autosave.{}", gt_project::file::EXTENSION)));
        let _ = std::fs::remove_file(dir.join("autosave-path.txt"));
    }

    /// Writes the recovery file every minute while there are unsaved changes.
    fn autosave(&mut self) {
        if self.edits == self.autosaved_edits
            || self.pending_edit.is_some()
            || self.last_autosave.elapsed() < AUTOSAVE_EVERY
        {
            return;
        }
        self.last_autosave = Instant::now();
        if !self.is_dirty() {
            // Saved since: nothing to recover.
            self.autosaved_edits = self.edits;
            self.clear_recovery();
            return;
        }
        let dir = self.recovery_dir();
        let path = dir.join(format!("autosave.{}", gt_project::file::EXTENSION));
        self.plugins.store_states(&mut self.project);
        let result = std::fs::create_dir_all(&dir)
            .map_err(gt_project::file::FileError::from)
            .and_then(|()| gt_project::file::save(&self.project, &path, Default::default()));
        match result {
            Ok(_) => {
                let original = self
                    .doc_path
                    .as_ref()
                    .map_or_else(String::new, |p| p.display().to_string());
                let _ = std::fs::write(dir.join("autosave-path.txt"), original);
                self.autosaved_edits = self.edits;
                log::info!("autosaved to {}", path.display());
            }
            Err(e) => log::warn!("autosave failed: {e}"),
        }
    }

    /// Clean exit: no recovery needed next time.
    fn end_session(&self) {
        self.clear_recovery();
        let _ = std::fs::remove_file(self.recovery_dir().join("session.lock"));
    }

    /// File menu and shortcuts.
    fn file_menu(&mut self, ui: &mut egui::Ui) {
        let mut pick = None;
        ui.menu_button("File", |ui| {
            for (label, then, keys) in [
                ("New", Some(Then::New), "Ctrl+N"),
                ("Open…", Some(Then::Open), "Ctrl+O"),
            ] {
                if ui
                    .add(egui::Button::new(label).shortcut_text(keys))
                    .clicked()
                {
                    pick = then.map(FileCmd::Request);
                }
            }
            ui.separator();
            if ui
                .add(egui::Button::new("Save").shortcut_text("Ctrl+S"))
                .clicked()
            {
                pick = Some(FileCmd::Save);
            }
            if ui
                .add(egui::Button::new("Save as…").shortcut_text("Ctrl+Shift+S"))
                .clicked()
            {
                pick = Some(FileCmd::SaveAs);
            }
            ui.separator();
            if ui
                .add(egui::Button::new("Export audio…").shortcut_text("Ctrl+Shift+E"))
                .clicked()
            {
                pick = Some(FileCmd::Export);
            }
            ui.separator();
            if ui.button("Import MIDI file…").clicked() {
                pick = Some(FileCmd::ImportMidi);
            }
            if ui.button("Export MIDI file…").clicked() {
                pick = Some(FileCmd::ExportMidi);
            }
        });
        if let Some(cmd) = pick {
            self.file_cmd(cmd);
        }
    }

    fn file_cmd(&mut self, cmd: FileCmd) {
        if self.export_job.is_some() {
            self.toast("Wait for the export to finish");
            return;
        }
        match cmd {
            FileCmd::Request(then) => self.request(then),
            FileCmd::Save => self.save(Then::Stay),
            FileCmd::SaveAs => self.save_as(Then::Stay),
            FileCmd::Export => self.show_export(),
            FileCmd::ImportMidi => self.show_import_midi(),
            FileCmd::ExportMidi => self.show_export_midi(),
        }
    }

    /// Ctrl+N/O/S, Ctrl+Shift+S and Ctrl+Shift+E.
    fn file_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        let key = |m, k| ctx.input_mut(|i| i.consume_key(m, k));
        let cmd = if key(cs, Key::S) {
            Some(FileCmd::SaveAs)
        } else if key(cs, Key::E) {
            Some(FileCmd::Export)
        } else if key(Modifiers::COMMAND, Key::S) {
            Some(FileCmd::Save)
        } else if key(Modifiers::COMMAND, Key::O) {
            Some(FileCmd::Request(Then::Open))
        } else if key(Modifiers::COMMAND, Key::N) {
            Some(FileCmd::Request(Then::New))
        } else {
            None
        };
        if let Some(c) = cmd {
            self.file_cmd(c);
        }
    }

    /// Dialogs, the export job, autosave, the window title, closing and the toast.
    fn document_frame(&mut self, ctx: &egui::Context, theme: &GloomTheme) {
        self.poll_export();
        if let Some(mut d) = self.dialog.take() {
            let action = files::show(ctx, theme, &mut d);
            self.dialog = Some(d);
            if let Some(a) = action {
                self.on_dialog(a);
            }
        }
        if self.export_job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        self.autosave();

        // Closing the window: ask about unsaved changes first.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_confirmed {
            if self.is_dirty() || self.export_job.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if self.export_job.is_none() {
                    self.request(Then::Quit);
                }
            } else {
                self.quit_confirmed = true;
            }
        }
        if self.quit_confirmed && !ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        let title = format!(
            "{}{} - GloomTunes Studio",
            self.doc_name(),
            if self.is_dirty() { " *" } else { "" }
        );
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }

        if let Some((text, at)) = &self.toast {
            let age = at.elapsed().as_secs_f32();
            if age > 5.0 {
                self.toast = None;
            } else {
                egui::Area::new(egui::Id::new("toast"))
                    .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            // Short messages stay on one line; long ones (errors with paths)
                            // wrap at a fixed width instead of the area's shrunken one.
                            let wrap = if text.chars().count() > 90 {
                                ui.set_width(480.0);
                                egui::TextWrapMode::Wrap
                            } else {
                                egui::TextWrapMode::Extend
                            };
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(text.as_str()).color(theme.text),
                                )
                                .wrap_mode(wrap),
                            );
                        });
                    });
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        }
    }
}

/// A File menu command.
#[derive(Debug, Clone, Copy)]
enum FileCmd {
    Request(Then),
    Save,
    SaveAs,
    Export,
    ImportMidi,
    ExportMidi,
}

impl eframe::App for GloomApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.plugins.shutdown();
        self.end_session();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.audio.poll();
        if self.audio.take_fresh_engine() {
            self.sync_new_engine();
            self.fresh_engine = true;
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
        // The tempo and signature shown are the ones at the playhead.
        self.transport.bpm = self.project.tempo.bpm_at(self.transport.position);
        self.transport.time_sig = self.project.signatures.sig_at(self.transport.position);
        param_ui::set_marks(&ctx, std::sync::Arc::new(self.param_marks()));
        // Before any shortcut: in piano mode the typing keyboard takes its keys first.
        self.live_frame(&ctx, dt);

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
            if key(Modifiers::NONE, Key::F5) {
                self.main_view = MainView::Playlist;
            }
            if key(Modifiers::NONE, Key::F6) {
                self.main_view = MainView::Rack;
            }
            if key(Modifiers::NONE, Key::L) {
                self.transport.song_mode = !self.transport.song_mode;
                self.push_song();
            }
            if key(Modifiers::NONE, Key::F7) {
                self.main_view = MainView::PianoRoll;
            }
            if key(Modifiers::NONE, Key::F9) {
                self.main_view = MainView::Mixer;
            }
        }
        if self.dialog.is_none() {
            self.file_shortcuts(&ctx);
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
                    self.file_menu(ui);
                    ui.separator();
                    let a =
                        transport_bar(ui, &theme, &mut self.transport, &self.project.signatures);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::selectable(self.show_audio, "Audio"))
                            .on_hover_text("Audio device, MIDI inputs and recording settings")
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
            let action = egui::Window::new("Audio and MIDI settings")
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .show(&ctx, |ui| {
                    let a = audio_panel(ui, &theme, &model);
                    ui.separator();
                    self.midi_settings(ui, &theme);
                    a
                })
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
        let show_instrument = matches!(self.main_view, MainView::Rack | MainView::PianoRoll);
        let mut plugin_actions = Vec::new();
        let mut plugin_changed = false;
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
                    let channel = Some(ch.id);
                    match &mut ch.instrument {
                        Instrument::Sampler(s) => {
                            let loaded = s.sample.as_ref().and_then(|x| self.library.get(x));
                            let status = s.sample.as_ref().and_then(|x| self.library.status(x));
                            let view = SamplerPanelView {
                                waveform: loaded.map(|l| l.overview.as_slice()),
                                seconds: loaded.map(|l| l.data.seconds()),
                                status: status.as_deref(),
                                channel,
                            };
                            (sampler_panel(ui, &theme, &mut ch.name, s, view), Vec::new())
                        }
                        Instrument::Synth(patch) => {
                            let view = SynthPanelView {
                                scope: &self.scope,
                                user_presets: &self.user_presets,
                                sample_rate,
                                status: self.preset_status.as_deref(),
                                channel,
                            };
                            (false, synth_panel(ui, &theme, &mut ch.name, patch, view))
                        }
                        Instrument::Plugin(p) => {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(&p.name).strong().color(theme.accent));
                                ui.add(
                                    egui::TextEdit::singleline(&mut ch.name).desired_width(140.0),
                                )
                                .on_hover_text("Channel name");
                            });
                            let view = plugin_view(&self.plugins, p.instance);
                            let owner = gt_core::PluginOwner::Channel(ch.id);
                            plugin_changed =
                                plugin_controls(ui, &theme, owner, p, &view, &mut plugin_actions);
                            (false, Vec::new())
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
        if plugin_changed {
            self.pending_edit = Some("Plugin parameter");
        }

        let playing = self.transport.state == PlayState::Playing;
        let pattern_pos = if playing {
            self.pattern_position()
        } else {
            None
        };
        if self.main_view == MainView::Mixer {
            self.read_meters(dt);
        }
        let mut mixer_changed = false;
        let mut playlist_actions = Vec::new();
        let (rack_actions, roll_actions) = egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (v, label, tip) in [
                        (MainView::Playlist, "Playlist", "Arrangement of clips (F5)"),
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
                    ui.separator();
                    let n = self.project.modulators.len();
                    let label = if n > 0 {
                        format!("Modulators ({n})")
                    } else {
                        "Modulators".to_owned()
                    };
                    if ui
                        .add(egui::Button::selectable(self.show_modulators, label))
                        .on_hover_text(
                            "LFOs and envelope followers. Right-click any knob or fader to add one",
                        )
                        .clicked()
                    {
                        self.show_modulators = !self.show_modulators;
                    }
                });
                ui.separator();
                match self.main_view {
                    MainView::Playlist => {
                        let r = self.loop_region();
                        playlist_actions = playlist(
                            ui,
                            &theme,
                            &mut self.project,
                            &mut self.playlist,
                            PlaylistView {
                                playhead: Some(self.transport.position.0),
                                loop_region: (r.start.0, r.end.0, r.enabled),
                                audio: &self.library,
                            },
                        );
                        (Vec::new(), Vec::new())
                    }
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
                        let open_plugin = self.mixer_state.slot.and_then(|k| {
                            let strip = &self.project.mixer.strips
                                [self.mixer_state.selected.min(STRIPS - 1)];
                            let p = strip.slots.get(k)?.as_ref()?.plugin.as_ref()?;
                            Some(plugin_view(&self.plugins, p.instance))
                        });
                        let view = MixerView {
                            meters: &self.strip_view,
                            fx_meters: &self.fx_meters,
                            sample_rate,
                            latency_ms: latency as f32 * 1000.0 / sample_rate,
                            plugin: open_plugin.as_ref(),
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
        for a in playlist_actions {
            self.on_playlist(a);
        }
        if mixer_changed {
            self.pending_edit = Some("Mixer");
        }
        if let Some(target) = self.mixer_state.plugin_request.take() {
            self.show_plugins = true;
            self.plugin_browser.kind = Some(PluginKind::Effect);
            self.plugin_slot = Some(target);
        }
        plugin_actions.append(&mut self.mixer_state.plugin_actions);
        for a in plugin_actions {
            self.on_plugin_action(a);
        }
        if self.show_plugins {
            self.plugin_window(&ctx, &theme);
        }
        if self.show_modulators {
            let mut open = true;
            let mut changed = false;
            egui::Window::new("Modulators")
                .open(&mut open)
                .default_width(560.0)
                .min_width(560.0)
                .default_pos(ctx.content_rect().center() - egui::vec2(280.0, 100.0))
                .show(&ctx, |ui| {
                    changed = modulators_panel(ui, &theme, &mut self.project, &mut self.modulators);
                });
            self.show_modulators = open;
            if changed {
                self.pending_edit = Some("Modulators");
            }
        }
        if let Some(req) = param_ui::take_request(&ctx) {
            self.on_param_request(req);
        }
        self.sync_plugins(&ctx);
        self.sync_mixer();
        self.sync_modulation();
        if self.main_view != MainView::PianoRoll {
            // Hidden mid-gesture (F6, tab click): the roll never sees the release.
            self.cancel_roll_gesture();
        }
        self.commit_if_idle(&ctx);
        self.document_frame(&ctx, &theme);

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
