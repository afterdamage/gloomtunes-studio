//! Factory demo projects (Prompt 28): ten short original songs in different styles, made only
//! from built-in instruments (drum synth kits, the synth's factory presets), so they open
//! anywhere. All CC0.

use crate::effects::{EffectKind, EffectSlot};
use crate::mixer::{DEMO_DELAY, DEMO_REVERB, FX_SLOTS, MASTER};
use crate::pads::DrumKit;
use crate::playlist::ClipKind;
use crate::project::{ChannelId, Note, PatternId, Project, STEP_TICKS};
use crate::synth::SynthPatch;
use crate::time::{TempoMap, PPQ};

/// Pad keys of a drum synth kit (bank A).
mod pad {
    pub const KICK: u8 = 60;
    pub const SNARE: u8 = 61;
    pub const HAT: u8 = 62;
    pub const OPEN: u8 = 63;
    pub const CLAP: u8 = 64;
    pub const LOW_TOM: u8 = 65;
    pub const HIGH_TOM: u8 = 66;
    pub const PERC: u8 = 67;
}
use pad::*;

const BAR: i64 = 4 * PPQ;

/// A demo: name, style and how to build it.
pub struct Demo {
    /// Song name.
    pub name: &'static str,
    /// Style, for the menu.
    pub style: &'static str,
    /// Builds the project.
    pub build: fn() -> Project,
}

/// The ten demos.
pub const DEMOS: [Demo; 10] = [
    Demo {
        name: "Acid Warehouse",
        style: "Acid house",
        build: acid_warehouse,
    },
    Demo {
        name: "Night Drive",
        style: "Synthwave",
        build: night_drive,
    },
    Demo {
        name: "Dust Tape",
        style: "Lo-fi hip hop",
        build: dust_tape,
    },
    Demo {
        name: "Trap Lanterns",
        style: "Trap",
        build: trap_lanterns,
    },
    Demo {
        name: "Concrete Pulse",
        style: "Techno",
        build: concrete_pulse,
    },
    Demo {
        name: "Glass Garden",
        style: "Ambient",
        build: glass_garden,
    },
    Demo {
        name: "Velvet House",
        style: "Deep house",
        build: velvet_house,
    },
    Demo {
        name: "Broken Circuit",
        style: "Electro",
        build: broken_circuit,
    },
    Demo {
        name: "Neon Roller",
        style: "Drum and bass",
        build: neon_roller,
    },
    Demo {
        name: "Harbour Echo",
        style: "Dub techno",
        build: harbour_echo,
    },
];

/// Key number of a note name such as `c3`, `f#2` or `bb4` (C4 is 60).
fn key(name: &str) -> u8 {
    let b = name.as_bytes();
    let base: i32 = match b[0].to_ascii_lowercase() {
        b'c' => 0,
        b'd' => 2,
        b'e' => 4,
        b'f' => 5,
        b'g' => 7,
        b'a' => 9,
        _ => 11,
    };
    let (shift, rest) = match b.get(1) {
        Some(b'#') => (1, &name[2..]),
        Some(b'b') if b.len() > 2 => (-1, &name[2..]),
        _ => (0, &name[1..]),
    };
    let octave: i32 = rest.parse().expect("octave");
    (12 * (octave + 1) + base + shift).clamp(0, 127) as u8
}

/// Builds a demo.
struct Song {
    p: Project,
    /// Patterns in the order they were made; pattern `i` plays on track `i`.
    order: Vec<PatternId>,
    at: i64,
}

impl Song {
    fn new(name: &str, style: &str, bpm: f64, swing: f32) -> Self {
        let mut p = Project::empty();
        p.tempo = TempoMap::constant(bpm);
        p.swing = swing;
        p.info.title = name.to_owned();
        p.info.artist = "GloomTunes Studio".to_owned();
        p.info.genre = style.to_owned();
        p.info.notes = "Factory demo, CC0".to_owned();
        // The empty project's first pattern is replaced by the song's own.
        p.patterns.clear();
        let m = &mut p.mixer;
        let (reverb, delay) = m.fx_returns();
        m.strips[reverb].name = "Reverb".to_owned();
        m.strips[reverb].slots[0] = Some(
            EffectSlot::new(EffectKind::Reverb)
                .with("decay", 2.4)
                .with("mix", 1.0)
                .with("damping", 6000.0),
        );
        m.strips[delay].name = "Delay".to_owned();
        m.strips[delay].slots[0] = Some(
            EffectSlot::new(EffectKind::Delay)
                .with("time", 6.0)
                .with("mix", 1.0)
                .with("feedback", 0.4),
        );
        m.strips[delay].volume = 0.7;
        m.strips[MASTER].slots[FX_SLOTS - 1] =
            Some(EffectSlot::new(EffectKind::Limiter).with("ceiling", -0.3));
        Self {
            p,
            order: Vec::new(),
            at: 0,
        }
    }

