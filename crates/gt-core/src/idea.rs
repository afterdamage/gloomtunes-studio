//! "Start from an idea" (Prompt 34, D155): builds a starter song from a tempo, a key and a
//! style with our own rules and factory sounds, offline. A style picks a drum synth kit, three
//! synth presets, drum grids and bass, chord and lead rhythms; the key and a seed pick the
//! chord progression and the lead motif. The same idea always builds the same song.

use crate::demos::pad::{CLAP, HAT, KICK, OPEN, PERC, SNARE};
use crate::demos::Song;
use crate::note_tools::Rng;
use crate::project::{Note, Project, STEP_TICKS};

/// Steps per bar (1/16 notes).
const BAR_STEPS: i64 = 16;
/// Bars per pattern: one pass of the chord progression.
const BARS: i64 = 4;
/// Major and natural minor, in semitones from the root.
const MAJOR: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];
const MINOR: [u8; 7] = [0, 2, 3, 5, 7, 8, 10];
/// Progressions as scale degrees (0 = the tonic), one chord per bar.
const MAJOR_PROGRESSIONS: [[usize; 4]; 5] = [
    [0, 4, 5, 3],
    [0, 5, 3, 4],
    [5, 3, 0, 4],
    [0, 3, 5, 4],
    [0, 3, 0, 4],
];
const MINOR_PROGRESSIONS: [[usize; 4]; 5] = [
    [0, 5, 2, 6],
    [0, 3, 4, 0],
    [0, 6, 5, 6],
    [0, 3, 5, 4],
    [0, 5, 3, 4],
];
/// Key names, C first.
pub const KEY_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// A musical style the rules know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    /// Four on the floor, off-beat hats, chord stabs.
    #[default]
    House,
    /// Driving kick, rolling bass, sparse stabs.
    Techno,
    /// Boom bap: swung drums, two chords a bar.
    HipHop,
    /// Half-time snare, fast hats, long sub bass.
    Trap,
    /// Two-step breakbeat at 174.
    DrumAndBass,
    /// Soft pulse, long chords, a sparse lead.
    Ambient,
    /// Straight 80s drums, octave bass, pads.
    Synthwave,
    /// Lazy swung drums and keys.
    LoFi,
}

