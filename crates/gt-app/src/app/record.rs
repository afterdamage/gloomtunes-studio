//! Audio recording, the app side (Prompt 13): live input routes, arming, takes into clips,
//! and the loopback latency measurement.

use std::time::{Duration, Instant};

use gt_core::{AudioInput, AudioTake, ClipId, ClipKind, SampleSource, TrackId};
use gt_engine::{CalibrationSink, EngineCommand, InputRoute, InputRoutes};
use gt_ui::views::{input_panel, InputAction, InputPanelModel};

use super::GloomApp;
use crate::library::data_folder;
use crate::recording::{self, Lane, WriterEvent};

/// The loopback measurement gives up after this long.
const CALIBRATION_TIMEOUT: Duration = Duration::from_secs(8);

/// A recording session: armed tracks, the writer thread, and the clips made so far.
pub(super) struct AudioRec {
    session: recording::Session,
    tracks: Vec<TrackId>,
    /// Start tick of the take being recorded (from the writer's start report).
    take_start: Option<i64>,
    /// Clips this session made (later passes over the same place stack onto them).
    made: Vec<ClipId>,
}

/// A loopback measurement in progress.
pub(super) struct Calibration {
    samples: rtrb::Consumer<f32>,
    captured: Vec<f32>,
    started: Instant,
}

/// Recording state kept by the app.
#[derive(Default)]
pub(super) struct RecordState {
    pub(super) rec: Option<AudioRec>,
    /// Routes last sent to the engine (none: send them).
    pub(super) sent_routes: Option<Vec<InputRoute>>,
    calibration: Option<Calibration>,
    calibration_channel: usize,
    status: String,
}

impl RecordState {
    /// The take being recorded, for drawing: its start and tracks.
    pub(super) fn live_take(&self) -> Option<(i64, &[TrackId])> {
        let rec = self.rec.as_ref()?;
        Some((rec.take_start?, &rec.tracks))
    }

    /// True while a recording session is open.
    pub(super) fn active(&self) -> bool {
        self.rec.is_some()
    }
}

impl GloomApp {
    /// Once per frame: input routes, writer reports and the measurement.
    pub(super) fn record_frame(&mut self) {
        self.sync_input_routes();
        self.poll_writer();
        self.poll_calibration();
    }