    fn insert(&self, ch: ChannelId) -> usize {
        self.p
            .channel_index(ch)
            .map_or(MASTER, |i| self.p.channels[i].insert)
    }

    /// A drum synth channel playing factory kit `kit`.
    fn drums(&mut self, kit: &str) -> ChannelId {
        let id = self.p.add_drum_synth_channel(kit).expect("demo fits");
        let k = DrumKit::factory_kits()
            .into_iter()
            .find(|(n, _)| *n == kit)
            .map(|(_, k)| k)
            .expect("factory kit");
        *self
            .p
            .channels
            .last_mut()
            .and_then(|c| c.drums_mut())
            .expect("just added") = k;
        id
    }

    /// A synth channel with factory preset `preset`.
    fn synth(&mut self, preset: &str, volume: f32) -> ChannelId {
        let patch = SynthPatch::factory()
            .into_iter()
            .find(|p| p.name == preset)
            .expect("factory preset");
        let id = self.p.add_synth_channel(preset, patch).expect("demo fits");
        self.p.channels.last_mut().expect("just added").volume = volume;
        id
    }

    /// Reverb and delay send levels of `ch`'s insert.
    fn sends(&mut self, ch: ChannelId, reverb: f32, delay: f32) {
        let i = self.insert(ch);
        let m = &mut self.p.mixer;
        for (to, level) in [(DEMO_REVERB, reverb), (DEMO_DELAY, delay)] {
            if level > 0.0 {
                m.add_send(i, to, level, false);
            }
        }
    }

    /// A new pattern of `bars` bars.
    fn pattern(&mut self, name: &str, bars: u16) -> PatternId {
        let id = self.p.new_pattern();
        self.p.rename_pattern(id, name);
        if let Some(p) = self.p.patterns.iter_mut().find(|p| p.id == id) {
            p.steps = 16 * bars;
            p.color = (self.order.len() % 8 + 1) as u8;
        }
        self.order.push(id);
        id
    }

    fn add(&mut self, pat: PatternId, ch: ChannelId, notes: impl IntoIterator<Item = Note>) {
        let p = self
            .p
            .patterns
            .iter_mut()
            .find(|p| p.id == pat)
            .expect("pattern");
        let list = p.notes.entry(ch).or_default();
        list.extend(notes);
        list.sort_by_key(|n| (n.start, n.key));
    }

    /// Drum hits of pad `key` from a grid: one character per cell, the cells spread evenly over
    /// `bars` bars; `x` hard, `o` soft, `.` nothing. Spaces are ignored.
    fn hits(&mut self, pat: PatternId, ch: ChannelId, key: u8, bars: i64, grid: &str) {
        let cells: Vec<char> = grid.chars().filter(|c| !c.is_whitespace()).collect();
        let cell = bars * BAR / cells.len() as i64;
        let notes = cells.iter().enumerate().filter_map(|(i, c)| {
            let v = match c {
                'x' => 0.9,
                'o' => 0.55,
                _ => return None,
            };
            Some(Note::new(i as i64 * cell, cell.min(STEP_TICKS), key, v))
        });
        self.add(pat, ch, notes.collect::<Vec<_>>());
    }

    /// A melodic line: tokens `key[:steps][!][~]` (`-` is a rest), each starting where the last
    /// ended; `!` is an accent (full velocity), `~` slides into the next note. Lengths are in
    /// 1/16 steps (default 1).
    fn line(&mut self, pat: PatternId, ch: ChannelId, velocity: f32, line: &str) {
        let mut at = 0;
        let mut notes = Vec::new();
        for tok in line.split_whitespace() {
            let accent = tok.contains('!');
            let slide = tok.contains('~');
            let tok = tok.trim_end_matches(['!', '~']);
            let (name, len) = tok.split_once(':').unwrap_or((tok, "1"));
            let len: f32 = len.parse().expect("length");
            let ticks = (len * STEP_TICKS as f32) as i64;
            if name != "-" {
                let mut n = Note::new(at, ticks, key(name), if accent { 1.0 } else { velocity });
                n.slide = slide;
                notes.push(n);
            }
            at += ticks;
        }
        self.add(pat, ch, notes);
    }

