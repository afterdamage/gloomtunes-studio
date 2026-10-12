//! Live playing, recording (overdub, replace, stacked loop passes, controllers, step
//! recording, retroactive capture), MIDI learn and MIDI files: the app side.

use std::path::Path;

use gt_core::{ChannelId, ControlKind, ControlPoint, Note, ParamId, Pattern, Tick};
use gt_engine::{EngineCommand, EngineEvent, LiveRoutes, SongSnapshot, TransportState};
use gt_export::smf::{self, SmfScope};
use gt_ui::views::{LoopPasses, MidiAction, MidiPortRow, MidiRouteRow, PlayState, RecordMode};

use super::{GloomApp, MainView};
use crate::files::Dialog;
use crate::live::ms_to_ticks;

/// The MIDI light falls by this much per second.
const MIDI_LIGHT_FALL_PER_S: f32 = 5.0;

/// A MIDI recording take.
#[derive(Debug, Clone)]
pub(super) struct Take {
    /// The pattern length when it started (notes wrap and are cut at it).
    len: i64,
    /// Overdub or replace, fixed for the take.
    mode: RecordMode,
    /// Pattern position the playhead had last frame, while recording.
    swept: Option<i64>,
    /// Stacking passes: the pattern as it was before the take.
    base: Option<Pattern>,
    /// Passes kept as patterns so far.
    passes: u32,
    /// Something was recorded in this pass.
    dirty: bool,
}

impl GloomApp {
    /// Once per frame, before the views: engine events, MIDI controllers, the typing
    /// keyboard, the live channel and the transport bar's live state.
    pub(super) fn live_frame(&mut self, ctx: &egui::Context, dt: f32) {
        let mut recorded = self.sweep_take();
        for e in self.audio.take_events() {
            match e {
                EngineEvent::LiveNote {
                    slot,
                    key,
                    velocity,
                    tick,
                    playing,
                } => {
                    recorded |= if playing {
                        self.record_live(slot, key, velocity, tick)
                    } else {
                        self.step_key(slot, key, velocity)
                    };
                }
                EngineEvent::LiveControl {
                    slot,
                    kind,
                    value,
                    tick,
                } => recorded |= self.record_control(slot, kind, value, tick),
                EngineEvent::TransportChanged { state, .. } => {
                    if state != TransportState::Playing {
                        self.end_take();
                    }
                }
            }
        }
        if recorded {
            self.push_recorded();
        }
        self.follow_step_cursor();

        let ccs = self.midi.take_cc();
        let mut moved = false;
        for cc in ccs {
            if let Some(param) = self.learning.take() {
                if self.project.learn_midi(cc.channel, cc.cc, param) {
                    self.toast(format!(
                        "CC {} (channel {}) now moves {}",
                        cc.cc,
                        cc.channel + 1,
                        param.name(&self.project)
                    ));
                    self.pending_edit = Some("MIDI learn");
                } else {
                    // Mode messages (CC 120 and up) cannot be learned; keep waiting.
                    self.learning = Some(param);
                }
                continue;
            }
            moved |= !self
                .project
                .apply_cc(cc.channel, cc.cc, cc.value)
                .is_empty();
        }
        if moved {
            // Mixer and effect changes reach the engine through the per-frame mixer sync.
            self.push_all_params();
            self.pending_edit = Some("MIDI controller");
        }
        if let Some(param) = self.learning {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                self.learning = None;
                self.toast("MIDI learn cancelled");
            } else {
                self.toast(format!(
                    "MIDI learn: move a controller to bind {} (Esc cancels)",
                    param.name(&self.project)
                ));
            }
        }

        let allowed = !ctx.text_edit_focused() && self.dialog.is_none();
        self.keyboard.octave = self.midi_model.octave;
        for n in self.keyboard.handle(ctx, allowed) {
            self.live.send(n);
        }
        self.midi_model.octave = self.keyboard.octave;

        let slot =
            (self.rack.selected < self.project.channels.len()).then_some(self.rack.selected as u16);
        if self.sent_live_slot != Some(slot) && self.try_send(EngineCommand::SetLiveChannel(slot)) {
            self.sent_live_slot = Some(slot);
        }
        let routes = self.live_routes();
        if self.sent_routes.as_ref() != Some(&routes)
            && self.try_send(EngineCommand::SetLiveRoutes(Box::new(routes.clone())))
        {
            self.sent_routes = Some(routes);
        }

