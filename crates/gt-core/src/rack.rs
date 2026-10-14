//! Channel rack 2 (Prompt 27, ARCHITECTURE.md D136): per-step graph lanes, step tools, layer
//! channels and the key settings every instrument shares.
//!
//! A step is a note at [`ROOT_KEY`] that starts within half a step of the step's grid tick:
//! the shift lane moves the note's start off the grid (micro-timing) and the step keeps it.
//! Every other lane is a field of the note itself (Prompt 21), so the piano roll and the rack
//! edit the same data.

use crate::project::{ChannelId, Note, Pattern, DEFAULT_VELOCITY, ROOT_KEY, STEP_TICKS};

/// Largest step shift either way, in ticks (just under half a step).
pub const MAX_SHIFT: i64 = STEP_TICKS / 2 - 1;

/// A per-step value shown as a graph under the step grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepLane {
    /// Velocity, 0 to 1.
    Velocity,
    /// Pan, -1 to 1.
    Pan,
    /// Release velocity, 0 to 1.
    Release,
    /// Fine pitch, -100 to 100 cents.
    Fine,
    /// Mod X, 0 to 1.
    ModX,
    /// Mod Y, 0 to 1.
    ModY,
    /// Micro-timing: the note's start off the grid, in ticks ([`MAX_SHIFT`] either way).
    Shift,
}

impl StepLane {
    /// Every lane, in menu order.
    pub const ALL: [StepLane; 7] = [
        Self::Velocity,
        Self::Pan,
        Self::Release,
        Self::Fine,
        Self::ModX,
        Self::ModY,
        Self::Shift,
    ];

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Velocity => "Velocity",
            Self::Pan => "Pan",
            Self::Release => "Release",
            Self::Fine => "Fine pitch",
            Self::ModX => "Mod X",
            Self::ModY => "Mod Y",
            Self::Shift => "Shift",
        }
    }

    /// Lowest and highest value.
    pub fn range(self) -> (f32, f32) {
        match self {
            Self::Pan => (-1.0, 1.0),
            Self::Fine => (-100.0, 100.0),
            Self::Shift => (-(MAX_SHIFT as f32), MAX_SHIFT as f32),
            _ => (0.0, 1.0),
        }
    }

    /// The value a new step has (the line the graph is drawn from).
    pub fn neutral(self) -> f32 {
        match self {
            Self::Velocity => DEFAULT_VELOCITY,
            Self::Release | Self::ModX | Self::ModY => Note::NEUTRAL,
            Self::Pan | Self::Fine | Self::Shift => 0.0,
        }
    }

    /// True when the graph grows from the middle (signed values).
    pub fn bipolar(self) -> bool {
        matches!(self, Self::Pan | Self::Fine | Self::Shift)
    }

    /// The lane's value on a step note at grid tick `at`.
    pub fn get(self, n: &Note, at: i64) -> f32 {
        match self {
            Self::Velocity => n.velocity,
            Self::Pan => n.pan,
            Self::Release => n.release,
            Self::Fine => n.fine,
            Self::ModX => n.mod_x,
            Self::ModY => n.mod_y,
            Self::Shift => (n.start - at) as f32,
        }
    }

    /// Sets the lane's value (clamped) on a step note at grid tick `at`.
    pub fn set(self, n: &mut Note, at: i64, v: f32) {
        let (lo, hi) = self.range();
        let v = if v.is_finite() {
            v.clamp(lo, hi)
        } else {
            self.neutral()
        };
        match self {
            Self::Velocity => n.velocity = v,
            Self::Pan => n.pan = v,
            Self::Release => n.release = v,
            Self::Fine => n.fine = v,
            Self::ModX => n.mod_x = v,
            Self::ModY => n.mod_y = v,
            Self::Shift => n.start = (at + v.round() as i64).max(0),
        }
    }
}

/// Pattern colours 1 to 15 ([`Pattern::color`]; 0 is the track's colour).
pub const PATTERN_COLORS: [[u8; 3]; 15] = [
    [138, 92, 196],
    [92, 132, 196],
    [72, 160, 150],
    [120, 168, 84],
    [196, 160, 72],
    [204, 116, 72],
    [196, 84, 112],
    [150, 150, 164],
    [84, 176, 196],
    [168, 196, 84],
    [220, 196, 96],
    [220, 92, 92],
    [176, 112, 220],
    [104, 104, 220],
    [96, 196, 128],
];

