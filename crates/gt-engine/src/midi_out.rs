//! MIDI out (Prompt 33, D148): MIDI out channels, MIDI clock and MIDI Time Code, on their way
//! from the audio thread to the ports.
//!
//! The audio thread never touches a MIDI port. It stamps every message with the
//! [`crate::clock_ns`] time it should leave so that it sounds with the audio rendered at the
//! same frame (the frame's place in the callback, plus the device's output latency and the
//! mixer's delay compensation, minus the receiving device's own latency), and pushes it into a
//! wait-free queue. A thread in the app sends each one when its time comes, to the port of its
//! channel (by slot) or to the ports picked for clock and time code ([`DEST_CLOCK`],
//! [`DEST_MTC`]).
//!
//! Clock: 24 pulses per quarter note at the exact song positions (tempo ramps included), start
//! from the song start, song position plus continue anywhere else, and stop, position and
//! continue around loop wraps and jumps. Time code: quarter-frame messages, four per frame, and
//! a full-frame message after every jump.

use gt_core::{MtcRate, PPQ};
use rtrb::Producer;

use crate::transport::PlaySegment;
use crate::LiveKind;

/// Capacity of the queue to the MIDI out thread.
pub const MIDI_OUT_CAPACITY: usize = 4096;
/// Destination of clock messages: the ports picked for clock out.
pub const DEST_CLOCK: u16 = u16::MAX;
/// Destination of time code messages: the ports picked for time code out.
pub const DEST_MTC: u16 = u16::MAX - 1;

/// One message for a MIDI port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiOutEvent {
    /// When to send it, on the [`crate::clock_ns`] clock.
    pub at_ns: u64,
    /// The channel slot whose port it goes to, or [`DEST_CLOCK`] / [`DEST_MTC`].
    pub dest: u16,
    len: u8,
    bytes: [u8; 10],
}

impl MidiOutEvent {
    /// A message of up to 10 bytes (longer ones are cut).
    pub fn new(at_ns: u64, dest: u16, msg: &[u8]) -> Self {
        let mut bytes = [0; 10];
        let len = msg.len().min(10);
        bytes[..len].copy_from_slice(&msg[..len]);
        Self {
            at_ns,
            dest,
            len: len as u8,
            bytes,
        }
    }

    /// The message bytes.
    pub fn message(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// What the engine sends besides channel messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MidiSyncSettings {
    /// MIDI clock with start, stop, continue and song position.
    pub clock: bool,
    /// MIDI Time Code at this rate.
    pub mtc: Option<MtcRate>,
}

/// A MIDI out channel's settings as the engine uses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MidiOutParams {
    /// MIDI channel, 0 to 15.
    pub channel: u8,
    /// Program change.
    pub program: Option<u8>,
    /// Bank select before it.
    pub bank: Option<u16>,
    /// Sent this much early, for the device's own latency.
    pub lead_ns: u64,
}

/// Maps a frame of the quantum being rendered to the time its sound leaves the speakers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameClock {
    /// The callback's start, and the engine frame it outputs first.
    pub cb_ns: u64,
    pub cb_out: u64,
    /// The quantum's first frame.
    pub frame: u64,
    pub sample_rate: f64,
    /// Frames from the callback to the speakers plus the mixer's delay compensation.
    pub delay: u64,
}

impl FrameClock {
    /// When a message for frame `offset` should leave, `lead_ns` early.
    pub(crate) fn at(&self, offset: u32, lead_ns: u64) -> u64 {
        let frames = (self.frame + u64::from(offset) + self.delay) as f64 - self.cb_out as f64;
        let ns = self.cb_ns as f64 + frames * 1e9 / self.sample_rate - lead_ns as f64;
        ns.max(0.0) as u64
    }
}

/// MIDI channel, bank and program as last sent.
type Program = (u8, Option<u16>, u8);

/// The audio thread's MIDI out state. Allocated once.
pub(crate) struct MidiOutEngine {
    tx: Producer<MidiOutEvent>,
    pub(crate) sync: MidiSyncSettings,
    sync_out: SyncOut,
    /// Per slot: keys sounding on the device (128 bits).
    held: Box<[u128]>,
    /// Per slot: the (channel, bank, program) last sent.
    sent: Box<[Option<Program>]>,
    /// Messages the queue had no room for.
    pub(crate) dropped: u32,
}

