//! Playing and recording live: the computer keyboard as a piano, and turning live notes into
//! pattern notes.

use egui::Key;
use gt_core::Note;
use gt_engine::LiveNote;

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

/// Builds pattern notes from live notes during a take.
#[derive(Debug, Clone)]
pub struct Recorder {
    /// Recording is on: live notes while playing go into the pattern.
    pub armed: bool,
    /// Keys held, with their start in pattern ticks and velocity.
    open: [Option<(i64, f32)>; 128],
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            armed: false,
            open: [None; 128],
        }
    }
}

impl Recorder {
    /// A key went down (`velocity > 0`) or up at pattern tick `at` (none: outside the
    /// pattern, e.g. between its clips in song mode). Returns the note a key-up completes.
    /// Notes are cut at the pattern end `len`; one held across the loop point wraps.
    pub fn note(&mut self, key: u8, velocity: f32, at: Option<i64>, len: i64) -> Option<Note> {
        let k = usize::from(key.min(127));
        let len = len.max(1);
        if velocity > 0.0 {
            // A key struck again while held ends the earlier note.
            let done = self.close(k, at, len);
            self.open[k] = at.map(|t| (t.rem_euclid(len), velocity));
            done
        } else {
            self.close(k, at, len)
        }
    }

    fn close(&mut self, k: usize, at: Option<i64>, len: i64) -> Option<Note> {
        let (start, velocity) = self.open[k].take()?;
        let length = match at {
            Some(t) => (t.rem_euclid(len) - start).rem_euclid(len),
            None => len - start,
        };
        Some(Note {
            start,
            length: length.clamp(1, len - start),
            key: k as u8,
            velocity,
        })
    }

    /// Ends the take: completes every held note at `at` (none: at the pattern end).
    pub fn finish(&mut self, at: Option<i64>, len: i64) -> Vec<Note> {
        (0..128)
            .filter_map(|k| self.close(k, at, len.max(1)))
            .collect()
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
        assert_eq!(r.note(60, 0.5, Some(100), len), None);
        let n = r.note(60, 0.0, Some(580), len).unwrap();
        assert_eq!((n.start, n.length, n.key, n.velocity), (100, 480, 60, 0.5));
        // Positions past the pattern wrap into it (pattern mode loops).
        r.note(62, 1.0, Some(3840 + 200), len);
        let n = r.note(62, 0.0, Some(3840 + 300), len).unwrap();
        assert_eq!((n.start, n.length), (200, 100));
        // A note held across the loop point is cut at the pattern end.
        r.note(64, 1.0, Some(3700), len);
        let n = r.note(64, 0.0, Some(3840 + 50), len).unwrap();
        assert_eq!((n.start, n.length), (3700, 140));
        // A key-up without its key-down (pressed before recording) is ignored.
        assert_eq!(r.note(65, 0.0, Some(10), len), None);
        // Outside the pattern (song mode, no clip under the playhead): not recorded.
        assert_eq!(r.note(66, 1.0, None, len), None);
        assert_eq!(r.note(66, 0.0, Some(20), len), None);
        // Stopping completes held notes.
        r.note(67, 1.0, Some(1000), len);
        r.note(69, 1.0, Some(1200), len);
        let left = r.finish(Some(1500), len);
        assert_eq!(left.len(), 2);
        assert_eq!((left[0].key, left[0].length), (67, 500));
        assert!(r.finish(None, len).is_empty());
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
