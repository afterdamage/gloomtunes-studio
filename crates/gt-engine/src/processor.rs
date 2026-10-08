//! The audio-thread half of the engine.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use gt_core::{Tick, MAX_CHANNELS, ROOT_KEY};
use gt_dsp::{db_to_gain, Click, LinearRamp, SineOsc};
use rtrb::{Consumer, Producer};

use crate::channel::ChannelSlot;
use crate::command::{EngineCommand, EngineEvent, Garbage, TransportState};
use crate::song::{ChannelParams, SongSnapshot};
use crate::transport::{EventBuf, EventKind, ScheduledEvent, Transport};
use crate::{EngineConfig, Telemetry, FADE_SECONDS, RENDER_QUANTUM, SCOPE_LEN};

/// Frequency of the device test tone (concert A).
pub const TEST_TONE_HZ: f32 = 440.0;
/// Level of the device test tone. -12 dBFS leaves headroom and is comfortable on speakers.
pub const TEST_TONE_DBFS: f32 = -12.0;
/// Linear gain of the sample-browser preview voice.
pub const PREVIEW_GAIN: f32 = 0.7;

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
    out_channels: usize,

    transport: Transport,
    scheduled: EventBuf,
    /// Engine frame at the start of the next quantum.
    frame_clock: u64,

    /// The current quantum, stereo (planar).
    quantum_l: [f32; Q],
    quantum_r: [f32; Q],
    /// Read position inside the quantum; `Q` means "render the next one".
    fifo_pos: usize,
    /// Mono scratch for the metronome and test tone.
    mono: [f32; Q],
    /// Per-channel scratch, reused for each channel in turn.
    scratch_l: [f32; Q],
    scratch_r: [f32; Q],

    song: Option<Box<SongSnapshot>>,
    /// `MAX_CHANNELS` slots, allocated once in `new`.
    channels: Vec<ChannelSlot>,
    preview: ChannelSlot,
    /// Increments with every note-on; orders voices for stealing.
    voice_age: u64,
    /// Channel slot that feeds the oscilloscope.
    scope_slot: Option<usize>,

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
            .field("out_channels", &self.out_channels)
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
        let mut preview = ChannelSlot::new(sr);
        preview.set_params_now(ChannelParams {
            gain: PREVIEW_GAIN,
            ..ChannelParams::default()
        });
        Self {
            telemetry,
            commands,
            events,
            garbage,
            out_channels: config.out_channels.max(1),
            transport: Transport::new(config.sample_rate, Box::default()),
            scheduled: EventBuf::default(),
            frame_clock: 0,
            quantum_l: [0.0; Q],
            quantum_r: [0.0; Q],
            fifo_pos: Q,
            mono: [0.0; Q],
            scratch_l: [0.0; Q],
            scratch_r: [0.0; Q],
            song: None,
            channels: (0..MAX_CHANNELS).map(|_| ChannelSlot::new(sr)).collect(),
            preview,
            voice_age: 0,
            scope_slot: None,
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
    /// fixed-size quanta. Left and right go to the first two device channels (further channels
    /// get silence); a mono device gets their average. A trailing partial frame (which a correct
    /// device never delivers) is zeroed.
    pub fn process(&mut self, out: &mut [f32]) {
        let ch = self.out_channels;
        let frames = out.len() / ch;
        let mut peak = 0.0_f32;
        let mut written = 0;
        while written < frames {
            if self.fifo_pos == Q {
                self.render_quantum();
                self.fifo_pos = 0;
            }
            let n = (Q - self.fifo_pos).min(frames - written);
            let range = self.fifo_pos..self.fifo_pos + n;
            let src = self.quantum_l[range.clone()]
                .iter()
                .zip(&self.quantum_r[range]);
            let dst = &mut out[written * ch..(written + n) * ch];
            for (frame, (&l, &r)) in dst.chunks_exact_mut(ch).zip(src) {
                if let [mono] = frame {
                    *mono = 0.5 * (l + r);
                } else {
                    frame[0] = l;
                    frame[1] = r;
                    frame[2..].fill(0.0);
                }
                peak = peak.max(l.abs()).max(r.abs());
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

    /// Renders the next `RENDER_QUANTUM` frames into the quantum buffers.
    fn render_quantum(&mut self) {
        self.apply_commands();

        self.quantum_l.fill(0.0);
        self.quantum_r.fill(0.0);
        self.mono.fill(0.0);
        self.transport.schedule(
            self.frame_clock,
            Q as u32,
            self.song.as_deref(),
            &mut self.scheduled,
        );

        // Metronome, sample-accurate: render up to each beat, trigger, continue.
        let mut cursor = 0;
        for e in self.scheduled.as_slice() {
            let EventKind::Beat { downbeat } = e.kind else {
                continue;
            };
            let at = (e.offset as usize).min(Q);
            self.click.add_to(&mut self.mono[cursor..at]);
            cursor = at;
            if self.metronome {
                let hz = if downbeat {
                    CLICK_DOWNBEAT_HZ
                } else {
                    CLICK_HZ
                };
                self.click.trigger(hz, self.click_gain);
            }
        }
        self.click.add_to(&mut self.mono[cursor..]);

        for slot in 0..self.channels.len() {
            self.render_channel(slot);
        }
        if self.preview.is_active() {
            self.scratch_l.fill(0.0);
            self.scratch_r.fill(0.0);
            self.preview
                .render_voices(&mut self.scratch_l, &mut self.scratch_r);
            self.preview.mix_into(
                (&self.scratch_l, &self.scratch_r),
                (&mut self.quantum_l, &mut self.quantum_r),
            );
        }

        let tone_idle = !self.tone_on && self.tone_ramp.is_settled();
        for i in 0..Q {
            let mut m = self.mono[i];
            if !tone_idle {
                m += self.tone.next_sample() * self.tone_ramp.next_value();
            }
            let g = self.master.next_value();
            self.quantum_l[i] = (self.quantum_l[i] + m) * g;
            self.quantum_r[i] = (self.quantum_r[i] + m) * g;
        }

        self.frame_clock += Q as u64;
        self.publish();
    }

    /// Renders one channel: its voices split at the channel's note events (sample-accurate),
    /// then gain and pan, mixed into the quantum. Idle channels cost one scan of the events.
    fn render_channel(&mut self, slot: usize) {
        let touches = |e: &&ScheduledEvent| match e.kind {
            EventKind::Wrap => true,
            k => k.slot() == Some(slot as u16),
        };
        let events = self.scheduled.as_slice();
        let ch = &mut self.channels[slot];
        if !ch.is_active() && !events.iter().any(|e| touches(&e)) {
            if self.scope_slot == Some(slot) {
                self.scratch_l.fill(0.0);
                self.write_scope();
            }
            return;
        }
        let (sl, sr) = (&mut self.scratch_l, &mut self.scratch_r);
        sl.fill(0.0);
        sr.fill(0.0);
        let mut cursor = 0;
        for e in events.iter().filter(touches) {
            let at = (e.offset as usize).min(Q);
            ch.render_voices(&mut sl[cursor..at], &mut sr[cursor..at]);
            cursor = at;
            match e.kind {
                EventKind::NoteOn { key, velocity, .. } => {
                    self.voice_age += 1;
                    ch.note_on(key, velocity, self.voice_age);
                }
                EventKind::NoteOff { key, .. } => ch.note_off(key),
                EventKind::Wrap => ch.release_all(),
                EventKind::Beat { .. } => {}
            }
        }
        ch.render_voices(&mut sl[cursor..], &mut sr[cursor..]);
        let peak = ch.mix_into((sl, sr), (&mut self.quantum_l, &mut self.quantum_r));
        self.telemetry.channel_peaks[slot].fetch_max(peak, Ordering::Relaxed);
        if self.scope_slot == Some(slot) {
            for (l, r) in self.scratch_l.iter_mut().zip(&self.scratch_r) {
                *l = 0.5 * (*l + r);
            }
            self.write_scope();
        }
    }

    /// Appends `scratch_l` to the telemetry oscilloscope ring.
    fn write_scope(&self) {
        let t = &*self.telemetry;
        let start = t.scope_write.load(Ordering::Relaxed);
        for (i, &x) in self.scratch_l.iter().enumerate() {
            t.scope[((start + i as u64) % SCOPE_LEN as u64) as usize].store(x, Ordering::Relaxed);
        }
        t.scope_write
            .store(start + self.scratch_l.len() as u64, Ordering::Relaxed);
    }

    fn apply_commands(&mut self) {
        for _ in 0..MAX_COMMANDS_PER_QUANTUM {
            // Commands that hand something back are only taken if the garbage queue has room
            // for it, so the audio thread never has to free anything. Otherwise retry next
            // quantum.
            let needs_gc = self
                .commands
                .peek()
                .is_ok_and(EngineCommand::returns_garbage);
            if needs_gc && self.garbage.slots() == 0 {
                break;
            }
            let Ok(cmd) = self.commands.pop() else {
                break;
            };
            self.apply(cmd);
        }
    }

    fn release_all_channels(&mut self) {
        for ch in &mut self.channels {
            ch.release_all();
        }
    }

    fn apply(&mut self, cmd: EngineCommand) {
        let now = self.frame_clock;
        let before = self.transport.state();
        match cmd {
            EngineCommand::Play => self.transport.play(now),
            EngineCommand::Pause => self.transport.pause(now),
            EngineCommand::Stop => self.transport.stop(),
            EngineCommand::Locate(t) => {
                self.transport.locate(t, now);
                self.release_all_channels();
            }
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
            EngineCommand::SetSong(song) => {
                // Note-offs of the old pattern may never come; release what it started.
                self.release_all_channels();
                if let Some(old) = self.song.replace(song) {
                    self.retire(Garbage::Song(old));
                }
            }
            EngineCommand::SetChannelParams { slot, params } => {
                if let Some(ch) = self.channels.get_mut(usize::from(slot)) {
                    if ch.is_active() {
                        ch.set_params(*params);
                    } else {
                        ch.set_params_now(*params);
                    }
                }
                self.retire(Garbage::Params(params));
            }
            EngineCommand::SetScopeChannel(slot) => self.scope_slot = slot.map(usize::from),
            EngineCommand::SetChannelSample { slot, sample } => {
                let old = match self.channels.get_mut(usize::from(slot)) {
                    Some(ch) => ch.set_sample(sample),
                    None => sample,
                };
                if let Some(old) = old {
                    self.retire(Garbage::Sample(old));
                }
            }
            EngineCommand::NoteOn {
                slot,
                key,
                velocity,
            } => {
                self.voice_age += 1;
                if let Some(ch) = self.channels.get_mut(usize::from(slot)) {
                    ch.note_on(key, velocity, self.voice_age);
                }
            }
            EngineCommand::NoteOff { slot, key } => {
                if let Some(ch) = self.channels.get_mut(usize::from(slot)) {
                    ch.note_off(key);
                }
            }
            EngineCommand::PreviewSample(sample) => {
                let start = sample.is_some();
                if let Some(old) = self.preview.set_sample(sample) {
                    self.retire(Garbage::Sample(old));
                }
                if start {
                    self.voice_age += 1;
                    self.preview.note_on(ROOT_KEY, 1.0, self.voice_age);
                }
            }
        }
        let after = self.transport.state();
        if before == TransportState::Playing && after != TransportState::Playing {
            self.release_all_channels();
        }
        if before != after {
            self.post(EngineEvent::TransportChanged {
                state: after,
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

    fn impulse_sample(sr: u32) -> std::sync::Arc<gt_core::SampleData> {
        let mut v = vec![0.0; 4800];
        v[0] = 1.0;
        std::sync::Arc::new(gt_core::SampleData::mono(sr, v))
    }

    fn full_params() -> Box<crate::ChannelParams> {
        Box::new(crate::ChannelParams {
            gain: 1.0,
            ..crate::ChannelParams::default()
        })
    }

    #[test]
    fn synth_channel_plays_and_feeds_the_scope() {
        let (mut h, mut p) = engine(48_000, 2);
        h.send(EngineCommand::SetChannelParams {
            slot: 5,
            params: Box::new(crate::ChannelParams {
                kind: crate::InstrumentKind::Synth,
                gain: 1.0,
                ..crate::ChannelParams::default()
            }),
        })
        .unwrap();
        h.send(EngineCommand::SetScopeChannel(Some(5))).unwrap();
        h.send(EngineCommand::NoteOn {
            slot: 5,
            key: 57,
            velocity: 1.0,
        })
        .unwrap();
        let mut buf = vec![0.0; 2 * 4800];
        p.process(&mut buf);
        assert!(buf.iter().any(|s| s.abs() > 0.05));
        let mut scope = vec![0.0; 1024];
        h.telemetry().read_scope(&mut scope);
        assert!(scope.iter().any(|s| s.abs() > 0.05));
        assert!(h.telemetry().scope_write.load(Ordering::Relaxed) >= 4800);
    }

    #[test]
    fn audition_note_plays_the_sample_panned() {
        let (mut h, mut p) = engine(48_000, 2);
        h.send(EngineCommand::SetChannelSample {
            slot: 3,
            sample: Some(impulse_sample(48_000)),
        })
        .unwrap();
        h.send(EngineCommand::SetChannelParams {
            slot: 3,
            params: Box::new(crate::ChannelParams {
                gain: 1.0,
                pan: 1.0,
                ..crate::ChannelParams::default()
            }),
        })
        .unwrap();
        h.send(EngineCommand::NoteOn {
            slot: 3,
            key: 60,
            velocity: 1.0,
        })
        .unwrap();
        let mut buf = vec![0.0; 2 * 128];
        p.process(&mut buf);
        assert!(buf[0].abs() < 1e-6, "hard right: left is silent");
        assert!((buf[1] - 1.0).abs() < 1e-6, "{}", buf[1]);
        assert!(buf[2..].iter().all(|&s| s == 0.0));
        let peak = h.telemetry().channel_peaks[3].load(Ordering::Relaxed);
        assert!((peak - 1.0).abs() < 1e-6);
        assert_eq!(h.collect_garbage(), 1, "the params box comes back");
    }

    #[test]
    fn velocity_is_squared_and_mute_ramps_to_silence() {
        let (mut h, mut p) = engine(48_000, 1);
        let s = std::sync::Arc::new(gt_core::SampleData::mono(48_000, vec![1.0; 48_000]));
        h.send(EngineCommand::SetChannelSample {
            slot: 0,
            sample: Some(s),
        })
        .unwrap();
        h.send(EngineCommand::SetChannelParams {
            slot: 0,
            params: full_params(),
        })
        .unwrap();
        h.send(EngineCommand::NoteOn {
            slot: 0,
            key: 60,
            velocity: 0.5,
        })
        .unwrap();
        let mut buf = vec![0.0; 64];
        p.process(&mut buf);
        // Centre pan is -3 dB per side; a mono device averages the sides.
        let want = 0.25 * std::f32::consts::FRAC_1_SQRT_2;
        assert!((buf[10] - want).abs() < 1e-6, "{}", buf[10]);
        h.send(EngineCommand::SetChannelParams {
            slot: 0,
            params: Box::new(crate::ChannelParams::default()),
        })
        .unwrap();
        let mut buf = vec![0.0; 4800];
        p.process(&mut buf);
        let steps = buf
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(steps < 1e-3, "gain change is smoothed: {steps}");
        assert_eq!(buf[4799], 0.0);
    }

    #[test]
    fn preview_plays_and_replaced_samples_come_back() {
        let (mut h, mut p) = engine(48_000, 2);
        h.send(EngineCommand::PreviewSample(Some(impulse_sample(48_000))))
            .unwrap();
        let mut buf = vec![0.0; 2 * 64];
        p.process(&mut buf);
        let g = crate::PREVIEW_GAIN * std::f32::consts::FRAC_1_SQRT_2;
        assert!((buf[0] - g).abs() < 1e-6, "{}", buf[0]);
        h.send(EngineCommand::PreviewSample(None)).unwrap();
        p.process(&mut buf);
        assert_eq!(h.collect_garbage(), 1);
        // Out-of-range slots are ignored, and their payload still comes back.
        h.send(EngineCommand::SetChannelSample {
            slot: 999,
            sample: Some(impulse_sample(48_000)),
        })
        .unwrap();
        p.process(&mut buf);
        assert_eq!(h.collect_garbage(), 1);
    }

    #[test]
    fn stop_releases_looped_voices() {
        let (mut h, mut p) = engine(48_000, 1);
        let s = std::sync::Arc::new(gt_core::SampleData::mono(48_000, vec![1.0; 100]));
        h.send(EngineCommand::SetChannelSample {
            slot: 0,
            sample: Some(s),
        })
        .unwrap();
        h.send(EngineCommand::SetChannelParams {
            slot: 0,
            params: Box::new(crate::ChannelParams {
                gain: 1.0,
                looped: true,
                ..crate::ChannelParams::default()
            }),
        })
        .unwrap();
        h.send(EngineCommand::Play).unwrap();
        h.send(EngineCommand::NoteOn {
            slot: 0,
            key: 60,
            velocity: 1.0,
        })
        .unwrap();
        let mut buf = vec![0.0; 48_000];
        p.process(&mut buf);
        assert!(buf[47_999] > 0.5, "loop keeps sounding while held");
        h.send(EngineCommand::Stop).unwrap();
        p.process(&mut buf);
        assert_eq!(buf[47_999], 0.0, "released by stop");
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