    /// Sends where live inputs are heard when that changed: every insert with an input, and
    /// every armed track's input into the track's strip.
    fn sync_input_routes(&mut self) {
        if self.audio.engine().is_none() {
            return;
        }
        let monitor = self.audio.monitor;
        let mut routes: Vec<InputRoute> = self
            .project
            .mixer
            .strips
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                Some(InputRoute {
                    input: s.input?,
                    strip: i as u16,
                    armed: None,
                })
            })
            .collect();
        for t in &self.project.playlist.tracks {
            if let (true, Some(input)) = (self.playlist.armed.contains(&t.id), t.input) {
                routes.push(InputRoute {
                    input,
                    strip: t.insert.min(gt_core::STRIPS - 1) as u16,
                    armed: Some(monitor),
                });
            }
        }
        if self.record.sent_routes.as_ref() != Some(&routes)
            && self.try_send(EngineCommand::SetInputRoutes(Box::new(InputRoutes {
                routes: routes.clone(),
            })))
        {
            self.record.sent_routes = Some(routes);
        }
    }

    /// Armed tracks that can record: armed, with an input the device offers.
    fn armed_lanes(&self) -> Vec<Lane> {
        let offered = self.audio.offered_inputs();
        self.project
            .playlist
            .tracks
            .iter()
            .filter(|t| self.playlist.armed.contains(&t.id))
            .filter_map(|t| {
                let input = t.input.filter(|i| offered.contains(i))?;
                Some(Lane {
                    track: t.id,
                    input,
                    name: t.name.clone(),
                })
            })
            .take(gt_engine::MAX_RECORD_LANES)
            .collect()
    }

    /// Starts audio recording on the armed tracks. Returns true if something is recording.
    /// Audio clips live on the playlist, so this switches to song mode.
    pub(super) fn start_audio_take(&mut self) -> bool {
        if self.record.rec.is_some() {
            return true;
        }
        let lanes = self.armed_lanes();
        if lanes.is_empty() {
            if !self.playlist.armed.is_empty() {
                self.toast(if self.audio.input_channels() == 0 {
                    "No input device is open: choose one in Settings > Audio and MIDI"
                } else {
                    "The armed tracks have no input this device offers"
                });
            }
            return false;
        }
        let Some(engine) = self.audio.engine() else {
            return false;
        };
        let rate = engine.config().sample_rate;
        let dir = recording::folder(self.doc_path.as_deref(), &data_folder());
        let tracks = lanes.iter().map(|l| l.track).collect();
        let t = &self.transport;
        let punch =
            (t.punch && t.loop_end > t.loop_start).then_some((t.loop_start.0, t.loop_end.0));
        match recording::start(&dir, lanes, rate, self.audio.compensation(), punch) {
            Ok((sink, session)) => {
                if !self.try_send(EngineCommand::SetRecorder(Some(Box::new(sink)))) {
                    return false;
                }
                if !self.transport.song_mode {
                    self.transport.song_mode = true;
                    self.push_song();
                }
                self.record.rec = Some(AudioRec {
                    session,
                    tracks,
                    take_start: None,
                    made: Vec::new(),
                });
                true
            }
            Err(e) => {
                self.toast(format!("Cannot record into {}: {e}", dir.display()));
                false
            }
        }
    }

    /// Stops audio recording: the take in progress ends and its clips appear once written.
    pub(super) fn stop_audio_take(&mut self) {
        if self.record.rec.is_some() {
            self.send(EngineCommand::SetRecorder(None));
        }
    }

    /// Turns finished takes into clips; ends the session once the writer is done.
    fn poll_writer(&mut self) {
        let Some(rec) = self.record.rec.as_mut() else {
            return;
        };
        let events = rec.session.poll();
        let finished = rec.session.finished() && events.is_empty();
        for e in events {
            match e {
                WriterEvent::Started { tick, .. } => {
                    if let Some(rec) = self.record.rec.as_mut() {
                        rec.take_start = Some(tick);
                    }
                }
                WriterEvent::Done(take) => self.add_take(take),
                WriterEvent::Failed(e) => self.toast(format!("Recording failed: {e}")),
            }
        }
        if finished {
            self.record.rec = None;
        }
    }

    /// Places a finished take, one per track. A take that starts where a clip of this session
    /// starts (the next loop pass) stacks onto that clip and plays instead; the earlier passes
    /// stay as its other takes. Otherwise it becomes a new clip, and earlier clips of this
    /// session under it are cut where it starts (or muted when they start at the same place).
    fn add_take(&mut self, take: recording::TakeDone) {
        let rate = f64::from(self.rate());
        let tempo = &self.project.tempo;
        let start_s = tempo.tick_to_seconds(take.tick as f64);
        let end = tempo
            .seconds_to_tick(start_s + take.frames as f64 / rate)
            .ceil() as i64;
        let length = (end - take.tick).max(1);
        let made_before = self
            .record
            .rec
            .as_ref()
            .map(|r| r.made.clone())
            .unwrap_or_default();
        let mut made = Vec::new();
        let mut stacked = 0;
        for (track, path) in take.files {
            let source = SampleSource::File(path);
            self.library.request(&source, self.rate());
            let pl = &mut self.project.playlist;
            let pass = made_before.iter().copied().find(|&id| {
                pl.clip(id)
                    .is_some_and(|c| c.track == track && c.start == take.tick)
            });
            if let Some(clip) = pass.and_then(|id| pl.clip_mut(id)) {
                clip.stack_take(AudioTake {
                    source,
                    gaps: take.gaps.clone(),
                    length,
                });
                clip.muted = false;
                stacked += 1;
                continue;
            }
            let (s, e) = (take.tick, take.tick + length);
            for &old in &made_before {
                if let Some(c) = pl.clip_mut(old) {
                    if c.track == track && c.start < e && s < c.end() {
                        if c.start < s {
                            c.length = s - c.start;
                        } else {
                            c.muted = true;
                        }
                    }
                }
            }
            let id = pl.add_clip(
                track,
                take.tick,
                length,
                ClipKind::Audio {
                    source,
                    gain: 1.0,
                    gaps: take.gaps.clone(),
                    takes: Vec::new(),
                    edit: gt_core::AudioEdit::default(),
                },
            );
            made.push(id);
        }
        if let Some(rec) = self.record.rec.as_mut() {
            rec.take_start = None;
            rec.made.extend(made);
        }
        if stacked > 0 {
            self.toast(format!(
                "Take {} recorded; right-click the clip to pick another take",
                take.take + 1
            ));
        }
        if !take.gaps.is_empty() {
            self.toast(format!(
                "The recording has {} gap(s) where input was lost (marked in the clip)",
                take.gaps.len()
            ));
        }
        self.pending_edit = Some("Record audio");
        self.push_song();
    }

    /// Collects the measurement's capture and, once complete, sets the offset.
    fn poll_calibration(&mut self) {
        let rate = self.rate() as f32;
        let Some(c) = self.record.calibration.as_mut() else {
            return;
        };
        while let Ok(s) = c.samples.pop() {
            c.captured.push(s);
        }
        let need = gt_dsp::latency::total_frames(rate);
        let timed_out = c.started.elapsed() > CALIBRATION_TIMEOUT;
        if c.captured.len() < need && !timed_out {
            return;
        }
        let captured = std::mem::take(&mut c.captured);
        self.record.calibration = None;
        self.send(EngineCommand::Calibrate(None));
        self.record.status = match gt_dsp::latency::measure_delay(&captured, rate) {
            Ok(frames) => {
                let reported = self.audio.reported_latency().unwrap_or(0);
                let ms = |f: f32| f * 1000.0 / rate;
                self.audio.offset_ms = ms(frames as f32 - reported as f32);
                self.settings_dirty = true;
                format!(
                    "Measured round trip {:.1} ms ({:.1} ms reported): offset set to {:+.1} ms",
                    ms(frames as f32),
                    ms(reported as f32),
                    self.audio.offset_ms
                )
            }
            Err(e) => format!("Loopback test failed: {e}"),
        };
    }

    fn on_input(&mut self, action: InputAction) {
        match action {
            InputAction::SelectDevice(i) => {
                self.audio.select_input(i);
                self.record.sent_routes = None;
            }
            InputAction::SetPair(pair, mode) => {
                let pairs = &mut self.audio.pairs;
                if pairs.len() <= pair {
                    pairs.resize(pair + 1, Default::default());
                }
                pairs[pair] = mode;
            }
            InputAction::SetMonitor(m) => self.audio.monitor = m,
            InputAction::SetOffset(ms) => self.audio.offset_ms = ms.clamp(-200.0, 200.0),
            InputAction::SetCalibrationChannel(c) => self.record.calibration_channel = c,
            InputAction::Calibrate => {
                if self.record.calibration.take().is_some() {
                    self.send(EngineCommand::Calibrate(None));
                    self.record.status = "Loopback test cancelled".to_owned();
                    return;
                }
                let rate = self.rate() as f32;
                let (tx, rx) = rtrb::RingBuffer::new(gt_dsp::latency::total_frames(rate) + 4096);
                let channel = self.record.calibration_channel.min(255) as u8;
                if self.try_send(EngineCommand::Calibrate(Some(Box::new(CalibrationSink {
                    samples: tx,
                    channel,
                })))) {
                    self.record.calibration = Some(Calibration {
                        samples: rx,
                        captured: Vec::new(),
                        started: Instant::now(),
                    });
                    self.record.status = "Measuring: eight clicks…".to_owned();
                }
            }
        }
        self.settings_dirty = true;
    }

    /// The input part of the Audio and MIDI settings page.
    pub(super) fn input_settings(&mut self, ui: &mut egui::Ui, theme: &gt_ui::GloomTheme) {
        let channels = self.audio.input_channels();
        let rate = self.rate() as f32;
        let levels_db = self.audio.engine().map_or_else(Vec::new, |e| {
            e.telemetry().input_peaks[..channels.min(gt_core::MAX_INPUT_CHANNELS)]
                .iter()
                .map(|p| gt_dsp::gain_to_db(p.swap(0.0, std::sync::atomic::Ordering::Relaxed)))
                .collect()
        });
        let (devices, selected) = self.audio.input_devices();
        let mut status = self.record.status.clone();
        if status.is_empty() {
            status = self.audio.input_status().to_owned();
        }
        let model = InputPanelModel {
            devices: devices.to_vec(),
            selected_device: selected,
            channels,
            pairs: self.audio.pairs.clone(),
            levels_db,
            monitor: self.audio.monitor,
            reported_ms: self
                .audio
                .reported_latency()
                .map(|f| f as f32 * 1000.0 / rate),
            offset_ms: self.audio.offset_ms,
            calibration_channel: self.record.calibration_channel,
            calibrating: self.record.calibration.is_some(),
            slips: self.audio.engine().map_or(0, |e| {
                e.telemetry()
                    .input_slips
                    .load(std::sync::atomic::Ordering::Relaxed)
            }),
            two_devices: self.audio.separate_clocks(),
            status,
        };
        if channels > 0 {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        if let Some(a) = input_panel(ui, theme, &model) {
            self.on_input(a);
        }
    }

    /// Before saving to `path`: copies recordings made while the project had no file (or
    /// another one) next to it, so the project folder holds everything it plays.
    pub(super) fn adopt_recordings(&mut self, path: &std::path::Path) {
        if self.record.rec.is_some() {
            return;
        }
        let from = recording::folder(self.doc_path.as_deref(), &data_folder());
        let to = recording::folder(Some(path), &data_folder());
        match recording::adopt(&mut self.project, &from, &to) {
            Ok(0) => {}
            Ok(_) => {
                self.request_clip_audio();
                self.push_song();
            }
            Err(e) => self.toast(format!(
                "Could not copy the recordings next to the project: {e}"
            )),
        }
    }

    /// The inputs tracks and inserts can choose from.
    pub(super) fn offered_inputs(&self) -> Vec<AudioInput> {
        self.audio.offered_inputs()
    }
}
