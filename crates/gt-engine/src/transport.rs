//! Transport and scheduler.
//!
//! The transport holds an *anchor*: the engine frame `anchor_sample` at which the song was at
//! `anchor_seconds` (song time from tick 0, via the tempo map). Everything else is derived:
//!
//! - position at frame `s`: `tempo.seconds_to_tick(anchor_seconds + (s - anchor_sample) / sr)`
//! - frame of tick `t`: `anchor_sample + round((tempo.tick_to_seconds(t) - anchor_seconds) * sr)`
//!
//! Nothing is accumulated per buffer, so there is no drift. Play, locate, loop wraps and tempo
//! map swaps re-anchor. Tempo changes inside the map need no special handling: the tempo map's
//! piecewise conversion already places events after a change correctly.
//!
//! Rounding: an event fires on the frame nearest to its exact time; an exact tie (x.5) goes to
//! the later frame. Every tick maps to exactly one frame and render quanta partition the frames,
//! so each event fires exactly once regardless of how the timeline is cut into buffers.

use gt_core::{TempoMap, Tick, TimeSig, PPQ};

use crate::command::{LoopRegion, TransportState};

/// Shortest loop accepted: a 1/16 note. Shorter regions disable looping.
pub const MIN_LOOP_TICKS: i64 = PPQ / 4;
/// Capacity of the per-quantum event list.
pub const MAX_EVENTS_PER_QUANTUM: usize = 64;
/// Upper bound on loop wraps inside one quantum (protects against pathological input).
const MAX_SEGMENTS: usize = 64;

/// What happens at a scheduled event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A metronome beat. `downbeat` is the first beat of a bar.
    Beat {
        /// True on beat 1.
        downbeat: bool,
    },
}

/// An event placed on a frame inside the current quantum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledEvent {
    /// Frame offset from the start of the quantum.
    pub offset: u32,
    /// Musical position of the event.
    pub tick: Tick,
    /// What happens.
    pub kind: EventKind,
}

const NO_EVENT: ScheduledEvent = ScheduledEvent {
    offset: 0,
    tick: Tick(0),
    kind: EventKind::Beat { downbeat: false },
};

/// Fixed-capacity, allocation-free list of events for one quantum, sorted by offset.
#[derive(Debug, Clone)]
pub struct EventBuf {
    events: [ScheduledEvent; MAX_EVENTS_PER_QUANTUM],
    len: usize,
    dropped: u32,
}

impl Default for EventBuf {
    fn default() -> Self {
        Self {
            events: [NO_EVENT; MAX_EVENTS_PER_QUANTUM],
            len: 0,
            dropped: 0,
        }
    }
}

impl EventBuf {
    /// Empties the list (keeps the dropped counter).
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// The events, in order.
    pub fn as_slice(&self) -> &[ScheduledEvent] {
        &self.events[..self.len]
    }

    /// Events that did not fit since creation.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    fn push(&mut self, e: ScheduledEvent) {
        if self.len < MAX_EVENTS_PER_QUANTUM {
            self.events[self.len] = e;
            self.len += 1;
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }
}

/// Transport state machine and event scheduler. Real-time safe: no allocation in any method
/// except [`Transport::new`].
#[derive(Debug)]
pub struct Transport {
    sr: f64,
    tempo: Box<TempoMap>,
    sig: TimeSig,
    looping: LoopRegion,
    state: TransportState,
    anchor_sample: u64,
    anchor_seconds: f64,
    /// Position in ticks while not playing.
    held: f64,
    /// Where the last Play started, for Stop.
    play_start: f64,
}

impl Transport {
    /// Creates a stopped transport at bar 1.
    pub fn new(sample_rate: u32, tempo: Box<TempoMap>) -> Self {
        Self {
            sr: f64::from(sample_rate.max(1)),
            tempo,
            sig: TimeSig::default(),
            looping: LoopRegion::default(),
            state: TransportState::Stopped,
            anchor_sample: 0,
            anchor_seconds: 0.0,
            held: 0.0,
            play_start: 0.0,
        }
    }

    /// Current state.
    pub fn state(&self) -> TransportState {
        self.state
    }

    /// Current time signature.
    pub fn time_sig(&self) -> TimeSig {
        self.sig
    }

    /// Current loop region (after validation).
    pub fn loop_region(&self) -> LoopRegion {
        self.looping
    }

    /// Fractional tick position at engine frame `now`.
    pub fn position_at(&self, now: u64) -> f64 {
        match self.state {
            TransportState::Playing => self.tick_at_frame(now as f64),
            _ => self.held,
        }
    }

