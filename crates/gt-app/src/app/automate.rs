//! Automation recording and live values, the app side (Prompt 17, D111): control moves during
//! a song-mode recording become automation clips, and the values automation and modulators
//! give parameters are shown on their controls.

use std::collections::HashSet;
use std::sync::Arc;

use gt_core::ParamId;
use gt_engine::{clock_ns, EngineCommand, ParamDest};
use gt_ui::param_ui::{self, LiveValues};
use gt_ui::views::PlayState;

use super::GloomApp;
use crate::autorec::AutoRecorder;

/// Automation recording and live-value state.
#[derive(Default)]
pub(super) struct AutoState {
    /// The recording under way (song mode, recording, playing).
    rec: Option<AutoRecorder>,
    /// Controls the mouse held last frame.
    held: HashSet<ParamId>,
    /// Parameters a MIDI controller moved since the last frame: when (clock ns) and to what
    /// (normalized).
    cc_moves: Vec<(ParamId, u64, f32)>,
    /// The paused lanes the engine has.
    sent_writing: Vec<ParamDest>,
    /// Targets of the engine's automation lanes, in lane order (live value cells).
    pub(super) lanes: Vec<ParamId>,
    /// Targets of the engine's modulation plan, in order.
    pub(super) mods: Vec<ParamId>,
    /// Song tick seen last frame while recording (where a stopped recording ends).
    last_tick: f64,
}

impl AutoState {
    /// A MIDI controller moved `param` (already applied to the document) at `at_ns`.
    pub(super) fn cc_moved(&mut self, param: ParamId, at_ns: u64, value: f32) {
        self.cc_moves.push((param, at_ns, value));
    }
}

impl GloomApp {
    /// Before the views: publishes the values automation and modulators give parameters now.
    pub(super) fn publish_live_values(&self, ctx: &egui::Context) {
        let mut live = LiveValues::new();
        if let Some(engine) = self.audio.engine() {
            let t = engine.telemetry();
            let cells = |ids: &[ParamId], cells: &[gt_engine::AtomicF32]| {
                ids.iter()
                    .zip(cells)
                    .map(|(&id, c)| (id, c.load(std::sync::atomic::Ordering::Relaxed)))
                    .filter(|(_, v)| v.is_finite())
                    .collect::<Vec<_>>()
            };
            // A modulated lane reports through its modulator.
            live.extend(cells(&self.auto.lanes, &t.lane_values));
            live.extend(cells(&self.auto.mods, &t.mod_values));
        }
        param_ui::set_live(ctx, Arc::new(live));
    }

    /// After the views and the plugin sync: feeds this frame's control moves to the
    /// recording, writes finished passes into the playlist and tells the engine which lanes
    /// to pause.
    pub(super) fn automation_frame(&mut self, ctx: &egui::Context, plugin_moves: &[ParamId]) {
        let touches = param_ui::take_touches(ctx);
        let ccs = std::mem::take(&mut self.auto.cc_moves);
        let on = self.transport.song_mode
            && self.transport.state == PlayState::Playing
            && (self.recorder.armed || self.record.active());
        let anchor = |ns: u64| {
            self.audio
                .engine()
                .and_then(|e| e.telemetry().anchor.tick_at(ns))
        };
        let now = anchor(clock_ns()).unwrap_or(self.transport.position.0 as f64);
        if !on {
            if let Some(mut rec) = self.auto.rec.take() {
                rec.stop(self.auto.last_tick);
                self.commit_passes(rec.take_done());
            }
            self.auto.held.clear();
            self.send_writing(Vec::new());
            return;
        }
        self.auto.last_tick = now;
        let rec = self
            .auto
            .rec
            .get_or_insert_with(|| AutoRecorder::new(self.midi_model.automation, now));
        rec.advance(now);
        let mut held = HashSet::new();
        for t in touches {
            if t.held {
                held.insert(t.id);
            }
            rec.moved(t.id, now, t.id.normalized(&self.project), t.held);
        }
        for id in self.auto.held.difference(&held) {
            rec.released(*id, now);
        }
        self.auto.held = held;
        for (id, at_ns, value) in ccs {
            rec.moved(id, anchor(at_ns).unwrap_or(now), value, false);
        }
        for &id in plugin_moves {
            rec.moved(id, now, id.normalized(&self.project), false);
        }
        let done = rec.take_done();
        let writing: Vec<ParamDest> = rec
            .writing()
            .iter()
            .filter_map(|t| ParamDest::resolve(t, &self.project))
            .collect();
        self.commit_passes(done);
        self.send_writing(writing);
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }

    fn send_writing(&mut self, writing: Vec<ParamDest>) {
        if writing != self.auto.sent_writing
            && self.try_send(EngineCommand::SetWriting(Box::new(writing.clone())))
        {
            self.auto.sent_writing = writing;
        }
    }

    /// Writes finished passes into automation clips and lets the engine play them.
    fn commit_passes(&mut self, passes: Vec<(ParamId, Vec<gt_core::AutoPoint>)>) {
        let mut any = false;
        for (target, points) in passes {
            any |= gt_core::record_automation(&mut self.project, target, &points).is_some();
        }
        if !any {
            return;
        }
        self.pending_edit = Some("Record automation");
        self.push_recorded();
    }
}
