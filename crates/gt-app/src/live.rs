//! Playing and recording live: the computer keyboard as a piano, turning live notes and
//! controllers into pattern data, step recording and capturing what was just played.

use std::collections::BTreeMap;

use egui::Key;
use gt_core::{ControlKind, ControlPoint, Note};
use gt_engine::{LiveEvent, LiveKind, LiveNote};

/// Typing-keyboard layout: two rows, as on a piano. Semitones above the base C. The bottom
/// row starts at the base C, the top row an octave higher.
const PIANO_KEYS: &[(Key, i32)] = &[
    (Key::Z, 0),
    (Key::S, 1),
    (Key::X, 2),
    (Key::D, 3),
    (Key::C, 4),
    (Key::V, 5),
    (Key::G, 6),
    (Key::B, 7),
    (Key::H, 8),
    (Key::N, 9),
    (Key::J, 10),
    (Key::M, 11),
    (Key::Comma, 12),
    (Key::L, 13),
    (Key::Period, 14),
    (Key::Semicolon, 15),
    (Key::Slash, 16),
    (Key::Q, 12),
    (Key::Num2, 13),
    (Key::W, 14),
    (Key::Num3, 15),
    (Key::E, 16),
    (Key::R, 17),
    (Key::Num5, 18),
    (Key::T, 19),
    (Key::Num6, 20),
    (Key::Y, 21),
    (Key::Num7, 22),
    (Key::U, 23),
    (Key::I, 24),
    (Key::Num9, 25),
    (Key::O, 26),
    (Key::Num0, 27),
    (Key::P, 28),
];

/// Velocity of typing-keyboard notes (100 of 127).
const TYPING_VELOCITY: f32 = 100.0 / 127.0;

/// The computer keyboard as a piano.
#[derive(Debug, Clone)]
pub struct TypingKeyboard {
    /// Piano mode on: the keys above play notes instead of their usual shortcuts.
    pub on: bool,
    /// Octave of the bottom row's C, as in "C3" (MIDI key 48).
    pub octave: i32,
    held: Vec<(Key, u8)>,
}

impl Default for TypingKeyboard {
    fn default() -> Self {
        Self {
            on: false,
            octave: 3,
            held: Vec::new(),
        }
    }
}

impl TypingKeyboard {
    fn base(&self) -> i32 {
        12 * (self.octave + 1)
    }

    /// Takes this frame's piano keys out of egui's input (so views do not also see them as
    /// shortcuts) and returns the notes they play. `allowed` is false while a text field or
    /// dialog has the keyboard; held keys are then released. Minus and Equals move the octave.
    pub fn handle(&mut self, ctx: &egui::Context, allowed: bool) -> Vec<LiveNote> {
        if !self.on || !allowed {
            return self.release_all();
        }
        let mut out = Vec::new();
        let focused = ctx.input_mut(|i| {
            let mut keep = Vec::with_capacity(i.events.len());
            for e in std::mem::take(&mut i.events) {
                let egui::Event::Key {
                    key,
                    pressed,
                    repeat,
                    modifiers,
                    ..
                } = e
                else {
                    keep.push(e);
                    continue;
                };
                // Ctrl/Alt shortcuts keep working in piano mode.
                if modifiers.command || modifiers.alt || modifiers.ctrl {
                    keep.push(e);
                    continue;
                }
                if matches!(key, Key::Minus | Key::Equals) {
                    if pressed && !repeat {
                        let d = if key == Key::Minus { -1 } else { 1 };
                        self.octave = (self.octave + d).clamp(-1, 7);
                    }
                    continue;
                }
                let Some(&(_, semi)) = PIANO_KEYS.iter().find(|(k, _)| *k == key) else {
                    keep.push(e);
                    continue;
                };
                if repeat {
                    continue;
                }
                if pressed {
                    let note = self.base() + semi;
                    if (0..128).contains(&note) && !self.held.iter().any(|(k, _)| *k == key) {
                        self.held.push((key, note as u8));
                        out.push(LiveNote::on(note as u8, TYPING_VELOCITY));
                    }
                } else if let Some(i) = self.held.iter().position(|(k, _)| *k == key) {
                    let (_, note) = self.held.remove(i);
                    out.push(LiveNote::off(note));
                }
            }
            // Text typed with these keys must not reach widgets either.
            keep.retain(|e| !matches!(e, egui::Event::Text(_)));
            i.events = keep;
            i.focused
        });
        if !focused {
            // Key releases are lost while the window is in the background.
            out.extend(self.release_all());
        }
        out
    }