    /// Starts playback at frame `now`. No-op while already playing.
    pub fn play(&mut self, now: u64) {
        if self.state == TransportState::Playing {
            return;
        }
        if self.state == TransportState::Stopped {
            self.play_start = self.held;
        }
        let pos = self.held;
        self.state = TransportState::Playing;
        self.anchor(pos, now);
    }

    /// Stops at frame `now`, holding the position.
    pub fn pause(&mut self, now: u64) {
        if self.state == TransportState::Playing {
            self.held = self.position_at(now);
            self.state = TransportState::Paused;
        }
    }

    /// Stops and returns to where playback started; when already stopped, to bar 1.
    pub fn stop(&mut self) {
        self.held = if self.state == TransportState::Stopped {
            0.0
        } else {
            self.play_start
        };
        self.play_start = self.held;
        self.state = TransportState::Stopped;
    }

    /// Moves the playhead to `t` at frame `now`. Stop will return here.
    pub fn locate(&mut self, t: Tick, now: u64) {
        let pos = t.0 as f64;
        self.play_start = pos;
        self.held = pos;
        if self.state == TransportState::Playing {
            self.anchor(pos, now);
        }
    }

    /// Sets the loop region; regions shorter than [`MIN_LOOP_TICKS`] are disabled.
    pub fn set_loop(&mut self, region: LoopRegion) {
        let valid = region.end.0 - region.start.0 >= MIN_LOOP_TICKS;
        self.looping = LoopRegion {
            enabled: region.enabled && valid,
            ..region
        };
    }

    /// Sets the time signature.
    pub fn set_time_sig(&mut self, sig: TimeSig) {
        self.sig = sig;
    }

    /// Swaps in a new tempo map at frame `now`, keeping the musical position. Returns the old map
    /// so the caller can hand it to the garbage queue instead of freeing it here.
    pub fn set_tempo_map(&mut self, tempo: Box<TempoMap>, now: u64) -> Box<TempoMap> {
        let pos = self.position_at(now);
        let old = core::mem::replace(&mut self.tempo, tempo);
        if self.state == TransportState::Playing {
            self.anchor(pos, now);
        }
        old
    }

    /// Schedules the events for frames `[q_start, q_start + frames)` into `out` (cleared first)
    /// and advances through loop wraps. Does nothing unless playing.
    pub fn schedule(&mut self, q_start: u64, frames: u32, out: &mut EventBuf) {
        out.clear();
        if self.state != TransportState::Playing {
            return;
        }
        let q_end = q_start + u64::from(frames);
        let mut seg_start = q_start;
        for _ in 0..MAX_SEGMENTS {
            if seg_start >= q_end {
                break;
            }
            let mut seg_end = q_end;
            let mut wrap = false;
            let end_tick = self.looping.end.0 as f64;
            if self.looping.enabled {
                // Decide in the frame domain, with the same rounding as events: the loop end
                // is "ahead" if its frame is at or after this segment's start. If it is behind
                // (playback was located past the loop), play on without wrapping.
                let wrap_at = self.frame_of_tick(end_tick);
                if wrap_at >= seg_start as i64 && wrap_at < q_end as i64 {
                    seg_end = wrap_at as u64;
                    wrap = true;
                }
            }
            let limit = if wrap { Some(self.looping.end.0) } else { None };
            self.collect(seg_start, seg_end, limit, q_start, out);
            if wrap {
                self.anchor(self.looping.start.0 as f64, seg_end);
            }
            seg_start = seg_end;
        }
    }

    /// Adds metronome beats whose frame is in `[a, b)` (and whose tick is below `limit`).
    fn collect(&self, a: u64, b: u64, limit: Option<i64>, q_start: u64, out: &mut EventBuf) {
        if a >= b {
            return;
        }
        // Candidate tick range, widened by a tick on each side; the exact frame test below
        // decides membership, so widening can never double-fire an event.
        let t_lo = self.tick_at_frame(a as f64 - 1.0).floor() as i64 - 1;
        let t_hi = self.tick_at_frame(b as f64).ceil() as i64 + 1;
        let beat = self.sig.beat_ticks();
        let bar = self.sig.bar_ticks();
        let mut k = t_lo.div_euclid(beat) + 1;
        while k * beat <= t_hi {
            let tick = k * beat;
            k += 1;
            if limit.is_some_and(|l| tick >= l) {
                break;
            }
            let f = self.frame_of_tick(tick as f64);
            if f >= a as i64 && f < b as i64 {
                out.push(ScheduledEvent {
                    offset: (f - q_start as i64) as u32,
                    tick: Tick(tick),
                    kind: EventKind::Beat {
                        downbeat: tick.rem_euclid(bar) == 0,
                    },
                });
            }
        }
    }