impl MidiOutEngine {
    pub(crate) fn new(tx: Producer<MidiOutEvent>) -> Self {
        Self {
            tx,
            sync: MidiSyncSettings::default(),
            sync_out: SyncOut::default(),
            held: vec![0; gt_core::MAX_CHANNELS].into_boxed_slice(),
            sent: vec![None; gt_core::MAX_CHANNELS].into_boxed_slice(),
            dropped: 0,
        }
    }

    fn push(&mut self, at_ns: u64, dest: u16, msg: &[u8]) {
        if self.tx.push(MidiOutEvent::new(at_ns, dest, msg)).is_err() {
            self.dropped += 1;
        }
    }

    /// Bank and program, when they changed since they were last sent.
    pub(crate) fn program(&mut self, slot: usize, p: &MidiOutParams, at_ns: u64) {
        let Some(program) = p.program else {
            return;
        };
        let want = Some((p.channel, p.bank, program));
        if self.sent.get(slot) == Some(&want) || slot >= self.sent.len() {
            return;
        }
        self.sent[slot] = want;
        let c = p.channel & 0x0F;
        if let Some(b) = p.bank {
            self.push(at_ns, slot as u16, &[0xB0 | c, 0, (b >> 7) as u8 & 0x7F]);
            self.push(at_ns, slot as u16, &[0xB0 | c, 32, b as u8 & 0x7F]);
        }
        self.push(at_ns, slot as u16, &[0xC0 | c, program & 0x7F]);
    }

    /// Sends every program again (the song starts).
    pub(crate) fn resend_programs(&mut self) {
        self.sent.fill(None);
    }

    pub(crate) fn note_on(&mut self, slot: usize, p: &MidiOutParams, at: u64, key: u8, v: f32) {
        let Some(held) = self.held.get(slot).copied() else {
            return;
        };
        let (c, key) = (p.channel & 0x0F, key & 0x7F);
        if held >> key & 1 == 1 {
            self.push(at, slot as u16, &[0x80 | c, key, 0]);
        }
        let vel = (v.clamp(0.0, 1.0) * 127.0).round().max(1.0) as u8;
        self.push(at, slot as u16, &[0x90 | c, key, vel]);
        self.held[slot] = held | 1 << key;
    }

    pub(crate) fn note_off(&mut self, slot: usize, p: &MidiOutParams, at: u64, key: u8) {
        let key = key & 0x7F;
        if self.held.get(slot).is_some_and(|h| h >> key & 1 == 1) {
            self.push(at, slot as u16, &[0x80 | (p.channel & 0x0F), key, 0]);
            self.held[slot] &= !(1 << key);
        }
    }

    /// Releases every key the slot holds on its device.
    pub(crate) fn all_off(&mut self, slot: usize, p: &MidiOutParams, at: u64) {
        let Some(&held) = self.held.get(slot) else {
            return;
        };
        for key in 0..128u8 {
            if held >> key & 1 == 1 {
                self.push(at, slot as u16, &[0x80 | (p.channel & 0x0F), key, 0]);
            }
        }
        self.held[slot] = 0;
    }

    /// True while the slot holds a key on its device.
    pub(crate) fn holds(&self, slot: usize) -> bool {
        self.held.get(slot).is_some_and(|&h| h != 0)
    }

    /// Bend, a controller or aftertouch.
    pub(crate) fn expression(&mut self, slot: usize, p: &MidiOutParams, at: u64, kind: LiveKind) {
        let c = p.channel & 0x0F;
        let seven = |v: f32| (v.clamp(0.0, 1.0) * 127.0).round() as u8;
        let (msg, n): ([u8; 3], usize) = match kind {
            LiveKind::Bend(b) => {
                let v = ((b.clamp(-1.0, 1.0) + 1.0) * 8192.0).round().min(16383.0) as u16;
                ([0xE0 | c, (v & 0x7F) as u8, (v >> 7) as u8], 3)
            }
            LiveKind::Sustain(down) => ([0xB0 | c, 64, if down { 127 } else { 0 }], 3),
            LiveKind::ModWheel(v) => ([0xB0 | c, 1, seven(v)], 3),
            LiveKind::Pressure(v) => ([0xD0 | c, seven(v), 0], 2),
            LiveKind::Cc { cc, value } => ([0xB0 | c, cc & 0x7F, seven(value)], 3),
            LiveKind::KeyPressure { key, value } => ([0xA0 | c, key & 0x7F, seven(value)], 3),
            LiveKind::Note(_) | LiveKind::NoteExpr { .. } => return,
        };
        self.push(at, slot as u16, &msg[..n]);
    }

