//! Hardware integration (Prompt 33, D148): MIDI out channels that play external synths, and
//! following an external MIDI clock.
//!
//! A [`MidiOutSettings`] channel makes no sound: its notes, controllers and program change go
//! to a MIDI output port, sent early by the device's own delay so they sound together with the
//! audio.
//!
//! [`ClockFollower`] turns the MIDI clock of another device (24 pulses per quarter note, with
//! start, continue, stop and song position) into a tempo and a transport. The tempo is the
//! slope of a least-squares line through the last [`ClockFollower::WINDOW`] pulse times, which
//! averages out the jitter of USB and the operating system (one pulse's timing error moves
//! the estimate by a few hundredths of a BPM), and follows tempo changes within a beat or two.
//! [`ClockFollower::tempo_for`] then nudges that tempo by up to [`MAX_NUDGE`] so the song
//! drifts back onto the external beat.

use std::collections::VecDeque;

use crate::PPQ;

/// Where a MIDI out channel sends.
#[derive(Debug, Clone, PartialEq)]
pub struct MidiOutSettings {
    /// Output port name ("" sends nowhere until one is picked).
    pub port: String,
    /// MIDI channel, 0 to 15.
    pub channel: u8,
    /// Program change sent when the song starts or the setting changes.
    pub program: Option<u8>,
    /// Bank select (CC 0 and 32, 14 bits) sent before the program change.
    pub bank: Option<u16>,
    /// How late the device sounds after it receives a note, in milliseconds: notes go out this
    /// much earlier.
    pub latency_ms: f32,
}

impl Default for MidiOutSettings {
    fn default() -> Self {
        Self {
            port: String::new(),
            channel: 0,
            program: None,
            bank: None,
            latency_ms: 0.0,
        }
    }
}

impl MidiOutSettings {
    /// Most latency compensation, in milliseconds.
    pub const MAX_LATENCY_MS: f32 = 500.0;

    /// Clamps every value into range.
    pub fn sanitize(&mut self) {
        self.channel = self.channel.min(15);
        self.program = self.program.map(|p| p.min(127));
        self.bank = self.bank.map(|b| b.min(16_383));
        self.latency_ms = if self.latency_ms.is_finite() {
            self.latency_ms.clamp(0.0, Self::MAX_LATENCY_MS)
        } else {
            0.0
        };
    }
}

/// MIDI Time Code frame rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MtcRate {
    /// 24 frames per second (film).
    Fps24,
    /// 25 frames per second (PAL).
    #[default]
    Fps25,
    /// 30 frames per second, non-drop.
    Fps30,
}

impl MtcRate {
    /// Frames per second.
    pub fn fps(self) -> u32 {
        match self {
            Self::Fps24 => 24,
            Self::Fps25 => 25,
            Self::Fps30 => 30,
        }
    }

    /// The rate code in the hours byte of MTC messages.
    pub fn code(self) -> u8 {
        match self {
            Self::Fps24 => 0,
            Self::Fps25 => 1,
            Self::Fps30 => 3,
        }
    }

    /// Every rate, for menus.
    pub const ALL: [Self; 3] = [Self::Fps24, Self::Fps25, Self::Fps30];
}

/// A system real-time or common message from an external clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockMessage {
    /// Timing clock (0xF8): one 24th of a quarter note.
    Pulse,
    /// Start (0xFA): play from the beginning at the next pulse.
    Start,
    /// Continue (0xFB): play from the song position.
    Continue,
    /// Stop (0xFC).
    Stop,
    /// Song position pointer (0xF2), in sixteenth notes.
    Position(u16),
}

impl ClockMessage {
    /// Decodes a clock message; anything else is `None`.
    pub fn decode(msg: &[u8]) -> Option<Self> {
        match msg {
            [0xF8, ..] => Some(Self::Pulse),
            [0xFA, ..] => Some(Self::Start),
            [0xFB, ..] => Some(Self::Continue),
            [0xFC, ..] => Some(Self::Stop),
            [0xF2, lsb, msb, ..] => Some(Self::Position(
                u16::from(msb & 0x7F) << 7 | u16::from(lsb & 0x7F),
            )),
            _ => None,
        }
    }
}

/// What the transport should do for an external clock message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SyncAction {
    /// Start playing from this tick.
    Play(i64),
    /// Stop, keeping the position.
    Stop,
    /// Move to this tick (only sent while stopped).
    Locate(i64),
}

/// Ticks per MIDI clock pulse.
pub const PULSE_TICKS: i64 = PPQ / 24;
/// Most the phase correction changes the tempo, as a fraction.
pub const MAX_NUDGE: f64 = 0.02;

