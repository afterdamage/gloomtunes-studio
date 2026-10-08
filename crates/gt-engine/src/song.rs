//! Compiled, real-time-friendly views of the project document.
//!
//! The UI thread compiles the document into these flat structures and sends them boxed to the
//! audio thread, which only reads them. Replacements come back through the garbage queue.

use gt_core::{Adsr, Channel, LoopMode, Project, STEP_TICKS};

/// What a pattern event does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NoteKind {
    /// Start a note with this velocity (0 to 1).
    On {
        /// Velocity.
        velocity: f32,
    },
    /// Release the note.
    Off,
}

/// One note-on or note-off at a tick inside the pattern.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SongEvent {
    /// Tick from the pattern start, in `0..length`.
    pub tick: i64,
    /// Channel slot (index in the channel rack).
    pub slot: u16,
    /// MIDI key.
    pub key: u8,
    /// On or off.
    pub kind: NoteKind,
}

impl SongEvent {
    /// Sort key: by tick, and note-offs before note-ons on the same tick, so a note that ends
    /// exactly where the next one starts does not cut the new one.
    fn order(&self) -> (i64, u8) {
        let k = match self.kind {
            NoteKind::Off => 0,
            NoteKind::On { .. } => 1,
        };
        (self.tick, k)
    }
}

/// The playing pattern, flattened: sorted note events that repeat every `length` ticks from
/// tick 0 of the timeline.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SongSnapshot {
    /// Pattern length in ticks (0: nothing plays).
    pub length: i64,
    /// Events sorted by [`SongEvent::order`].
    pub events: Vec<SongEvent>,
}

impl SongSnapshot {
    /// Compiles the current pattern of `project`. Slots are channel indices. Swing delays notes
    /// that start on an odd 1/16 step by `swing * STEP_TICKS / 2`. Note-offs past the pattern
    /// end wrap around to the next repetition. The pattern length fits all notes
    /// (`Pattern::length_ticks`); notes with a negative start are skipped.
    pub fn compile(project: &Project) -> Self {
        let pattern = project.current_pattern();
        let length = pattern.length_ticks();
        let swing_ticks =
            (f64::from(project.swing.clamp(0.0, 1.0)) * STEP_TICKS as f64 / 2.0).round() as i64;
        let mut events = Vec::new();
        for (slot, ch) in project.channels.iter().enumerate() {
            for n in pattern.channel_notes(ch.id) {
                if n.start < 0 || n.start >= length {
                    continue;
                }
                let swung = n.start.rem_euclid(2 * STEP_TICKS) == STEP_TICKS;
                let on = n.start + if swung { swing_ticks } else { 0 };
                let off = on + n.length.max(1);
                events.push(SongEvent {
                    tick: on.rem_euclid(length),
                    slot: slot as u16,
                    key: n.key,
                    kind: NoteKind::On {
                        velocity: n.velocity.clamp(0.0, 1.0),
                    },
                });
                events.push(SongEvent {
                    tick: off.rem_euclid(length),
                    slot: slot as u16,
                    key: n.key,
                    kind: NoteKind::Off,
                });
            }
        }
        events.sort_by_key(SongEvent::order);
        Self { length, events }
    }

    /// Events with `lo <= tick <= hi`.
    pub fn events_in(&self, lo: i64, hi: i64) -> &[SongEvent] {
        let a = self.events.partition_point(|e| e.tick < lo);
        let b = self.events.partition_point(|e| e.tick <= hi);
        &self.events[a..b.max(a)]
    }
}

/// Per-channel settings as the engine uses them. Sent boxed, see `EngineCommand`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelParams {
    /// Linear gain; 0 when muted or silenced by another channel's solo.
    pub gain: f32,
    /// Pan, -1 to 1.
    pub pan: f32,
    /// Transposition in semitones.
    pub pitch: f32,
    /// Start point, fraction of the sample.
    pub start: f32,
    /// End point, fraction of the sample.
    pub end: f32,
    /// Loop instead of one-shot.
    pub looped: bool,
    /// Envelope.
    pub adsr: Adsr,
}