/// What a style plays: sounds, drum grids and rhythms.
struct Rules {
    tempo: f32,
    minor: bool,
    swing: f32,
    kit: &'static str,
    bass: &'static str,
    chords: &'static str,
    lead: &'static str,
    /// One bar per pad: kick, snare, closed hat, open hat, clap, perc. `x` hard, `o` soft.
    drums: [&'static str; 6],
    /// Bass notes in a bar: (step, length in steps, octave up).
    bass_rhythm: &'static [(i64, i64, bool)],
    /// Chords in a bar: (step, length in steps).
    chord_rhythm: &'static [(i64, i64)],
    /// Lead notes per bar.
    lead_notes: usize,
    /// Longest lead note, in steps.
    lead_max: i64,
}

impl Style {
    /// Every style, in menu order.
    pub const ALL: [Style; 8] = [
        Style::House,
        Style::Techno,
        Style::HipHop,
        Style::Trap,
        Style::DrumAndBass,
        Style::Ambient,
        Style::Synthwave,
        Style::LoFi,
    ];

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            Style::House => "House",
            Style::Techno => "Techno",
            Style::HipHop => "Hip hop",
            Style::Trap => "Trap",
            Style::DrumAndBass => "Drum and bass",
            Style::Ambient => "Ambient",
            Style::Synthwave => "Synthwave",
            Style::LoFi => "Lo-fi",
        }
    }

    /// The style's usual tempo.
    pub fn tempo(self) -> f32 {
        self.rules().tempo
    }

    /// True when the style usually sits in a minor key.
    pub fn minor(self) -> bool {
        self.rules().minor
    }

    fn rules(self) -> Rules {
        match self {
            Style::House => Rules {
                tempo: 124.0,
                minor: true,
                swing: 0.1,
                kit: "Deep House",
                bass: "Gloom Bass",
                chords: "FM Electric Piano",
                lead: "Glass Pluck",
                drums: [
                    "x...x...x...x...",
                    "................",
                    ".o.o.o.o.o.o.o.o",
                    "..x...x...x...x.",
                    "....x.......x...",
                    "..........o.....",
                ],
                bass_rhythm: &[(2, 2, false), (6, 2, false), (10, 2, false), (14, 1, true)],
                chord_rhythm: &[(3, 2), (6, 2), (11, 3)],
                lead_notes: 5,
                lead_max: 4,
            },
            Style::Techno => Rules {
                tempo: 132.0,
                minor: true,
                swing: 0.0,
                kit: "Hard Techno",
                bass: "Acid Line",
                chords: "Dark Pad",
                lead: "WT Sync Lead",
                drums: [
                    "x...x...x...x...",
                    "................",
                    "oo.ooo.ooo.ooo.o",
                    "..x...x...x...x.",
                    "....x.......x...",
                    "...o.....o...o..",
                ],
                bass_rhythm: &[
                    (1, 1, false),
                    (2, 1, false),
                    (3, 1, true),
                    (5, 1, false),
                    (6, 1, false),
                    (7, 1, false),
                    (9, 1, false),
                    (10, 1, true),
                    (11, 1, false),
                    (13, 1, false),
                    (14, 1, false),
                    (15, 1, false),
                ],
                chord_rhythm: &[(2, 1), (10, 1)],
                lead_notes: 4,
                lead_max: 2,
            },
            Style::HipHop => Rules {
                tempo: 90.0,
                minor: true,
                swing: 0.25,
                kit: "Boom Bap Synth",
                bass: "Gloom Bass",
                chords: "FM Electric Piano",
                lead: "FM Bell",
                drums: [
                    "x......x..x.....",
                    "....x.......x...",
                    "x.o.x.o.x.o.x.o.",
                    "..............o.",
                    "................",
                    ".........o......",
                ],
                bass_rhythm: &[(0, 3, false), (3, 1, false), (7, 2, false), (10, 4, false)],
                chord_rhythm: &[(0, 8), (8, 8)],
                lead_notes: 4,
                lead_max: 4,
            },
            Style::Trap => Rules {
                tempo: 140.0,
                minor: true,
                swing: 0.0,
                kit: "Trap Night",
                bass: "FM Sub Thump",
                chords: "Dark Pad",
                lead: "FM Bell",
                drums: [
                    "x......x..x.....",
                    "........x.......",
                    "x.x.x.x.x.xxx.x.",
                    "................",
                    "........x.......",
                    "...........o..o.",
                ],
                bass_rhythm: &[(0, 6, false), (7, 3, false), (10, 6, false)],
                chord_rhythm: &[(0, 16)],
                lead_notes: 5,
                lead_max: 3,
            },
            Style::DrumAndBass => Rules {
                tempo: 174.0,
                minor: true,
                swing: 0.0,
                kit: "Electro Snap",
                bass: "WT Reese",
                chords: "Glass Pad",
                lead: "Ember Lead",
                drums: [
                    "x.........x.....",
                    "....x.......x...",
                    "o.o.o.o.o.o.o.o.",
                    "......o.........",
                    "................",
                    ".......o.......o",
                ],
                bass_rhythm: &[(0, 6, false), (10, 2, false), (12, 4, false)],
                chord_rhythm: &[(0, 16)],
                lead_notes: 6,
                lead_max: 4,
            },
            Style::Ambient => Rules {
                tempo: 80.0,
                minor: false,
                swing: 0.0,
                kit: "Dub Wide",
                bass: "WT Soft Sine Sub",
                chords: "Glass Pad",
                lead: "Night Wind",
                drums: [
                    "x.........x.....",
                    "................",
                    "....o.......o...",
                    "................",
                    "........o.......",
                    "..o.....o.....o.",
                ],
                bass_rhythm: &[(0, 16, false)],
                chord_rhythm: &[(0, 16)],
                lead_notes: 2,
                lead_max: 8,
            },
            Style::Synthwave => Rules {
                tempo: 100.0,
                minor: true,
                swing: 0.0,
                kit: "Disco Tight",
                bass: "WT Sweep Bass",
                chords: "WT PWM Strings",
                lead: "Ember Lead",
                drums: [
                    "x...x...x...x...",
                    "....x.......x...",
                    "o.o.o.o.o.o.o.o.",
                    "................",
                    "....x.......x...",
                    "................",
                ],
                bass_rhythm: &[
                    (0, 2, false),
                    (2, 2, true),
                    (4, 2, false),
                    (6, 2, true),
                    (8, 2, false),
                    (10, 2, true),
                    (12, 2, false),
                    (14, 2, true),
                ],
                chord_rhythm: &[(0, 16)],
                lead_notes: 6,
                lead_max: 4,
            },
            Style::LoFi => Rules {
                tempo: 82.0,
                minor: false,
                swing: 0.35,
                kit: "Lo-Fi Dust",
                bass: "Gloom Bass",
                chords: "FM Electric Piano",
                lead: "Glass Pluck",
                drums: [
                    "x......o..x.....",
                    "....x.......x...",
                    "o.o.o.o.o.o.o.o.",
                    "................",
                    "................",
                    "......o.......o.",
                ],
                bass_rhythm: &[(0, 4, false), (7, 2, false), (10, 4, false)],
                chord_rhythm: &[(0, 7), (7, 9)],
                lead_notes: 3,
                lead_max: 6,
            },
        }
    }
}

