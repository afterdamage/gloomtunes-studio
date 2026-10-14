//! Note routing between scheduling and rendering (Prompt 27, D136): layer channels fan their
//! notes out to their children, key settings (range, root, mono, voice limit) shape the notes
//! of non-sampler instruments, and a note on a channel in a choke group cuts the other
//! channels of that group on the same frame. Real-time safe: every buffer is allocated in
//! [`NoteRouter::new`].

use gt_core::{LayerMode, MAX_CHANNELS, ROOT_KEY};

use crate::channel::ChannelSlot;
use crate::song::{InstrumentKind, MAX_LAYER_CHILDREN};
use crate::transport::{EventBuf, EventKind, ScheduledEvent, MAX_EVENTS_PER_QUANTUM};

/// Most keys tracked per channel for mono and the voice limit.
const HELD: usize = gt_core::KeyLimits::MAX_VOICES as usize;

/// Routing state carried from one quantum to the next.
pub(crate) struct NoteRouter {
    /// The quantum's events as scheduled, read while the routed ones are written back.
    input: Box<[ScheduledEvent]>,
    /// Per layer slot and key: the children (bits) that play the note, for its note-off.
    layer_keys: Box<[u16]>,
    /// Per layer slot: the next child in round-robin mode.
    next: Box<[u8]>,
    /// Random mode's generator (xorshift32).
    rng: u32,
    /// Per slot: keys held, oldest first (only for mono or a voice limit).
    held: Box<[[u8; HELD]]>,
    held_len: Box<[u8]>,
}

const SEED: u32 = 0x9E37_79B9;

impl NoteRouter {
    pub(crate) fn new() -> Self {
        Self {
            input: vec![
                ScheduledEvent {
                    offset: 0,
                    tick: gt_core::Tick(0),
                    kind: EventKind::Wrap,
                };
                MAX_EVENTS_PER_QUANTUM
            ]
            .into_boxed_slice(),
            layer_keys: vec![0; MAX_CHANNELS * 128].into_boxed_slice(),
            next: vec![0; MAX_CHANNELS].into_boxed_slice(),
            rng: SEED,
            held: vec![[0; HELD]; MAX_CHANNELS].into_boxed_slice(),
            held_len: vec![0; MAX_CHANNELS].into_boxed_slice(),
        }
    }

    /// Forgets held notes, round-robin positions and the random sequence (transport start,
    /// new song), so a render plays the same way every time.
    pub(crate) fn reset(&mut self) {
        self.layer_keys.fill(0);
        self.next.fill(0);
        self.rng = SEED;
        self.held_len.fill(0);
    }

    /// Rewrites the quantum's note events in place. Cheap when no channel uses a layer, key
    /// settings or choke group: one scan of the events.
    pub(crate) fn route(&mut self, events: &mut EventBuf, channels: &[ChannelSlot]) {
        let busy = |s: u16| {
            channels.get(usize::from(s)).is_some_and(|c| {
                let p = c.params();
                p.kind == InstrumentKind::Layer || p.choke > 0 || p.keys.apply
            })
        };
        if !events
            .as_slice()
            .iter()
            .any(|e| matches!(e.kind, EventKind::Wrap) || target(e.kind).is_some_and(busy))
        {
            return;
        }
        let n = events.take_events(&mut self.input);
        for i in 0..n {
            let e = self.input[i];
            match e.kind {
                EventKind::Wrap => {
                    self.layer_keys.fill(0);
                    self.held_len.fill(0);
                    events.push_unsorted(e);
                }
                k => match target(k) {
                    Some(s) if usize::from(s) < channels.len() => {
                        let p = channels[usize::from(s)].params();
                        if p.kind == InstrumentKind::Layer {
                            self.fan_out(e, channels, events);
                        } else {
                            self.play(e, channels, events);
                        }
                    }
                    _ => events.push_unsorted(e),
                },
            }
        }
        events.finish();
    }