    /// Clock and time code for one quantum.
    pub(crate) fn run_sync(
        &mut self,
        playing: bool,
        segments: &[PlaySegment],
        sample_rate: f64,
        tempo: &impl SongTime,
        clock: &FrameClock,
    ) {
        let sync = self.sync;
        let mut out = std::mem::take(&mut self.sync_out);
        out.run(
            sync,
            playing,
            segments,
            sample_rate,
            tempo,
            &mut |off, dest, msg| {
                self.push(clock.at(off, 0), dest, msg);
            },
        );
        self.sync_out = out;
    }
}

/// Converts between song seconds and ticks (the transport's tempo map).
pub(crate) trait SongTime {
    fn tick_at_seconds(&self, s: f64) -> f64;
    fn seconds_at_tick(&self, t: f64) -> f64;
}

impl SongTime for crate::transport::Transport {
    fn tick_at_seconds(&self, s: f64) -> f64 {
        crate::transport::Transport::tick_at_seconds(self, s)
    }
    fn seconds_at_tick(&self, t: f64) -> f64 {
        crate::transport::Transport::seconds_at_tick(self, t)
    }
}

/// Ticks per clock pulse.
const PULSE: f64 = (PPQ / 24) as f64;

/// Clock and time code generation, carried between quanta.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SyncOut {
    playing: bool,
    /// Song time the next quantum continues from (anything else is a jump).
    resume_s: f64,
    /// The next clock pulse and quarter frame to send, by number (so rounding at quantum
    /// edges never sends one twice or skips one).
    next_pulse: f64,
    next_qf: f64,
}

impl SyncOut {
    /// Emits `(offset, destination, message)` for one quantum.
    pub(crate) fn run(
        &mut self,
        sync: MidiSyncSettings,
        playing: bool,
        segments: &[PlaySegment],
        sr: f64,
        tempo: &impl SongTime,
        emit: &mut impl FnMut(u32, u16, &[u8]),
    ) {
        if !playing {
            if self.playing && sync.clock {
                emit(0, DEST_CLOCK, &[0xFC]);
            }
            self.playing = false;
            return;
        }
        for (i, seg) in segments.iter().enumerate() {
            let s0 = seg.seconds;
            let s1 = s0 + f64::from(seg.frames) / sr;
            let jump = i > 0 || !self.playing || (s0 - self.resume_s).abs() > 1.5 / sr;
            let frame_of = |s: f64| {
                let f = ((s - s0) * sr).round().max(0.0) as u32;
                seg.offset + f.min(seg.frames.saturating_sub(1))
            };
            if sync.clock {
                if jump {
                    if self.playing {
                        emit(seg.offset, DEST_CLOCK, &[0xFC]);
                    }
                    let tick = tempo.tick_at_seconds(s0).max(0.0);
                    if tick < 0.5 {
                        emit(seg.offset, DEST_CLOCK, &[0xFA]);
                    } else {
                        let sixteenths = (tick / (PPQ / 4) as f64).round().min(16_383.0) as u16;
                        emit(
                            seg.offset,
                            DEST_CLOCK,
                            &[0xF2, (sixteenths & 0x7F) as u8, (sixteenths >> 7) as u8],
                        );
                        emit(seg.offset, DEST_CLOCK, &[0xFB]);
                    }
                }
                if jump {
                    self.next_pulse = (tempo.tick_at_seconds(s0) / PULSE - 1e-9).ceil();
                }
                let t1 = tempo.tick_at_seconds(s1);
                while self.next_pulse * PULSE < t1 - 1e-9 {
                    let s = tempo.seconds_at_tick(self.next_pulse * PULSE);
                    emit(frame_of(s), DEST_CLOCK, &[0xF8]);
                    self.next_pulse += 1.0;
                }
            }
            if let Some(rate) = sync.mtc {
                if jump {
                    emit(seg.offset, DEST_MTC, &full_frame(rate, s0));
                }
                let q = f64::from(4 * rate.fps());
                if jump {
                    self.next_qf = (s0.max(0.0) * q - 1e-6).ceil();
                }
                while self.next_qf / q < s1 - 1e-9 {
                    let n = self.next_qf;
                    emit(frame_of(n / q), DEST_MTC, &quarter_frame(rate, n as u64));
                    self.next_qf += 1.0;
                }
            }
            self.playing = true;
            self.resume_s = s1;
        }
        // Playing with no stretch of song time (should not happen): nothing to send.
        self.playing = true;
    }
}

