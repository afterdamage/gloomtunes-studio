//! Audio input on the audio thread: reading the device input, monitoring it through the mixer,
//! recording it, and the loopback latency measurement (Prompt 13, ARCHITECTURE.md §7.10).
//!
//! The device layer's input callback pushes interleaved samples into an `rtrb` ring
//! ([`InputFeed`]); the engine takes exactly one quantum of frames from it per quantum, so the
//! input runs on the output's clock. A small cushion of frames ([`InputFeed::prime_frames`])
//! absorbs callback jitter. When the two clocks disagree (two devices) the cushion drains or
//! grows; the engine then waits for it to refill or drops the surplus, and reports the slip
//! (D97).
//!
//! Recording ([`RecordSink`]) copies the inputs of the armed tracks into a second ring that a
//! disk-writer thread empties, never touching a file here. A take starts at the first whole
//! tick at or after the playhead (or at the punch-in point) and ends when playback stops,
//! jumps (a locate or a loop wrap) or reaches the punch-out point; each loop pass is a take. Latency compensation is a number of frames `D`: the input read with engine frame
//! `F` is what the player did while hearing engine frame `F - D`, so the take's first written
//! frame is the one read `D` frames after its start tick sounded. Gaps (input that did not
//! arrive, or a full disk ring) are reported to the writer as [`RecMark::Gap`] with the take
//! frame where they happen, so the file keeps its timing.

use gt_core::{AudioInput, MonitorMode, MAX_INPUT_CHANNELS};
use gt_dsp::latency;
use rtrb::{Consumer, Producer};

use crate::mixer::MixerEngine;
use crate::transport::{PlaySegment, Transport};
use crate::{Telemetry, RENDER_QUANTUM};

const Q: usize = RENDER_QUANTUM;

/// Most tracks recorded at once.
pub const MAX_RECORD_LANES: usize = 32;

/// The device input as the engine reads it.
#[derive(Debug)]
pub struct InputFeed {
    /// Interleaved samples from the input callback.
    pub samples: Consumer<f32>,
    /// Channels per frame in `samples`.
    pub channels: usize,
    /// Frames to collect before reading starts (and again after the ring ran dry).
    pub prime_frames: usize,
    /// More frames than this waiting at a read are dropped back down to `prime_frames`.
    pub max_frames: usize,
}

/// Where a live input is heard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputRoute {
    /// The input.
    pub input: AudioInput,
    /// Mixer strip it is added to (before the strip's effects).
    pub strip: u8,
    /// `None`: always (a mixer insert's input). `Some(mode)`: an armed track's input, heard
    /// as the monitoring mode says.
    pub armed: Option<MonitorMode>,
}

/// Every live input route. Built on the UI thread; swapped in whole.
#[derive(Debug, Clone, Default)]
pub struct InputRoutes {
    /// Routes in any order.
    pub routes: Vec<InputRoute>,
}

/// A message from the engine to the disk writer, in take order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecMark {
    /// A take starts: the samples that follow belong to it.
    Start {
        /// Take number within this recording session, from 0.
        take: u32,
        /// Song tick where the take's first frame belongs.
        tick: i64,
    },
    /// Something went wrong at take frame `at`.
    Gap {
        /// Take frame (from the take's start) where it happened.
        at: u64,
        /// Frames affected.
        frames: u32,
        /// True when the frames were never sent (the disk ring was full): the writer inserts
        /// silence. False when silence was already sent in place of missing input, or (with
        /// `frames` 0) when input was skipped to realign it with the output.
        fill: bool,
    },
    /// The take ends after `frames` take frames (gaps included).
    End {
        /// Length of the take in frames.
        frames: u64,
    },
}

/// Where recorded audio goes: the writer thread holds the other ends.
#[derive(Debug)]
pub struct RecordSink {
    /// Interleaved frames: each lane's channels in lane order.
    pub samples: Producer<f32>,
    /// Starts, gaps and ends.
    pub marks: Producer<RecMark>,
    /// The inputs recorded, one lane per armed track (at most [`MAX_RECORD_LANES`]).
    pub lanes: Vec<AudioInput>,
    /// Latency compensation `D` in frames (see the module docs).
    pub compensation: u32,
    /// Punch in and out: record only between these ticks (the loop markers).
    pub punch: Option<(i64, i64)>,
}