        let seen = self.midi.activity();
        let t = &mut self.transport;
        if seen != self.midi_seen {
            self.midi_seen = seen;
            t.midi_light = 1.0;
        } else {
            t.midi_light = (t.midi_light - MIDI_LIGHT_FALL_PER_S * dt).max(0.0);
        }
        t.recording = self.recorder.armed || self.record.active();
        t.step_record = self.step.on;
        t.typing_keyboard = self.keyboard.on;
        t.count_in = self.audio.engine().map_or(0, |e| {
            e.telemetry()
                .count_in_beats
                .load(std::sync::atomic::Ordering::Relaxed)
        });
        if t.midi_light > 0.0 || t.count_in > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    /// Arms recording (counting in from stop) or ends the take. MIDI records into the current
    /// pattern, each note into the channel it played; armed playlist tracks record their audio
    /// input.
    pub(super) fn toggle_record(&mut self) {
        if self.recorder.armed || self.record.active() {
            self.end_take();
            return;
        }
        let audio = self.start_audio_take();
        if self.project.channels.get(self.rack.selected).is_some() {
            let stack = self.midi_model.passes == LoopPasses::Stack && !self.transport.song_mode;
            let pattern = self.project.current_pattern();
            self.take = Some(Take {
                len: pattern.length_ticks(),
                mode: self.midi_model.mode,
                swept: None,
                base: stack.then(|| pattern.clone()),
                passes: 0,
                dirty: false,
            });
            self.recorder.armed = true;
            self.step.on = false;
        } else if !audio {
            self.toast("Add a channel or arm a track to record into");
            return;
        }
        if self.midi_model.record_click {
            self.send(EngineCommand::SetMetronome(true));
        }
        if self.transport.state != PlayState::Playing {
            self.send(EngineCommand::CountIn {
                bars: self.midi_model.count_in_bars,
            });
        }
    }

    /// Ends the take: stops audio recording, completes held notes at the playhead and
    /// disarms.
    pub(super) fn end_take(&mut self) {
        let audio = self.record.active();
        self.stop_audio_take();
        if !self.recorder.armed {
            if audio {
                self.send(EngineCommand::SetMetronome(self.transport.metronome));
            }
            return;
        }
        self.recorder.armed = false;
        if let Some(take) = self.take.take() {
            let at = self.pattern_position();
            let notes = self.recorder.finish(at, take.len);
            if self.add_notes(&notes) {
                self.push_recorded();
            }
            if take.passes > 0 {
                let name = self.project.current_pattern().name.clone();
                self.toast(format!(
                    "Kept {} earlier pass{} as \"{name} take 1\"…; the last one is in \"{name}\"",
                    take.passes,
                    if take.passes == 1 { "" } else { "es" },
                ));
            }
        }
        self.send(EngineCommand::SetMetronome(self.transport.metronome));
    }

    /// While recording, follows the playhead through the pattern: in replace mode it clears
    /// what the recording channels had where it passed, and when stacking passes it keeps a
    /// finished pass as its own pattern each time the pattern comes round. True when the
    /// pattern changed.
    fn sweep_take(&mut self) -> bool {
        let playing = self.transport.state == PlayState::Playing;
        let pos = self.pattern_position();
        let targets = self.record_targets();
        let Some(take) = self.take.as_mut().filter(|_| self.recorder.armed) else {
            return false;
        };
        let (Some(p), true) = (pos, playing) else {
            take.swept = None;
            return false;
        };
        let Some(prev) = take.swept.replace(p) else {
            return false;
        };
        let (ranges, wrapped) = crate::live::swept(prev, p, take.len);
        let replace = take.mode == RecordMode::Replace;
        let mut changed = false;
        if wrapped && take.base.is_some() {
            changed |= self.stack_pass();
        }
        if replace {
            let pat = self.project.current_pattern_mut();
            for &id in &targets {
                for &(lo, hi) in &ranges {
                    changed |= pat.clear_range(id, lo, hi);
                }
            }
        }
        changed
    }