    /// Chords: (start step, length in steps, notes).
    fn chords(
        &mut self,
        pat: PatternId,
        ch: ChannelId,
        velocity: f32,
        chords: &[(i64, i64, &str)],
    ) {
        let notes: Vec<Note> = chords
            .iter()
            .flat_map(|&(at, len, keys)| {
                keys.split_whitespace()
                    .map(move |k| Note::new(at * STEP_TICKS, len * STEP_TICKS, key(k), velocity))
            })
            .collect();
        self.add(pat, ch, notes);
    }

    /// The next `bars` bars play `patterns` (each repeated to fill the section), under a marker.
    fn section(&mut self, marker: &str, bars: i64, patterns: &[PatternId]) {
        let start = self.at;
        self.p.playlist.add_marker(start, marker);
        for &id in patterns {
            let track_index = self.order.iter().position(|&o| o == id).expect("pattern");
            while self.p.playlist.tracks.len() <= track_index {
                self.p.playlist.add_track();
            }
            let track = self.p.playlist.tracks[track_index].id;
            let len = self
                .p
                .pattern(id)
                .map_or(BAR, |p| i64::from(p.steps) * STEP_TICKS);
            let mut t = start;
            while t < start + bars * BAR {
                let l = len.min(start + bars * BAR - t);
                self.p.playlist.add_clip(track, t, l, ClipKind::Pattern(id));
                t += l;
            }
        }
        self.at += bars * BAR;
    }

    fn finish(mut self) -> Project {
        for (t, id) in self.p.playlist.tracks.iter_mut().zip(&self.order) {
            if let Some(p) = self.p.patterns.iter().find(|p| p.id == *id) {
                t.name.clone_from(&p.name);
            }
        }
        self.p.current_pattern = self.order[0];
        self.p.sanitize();
        self.p
    }
}

fn acid_warehouse() -> Project {
    let mut s = Song::new("Acid Warehouse", "Acid house", 126.0, 0.08);
    let d = s.drums("Deep House");
    let bass = s.synth("Acid Line", 0.8);
    let stab = s.synth("Crystal Stab", 0.45);
    s.sends(stab, 0.3, 0.35);
    let beat = s.pattern("Beat", 1);
    s.hits(beat, d, KICK, 1, "x...x...x...x...");
    s.hits(beat, d, CLAP, 1, "....x.......x...");
    s.hits(beat, d, HAT, 1, "..x...x...x...xo");
    s.hits(beat, d, OPEN, 1, "......x.......x.");
    let acid = s.pattern("Acid", 2);
    s.line(
        acid,
        bass,
        0.6,
        "a1 a1 a2! a1 c2~ d2 a1 g1! a1 a1 e2! a1 d2~ c2 a1 a2! \
         a1 a1 a2! g2~ a2 a1 c2! a1 a1 e2 a1 g1! a1 c2~ d2 e2!",
    );
    let stabs = s.pattern("Stabs", 2);
    s.chords(
        stabs,
        stab,
        0.75,
        &[
            (2, 1, "a3 c4 e4 g4"),
            (10, 1, "a3 c4 e4 g4"),
            (19, 1, "g3 c4 d4 f4"),
            (26, 1, "a3 c4 e4 g4"),
        ],
    );
    let perc = s.pattern("Perc", 1);
    s.hits(perc, d, PERC, 1, "...o..o....o..o.");
    s.hits(perc, d, HIGH_TOM, 1, ".........o......");
    s.section("Intro", 4, &[beat]);
    s.section("Acid", 8, &[beat, acid]);
    s.section("Peak", 8, &[beat, acid, stabs, perc]);
    s.section("Break", 4, &[acid, stabs]);
    s.section("Out", 4, &[beat, acid, perc]);
    s.finish()
}

