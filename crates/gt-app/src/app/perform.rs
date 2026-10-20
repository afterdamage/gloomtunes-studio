//! Performance mode (Prompt 31, D145): clips launched from the playlist grid, keys 1 to 9 or
//! MIDI notes on channel 16, each on the next bar or beat, and recorded back into a new
//! arrangement when playback stops.
//!
//! The engine plays the performance as an ordinary song: the launches laid out as clips up to
//! a horizon ([`gt_core::Project::performance_project`]). A launch only changes the song from
//! its launch point on, so the new song goes in with `UpdateSong` (nothing is cut before it)
//! and its notes start on the exact frame of the launch point. The horizon moves on ahead of
//! the playhead.

use gt_core::performance::launch_point;
use gt_core::{ClipId, Tick, TrackId, PPQ};
use gt_engine::{EngineCommand, SongSnapshot};
use gt_ui::views::{PlayState, PlaylistAction};

use super::GloomApp;

/// How far ahead of the playhead the performance is laid out, in bars.
const HORIZON_BARS: i64 = 32;
/// The horizon moves on when the playhead is this many bars from it.
const RENEW_BARS: i64 = 8;
/// Time a launch needs to reach the engine before its launch point (a UI frame, the command
/// queue and a device buffer, with room to spare).
const LAUNCH_MARGIN_S: f64 = 0.08;
/// The first key of the MIDI launch grid: C1. Each track takes 8 keys (8 cells).
const LAUNCH_BASE_KEY: u8 = 36;
/// Cells per track in the MIDI launch grid.
const LAUNCH_KEYS_PER_TRACK: u8 = 8;

impl GloomApp {
    /// Performance actions from the playlist; false when `a` is not one.
    pub(super) fn on_performance(&mut self, a: &PlaylistAction) -> bool {
        match *a {
            PlaylistAction::PerformanceMode(on) => {
                self.midi.set_launch_mode(on);
                self.show.launches.clear();
                if on {
                    // The grid plays the playlist's clips in song mode.
                    self.transport.song_mode = true;
                    self.send(EngineCommand::Stop);
                }
                self.perf_horizon = 0;
                self.push_song();
            }
            PlaylistAction::Launch { track, clip } => self.launch(&[(track, clip)]),
            PlaylistAction::LaunchScene(k) => self.launch_scene(k),
            PlaylistAction::SetLaunchQuant(q) => self.show.quant = q,
            PlaylistAction::RecordPerformance(on) => self.perf_record = on,
            _ => return false,
        }
        true
    }

    /// True while the playlist shows the launch grid.
    pub(super) fn performing(&self) -> bool {
        self.playlist.performance
    }

    /// Launches column `k` on every track that has one.
    fn launch_scene(&mut self, k: usize) {
        let cells: Vec<(TrackId, Option<ClipId>)> = gt_ui::views::performance_rows(&self.project)
            .into_iter()
            .filter_map(|(t, row)| row.get(k).map(|&c| (t, Some(c))))
            .collect();
        self.launch(&cells);
    }

    /// Launches cells at the next launch point (all at the same one), starting playback when
    /// stopped.
    fn launch(&mut self, cells: &[(TrackId, Option<ClipId>)]) {
        if cells.is_empty() || !self.performing() {
            return;
        }
        let playing = self.transport.state == PlayState::Playing;
        let sigs = self.project.signatures.clone();
        let at = if playing {
            let bpm = self.song_tempo.bpm_at(self.transport.position).max(1.0);
            let margin = (LAUNCH_MARGIN_S * bpm / 60.0 * PPQ as f64).ceil() as i64;
            launch_point(self.show.quant, self.transport.position, margin, &sigs)
        } else {
            // From a stop the launch point is where playback starts.
            let at = self.show.quant.next(self.transport.position.0, &sigs);
            self.show.launches.retain(|l| l.at < at);
            at
        };
        for &(track, clip) in cells {
            self.show.launch(track, clip, at, &sigs);
        }
        self.perf_horizon = self.perf_horizon.max(at + HORIZON_BARS * bar(&sigs, at));
        if playing {
            self.update_performance();
        } else {
            self.send(EngineCommand::Locate(Tick(at)));
            self.push_song();
            self.send(EngineCommand::Play);
        }
    }

    /// Sends the performance as it now stands without cutting what already plays.
    fn update_performance(&mut self) {
        let p = self
            .project
            .performance_project(&self.show, self.perf_horizon);
        let lib = &self.library;
        let cache = &self.stretch;
        let song = SongSnapshot::compile_song_with(
            &p,
            |src| lib.get(src).map(|l| std::sync::Arc::clone(&l.data)),
            |key| cache.get(key),
        );
        self.try_send(EngineCommand::UpdateSong(Box::new(song)));
    }

    /// Once a frame: MIDI launches, moving the horizon on, and recording back at a stop.
    pub(super) fn tick_performance(&mut self) {
        let keys = self.midi.take_launches();
        if !self.performing() {
            return;
        }
        if !keys.is_empty() {
            let rows = gt_ui::views::performance_rows(&self.project);
            let cells: Vec<(TrackId, Option<ClipId>)> = keys
                .into_iter()
                .filter_map(|k| {
                    let i = k.checked_sub(LAUNCH_BASE_KEY)?;
                    let (t, row) = rows.get(usize::from(i / LAUNCH_KEYS_PER_TRACK))?;
                    // The key after a track's last cell stops it.
                    let cell = usize::from(i % LAUNCH_KEYS_PER_TRACK);
                    match row.get(cell) {
                        Some(&c) => Some((*t, Some(c))),
                        None if cell == row.len() => Some((*t, None)),
                        None => None,
                    }
                })
                .collect();
            self.launch(&cells);
        }
        let pos = self.transport.position.0;
        let playing = self.transport.state == PlayState::Playing;
        let sigs = &self.project.signatures;
        if playing && !self.show.launches.is_empty() {
            let b = bar(sigs, pos);
            if pos > self.perf_horizon - RENEW_BARS * b {
                self.perf_horizon = pos + HORIZON_BARS * b;
                self.update_performance();
            }
            self.perf_end = self.perf_end.max(pos);
            self.perf_live = true;
        }
        if self.perf_live && self.transport.state == PlayState::Stopped {
            self.perf_live = false;
            // Stopped: record what was played, then start the next performance afresh.
            let end = self.perf_end;
            let perf = std::mem::take(&mut self.show.launches);
            if self.perf_record && end > 0 {
                let rec = gt_core::Performance {
                    quant: self.show.quant,
                    launches: perf,
                };
                if let Some(i) = self.project.record_performance(&rec, end) {
                    let name = self
                        .project
                        .arrangement_names()
                        .nth(i)
                        .unwrap_or_default()
                        .to_owned();
                    self.toast(format!("Performance recorded as \"{name}\""));
                    self.pending_edit = Some("Record performance");
                    self.request_clip_audio();
                    self.push_timing();
                }
            }
            self.perf_end = 0;
            self.perf_horizon = 0;
        }
    }
}

/// Ticks in the bar at `t`.
fn bar(sigs: &gt_core::TimeSigMap, t: i64) -> i64 {
    sigs.sig_at(Tick(t)).bar_ticks().max(1)
}
