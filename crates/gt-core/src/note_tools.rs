//! Note tools for the piano roll (Prompt 22): chord stamps, arpeggiate, strum, chop, flam,
//! glue, legato, flip, randomize and scale velocities, plus a chord-progression and bassline
//! generator. Each tool takes the selected notes and returns the notes that replace them, so
//! the roll can preview a tool by re-running it on the originals. Random choices come from
//! [`Rng`] and a seed, so a tool gives the same result every time for the same settings.

use crate::Note;

/// A small seeded generator (SplitMix64), the same on every platform.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// A generator for `seed`.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in 0..1.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in -1..1.
    pub fn bipolar(&mut self) -> f64 {
        self.unit() * 2.0 - 1.0
    }

    /// Uniform integer in `0..n` (0 when `n` is 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
}

/// True if `key` is in the scale with these intervals (semitones from the root) on `root`
/// (0 = C). An empty scale contains every key.
pub fn in_scale(intervals: &[u8], root: u8, key: i32) -> bool {
    intervals.is_empty() || intervals.contains(&((key - i32::from(root)).rem_euclid(12) as u8))
}

/// The key of the scale nearest to `key` (ties go down), kept in 0..=127.
pub fn nearest_in_scale(intervals: &[u8], root: u8, key: i32) -> i32 {
    let key = key.clamp(0, 127);
    for d in 0..12 {
        for k in [key - d, key + d] {
            if (0..128).contains(&k) && in_scale(intervals, root, k) {
                return k;
            }
        }
    }
    key
}

/// Chord shapes for the chord stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChordKind {
    /// Root, major third, fifth.
    Major,
    /// Root, minor third, fifth.
    Minor,
    /// Root, minor third, flat fifth.
    Diminished,
    /// Root, major third, sharp fifth.
    Augmented,
    /// Root, second, fifth.
    Sus2,
    /// Root, fourth, fifth.
    Sus4,
    /// Root and fifth.
    Power,
    /// Major with a major seventh.
    Major7,
    /// Minor with a minor seventh.
    Minor7,
    /// Major with a minor seventh.
    Dominant7,
    /// Diminished with a minor seventh.
    HalfDiminished7,
    /// Diminished with a diminished seventh.
    Diminished7,
    /// Major with a sixth.
    Major6,
    /// Minor with a sixth.
    Minor6,
    /// Major with a ninth, no seventh.
    Add9,
    /// Dominant seventh with a ninth.
    Dominant9,
    /// Major seventh with a ninth.
    Major9,
    /// Minor seventh with a ninth.
    Minor9,
}

