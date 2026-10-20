//! The eframe application: owns the device layer, the project document and the transport
//! settings, and draws the UI.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_core::effects::IMPULSE_RESPONSES;
use gt_core::{
    ClipKind, EffectKind, EffectSlot, Instrument, ModSourceKind, Modulator, ParamId,
    PluginInstanceId, PluginKind, Project, SampleData, SampleSource, SigChange, TempoPoint, Tick,
    TimeSigMap, FX_SLOTS, MAX_CHANNELS, ROOT_KEY, STRIPS,
};
use gt_dsp::analysis::Scope;
use gt_engine::{
    create_effect_with, ChannelParams, EngineCommand, LoopRegion, MixerParams, ModPlan, ParamDest,
    SongSnapshot, TransportState,
};
use gt_plugin_host::{PluginHost, PluginStatus};
use gt_project::{ops, presets, strip_presets, History};
use gt_ui::param_ui::{self, ParamMarks, ParamRequest};
use gt_ui::views::{
    add_automation, browser, modulators_panel, piano_roll, plugin_controls, sampler_panel,
    synth_panel, transport_bar, AudioAction, BrowserModel, GhostChannel, MidiPanelModel,
    MixerState, ModulatorsState, PianoRollAction, PianoRollState, PianoRollView, PlayState,
    PlaylistAction, PlaylistState, PluginBrowserState, PluginPanelView, PluginState, RackAction,
    RackState, SamplerPanelView, StripMeter, SynthPanelAction, SynthPanelView, TransportAction,
    TransportModel,
};
use gt_ui::views::{FirstRunState, SettingsPage, ShortcutEditorState};
use gt_ui::widgets::MeterBallistics;
use gt_ui::{Command, GloomTheme, Keymap};

/// What a synth channel's wavetables were built from.
#[derive(Debug, Clone, PartialEq)]
struct TablesKey {
    tables: [Option<gt_core::SampleSource>; 2],
    loaded: [bool; 2],
}

mod automate;
mod bounce;
mod browse;
mod controllers;
mod edit_audio;
mod hardware;
mod midi;
mod perform;
mod plugins;
mod prefs;
mod record;
mod stretch;
mod views;

use crate::audio_io::AudioIo;
use crate::files::{self, Dialog, DialogAction, ExportForm, Then};
use crate::library::{data_folder, default_folder, packs_folder, synth_preset_folder, Library};

/// Sample rate used for loading while no device is open.
const FALLBACK_RATE: u32 = 48_000;
/// Activity lights fall by this much per second.
const ACTIVITY_FALL_PER_S: f32 = 4.0;

use crate::layout::{Layout, MainView};
use gt_ui::tr;

/// What the engine last received for one effect slot, so only differences are sent.
#[derive(Debug, Clone, PartialEq)]
struct SentEffect {
    kind: EffectKind,
    params: Vec<f32>,
    file: Option<PathBuf>,
}

pub struct GloomApp {
    theme: GloomTheme,
    audio: AudioIo,
    meter: MeterBallistics,
    transport: TransportModel,
    test_tone: bool,
    show_settings: bool,
    settings_page: SettingsPage,
    /// Saved preferences; written back by `save_settings`.
    settings: crate::settings::Settings,
    /// A preference changed that `save_settings` cannot detect by comparison alone.
    settings_dirty: bool,
    settings_checked: Instant,
    keymap: Keymap,
    shortcut_state: ShortcutEditorState,
    /// The first-run wizard while it is open.
    wizard: Option<FirstRunState>,
    /// A crash report saved last time (path and text), until the user dismisses it.
    crash_report: Option<(PathBuf, String)>,
    perf: prefs::Perf,

    project: Project,
    rack: RackState,
    library: Library,
    browser: BrowserModel,
    /// Sound the user clicked in the browser, waiting to load before it can be previewed.
    preview_pending: Option<SampleSource>,
    /// Engine slots that may hold a sample (cleared when channels are removed).
    slots_in_use: usize,
    activity: [f32; MAX_CHANNELS],

    /// Docked and floating views, the browser panel (Prompt 25).
    layout: Layout,
    /// A floating view to bring to the front this frame.
    focus_view: Option<MainView>,
    browse: browse::BrowseState,
    /// Tempo of the sound being previewed, when it follows the project tempo.
    preview_bpm: Option<f32>,
    /// A project to open once unsaved changes are dealt with.
    pending_open: Option<PathBuf>,
    show_history: bool,
    show_info: bool,
    show_help: bool,
    help: gt_ui::views::HelpState,
    /// The keymap the views read, re-installed when it changes.
    installed_keys: Option<Arc<Keymap>>,
    /// Applied UI scale.
    ui_scale: f32,
    roll: PianoRollState,
    /// Shown bank and pad of the drum pad panel.
    drum_state: gt_ui::views::DrumPadState,
    /// Sound packs in the packs folder (name, path), Prompt 28.
    packs: Vec<(String, PathBuf)>,
    /// Packs opened for the player panel, or why they did not open.
    opened_packs: HashMap<PathBuf, Result<gt_project::pack::Pack, String>>,
    /// Set by the worker that builds missing factory packs when it finishes.
    packs_built: Arc<std::sync::atomic::AtomicBool>,
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
    /// Mixer strip and effect-chain presets (Prompt 29).
    strip_presets: Vec<(String, std::path::PathBuf)>,
    chain_presets: Vec<(String, std::path::PathBuf)>,
    strip_preset_status: Option<String>,

    mixer_state: MixerState,
    /// Mixer settings and effects as last sent to the engine (none: send everything).
    sent_mixer: Option<MixerParams>,
    sent_fx: Vec<[Option<SentEffect>; FX_SLOTS]>,
    /// Per channel slot, what its synth's wavetables were built from (none: no tables sent).
    sent_tables: Vec<Option<TablesKey>>,
    strip_meters: Vec<[MeterBallistics; 2]>,
    strip_view: Vec<StripMeter>,
    fx_meters: Vec<[f32; FX_SLOTS]>,
    /// Signal taps of the effects that have one (analyzers), by strip and slot.
    scopes: Vec<[Option<Arc<Scope>>; FX_SLOTS]>,
    /// Audio files loaded for effects (convolution responses), at the engine rate.
    effect_files: HashMap<PathBuf, Arc<SampleData>>,

    playlist: PlaylistState,
    /// The open audio editor (Prompt 32).
    audio_editor: Option<gt_ui::views::AudioEditor>,
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
    /// Render to audio, consolidate or freeze in progress.
    bounce_job: Option<bounce::BounceJob>,
    /// Renders of stretched audio clips.
    stretch: stretch::StretchCache,
    /// The tempo map last sent to the engine ([`Project::song_tempo`]).
    song_tempo: gt_core::TempoMap,
    /// Performance mode's launches (Prompt 31).
    show: gt_core::Performance,
    /// Record the performance into a new arrangement when playback stops.
    perf_record: bool,
    /// Where the performance is laid out to.
    perf_horizon: i64,
    /// The furthest the playhead got while performing.
    perf_end: i64,
    /// A performance is playing (it is recorded when playback stops).
    perf_live: bool,
    /// A short message shown at the bottom right, and when it appeared.
    toast: Option<(String, Instant)>,
    /// The user confirmed quitting (or there was nothing to save).
    quit_confirmed: bool,
    window_title: String,