    /// Releases every held key.
    pub fn release_all(&mut self) -> Vec<LiveNote> {
        self.held.drain(..).map(|(_, n)| LiveNote::off(n)).collect()
    }
}

/// Builds pattern notes from live notes during a take, for every channel slot played.
#[derive(Debug, Clone, Default)]
pub struct Recorder {
    /// Recording is on: live notes while playing go into the pattern.
    pub armed: bool,
    /// Keys held, by slot and key, with their start in pattern ticks and velocity.
    open: BTreeMap<(u16, u8), (i64, f32)>,
}

impl Recorder {
    /// A key of channel `slot` went down (`velocity > 0`) or up at pattern tick `at` (none:
    /// outside the pattern, e.g. between its clips in song mode). Returns the note a key-up
    /// completes. Notes are cut at the pattern end `len`; one held across the loop point
    /// wraps.
    pub fn note(
        &mut self,
        slot: u16,
        key: u8,
        velocity: f32,
        at: Option<i64>,
        len: i64,
    ) -> Option<(u16, Note)> {
        let k = (slot, key.min(127));
        let len = len.max(1);
        // A key struck again while held ends the earlier note.
        let done = self.close(k, at, len);
        if velocity > 0.0 {
            if let Some(t) = at {
                self.open.insert(k, (t.rem_euclid(len), velocity));
            }
        }
        done
    }

    fn close(&mut self, k: (u16, u8), at: Option<i64>, len: i64) -> Option<(u16, Note)> {
        let (start, velocity) = self.open.remove(&k)?;
        let length = match at {
            Some(t) => (t.rem_euclid(len) - start).rem_euclid(len),
            None => len - start,
        };
        Some((
            k.0,
            Note {
                start,
                length: length.clamp(1, len - start),
                key: k.1,
                velocity,
            },
        ))
    }

    /// Ends the take (or a stacked pass): completes every held note at `at` (none: at the
    /// pattern end).
    pub fn finish(&mut self, at: Option<i64>, len: i64) -> Vec<(u16, Note)> {
        let keys: Vec<_> = self.open.keys().copied().collect();
        keys.into_iter()
            .filter_map(|k| self.close(k, at, len.max(1)))
            .collect()
    }
}

/// The pattern ranges `[lo, hi)` the playhead passed going from `prev` to `now` in a pattern
/// `len` ticks long (two when it came round), and whether it came round.
pub fn swept(prev: i64, now: i64, len: i64) -> (Vec<(i64, i64)>, bool) {
    if now >= prev {
        (vec![(prev, now)], false)
    } else {
        (vec![(prev, len.max(prev)), (0, now)], true)
    }
}

/// Recorded controller points closer than this (about 5 ms at 120 BPM) to the previous one
/// of their kind replace it, so a wheel moving fast does not leave thousands of points.
pub const THIN_TICKS: i64 = 10;

/// Adds a recorded controller value to a channel's list, kept sorted. A value equal to the
/// one already in force is left out; a continuous controller's point within [`THIN_TICKS`]
/// after the previous one of its kind replaces it. True when the list changed.
pub fn add_control(list: &mut Vec<ControlPoint>, p: ControlPoint) -> bool {
    let at = list.partition_point(|c| c.tick <= p.tick);
    let prev = list[..at].iter().rposition(|c| c.kind == p.kind);
    if let Some(i) = prev {
        let q = list[i];
        if q.value == p.value {
            return false;
        }
        let close = p.tick - q.tick <= THIN_TICKS && p.kind != ControlKind::Sustain;
        if q.tick == p.tick || close {
            // The thinned point keeps its place, or moves no further than the next point.
            list[i].value = p.value;
            return true;
        }
    } else if p.value == p.kind.min().max(0.0) {
        // The resting value, with nothing earlier to undo.
        return false;
    }
    list.insert(at, p);
    true
}

/// Step recording: with the transport stopped, each note (or chord) played goes in at the
/// cursor, which moves on by one step when every key is up again.
#[derive(Debug, Clone, Default)]
pub struct StepRecorder {
    /// Step recording is on.
    pub on: bool,
    /// Where the next note goes, in pattern ticks.
    pub cursor: i64,
    /// Keys down, by slot and key.
    held: Vec<(u16, u8)>,
}