impl ChordKind {
    /// Every chord, in menu order.
    pub const ALL: [ChordKind; 18] = [
        ChordKind::Major,
        ChordKind::Minor,
        ChordKind::Diminished,
        ChordKind::Augmented,
        ChordKind::Sus2,
        ChordKind::Sus4,
        ChordKind::Power,
        ChordKind::Major7,
        ChordKind::Minor7,
        ChordKind::Dominant7,
        ChordKind::HalfDiminished7,
        ChordKind::Diminished7,
        ChordKind::Major6,
        ChordKind::Minor6,
        ChordKind::Add9,
        ChordKind::Dominant9,
        ChordKind::Major9,
        ChordKind::Minor9,
    ];

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            ChordKind::Major => "Major",
            ChordKind::Minor => "Minor",
            ChordKind::Diminished => "Diminished",
            ChordKind::Augmented => "Augmented",
            ChordKind::Sus2 => "Sus2",
            ChordKind::Sus4 => "Sus4",
            ChordKind::Power => "Power (5)",
            ChordKind::Major7 => "Major 7",
            ChordKind::Minor7 => "Minor 7",
            ChordKind::Dominant7 => "Dominant 7",
            ChordKind::HalfDiminished7 => "Minor 7 flat 5",
            ChordKind::Diminished7 => "Diminished 7",
            ChordKind::Major6 => "Major 6",
            ChordKind::Minor6 => "Minor 6",
            ChordKind::Add9 => "Add 9",
            ChordKind::Dominant9 => "Dominant 9",
            ChordKind::Major9 => "Major 9",
            ChordKind::Minor9 => "Minor 9",
        }
    }

    /// Semitones above the root, lowest first.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            ChordKind::Major => &[0, 4, 7],
            ChordKind::Minor => &[0, 3, 7],
            ChordKind::Diminished => &[0, 3, 6],
            ChordKind::Augmented => &[0, 4, 8],
            ChordKind::Sus2 => &[0, 2, 7],
            ChordKind::Sus4 => &[0, 5, 7],
            ChordKind::Power => &[0, 7],
            ChordKind::Major7 => &[0, 4, 7, 11],
            ChordKind::Minor7 => &[0, 3, 7, 10],
            ChordKind::Dominant7 => &[0, 4, 7, 10],
            ChordKind::HalfDiminished7 => &[0, 3, 6, 10],
            ChordKind::Diminished7 => &[0, 3, 6, 9],
            ChordKind::Major6 => &[0, 4, 7, 9],
            ChordKind::Minor6 => &[0, 3, 7, 9],
            ChordKind::Add9 => &[0, 4, 7, 14],
            ChordKind::Dominant9 => &[0, 4, 7, 10, 14],
            ChordKind::Major9 => &[0, 4, 7, 11, 14],
            ChordKind::Minor9 => &[0, 3, 7, 10, 14],
        }
    }

    /// The keys of this chord on `root`, inverted `inversion` times (each inversion moves the
    /// lowest note up an octave), lowest first. Keys outside 0..=127 are dropped.
    pub fn keys(self, root: i32, inversion: usize) -> Vec<u8> {
        let mut k: Vec<i32> = self
            .intervals()
            .iter()
            .map(|&i| root + i32::from(i))
            .collect();
        for _ in 0..inversion % k.len().max(1) {
            let low = k.remove(0);
            k.push(low + 12);
        }
        k.into_iter()
            .filter(|k| (0..128).contains(k))
            .map(|k| k as u8)
            .collect()
    }
}

/// Copies of `template` (start, length, velocity and the rest) on each key of a chord.
pub fn chord_stamp(template: &Note, kind: ChordKind, inversion: usize) -> Vec<Note> {
    kind.keys(i32::from(template.key), inversion)
        .into_iter()
        .map(|key| Note { key, ..*template })
        .collect()
}

/// Groups notes that overlap in time (a chord, or a held note with others over it): each
/// group's notes are sorted by key. Groups come in time order.
fn overlap_groups(notes: &[Note]) -> Vec<Vec<Note>> {
    let mut sorted = notes.to_vec();
    sorted.sort_by_key(|n| (n.start, n.key));
    let mut groups: Vec<Vec<Note>> = Vec::new();
    let mut end = i64::MIN;
    for n in sorted {
        let e = n.start + n.length.max(1);
        match groups.last_mut() {
            Some(g) if n.start < end => {
                g.push(n);
                end = end.max(e);
            }
            _ => {
                groups.push(vec![n]);
                end = e;
            }
        }
    }
    for g in &mut groups {
        g.sort_by_key(|n| n.key);
    }
    groups
}

/// Groups notes that start on the same tick, each sorted by key, in time order.
fn start_groups(notes: &[Note]) -> Vec<Vec<Note>> {
    let mut sorted = notes.to_vec();
    sorted.sort_by_key(|n| (n.start, n.key));
    let mut groups: Vec<Vec<Note>> = Vec::new();
    for n in sorted {
        match groups.last_mut() {
            Some(g) if g[0].start == n.start => g.push(n),
            _ => groups.push(vec![n]),
        }
    }
    groups
}

/// Order of an arpeggio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArpPattern {
    /// Lowest to highest, then again.
    Up,
    /// Highest to lowest.
    Down,
    /// Up then down, without repeating the top and bottom notes.
    UpDown,
    /// A random key of the chord each step.
    Random,
}

impl ArpPattern {
    /// Every pattern, in menu order.
    pub const ALL: [ArpPattern; 4] = [
        ArpPattern::Up,
        ArpPattern::Down,
        ArpPattern::UpDown,
        ArpPattern::Random,
    ];

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            ArpPattern::Up => "Up",
            ArpPattern::Down => "Down",
            ArpPattern::UpDown => "Up and down",
            ArpPattern::Random => "Random",
        }
    }
}