    /// Live notes from MIDI devices and the typing keyboard, to the engine.
    live: gt_engine::LiveInput,
    midi: crate::midi_io::MidiIo,
    /// The starting per-note values (MPE) of live notes being recorded, by slot and key.
    note_expr: std::collections::HashMap<(u16, u8), gt_engine::NoteExpr>,
    /// MIDI output ports (MIDI out channels, clock, time code).
    midi_out: crate::midi_out_io::MidiOutIo,
    /// Clock and time code out, following an external clock (Prompt 33).
    sync: hardware::SyncState,
    /// Controller mappings and scripts (D149).
    controllers: controllers::Controllers,
    keyboard: crate::live::TypingKeyboard,
    recorder: crate::live::Recorder,
    /// The take under way.
    take: Option<midi::Take>,
    /// Step recording.
    step: crate::live::StepRecorder,
    /// Playhead last seen while step recording (a jump moves the cursor).
    step_seen: Tick,
    /// MIDI and recording settings (shown in the Audio and MIDI window).
    midi_model: MidiPanelModel,
    /// Live channel slot last sent to the engine (outer none: send it).
    sent_live_slot: Option<Option<u16>>,
    /// The input routes the engine has (None after a restart).
    sent_routes: Option<gt_engine::LiveRoutes>,
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
    /// Audio recording: input routes, the session, the latency measurement.
    record: record::RecordState,
    /// Automation recording and live values.
    auto: automate::AutoState,
}