impl StepRecorder {
    /// A key of channel `slot` went down (`velocity > 0`) or up. Returns the note a key-down
    /// enters: at the cursor, `step` ticks long. When the last held key goes up the cursor
    /// moves on by `step`.
    pub fn key(&mut self, slot: u16, key: u8, velocity: f32, step: i64) -> Option<(u16, Note)> {
        let step = step.max(1);
        if velocity > 0.0 {
            if self.held.contains(&(slot, key)) {
                return None;
            }
            self.held.push((slot, key));
            return Some((
                slot,
                Note {
                    start: self.cursor,
                    length: step,
                    key: key.min(127),
                    velocity,
                },
            ));
        }
        let i = self.held.iter().position(|&h| h == (slot, key))?;
        self.held.remove(i);
        if self.held.is_empty() {
            self.cursor += step;
        }
        None
    }

    /// Moves the cursor (the user located), forgetting held keys.
    pub fn move_to(&mut self, tick: i64) {
        self.cursor = tick.max(0);
        self.held.clear();
    }
}

/// How far back "Capture last take" looks, in seconds.
pub const CAPTURE_WINDOW_S: f64 = 300.0;
/// A pause this long (seconds) separates the last take from what was played before it.
pub const TAKE_GAP_S: f64 = 8.0;

/// Notes and controller data rebuilt from live events.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Captured {
    /// Notes by channel slot, from tick 0 (the first note).
    pub notes: Vec<(u16, Note)>,
    /// Controller points by channel slot.
    pub controls: Vec<(u16, ControlPoint)>,
}

/// The last take in `events` (stamped with their arrival, oldest first, as
/// [`gt_engine::LiveInput::recent`] gives them) as notes at `bpm`: the events after the last
/// pause of [`TAKE_GAP_S`], no older than [`CAPTURE_WINDOW_S`] before `now_ns`. The first
/// note starts at tick 0. `slot_of` says which channel slot an event plays.
pub fn capture(
    events: &[LiveEvent],
    now_ns: u64,
    bpm: f64,
    slot_of: impl Fn(&LiveEvent) -> Option<u16>,
) -> Captured {
    let ns = |s: f64| (s * 1e9) as u64;
    let oldest = now_ns.saturating_sub(ns(CAPTURE_WINDOW_S));
    let mut first = events.partition_point(|e| e.at_ns < oldest);
    for i in (first + 1..events.len()).rev() {
        if events[i].at_ns.saturating_sub(events[i - 1].at_ns) >= ns(TAKE_GAP_S) {
            first = i;
            break;
        }
    }
    let take = &events[first..];
    let Some(t0) = take
        .iter()
        .find(|e| matches!(e.kind, LiveKind::Note(n) if n.is_on()))
        .map(|e| e.at_ns)
    else {
        return Captured::default();
    };
    let ticks_per_ns = bpm.max(1.0) / 60.0 * gt_core::PPQ as f64 / 1e9;
    let tick = |at: u64| (at.saturating_sub(t0) as f64 * ticks_per_ns).round() as i64;
    let end = take.last().map_or(0, |e| tick(e.at_ns)) + 1;
    let mut out = Captured::default();
    let mut open: BTreeMap<(u16, u8), (i64, f32)> = BTreeMap::new();
    let mut controls: BTreeMap<u16, Vec<ControlPoint>> = BTreeMap::new();
    for e in take {
        let Some(slot) = slot_of(e) else {
            continue;
        };
        let t = tick(e.at_ns);
        let (kind, value) = match e.kind {
            LiveKind::Note(n) => {
                let k = (slot, n.key);
                if let Some((start, velocity)) = open.remove(&k) {
                    out.notes.push((slot, note(start, t, n.key, velocity)));
                }
                if n.is_on() && e.at_ns >= t0 {
                    open.insert(k, (t, n.velocity));
                }
                continue;
            }
            LiveKind::Bend(v) => (ControlKind::Bend, v),
            LiveKind::Sustain(d) => (ControlKind::Sustain, if d { 1.0 } else { 0.0 }),
            LiveKind::ModWheel(v) => (ControlKind::ModWheel, v),
            LiveKind::Pressure(v) => (ControlKind::Pressure, v),
            LiveKind::KeyPressure { .. } => continue,
        };
        // Controllers moved before the first note set the value it starts with.
        add_control(
            controls.entry(slot).or_default(),
            ControlPoint {
                tick: t,
                kind,
                value,
            },
        );
    }
    for ((slot, key), (start, velocity)) in open {
        out.notes.push((slot, note(start, end, key, velocity)));
    }
    out.notes.sort_by_key(|(s, n)| (n.start, *s, n.key));
    out.controls = controls
        .into_iter()
        .flat_map(|(s, v)| v.into_iter().map(move |c| (s, c)))
        .collect();
    out
}

fn note(start: i64, end: i64, key: u8, velocity: f32) -> Note {
    Note {
        start,
        length: (end - start).max(1),
        key,
        velocity,
    }
}