/// Arpeggiator settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arp {
    /// Order of the keys.
    pub pattern: ArpPattern,
    /// Octaves the chord is spread over, 1 to 4.
    pub octaves: u8,
    /// Ticks between steps.
    pub rate: i64,
    /// Fraction of a step each note sounds, 0.05 to 1.
    pub gate: f32,
    /// Seed for the random pattern.
    pub seed: u64,
}

/// Plays each group of overlapping notes as an arpeggio over the group's span: a note every
/// `rate` ticks, walking the group's keys (spread over `octaves`) in `pattern` order. Each
/// step keeps the velocity and per-note values of the note its key came from.
pub fn arpeggiate(notes: &[Note], a: &Arp) -> Vec<Note> {
    let rate = a.rate.max(1);
    let gate = f64::from(a.gate.clamp(0.05, 1.0));
    let len = ((rate as f64 * gate).round() as i64).max(1);
    let mut rng = Rng::new(a.seed);
    let mut out = Vec::new();
    for g in overlap_groups(notes) {
        let from = g[0].start;
        let to = g
            .iter()
            .map(|n| n.start + n.length.max(1))
            .max()
            .unwrap_or(from + 1);
        let mut up: Vec<Note> = Vec::new();
        for o in 0..i32::from(a.octaves.clamp(1, 4)) {
            for n in &g {
                let key = i32::from(n.key) + 12 * o;
                if key < 128 && !up.iter().any(|u| i32::from(u.key) == key) {
                    up.push(Note {
                        key: key as u8,
                        ..*n
                    });
                }
            }
        }
        up.sort_by_key(|n| n.key);
        let order: Vec<Note> = match a.pattern {
            ArpPattern::Up | ArpPattern::Random => up.clone(),
            ArpPattern::Down => up.iter().rev().copied().collect(),
            ArpPattern::UpDown => {
                let mut o = up.clone();
                // With two keys up-down is just alternation.
                if up.len() > 2 {
                    o.extend(up[1..up.len() - 1].iter().rev());
                }
                o
            }
        };
        if order.is_empty() {
            continue;
        }
        let mut t = from;
        let mut i = 0;
        while t < to {
            let src = if a.pattern == ArpPattern::Random {
                order[rng.below(order.len())]
            } else {
                order[i % order.len()]
            };
            out.push(Note {
                start: t,
                length: len.min(to - t).max(1),
                ..src
            });
            t += rate;
            i += 1;
        }
    }
    out
}

/// Strum settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Strum {
    /// Ticks from the first note of a chord to the last.
    pub time: i64,
    /// Bends the spacing: 0 even, positive starts slowly and speeds up, negative the reverse
    /// (-1 to 1).
    pub curve: f32,
    /// Strum from the top key down instead of from the bottom up.
    pub down: bool,
    /// Velocity change from the first note to the last, -1 to 1 (negative fades out).
    pub velocity: f32,
}

/// Spreads the notes of each chord (notes starting together) over `time`; note ends stay
/// where they were.
pub fn strum(notes: &[Note], s: &Strum) -> Vec<Note> {
    let mut out = Vec::new();
    // The curve as an exponent on the position through the chord: 2^(2·curve).
    let power = f64::from(s.curve.clamp(-1.0, 1.0) * 2.0).exp2();
    for mut g in start_groups(notes) {
        if s.down {
            g.reverse();
        }
        let n = g.len();
        for (i, note) in g.into_iter().enumerate() {
            let x = if n > 1 {
                i as f64 / (n - 1) as f64
            } else {
                0.0
            };
            let offset = (s.time.max(0) as f64 * x.powf(power)).round() as i64;
            let offset = offset.min(note.length.max(1) - 1);
            let v = note.velocity * (1.0 + s.velocity.clamp(-1.0, 1.0) * x as f32);
            out.push(Note {
                start: note.start + offset,
                length: note.length.max(1) - offset,
                velocity: v.clamp(0.01, 1.0),
                ..note
            });
        }
    }
    out
}