/// What the plugin panels show for plugin `id`.
fn plugin_view(host: &PluginHost, project: &Project, id: PluginInstanceId) -> PluginPanelView {
    PluginPanelView {
        state: match host.status(id) {
            PluginStatus::Running => PluginState::Running,
            PluginStatus::Waiting => PluginState::Waiting,
            PluginStatus::Failed => PluginState::Failed,
            PluginStatus::Unavailable(why) => PluginState::Unavailable(why),
        },
        has_editor: host.has_editor(id),
        editor_open: host.editor_open(id),
        texts: host.param_texts(id),
        presets: host
            .listed_presets(id)
            .map(|list| list.iter().map(|p| (p.name.clone(), p.user)).collect()),
        strips: project
            .mixer
            .strips
            .iter()
            .map(|s| s.name.clone())
            .collect(),
        midi_targets: project
            .channels
            .iter()
            .filter(
                |c| !matches!(&c.instrument, gt_core::Instrument::Plugin(p) if p.instance == id),
            )
            .map(|c| (c.id, c.name.clone()))
            .collect(),
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
    pub fn new(ctx: &egui::Context, open: Option<PathBuf>, boot: crate::Boot) -> Self {
        let settings = boot.settings;
        let theme = settings.theme.to_theme();
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
            audio: AudioIo::new(&settings.audio),
            meter: MeterBallistics::default(),
            // The demo opens on its arrangement, ready to play.
            transport: TransportModel {
                song_mode: true,
                ..TransportModel::default()
            },
            test_tone: false,
            show_settings: false,
            settings_page: SettingsPage::default(),
            keymap: settings.keymap(),
            shortcut_state: ShortcutEditorState::default(),
            wizard: (!settings.first_run_done).then(FirstRunState::default),
            crash_report: Self::find_crash_report(),
            perf: prefs::Perf::new(boot.started, boot.renderer),
            settings_dirty: false,
            settings_checked: Instant::now(),
            project,
            rack: RackState::default(),
            library: Library::new(),
            browser: BrowserModel::default(),
            preview_pending: None,
            slots_in_use: 0,
            activity: [0.0; MAX_CHANNELS],
            layout: Layout::from_prefs(&settings.ui.layout),
            focus_view: None,
            browse: browse::BrowseState::default(),
            preview_bpm: None,
            pending_open: None,
            show_history: false,
            show_info: false,
            show_help: false,
            help: gt_ui::views::HelpState::default(),
            installed_keys: None,
            ui_scale: 0.0,
            roll: PianoRollState::default(),
            drum_state: gt_ui::views::DrumPadState::default(),
            packs: list_packs(),
            opened_packs: HashMap::new(),
            packs_built: build_factory_packs(),
            history,
            pending_edit: None,
            humanize_seed: 0x9E37_79B9_7F4A_7C15,
            scope_slot: None,
            scope: vec![0.0; gt_engine::SCOPE_LEN],
            preset_dir: synth_preset_folder(),
            user_presets: Vec::new(),
            preset_status: None,
            strip_presets: Vec::new(),
            chain_presets: Vec::new(),
            strip_preset_status: None,
            mixer_state: MixerState::default(),
            sent_mixer: None,
            sent_fx: vec![Default::default(); STRIPS],
            sent_tables: Vec::new(),
            strip_meters: vec![Default::default(); STRIPS],
            strip_view: vec![StripMeter::default(); STRIPS],
            fx_meters: vec![[0.0; FX_SLOTS]; STRIPS],
            scopes: vec![Default::default(); STRIPS],
            effect_files: HashMap::new(),
            playlist: PlaylistState::default(),
            audio_editor: None,
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
            bounce_job: None,
            stretch: stretch::StretchCache::new(),
            song_tempo: gt_core::TempoMap::default(),
            show: gt_core::Performance::default(),
            perf_record: false,
            perf_horizon: 0,
            perf_end: 0,
            perf_live: false,
            toast: None,
            quit_confirmed: false,
            window_title: String::new(),
            live: live.clone(),
            midi: crate::midi_io::MidiIo::start(live, ctx.clone()),
            midi_out: crate::midi_out_io::MidiOutIo::start(),
            note_expr: std::collections::HashMap::new(),
            sync: hardware::SyncState::from_prefs(&settings.midi),
            controllers: controllers::Controllers::load(settings.midi.controller_maps.clone()),
            keyboard: crate::live::TypingKeyboard::default(),
            recorder: crate::live::Recorder::default(),
            take: None,
            step: crate::live::StepRecorder::default(),
            step_seen: Tick::ZERO,
            midi_model: MidiPanelModel {
                count_in_bars: settings.midi.count_in_bars,
                record_click: settings.midi.record_click,
                extra_ms: settings.midi.extra_ms,
                octave: settings.midi.octave,
                mode: gt_ui::views::RecordMode::from_key(&settings.midi.record_mode),
                passes: gt_ui::views::LoopPasses::from_key(&settings.midi.loop_passes),
                automation: gt_ui::views::AutoMode::from_key(&settings.midi.automation_mode),
                mtc_rate: gt_core::MtcRate::ALL
                    .into_iter()
                    .find(|r| r.fps() == settings.midi.mtc_fps)
                    .unwrap_or_default(),
                clock_in: settings.midi.clock_in_port.clone(),
                ..MidiPanelModel::default()
            },
            sent_live_slot: None,
            sent_routes: None,
            learning: None,
            midi_seen: 0,
            plugins,
            crashed_plugins: crash.in_use.clone(),
            show_plugins: false,
            plugin_browser: PluginBrowserState::default(),
            plugin_slot: None,
            fresh_engine: false,
            record: record::RecordState::default(),
            auto: automate::AutoState::default(),
            settings,
        };
        for port in &app.settings.midi.disabled_ports {
            app.midi.set_enabled(port, false);
        }
        for port in &app.settings.midi.mpe_ports {
            app.midi.set_mpe(port, true);
        }
        app.apply_sync();
        if let Some((path, plugin, action)) = &crash.quarantined {
            app.toast(format!(
                "{plugin} was switched off: GloomTunes closed while {action} it ({}). \
                 Allow it again in Plugins.",
                path.display()
            ));
        }
        app.plugins.start_scan();
        app.refresh_presets();
        app.init_browser();
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

    /// Starts loading every sample the project uses (channels and audio clips).
    fn request_channel_samples(&mut self) {
        let rate = self.rate();
        for ch in &self.project.channels {
            for src in ch.samples() {
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
        for t in &self.project.playlist.tracks {
            if let Some(f) = &t.frozen {
                self.library.request(&f.source, rate);
            }
        }
    }

    /// Sends channel `i`'s sample to the engine. If it is not loaded yet, the slot is silenced
    /// and the sample is sent when the load finishes, so a failed load never leaves the
    /// previous sample playing.
    fn push_sample(&mut self, i: usize) {
        let rate = self.rate();
        let ch = &self.project.channels[i];
        for src in ch.samples() {
            if self.library.get(src).is_none() {
                self.library.request(&src.clone(), rate);
            }
        }
        let lib = &self.library;
        let get = |src: &gt_core::SampleSource| lib.get(src).map(|l| Arc::clone(&l.data));
        let fitted = ch
            .sampler()
            .and_then(|s| self.stretch.fitted(&self.project, s, get));
        let (sample, zones) = gt_engine::zones::channel_sources(ch, get, fitted);
        self.send(EngineCommand::SetChannelSample {
            slot: i as u16,
            sample,
        });
        self.send(EngineCommand::SetChannelZones {
            slot: i as u16,
            zones,
        });
    }

    fn push_all_params(&mut self) {
        self.midi_out.set_slots(
            self.project
                .channels
                .iter()
                .map(|c| c.midi_out().map(|m| m.port.clone()).unwrap_or_default())
                .collect(),
        );
        for i in 0..self.project.channels.len() {
            let p = ChannelParams::from_project(&self.project, i);
            self.send(EngineCommand::SetChannelParams {
                slot: i as u16,
                params: Box::new(p),
            });
        }
    }

    /// Sends what plays: the current pattern (pattern mode) or the playlist (song mode). Audio
    /// clips whose sound is still loading join when it arrives.
    fn push_song(&mut self) {
        // Tempo automation clips are part of the tempo map, so it goes first when it changed.
        let tempo = self.project.song_tempo();
        if tempo != self.song_tempo {
            self.song_tempo = tempo;
            self.send(EngineCommand::SetTempoMap(Box::new(self.engine_tempo())));
        }
        let song = if self.transport.song_mode {
            self.request_clip_audio();
            self.request_stretches();
            let lib = &self.library;
            let cache = &self.stretch;
            let perf;
            let project = if self.playlist.performance {
                perf = self
                    .project
                    .performance_project(&self.show, self.perf_horizon);
                &perf
            } else {
                &self.project
            };
            SongSnapshot::compile_song_with(
                project,
                |src| lib.get(src).map(|l| std::sync::Arc::clone(&l.data)),
                |key| cache.get(key),
            )
        } else {
            SongSnapshot::compile(&self.project)
        };
        let lanes = song.automation.iter().map(|l| l.target).collect();
        let automated: Vec<(usize, usize, usize)> = song
            .automation
            .iter()
            .filter_map(|l| effect_of(&l.dest))
            .collect();
        if self.try_send(EngineCommand::SetSong(Box::new(song))) {
            self.auto.lanes = lanes;
            let old = std::mem::replace(&mut self.lane_fx, automated);
            self.release_fx(old);
        }
        // The song's end is the loop in song mode.
        self.send(EngineCommand::SetLoop(self.loop_region()));
    }

    /// Sends the tempo and time-signature maps, then the song (audio clip times depend on the
    /// tempo).
    fn push_timing(&mut self) {
        self.song_tempo = self.project.song_tempo();
        self.send(EngineCommand::SetTempoMap(Box::new(self.engine_tempo())));
        self.send(EngineCommand::SetSignatures(Box::new(
            self.project.signatures.clone(),
        )));
        self.push_song();
        // Samplers in time-stretch mode fit their sample to the new tempo.
        for i in 0..self.project.channels.len() {
            if self.project.channels[i]
                .sampler()
                .is_some_and(|s| s.stretch_beats > 0.0)
            {
                self.push_sample(i);
            }
        }
    }

    /// Makes the selected sampler a multi-sample instrument from `files` (Prompt 23).
    fn load_zones(&mut self, files: &[std::path::PathBuf]) {
        let named: Vec<(gt_core::SampleSource, String)> = files
            .iter()
            .filter(|p| gt_project::is_audio_file(p))
            .map(|p| {
                let name = p
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                (gt_core::SampleSource::File(p.clone()), name)
            })
            .collect();
        if named.is_empty() {
            return;
        }
        let i = self.rack.selected;
        let Some(s) = self
            .project
            .channels
            .get_mut(i)
            .and_then(gt_core::Channel::sampler_mut)
        else {
            return;
        };
        if named.len() == 1 {
            s.sample = Some(named[0].0.clone());
            s.zones.clear();
        } else {
            s.zones = gt_core::sampler::map_folder(&named);
            s.zones.truncate(gt_core::SamplerSettings::MAX_ZONES);
        }
        self.push_sample(i);
        self.push_all_params();
        self.pending_edit = Some("Load samples");
    }

    /// Acts on the drum pad panel: resend the kit, play a pad, load samples.
    /// Writes the notes that replay channel `i`'s sliced loop into the current pattern,
    /// replacing the channel's notes there.
    fn export_slices(&mut self, i: usize) {
        let Some(ch) = self.project.channels.get(i) else {
            return;
        };
        let gt_core::Instrument::Sampler(s) = &ch.instrument else {
            return;
        };
        let notes = gt_core::sampler::slice_notes(&s.zones, f64::from(s.slice_beats));
        let id = ch.id;
        if notes.is_empty() {
            return;
        }
        self.project.current_pattern_mut().notes.insert(id, notes);
        self.pending_edit = Some("Export slices");
    }

    /// Opens the pack at `path` once (cached, failures too).
    fn open_pack(&mut self, path: &std::path::Path) -> Result<&gt_project::pack::Pack, String> {
        self.opened_packs
            .entry(path.to_path_buf())
            .or_insert_with(|| {
                gt_project::pack::open(path, &gt_project::pack::default_cache())
                    .map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    fn on_player_panel(&mut self, out: gt_ui::views::PlayerPanelOut) {
        let i = self.rack.selected;
        let Some(current) = self.project.channels.get(i).and_then(|c| c.player()) else {
            return;
        };
        let mut pack = out.pack;
        if out.open {
            let d = rfd::FileDialog::new()
                .add_filter(tr("Sound pack"), &[gt_project::pack::EXTENSION, "zip"])
                .set_directory(packs_folder());
            pack = d.pick_file();
            if let Some(p) = &pack {
                // A failed open is retried when picked again.
                self.opened_packs.remove(p);
            }
        }
        let (pack, preset) = match (pack, out.preset) {
            (Some(p), _) => (p, None),
            (None, Some(name)) => (current.pack.clone(), Some(name)),
            (None, None) => {
                if out.changed {
                    self.push_all_params();
                    self.push_sample(i);
                    self.pending_edit = Some("Player macros");
                }
                return;
            }
        };
        let player = match self.open_pack(&pack) {
            Ok(opened) => {
                let name = preset.or_else(|| opened.preset_names().next().map(str::to_owned));
                name.and_then(|n| opened.player(&n))
            }
            // The panel shows why (the failure is cached).
            Err(_) => None,
        };
        if let Some(player) = player {
            if let Some(p) = self
                .project
                .channels
                .get_mut(i)
                .and_then(|c| c.player_mut())
            {
                *p = player;
            }
            self.push_all_params();
            self.push_sample(i);
            self.pending_edit = Some("Player preset");
        }
    }

    fn on_drum_panel(&mut self, out: gt_ui::views::DrumPadOut) {
        let i = self.rack.selected;
        let (bank, pad) = (self.drum_state.bank, self.drum_state.pad);
        if out.changed {
            self.push_sample(i);
            self.pending_edit = Some("Drum pads");
        }
        if let Some(key) = out.audition {
            self.send(EngineCommand::NoteOn {
                slot: i as u16,
                key,
                velocity: 0.8,
            });
            self.send(EngineCommand::NoteOff {
                slot: i as u16,
                key,
            });
        }
        if let Some(add) = out.load {
            let d = rfd::FileDialog::new().add_filter("Audio", &gt_project::AUDIO_EXTENSIONS);
            if let Some(path) = d.pick_file() {
                if let Some(p) = self
                    .project
                    .channels
                    .get_mut(i)
                    .and_then(gt_core::Channel::drums_mut)
                    .and_then(|k| k.banks.get_mut(bank)?.get_mut(pad))
                {
                    let src = gt_core::SampleSource::File(path);
                    let mut layers: Vec<_> = if add {
                        p.layers.iter().map(|l| l.sample.clone()).collect()
                    } else {
                        Vec::new()
                    };
                    layers.push(src);
                    p.set_layers(layers);
                    self.push_sample(i);
                    self.pending_edit = Some("Load pad sample");
                }
            }
        }
        if out.load_folder {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                let files = gt_project::list_audio_files(&dir);
                self.fill_pads(&files);
            }
        }
    }

    /// Puts `files` on the selected kit's pads from the selected pad on.
    fn fill_pads(&mut self, files: &[std::path::PathBuf]) {
        let i = self.rack.selected;
        let start = self.drum_state.bank * gt_core::pads::PADS_PER_BANK + self.drum_state.pad;
        let Some(kit) = self
            .project
            .channels
            .get_mut(i)
            .and_then(gt_core::Channel::drums_mut)
        else {
            return;
        };
        let sources: Vec<_> = files
            .iter()
            .filter(|p| gt_project::is_audio_file(p))
            .map(|p| gt_core::SampleSource::File(p.clone()))
            .collect();
        if sources.is_empty() {
            return;
        }
        // Keep the pads before the selected one: fill a copy from there and splice it in.
        let mut rest = gt_core::DrumKit {
            banks: vec![vec![gt_core::Pad::default(); gt_core::pads::PADS_PER_BANK]],
        };
        rest.fill(sources);
        let pads: Vec<gt_core::Pad> = rest.banks.into_iter().flatten().collect();
        for (k, p) in pads.into_iter().enumerate() {
            let at = start + k;
            let (b, n) = (
                at / gt_core::pads::PADS_PER_BANK,
                at % gt_core::pads::PADS_PER_BANK,
            );
            if b >= gt_core::pads::MAX_BANKS {
                break;
            }
            while kit.banks.len() <= b {
                kit.banks
                    .push(vec![gt_core::Pad::default(); gt_core::pads::PADS_PER_BANK]);
            }
            if !p.layers.is_empty() {
                kit.banks[b][n] = p;
            }
        }
        self.push_sample(i);
        self.pending_edit = Some("Fill pads");
    }

    /// Files dropped on the window: audio files (or folders of them) go to the selected
    /// sampler, one as its sample and several as zones.
    fn on_dropped_files(&mut self, paths: &[std::path::PathBuf]) {
        let mut files = Vec::new();
        for p in paths {
            if p.is_dir() {
                files.extend(gt_project::list_audio_files(p));
            } else {
                files.push(p.clone());
            }
        }
        if self
            .project
            .channels
            .get(self.rack.selected)
            .is_some_and(|c| c.drums().is_some())
        {
            self.fill_pads(&files);
        } else {
            self.load_zones(&files);
        }
    }

    /// Sends every channel's sample and settings, clears slots no longer used, and the song.
    fn push_channels(&mut self) {
        let n = self.project.channels.len();
        for slot in n..self.slots_in_use {
            self.send(EngineCommand::SetChannelSample {
                slot: slot as u16,
                sample: None,
            });
            self.send(EngineCommand::SetChannelZones {
                slot: slot as u16,
                zones: None,
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
                    || self
                        .project
                        .playlist
                        .tracks
                        .iter()
                        .any(|t| t.frozen.as_ref().is_some_and(|f| f.source == *src))
            });
        if clips_waiting {
            self.push_song();
        }
        for src in ready {
            for i in 0..self.project.channels.len() {
                if self.project.channels[i].samples().any(|s| *s == src) {
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
                self.preview_data(src, data);
            }
            None => {
                self.preview_pending = Some(src.clone());
                let rate = self.rate();
                self.library.request(src, rate);
            }
        }
    }

    fn refresh_presets(&mut self) {
        let named = |v: Vec<std::path::PathBuf>| -> Vec<(String, std::path::PathBuf)> {
            v.into_iter()
                .map(|p| {
                    let name = p
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    (name, p)
                })
                .collect()
        };
        self.user_presets = named(presets::list(&self.preset_dir));
        let dir = crate::library::mixer_preset_folder();
        self.strip_presets = named(strip_presets::list(&dir, strip_presets::Kind::Strip));
        self.chain_presets = named(strip_presets::list(&dir, strip_presets::Kind::Chain));
        self.refresh_browser();
    }

    /// Saves or loads a mixer preset, or links a track, for the mixer (Prompt 29).
    pub(crate) fn on_mixer_request(&mut self, r: gt_ui::views::MixerRequest) {
        use gt_ui::views::MixerRequest as R;
        let kind = |chain: bool| {
            if chain {
                strip_presets::Kind::Chain
            } else {
                strip_presets::Kind::Strip
            }
        };
        match r {
            R::LinkTrack(strip, track) => {
                if self.project.link_track(strip, track) {
                    self.pending_edit = Some("Link track");
                }
            }
            R::Save { strip, chain } => {
                let Some(s) = self.project.mixer.strips.get(strip) else {
                    return;
                };
                let dir = crate::library::mixer_preset_folder();
                self.strip_preset_status =
                    Some(match strip_presets::save(&dir, &s.preset(), kind(chain)) {
                        Ok(path) => format!(
                            "Saved {}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        ),
                        Err(e) => format!("Could not save: {e}"),
                    });
                self.refresh_presets();
            }
            R::Load { strip, chain, path } => match strip_presets::load(&path, kind(chain)) {
                Ok((preset, warnings)) => {
                    if self.project.apply_strip_preset(strip, &preset, chain) {
                        self.pending_edit = Some(if chain {
                            "Load effect chain"
                        } else {
                            "Load strip preset"
                        });
                    }
                    self.strip_preset_status = warnings.first().cloned();
                }
                Err(e) => self.strip_preset_status = Some(e.to_string()),
            },
        }
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
            SynthPanelAction::LoadTable(osc) => {
                let d = rfd::FileDialog::new().add_filter("Audio", &gt_project::AUDIO_EXTENSIONS);
                if let Some(path) = d.pick_file() {
                    if let Some(p) = self.project.channels.get_mut(i).and_then(|c| c.synth_mut()) {
                        if let Some(t) = p.tables.get_mut(osc) {
                            *t = Some(gt_core::SampleSource::File(path));
                            self.pending_edit = Some("Load wavetable");
                        }
                    }
                }
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
            RackAction::OpenPianoRoll(_) => self.show_view(MainView::PianoRoll),
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
            RackAction::PatternColor => self.pending_edit = Some("Pattern colour"),
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
            PianoRollAction::AutomationChanged => {
                self.push_song();
                self.pending_edit = Some("Edit pattern automation");
            }
            PianoRollAction::AutomateParam(target) => {
                self.on_param_request(ParamRequest::AutomatePattern(target));
            }
            PianoRollAction::AuditionOn { key, velocity } => self.send(EngineCommand::NoteOn {
                slot,
                key,
                velocity,
            }),
            PianoRollAction::AuditionOff { key } => {
                self.send(EngineCommand::NoteOff { slot, key });
            }
            PianoRollAction::ClearControls(kind) => {
                let Some(id) = self.project.channels.get(self.rack.selected).map(|c| c.id) else {
                    return;
                };
                let pat = self.project.current_pattern_mut();
                if let Some(v) = pat.controls.get_mut(&id) {
                    v.retain(|c| kind.is_some_and(|k| c.kind != k));
                    if v.is_empty() {
                        pat.controls.remove(&id);
                    }
                }
                self.push_song();
                self.pending_edit = Some("Clear controller data");
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
                // The roll's selection starts with this channel's notes (other editable
                // channels follow); quantize and humanize apply to this channel's.
                let n = notes.len();
                let mut sel: Vec<bool> = (0..n)
                    .map(|i| roll.selected.get(i).copied().unwrap_or(false))
                    .collect();
                if action == PianoRollAction::Quantize {
                    ops::quantize(notes, &sel, roll.snap.ticks(), roll.quantize_strength);
                } else {
                    self.humanize_seed = self.humanize_seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
                    ops::humanize(
                        notes,
                        &sel,
                        roll.humanize_ticks,
                        roll.humanize_velocity,
                        self.humanize_seed,
                    );
                }
                ops::sort_with_selection(notes, &mut sel);
                let keep = n.min(roll.selected.len());
                roll.selected.splice(..keep, sel);
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
            // An edit to a linked automation clip goes to its copies.
            if self
                .project
                .playlist
                .sync_links(&self.history.base().playlist)
                > 0
            {
                self.push_song();
            }
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
            self.after_undo();
        }
    }

    /// Brings the engine and the views in line after undo, redo or a history jump.
    fn after_undo(&mut self) {
        self.edits += 1;
        self.roll.selected.clear();
        let n = self.project.channels.len();
        self.rack.selected = self.rack.selected.min(n.saturating_sub(1));
        self.push_channels();
        self.push_timing();
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
        let live = self.project.mixer.strips.len();
        for strip in 0..STRIPS {
            if strip >= live && self.sent_fx[strip].iter().all(Option::is_none) {
                continue;
            }
            for k in 0..FX_SLOTS {
                let want = self
                    .project
                    .mixer
                    .strips
                    .get(strip)
                    .and_then(|s| s.slots[k].clone());
                let have = self.sent_fx[strip][k].clone();
                match (want, have) {
                    (None, None) => {}
                    (None, Some(_)) => {
                        if self.try_send(EngineCommand::SetEffect {
                            strip: strip as u16,
                            slot: k as u8,
                            effect: None,
                        }) {
                            self.sent_fx[strip][k] = None;
                            self.scopes[strip][k] = None;
                        }
                    }
                    (Some(w), Some(h))
                        if w.kind == h.kind
                            && w.file == h.file
                            && !w
                                .params
                                .iter()
                                .zip(&h.params)
                                .enumerate()
                                .any(|(i, (a, b))| a != b && w.kind.rebuilds(i)) =>
                    {
                        for (i, (&a, &b)) in w.params.iter().zip(&h.params).enumerate() {
                            if a != b
                                && self.try_send(EngineCommand::SetEffectParam {
                                    strip: strip as u16,
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
                        let file = self.effect_file(&w, rate as u32);
                        let effect = create_effect_with(&w, rate, file.as_deref());
                        let scope = effect.0.scope();
                        if self.try_send(EngineCommand::SetEffect {
                            strip: strip as u16,
                            slot: k as u8,
                            effect: Some(effect),
                        }) {
                            self.scopes[strip][k] = scope;
                            self.sent_fx[strip][k] = Some(SentEffect {
                                kind: w.kind,
                                params: w.params,
                                file: w.file,
                            });
                        }
                    }
                }
            }
        }
    }

    /// The audio file an effect slot uses, loaded at `rate` (a convolution reverb set to its
    /// user response). A file that fails to load is reported and the effect falls back to
    /// its built-in response.
    fn effect_file(&mut self, slot: &EffectSlot, rate: u32) -> Option<Arc<SampleData>> {
        let path = slot.file.as_ref()?;
        let user = slot
            .get("ir")
            .is_some_and(|v| v as usize == IMPULSE_RESPONSES.len() - 1);
        if !user {
            return None;
        }
        if let Some(d) = self.effect_files.get(path) {
            return Some(Arc::clone(d));
        }
        match gt_project::load_sample(path, rate) {
            Ok(d) => {
                let d = Arc::new(d);
                self.effect_files.insert(path.clone(), Arc::clone(&d));
                Some(d)
            }
            Err(e) => {
                self.toast(format!("Could not load {}: {e}", path.display()));
                None
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
        let targets = plan.targets.iter().map(|t| t.target).collect();
        let modulated: Vec<(usize, usize, usize)> = plan
            .targets
            .iter()
            .filter_map(|t| effect_of(&t.dest))
            .collect();
        if self.try_send(EngineCommand::SetModulation(Box::new(plan))) {
            self.sent_mods = Some(key);
            self.auto.mods = targets;
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
        for p in &self.project.patterns {
            marks
                .automated
                .extend(p.automation.iter().map(|a| a.target));
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
                self.show_view(MainView::Playlist);
                // The clip only plays in song mode.
                self.transport.song_mode = true;
                self.push_song();
                self.pending_edit = Some("Create automation clip");
            }
            ParamRequest::AutomatePattern(target) => {
                let value = target.normalized(&self.project);
                let pat = self.project.current_pattern_mut();
                if !pat.automation.iter().any(|a| a.target == target) {
                    let len = pat.length_ticks();
                    pat.automation.push(gt_core::Automation {
                        target,
                        points: vec![
                            gt_core::AutoPoint::new(0, value),
                            gt_core::AutoPoint::new(len, value),
                        ],
                    });
                    self.pending_edit = Some("Automate in pattern");
                }
                // Draw it in the piano roll's bottom lane; it plays wherever the pattern does.
                self.roll.lane = gt_ui::views::Lane::Automation(target);
                self.show_view(MainView::PianoRoll);
                self.push_song();
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

    /// Sends each synth channel the wavetables its patch needs whenever that changes: the
    /// engine, a user table, or a user table finishing loading.
    fn sync_tables(&mut self) {
        if self.audio.engine().is_none() {
            return;
        }
        let rate = self.rate();
        let n = self.project.channels.len();
        self.sent_tables.resize(n, None);
        for i in 0..n {
            let key = match &self.project.channels[i].instrument {
                Instrument::Synth(p) if p.engine() == 2 => {
                    for src in p.tables.iter().flatten() {
                        if self.library.get(src).is_none() && self.library.status(src).is_none() {
                            self.library.request(src, rate);
                        }
                    }
                    let loaded = |k: usize| {
                        p.tables[k]
                            .as_ref()
                            .is_some_and(|s| self.library.get(s).is_some())
                    };
                    Some(TablesKey {
                        tables: p.tables.clone(),
                        loaded: [loaded(0), loaded(1)],
                    })
                }
                _ => None,
            };
            if self.sent_tables[i] == key {
                continue;
            }
            let tables = match &self.project.channels[i].instrument {
                Instrument::Synth(p) if key.is_some() => {
                    let lib = &self.library;
                    gt_engine::tables::synth_tables(p, |s| lib.get(s).map(|l| Arc::clone(&l.data)))
                }
                _ => None,
            };
            if self.try_send(EngineCommand::SetChannelTables {
                slot: i as u16,
                tables,
            }) {
                self.sent_tables[i] = key;
            }
        }
    }

    /// Brings a freshly created engine up to date with the current settings.
    fn sync_new_engine(&mut self) {
        self.song_tempo = self.project.song_tempo();
        self.send(EngineCommand::SetTempoMap(Box::new(self.engine_tempo())));
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
        if let Some(q) = self.audio.engine_mut().and_then(|e| e.take_midi_out()) {
            self.midi_out.connect(q);
        }
        self.send(EngineCommand::SetMidiSync(
            self.sync.engine_settings(self.midi_model.mtc_rate),
        ));
        self.sent_live_slot = None;
        self.sent_routes = None;
        self.record.sent_routes = None;
        self.push_channels();
        self.send(EngineCommand::SetScopeChannel(self.scope_slot));
        // A new engine has an empty mixer.
        self.sent_mixer = None;
        self.sent_fx = vec![Default::default(); STRIPS];
        self.sent_tables.clear();
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
            ui.label(egui::RichText::new(tr("Add a channel to write notes")).color(theme.text_dim));
            return Vec::new();
        };
        let (id, name) = (ch.id, ch.name.clone());
        let params = ParamId::of_channel(ch)
            .into_iter()
            .map(|p| (p, p.name(&self.project)))
            .collect();
        let bar_ticks = self.project.signatures.sig_of_bar(0).bar_ticks();
        let automation_names = self
            .project
            .current_pattern()
            .automation
            .iter()
            .map(|a| a.target.name(&self.project))
            .collect();
        let names: Vec<(gt_core::ChannelId, String)> = self
            .project
            .channels
            .iter()
            .map(|c| (c.id, c.name.clone()))
            .collect();
        let pat = self.project.current_pattern_mut();
        self.roll.set_target(pat.id.0, id.0);
        let pattern_len = pat.length_ticks();
        // Take the edited list out so the other channels can be borrowed as ghosts.
        let mut notes = pat.notes.remove(&id).unwrap_or_default();
        let ghosts: Vec<GhostChannel<'_>> = pat
            .notes
            .iter_mut()
            .filter_map(|(cid, v)| {
                let name = names.iter().find(|(c, _)| c == cid)?.1.as_str();
                Some(GhostChannel {
                    id: cid.0,
                    name,
                    notes: v,
                })
            })
            .collect();
        let mut controls = pat.controls.remove(&id).unwrap_or_default();
        let automation = &mut pat.automation;
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
                controls: &mut controls,
                automation,
                automation_names,
                params,
            },
        );
        if !notes.is_empty() {
            pat.notes.insert(id, notes);
        }
        if !controls.is_empty() {
            pat.controls.insert(id, controls);
        }
        pat.notes.retain(|_, v| !v.is_empty());
        actions
    }

    /// The loop the engine plays: the user's loop when on; in song mode without one, the
    /// whole song (from bar 1 to the end of the last clip, whole bars).
    fn loop_region(&self) -> LoopRegion {
        let t = &self.transport;
        if self.playlist.performance {
            // Launched clips play on until stopped.
            return LoopRegion {
                enabled: false,
                ..LoopRegion::default()
            };
        }
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
        if self.on_performance(&action) {
            return;
        }
        match action {
            PlaylistAction::Changed => {
                self.push_song();
                self.pending_edit = Some("Edit playlist");
            }
            PlaylistAction::TimingChanged => {
                self.push_timing();
                self.pending_edit = Some("Tempo or time signature");
            }
            PlaylistAction::ArrangementChanged => {
                // Its clips' audio may not be loaded yet; the song plays on from the same tick.
                self.request_clip_audio();
                self.push_timing();
                self.pending_edit = Some("Arrangement");
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
                self.show_view(MainView::PianoRoll);
                self.push_song();
            }
            PlaylistAction::Bounce { kind, track, clips } => self.on_bounce(kind, track, clips),
            // Taken by `on_performance`.
            PlaylistAction::PerformanceMode(_)
            | PlaylistAction::Launch { .. }
            | PlaylistAction::LaunchScene(_)
            | PlaylistAction::SetLaunchQuant(_)
            | PlaylistAction::RecordPerformance(_) => {}
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
            TransportAction::ToggleStepRecord => self.toggle_step_record(),
            TransportAction::CaptureTake => self.capture_take(),
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
            AudioAction::SelectSampleRate(r) => self.audio.select_sample_rate(r),
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
        self.stop_audio_take();
        self.send(EngineCommand::Locate(Tick(0)));
        self.recorder = crate::live::Recorder::default();
        self.take = None;
        self.step.on = false;
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
        self.adopt_recordings(&path);
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
            Then::New => {
                let templates = files::list_templates(&files::templates_folder());
                if templates.is_empty() {
                    self.replace_project(gt_core::Project::empty(), None);
                } else {
                    self.dialog = Some(Dialog::NewProject { templates });
                }
            }
            Then::Demo(n) => {
                if let Some(demo) = gt_core::demos::DEMOS.get(n) {
                    self.replace_project((demo.build)(), None);
                }
            }
            Then::OpenPending => {
                if let Some(path) = self.pending_open.take() {
                    self.open_path(&path);
                }
            }
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
        if settings.regions && gt_export::marker_regions(&self.project).is_empty() {
            form.message = Some("No marker regions: add markers to the playlist first.".to_owned());
            return;
        }
        let video = form.video_settings(&self.theme);
        if let Some(gt_export::video::VideoSettings {
            style: gt_export::video::VideoStyle::Image(img),
            ..
        }) = &video
        {
            if !img.is_file() {
                form.message = Some("Choose an image for the video.".to_owned());
                return;
            }
        }
        if !settings.regions && gt_export::range_ticks(&self.project, settings.range).is_none() {
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
            gt_export::export_with_video(
                &project,
                &settings,
                &path,
                &progress,
                &mut plugins,
                video.as_ref(),
            )
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
                "Exported {} files to {}",
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
            DialogAction::NewFrom(None) => {
                self.dialog = None;
                self.replace_project(gt_core::Project::empty(), None);
            }
            DialogAction::NewFrom(Some(path)) => {
                self.dialog = None;
                self.open_path(&path);
                // A template starts a new, unnamed project.
                if self.doc_path.as_deref() == Some(path.as_path()) {
                    self.doc_path = None;
                }
            }
            DialogAction::DeleteTemplate(path) => {
                if let Err(e) = std::fs::remove_file(&path) {
                    self.toast(format!("Cannot delete {}: {e}", path.display()));
                }
                let templates = files::list_templates(&files::templates_folder());
                self.dialog = (!templates.is_empty()).then_some(Dialog::NewProject { templates });
            }
            DialogAction::SaveTemplate(name) => self.save_template(&name),
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
                self.toast(tr("Recovered the autosaved project; save it to keep it"));
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
        ui.menu_button(tr("File"), |ui| {
            for (label, then, keys) in [
                ("New", Some(Then::New), self.keys(Command::New)),
                ("Open…", Some(Then::Open), self.keys(Command::Open)),
            ] {
                if ui
                    .add(egui::Button::new(label).shortcut_text(keys))
                    .clicked()
                {
                    pick = then.map(FileCmd::Request);
                }
            }
            ui.menu_button(tr("Demo songs"), |ui| {
                for (n, demo) in gt_core::demos::DEMOS.iter().enumerate() {
                    if ui
                        .button(format!("{} ({})", demo.name, tr(demo.style)))
                        .clicked()
                    {
                        pick = Some(FileCmd::Request(Then::Demo(n)));
                    }
                }
            });
            ui.separator();
            if ui
                .add(egui::Button::new(tr("Save")).shortcut_text(self.keys(Command::Save)))
                .clicked()
            {
                pick = Some(FileCmd::Save);
            }
            if ui
                .add(egui::Button::new(tr("Save as…")).shortcut_text(self.keys(Command::SaveAs)))
                .clicked()
            {
                pick = Some(FileCmd::SaveAs);
            }
            ui.separator();
            if ui
                .add(
                    egui::Button::new(tr("Export audio…"))
                        .shortcut_text(self.keys(Command::ExportAudio)),
                )
                .clicked()
            {
                pick = Some(FileCmd::Export);
            }
            if ui.button(tr("Save as template…")).clicked() {
                pick = Some(FileCmd::SaveTemplate);
            }
            ui.separator();
            if ui.button(tr("Import MIDI file…")).clicked() {
                pick = Some(FileCmd::ImportMidi);
            }
            if ui.button(tr("Export MIDI file…")).clicked() {
                pick = Some(FileCmd::ExportMidi);
            }
            ui.separator();
            if ui
                .add(egui::Button::new(tr("Settings…")).shortcut_text(self.keys(Command::Settings)))
                .clicked()
            {
                self.show_settings = true;
            }
        });
        if let Some(cmd) = pick {
            self.file_cmd(cmd);
        }
        self.view_menu(ui);
    }

    /// View menu: panels and the workflow windows.
    fn view_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button(gt_ui::tr("View"), |ui| {
            let mut browser = self.layout.browser;
            if ui
                .checkbox(&mut browser, gt_ui::tr("Browser"))
                .on_hover_text(self.keys(Command::ShowBrowser))
                .changed()
            {
                self.layout.browser = browser;
            }
            for v in MainView::ALL {
                let floating = self.layout.is_floating(v);
                let label = format!("{} {}", v.label(), gt_ui::tr("in its own window"));
                let mut f = floating;
                if ui.checkbox(&mut f, label).changed() {
                    if f {
                        self.layout.float(v, None);
                    } else {
                        self.layout.dock(v);
                    }
                }
            }
            ui.separator();
            for (cmd, label) in [
                (Command::UndoHistory, gt_ui::tr("Undo history")),
                (Command::ProjectInfo, gt_ui::tr("Project info")),
                (Command::Help, gt_ui::tr("Help")),
            ] {
                if ui
                    .add(egui::Button::new(label).shortcut_text(self.keys(cmd)))
                    .clicked()
                {
                    self.run_command(cmd);
                    ui.close();
                }
            }
        });
    }

    /// Saves a copy of the project, with its samples, as a template.
    fn save_template(&mut self, name: &str) {
        let name = files::template_name(name);
        let path =
            files::templates_folder().join(format!("{name}.{}", gt_project::file::EXTENSION));
        if let Some(label) = self.pending_edit.take() {
            if self.history.commit(&self.project, label) {
                self.edits += 1;
            }
        }
        self.plugins.store_states(&mut self.project);
        let result = std::fs::create_dir_all(files::templates_folder())
            .map_err(|e| e.to_string())
            .and_then(|()| {
                gt_project::file::save(
                    &self.project,
                    &path,
                    gt_project::file::SaveOptions {
                        embed_samples: true,
                    },
                )
                .map_err(|e| e.to_string())
            });
        match result {
            Ok(_) => {
                self.dialog = None;
                self.toast(format!("Saved template \"{name}\""));
            }
            Err(e) => {
                self.dialog = Some(Dialog::SaveTemplate {
                    name,
                    error: Some(format!("Could not save: {e}")),
                });
            }
        }
    }

    fn file_cmd(&mut self, cmd: FileCmd) {
        if self.export_job.is_some() || self.bounce_job.is_some() {
            self.toast(tr("Wait for the render to finish"));
            return;
        }
        match cmd {
            FileCmd::Request(then) => self.request(then),
            FileCmd::Save => self.save(Then::Stay),
            FileCmd::SaveAs => self.save_as(Then::Stay),
            FileCmd::Export => self.show_export(),
            FileCmd::ImportMidi => self.show_import_midi(),
            FileCmd::ExportMidi => self.show_export_midi(),
            FileCmd::SaveTemplate => {
                self.dialog = Some(Dialog::SaveTemplate {
                    name: self.doc_name(),
                    error: None,
                });
            }
        }
    }

    /// Dialogs, the export job, autosave, the window title, closing and the toast.
    fn document_frame(&mut self, ctx: &egui::Context, theme: &GloomTheme) {
        self.poll_export();
        self.poll_bounce();
        self.poll_stretches();
        if let Some(mut d) = self.dialog.take() {
            let action = files::show(ctx, theme, &mut d);
            self.dialog = Some(d);
            if let Some(a) = action {
                self.on_dialog(a);
            }
        }
        if self.export_job.is_some() || self.bounce_job.is_some() {
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
    SaveTemplate,
}

impl eframe::App for GloomApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings_now();
        self.plugins.shutdown();
        self.end_session();
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.perf.frame(frame.info().cpu_usage);
        self.audio.poll();
        if self.audio.take_fresh_engine() {
            self.sync_new_engine();
            self.fresh_engine = true;
        }
        let ready = self.library.poll();
        self.on_samples_ready(ready);
        self.poll_index();
        self.poll_preview();
        self.apply_ui_prefs(&ctx);

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
        self.transport.bpm = self.song_tempo.bpm_at(self.transport.position);
        self.transport.time_sig = self.project.signatures.sig_at(self.transport.position);
        param_ui::set_marks(&ctx, std::sync::Arc::new(self.param_marks()));
        self.publish_live_values(&ctx);
        // Before any shortcut: in piano mode the typing keyboard takes its keys first.
        self.live_frame(&ctx, dt);
        self.record_frame();

        // Shortcuts run before any widget, so a focused button does not also react to Space;
        // keys the typing keyboard took above are gone by now.
        self.run_shortcuts(&ctx);

        let theme = self.theme.clone();
        let mut undo_clicked = None;
        let undo_tip = self
            .history
            .undo_label()
            .map(|l| format!("Undo {l} ({})", self.keys(Command::Undo)));
        let redo_tip = self
            .history
            .redo_label()
            .map(|l| format!("Redo {l} ({})", self.keys(Command::Redo)));
        let transport_actions = egui::Panel::top("transport")
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    self.file_menu(ui);
                    ui.separator();
                    let a =
                        transport_bar(ui, &theme, &mut self.transport, &self.project.signatures);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::selectable(self.show_settings, tr("Settings")))
                            .on_hover_text(tr(
                                "Audio device, MIDI, shortcuts, theme, performance and privacy",
                            ))
                            .clicked()
                        {
                            self.show_settings = !self.show_settings;
                        }
                        let redo =
                            ui.add_enabled(redo_tip.is_some(), egui::Button::new(tr("Redo")));
                        if redo
                            .on_hover_text(redo_tip.as_deref().unwrap_or("Nothing to redo"))
                            .clicked()
                        {
                            undo_clicked = Some(true);
                        }
                        let undo =
                            ui.add_enabled(undo_tip.is_some(), egui::Button::new(tr("Undo")));
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
                        self.cpu_meter_ui(ui, &theme);
                    });
                    a
                })
                .inner
            })
            .inner;
        for a in transport_actions {
            self.on_transport(a);
        }
        if let Some(redo) = undo_clicked {
            self.undo(redo);
        }

        if self.show_settings {
            self.settings_window(&ctx, &theme);
        }
        self.first_run_window(&ctx);
        if self.wizard.is_none() {
            self.crash_window(&ctx, &theme);
        }

        let target = self
            .project
            .channels
            .get(self.rack.selected)
            .filter(|c| c.sampler().is_some())
            .map(|c| c.name.clone());
        if self.layout.browser {
            let browser_actions = egui::Panel::left("browser")
                .resizable(true)
                .default_size(250.0)
                .min_size(180.0)
                .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(8))
                .show(ui, |ui| {
                    browser(ui, &theme, &mut self.browser, target.as_deref())
                })
                .inner;
            for a in browser_actions {
                self.on_browser(a);
            }
        }

        // The oscilloscope follows the selected synth channel.
        self.update_scope_slot();
        if self.scope_slot.is_some() {
            if let Some(engine) = self.audio.engine() {
                engine.telemetry().read_scope(&mut self.scope);
            }
        }
        let sample_rate = self.rate() as f32;
        let show_instrument =
            self.layout.visible(MainView::Rack) || self.layout.visible(MainView::PianoRoll);
        let mut plugin_actions = Vec::new();
        let mut plugin_changed = false;
        // The selected channel's plugin panel (read before the channel is borrowed to edit).
        let channel_plugin_view = self
            .project
            .channels
            .get(self.rack.selected)
            .and_then(|c| match &c.instrument {
                Instrument::Plugin(p) => {
                    Some(plugin_view(&self.plugins, &self.project, p.instance))
                }
                _ => None,
            })
            .unwrap_or_default();
        if self.packs_built.swap(false, Ordering::Relaxed) {
            self.packs = list_packs();
        }
        // The selected player's pack, opened before the channel is borrowed.
        let player_pack = self
            .project
            .channels
            .get(self.rack.selected)
            .and_then(|c| c.player())
            .map(|p| p.pack.clone())
            .filter(|p| !p.as_os_str().is_empty());
        let (player_presets, player_error) = match player_pack.map(|p| {
            self.open_pack(&p)
                .map(|pack| pack.preset_names().map(str::to_owned).collect::<Vec<_>>())
        }) {
            Some(Ok(names)) => (Some(names), None),
            Some(Err(e)) => (None, Some(e)),
            None => (None, None),
        };
        let mut player_out = gt_ui::views::PlayerPanelOut::default();
        let mut sampler_out = gt_ui::views::SamplerPanelOut::default();
        let mut drum_out = gt_ui::views::DrumPadOut::default();
        let mut swing_changed = false;
        let rack_swing = self.project.swing;
        let groups = gt_ui::views::rack_groups(&self.project);
        self.drum_state.inserts = self.project.mixer.inserts();
        let midi_out_ports = self.midi_out_ports();
        let layerable: Vec<(gt_core::ChannelId, String)> = self
            .project
            .channels
            .iter()
            .filter(|c| c.layer().is_none())
            .map(|c| (c.id, c.name.clone()))
            .collect();
        let (sampler_changed, synth_actions) = if !show_instrument {
            (false, Vec::new())
        } else {
            egui::Panel::bottom("instrument")
                .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(8))
                .show(ui, |ui| {
                    let Some(ch) = self.project.channels.get_mut(self.rack.selected) else {
                        ui.label(
                            egui::RichText::new(tr("No channel selected")).color(theme.text_dim),
                        );
                        return (false, Vec::new());
                    };
                    let channel = Some(ch.id);
                    let tab = &mut self.rack.settings_tab;
                    ui.horizontal(|ui| {
                        ui.selectable_value(tab, false, tr("Instrument"));
                        ui.selectable_value(tab, true, tr("Channel settings"))
                            .on_hover_text(tr(
                                "Pitch bend, swing, choke group, rack group, root key, key \
                                 range, mono and voices",
                            ));
                    });
                    if *tab {
                        let out =
                            gt_ui::views::channel_settings(ui, &theme, ch, rack_swing, &groups);
                        swing_changed = out.swing;
                        return (out.changed || out.swing, Vec::new());
                    }
                    let (changed, actions) = match &mut ch.instrument {
                        Instrument::Sampler(s) => {
                            let loaded = s.sample.as_ref().and_then(|x| self.library.get(x));
                            let status = s.sample.as_ref().and_then(|x| self.library.status(x));
                            let view = SamplerPanelView {
                                waveform: loaded.map(|l| l.overview.as_slice()),
                                seconds: loaded.map(|l| l.data.seconds()),
                                status: status.as_deref(),
                                channel,
                                data: loaded.map(|l| &*l.data),
                            };
                            let out = sampler_panel(ui, &theme, &mut ch.name, s, view);
                            sampler_out = out;
                            (out.changed, Vec::new())
                        }
                        Instrument::MidiOut(m) => {
                            let changed = gt_ui::views::midi_out_panel(
                                ui,
                                &theme,
                                &mut ch.name,
                                m,
                                &midi_out_ports,
                            );
                            (changed, Vec::new())
                        }
                        Instrument::Layer(l) => {
                            let own = ch.id;
                            let others: Vec<_> = layerable
                                .iter()
                                .filter(|(c, _)| *c != own)
                                .cloned()
                                .collect();
                            let changed =
                                gt_ui::views::layer_panel(ui, &theme, &mut ch.name, l, &others);
                            (changed, Vec::new())
                        }
                        Instrument::Drums(kit) => {
                            drum_out = gt_ui::views::drum_pads(
                                ui,
                                &theme,
                                &mut ch.name,
                                kit,
                                &mut self.drum_state,
                            );
                            (false, Vec::new())
                        }
                        Instrument::Player(pl) => {
                            let view = gt_ui::views::PlayerPanelView {
                                packs: &self.packs,
                                presets: player_presets.as_deref(),
                                error: player_error.as_deref(),
                            };
                            player_out =
                                gt_ui::views::player_panel(ui, &theme, &mut ch.name, pl, view);
                            (false, Vec::new())
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
                                .on_hover_text(tr("Channel name"));
                            });
                            let view = &channel_plugin_view;
                            let owner = gt_core::PluginOwner::Channel(ch.id);
                            plugin_changed =
                                plugin_controls(ui, &theme, owner, p, view, &mut plugin_actions);
                            (false, Vec::new())
                        }
                    };
                    (changed, actions)
                })
                .inner
        };
        if sampler_changed {
            self.push_all_params();
            self.pending_edit = Some("Instrument settings");
        }
        if swing_changed {
            self.push_song();
        }
        if sampler_out.reload {
            self.push_sample(self.rack.selected);
            self.pending_edit = Some("Instrument settings");
        }
        self.on_drum_panel(drum_out);
        self.on_player_panel(player_out);
        if sampler_out.export_pattern {
            self.export_slices(self.rack.selected);
        }
        if sampler_out.load_folder {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                let files = gt_project::list_audio_files(&dir);
                self.load_zones(&files);
            }
        }
        if show_instrument {
            let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
                i.raw
                    .dropped_files
                    .iter()
                    .map(|f| f.path().to_path_buf())
                    .collect()
            });
            if !dropped.is_empty() {
                self.on_dropped_files(&dropped);
            }
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
            self.step_cursor()
        };
        if self.layout.visible(MainView::Mixer) {
            self.read_meters(dt);
        }
        let cx = views::ViewCtx {
            pattern_pos,
            inputs: self.offered_inputs(),
            sample_rate,
        };
        let mut out = views::ViewOut::default();
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(10))
            .show(ui, |ui| self.docked_views(ui, &theme, &cx, &mut out));
        self.floating_views(&ctx, &theme, &cx, &mut out);
        self.on_views(out);
        self.workflow_windows(&ctx, &theme);
        self.audio_editor_window(&ctx, &theme);
        if let Some((strip, k)) = self.mixer_state.file_request.take() {
            let d = rfd::FileDialog::new().add_filter("Audio", &gt_project::AUDIO_EXTENSIONS);
            if let Some(path) = d.pick_file() {
                if let Some(Some(slot)) = self.project.mixer.strips[strip].slots.get_mut(k) {
                    slot.file = Some(path);
                    *slot = slot
                        .clone()
                        .with("ir", (IMPULSE_RESPONSES.len() - 1) as f32);
                    self.pending_edit = Some("Mixer");
                }
            }
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
            egui::Window::new(tr("Modulators"))
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
        let plugin_moves = self.sync_plugins(&ctx);
        self.automation_frame(&ctx, &plugin_moves);
        self.sync_mixer();
        self.sync_tables();
        self.sync_modulation();
        if !self.layout.visible(MainView::PianoRoll) {
            // Hidden mid-gesture (F6, tab click): the roll never sees the release.
            self.cancel_roll_gesture();
        }
        self.commit_if_idle(&ctx);
        self.document_frame(&ctx, &theme);
        self.save_settings(&ctx, false);

        // Repaint at display rate while anything moves; idle otherwise.
        let lights = self.activity.iter().any(|&a| a > 0.0)
            || self.layout.visible(MainView::Mixer)
                && self
                    .strip_meters
                    .iter()
                    .flatten()
                    .any(MeterBallistics::is_moving);
        let busy = self.library.is_busy() || self.indexing() || self.waiting_preview();
        if playing || self.test_tone || self.meter.is_moving() || lights || busy {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if self.audio.engine().is_some() {
            // Keep stream housekeeping (reopen, errors) ticking slowly.
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

/// The sound packs in the packs folder, by file name.
fn list_packs() -> Vec<(String, PathBuf)> {
    gt_project::pack::list(&packs_folder())
        .into_iter()
        .map(|p| {
            let name = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            (name, p)
        })
        .collect()
}

/// Builds the factory packs missing from the packs folder on a worker thread; the flag is set
/// when it is done.
fn build_factory_packs() -> Arc<std::sync::atomic::AtomicBool> {
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&done);
    let spawned = std::thread::Builder::new()
        .name("factory-packs".into())
        .spawn(move || {
            if let Err(e) = gt_export::packs::ensure_factory_packs(&packs_folder()) {
                eprintln!("factory packs: {e}");
            }
            flag.store(true, Ordering::Relaxed);
        });
    if spawned.is_err() {
        done.store(true, Ordering::Relaxed);
    }
    done
}