fn night_drive() -> Project {
    let mut s = Song::new("Night Drive", "Synthwave", 100.0, 0.0);
    let d = s.drums("Disco Tight");
    let bass = s.synth("Rubber Pluck Bass", 0.7);
    let pad = s.synth("Wide Saw Chord", 0.45);
    let lead = s.synth("Ember Lead", 0.55);
    s.sends(pad, 0.4, 0.0);
    s.sends(lead, 0.3, 0.4);
    let beat = s.pattern("Beat", 1);
    s.hits(beat, d, KICK, 1, "x.......x.......");
    s.hits(beat, d, SNARE, 1, "....x.......x...");
    s.hits(beat, d, HAT, 1, "x.x.x.x.x.x.x.x.");
    let line = s.pattern("Bass", 4);
    s.line(
        line,
        bass,
        0.75,
        "a1:2 a1:2 a2:2 a1:2 a1:2 a1:2 a2:2 a1:2 \
         f1:2 f1:2 f2:2 f1:2 f1:2 f1:2 f2:2 f1:2 \
         c2:2 c2:2 c3:2 c2:2 c2:2 c2:2 c3:2 c2:2 \
         g1:2 g1:2 g2:2 g1:2 g1:2 g1:2 g2:2 g1:2",
    );
    let chords = s.pattern("Chords", 4);
    s.chords(
        chords,
        pad,
        0.6,
        &[
            (0, 16, "a3 c4 e4"),
            (16, 16, "f3 a3 c4"),
            (32, 16, "c4 e4 g4"),
            (48, 16, "g3 b3 d4"),
        ],
    );
    let melody = s.pattern("Lead", 4);
    s.line(
        melody,
        lead,
        0.7,
        "e5:6 d5:2 c5:4 b4:4 c5:6 a4:2 f4:8 \
         g4:4 c5:4 e5:4 g5:4 d5:12 -:4",
    );
    s.section("Intro", 4, &[chords]);
    s.section("Drive", 8, &[beat, line, chords]);
    s.section("Hook", 8, &[beat, line, chords, melody]);
    s.section("Fade", 4, &[line, chords]);
    s.finish()
}

fn dust_tape() -> Project {
    let mut s = Song::new("Dust Tape", "Lo-fi hip hop", 82.0, 0.35);
    let d = s.drums("Lo-Fi Dust");
    let keys = s.synth("Velvet Keys", 0.6);
    let bass = s.synth("Soft Sub", 0.7);
    s.sends(keys, 0.35, 0.15);
    let beat = s.pattern("Beat", 2);
    s.hits(beat, d, KICK, 2, "x......x..x..... x.....x...x.....");
    s.hits(beat, d, SNARE, 2, "....x.......x... ....x.......x..o");
    s.hits(beat, d, HAT, 2, "x.o.x.o.x.o.x.oo x.o.x.o.x.o.x.o.");
    let chords = s.pattern("Keys", 4);
    s.chords(
        chords,
        keys,
        0.55,
        &[
            (0, 14, "d3 f3 a3 c4 e4"),
            (16, 14, "g3 b3 d4 f4 a4"),
            (32, 14, "c3 e3 g3 b3 d4"),
            (48, 8, "a2 c#3 g3 b3 e4"),
            (56, 6, "a2 c3 e3 g3 c4"),
        ],
    );
    let low = s.pattern("Bass", 4);
    s.line(
        low,
        bass,
        0.7,
        "d2:6 -:2 d2:3 a1:5 g1:6 -:2 g1:3 b1:5 c2:6 -:2 c2:3 e2:5 a1:8 a1:4 c2:4",
    );
    s.section("Tape in", 4, &[chords]);
    s.section("Verse", 8, &[beat, chords, low]);
    s.section("Drop out", 4, &[chords, low]);
    s.section("Verse 2", 8, &[beat, chords, low]);
    s.finish()
}