    fn anchor(&mut self, tick: f64, frame: u64) {
        self.anchor_sample = frame;
        self.anchor_seconds = self.tempo.tick_to_seconds(tick);
    }

    #[inline]
    fn tick_at_frame(&self, frame: f64) -> f64 {
        let seconds = self.anchor_seconds + (frame - self.anchor_sample as f64) / self.sr;
        self.tempo.seconds_to_tick(seconds)
    }

    #[inline]
    fn frame_of_tick(&self, tick: f64) -> i64 {
        let rel = (self.tempo.tick_to_seconds(tick) - self.anchor_seconds) * self.sr;
        self.anchor_sample as i64 + (rel + 0.5).floor() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_core::TempoPoint;

    /// Runs the scheduler over `seconds` in quanta of `q` frames and returns (frame, tick) pairs.
    fn run(t: &mut Transport, sr: u32, seconds: f64, q: u32) -> Vec<(u64, i64)> {
        let mut buf = EventBuf::default();
        let mut out = Vec::new();
        let total = (seconds * f64::from(sr)) as u64;
        let mut f = 0;
        while f < total {
            t.schedule(f, q, &mut buf);
            out.extend(
                buf.as_slice()
                    .iter()
                    .map(|e| (f + u64::from(e.offset), e.tick.0))
                    .filter(|&(frame, _)| frame < total),
            );
            f += u64::from(q);
        }
        assert_eq!(buf.dropped(), 0);
        out
    }

    fn expected_frame(seconds: f64, sr: u32) -> u64 {
        (seconds * f64::from(sr) + 0.5).floor() as u64
    }

    #[test]
    fn beats_land_on_exact_frames_at_common_rates() {
        for sr in [44_100, 48_000, 96_000] {
            for bpm in [60.0, 120.0, 137.0, 174.5] {
                let mut t = Transport::new(sr, Box::new(TempoMap::constant(bpm)));
                t.play(0);
                let got = run(&mut t, sr, 30.0, 64);
                for (k, &(frame, tick)) in got.iter().enumerate() {
                    assert_eq!(tick, k as i64 * PPQ);
                    let want = expected_frame(k as f64 * 60.0 / bpm, sr);
                    assert_eq!(frame, want, "sr {sr} bpm {bpm} beat {k}");
                }
                let beats = (0..)
                    .take_while(|&k| expected_frame(k as f64 * 60.0 / bpm, sr) < u64::from(sr) * 30)
                    .count();
                assert_eq!(got.len(), beats, "sr {sr} bpm {bpm}");
            }
        }
    }

    #[test]
    fn quantum_size_does_not_change_the_result() {
        let mk = || {
            let mut t = Transport::new(44_100, Box::new(TempoMap::constant(133.0)));
            t.set_loop(LoopRegion {
                start: Tick(PPQ * 2),
                end: Tick(PPQ * 9),
                enabled: true,
            });
            t.play(0);
            t
        };
        let reference = run(&mut mk(), 44_100, 20.0, 64);
        for q in [1, 7, 63, 441, 1024] {
            assert_eq!(run(&mut mk(), 44_100, 20.0, q), reference, "quantum {q}");
        }
    }

    #[test]
    fn beats_follow_tempo_changes() {
        // 120 BPM for two bars (8 beats, 4 s), then 90 BPM.
        let map = TempoMap::new(&[
            TempoPoint {
                at: Tick(0),
                bpm: 120.0,
            },
            TempoPoint {
                at: Tick(8 * PPQ),
                bpm: 90.0,
            },
        ])
        .unwrap();
        for sr in [44_100, 48_000, 96_000] {
            let mut t = Transport::new(sr, Box::new(map.clone()));
            t.play(0);
            let got = run(&mut t, sr, 12.0, 64);
            for (k, &(frame, _)) in got.iter().enumerate() {
                let secs = if k < 8 {
                    k as f64 * 0.5
                } else {
                    4.0 + (k - 8) as f64 * 60.0 / 90.0
                };
                assert_eq!(frame, expected_frame(secs, sr), "sr {sr} beat {k}");
            }
            assert_eq!(got.len(), 8 + 12); // 4 s at 120, then 8 s at 90 = 12 beats
        }
    }

    #[test]
    fn tempo_swap_mid_play_keeps_position() {
        let sr = 48_000;
        let mut t = Transport::new(sr, Box::new(TempoMap::constant(120.0)));
        t.play(0);
        // After exactly 1 s at 120 BPM we are on beat 3 (tick 1920).
        let at = 48_000;
        assert!((t.position_at(at) - 1920.0).abs() < 1e-9);
        let _old = t.set_tempo_map(Box::new(TempoMap::constant(60.0)), at);
        assert!((t.position_at(at) - 1920.0).abs() < 1e-9);
        // Next beat (tick 2880) is now 1 s away instead of 0.5 s.
        let mut buf = EventBuf::default();
        t.schedule(at + 47_990, 64, &mut buf);
        assert_eq!(buf.as_slice()[0].offset, 10);
        assert_eq!(buf.as_slice()[0].tick, Tick(2880));
    }

    #[test]
    fn loop_wraps_and_stays_periodic() {
        let sr = 44_100;
        let bpm = 133.0;
        let mut t = Transport::new(sr, Box::new(TempoMap::constant(bpm)));
        // Loop bar 2 (ticks 3840..7680), start playing at bar 1.
        t.set_loop(LoopRegion {
            start: Tick(3840),
            end: Tick(7680),
            enabled: true,
        });
        t.play(0);
        let got = run(&mut t, sr, 20.0, 64);
        let beat = 60.0 / bpm;
        let bar_frames = expected_frame(4.0 * beat, sr);
        let wrap1 = expected_frame(8.0 * beat, sr);
        for (i, &(frame, tick)) in got.iter().enumerate() {
            if i < 8 {
                assert_eq!(frame, expected_frame(i as f64 * beat, sr));
                assert_eq!(tick, i as i64 * PPQ);
            } else {
                let pass = (i - 8) / 4;
                let j = (i - 8) % 4;
                let base = wrap1 + pass as u64 * bar_frames;
                assert_eq!(
                    frame,
                    base + expected_frame(j as f64 * beat, sr),
                    "event {i}"
                );
                assert_eq!(tick, 3840 + j as i64 * PPQ);
            }
        }
        assert!(got.len() > 30);
    }

    #[test]
    fn playing_past_the_loop_end_does_not_wrap() {
        let mut t = Transport::new(48_000, Box::new(TempoMap::constant(120.0)));
        t.set_loop(LoopRegion {
            start: Tick(0),
            end: Tick(3840),
            enabled: true,
        });
        t.locate(Tick(7680), 0);
        t.play(0);
        let got = run(&mut t, 48_000, 3.0, 64);
        assert_eq!(got[0], (0, 7680));
        assert!(got.iter().all(|&(_, tick)| tick >= 7680));
    }

    #[test]
    fn too_short_loop_is_disabled() {
        let mut t = Transport::new(48_000, Box::new(TempoMap::constant(120.0)));
        t.set_loop(LoopRegion {
            start: Tick(100),
            end: Tick(101),
            enabled: true,
        });
        assert!(!t.loop_region().enabled);
    }

    #[test]
    fn pause_holds_and_stop_returns() {
        let mut t = Transport::new(48_000, Box::new(TempoMap::constant(120.0)));
        t.locate(Tick(960), 0);
        t.play(0);
        t.pause(24_000); // 0.5 s = one beat later
        assert_eq!(t.state(), TransportState::Paused);
        assert!((t.position_at(99_999) - 1920.0).abs() < 1e-9);
        t.play(100_000);
        assert!((t.position_at(124_000) - 2880.0).abs() < 1e-9);
        t.stop();
        assert_eq!(t.position_at(0), 960.0);
        t.stop();
        assert_eq!(t.position_at(0), 0.0);
    }

    #[test]
    fn odd_meter_marks_downbeats() {
        let mut t = Transport::new(48_000, Box::new(TempoMap::constant(120.0)));
        t.set_time_sig(TimeSig::new(7, 8));
        t.play(0);
        let mut buf = EventBuf::default();
        let mut downbeats = Vec::new();
        let mut f = 0;
        while f < 48_000 * 4 {
            t.schedule(f, 64, &mut buf);
            for e in buf.as_slice() {
                if e.kind == (EventKind::Beat { downbeat: true }) {
                    downbeats.push(e.tick.0);
                }
            }
            f += 64;
        }
        assert_eq!(&downbeats[..3], &[0, 3360, 6720]);
    }
}