impl Pattern {
    /// The pattern's own colour, or `None` to use the track's.
    pub fn rgb(&self) -> Option<[u8; 3]> {
        PATTERN_COLORS
            .get(usize::from(self.color).checked_sub(1)?)
            .copied()
    }
}

/// Grid tick of `step`.
pub fn step_tick(step: u16) -> i64 {
    i64::from(step) * STEP_TICKS
}

/// True when `n` is the step note of grid tick `at`.
pub fn is_step(n: &Note, at: i64) -> bool {
    n.key == ROOT_KEY && (n.start - at).abs() <= MAX_SHIFT
}

impl Pattern {
    /// The step notes of `channel` as an on/off row, one entry per step.
    pub fn step_row(&self, channel: ChannelId) -> Vec<Option<Note>> {
        (0..self.steps)
            .map(|s| self.step_note(channel, s).copied())
            .collect()
    }

    /// Replaces the step notes of `channel` with `row` (one entry per step; steps past the
    /// row are cleared). Other notes of the channel stay. A note keeps its shift: a note moved
    /// to another step keeps its offset from the grid, its other values unchanged.
    pub fn set_step_row(&mut self, channel: ChannelId, row: &[Option<Note>]) {
        let steps = self.steps;
        let notes = self.notes.entry(channel).or_default();
        notes.retain(|n| !(0..steps).any(|s| is_step(n, step_tick(s))));
        notes.extend(row.iter().take(usize::from(steps)).flatten());
        notes.sort_by_key(|n| n.start);
    }

    /// Sets a lane value on the note at `step` (no-op if the step is off).
    pub fn set_step_lane(&mut self, channel: ChannelId, step: u16, lane: StepLane, v: f32) {
        let at = step_tick(step);
        let Some(notes) = self.notes.get_mut(&channel) else {
            return;
        };
        if let Some(n) = notes.iter_mut().find(|n| is_step(n, at)) {
            lane.set(n, at, v);
        }
        notes.sort_by_key(|n| n.start);
    }
}

/// A step row moved to the grid ticks of `row`'s positions: each note keeps its shift and
/// values.
fn placed(row: Vec<Option<Note>>, old: &[Option<Note>]) -> Vec<Option<Note>> {
    // Shift of each note relative to the step it came from.
    let shift = |n: &Note| {
        old.iter()
            .enumerate()
            .find(|(_, o)| o.as_ref() == Some(n))
            .map_or(0, |(s, _)| n.start - step_tick(s as u16))
    };
    row.into_iter()
        .enumerate()
        .map(|(s, n)| {
            n.map(|mut n| {
                n.start = (step_tick(s as u16) + shift(&n)).max(0);
                n
            })
        })
        .collect()
}

/// Turns on every `every`-th step from `offset` (other steps unchanged; new steps are plain).
pub fn fill_every(row: &[Option<Note>], every: u16, offset: u16) -> Vec<Option<Note>> {
    let every = usize::from(every.max(1));
    row.iter()
        .enumerate()
        .map(|(s, n)| {
            if s >= usize::from(offset) && (s - usize::from(offset)) % every == 0 {
                n.or_else(|| Some(plain_step(s as u16)))
            } else {
                *n
            }
        })
        .collect()
}

/// A plain step note at `step`.
pub fn plain_step(step: u16) -> Note {
    Note::new(step_tick(step), STEP_TICKS, ROOT_KEY, DEFAULT_VELOCITY)
}

/// Rotates the row by `by` steps (positive: later), wrapping around the pattern end.
pub fn rotate(row: &[Option<Note>], by: i32) -> Vec<Option<Note>> {
    let len = row.len();
    if len == 0 {
        return Vec::new();
    }
    let by = by.rem_euclid(len as i32) as usize;
    let mut out = vec![None; len];
    for (s, n) in row.iter().enumerate() {
        out[(s + by) % len] = *n;
    }
    placed(out, row)
}

