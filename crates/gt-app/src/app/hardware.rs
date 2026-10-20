//! Hardware integration, the app side (Prompt 33, D148): MIDI clock and time code out,
//! following an external clock, MPE ports, and the panel of MIDI out channels.

use std::time::{Duration, Instant};

use gt_core::{ClockFollower, MtcRate, SyncAction, TempoMap, Tick};
use gt_engine::{EngineCommand, MidiSyncSettings};
use gt_ui::views::{MidiAction, MidiOutRow};

use super::GloomApp;
use crate::settings::MidiPrefs;

/// How often the followed tempo is sent to the engine at most.
const TEMPO_EVERY: Duration = Duration::from_millis(40);
/// Smaller tempo changes are not sent.
const TEMPO_STEP: f64 = 0.005;

/// Sync settings and the clock being followed.
#[derive(Debug, Default)]
pub(super) struct SyncState {
    /// Output ports that get clock, and time code.
    pub clock_out: Vec<String>,
    pub mtc_out: Vec<String>,
    /// The external clock, while one is picked.
    follower: ClockFollower,
    /// The tempo the engine plays while following, and when it was sent.
    sent_bpm: Option<f64>,
    sent_at: Option<Instant>,
}

impl SyncState {
    pub(super) fn from_prefs(p: &MidiPrefs) -> Self {
        Self {
            clock_out: p.clock_out_ports.clone(),
            mtc_out: p.mtc_out_ports.clone(),
            ..Self::default()
        }
    }

    /// What the engine sends.
    pub(super) fn engine_settings(&self, rate: MtcRate) -> MidiSyncSettings {
        MidiSyncSettings {
            clock: !self.clock_out.is_empty(),
            mtc: (!self.mtc_out.is_empty()).then_some(rate),
        }
    }
}

impl GloomApp {
    /// The tempo map the engine plays: the song's, or the external clock's tempo while one is
    /// followed.
    pub(super) fn engine_tempo(&self) -> TempoMap {
        match self.sync.sent_bpm {
            Some(bpm) if self.midi_model.clock_in.is_some() => TempoMap::constant(bpm),
            _ => self.song_tempo.clone(),
        }
    }

    /// Applies the sync settings: ports, engine flags and the clock source.
    pub(super) fn apply_sync(&mut self) {
        self.midi_out
            .set_sync_ports(&self.sync.clock_out, &self.sync.mtc_out);
        let s = self.sync.engine_settings(self.midi_model.mtc_rate);
        self.send(EngineCommand::SetMidiSync(s));
        self.midi
            .set_clock_source(self.midi_model.clock_in.as_deref());
        self.sync.follower = ClockFollower::new();
        if self.midi_model.clock_in.is_none() && self.sync.sent_bpm.take().is_some() {
            // Back to the song's own tempo.
            self.send(EngineCommand::SetTempoMap(Box::new(
                self.song_tempo.clone(),
            )));
        }
    }

    /// Once per frame: follows the external clock's transport and tempo.
    pub(super) fn follow_clock(&mut self) {
        let messages = self.midi.take_clock();
        if self.midi_model.clock_in.is_none() {
            return;
        }
        for (m, at) in messages {
            match self.sync.follower.feed(m, at) {
                Some(SyncAction::Play(tick)) => {
                    self.send(EngineCommand::Locate(Tick(tick)));
                    self.send(EngineCommand::Play);
                }
                Some(SyncAction::Stop) => self.send(EngineCommand::Pause),
                Some(SyncAction::Locate(tick)) => self.send(EngineCommand::Locate(Tick(tick))),
                None => {}
            }
        }
        let now = gt_engine::clock_ns();
        let song = self
            .audio
            .engine()
            .and_then(|e| e.telemetry().anchor.tick_at(now));
        let Some(bpm) = self.sync.follower.tempo_for(song, now) else {
            return;
        };
        let due = self.sync.sent_at.is_none_or(|t| t.elapsed() >= TEMPO_EVERY);
        let moved = self
            .sync
            .sent_bpm
            .is_none_or(|b| (b - bpm).abs() >= TEMPO_STEP);
        if due && moved {
            self.sync.sent_bpm = Some(bpm);
            self.sync.sent_at = Some(Instant::now());
            self.send(EngineCommand::SetTempoMap(Box::new(TempoMap::constant(
                bpm,
            ))));
        }
        self.midi_model.clock_bpm = self.sync.follower.bpm();
    }

    /// Fills the sync part of the MIDI settings model.
    pub(super) fn sync_model(&mut self) {
        let mpe = self.midi.mpe_ports();
        for p in &mut self.midi_model.ports {
            p.mpe = mpe.contains(&p.name);
        }
        let mut names = self.midi_out.ports();
        for n in self.sync.clock_out.iter().chain(&self.sync.mtc_out) {
            if !names.contains(n) {
                names.push(n.clone());
            }
        }
        self.midi_model.out_error = self.midi_out.error();
        self.midi_model.outputs = names
            .into_iter()
            .map(|name| MidiOutRow {
                clock: self.sync.clock_out.contains(&name),
                mtc: self.sync.mtc_out.contains(&name),
                name,
            })
            .collect();
    }

    /// The sync and MPE actions of the MIDI settings panel.
    pub(super) fn sync_action(&mut self, a: &MidiAction) {
        match a {
            MidiAction::SetPortMpe(name, on) => self.midi.set_mpe(name, *on),
            MidiAction::SyncChanged => {
                let rows = &self.midi_model.outputs;
                self.sync.clock_out = rows
                    .iter()
                    .filter(|r| r.clock)
                    .map(|r| r.name.clone())
                    .collect();
                self.sync.mtc_out = rows
                    .iter()
                    .filter(|r| r.mtc)
                    .map(|r| r.name.clone())
                    .collect();
                self.apply_sync();
            }
            _ => {}
        }
    }

    /// The MIDI out channel panel's port list.
    pub(super) fn midi_out_ports(&self) -> Vec<String> {
        self.midi_out.ports()
    }
}