/// Hours, minutes, seconds and frames of frame number `f`.
fn hmsf(rate: MtcRate, f: u64) -> [u8; 4] {
    let fps = u64::from(rate.fps());
    let s = f / fps;
    [
        ((s / 3600) % 24) as u8,
        ((s / 60) % 60) as u8,
        (s % 60) as u8,
        (f % fps) as u8,
    ]
}

/// The full-frame time code message for song time `seconds`.
pub(crate) fn full_frame(rate: MtcRate, seconds: f64) -> [u8; 10] {
    let f = (seconds.max(0.0) * f64::from(rate.fps()) + 1e-6).floor() as u64;
    let [h, m, s, fr] = hmsf(rate, f);
    [
        0xF0,
        0x7F,
        0x7F,
        0x01,
        0x01,
        rate.code() << 5 | h,
        m,
        s,
        fr,
        0xF7,
    ]
}

/// Quarter-frame message number `n` (four per frame): piece `n % 8` of the time of the frame
/// where its group of eight started.
pub(crate) fn quarter_frame(rate: MtcRate, n: u64) -> [u8; 2] {
    let piece = (n % 8) as u8;
    let [h, m, s, f] = hmsf(rate, n / 8 * 2);
    let nibble = match piece {
        0 => f & 0x0F,
        1 => f >> 4,
        2 => s & 0x0F,
        3 => s >> 4,
        4 => m & 0x0F,
        5 => m >> 4,
        6 => h & 0x0F,
        _ => h >> 4 | rate.code() << 1,
    };
    [0xF1, piece << 4 | nibble]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A constant tempo.
    struct Fixed(f64);
    impl SongTime for Fixed {
        fn tick_at_seconds(&self, s: f64) -> f64 {
            s * self.0 / 60.0 * PPQ as f64
        }
        fn seconds_at_tick(&self, t: f64) -> f64 {
            t / PPQ as f64 * 60.0 / self.0
        }
    }

    /// Runs `quanta` quanta of 32 frames at 48 kHz from song time `from`, collecting every
    /// message with its absolute frame.
    fn render(
        sync: MidiSyncSettings,
        out: &mut SyncOut,
        from: f64,
        quanta: u64,
        frame0: u64,
    ) -> Vec<(u64, u16, Vec<u8>)> {
        let sr = 48_000.0;
        let mut got = Vec::new();
        for q in 0..quanta {
            let seg = PlaySegment {
                offset: 0,
                frames: 32,
                seconds: from + q as f64 * 32.0 / sr,
            };
            out.run(sync, true, &[seg], sr, &Fixed(120.0), &mut |o, d, m| {
                got.push((frame0 + q * 32 + u64::from(o), d, m.to_vec()));
            });
        }
        got
    }

    #[test]
    fn clock_pulses_land_24_per_beat_after_start() {
        let sync = MidiSyncSettings {
            clock: true,
            mtc: None,
        };
        let mut out = SyncOut::default();
        // 2 seconds at 120 BPM: four beats.
        let got = render(sync, &mut out, 0.0, 3000, 0);
        assert_eq!(got[0], (0, DEST_CLOCK, vec![0xFA]));
        let pulses: Vec<u64> = got
            .iter()
            .filter(|(_, _, m)| m == &[0xF8])
            .map(|e| e.0)
            .collect();
        assert_eq!(pulses.len(), 96);
        // 1000 frames apart at 48 kHz and 120 BPM, with no jitter at all.
        for w in pulses.windows(2) {
            assert_eq!(w[1] - w[0], 1000);
        }
        // Stopping sends stop; playing again from beat 9 sends its position and continue.
        let mut stop = Vec::new();
        out.run(sync, false, &[], 48_000.0, &Fixed(120.0), &mut |o, _, m| {
            stop.push((o, m.to_vec()));
        });
        assert_eq!(stop, vec![(0, vec![0xFC])]);
        let again = render(sync, &mut out, 4.0, 1, 0);
        // Beat 9 is the 32nd sixteenth.
        assert_eq!(again[0].2, vec![0xF2, 32, 0]);
        assert_eq!(again[1].2, vec![0xFB]);
    }

    #[test]
    fn a_jump_sends_stop_position_and_continue() {
        let sync = MidiSyncSettings {
            clock: true,
            mtc: None,
        };
        let mut out = SyncOut::default();
        render(sync, &mut out, 0.0, 10, 0);
        let sr = 48_000.0;
        let segs = [
            PlaySegment {
                offset: 0,
                frames: 16,
                seconds: 10.0 * 32.0 / sr,
            },
            PlaySegment {
                offset: 16,
                frames: 16,
                seconds: 1.0,
            },
        ];
        let mut got = Vec::new();
        out.run(sync, true, &segs, sr, &Fixed(120.0), &mut |o, _, m| {
            got.push((o, m.to_vec()));
        });
        // The wrap in the middle of the quantum: stop, position (beat 3 = sixteenth 8), go.
        assert_eq!(
            got,
            vec![
                (16, vec![0xFC]),
                (16, vec![0xF2, 8, 0]),
                (16, vec![0xFB]),
                (16, vec![0xF8]),
            ]
        );
    }

    #[test]
    fn time_code_counts_quarter_frames_and_frames() {
        let sync = MidiSyncSettings {
            clock: false,
            mtc: Some(MtcRate::Fps25),
        };
        let mut out = SyncOut::default();
        // From 1 h 2 min 3 s + 4 frames.
        let from = 3723.0 + 4.0 / 25.0;
        let got = render(sync, &mut out, from, 1500, 0);
        assert_eq!(
            got[0].2,
            vec![0xF0, 0x7F, 0x7F, 0x01, 0x01, 1 << 5 | 1, 2, 3, 4, 0xF7]
        );
        let qf: Vec<&(u64, u16, Vec<u8>)> = got.iter().skip(1).collect();
        // One second: 100 quarter frames, 480 frames apart.
        assert_eq!(qf.len(), 100);
        assert!(qf.windows(2).all(|w| w[1].0 - w[0].0 == 480));
        // The first full group of eight spells the time of its first frame.
        let start = qf.iter().position(|e| e.2[1] >> 4 == 0).unwrap();
        let nibbles: Vec<u8> = qf[start..start + 8].iter().map(|e| e.2[1] & 0x0F).collect();
        let frames = nibbles[0] | nibbles[1] << 4;
        let secs = nibbles[2] | nibbles[3] << 4;
        let mins = nibbles[4] | nibbles[5] << 4;
        let hours = nibbles[6] | (nibbles[7] & 1) << 4;
        assert_eq!((hours, mins, secs), (1, 2, 3));
        assert!(frames < 25);
        assert_eq!(nibbles[7] >> 1, 1, "rate code 25 fps");
    }

    #[test]
    fn frames_map_to_send_times_with_latency_and_lead() {
        let c = FrameClock {
            cb_ns: 1_000_000_000,
            cb_out: 480,
            frame: 512,
            sample_rate: 48_000.0,
            delay: 448,
        };
        // Frame 512 + 16 is 48 frames after the callback's first, plus 448 of delay: 496
        // frames = 10.333 ms; 5 ms early.
        let at = c.at(16, 5_000_000);
        assert_eq!(at, 1_000_000_000 + 10_333_333 - 5_000_000);
    }
}