/// The dialog's settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Idea {
    /// Beats per minute, 40 to 240.
    pub tempo: f32,
    /// Key root, 0 (C) to 11 (B).
    pub root: u8,
    /// Minor (true) or major.
    pub minor: bool,
    /// Drums, sounds and rhythms.
    pub style: Style,
    /// Picks the progression and the lead motif.
    pub seed: u64,
}

impl Default for Idea {
    fn default() -> Self {
        Self::for_style(Style::default())
    }
}

impl Idea {
    /// The style's usual tempo and mode, in A, seed 1.
    pub fn for_style(style: Style) -> Self {
        Self {
            tempo: style.tempo(),
            root: 9,
            minor: style.minor(),
            style,
            seed: 1,
        }
    }

    /// "A minor", "F# major".
    pub fn key_name(&self) -> String {
        let mode = if self.minor { "minor" } else { "major" };
        format!("{} {mode}", KEY_NAMES[usize::from(self.root % 12)])
    }

    /// The scale, in semitones from the root.
    pub fn scale(&self) -> &'static [u8; 7] {
        if self.minor {
            &MINOR
        } else {
            &MAJOR
        }
    }

    /// The four chords as scale degrees.
    pub fn progression(&self) -> [usize; 4] {
        let list = if self.minor {
            &MINOR_PROGRESSIONS
        } else {
            &MAJOR_PROGRESSIONS
        };
        list[Rng::new(self.seed).below(list.len())]
    }

    /// Key of scale degree `degree` (any number; 7 is the root an octave up) above `base`,
    /// the root's key in the wanted octave.
    fn key_of(&self, base: i32, degree: i32) -> i32 {
        let (oct, d) = (degree.div_euclid(7), degree.rem_euclid(7) as usize);
        base + 12 * oct + i32::from(self.scale()[d])
    }
}