impl RecordSink {
    /// Samples per recorded frame.
    pub fn frame_width(&self) -> usize {
        self.lanes.iter().map(|l| l.channels()).sum()
    }
}

/// Where the loopback measurement sends what the input heard.
#[derive(Debug)]
pub struct CalibrationSink {
    /// One sample per frame, from `channel`, aligned with the bursts' output frames.
    pub samples: Producer<f32>,
    /// Device input channel to listen on.
    pub channel: u8,
}

/// A take being recorded.
#[derive(Debug, Clone, Copy)]
struct Take {
    /// Frames still to skip before writing (the compensation, then whole frames).
    skip: u64,
    /// Take frames so far (written or reported as gaps).
    frames: u64,
    /// Song time where the next quantum's playback should continue; anything else is a jump.
    expect_s: f64,
    /// Frames still to write before the take ends (after a jump or a punch-out).
    tail: Option<u64>,
}

/// A take that starts once the current one's tail is written.
#[derive(Debug, Clone, Copy)]
struct Next {
    tick: i64,
    /// Frames to skip, counted from now.
    skip: u64,
}

/// What reading the input did this quantum.
#[derive(Debug, Clone, Copy, Default)]
struct ReadStatus {
    /// Frames that were missing (silence was read instead).
    missing: u32,
    /// Frames dropped to bring the ring back down.
    dropped: u32,
}

/// Input state of the processor. Boxed, so the per-channel buffers sit off the stack.
pub(crate) struct InputEngine {
    feed: Option<Box<InputFeed>>,
    primed: bool,
    /// One quantum per device channel, planar.
    buf: [[f32; Q]; MAX_INPUT_CHANNELS],
    /// Channels with data this quantum.
    live_channels: usize,
    status: ReadStatus,
    fill: f32,
    routes: Option<Box<InputRoutes>>,
    sink: Option<Box<RecordSink>>,
    take: Option<Take>,
    next: Option<Next>,
    /// The quantum's input gap was already reported.
    gap_reported: bool,
    takes: u32,
    /// An end mark that did not fit into the mark ring yet.
    end_pending: Option<u64>,
    /// A filled gap (take frame, frames) not sent yet: it grows while the samples ring stays
    /// full.
    fill_pending: Option<(u64, u32)>,
    calib: Option<Box<CalibrationSink>>,
    calib_frame: usize,
}

impl InputEngine {
    pub(crate) fn new() -> Self {
        Self {
            feed: None,
            primed: false,
            buf: [[0.0; Q]; MAX_INPUT_CHANNELS],
            live_channels: 0,
            status: ReadStatus::default(),
            fill: 0.0,
            routes: None,
            sink: None,
            take: None,
            next: None,
            gap_reported: false,
            takes: 0,
            end_pending: None,
            fill_pending: None,
            calib: None,
            calib_frame: 0,
        }
    }

    /// Installs a new feed; returns the old one for the garbage queue.
    pub(crate) fn set_feed(&mut self, feed: Option<Box<InputFeed>>) -> Option<Box<InputFeed>> {
        self.primed = false;
        std::mem::replace(&mut self.feed, feed)
    }

    pub(crate) fn set_routes(&mut self, routes: Box<InputRoutes>) -> Option<Box<InputRoutes>> {
        self.routes.replace(routes)
    }

    /// Installs or removes the recorder. A take in progress ends first.
    pub(crate) fn set_sink(&mut self, sink: Option<Box<RecordSink>>) -> Option<Box<RecordSink>> {
        self.end_take();
        self.next = None;
        self.flush_end();
        self.fill_pending = None;
        self.takes = 0;
        std::mem::replace(&mut self.sink, sink)
    }

    pub(crate) fn set_calibration(
        &mut self,
        calib: Option<Box<CalibrationSink>>,
    ) -> Option<Box<CalibrationSink>> {
        self.calib_frame = 0;
        std::mem::replace(&mut self.calib, calib)
    }