/// Follows an external MIDI clock: tempo, transport and where the song should be.
#[derive(Debug, Clone, Default)]
pub struct ClockFollower {
    /// Arrival times of the last pulses, nanoseconds.
    times: VecDeque<u64>,
    /// Nanoseconds per pulse from the last fit, and the fitted time of the newest pulse.
    fit: Option<(f64, f64)>,
    /// Playing (after start or continue).
    running: bool,
    /// The song tick of the newest pulse while running (start, continue or position set it).
    tick: i64,
    /// Start or continue arrived: the next pulse is at `tick`, not one pulse after it.
    armed: bool,
}

impl ClockFollower {
    /// Pulses in the tempo fit (two beats).
    pub const WINDOW: usize = 48;
    /// A gap this many times the pulse length means the clock stopped: the fit starts over.
    const GAP: f64 = 4.0;

    /// A follower that has heard nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one message that arrived at `at_ns`. Returns what the transport should do.
    pub fn feed(&mut self, msg: ClockMessage, at_ns: u64) -> Option<SyncAction> {
        match msg {
            ClockMessage::Pulse => {
                if let (Some(&last), Some((ns, _))) = (self.times.back(), self.fit) {
                    if (at_ns.saturating_sub(last)) as f64 > Self::GAP * ns {
                        self.times.clear();
                        self.fit = None;
                    }
                }
                self.times.push_back(at_ns);
                while self.times.len() > Self::WINDOW {
                    self.times.pop_front();
                }
                self.refit();
                if self.running {
                    if self.armed {
                        self.armed = false;
                    } else {
                        self.tick += PULSE_TICKS;
                    }
                }
                None
            }
            ClockMessage::Start => {
                self.running = true;
                self.armed = true;
                self.tick = 0;
                Some(SyncAction::Play(0))
            }
            ClockMessage::Continue => {
                self.running = true;
                self.armed = true;
                Some(SyncAction::Play(self.tick))
            }
            ClockMessage::Stop => {
                self.running = false;
                Some(SyncAction::Stop)
            }
            ClockMessage::Position(sixteenths) => {
                self.tick = i64::from(sixteenths) * (PPQ / 4);
                (!self.running).then_some(SyncAction::Locate(self.tick))
            }
        }
    }

    fn refit(&mut self) {
        let n = self.times.len();
        if n < 3 {
            self.fit = None;
            return;
        }
        // Least squares of time against pulse index, relative to the first pulse.
        let t0 = self.times[0];
        let nf = n as f64;
        let mean_i = (nf - 1.0) / 2.0;
        let mean_t = self.times.iter().map(|&t| (t - t0) as f64).sum::<f64>() / nf;
        let (mut sxy, mut sxx) = (0.0, 0.0);
        for (i, &t) in self.times.iter().enumerate() {
            let di = i as f64 - mean_i;
            sxy += di * ((t - t0) as f64 - mean_t);
            sxx += di * di;
        }
        let slope = sxy / sxx;
        if slope > 0.0 {
            let newest = t0 as f64 + mean_t + slope * (nf - 1.0 - mean_i);
            self.fit = Some((slope, newest));
        }
    }

    /// The external tempo, once enough pulses arrived.
    pub fn bpm(&self) -> Option<f64> {
        self.fit.map(|(ns, _)| 60e9 / (ns * 24.0))
    }

    /// True between start (or continue) and stop.
    pub fn running(&self) -> bool {
        self.running
    }

    /// Where the song should be at `at_ns`, in ticks, while running.
    pub fn target_tick(&self, at_ns: u64) -> Option<f64> {
        let (ns, newest) = self.fit?;
        if !self.running || self.armed {
            return None;
        }
        Some(self.tick as f64 + (at_ns as f64 - newest) / ns * PULSE_TICKS as f64)
    }