/// Milliseconds as ticks at `bpm`.
pub fn ms_to_ticks(ms: f64, bpm: f64) -> f64 {
    ms / 1000.0 * bpm / 60.0 * gt_core::PPQ as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_take_turns_key_presses_into_notes() {
        let mut r = Recorder::default();
        let len = 3840;
        let n = |x: Option<(u16, Note)>| x.map(|(s, n)| (s, n.start, n.length, n.key));
        assert_eq!(r.note(0, 60, 0.5, Some(100), len), None);
        let (_, x) = r.note(0, 60, 0.0, Some(580), len).unwrap();
        assert_eq!((x.start, x.length, x.key, x.velocity), (100, 480, 60, 0.5));
        // Positions past the pattern wrap into it (pattern mode loops).
        r.note(0, 62, 1.0, Some(3840 + 200), len);
        assert_eq!(
            n(r.note(0, 62, 0.0, Some(3840 + 300), len)),
            Some((0, 200, 100, 62))
        );
        // A note held across the loop point is cut at the pattern end.
        r.note(0, 64, 1.0, Some(3700), len);
        assert_eq!(
            n(r.note(0, 64, 0.0, Some(3840 + 50), len)),
            Some((0, 3700, 140, 64))
        );
        // A key-up without its key-down (pressed before recording) is ignored.
        assert_eq!(r.note(0, 65, 0.0, Some(10), len), None);
        // Outside the pattern (song mode, no clip under the playhead): not recorded.
        assert_eq!(r.note(0, 66, 1.0, None, len), None);
        assert_eq!(r.note(0, 66, 0.0, Some(20), len), None);
        // The same key on two channels (two controllers) makes two notes.
        r.note(0, 67, 1.0, Some(1000), len);
        r.note(3, 67, 1.0, Some(1100), len);
        assert_eq!(
            n(r.note(3, 67, 0.0, Some(1300), len)),
            Some((3, 1100, 200, 67))
        );
        // Stopping completes held notes.
        r.note(0, 69, 1.0, Some(1200), len);
        let left = r.finish(Some(1500), len);
        assert_eq!(left.len(), 2);
        assert_eq!((left[0].0, left[0].1.key, left[0].1.length), (0, 67, 500));
        assert!(r.finish(None, len).is_empty());
    }

    #[test]
    fn the_playhead_sweeps_ranges_and_wraps() {
        assert_eq!(swept(100, 250, 3840), (vec![(100, 250)], false));
        assert_eq!(swept(3800, 40, 3840), (vec![(3800, 3840), (0, 40)], true));
        assert_eq!(swept(7, 7, 3840), (vec![(7, 7)], false));
    }

    #[test]
    fn controller_points_are_thinned_and_kept_sorted() {
        let p = |tick, kind, value| ControlPoint { tick, kind, value };
        let mut v = Vec::new();
        // Resting values with nothing before them are not worth a point.
        assert!(!add_control(&mut v, p(0, ControlKind::Bend, 0.0)));
        assert!(add_control(&mut v, p(100, ControlKind::Bend, 0.2)));
        // 5 ticks later: the point takes the new value instead.
        assert!(add_control(&mut v, p(105, ControlKind::Bend, 0.3)));
        assert_eq!(v, vec![p(100, ControlKind::Bend, 0.3)]);
        assert!(add_control(&mut v, p(200, ControlKind::Bend, 0.0)));
        // The same value again is left out.
        assert!(!add_control(&mut v, p(300, ControlKind::Bend, 0.0)));
        // The pedal is never thinned by time, and other kinds do not interfere.
        assert!(add_control(&mut v, p(150, ControlKind::Sustain, 1.0)));
        assert!(add_control(&mut v, p(152, ControlKind::Sustain, 0.0)));
        assert!(add_control(&mut v, p(110, ControlKind::ModWheel, 0.5)));
        let ticks: Vec<i64> = v.iter().map(|c| c.tick).collect();
        assert_eq!(ticks, vec![100, 110, 150, 152, 200]);
    }

    #[test]
    fn step_recording_enters_chords_at_the_cursor_and_moves_on() {
        let mut s = StepRecorder::default();
        let step = 240;
        // A chord: both notes at the cursor; the cursor moves when the last key is up.
        let a = s.key(0, 60, 0.8, step).unwrap().1;
        let b = s.key(0, 64, 0.8, step).unwrap().1;
        assert_eq!((a.start, a.length, b.start), (0, 240, 0));
        assert_eq!(s.key(0, 60, 0.0, step), None);
        assert_eq!(s.cursor, 0);
        s.key(0, 64, 0.0, step);
        assert_eq!(s.cursor, 240);
        // A key repeating while held is ignored; a stray key-up does nothing.
        let c = s.key(1, 67, 1.0, step).unwrap();
        assert_eq!((c.0, c.1.start), (1, 240));
        assert_eq!(s.key(1, 67, 1.0, step), None);
        assert_eq!(s.key(0, 50, 0.0, step), None);
        assert_eq!(s.cursor, 240);
        s.key(1, 67, 0.0, step);
        assert_eq!(s.cursor, 480);
        // A new snap applies from the next step; locating moves the cursor.
        s.key(1, 69, 1.0, 120);
        s.key(1, 69, 0.0, 120);
        assert_eq!(s.cursor, 600);
        s.move_to(1920);
        assert_eq!(s.key(0, 60, 1.0, step).unwrap().1.start, 1920);
    }

    #[test]
    fn capture_rebuilds_the_last_take_from_its_arrival_times() {
        let s = 1_000_000_000_u64;
        let ev = |at_ns, kind| LiveEvent {
            kind,
            port: 0,
            channel: 0,
            at_ns,
        };
        let on = |k| LiveKind::Note(LiveNote::on(k, 0.5));
        let off = |k| LiveKind::Note(LiveNote::off(k));
        let events = vec![
            // An earlier take, 20 s of silence before the last one.
            ev(10 * s, on(40)),
            ev(11 * s, off(40)),
            // The last take, at 120 BPM: a beat is 0.5 s (960 ticks).
            ev(30 * s, LiveKind::ModWheel(0.5)),
            ev(31 * s, on(60)),
            ev(31 * s + s / 2, off(60)),
            ev(32 * s, on(62)),
            ev(32 * s + s / 4, LiveKind::Bend(0.5)),
            ev(33 * s, on(64)),
        ];
        let c = capture(&events, 34 * s, 120.0, |_| Some(2));
        let notes: Vec<_> = c
            .notes
            .iter()
            .map(|(s, n)| (*s, n.start, n.length, n.key))
            .collect();
        // 64 never went up: it lasts to the last event.
        assert_eq!(
            notes,
            vec![(2, 0, 960, 60), (2, 1920, 1921, 62), (2, 3840, 1, 64)]
        );
        // The wheel moved before the first note: it holds from tick 0 (thinned onto it).
        let controls: Vec<_> = c.controls.iter().map(|(_, p)| (p.tick, p.kind)).collect();
        assert_eq!(
            controls,
            vec![(0, ControlKind::ModWheel), (2400, ControlKind::Bend)]
        );
        // Older than the window, or nothing but controllers: nothing.
        assert!(capture(&events, 1000 * s, 120.0, |_| Some(2))
            .notes
            .is_empty());
        assert!(capture(&events[2..3], 31 * s, 120.0, |_| Some(2))
            .notes
            .is_empty());
        // Events that play no channel are left out.
        assert!(capture(&events, 34 * s, 120.0, |_| None).notes.is_empty());
    }

    #[test]
    fn latency_converts_to_ticks() {
        // 10 ms at 120 BPM: 0.02 beats = 19.2 ticks.
        assert!((ms_to_ticks(10.0, 120.0) - 19.2).abs() < 1e-9);
    }

    #[test]
    fn typing_keys_play_notes_and_release_on_focus_loss() {
        let ctx = egui::Context::default();
        let mut kb = TypingKeyboard {
            on: true,
            ..TypingKeyboard::default()
        };
        let key = |key, pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let mut input = egui::RawInput {
            focused: true,
            ..Default::default()
        };
        input.events = vec![
            key(Key::Z, true),
            key(Key::Num2, true),
            egui::Event::Text("z".into()),
            key(Key::Space, true),
        ];
        let mut notes = Vec::new();
        let mut rest = Vec::new();
        let mut out = ctx.run_ui(input, |ui| {
            notes = kb.handle(ui.ctx(), true);
            rest = ui.input(|i| i.events.clone());
        });
        out.textures_delta.clear();
        assert_eq!(
            notes,
            vec![
                LiveNote::on(48, TYPING_VELOCITY),
                LiveNote::on(61, TYPING_VELOCITY)
            ]
        );
        // Space stays a shortcut; typed text is swallowed.
        assert_eq!(rest, vec![key(Key::Space, true)]);
        // Leaving piano mode releases what is held.
        kb.on = false;
        let mut off = Vec::new();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            off = kb.handle(ui.ctx(), true);
        });
        out.textures_delta.clear();
        assert_eq!(off.len(), 2);
        assert!(off.iter().all(|n| !n.is_on()));
    }
}