    /// Takes one quantum from the device ring into the planar buffers.
    pub(crate) fn read(&mut self, telemetry: &Telemetry) {
        use std::sync::atomic::Ordering::Relaxed;
        self.status = ReadStatus::default();
        let Some(feed) = self.feed.as_deref_mut() else {
            if self.live_channels > 0 {
                for c in &mut self.buf[..self.live_channels] {
                    c.fill(0.0);
                }
                self.live_channels = 0;
            }
            return;
        };
        let ch = feed.channels.max(1);
        let used = ch.min(MAX_INPUT_CHANNELS);
        for c in &mut self.buf[..used.max(self.live_channels)] {
            c.fill(0.0);
        }
        self.live_channels = used;
        let mut avail = feed.samples.slots() / ch;
        if !self.primed {
            if avail < feed.prime_frames + Q {
                self.status.missing = Q as u32;
                return;
            }
            self.primed = true;
        }
        if avail > feed.max_frames.max(feed.prime_frames + Q) {
            let drop = avail - feed.prime_frames - Q;
            if let Ok(chunk) = feed.samples.read_chunk(drop * ch) {
                chunk.commit_all();
            }
            self.status.dropped = drop as u32;
            avail -= drop;
            telemetry.input_slips.fetch_add(1, Relaxed);
        }
        let n = avail.min(Q);
        if let Ok(chunk) = feed.samples.read_chunk(n * ch) {
            let (a, b) = chunk.as_slices();
            for (k, &s) in a.iter().chain(b).enumerate() {
                let (frame, c) = (k / ch, k % ch);
                if c < used {
                    self.buf[c][frame] = s;
                }
            }
            chunk.commit_all();
        }
        if n < Q {
            self.status.missing = (Q - n) as u32;
            self.primed = false;
            telemetry.input_slips.fetch_add(1, Relaxed);
        }
        // About a second of smoothing for the fill shown as latency.
        self.fill += (avail as f32 - self.fill) * 0.002;
        telemetry.input_fill.store(self.fill, Relaxed);
        for (c, meter) in self.buf[..used].iter().zip(&telemetry.input_peaks) {
            let peak = c.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
            meter.fetch_max(peak, Relaxed);
        }
    }

    /// Input `c` of this quantum (silence beyond the device's channels).
    fn channel(&self, c: usize) -> &[f32; Q] {
        const SILENCE: [f32; Q] = [0.0; Q];
        if c < self.live_channels {
            &self.buf[c]
        } else {
            &SILENCE
        }
    }

    /// Adds the inputs that are heard right now to their strips.
    pub(crate) fn monitor(&self, mixer: &mut MixerEngine, playing: bool) {
        let Some(routes) = self.routes.as_deref() else {
            return;
        };
        if self.live_channels == 0 {
            return;
        }
        let recording = self.sink.is_some();
        for r in &routes.routes {
            let on = match r.armed {
                None => true,
                Some(MonitorMode::Off) => false,
                Some(MonitorMode::Mixer) => true,
                Some(MonitorMode::Auto) => !playing || recording,
            };
            if !on {
                continue;
            }
            let first = usize::from(r.input.first);
            let l = self.channel(first);
            let rr = if r.input.stereo {
                self.channel(first + 1)
            } else {
                l
            };
            let (dl, dr) = mixer.direct_mut(usize::from(r.strip));
            for i in 0..Q {
                dl[i] += l[i];
                dr[i] += rr[i];
            }
        }
    }

