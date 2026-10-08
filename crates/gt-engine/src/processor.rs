//! The audio-thread half of the engine.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use gt_core::Tick;
use gt_dsp::{db_to_gain, Click, LinearRamp, SineOsc};
use rtrb::{Consumer, Producer};

use crate::command::{EngineCommand, EngineEvent, Garbage};
use crate::transport::{EventBuf, EventKind, Transport};
use crate::{EngineConfig, Telemetry, FADE_SECONDS, RENDER_QUANTUM};

/// Frequency of the device test tone (concert A).
pub const TEST_TONE_HZ: f32 = 440.0;
/// Level of the device test tone. -12 dBFS leaves headroom and is comfortable on speakers.
pub const TEST_TONE_DBFS: f32 = -12.0;

/// Metronome levels and pitches. The downbeat is higher so bar starts are audible.
const CLICK_DBFS: f32 = -9.0;
const CLICK_HZ: f32 = 1000.0;
const CLICK_DOWNBEAT_HZ: f32 = 1600.0;

/// Commands applied per quantum at most, so a flood of commands cannot stall a callback.
const MAX_COMMANDS_PER_QUANTUM: usize = 64;

const Q: usize = RENDER_QUANTUM;

/// Renders audio. Lives on the audio thread; every method here is real-time safe:
/// no allocation, no locks, no I/O, no panics for any buffer length.
pub struct AudioProcessor {
    telemetry: Arc<Telemetry>,
    commands: Consumer<EngineCommand>,
    events: Producer<EngineEvent>,
    garbage: Producer<Garbage>,
    channels: usize,

    transport: Transport,
    scheduled: EventBuf,
    /// Engine frame at the start of the next quantum.
    frame_clock: u64,

    /// The current quantum (mono for now; ARCHITECTURE.md's stereo buses come with the mixer).
    quantum: [f32; Q],
    /// Read position inside `quantum`; `Q` means "render the next one".
    fifo_pos: usize,

    click: Click,
    click_gain: f32,
    metronome: bool,

    tone: SineOsc,
    tone_ramp: LinearRamp,
    tone_gain: f32,
    tone_on: bool,

    master: LinearRamp,
    fade_frames: u32,
}

impl std::fmt::Debug for AudioProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioProcessor")
            .field("channels", &self.channels)
            .field("frame_clock", &self.frame_clock)
            .field("transport", &self.transport)
            .finish_non_exhaustive()
    }
}

impl AudioProcessor {
    pub(crate) fn new(
        config: EngineConfig,
        telemetry: Arc<Telemetry>,
        commands: Consumer<EngineCommand>,
        events: Producer<EngineEvent>,
        garbage: Producer<Garbage>,
    ) -> Self {
        let sr = config.sample_rate.max(1) as f32;
        Self {
            telemetry,
            commands,
            events,
            garbage,
            channels: config.out_channels.max(1),
            transport: Transport::new(config.sample_rate, Box::default()),
            scheduled: EventBuf::default(),
            frame_clock: 0,
            quantum: [0.0; Q],
            fifo_pos: Q,
            click: Click::new(sr),
            click_gain: db_to_gain(CLICK_DBFS),
            metronome: true,
            tone: SineOsc::new(sr, TEST_TONE_HZ),
            tone_ramp: LinearRamp::new(0.0),
            tone_gain: db_to_gain(TEST_TONE_DBFS),
            tone_on: false,
            master: LinearRamp::new(1.0),
            fade_frames: (sr * FADE_SECONDS).round() as u32,
        }
    }

    /// The telemetry this processor publishes (also visible through the `EngineHandle`).
    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    /// Fills `out`, an interleaved buffer of `frames * out_channels` samples, from the stream of
    /// fixed-size quanta. A trailing partial frame (which a correct device never delivers) is
    /// zeroed.
    pub fn process(&mut self, out: &mut [f32]) {
        let ch = self.channels;
        let frames = out.len() / ch;
        let mut peak = 0.0_f32;
        let mut written = 0;
        while written < frames {
            if self.fifo_pos == Q {
                self.render_quantum();
                self.fifo_pos = 0;
            }
            let n = (Q - self.fifo_pos).min(frames - written);
            let src = &self.quantum[self.fifo_pos..self.fifo_pos + n];
            let dst = &mut out[written * ch..(written + n) * ch];
            for (frame, &s) in dst.chunks_exact_mut(ch).zip(src) {
                frame.fill(s);
                peak = peak.max(s.abs());
            }
            self.fifo_pos += n;
            written += n;
        }
        out[frames * ch..].fill(0.0);

        let t = &*self.telemetry;
        t.last_block_frames.store(frames as u32, Ordering::Relaxed);
        t.blocks.fetch_add(1, Ordering::Relaxed);
        t.peak.fetch_max(peak, Ordering::Relaxed);
    }