/// Builds the song: drums, bass, chords and lead channels, five four-bar patterns (Drums, Hats,
/// Bass, Chords, Lead) and an arrangement of them (intro, verse, break, drop, outro) under
/// markers.
pub fn build(idea: &Idea) -> Project {
    let idea = Idea {
        tempo: if idea.tempo.is_finite() {
            idea.tempo.clamp(40.0, 240.0)
        } else {
            120.0
        },
        root: idea.root % 12,
        ..*idea
    };
    let r = idea.style.rules();
    let title = format!("{} idea in {}", idea.style.name(), idea.key_name());
    let mut s = Song::new(&title, idea.style.name(), f64::from(idea.tempo), r.swing);
    s.p.info.notes = format!(
        "Built by Start from an idea: {} BPM, {}, seed {}.",
        idea.tempo,
        idea.key_name(),
        idea.seed
    );
    let mut rng = Rng::new(idea.seed ^ 0x1DEA);
    let prog = idea.progression();

    let drums = s.drums(r.kit);
    let bass = s.synth(r.bass, 0.75);
    let chords = s.synth(r.chords, 0.5);
    let lead = s.synth(r.lead, 0.5);
    s.sends(chords, 0.3, 0.0);
    s.sends(lead, 0.25, 0.2);

    let p_drums = s.pattern("Drums", BARS as u16);
    let p_hats = s.pattern("Hats", BARS as u16);
    let p_bass = s.pattern("Bass", BARS as u16);
    let p_chords = s.pattern("Chords", BARS as u16);
    let p_lead = s.pattern("Lead", BARS as u16);

    let pads = [KICK, SNARE, HAT, OPEN, CLAP, PERC];
    for bar in 0..BARS {
        let last = bar == BARS - 1;
        for (row, (&key, grid)) in pads.iter().zip(r.drums).enumerate() {
            // The last bar's snare (or clap) ends on a fill.
            let fill = last && row == if r.drums[1].contains('x') { 1 } else { 4 };
            let hits = drum_bar(grid, key, bar, fill, &mut rng);
            let pat = if key == KICK || key == SNARE || key == CLAP {
                vec![p_drums]
            } else {
                vec![p_drums, p_hats]
            };
            for p in pat {
                s.add(p, drums, hits.clone());
            }
        }
    }

    let bass_base = 36 + i32::from(idea.root);
    let chord_notes: Vec<Note> = (0..BARS)
        .flat_map(|bar| {
            let tones = chord_keys(&idea, prog[bar as usize]);
            r.chord_rhythm.iter().flat_map(move |&(at, len)| {
                tones.clone().into_iter().map(move |k| {
                    Note::new(
                        (bar * BAR_STEPS + at) * STEP_TICKS,
                        len * STEP_TICKS,
                        k,
                        0.7,
                    )
                })
            })
        })
        .collect();
    s.add(p_chords, chords, chord_notes);
    let bass_notes: Vec<Note> = (0..BARS)
        .flat_map(|bar| {
            let root = idea.key_of(bass_base, prog[bar as usize] as i32);
            // Keep the bass in the bottom octave of its range.
            let root = if root >= bass_base + 12 {
                root - 12
            } else {
                root
            };
            r.bass_rhythm.iter().map(move |&(at, len, up)| {
                let k = root + if up { 12 } else { 0 };
                Note::new(
                    (bar * BAR_STEPS + at) * STEP_TICKS,
                    len * STEP_TICKS,
                    k.clamp(0, 127) as u8,
                    if at == 0 { 0.9 } else { 0.75 },
                )
            })
        })
        .collect();
    s.add(p_bass, bass, bass_notes);
    let lead_notes = lead_line(&idea, &r, prog, &mut rng);
    s.add(p_lead, lead, lead_notes);

    s.section("Intro", 4, &[p_hats, p_chords]);
    s.section("Verse", 8, &[p_drums, p_bass, p_chords]);
    s.section("Break", 4, &[p_chords, p_lead]);
    s.section("Drop", 8, &[p_drums, p_bass, p_chords, p_lead]);
    s.section("Outro", 4, &[p_hats, p_chords]);
    let mut p = s.finish();
    p.current_pattern = p_drums;
    p
}

/// One bar of hits of pad `key` from a 16-step grid, with a little seeded velocity play on
/// the soft hits; `fill` adds a rising run over the last four steps.
fn drum_bar(grid: &str, key: u8, bar: i64, fill: bool, rng: &mut Rng) -> Vec<Note> {
    let mut out: Vec<Note> = grid
        .chars()
        .enumerate()
        .filter_map(|(i, c)| {
            let v = match c {
                'x' => 0.9,
                'o' => 0.55 + 0.1 * rng.bipolar() as f32,
                _ => return None,
            };
            Some((i as i64, v))
        })
        .filter(|&(i, _)| !fill || i < 12)
        .map(|(i, v)| Note::new((bar * BAR_STEPS + i) * STEP_TICKS, STEP_TICKS, key, v))
        .collect();
    if fill {
        out.extend((12..16).map(|i| {
            let v = 0.5 + 0.12 * (i - 12) as f32;
            Note::new((bar * BAR_STEPS + i) * STEP_TICKS, STEP_TICKS, key, v)
        }));
    }
    out
}