    /// The channels a take records into: the selected one and every route target.
    fn record_targets(&self) -> Vec<ChannelId> {
        let p = &self.project;
        let mut out: Vec<ChannelId> = p.midi_routes.iter().map(|r| r.target).collect();
        out.extend(p.channels.get(self.rack.selected).map(|c| c.id));
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The pattern came round while stacking passes: the pass just played becomes a pattern
    /// of its own and the pattern goes back to how it was before the take.
    fn stack_pass(&mut self) -> bool {
        let Some(take) = self.take.as_mut() else {
            return false;
        };
        let len = take.len;
        let held = self.recorder.finish(None, len);
        let dirty = take.dirty || !held.is_empty();
        self.add_notes(&held);
        let Some(take) = self.take.as_mut() else {
            return false;
        };
        let Some(base) = take.base.clone().filter(|_| dirty) else {
            return false;
        };
        take.passes += 1;
        take.dirty = false;
        let n = take.passes;
        let id = self.project.current_pattern;
        if let Some(copy) = self.project.clone_pattern(id) {
            let name = self
                .project
                .unique_pattern_name(&format!("{} take {n}", base.name));
            self.project.rename_pattern(copy, &name);
        }
        *self.project.current_pattern_mut() = base;
        true
    }

    /// Puts recorded notes (by channel slot) into the current pattern. True when any went in.
    fn add_notes(&mut self, notes: &[(u16, Note)]) -> bool {
        let mut added = false;
        for &(slot, n) in notes {
            let Some(id) = self.project.channels.get(usize::from(slot)).map(|c| c.id) else {
                continue;
            };
            let list = self
                .project
                .current_pattern_mut()
                .notes
                .entry(id)
                .or_default();
            let at = list.partition_point(|x| (x.start, x.key) <= (n.start, n.key));
            list.insert(at, n);
            added = true;
        }
        if added {
            if let Some(take) = self.take.as_mut() {
                take.dirty = true;
            }
        }
        added
    }

    /// The pattern position a live event at song tick `tick` belongs to: the player heard
    /// the music late by the output latency, so they played late too.
    fn recorded_at(&self, tick: f64) -> Option<i64> {
        let bpm = self.project.tempo.bpm_at(Tick(tick.max(0.0) as i64));
        let ms = f64::from(self.audio.buffer_ms().unwrap_or(0.0) + self.midi_model.extra_ms);
        let t = (tick - ms_to_ticks(ms, bpm)).round() as i64;
        self.pattern_tick_at(t.max(0))
    }

    /// A live note the engine played on `slot` while the transport ran, at song tick `tick`.
    fn record_live(&mut self, slot: u16, key: u8, velocity: f32, tick: f64) -> bool {
        let Some(len) = self
            .take
            .as_ref()
            .filter(|_| self.recorder.armed)
            .map(|t| t.len)
        else {
            return false;
        };
        let at = self.recorded_at(tick);
        let done = self.recorder.note(slot, key, velocity, at, len);
        self.add_notes(&done.into_iter().collect::<Vec<_>>())
    }

    /// A live controller moved on `slot` while the transport ran: recorded into the
    /// pattern's controller data for that channel.
    fn record_control(&mut self, slot: u16, kind: ControlKind, value: f32, tick: f64) -> bool {
        let Some(len) = self
            .take
            .as_ref()
            .filter(|_| self.recorder.armed)
            .map(|t| t.len)
        else {
            return false;
        };
        let (Some(at), Some(id)) = (
            self.recorded_at(tick),
            self.project.channels.get(usize::from(slot)).map(|c| c.id),
        ) else {
            return false;
        };
        let point = ControlPoint {
            tick: at.rem_euclid(len.max(1)),
            kind,
            value,
        };
        let list = self
            .project
            .current_pattern_mut()
            .controls
            .entry(id)
            .or_default();
        let changed = crate::live::add_control(list, point);
        if changed {
            if let Some(take) = self.take.as_mut() {
                take.dirty = true;
            }
        }
        changed
    }

    /// Lets the engine play what was recorded, without cutting what sounds.
    fn push_recorded(&mut self) {
        self.project
            .current_pattern_mut()
            .controls
            .retain(|_, v| !v.is_empty());
        let song = if self.transport.song_mode {
            let lib = &self.library;
            let cache = &self.stretch;
            SongSnapshot::compile_song_with(
                &self.project,
                |src| lib.get(src).map(|l| std::sync::Arc::clone(&l.data)),
                |key| cache.get(key),
            )
        } else {
            SongSnapshot::compile(&self.project)
        };
        self.send(EngineCommand::UpdateSong(Box::new(song)));
        if self.pending_edit.is_none() {
            self.pending_edit = Some(if self.step.on {
                "Step record"
            } else {
                "Record notes"
            });
        }
    }

    /// Step recording on or off. It starts at the playhead's place in the pattern.
    pub(super) fn toggle_step_record(&mut self) {
        self.step.on = !self.step.on;
        if !self.step.on {
            return;
        }
        if self.recorder.armed {
            self.end_take();
        }
        let at = self.pattern_position().unwrap_or(0);
        let step = self.step_ticks();
        self.step.move_to(at.div_euclid(step) * step);
        self.step_seen = self.transport.position;
        self.toast(
            "Step record: with the transport stopped, play a note or chord to enter it at the \
             cursor",
        );
    }

    /// The step length: the piano roll's snap (a 1/16 step with snap off).
    fn step_ticks(&self) -> i64 {
        match self.roll.snap {
            gt_ui::views::Snap::Off => gt_core::STEP_TICKS,
            s => s.ticks(),
        }
    }

    /// A live note the engine played on `slot` with the transport stopped.
    fn step_key(&mut self, slot: u16, key: u8, velocity: f32) -> bool {
        if !self.step.on || self.recorder.armed {
            return false;
        }
        let step = self.step_ticks();
        let note = self.step.key(slot, key, velocity, step);
        self.add_notes(&note.into_iter().collect::<Vec<_>>())
    }

    /// Moves the step cursor where the user located the playhead.
    fn follow_step_cursor(&mut self) {
        if !self.step.on {
            return;
        }
        if self.transport.position != self.step_seen {
            self.step_seen = self.transport.position;
            if self.transport.state != PlayState::Playing {
                if let Some(at) = self.pattern_position() {
                    self.step.move_to(at);
                }
            }
        }
    }

    /// The step cursor, for the piano roll, while step recording with the transport stopped.
    pub(super) fn step_cursor(&self) -> Option<i64> {
        (self.step.on && self.transport.state != PlayState::Playing).then_some(self.step.cursor)
    }

    /// Turns what was just played (kept even with Record off) into a new pattern.
    pub(super) fn capture_take(&mut self) {
        let events = self.live.recent();
        let routes = self.live_routes();
        let selected =
            (self.rack.selected < self.project.channels.len()).then_some(self.rack.selected as u16);
        let channels = self.project.channels.len();
        let bpm = self.project.tempo.bpm_at(self.transport.position);
        let got = crate::live::capture(&events, gt_engine::clock_ns(), bpm, |e| {
            routes
                .slot(e.port, e.channel, selected)
                .filter(|&s| usize::from(s) < channels)
        });
        if got.notes.is_empty() {
            self.toast("Nothing played lately to capture (play something first)");
            return;
        }
        let id = self.project.new_pattern();
        let name = self.project.unique_pattern_name("Captured take");
        self.project.rename_pattern(id, &name);
        self.project.select_pattern(id);
        self.add_notes(&got.notes);
        for &(slot, c) in &got.controls {
            let ch = self.project.channels[usize::from(slot)].id;
            self.project
                .current_pattern_mut()
                .controls
                .entry(ch)
                .or_default()
                .push(c);
        }
        if let Some(&(slot, _)) = got.notes.first() {
            self.rack.selected = usize::from(slot);
        }
        self.push_song();
        self.main_view = MainView::PianoRoll;
        self.pending_edit = Some("Capture last take");
        self.toast(format!(
            "Captured {} note{} into pattern \"{name}\"",
            got.notes.len(),
            if got.notes.len() == 1 { "" } else { "s" }
        ));
    }

    /// The project's input routes for the engine, by the numbers of the ports seen so far.
    /// Whole-port routes go first so a route for one of the port's channels wins.
    fn live_routes(&self) -> LiveRoutes {
        let mut routes = LiveRoutes::default();
        let mut list: Vec<_> = self.project.midi_routes.iter().collect();
        list.sort_by_key(|r| r.channel.is_some());
        for r in list {
            let (Some(port), Some(slot)) = (
                self.midi.port_number(&r.port),
                self.project.channel_index(r.target),
            ) else {
                continue;
            };
            routes.set(port, r.channel, Some(slot as u16));
        }
        routes
    }

    pub(super) fn start_learning(&mut self, param: ParamId) {
        self.learning = Some(param);
        if self.midi.ports().iter().all(|p| !p.connected) {
            self.toast("No MIDI input is connected (see Audio and MIDI settings)");
        }
    }

    /// The MIDI part of the settings window.
    pub(super) fn midi_settings(&mut self, ui: &mut egui::Ui, theme: &gt_ui::GloomTheme) {
        let m = &mut self.midi_model;
        m.ports = self
            .midi
            .ports()
            .into_iter()
            .map(|p| MidiPortRow {
                name: p.name,
                enabled: p.enabled,
                connected: p.connected,
            })
            .collect();
        m.error = self.midi.error();
        m.buffer_ms = self.audio.buffer_ms().unwrap_or(0.0);
        let p = &self.project;
        m.channels = p.channels.iter().map(|c| c.name.clone()).collect();
        m.routes = p
            .midi_routes
            .iter()
            .filter_map(|r| {
                Some(MidiRouteRow {
                    port: r.port.clone(),
                    channel: r.channel,
                    target: p.channel_index(r.target)?,
                })
            })
            .collect();
        for a in gt_ui::views::midi_panel(ui, theme, m) {
            match a {
                MidiAction::SetPortEnabled(name, on) => self.midi.set_enabled(&name, on),
                MidiAction::RoutesChanged => {
                    let p = &mut self.project;
                    p.midi_routes = m
                        .routes
                        .iter()
                        .filter_map(|r| {
                            Some(gt_core::MidiRoute {
                                port: r.port.clone(),
                                channel: r.channel,
                                target: p.channels.get(r.target)?.id,
                            })
                        })
                        .collect();
                    self.pending_edit = Some("MIDI routing");
                }
            }
        }
    }

    pub(super) fn show_import_midi(&mut self) {
        let dir = self
            .doc_path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(crate::library::default_folder);
        self.dialog = Some(Dialog::ImportMidi {
            path: format!("{}{}", dir.display(), std::path::MAIN_SEPARATOR),
            timing: false,
            error: None,
        });
    }

    pub(super) fn show_export_midi(&mut self) {
        let dir = self
            .doc_path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(crate::library::default_folder);
        self.dialog = Some(Dialog::ExportMidi {
            path: dir
                .join(format!("{}.mid", self.doc_name()))
                .display()
                .to_string(),
            song: self.transport.song_mode,
            error: None,
        });
    }

    pub(super) fn import_midi(&mut self, path: &Path, timing: bool) {
        let file = match smf::read(path) {
            Ok(f) => f,
            Err(e) => {
                if let Some(Dialog::ImportMidi { error, .. }) = &mut self.dialog {
                    *error = Some(e.to_string());
                }
                return;
            }
        };
        let name = path
            .file_stem()
            .map_or_else(|| "MIDI".to_owned(), |s| s.to_string_lossy().into_owned());
        let first_new = self.project.channels.len();
        let report = file.apply(&mut self.project, &name, timing);
        self.dialog = None;
        if report.channels > 0 {
            self.rack.selected = first_new;
        }
        self.push_channels();
        if timing {
            self.push_timing();
        }
        self.main_view = MainView::PianoRoll;
        self.pending_edit = Some("Import MIDI file");
        let mut msg = format!(
            "Imported {} part{} into pattern \"{name}\"",
            report.channels,
            if report.channels == 1 { "" } else { "s" }
        );
        if report.skipped > 0 {
            msg += &format!("; {} left out (the rack is full)", report.skipped);
        }
        self.toast(msg);
    }

    pub(super) fn export_midi(&mut self, path: &Path, song: bool) {
        let scope = if song {
            SmfScope::Song
        } else {
            SmfScope::Pattern(self.project.current_pattern)
        };
        match smf::write(&self.project, scope, path) {
            Ok(()) => {
                self.dialog = None;
                self.toast(format!("Exported {}", path.display()));
            }
            Err(e) => {
                if let Some(Dialog::ExportMidi { error, .. }) = &mut self.dialog {
                    *error = Some(e.to_string());
                }
            }
        }
    }
}