fn trap_lanterns() -> Project {
    let mut s = Song::new("Trap Lanterns", "Trap", 140.0, 0.0);
    let d = s.drums("Trap Night");
    let bell = s.synth("FM Bell", 0.5);
    let sub = s.synth("FM Sub Thump", 0.7);
    s.sends(bell, 0.3, 0.4);
    let beat = s.pattern("Beat", 2);
    s.hits(beat, d, KICK, 2, "x.........x..... x.....x.......x.");
    s.hits(beat, d, CLAP, 2, "........x....... ........x.......");
    // Hats in 1/32 with rolls.
    s.hits(
        beat,
        d,
        HAT,
        2,
        "x.x.x.x.x.x.xxxx x.x.x.x.x.x.x.x. x.x.x.x.xxxxxxxx x.x.x.x.x.x.x.xx",
    );
    s.hits(beat, d, OPEN, 2, "..............x. ................");
    let melody = s.pattern("Bells", 4);
    s.line(
        melody,
        bell,
        0.6,
        "c#5:2 e5:2 g#5:2 e5:2 b4:4 g#4:4 a4:2 c#5:2 e5:2 c#5:2 f#4:8 \
         c#5:2 e5:2 g#5:2 b5:2 a5:4 g#5:4 e5:2 f#5:2 e5:2 c#5:2 g#4:8",
    );
    let low = s.pattern("808", 4);
    s.line(
        low,
        sub,
        0.8,
        "c#2:10 c#2:6 a1:12 a1:4 f#1:10 f#1:6 g#1:8 g#1:4 b1:4",
    );
    s.section("Intro", 4, &[melody]);
    s.section("Hook", 8, &[beat, melody, low]);
    s.section("Bridge", 4, &[melody, low]);
    s.section("Hook 2", 8, &[beat, melody, low]);
    s.finish()
}

fn concrete_pulse() -> Project {
    let mut s = Song::new("Concrete Pulse", "Techno", 132.0, 0.0);
    let d = s.drums("Industrial");
    let rumble = s.synth("Iron Bass", 0.6);
    let stab = s.synth("Static Pluck", 0.5);
    let riser = s.synth("Lunar Sweep", 0.45);
    s.sends(stab, 0.2, 0.5);
    s.sends(riser, 0.5, 0.0);
    let beat = s.pattern("Kick", 1);
    s.hits(beat, d, KICK, 1, "x...x...x...x...");
    let top = s.pattern("Tops", 1);
    s.hits(top, d, HAT, 1, "..x...x...x...x.");
    s.hits(top, d, OPEN, 1, "..o...o...o...o.");
    s.hits(top, d, CLAP, 1, "....x.......x...");
    s.hits(top, d, PERC, 1, ".o....o..o....o.");
    let low = s.pattern("Rumble", 1);
    s.line(low, rumble, 0.6, "-:2 f1 - -:2 f1 - -:2 f1 f1 -:2 f1 g#1");
    let stabs = s.pattern("Stabs", 2);
    s.chords(
        stabs,
        stab,
        0.7,
        &[
            (3, 1, "f3 g#3 c4"),
            (11, 1, "f3 g#3 c4"),
            (14, 1, "d#3 g3 c4"),
            (22, 2, "f3 g#3 c#4"),
        ],
    );
    let rise = s.pattern("Riser", 4);
    s.chords(rise, riser, 0.7, &[(0, 64, "f3")]);
    s.section("Intro", 8, &[beat, top]);
    s.section("Build", 4, &[beat, top, low, rise]);
    s.section("Main", 12, &[beat, top, low, stabs]);
    s.section("Out", 4, &[beat, low]);
    s.finish()
}

fn glass_garden() -> Project {
    let mut s = Song::new("Glass Garden", "Ambient", 70.0, 0.0);
    let d = s.drums("Minimal Click");
    let pad = s.synth("Cathedral Pad", 0.5);
    let bell = s.synth("Fog Bell", 0.45);
    let drops = s.synth("Rain Pluck", 0.4);
    s.sends(pad, 0.6, 0.0);
    s.sends(bell, 0.6, 0.5);
    s.sends(drops, 0.5, 0.6);
    let chords = s.pattern("Pad", 8);
    s.chords(
        chords,
        pad,
        0.5,
        &[
            (0, 32, "e3 b3 d4 g4"),
            (32, 32, "c3 g3 b3 e4"),
            (64, 32, "a2 e3 g3 c4"),
            (96, 32, "d3 a3 c4 f#4"),
        ],
    );
    let bells = s.pattern("Bells", 4);
    s.line(
        bells,
        bell,
        0.5,
        "b5:6 g5:6 e5:4 -:8 d5:6 e5:10 -:16 a5:6 f#5:10",
    );
    let rain = s.pattern("Rain", 2);
    s.line(
        rain,
        drops,
        0.45,
        "e6 - b5 - g6:2 - d6 -:3 e6 b5:2 -:2 a5 -:3 g5 - d6:3 -:4 f#6:2 -:4",
    );
    let tick = s.pattern("Ticks", 1);
    s.hits(tick, d, PERC, 1, "o.....o...o.....");
    s.hits(tick, d, HAT, 1, "....o.......o..o");
    s.section("Dawn", 8, &[chords]);
    s.section("Bloom", 8, &[chords, bells, rain]);
    s.section("Pulse", 8, &[chords, bells, rain, tick]);
    s.section("Dusk", 8, &[chords, rain]);
    s.finish()
}