/// Splits each note into repeats `step` ticks long (the last one takes what is left).
pub fn chop(notes: &[Note], step: i64) -> Vec<Note> {
    let step = step.max(1);
    let mut out = Vec::new();
    for n in notes {
        let end = n.start + n.length.max(1);
        let mut t = n.start;
        while t < end {
            out.push(Note {
                start: t,
                length: step.min(end - t),
                ..*n
            });
            t += step;
        }
    }
    out
}

/// Adds a grace note `offset` ticks before each note (at least at tick 0), `velocity` times
/// as loud, as long as the gap or the note, whichever is shorter.
pub fn flam(notes: &[Note], offset: i64, velocity: f32) -> Vec<Note> {
    let offset = offset.max(1);
    let mut out = Vec::with_capacity(notes.len() * 2);
    for n in notes {
        let start = (n.start - offset).max(0);
        if start < n.start {
            out.push(Note {
                start,
                length: (n.start - start).min(n.length.max(1)),
                velocity: (n.velocity * velocity.clamp(0.0, 1.0)).clamp(0.01, 1.0),
                slide: false,
                ..*n
            });
        }
        out.push(*n);
    }
    out
}

/// Joins notes of the same key that touch or overlap into one note (keeping the first's
/// values).
pub fn glue(notes: &[Note]) -> Vec<Note> {
    let mut sorted = notes.to_vec();
    sorted.sort_by_key(|n| (n.key, n.start));
    let mut out: Vec<Note> = Vec::new();
    for n in sorted {
        match out.last_mut() {
            Some(p) if p.key == n.key && n.start <= p.start + p.length => {
                let end = (p.start + p.length).max(n.start + n.length);
                p.length = end - p.start;
            }
            _ => out.push(n),
        }
    }
    out.sort_by_key(|n| (n.start, n.key));
    out
}

/// Makes each note last until the next later start among the notes; notes at the last
/// start keep their length.
pub fn legato(notes: &[Note]) -> Vec<Note> {
    let mut starts: Vec<i64> = notes.iter().map(|n| n.start).collect();
    starts.sort_unstable();
    starts.dedup();
    notes
        .iter()
        .map(|n| {
            let i = starts.partition_point(|&s| s <= n.start);
            match starts.get(i) {
                Some(&next) => Note {
                    length: next - n.start,
                    ..*n
                },
                None => *n,
            }
        })
        .collect()
}

/// Mirrors the notes in time around the middle of their span (`horizontal`) and/or in pitch
/// around the middle of their key range (`vertical`).
pub fn flip(notes: &[Note], horizontal: bool, vertical: bool) -> Vec<Note> {
    let from = notes.iter().map(|n| n.start).min().unwrap_or(0);
    let to = notes
        .iter()
        .map(|n| n.start + n.length.max(1))
        .max()
        .unwrap_or(0);
    let lo = notes.iter().map(|n| i32::from(n.key)).min().unwrap_or(0);
    let hi = notes.iter().map(|n| i32::from(n.key)).max().unwrap_or(0);
    notes
        .iter()
        .map(|n| {
            let mut m = *n;
            if horizontal {
                m.start = from + to - (n.start + n.length.max(1));
            }
            if vertical {
                m.key = (lo + hi - i32::from(n.key)).clamp(0, 127) as u8;
            }
            m
        })
        .collect()
}

/// Randomize settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Randomize {
    /// Seed: the same seed gives the same result.
    pub seed: u64,
    /// Starts move by up to this many ticks either way (never before tick 0).
    pub timing: i64,
    /// Velocities move by up to this much either way (0 to 1).
    pub velocity: f32,
    /// Keys move by up to this many semitones either way, onto the scale.
    pub pitch: i32,
}

/// Moves starts, velocities and keys by random amounts, keys landing on the scale given by
/// `intervals` on `root` (an empty scale allows every key).
pub fn randomize(notes: &[Note], r: &Randomize, intervals: &[u8], root: u8) -> Vec<Note> {
    let mut rng = Rng::new(r.seed);
    notes
        .iter()
        .map(|n| {
            // Three draws per note, always, so one setting does not shift the others.
            let (dt, dv, dk) = (rng.bipolar(), rng.bipolar(), rng.bipolar());
            let mut m = *n;
            m.start = (n.start + (dt * r.timing.max(0) as f64).round() as i64).max(0);
            m.velocity = (n.velocity + dv as f32 * r.velocity.clamp(0.0, 1.0)).clamp(0.01, 1.0);
            if r.pitch > 0 {
                let k = i32::from(n.key) + (dk * f64::from(r.pitch)).round() as i32;
                m.key = nearest_in_scale(intervals, root, k) as u8;
            }
            m
        })
        .collect()
}