    /// Renders the next `RENDER_QUANTUM` frames into `self.quantum`.
    fn render_quantum(&mut self) {
        self.apply_commands();

        self.quantum.fill(0.0);
        self.transport
            .schedule(self.frame_clock, Q as u32, &mut self.scheduled);

        // Sample-accurate events: render up to each event's offset, apply it, continue.
        let mut cursor = 0;
        for i in 0..self.scheduled.as_slice().len() {
            let e = self.scheduled.as_slice()[i];
            let at = (e.offset as usize).min(Q);
            self.click.add_to(&mut self.quantum[cursor..at]);
            cursor = at;
            match e.kind {
                EventKind::Beat { downbeat } if self.metronome => {
                    let hz = if downbeat {
                        CLICK_DOWNBEAT_HZ
                    } else {
                        CLICK_HZ
                    };
                    self.click.trigger(hz, self.click_gain);
                }
                EventKind::Beat { .. } => {}
            }
        }
        self.click.add_to(&mut self.quantum[cursor..]);

        let tone_idle = !self.tone_on && self.tone_ramp.is_settled();
        for s in &mut self.quantum {
            if !tone_idle {
                *s += self.tone.next_sample() * self.tone_ramp.next_value();
            }
            *s *= self.master.next_value();
        }

        self.frame_clock += Q as u64;
        self.publish();
    }

    fn apply_commands(&mut self) {
        for _ in 0..MAX_COMMANDS_PER_QUANTUM {
            // A tempo swap hands the old map back; only take it if there is room for that, so
            // the audio thread never has to free it. Otherwise retry next quantum.
            let needs_gc = matches!(self.commands.peek(), Ok(EngineCommand::SetTempoMap(_)));
            if needs_gc && self.garbage.slots() == 0 {
                break;
            }
            let Ok(cmd) = self.commands.pop() else {
                break;
            };
            self.apply(cmd);
        }
    }

    fn apply(&mut self, cmd: EngineCommand) {
        let now = self.frame_clock;
        let before = self.transport.state();
        match cmd {
            EngineCommand::Play => self.transport.play(now),
            EngineCommand::Pause => self.transport.pause(now),
            EngineCommand::Stop => self.transport.stop(),
            EngineCommand::Locate(t) => self.transport.locate(t, now),
            EngineCommand::SetLoop(region) => self.transport.set_loop(region),
            EngineCommand::SetTimeSig(sig) => self.transport.set_time_sig(sig),
            EngineCommand::SetTempoMap(map) => {
                let old = self.transport.set_tempo_map(map, now);
                self.retire(Garbage::TempoMap(old));
            }
            EngineCommand::SetMetronome(on) => self.metronome = on,
            EngineCommand::SetTestTone(on) => {
                if on && !self.tone_on && self.tone_ramp.value() == 0.0 {
                    // Start from a zero crossing so the fade-in begins cleanly.
                    self.tone.reset();
                }
                self.tone_on = on;
                let target = if on { self.tone_gain } else { 0.0 };
                self.tone_ramp.set_target(target, self.fade_frames);
            }
            EngineCommand::FadeOut => self.master.set_target(0.0, self.fade_frames),
            EngineCommand::FadeIn => self.master.set_target(1.0, self.fade_frames),
        }
        if before != self.transport.state() {
            self.post(EngineEvent::TransportChanged {
                state: self.transport.state(),
                position: Tick(self.transport.position_at(now).floor() as i64),
            });
        }
    }

    /// Hands an object back to the UI thread for freeing.
    fn retire(&mut self, g: Garbage) {
        if let Err(rtrb::PushError::Full(g)) = self.garbage.push(g) {
            // Unreachable: callers check for a free slot first. Leaking is the safe fallback;
            // freeing here would break the real-time contract.
            std::mem::forget(g);
        }
    }