fn velvet_house() -> Project {
    let mut s = Song::new("Velvet House", "Deep house", 122.0, 0.12);
    let d = s.drums("Deep House");
    let chords = s.synth("Warm Poly", 0.5);
    let bass = s.synth("Dub Sub Line", 0.75);
    s.sends(chords, 0.3, 0.3);
    let beat = s.pattern("Beat", 1);
    s.hits(beat, d, KICK, 1, "x...x...x...x...");
    s.hits(beat, d, CLAP, 1, "....x.......x...");
    s.hits(beat, d, HAT, 1, "..x...x...x...x.");
    s.hits(beat, d, PERC, 1, ".o.....o..o.....");
    let stabs = s.pattern("Chords", 2);
    s.chords(
        stabs,
        chords,
        0.6,
        &[
            (2, 2, "c3 eb3 g3 bb3 d4"),
            (7, 1, "c3 eb3 g3 bb3 d4"),
            (10, 2, "c3 eb3 g3 bb3 d4"),
            (18, 2, "ab2 c3 eb3 g3 bb3"),
            (23, 1, "ab2 c3 eb3 g3 bb3"),
            (26, 3, "bb2 d3 f3 ab3 c4"),
        ],
    );
    let low = s.pattern("Bass", 2);
    s.line(
        low,
        bass,
        0.75,
        "-:2 c2:2 -:1 c2 -:2 c2:2 eb2:2 -:2 g1:2 -:2 ab1:2 -:1 ab1 -:2 ab1:2 bb1:4 bb1:2",
    );
    s.section("Intro", 4, &[beat]);
    s.section("Groove", 8, &[beat, low]);
    s.section("Full", 12, &[beat, low, stabs]);
    s.section("Breather", 4, &[stabs]);
    s.section("Full 2", 8, &[beat, low, stabs]);
    s.finish()
}

fn broken_circuit() -> Project {
    let mut s = Song::new("Broken Circuit", "Electro", 128.0, 0.0);
    let d = s.drums("Electro Snap");
    let bass = s.synth("Tight Electro", 0.75);
    let arp = s.synth("Ghost Arp", 0.45);
    s.sends(arp, 0.2, 0.5);
    let beat = s.pattern("Beat", 2);
    s.hits(beat, d, KICK, 2, "x......x..x..... x.....x...x..x..");
    s.hits(beat, d, SNARE, 2, "....x.......x... ....x.......x...");
    s.hits(beat, d, HAT, 2, "xxxxxxxxxxxxxxxx xxxxxxxxxxxxxxxx");
    s.hits(beat, d, LOW_TOM, 2, "................ ..............o.");
    s.hits(beat, d, HIGH_TOM, 2, "................ ...............o");
    let low = s.pattern("Bass", 2);
    s.line(
        low,
        bass,
        0.65,
        "e1:2 e1 - e2! e1 -:2 g1:2 a1~ b1 e1 - d2! -:1 e1 e1 - e2! e1 -:2 g1:2 a1 c2! b1 a1:2 g1",
    );
    let arps = s.pattern("Arp", 1);
    s.line(
        arps,
        arp,
        0.6,
        "e4 g4 b4 e5 g4 b4 e5 g5 d4 g4 b4 d5 g4 b4 d5 f#5",
    );
    s.section("Boot", 4, &[beat]);
    s.section("Circuit", 8, &[beat, low]);
    s.section("Overload", 8, &[beat, low, arps]);
    s.section("Glitch", 4, &[low, arps]);
    s.section("Reboot", 8, &[beat, low, arps]);
    s.finish()
}