/// A triad on scale degree `degree`, voiced close between A3 and G#4 so chords move smoothly.
fn chord_keys(idea: &Idea, degree: usize) -> Vec<u8> {
    let mut keys: Vec<u8> = [0, 2, 4]
        .iter()
        .map(|&third| {
            let pc = idea.key_of(i32::from(idea.root), (degree + third) as i32) % 12;
            (57 + (pc - 57).rem_euclid(12)) as u8
        })
        .collect();
    keys.sort_unstable();
    keys
}

/// True when scale degree `degree` is a tone of the triad on `chord`.
fn chord_tone(degree: i32, chord: usize) -> bool {
    [0, 2, 4].contains(&(degree - chord as i32).rem_euclid(7))
}

/// The lead: a seeded one-bar rhythm repeated each bar, with a melody that walks the scale,
/// lands on a chord tone on every beat and ends the phrase on the root. Bars 1 and 3 share a
/// shape, as do bars 2 and 4.
fn lead_line(idea: &Idea, r: &Rules, prog: [usize; 4], rng: &mut Rng) -> Vec<Note> {
    // Rhythm: distinct steps on the 1/8 grid (1/16 for busy styles), always the downbeat.
    let grid: i64 = if r.lead_notes > 5 { 1 } else { 2 };
    let slots: Vec<i64> = (1..BAR_STEPS / grid).map(|i| i * grid).collect();
    let mut steps = vec![0];
    let mut pool = slots;
    while steps.len() < r.lead_notes && !pool.is_empty() {
        steps.push(pool.remove(rng.below(pool.len())));
    }
    steps.sort_unstable();
    let lens: Vec<i64> = steps
        .iter()
        .enumerate()
        .map(|(i, &at)| {
            let next = steps.get(i + 1).copied().unwrap_or(BAR_STEPS);
            (next - at).clamp(1, r.lead_max)
        })
        .collect();
    // Two shapes: degree moves per note.
    let shapes: [Vec<i32>; 2] = std::array::from_fn(|_| {
        steps
            .iter()
            .map(|_| [-2, -1, -1, 1, 1, 2, 0][rng.below(7)])
            .collect()
    });
    let base = 60 + i32::from(idea.root);
    let mut out = Vec::new();
    let mut degree = 4;
    for bar in 0..BARS {
        let chord = prog[bar as usize];
        let shape = &shapes[(bar % 2) as usize];
        for (i, (&at, &len)) in steps.iter().zip(&lens).enumerate() {
            degree = (degree + shape[i]).clamp(0, 11);
            let last = bar == BARS - 1 && i == steps.len() - 1;
            if last {
                degree = if degree >= 4 { 7 } else { 0 };
            } else if at % 4 == 0 {
                // Strong steps move to the nearest chord tone.
                degree = (0..=4)
                    .flat_map(|d| [degree - d, degree + d])
                    .find(|&d| (0..=11).contains(&d) && chord_tone(d, chord))
                    .unwrap_or(degree);
            }
            let k = idea.key_of(base, degree).clamp(0, 127) as u8;
            let v = if at % 4 == 0 { 0.85 } else { 0.7 };
            out.push(Note::new(
                (bar * BAR_STEPS + at) * STEP_TICKS,
                len * STEP_TICKS,
                k,
                v,
            ));
        }
    }
    out
}