    /// A layer's note: copies for the children the mode picks; the note-off goes to the
    /// children that played the key.
    fn fan_out(&mut self, e: ScheduledEvent, channels: &[ChannelSlot], out: &mut EventBuf) {
        let Some(slot) = target(e.kind).map(usize::from) else {
            return;
        };
        let layer = channels[slot].params().layer;
        let count = usize::from(layer.count).min(MAX_LAYER_CHILDREN);
        let child = |e: ScheduledEvent, to: u16| ScheduledEvent {
            kind: retarget(e.kind, to),
            ..e
        };
        match e.kind {
            EventKind::NoteOn { key, velocity, .. } | EventKind::Chase { key, velocity, .. } => {
                let range = gt_core::rack::layer_candidates(count, layer.split, velocity);
                let mask: u16 = match (layer.mode, range.len()) {
                    (_, 0) => 0,
                    (LayerMode::All, _) => range.clone().fold(0, |m, i| m | 1 << i),
                    (LayerMode::Random, len) => {
                        self.rng ^= self.rng << 13;
                        self.rng ^= self.rng >> 17;
                        self.rng ^= self.rng << 5;
                        1 << (range.start + self.rng as usize % len)
                    }
                    (LayerMode::RoundRobin, len) => {
                        let k = usize::from(self.next[slot]) % len;
                        self.next[slot] = ((k + 1) % len) as u8;
                        1 << (range.start + k)
                    }
                };
                let held = !matches!(e.kind, EventKind::Chase { held: false, .. });
                let h = &mut self.layer_keys[slot * 128 + usize::from(key & 0x7F)];
                if held {
                    *h |= mask;
                }
                for i in (0..count).filter(|i| mask & 1 << i != 0) {
                    self.play(child(e, layer.children[i]), channels, out);
                }
            }
            EventKind::NoteOff { key, .. } => {
                let h = &mut self.layer_keys[slot * 128 + usize::from(key & 0x7F)];
                let mask = std::mem::take(h);
                for i in (0..count).filter(|i| mask & 1 << i != 0) {
                    self.play(child(e, layer.children[i]), channels, out);
                }
            }
            // A layer's controllers reach every child.
            EventKind::Control { .. } => {
                for &c in &layer.children[..count] {
                    out.push_unsorted(child(e, c));
                }
            }
            _ => {}
        }
    }

    /// A note on a sounding channel: key settings, then the choke group.
    fn play(&mut self, mut e: ScheduledEvent, channels: &[ChannelSlot], out: &mut EventBuf) {
        let Some(slot) = e.kind.slot().map(usize::from) else {
            return;
        };
        let Some(ch) = channels.get(slot) else {
            return;
        };
        let p = ch.params();
        if p.kind == InstrumentKind::Layer {
            // Layers of layers are not allowed (the document drops them).
            return;
        }
        if p.keys.apply {
            let k = p.keys.limits;
            let Some(key) = note_key(e.kind) else {
                out.push_unsorted(e);
                return;
            };
            if key < k.lo || key > k.hi {
                return;
            }
            let shifted = (i16::from(key) + i16::from(ROOT_KEY) - i16::from(k.root)).clamp(0, 127);
            set_key(&mut e.kind, shifted as u8);
            let key = shifted as u8;
            let limit = if k.mono {
                1
            } else {
                usize::from(k.voices).min(HELD)
            };
            if limit > 0 {
                let held = &mut self.held[slot];
                let len = &mut self.held_len[slot];
                match e.kind {
                    EventKind::NoteOn { .. } | EventKind::Chase { held: true, .. } => {
                        // The same key again replaces itself; otherwise the oldest ends.
                        if let Some(i) = held[..usize::from(*len)].iter().position(|&h| h == key) {
                            held.copy_within(i + 1..usize::from(*len), i);
                            *len -= 1;
                        }
                        while usize::from(*len) >= limit {
                            out.push_unsorted(ScheduledEvent {
                                kind: EventKind::NoteOff {
                                    slot: slot as u16,
                                    key: held[0],
                                },
                                ..e
                            });
                            held.copy_within(1..usize::from(*len), 0);
                            *len -= 1;
                        }
                        held[usize::from(*len)] = key;
                        *len += 1;
                    }
                    EventKind::NoteOff { .. } => {
                        // A note the limit already ended stays ended (its key may sound again).
                        match held[..usize::from(*len)].iter().position(|&h| h == key) {
                            Some(i) => {
                                held.copy_within(i + 1..usize::from(*len), i);
                                *len -= 1;
                            }
                            None => return,
                        }
                    }
                    _ => {}
                }
            }
        }
        let starts = matches!(e.kind, EventKind::NoteOn { .. } | EventKind::Chase { .. });
        if starts && p.choke > 0 {
            for (other, c) in channels.iter().enumerate() {
                if other != slot && c.params().choke == p.choke {
                    out.push_unsorted(ScheduledEvent {
                        kind: EventKind::Choke { slot: other as u16 },
                        ..e
                    });
                }
            }
        }
        out.push_unsorted(e);
    }
}