/// Shifts the row by `by` steps (positive: later) without wrapping: steps pushed past either
/// end are dropped.
pub fn shift(row: &[Option<Note>], by: i32) -> Vec<Option<Note>> {
    let len = row.len() as i64;
    let mut out = vec![None; row.len()];
    for (s, n) in row.iter().enumerate() {
        let t = s as i64 + i64::from(by);
        if (0..len).contains(&t) {
            out[t as usize] = *n;
        }
    }
    placed(out, row)
}

/// A random row: each step is on with probability `density` (0 to 1), with a velocity
/// between 0.5 and 1. The same seed gives the same row.
pub fn randomize(len: usize, density: f32, seed: u64) -> Vec<Option<Note>> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = move || {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
    };
    (0..len)
        .map(|s| {
            let on = next() < density.clamp(0.0, 1.0);
            let vel = 0.5 + 0.5 * next();
            on.then(|| {
                let mut n = plain_step(s as u16);
                n.velocity = vel;
                n
            })
        })
        .collect()
}

/// Key settings every instrument has (Prompt 27). Samplers keep theirs in
/// [`crate::SamplerSettings`] (their engine uses them per voice); the engine applies these
/// to the notes of every other instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyLimits {
    /// The key that plays at the instrument's own pitch: notes are transposed by
    /// `ROOT_KEY - root` (so 60 changes nothing).
    pub root: u8,
    /// Lowest key that plays.
    pub lo: u8,
    /// Highest key that plays.
    pub hi: u8,
    /// One note at a time: a new note ends the one before.
    pub mono: bool,
    /// Most notes held at once (the oldest ends); 0 is no limit.
    pub voices: u8,
}

impl Default for KeyLimits {
    fn default() -> Self {
        Self {
            root: ROOT_KEY,
            lo: 0,
            hi: 127,
            mono: false,
            voices: 0,
        }
    }
}

impl KeyLimits {
    /// Most voices the limit can be set to.
    pub const MAX_VOICES: u8 = 32;

    /// Puts the values in range.
    pub fn sanitize(&mut self) {
        self.root = self.root.min(127);
        self.lo = self.lo.min(127);
        self.hi = self.hi.clamp(self.lo, 127);
        self.voices = self.voices.min(Self::MAX_VOICES);
    }
}

/// How a layer channel picks its children for each note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayerMode {
    /// Every child plays.
    #[default]
    All,
    /// One child at random.
    Random,
    /// One child after another.
    RoundRobin,
}

impl LayerMode {
    /// Every mode, in menu order.
    pub const ALL: [LayerMode; 3] = [Self::All, Self::Random, Self::RoundRobin];

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Random => "Random",
            Self::RoundRobin => "Round robin",
        }
    }
}

/// A layer channel: one note plays several other channels (Prompt 27).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LayerSettings {
    /// The channels it plays, in order (never another layer, never itself).
    pub children: Vec<ChannelId>,
    /// How children are picked.
    pub mode: LayerMode,
    /// Velocity split: child `i` of `n` answers velocities in `[i/n, (i+1)/n)` (the top one
    /// up to 1), before the mode picks among the ones that answer.
    pub split: bool,
}

impl LayerSettings {
    /// Most children of one layer.
    pub const MAX_CHILDREN: usize = 16;
}