/// Every pattern a build makes, by name.
pub const PATTERNS: [&str; 5] = ["Drums", "Hats", "Bass", "Chords", "Lead"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playlist::ClipKind;
    use crate::project::PatternId;

    fn notes_of(p: &Project, pattern: &str) -> Vec<Note> {
        let pat = p.patterns.iter().find(|x| x.name == pattern).unwrap();
        pat.notes.values().flatten().copied().collect()
    }

    fn pattern_id(p: &Project, name: &str) -> PatternId {
        p.patterns.iter().find(|x| x.name == name).unwrap().id
    }

    #[test]
    fn every_style_builds_a_song_from_factory_sounds() {
        for style in Style::ALL {
            // Song::drums and Song::synth panic on a missing factory kit or preset.
            let p = build(&Idea::for_style(style));
            assert_eq!(p.channels.len(), 4, "{style:?}");
            assert!(p.channels[0].drums().is_some());
            assert!(p.channels[1..].iter().all(|c| c.synth().is_some()));
            let names: Vec<&str> = p.patterns.iter().map(|x| x.name.as_str()).collect();
            assert_eq!(names, PATTERNS);
            for name in PATTERNS {
                let notes = notes_of(&p, name);
                assert!(!notes.is_empty(), "{style:?} {name}");
                assert!(
                    notes
                        .iter()
                        .all(|n| n.start >= 0 && n.start + n.length <= BARS * 16 * STEP_TICKS),
                    "{style:?} {name}"
                );
            }
            assert!((p.tempo.bpm_at(crate::time::Tick(0)) - f64::from(style.tempo())).abs() < 1e-6);
            assert_eq!(p.info.genre, style.name());
            let mut copy = p.clone();
            copy.sanitize();
            assert_eq!(copy, p, "{style:?} builds a sane project");
        }
    }

    #[test]
    fn melodic_parts_stay_in_the_key() {
        for (root, minor) in [(9, true), (0, false), (6, true), (11, false)] {
            for style in Style::ALL {
                let idea = Idea {
                    root,
                    minor,
                    ..Idea::for_style(style)
                };
                let p = build(&idea);
                for name in ["Bass", "Chords", "Lead"] {
                    for n in notes_of(&p, name) {
                        let off = (i32::from(n.key) - i32::from(root)).rem_euclid(12) as u8;
                        assert!(idea.scale().contains(&off), "{style:?} {name} {}", n.key);
                    }
                }
            }
        }
    }

    #[test]
    fn the_lead_lands_on_chord_tones_and_ends_on_the_root() {
        let idea = Idea {
            seed: 7,
            ..Idea::for_style(Style::Synthwave)
        };
        let p = build(&idea);
        let prog = idea.progression();
        let lead = notes_of(&p, "Lead");
        let last = lead.iter().max_by_key(|n| n.start).unwrap();
        for n in &lead {
            let step = n.start / STEP_TICKS;
            if step % 4 == 0 && n.start != last.start {
                let pc = i32::from(n.key) % 12;
                let tones: Vec<i32> = chord_keys(&idea, prog[(step / 16) as usize])
                    .iter()
                    .map(|&k| i32::from(k) % 12)
                    .collect();
                assert!(tones.contains(&pc), "step {step} key {}", n.key);
            }
        }
        assert_eq!(i32::from(last.key) % 12, i32::from(idea.root));
    }

    #[test]
    fn the_same_idea_builds_the_same_song_and_the_seed_changes_it() {
        let idea = Idea::for_style(Style::House);
        assert_eq!(build(&idea), build(&idea));
        let lead = |seed| {
            notes_of(&build(&Idea { seed, ..idea }), "Lead")
                .iter()
                .map(|n| (n.start, n.key))
                .collect::<Vec<_>>()
        };
        let a = lead(1);
        assert!((2..12).any(|s| lead(s) != a));
    }

    #[test]
    fn the_arrangement_plays_every_pattern_under_markers() {
        let p = build(&Idea::default());
        let bar = 16 * STEP_TICKS;
        let mut used = std::collections::BTreeSet::new();
        let mut end = 0;
        for c in &p.playlist.clips {
            if let ClipKind::Pattern(id) = c.kind {
                used.insert(id);
            }
            end = end.max(c.start + c.length);
        }
        assert_eq!(used.len(), PATTERNS.len());
        assert_eq!(end, 28 * bar);
        let markers: Vec<&str> = p.playlist.markers.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(markers, ["Intro", "Verse", "Break", "Drop", "Outro"]);
        // The intro has no kick.
        let intro: Vec<_> = p
            .playlist
            .clips
            .iter()
            .filter(|c| c.start < 4 * bar)
            .map(|c| c.kind.clone())
            .collect();
        assert!(!intro.contains(&ClipKind::Pattern(pattern_id(&p, "Drums"))));
    }

    #[test]
    fn odd_settings_are_clamped() {
        let p = build(&Idea {
            tempo: f32::NAN,
            root: 40,
            ..Idea::default()
        });
        assert!((p.tempo.bpm_at(crate::time::Tick(0)) - 120.0).abs() < 1e-6);
        let p = build(&Idea {
            tempo: 999.0,
            ..Idea::default()
        });
        assert!((p.tempo.bpm_at(crate::time::Tick(0)) - 240.0).abs() < 1e-6);
    }
}