fn neon_roller() -> Project {
    let mut s = Song::new("Neon Roller", "Drum and bass", 172.0, 0.0);
    let d = s.drums("Gloom Default");
    let reese = s.synth("WT Reese", 0.6);
    let pad = s.synth("Drift Pad", 0.4);
    s.sends(pad, 0.5, 0.0);
    let beat = s.pattern("Break", 2);
    s.hits(beat, d, KICK, 2, "x.........x..... x.........x.....");
    s.hits(beat, d, SNARE, 2, "....x.......x... ....x..o....x..o");
    s.hits(beat, d, HAT, 2, "x.x.x.x.x.x.x.x. x.x.xxx.x.x.x.x.");
    s.hits(beat, d, PERC, 2, "..o.....o....... ..o.....o.....o.");
    let low = s.pattern("Reese", 4);
    s.line(low, reese, 0.7, "f1:12 ab1:4 eb1:12 -:4 db1:12 eb1:4 f1:16");
    let wash = s.pattern("Pad", 4);
    s.chords(
        wash,
        pad,
        0.5,
        &[
            (0, 16, "f3 ab3 c4 eb4"),
            (16, 16, "eb3 g3 bb3 db4"),
            (32, 16, "db3 f3 ab3 c4"),
            (48, 16, "c3 f3 g3 c4"),
        ],
    );
    s.section("Intro", 8, &[wash]);
    s.section("Roll", 16, &[beat, low, wash]);
    s.section("Break", 4, &[wash]);
    s.section("Roll 2", 16, &[beat, low, wash]);
    s.finish()
}

fn harbour_echo() -> Project {
    let mut s = Song::new("Harbour Echo", "Dub techno", 118.0, 0.05);
    let d = s.drums("Dub Wide");
    let stab = s.synth("Crystal Stab", 0.45);
    let bass = s.synth("Deep Roll", 0.7);
    s.sends(stab, 0.5, 0.75);
    let beat = s.pattern("Beat", 1);
    s.hits(beat, d, KICK, 1, "x...x...x...x...");
    s.hits(beat, d, HAT, 1, "..x...x...x...x.");
    s.hits(beat, d, OPEN, 1, "..........o.....");
    s.hits(beat, d, PERC, 1, ".......o.......o");
    let stabs = s.pattern("Chord", 2);
    s.chords(
        stabs,
        stab,
        0.6,
        &[
            (3, 1, "d3 f3 a3 c4"),
            (13, 1, "d3 f3 a3 c4"),
            (22, 1, "d3 f3 g3 c4"),
        ],
    );
    let low = s.pattern("Bass", 1);
    s.line(low, bass, 0.6, "d1:3 -:1 d1:2 -:2 d1:3 -:1 f1:2 c1:2");
    s.section("Mist", 8, &[stabs]);
    s.section("Harbour", 8, &[beat, stabs]);
    s.section("Deep", 16, &[beat, stabs, low]);
    s.section("Tide out", 4, &[stabs, low]);
    s.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names() {
        assert_eq!(key("c4"), 60);
        assert_eq!(key("a2"), 45);
        assert_eq!(key("f#2"), 42);
        assert_eq!(key("bb1"), 34);
        assert_eq!(key("eb3"), 51);
    }

    #[test]
    fn every_demo_builds_with_notes_on_every_channel_and_a_song() {
        let mut names = Vec::new();
        for demo in &DEMOS {
            let p = (demo.build)();
            names.push(demo.name);
            assert_eq!(p.info.title, demo.name);
            assert!(p.playlist.song_end() >= 16 * BAR, "{}", demo.name);
            for c in &p.channels {
                let used = p
                    .patterns
                    .iter()
                    .any(|pat| pat.notes.get(&c.id).is_some_and(|n| !n.is_empty()));
                assert!(used, "{}: {} plays nothing", demo.name, c.name);
            }
            // Every pattern is in the song, and notes fit their pattern.
            for pat in &p.patterns {
                assert!(
                    p.playlist
                        .clips
                        .iter()
                        .any(|c| c.kind == ClipKind::Pattern(pat.id)),
                    "{}: {} unused",
                    demo.name,
                    pat.name
                );
                let len = i64::from(pat.steps) * STEP_TICKS;
                for n in pat.notes.values().flatten() {
                    assert!(n.start >= 0 && n.start < len, "{}: {}", demo.name, pat.name);
                }
            }
            let mut again = p.clone();
            again.sanitize();
            assert_eq!(again, p, "{} is not sanitize-stable", demo.name);
        }
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 10);
    }
}