/// Children of a layer that answer `velocity`, as indices into `n` children: every child, or
/// with `split` the one whose share of the velocity range holds it.
pub fn layer_candidates(n: usize, split: bool, velocity: f32) -> std::ops::Range<usize> {
    if !split || n == 0 {
        return 0..n;
    }
    let i = ((velocity.clamp(0.0, 1.0) * n as f32) as usize).min(n - 1);
    i..i + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(on: &[usize], len: usize) -> Vec<Option<Note>> {
        (0..len)
            .map(|s| on.contains(&s).then(|| plain_step(s as u16)))
            .collect()
    }

    fn on(r: &[Option<Note>]) -> Vec<usize> {
        r.iter()
            .enumerate()
            .filter_map(|(s, n)| n.map(|_| s))
            .collect()
    }

    #[test]
    fn step_tools() {
        let r = row(&[0, 3], 8);
        assert_eq!(on(&fill_every(&r, 2, 0)), [0, 2, 3, 4, 6]);
        assert_eq!(on(&fill_every(&r, 3, 1)), [0, 1, 3, 4, 7]);
        assert_eq!(on(&rotate(&r, 6)), [1, 6]);
        assert_eq!(on(&rotate(&r, -1)), [2, 7]);
        assert_eq!(on(&shift(&r, 5)), [5]);
        assert_eq!(on(&shift(&r, -1)), [2]);
        // Moved notes start on their new step, keeping their shift and values.
        let mut r = row(&[1], 4);
        r[1].as_mut().unwrap().start += 30;
        r[1].as_mut().unwrap().velocity = 0.3;
        let m = rotate(&r, 2);
        assert_eq!(m[3].unwrap().start, step_tick(3) + 30);
        assert_eq!(m[3].unwrap().velocity, 0.3);
        let a = randomize(64, 0.25, 7);
        assert_eq!(a, randomize(64, 0.25, 7), "a seed gives the same row");
        assert_ne!(a, randomize(64, 0.25, 8));
        let n = on(&a).len();
        assert!((8..=24).contains(&n), "{n} of 64 at 25 %");
        assert!(on(&randomize(16, 0.0, 1)).is_empty());
        assert_eq!(on(&randomize(16, 1.0, 1)).len(), 16);
    }

    #[test]
    fn lanes_edit_note_fields_and_shift_keeps_the_step() {
        let mut p = Pattern {
            id: crate::PatternId(1),
            name: "P".into(),
            steps: 8,
            color: 0,
            notes: Default::default(),
            controls: Default::default(),
            automation: Vec::new(),
        };
        let ch = ChannelId(3);
        p.toggle_step(ch, 2);
        p.set_step_lane(ch, 2, StepLane::Shift, 500.0);
        let n = *p.step_note(ch, 2).expect("still step 2");
        assert_eq!(n.start, step_tick(2) + MAX_SHIFT);
        assert_eq!(StepLane::Shift.get(&n, step_tick(2)), MAX_SHIFT as f32);
        p.set_step_lane(ch, 2, StepLane::Pan, -3.0);
        p.set_step_lane(ch, 2, StepLane::Fine, 25.0);
        let n = *p.step_note(ch, 2).unwrap();
        assert_eq!((n.pan, n.fine), (-1.0, 25.0));
        assert!(!p.toggle_step(ch, 2), "a shifted step still toggles off");
        assert!(p.channel_notes(ch).is_empty());
    }

    #[test]
    fn velocity_split_picks_one_child() {
        assert_eq!(layer_candidates(3, false, 0.2), 0..3);
        assert_eq!(layer_candidates(3, true, 0.2), 0..1);
        assert_eq!(layer_candidates(3, true, 0.5), 1..2);
        assert_eq!(layer_candidates(3, true, 1.0), 2..3);
        assert_eq!(layer_candidates(0, true, 1.0), 0..0);
    }
}

#[cfg(test)]
mod project_tests {
    use crate::{ClipKind, Project};

    #[test]
    fn any_length_and_split_by_channel() {
        let mut p = Project::demo();
        let pid = p.current_pattern;
        let pat = p.current_pattern_mut();
        pat.steps = 12;
        let ticks = pat.length_ticks();
        assert!(ticks == 12 * crate::STEP_TICKS || ticks % (16 * crate::STEP_TICKS) == 0);
        pat.steps = 300;
        assert_eq!(pat.length_ticks(), 300 * crate::STEP_TICKS);
        let used = p
            .channels
            .iter()
            .filter(|c| !p.current_pattern().channel_notes(c.id).is_empty())
            .count();
        let clips_before = p
            .playlist
            .clips
            .iter()
            .filter(|c| c.kind == ClipKind::Pattern(pid))
            .count();
        assert!(used >= 2 && clips_before > 0);
        let parts = p.split_pattern_by_channel(pid);
        assert_eq!(parts.len(), used);
        for id in &parts {
            let part = p.pattern(*id).unwrap();
            assert_eq!(part.notes.len(), 1, "{}", part.name);
            assert_eq!(part.steps, 300);
            let n = p
                .playlist
                .clips
                .iter()
                .filter(|c| c.kind == ClipKind::Pattern(*id))
                .count();
            assert_eq!(n, clips_before);
        }
        assert!(!p
            .playlist
            .clips
            .iter()
            .any(|c| c.kind == ClipKind::Pattern(pid)));
        assert!(p.split_pattern_by_channel(parts[0]).is_empty());
    }
}