/// Scales velocities by `scale` and adds `add` (both applied, then clamped to 0.01..1).
pub fn scale_velocity(notes: &[Note], scale: f32, add: f32) -> Vec<Note> {
    notes
        .iter()
        .map(|n| Note {
            velocity: (n.velocity * scale.max(0.0) + add).clamp(0.01, 1.0),
            ..*n
        })
        .collect()
}

/// What the generator writes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Generator {
    /// Scale root, 0 = C.
    pub root: u8,
    /// Style seed: picks the progression, the rhythm and the bass pattern.
    pub seed: u64,
    /// Bars to fill (one chord per bar).
    pub bars: u32,
    /// Ticks per bar.
    pub bar_ticks: i64,
    /// Write chords.
    pub chords: bool,
    /// Write a bassline.
    pub bass: bool,
    /// Octave of the chords (5 puts C5 = key 60 in the middle).
    pub octave: u8,
}

/// Progressions as scale degrees (0 = the root), each four bars long.
const PROGRESSIONS: [[usize; 4]; 10] = [
    [0, 4, 5, 3],
    [0, 5, 3, 4],
    [5, 3, 0, 4],
    [0, 3, 4, 3],
    [1, 4, 0, 0],
    [0, 3, 5, 4],
    [0, 5, 2, 6],
    [0, 6, 5, 6],
    [3, 4, 2, 5],
    [0, 2, 3, 4],
];

/// Chord rhythms: (start, length) in sixteenths of a 4/4 bar.
const CHORD_RHYTHMS: [&[(i64, i64)]; 4] = [
    &[(0, 16)],
    &[(0, 8), (8, 8)],
    &[(0, 6), (6, 6), (12, 4)],
    &[(0, 3), (4, 3), (8, 3), (12, 3)],
];

/// Bass patterns: (start, length in sixteenths, scale steps above the chord root; 7 is an
/// octave in a seven-note scale and is taken as +12 semitones).
const BASS_PATTERNS: [&[(i64, i64, i32)]; 5] = [
    &[(0, 16, 0)],
    &[(0, 4, 0), (4, 4, 0), (8, 4, 0), (12, 4, 0)],
    &[(0, 4, 0), (4, 4, 4), (8, 4, 0), (12, 4, 4)],
    &[
        (0, 2, 0),
        (2, 2, 7),
        (4, 2, 0),
        (6, 2, 7),
        (8, 2, 0),
        (10, 2, 7),
        (12, 2, 0),
        (14, 2, 7),
    ],
    &[(0, 6, 0), (6, 2, 0), (8, 4, 4), (12, 2, 2), (14, 2, 4)],
];

/// The key `steps` scale degrees above `degree`'s key in octave `octave` (degrees wrap into
/// higher octaves).
fn degree_key(intervals: &[u8], root: u8, octave: i32, degree: i32) -> i32 {
    let n = intervals.len().max(1) as i32;
    let o = degree.div_euclid(n);
    let d = degree.rem_euclid(n) as usize;
    let semis = intervals.get(d).copied().unwrap_or(0);
    12 * (octave + o) + i32::from(root) + i32::from(semis)
}