    /// The tempo to play at, given that the song is at `song_tick` at `at_ns`: the external
    /// tempo, nudged (by up to [`MAX_NUDGE`]) so the song catches up with the external beat
    /// over about one beat.
    pub fn tempo_for(&self, song_tick: Option<f64>, at_ns: u64) -> Option<f64> {
        let bpm = self.bpm()?;
        let (Some(song), Some(target)) = (song_tick, self.target_tick(at_ns)) else {
            return Some(bpm);
        };
        let behind_beats = (target - song) / PPQ as f64;
        Some(bpm * (1.0 + behind_beats.clamp(-MAX_NUDGE, MAX_NUDGE)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed-seed generator for jitter, in -1..1.
    fn jitter(seed: &mut u32) -> f64 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(*seed >> 8) / f64::from(1u32 << 24) * 2.0 - 1.0
    }

    fn pulse_ns(bpm: f64) -> f64 {
        60e9 / (bpm * 24.0)
    }

    #[test]
    fn a_jittery_clock_gives_a_steady_tempo() {
        let mut f = ClockFollower::new();
        let mut seed = 3;
        let mut worst: f64 = 0.0;
        for i in 0..24 * 16 {
            // ±1 ms of jitter on every pulse at 120 BPM (pulses 20.8 ms apart).
            let t = 1e9 + i as f64 * pulse_ns(120.0) + 1e6 * jitter(&mut seed);
            f.feed(ClockMessage::Pulse, t as u64);
            if i >= ClockFollower::WINDOW as i32 {
                worst = worst.max((f.bpm().unwrap() - 120.0).abs());
            }
        }
        assert!(worst < 0.5, "tempo strays {worst:.3} BPM");
    }

    #[test]
    fn tempo_changes_are_followed_within_two_beats() {
        let mut f = ClockFollower::new();
        let mut t = 0.0;
        for _ in 0..96 {
            t += pulse_ns(100.0);
            f.feed(ClockMessage::Pulse, t as u64);
        }
        assert!((f.bpm().unwrap() - 100.0).abs() < 1e-6);
        for _ in 0..ClockFollower::WINDOW {
            t += pulse_ns(140.0);
            f.feed(ClockMessage::Pulse, t as u64);
        }
        assert!((f.bpm().unwrap() - 140.0).abs() < 1e-6);
        // A long silence starts the fit over instead of averaging across it.
        t += 5e9;
        for _ in 0..4 {
            t += pulse_ns(90.0);
            f.feed(ClockMessage::Pulse, t as u64);
        }
        assert!((f.bpm().unwrap() - 90.0).abs() < 1e-6);
    }

    #[test]
    fn start_continue_stop_and_position_drive_the_transport() {
        let mut f = ClockFollower::new();
        assert_eq!(
            ClockMessage::decode(&[0xF2, 0x10, 0x01]),
            Some(ClockMessage::Position(144))
        );
        assert_eq!(ClockMessage::decode(&[0x90, 60, 100]), None);
        // Position while stopped locates: 144 sixteenths are 36 beats.
        assert_eq!(
            f.feed(ClockMessage::Position(144), 0),
            Some(SyncAction::Locate(36 * PPQ))
        );
        assert_eq!(
            f.feed(ClockMessage::Continue, 0),
            Some(SyncAction::Play(36 * PPQ))
        );
        let p = pulse_ns(120.0);
        for i in 0..=24 {
            f.feed(ClockMessage::Pulse, (i as f64 * p) as u64);
        }
        // The first pulse after continue is the song position; 24 more are one beat.
        let at = (24.0 * p) as u64;
        assert!((f.target_tick(at).unwrap() - (37 * PPQ) as f64).abs() < 1e-6);
        assert_eq!(f.feed(ClockMessage::Stop, at), Some(SyncAction::Stop));
        assert_eq!(f.target_tick(at), None);
        assert_eq!(f.feed(ClockMessage::Start, at), Some(SyncAction::Play(0)));
    }

    #[test]
    fn the_phase_correction_pulls_a_late_song_onto_the_beat() {
        // A song that started 50 ms late follows the tempo the follower asks for.
        let mut f = ClockFollower::new();
        f.feed(ClockMessage::Start, 0);
        let p = pulse_ns(120.0);
        let mut song = -0.05 * 120.0 / 60.0 * PPQ as f64;
        let mut seed = 9;
        let step = 1e7; // the app asks every 10 ms
        let mut t = 0.0;
        let mut next_pulse = 0.0;
        for _ in 0..800 {
            while next_pulse <= t {
                f.feed(
                    ClockMessage::Pulse,
                    (next_pulse + 5e5 * jitter(&mut seed)) as u64,
                );
                next_pulse += p;
            }
            let bpm = f.tempo_for(Some(song), t as u64).unwrap_or(120.0);
            song += bpm / 60.0 * PPQ as f64 * step / 1e9;
            t += step;
        }
        let err = f.target_tick(t as u64).unwrap() - song;
        // Within a millisecond's worth of ticks after 8 s.
        assert!(err.abs() < 2.0, "{err:.2} ticks off");
    }

    #[test]
    fn settings_are_clamped() {
        let mut s = MidiOutSettings {
            port: "x".into(),
            channel: 20,
            program: Some(200),
            bank: Some(60_000),
            latency_ms: f32::NAN,
        };
        s.sanitize();
        assert_eq!(
            (s.channel, s.program, s.bank, s.latency_ms),
            (15, Some(127), Some(16_383), 0.0)
        );
    }
}