    /// Records this quantum: starts a take on the first quantum of playback (or where the
    /// punch region begins), writes the frames that belong to it, and ends it when playback
    /// stops, jumps or leaves the punch region.
    ///
    /// A take that ends by a jump or a punch-out keeps receiving input for `D` more frames (its
    /// tail): that input is what the player did while hearing the take's last `D` frames. The
    /// next take starts once the tail is written, skipping the same frames, so loop passes
    /// meet without a hole or an overlap.
    pub(crate) fn record(
        &mut self,
        playing: bool,
        segments: &[PlaySegment],
        transport: &Transport,
        sample_rate: f64,
        telemetry: &Telemetry,
    ) {
        use std::sync::atomic::Ordering::Relaxed;
        self.flush_end();
        let Some(sink) = self.sink.as_deref() else {
            return;
        };
        let (comp, punch) = (u64::from(sink.compensation), sink.punch);
        if !playing || segments.is_empty() {
            self.end_take();
            self.next = None;
            telemetry.recording.store(false, Relaxed);
            return;
        }
        self.gap_reported = false;
        for seg in segments {
            let jumped = self.take.is_some_and(|t| {
                t.tail.is_none() && (seg.seconds - t.expect_s).abs() * sample_rate > 0.5
            });
            if jumped {
                self.begin_tail(comp);
            }
            let seg_end_s = seg.seconds + f64::from(seg.frames) / sample_rate;
            let frame_of = |tick: f64| {
                let s = transport.seconds_at_tick(tick);
                (((s - seg.seconds) * sample_rate) - 1e-6).ceil().max(0.0) as u64
            };
            // Punch-out frame within this segment (if the region ends inside it).
            let out_at = punch
                .map(|(_, end)| end as f64)
                .filter(|&end| {
                    let s = transport.seconds_at_tick(end);
                    s >= seg.seconds && s <= seg_end_s
                })
                .map(frame_of);
            let len = u64::from(seg.frames);
            let mut pos = 0_u64;
            while pos < len {
                let recording = self.take.is_some_and(|t| t.tail.is_none());
                if self.take.is_none() || (!recording && self.next.is_none()) {
                    // Find where a new take starts in this segment, if anywhere.
                    let at_s = seg.seconds + pos as f64 / sample_rate;
                    let mut tick = (transport.tick_at_seconds(at_s) - 1e-6).ceil();
                    if let Some((start, _)) = punch {
                        tick = tick.max(start as f64);
                    }
                    let first = frame_of(tick).max(pos);
                    let inside = punch.is_none_or(|(_, end)| tick < end as f64);
                    if inside && first < len {
                        let skip = first - pos + comp;
                        if self.take.is_some() {
                            // The previous take's tail is still being written.
                            self.next = Some(Next {
                                tick: tick as i64,
                                skip,
                            });
                        } else {
                            self.start_take(tick as i64, skip, at_s);
                        }
                    }
                }
                let Some(mut take) = self.take else {
                    break;
                };
                let mut n = len - pos;
                if let Some(t) = take.tail {
                    n = n.min(t);
                }
                if let Some(o) = out_at.filter(|&o| take.tail.is_none() && o > pos) {
                    n = n.min(o - pos);
                }
                let skip = take.skip.min(n);
                take.skip -= skip;
                let written = n - skip;
                if written > 0 {
                    let from = (u64::from(seg.offset) + pos + skip) as usize;
                    let frames = written as u32;
                    let status = self.status;
                    if !self.gap_reported && (status.missing > 0 || status.dropped > 0) {
                        // Missing input was read as silence; dropped input leaves no hole,
                        // only a jump (0 frames).
                        self.gap_reported = true;
                        self.mark(RecMark::Gap {
                            at: take.frames,
                            frames: status.missing.min(frames),
                            fill: false,
                        });
                    }
                    if !(self.flush_fill() && self.write(from, from + frames as usize)) {
                        // Frames that did not fit become one growing gap, sent before the
                        // next frames that fit, so a long stall costs one mark.
                        match self.fill_pending.as_mut() {
                            Some((_, n)) => *n += frames,
                            None => {
                                self.fill_pending = Some((take.frames, frames));
                                telemetry.record_gaps.fetch_add(1, Relaxed);
                            }
                        }
                    }
                    take.frames += written;
                }
                take.expect_s = seg.seconds + (pos + n) as f64 / sample_rate;
                if let Some(next) = self.next.as_mut() {
                    next.skip = next.skip.saturating_sub(n);
                }
                pos += n;
                take.tail = take.tail.map(|t| t - n);
                self.take = Some(take);
                if take.tail == Some(0) {
                    self.end_take();
                    if let Some(next) = self.next.take() {
                        let at_s = seg.seconds + pos as f64 / sample_rate;
                        self.start_take(next.tick, next.skip, at_s);
                    }
                } else if take.tail.is_none() && out_at == Some(pos) {
                    self.begin_tail(comp);
                }
            }
        }
        let frames = self.take.map_or(0, |t| t.frames);
        telemetry.record_frames.store(frames, Relaxed);
        telemetry.recording.store(self.take.is_some(), Relaxed);
    }