    fn post(&mut self, e: EngineEvent) {
        if self.events.push(e).is_err() {
            self.telemetry
                .events_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn publish(&self) {
        let t = &*self.telemetry;
        let pos = self.transport.position_at(self.frame_clock);
        t.position_ticks
            .store(pos.floor() as i64, Ordering::Relaxed);
        t.transport_state
            .store(self.transport.state() as u8, Ordering::Relaxed);
        t.frames_rendered.store(self.frame_clock, Ordering::Relaxed);
        t.silent.store(
            self.master.is_settled() && self.master.value() == 0.0,
            Ordering::Relaxed,
        );
        let dropped = self.scheduled.dropped();
        if dropped > 0 {
            t.events_dropped.fetch_max(dropped, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{create, AudioProcessor, EngineCommand, EngineConfig, EngineHandle};
    use std::sync::atomic::Ordering;

    fn engine(sr: u32, ch: usize) -> (EngineHandle, AudioProcessor) {
        create(EngineConfig {
            sample_rate: sr,
            out_channels: ch,
        })
    }

    #[test]
    fn silent_until_something_plays() {
        let (_h, mut p) = engine(48_000, 2);
        let mut buf = vec![1.0; 512];
        p.process(&mut buf);
        assert!(buf.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn test_tone_plays_at_minus_12_dbfs_on_all_channels() {
        let (mut h, mut p) = engine(48_000, 2);
        h.send(EngineCommand::SetTestTone(true)).unwrap();
        let mut buf = vec![0.0; 2 * 48_000];
        p.process(&mut buf);
        let peak = buf[2 * 4800..].iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.251_188_64).abs() < 1e-3, "{peak}");
        for f in buf.chunks_exact(2) {
            assert_eq!(f[0], f[1]);
        }
    }

    #[test]
    fn tone_fades_have_no_steps() {
        let (mut h, mut p) = engine(44_100, 1);
        h.send(EngineCommand::SetTestTone(true)).unwrap();
        let mut buf = vec![0.0; 4410];
        p.process(&mut buf);
        h.send(EngineCommand::SetTestTone(false)).unwrap();
        let mut tail = vec![0.0; 4410];
        p.process(&mut tail);
        buf.extend_from_slice(&tail);
        // Largest step of a 440 Hz sine at amplitude a is a * 2π * 440 / 44100 ≈ 0.0157.
        let max_step = buf
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(max_step < 0.02, "{max_step}");
        assert_eq!(tail[tail.len() - 1], 0.0);
    }

    #[test]
    fn fade_out_reports_silent() {
        let (mut h, mut p) = engine(48_000, 2);
        h.send(EngineCommand::SetTestTone(true)).unwrap();
        let mut buf = vec![0.0; 2048];
        p.process(&mut buf);
        assert!(!h.telemetry().silent.load(Ordering::Relaxed));
        h.send(EngineCommand::FadeOut).unwrap();
        for _ in 0..10 {
            p.process(&mut buf);
        }
        assert!(h.telemetry().silent.load(Ordering::Relaxed));
        assert!(buf.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn odd_block_sizes_and_partial_frames_are_handled() {
        let (mut h, mut p) = engine(48_000, 2);
        h.send(EngineCommand::SetTestTone(true)).unwrap();
        for len in [0, 1, 2, 3, 441 * 2, 1023] {
            let mut buf = vec![9.0; len];
            p.process(&mut buf);
            assert!(buf.iter().all(|s| s.abs() <= 0.26), "len {len}");
        }
        assert_eq!(h.telemetry().last_block_frames.load(Ordering::Relaxed), 511);
    }

    #[test]
    fn transport_state_is_published_and_evented() {
        let (mut h, mut p) = engine(48_000, 1);
        h.send(EngineCommand::Play).unwrap();
        let mut buf = vec![0.0; 48_000];
        p.process(&mut buf);
        let t = h.telemetry();
        assert_eq!(t.transport_state(), crate::TransportState::Playing);
        // 1 s at 120 BPM = 2 beats = 1920 ticks (rendered up to the next quantum boundary).
        assert!(
            (1920..1920 + 64).contains(&t.position().0),
            "{:?}",
            t.position()
        );
        let mut events = Vec::new();
        h.poll_events(|e| events.push(e));
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn tempo_map_comes_back_as_garbage() {
        let (mut h, mut p) = engine(48_000, 1);
        h.send(EngineCommand::SetTempoMap(Box::new(
            gt_core::TempoMap::constant(90.0),
        )))
        .unwrap();
        let mut buf = vec![0.0; 64];
        p.process(&mut buf);
        assert_eq!(h.collect_garbage(), 1);
    }
}