impl Default for ChannelParams {
    fn default() -> Self {
        Self {
            gain: 0.0,
            pan: 0.0,
            pitch: 0.0,
            start: 0.0,
            end: 1.0,
            looped: false,
            adsr: Adsr::default(),
        }
    }
}

impl ChannelParams {
    /// Engine parameters for a document channel. `silenced` is the effective mute (mute, or
    /// another channel is soloed).
    pub fn from_channel(ch: &Channel, silenced: bool) -> Self {
        let s = &ch.sampler;
        Self {
            gain: if silenced {
                0.0
            } else {
                ch.volume.clamp(0.0, Channel::MAX_VOLUME)
            },
            pan: ch.pan.clamp(-1.0, 1.0),
            pitch: s.pitch,
            start: s.start,
            end: s.end,
            looped: s.loop_mode == LoopMode::Loop,
            adsr: s.adsr,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_core::{Project, PPQ};

    fn ons(s: &SongSnapshot) -> Vec<(i64, u16)> {
        s.events
            .iter()
            .filter(|e| matches!(e.kind, NoteKind::On { .. }))
            .map(|e| (e.tick, e.slot))
            .collect()
    }

    #[test]
    fn demo_pattern_compiles_to_sorted_events() {
        let p = Project::demo();
        let s = SongSnapshot::compile(&p);
        assert_eq!(s.length, 4 * PPQ);
        assert!(s.events.windows(2).all(|w| w[0].order() <= w[1].order()));
        let kicks: Vec<_> = ons(&s).into_iter().filter(|&(_, slot)| slot == 0).collect();
        assert_eq!(kicks, vec![(0, 0), (960, 0), (1920, 0), (2880, 0)]);
        // Every on has an off.
        let offs = s.events.iter().filter(|e| e.kind == NoteKind::Off).count();
        assert_eq!(offs * 2, s.events.len());
    }

    #[test]
    fn swing_delays_odd_steps_only() {
        let mut p = Project::empty();
        let c = p.add_channel("c", None).unwrap();
        for step in 0..4 {
            p.current_pattern_mut().toggle_step(c, step);
        }
        p.swing = 1.0;
        let s = SongSnapshot::compile(&p);
        assert_eq!(
            ons(&s).iter().map(|e| e.0).collect::<Vec<_>>(),
            vec![0, 240 + 120, 480, 720 + 120]
        );
        p.swing = 0.5;
        let s = SongSnapshot::compile(&p);
        assert_eq!(ons(&s)[1].0, 240 + 60);
    }

    #[test]
    fn offs_wrap_and_order_before_ons() {
        let mut p = Project::empty();
        let c = p.add_channel("c", None).unwrap();
        p.current_pattern_mut().toggle_step(c, 15);
        p.current_pattern_mut().toggle_step(c, 0);
        let s = SongSnapshot::compile(&p);
        // Step 15's off lands on tick 0 of the next repetition, before step 0's on.
        assert_eq!(s.events[0].tick, 0);
        assert_eq!(s.events[0].kind, NoteKind::Off);
        assert!(matches!(s.events[1].kind, NoteKind::On { .. }));
    }

    #[test]
    fn pattern_length_covers_notes_past_the_grid() {
        let mut p = Project::empty();
        let c = p.add_channel("c", None).unwrap();
        let pat = p.current_pattern_mut();
        pat.steps = 32;
        pat.toggle_step(c, 20);
        pat.toggle_step(c, 2);
        pat.steps = 16;
        let s = SongSnapshot::compile(&p);
        assert_eq!(s.length, 32 * STEP_TICKS);
        assert_eq!(ons(&s), vec![(480, 0), (20 * 240, 0)]);
    }

    #[test]
    fn events_in_is_inclusive() {
        let s = SongSnapshot::compile(&Project::demo());
        let e = s.events_in(960, 960);
        assert!(e.iter().all(|e| e.tick == 960));
        assert!(e.iter().any(|e| e.slot == 0));
        assert!(s.events_in(5, 4).is_empty());
    }

    #[test]
    fn silenced_channels_have_zero_gain() {
        let p = Project::demo();
        assert_eq!(ChannelParams::from_channel(&p.channels[0], true).gain, 0.0);
        assert_eq!(
            ChannelParams::from_channel(&p.channels[0], false).gain,
            0.63
        );
    }
}