    /// Ends the current take after `comp` more frames (at once when `comp` is 0).
    fn begin_tail(&mut self, comp: u64) {
        match self.take.as_mut() {
            Some(t) if comp > 0 && t.tail.is_none() => t.tail = Some(comp),
            Some(t) if t.tail.is_some() => {}
            _ => self.end_take(),
        }
    }

    /// Starts take `tick`, skipping `skip` frames first. False if the marks ring is full
    /// (tried again next quantum).
    fn start_take(&mut self, tick: i64, skip: u64, at_s: f64) -> bool {
        let Some(sink) = self.sink.as_deref_mut() else {
            return false;
        };
        if self.end_pending.is_some() || sink.marks.slots() < 2 {
            return false;
        }
        let _ = sink.marks.push(RecMark::Start {
            take: self.takes,
            tick,
        });
        self.takes += 1;
        self.take = Some(Take {
            skip,
            frames: 0,
            expect_s: at_s,
            tail: None,
        });
        true
    }

    /// Pushes frames `[from, to)` of this quantum for every lane. False (nothing pushed) when
    /// the ring has no room for all of them.
    fn write(&mut self, from: usize, to: usize) -> bool {
        let Some(sink) = self.sink.as_deref_mut() else {
            return false;
        };
        let width = sink.lanes.iter().map(|l| l.channels()).sum::<usize>();
        let n = (to - from) * width;
        if n == 0 {
            return true;
        }
        let Ok(chunk) = sink.samples.write_chunk_uninit(n) else {
            return false;
        };
        let buf = &self.buf;
        let live = self.live_channels;
        let lanes = &sink.lanes;
        let sample = move |c: usize, i: usize| if c < live { buf[c][i] } else { 0.0 };
        let iter = (from..to).flat_map(move |i| {
            lanes.iter().flat_map(move |lane| {
                let first = usize::from(lane.first);
                let two = lane.stereo;
                [sample(first, i), sample(first + 1, i)]
                    .into_iter()
                    .take(if two { 2 } else { 1 })
            })
        });
        chunk.fill_from_iter(iter);
        true
    }

    fn mark(&mut self, m: RecMark) {
        if let Some(sink) = self.sink.as_deref_mut() {
            // A lost gap mark only loses the report, never the file's timing.
            let _ = sink.marks.push(m);
        }
    }

    /// Ends the take in progress, if any.
    fn end_take(&mut self) {
        if let Some(t) = self.take.take() {
            self.end_pending = Some(t.frames);
            self.flush_end();
        }
    }

    /// Sends the pending filled gap. False while the marks ring has no room for it (nothing
    /// may be written after it until it is sent, or the file would lose its timing).
    fn flush_fill(&mut self) -> bool {
        let (Some((at, frames)), Some(sink)) = (self.fill_pending, self.sink.as_deref_mut()) else {
            return true;
        };
        let sent = sink
            .marks
            .push(RecMark::Gap {
                at,
                frames,
                fill: true,
            })
            .is_ok();
        if sent {
            self.fill_pending = None;
        }
        sent
    }

    fn flush_end(&mut self) {
        if !self.flush_fill() {
            return;
        }
        let (Some(frames), Some(sink)) = (self.end_pending, self.sink.as_deref_mut()) else {
            return;
        };
        if sink.marks.push(RecMark::End { frames }).is_ok() {
            self.end_pending = None;
        }
    }

    /// Plays the measurement's bursts into `out` (mono, before the master fade) and sends
    /// what the input heard alongside them.
    pub(crate) fn calibrate(&mut self, out: &mut [f32; Q], sample_rate: f32) {
        let Some(calib) = self.calib.as_deref_mut() else {
            return;
        };
        let total = latency::total_frames(sample_rate);
        if self.calib_frame >= total {
            return;
        }
        let c = usize::from(calib.channel);
        let input = if c < self.live_channels {
            &self.buf[c]
        } else {
            &[0.0; Q]
        };
        let n = Q.min(total - self.calib_frame);
        for (i, o) in out.iter_mut().enumerate().take(n) {
            *o += latency::signal_at(self.calib_frame + i, sample_rate);
        }
        if let Ok(chunk) = calib.samples.write_chunk_uninit(n) {
            chunk.fill_from_iter(input[..n].iter().copied());
        }
        self.calib_frame += n;
    }
}
