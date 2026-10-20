//! MPE input (Prompt 33, D148): an MPE controller plays each note on its own MIDI channel
//! (the lower zone: channel 1 is the master, 2 to 16 the members), so that channel's pitch
//! bend, pressure and CC 74 belong to that note alone. [`MpeState`] turns member-channel
//! messages into notes on the master channel (which is what routing sees) plus per-note
//! expressions on their key; a note's expressions are also sent just before it starts, so it
//! starts with them (and records them into its fine pitch and mod X).

use gt_engine::{LiveKind, LiveNote, NoteExprKind};

/// Member channel pitch bend range, semitones each way (the MPE default).
pub const MEMBER_BEND: f32 = 48.0;
/// The master channel of the lower zone (MIDI channel 1).
pub const MASTER: u8 = 0;

/// What a member channel last sent.
#[derive(Debug, Clone, Copy)]
struct Member {
    /// Keys held on the channel (128 bits).
    keys: u128,
    /// Bend, -1 to 1.
    bend: f32,
    pressure: f32,
    timbre: f32,
}

impl Default for Member {
    fn default() -> Self {
        Self {
            keys: 0,
            bend: 0.0,
            pressure: 0.0,
            timbre: 0.5,
        }
    }
}

/// One MPE port's member channels.
#[derive(Debug, Clone, Default)]
pub struct MpeState {
    members: [Member; 16],
}

impl MpeState {
    /// Handles a message from a member channel: `out` gets what to play on the master
    /// channel. False for messages it leaves alone (the master channel, and member messages
    /// that are not notes or expressions).
    pub fn handle(&mut self, msg: &[u8], out: &mut impl FnMut(LiveKind)) -> bool {
        let Some((&status, data)) = msg.split_first() else {
            return false;
        };
        let ch = usize::from(status & 0x0F);
        if ch == usize::from(MASTER) || !(0x80..0xF0).contains(&status) {
            return false;
        }
        let m = &mut self.members[ch];
        let unit = |v: u8| f32::from(v & 0x7F) / 127.0;
        let expr = |key: u8, kind, value| LiveKind::NoteExpr { key, kind, value };
        let each_key = |keys: u128, out: &mut dyn FnMut(u8)| {
            for k in 0..128u8 {
                if keys >> k & 1 == 1 {
                    out(k);
                }
            }
        };
        match (status & 0xF0, data) {
            (0x90, &[key, vel, ..]) if vel > 0 => {
                let key = key & 0x7F;
                out(expr(key, NoteExprKind::Tuning, m.bend * MEMBER_BEND));
                out(expr(key, NoteExprKind::Timbre, m.timbre));
                out(LiveKind::Note(LiveNote::on(key, unit(vel))));
                if m.pressure > 0.0 {
                    out(expr(key, NoteExprKind::Pressure, m.pressure));
                }
                m.keys |= 1 << key;
            }
            (0x80 | 0x90, &[key, ..]) => {
                let key = key & 0x7F;
                out(LiveKind::Note(LiveNote::off(key)));
                m.keys &= !(1 << key);
            }
            (0xE0, &[lsb, msb, ..]) => {
                let v = (i32::from(msb & 0x7F) << 7 | i32::from(lsb & 0x7F)) - 8192;
                m.bend = (v as f32 / 8191.0).clamp(-1.0, 1.0);
                let semis = m.bend * MEMBER_BEND;
                each_key(m.keys, &mut |k| out(expr(k, NoteExprKind::Tuning, semis)));
            }
            (0xD0, &[v, ..]) => {
                m.pressure = unit(v);
                let p = m.pressure;
                each_key(m.keys, &mut |k| out(expr(k, NoteExprKind::Pressure, p)));
            }
            (0xA0, &[key, v, ..]) => {
                out(expr(key & 0x7F, NoteExprKind::Pressure, unit(v)));
            }
            (0xB0, &[74, v, ..]) => {
                m.timbre = unit(v);
                let t = m.timbre;
                each_key(m.keys, &mut |k| out(expr(k, NoteExprKind::Timbre, t)));
            }
            _ => return false,
        }
        true
    }

    /// Keys held on member channels.
    #[cfg(test)]
    pub fn held(&self) -> impl Iterator<Item = u8> + '_ {
        self.members
            .iter()
            .flat_map(|m| (0..128u8).filter(move |&k| m.keys >> k & 1 == 1))
    }

    /// Forgets every held key.
    pub fn clear(&mut self) {
        for m in &mut self.members {
            m.keys = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &mut MpeState, msg: &[u8]) -> (bool, Vec<LiveKind>) {
        let mut got = Vec::new();
        let used = s.handle(msg, &mut |k| got.push(k));
        (used, got)
    }

    #[test]
    fn member_channels_become_notes_with_their_own_expression() {
        let mut s = MpeState::default();
        // Bend and timbre before the note set its starting values.
        run(&mut s, &[0xE1, 0, 0x50]); // channel 2, bend up
        run(&mut s, &[0xB1, 74, 127]);
        let (used, got) = run(&mut s, &[0x91, 60, 100]);
        assert!(used);
        let bend = ((0x50 << 7) - 8192) as f32 / 8191.0 * MEMBER_BEND;
        assert_eq!(
            got,
            vec![
                LiveKind::NoteExpr {
                    key: 60,
                    kind: NoteExprKind::Tuning,
                    value: bend
                },
                LiveKind::NoteExpr {
                    key: 60,
                    kind: NoteExprKind::Timbre,
                    value: 1.0
                },
                LiveKind::Note(LiveNote::on(60, 100.0 / 127.0)),
            ]
        );
        // A second note on channel 3: pressure on channel 2 reaches only key 60.
        run(&mut s, &[0x92, 64, 90]);
        let (_, got) = run(&mut s, &[0xD1, 64]);
        assert_eq!(
            got,
            vec![LiveKind::NoteExpr {
                key: 60,
                kind: NoteExprKind::Pressure,
                value: 64.0 / 127.0
            }]
        );
        let (_, got) = run(&mut s, &[0xE2, 0, 64]);
        assert_eq!(
            got,
            vec![LiveKind::NoteExpr {
                key: 64,
                kind: NoteExprKind::Tuning,
                value: 0.0
            }]
        );
        assert_eq!(s.held().collect::<Vec<_>>(), vec![60, 64]);
        let (_, got) = run(&mut s, &[0x81, 60, 0]);
        assert_eq!(got, vec![LiveKind::Note(LiveNote::off(60))]);
        // After the release, channel 2's expression reaches nothing.
        assert_eq!(run(&mut s, &[0xD1, 10]).1, vec![]);
    }

    #[test]
    fn the_master_channel_and_other_messages_pass_through() {
        let mut s = MpeState::default();
        assert!(!run(&mut s, &[0x90, 60, 100]).0);
        assert!(!run(&mut s, &[0xE0, 0, 64]).0);
        // Sustain on a member channel is not a per-note expression.
        assert!(!run(&mut s, &[0xB3, 64, 127]).0);
        assert!(!run(&mut s, &[0xF8]).0);
    }
}
