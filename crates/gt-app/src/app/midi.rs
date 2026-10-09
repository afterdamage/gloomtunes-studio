//! Live playing, recording, MIDI learn and MIDI files: the app side.

use std::path::Path;

use gt_core::{ParamId, Tick};
use gt_engine::{EngineCommand, EngineEvent, SongSnapshot, TransportState};
use gt_export::smf::{self, SmfScope};
use gt_ui::views::{MidiAction, MidiPortRow, PlayState};

use super::{GloomApp, MainView};
use crate::files::Dialog;
use crate::live::ms_to_ticks;

/// The MIDI light falls by this much per second.
const MIDI_LIGHT_FALL_PER_S: f32 = 5.0;

impl GloomApp {
    /// Once per frame, before the views: engine events, MIDI controllers, the typing
    /// keyboard, the live channel and the transport bar's live state.
    pub(super) fn live_frame(&mut self, ctx: &egui::Context, dt: f32) {
        for e in self.audio.take_events() {
            match e {
                EngineEvent::LiveNote {
                    key,
                    velocity,
                    tick,
                } => self.record_live(key, velocity, tick),
                EngineEvent::TransportChanged { state, .. } => {
                    if state != TransportState::Playing {
                        self.end_take();
                    }
                }
            }
        }

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
        if allowed {
            use egui::{Key, Modifiers};
            if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::T)) {
                self.keyboard.on = !self.keyboard.on;
            }
            if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::R)) {
                self.toggle_record();
            }
        }
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

        let seen = self.midi.activity();
        let t = &mut self.transport;
        if seen != self.midi_seen {
            self.midi_seen = seen;
            t.midi_light = 1.0;
        } else {
            t.midi_light = (t.midi_light - MIDI_LIGHT_FALL_PER_S * dt).max(0.0);
        }
        t.recording = self.recorder.armed;
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

    /// Arms recording (counting in from stop) or ends the take.
    pub(super) fn toggle_record(&mut self) {
        if self.recorder.armed {
            self.end_take();
            return;
        }
        let Some(ch) = self.project.channels.get(self.rack.selected) else {
            self.toast("Add a channel to record into");
            return;
        };
        self.take = Some((ch.id, self.project.current_pattern().length_ticks()));
        self.recorder.armed = true;
        if self.midi_model.record_click {
            self.send(EngineCommand::SetMetronome(true));
        }
        if self.transport.state != PlayState::Playing {
            self.send(EngineCommand::CountIn {
                bars: self.midi_model.count_in_bars,
            });
        }
    }

    /// Ends the take: completes held notes at the playhead and disarms.
    pub(super) fn end_take(&mut self) {
        if !self.recorder.armed {
            return;
        }
        self.recorder.armed = false;
        if let Some((_, len)) = self.take {
            let at = self.pattern_position();
            let notes = self.recorder.finish(at, len);
            self.add_recorded(&notes);
        }
        self.take = None;
        self.send(EngineCommand::SetMetronome(self.transport.metronome));
    }

    /// A live note the engine played while the transport ran, at song tick `tick`.
    fn record_live(&mut self, key: u8, velocity: f32, tick: f64) {
        let Some((_, len)) = self.take.filter(|_| self.recorder.armed) else {
            return;
        };
        // The player heard the music late by the output latency, so they played late too.
        let bpm = self.project.tempo.bpm_at(Tick(tick.max(0.0) as i64));
        let ms = f64::from(self.audio.buffer_ms().unwrap_or(0.0) + self.midi_model.extra_ms);
        let t = (tick - ms_to_ticks(ms, bpm)).round() as i64;
        let at = self.pattern_tick_at(t.max(0));
        if let Some(n) = self.recorder.note(key, velocity, at, len) {
            self.add_recorded(&[n]);
        }
    }

    /// Puts recorded notes into the current pattern and lets the engine play them, without
    /// cutting what sounds.
    fn add_recorded(&mut self, notes: &[gt_core::Note]) {
        let Some((channel, _)) = self.take else {
            return;
        };
        if notes.is_empty() || self.project.channel_index(channel).is_none() {
            return;
        }
        let list = self
            .project
            .current_pattern_mut()
            .notes
            .entry(channel)
            .or_default();
        list.extend_from_slice(notes);
        list.sort_by_key(|n| (n.start, n.key));
        let song = if self.transport.song_mode {
            let lib = &self.library;
            SongSnapshot::compile_song(&self.project, |src| {
                lib.get(src).map(|l| std::sync::Arc::clone(&l.data))
            })
        } else {
            SongSnapshot::compile(&self.project)
        };
        self.send(EngineCommand::UpdateSong(Box::new(song)));
        self.pending_edit = Some("Record notes");
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
        for a in gt_ui::views::midi_panel(ui, theme, m) {
            match a {
                MidiAction::SetPortEnabled(name, on) => self.midi.set_enabled(&name, on),
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