/// Writes a chord progression and/or a bassline in the scale with `intervals` (semitones from
/// the root, at least 3 keys for chords) from a seed, using fixed rules: a four-chord
/// progression from a table, triads stacked in thirds of the scale, each voiced as close as
/// possible to the one before, and a rhythm and bass pattern from small tables.
pub fn generate(g: &Generator, intervals: &[u8]) -> Vec<Note> {
    let intervals: &[u8] = if intervals.len() >= 3 {
        intervals
    } else {
        &[0, 2, 4, 5, 7, 9, 11]
    };
    let mut rng = Rng::new(g.seed);
    let prog = PROGRESSIONS[rng.below(PROGRESSIONS.len())];
    let rhythm = CHORD_RHYTHMS[rng.below(CHORD_RHYTHMS.len())];
    let bass = BASS_PATTERNS[rng.below(BASS_PATTERNS.len())];
    let sixteenth = (g.bar_ticks / 16).max(1);
    let octave = i32::from(g.octave.clamp(2, 8));
    let n = intervals.len() as i32;
    let mut out = Vec::new();
    let mut last: Option<Vec<i32>> = None;
    for bar in 0..g.bars as i64 {
        let degree = (prog[bar as usize % 4] as i32).min(n - 1);
        let t0 = bar * g.bar_ticks;
        if g.chords {
            let triad: Vec<i32> = (0..3)
                .map(|k| degree_key(intervals, g.root, octave, degree + 2 * k))
                .collect();
            // Voice leading: of the three inversions (and an octave down), the one whose keys
            // are nearest the previous chord's.
            let mut best = triad.clone();
            if let Some(prev) = &last {
                let centre: f32 = prev.iter().sum::<i32>() as f32 / prev.len() as f32;
                let mut candidates = Vec::new();
                for inv in 0..3 {
                    let mut c = triad.clone();
                    for k in c.iter_mut().take(inv) {
                        *k += 12;
                    }
                    candidates.push(c.clone());
                    candidates.push(c.iter().map(|k| k - 12).collect());
                }
                best = candidates
                    .into_iter()
                    .min_by(|a, b| {
                        let d = |c: &Vec<i32>| {
                            (c.iter().sum::<i32>() as f32 / c.len() as f32 - centre).abs()
                        };
                        d(a).total_cmp(&d(b))
                    })
                    .unwrap_or(best);
            }
            for &(s, l) in rhythm {
                for &k in &best {
                    if (0..128).contains(&k) {
                        out.push(Note::new(t0 + s * sixteenth, l * sixteenth, k as u8, 0.72));
                    }
                }
            }
            last = Some(best);
        }
        if g.bass {
            for &(s, l, step) in bass {
                let key = if step == 7 {
                    degree_key(intervals, g.root, octave - 2, degree) + 12
                } else {
                    degree_key(intervals, g.root, octave - 2, degree + step)
                };
                if (0..128).contains(&key) {
                    out.push(Note::new(
                        t0 + s * sixteenth,
                        l * sixteenth,
                        key as u8,
                        0.85,
                    ));
                }
            }
        }
    }
    out.sort_by_key(|n| (n.start, n.key));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(start: i64, length: i64, key: u8) -> Note {
        Note::new(start, length, key, 0.8)
    }

    fn sk(v: &[Note]) -> Vec<(i64, i64, u8)> {
        v.iter().map(|n| (n.start, n.length, n.key)).collect()
    }

    const MAJOR: &[u8] = &[0, 2, 4, 5, 7, 9, 11];

    #[test]
    fn chords_and_inversions() {
        assert_eq!(ChordKind::Major.keys(60, 0), [60, 64, 67]);
        assert_eq!(ChordKind::Major.keys(60, 1), [64, 67, 72]);
        assert_eq!(ChordKind::Major.keys(60, 2), [67, 72, 76]);
        assert_eq!(ChordKind::Major.keys(60, 3), [60, 64, 67]);
        assert_eq!(ChordKind::Minor7.keys(57, 0), [57, 60, 64, 67]);
        assert_eq!(ChordKind::Major.keys(125, 0), [125]);
        let s = chord_stamp(&n(480, 240, 62), ChordKind::Minor, 0);
        assert_eq!(sk(&s), [(480, 240, 62), (480, 240, 65), (480, 240, 69)]);
    }

    #[test]
    fn arpeggio_patterns() {
        // C major triad held for a bar, sixteenth steps, half gate.
        let chord = chord_stamp(&n(0, 3840, 60), ChordKind::Major, 0);
        let a = |pattern, octaves| Arp {
            pattern,
            octaves,
            rate: 240,
            gate: 0.5,
            seed: 1,
        };
        let up = arpeggiate(&chord, &a(ArpPattern::Up, 1));
        assert_eq!(up.len(), 16);
        assert_eq!(
            up.iter().take(4).map(|n| n.key).collect::<Vec<_>>(),
            [60, 64, 67, 60]
        );
        assert!(up.iter().all(|n| n.length == 120));
        assert_eq!(up[15].start, 3600);
        let down = arpeggiate(&chord, &a(ArpPattern::Down, 2));
        assert_eq!(
            down.iter().take(7).map(|n| n.key).collect::<Vec<_>>(),
            [79, 76, 72, 67, 64, 60, 79]
        );
        let ud = arpeggiate(&chord, &a(ArpPattern::UpDown, 1));
        assert_eq!(
            ud.iter().take(6).map(|n| n.key).collect::<Vec<_>>(),
            [60, 64, 67, 64, 60, 64]
        );
        let r1 = arpeggiate(&chord, &a(ArpPattern::Random, 1));
        let r2 = arpeggiate(&chord, &a(ArpPattern::Random, 1));
        assert_eq!(r1, r2);
        assert!(r1.iter().all(|n| [60, 64, 67].contains(&n.key)));
        // Two chords one after the other arpeggiate separately.
        let mut two = chord_stamp(&n(0, 480, 60), ChordKind::Major, 0);
        two.extend(chord_stamp(&n(480, 480, 62), ChordKind::Minor, 0));
        let arp = arpeggiate(&two, &a(ArpPattern::Up, 1));
        assert_eq!(
            arp.iter().map(|n| (n.start, n.key)).collect::<Vec<_>>(),
            [(0, 60), (240, 64), (480, 62), (720, 65)]
        );
    }

    #[test]
    fn strum_spreads_a_chord_and_keeps_its_end() {
        let chord = chord_stamp(&n(0, 960, 60), ChordKind::Major, 0);
        let s = strum(
            &chord,
            &Strum {
                time: 120,
                curve: 0.0,
                down: false,
                velocity: -0.5,
            },
        );
        assert_eq!(sk(&s), [(0, 960, 60), (60, 900, 64), (120, 840, 67)]);
        assert!((s[2].velocity - 0.4).abs() < 1e-6);
        let d = strum(
            &chord,
            &Strum {
                time: 120,
                curve: 0.0,
                down: true,
                velocity: 0.0,
            },
        );
        assert_eq!(
            d.iter().map(|n| (n.key, n.start)).collect::<Vec<_>>(),
            [(67, 0), (64, 60), (60, 120)]
        );
        // A positive curve starts slowly: the middle note is less than half way.
        let c = strum(
            &chord,
            &Strum {
                time: 120,
                curve: 0.5,
                down: false,
                velocity: 0.0,
            },
        );
        assert_eq!(c[1].start, 30);
    }

    #[test]
    fn chop_flam_glue_and_legato() {
        assert_eq!(
            sk(&chop(&[n(0, 500, 60)], 240)),
            [(0, 240, 60), (240, 240, 60), (480, 20, 60)]
        );
        let f = flam(&[n(480, 240, 60), n(10, 240, 62)], 30, 0.5);
        assert_eq!(
            sk(&f),
            [(450, 30, 60), (480, 240, 60), (0, 10, 62), (10, 240, 62)]
        );
        assert!((f[0].velocity - 0.4).abs() < 1e-6);
        let g = glue(&[
            n(0, 240, 60),
            n(240, 240, 60),
            n(400, 200, 60),
            n(700, 10, 60),
            n(0, 100, 62),
        ]);
        assert_eq!(sk(&g), [(0, 600, 60), (0, 100, 62), (700, 10, 60)]);
        let l = legato(&[n(0, 10, 60), n(0, 10, 64), n(480, 10, 62), n(960, 50, 65)]);
        assert_eq!(
            sk(&l),
            [(0, 480, 60), (0, 480, 64), (480, 480, 62), (960, 50, 65)]
        );
    }

    #[test]
    fn flip_mirrors_time_and_pitch() {
        let notes = [n(0, 240, 60), n(480, 480, 67)];
        let h = flip(&notes, true, false);
        assert_eq!(sk(&h), [(720, 240, 60), (0, 480, 67)]);
        let v = flip(&notes, false, true);
        assert_eq!(sk(&v), [(0, 240, 67), (480, 480, 60)]);
        // Flipping twice gives the original back.
        assert_eq!(flip(&flip(&notes, true, true), true, true), notes);
    }

    #[test]
    fn randomize_is_deterministic_and_stays_in_the_scale() {
        let notes: Vec<Note> = (0..32)
            .map(|i| n(960 + i * 240, 120, 60 + (i % 12) as u8))
            .collect();
        let r = Randomize {
            seed: 42,
            timing: 30,
            velocity: 0.2,
            pitch: 3,
        };
        let a = randomize(&notes, &r, MAJOR, 0);
        assert_eq!(a, randomize(&notes, &r, MAJOR, 0));
        assert_ne!(a, randomize(&notes, &Randomize { seed: 43, ..r }, MAJOR, 0));
        for (x, y) in a.iter().zip(&notes) {
            assert!((x.start - y.start).abs() <= 30);
            assert!((x.velocity - y.velocity).abs() <= 0.2 + 1e-6);
            assert!(in_scale(MAJOR, 0, i32::from(x.key)), "{}", x.key);
            assert!((i32::from(x.key) - i32::from(y.key)).abs() <= 4);
        }
        assert!(a.iter().zip(&notes).any(|(x, y)| x.start != y.start));
        // Nothing to randomize: unchanged.
        let none = Randomize {
            seed: 1,
            timing: 0,
            velocity: 0.0,
            pitch: 0,
        };
        assert_eq!(randomize(&notes, &none, MAJOR, 0), notes);
    }

    #[test]
    fn velocities_scale_and_clamp() {
        let v = scale_velocity(&[n(0, 1, 60), Note::new(0, 1, 61, 0.2)], 1.5, 0.1);
        assert_eq!(v[0].velocity, 1.0);
        assert!((v[1].velocity - 0.4).abs() < 1e-6);
        assert_eq!(scale_velocity(&[n(0, 1, 60)], 0.0, 0.0)[0].velocity, 0.01);
    }

    #[test]
    fn generator_follows_its_rules() {
        let g = Generator {
            root: 9, // A
            seed: 7,
            bars: 8,
            bar_ticks: 3840,
            chords: true,
            bass: true,
            octave: 5,
        };
        let minor: &[u8] = &[0, 2, 3, 5, 7, 8, 10];
        let a = generate(&g, minor);
        assert_eq!(a, generate(&g, minor));
        assert!(!a.is_empty());
        // Every key in A minor, everything inside the 8 bars.
        assert!(a.iter().all(|n| in_scale(minor, 9, i32::from(n.key))));
        assert!(a
            .iter()
            .all(|n| n.start >= 0 && n.start + n.length <= 8 * 3840));
        // Bass notes sit two octaves below the chords.
        let lowest_chord = a
            .iter()
            .filter(|n| n.key >= 48)
            .map(|n| n.key)
            .min()
            .unwrap();
        assert!(a.iter().any(|n| n.key < 48) && lowest_chord >= 48);
        // Bar 1 starts on the tonic chord or the progression's first degree, as a triad.
        let first: Vec<u8> = a
            .iter()
            .filter(|n| n.start == 0 && n.key >= 48)
            .map(|n| n.key)
            .collect();
        assert_eq!(first.len(), 3);
        // Different seeds give different music, and chords-only has no bass.
        let others: Vec<Vec<Note>> = (0..6)
            .map(|s| generate(&Generator { seed: s, ..g }, minor))
            .collect();
        assert!(others.iter().any(|o| *o != a));
        let chords = generate(&Generator { bass: false, ..g }, minor);
        assert!(chords.iter().all(|n| n.key >= 48));
    }

    #[test]
    fn nearest_scale_key() {
        assert_eq!(nearest_in_scale(MAJOR, 0, 61), 60);
        assert_eq!(nearest_in_scale(MAJOR, 0, 63), 62);
        assert_eq!(nearest_in_scale(&[], 0, 63), 63);
        assert_eq!(nearest_in_scale(MAJOR, 0, 200), 127);
    }
}