/// The channel slot an event is for (notes, chokes and controllers).
fn target(k: EventKind) -> Option<u16> {
    match k {
        EventKind::Control { slot, .. } => Some(slot),
        k => k.slot(),
    }
}

fn note_key(k: EventKind) -> Option<u8> {
    match k {
        EventKind::NoteOn { key, .. }
        | EventKind::NoteOff { key, .. }
        | EventKind::Chase { key, .. } => Some(key),
        _ => None,
    }
}

fn set_key(k: &mut EventKind, to: u8) {
    if let EventKind::NoteOn { key, .. }
    | EventKind::NoteOff { key, .. }
    | EventKind::Chase { key, .. } = k
    {
        *key = to;
    }
}

fn retarget(mut k: EventKind, to: u16) -> EventKind {
    match &mut k {
        EventKind::NoteOn { slot, .. }
        | EventKind::NoteOff { slot, .. }
        | EventKind::Chase { slot, .. }
        | EventKind::Choke { slot }
        | EventKind::Control { slot, .. } => *slot = to,
        _ => {}
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song::ChannelParams;
    use gt_core::{KeyLimits, Tick};

    fn on(offset: u32, key: u8) -> ScheduledEvent {
        ScheduledEvent {
            offset,
            tick: Tick(0),
            kind: EventKind::NoteOn {
                slot: 0,
                key,
                velocity: 1.0,
                expr: Default::default(),
            },
        }
    }

    fn off(offset: u32, key: u8) -> ScheduledEvent {
        ScheduledEvent {
            offset,
            tick: Tick(0),
            kind: EventKind::NoteOff { slot: 0, key },
        }
    }

    fn routed(limits: KeyLimits, input: &[ScheduledEvent]) -> Vec<(u32, bool, u8)> {
        let mut ch = ChannelSlot::new(48_000.0);
        ch.set_params_now(ChannelParams {
            kind: InstrumentKind::Synth,
            keys: crate::song::KeyParams {
                apply: true,
                limits,
            },
            ..ChannelParams::default()
        });
        let mut buf = EventBuf::default();
        for &e in input {
            buf.insert(e);
        }
        NoteRouter::new().route(&mut buf, std::slice::from_ref(&ch));
        buf.as_slice()
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::NoteOn { key, .. } => Some((e.offset, true, key)),
                EventKind::NoteOff { key, .. } => Some((e.offset, false, key)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn key_settings_transpose_limit_and_end_notes() {
        // Root 48: an octave up; keys outside 50..=80 (before transposing) are dropped.
        let k = KeyLimits {
            root: 48,
            lo: 50,
            hi: 80,
            ..KeyLimits::default()
        };
        assert_eq!(
            routed(k, &[on(0, 40), on(1, 60), off(5, 60), off(6, 40)]),
            [(1, true, 72), (5, false, 72)]
        );
        // Mono: each note ends the one before, on its own frame; the late note-off of an
        // ended note is dropped.
        let mono = KeyLimits {
            mono: true,
            ..KeyLimits::default()
        };
        assert_eq!(
            routed(mono, &[on(0, 60), on(10, 64), off(12, 60), off(20, 64)]),
            [
                (0, true, 60),
                (10, false, 60),
                (10, true, 64),
                (20, false, 64)
            ]
        );
        // Two voices: the third note ends the oldest.
        let two = KeyLimits {
            voices: 2,
            ..KeyLimits::default()
        };
        assert_eq!(
            routed(two, &[on(0, 60), on(1, 62), on(2, 64)]),
            [(0, true, 60), (1, true, 62), (2, false, 60), (2, true, 64)]
        );
    }
}
