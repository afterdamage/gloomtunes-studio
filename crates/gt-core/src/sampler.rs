//! Sampler 2 settings (Prompt 23): interpolation quality, the voice filter and its envelope,
//! two LFOs, key zones and velocity layers, and how a folder of samples maps onto keys.

use crate::modulation::LfoShape;
use crate::project::{Adsr, Note, SampleSource};
use crate::time::PPQ;

/// How the sampler reads between stored frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Interp {
    /// Linear: cheapest, some aliasing when pitched up (draft).
    #[default]
    Linear,
    /// Windowed sinc, 8 points per side.
    Good,
    /// Windowed sinc, 32 points per side.
    Best,
}

impl Interp {
    /// Every quality, cheapest first.
    pub const ALL: [Interp; 3] = [Interp::Linear, Interp::Good, Interp::Best];

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            Interp::Linear => "Draft (linear)",
            Interp::Good => "Good (sinc 16)",
            Interp::Best => "Best (sinc 64)",
        }
    }

    /// Stable file key.
    pub fn key(self) -> &'static str {
        match self {
            Interp::Linear => "linear",
            Interp::Good => "sinc8",
            Interp::Best => "sinc32",
        }
    }

    /// The quality for a file key (linear when unknown).
    pub fn from_key(k: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|i| i.key() == k)
            .unwrap_or_default()
    }

    /// Kernel half-width in input frames (0 for linear).
    pub fn half_width(self) -> usize {
        match self {
            Interp::Linear => 0,
            Interp::Good => 8,
            Interp::Best => 32,
        }
    }
}

/// Response of the sampler's voice filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum SamplerFilterMode {
    /// No filter.
    #[default]
    Off,
    /// 12 dB/oct low-pass.
    LowPass,
    /// 12 dB/oct high-pass.
    HighPass,
    /// Band-pass.
    BandPass,
}

impl SamplerFilterMode {
    /// Every mode.
    pub const ALL: [SamplerFilterMode; 4] = [
        SamplerFilterMode::Off,
        SamplerFilterMode::LowPass,
        SamplerFilterMode::HighPass,
        SamplerFilterMode::BandPass,
    ];

    /// Display name and file key.
    pub fn name(self) -> &'static str {
        match self {
            SamplerFilterMode::Off => "Off",
            SamplerFilterMode::LowPass => "Low-pass",
            SamplerFilterMode::HighPass => "High-pass",
            SamplerFilterMode::BandPass => "Band-pass",
        }
    }

    /// The mode named `k` (off when unknown).
    pub fn from_name(k: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|m| m.name() == k)
            .unwrap_or_default()
    }
}

/// The voice filter: cutoff moved by its own envelope (in octaves) and the LFOs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SamplerFilter {
    /// Response.
    pub mode: SamplerFilterMode,
    /// Cutoff in Hz.
    pub cutoff: f32,
    /// Resonance, 0 to 1.
    pub resonance: f32,
    /// How far the envelope moves the cutoff at its peak, in octaves (-8 to 8).
    pub env_amount: f32,
    /// The filter envelope.
    pub env: Adsr,
}

impl Default for SamplerFilter {
    fn default() -> Self {
        Self {
            mode: SamplerFilterMode::Off,
            cutoff: 8000.0,
            resonance: 0.2,
            env_amount: 0.0,
            env: Adsr {
                attack_ms: 0.0,
                decay_ms: 300.0,
                sustain: 0.0,
                release_ms: 200.0,
            },
        }
    }
}

/// What an LFO moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum LfoTarget {
    /// Nothing (the LFO is off).
    #[default]
    Off,
    /// Pitch, up to ±12 semitones at full amount.
    Pitch,
    /// Filter cutoff, up to ±4 octaves.
    Cutoff,
    /// Volume (tremolo), down to silence.
    Volume,
    /// Pan, side to side.
    Pan,
}

impl LfoTarget {
    /// Every target.
    pub const ALL: [LfoTarget; 5] = [
        LfoTarget::Off,
        LfoTarget::Pitch,
        LfoTarget::Cutoff,
        LfoTarget::Volume,
        LfoTarget::Pan,
    ];

    /// Display name and file key.
    pub fn name(self) -> &'static str {
        match self {
            LfoTarget::Off => "Off",
            LfoTarget::Pitch => "Pitch",
            LfoTarget::Cutoff => "Cutoff",
            LfoTarget::Volume => "Volume",
            LfoTarget::Pan => "Pan",
        }
    }

    /// The target named `k` (off when unknown).
    pub fn from_name(k: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|m| m.name() == k)
            .unwrap_or_default()
    }
}

/// One of the sampler's two LFOs. Each note restarts it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SamplerLfo {
    /// Waveform.
    pub shape: LfoShape,
    /// Rate in Hz.
    pub rate: f32,
    /// What it moves.
    pub target: LfoTarget,
    /// Depth, 0 to 1 of the target's full range.
    pub amount: f32,
}

impl Default for SamplerLfo {
    fn default() -> Self {
        Self {
            shape: LfoShape::Sine,
            rate: 4.0,
            target: LfoTarget::Off,
            amount: 0.25,
        }
    }
}

/// One sample of a multi-sample instrument: the keys and velocities it answers.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyZone {
    /// The sound.
    pub sample: SampleSource,
    /// Key at which it plays at its recorded pitch.
    pub root: u8,
    /// Lowest key.
    pub key_lo: u8,
    /// Highest key.
    pub key_hi: u8,
    /// Lowest velocity (0 to 1).
    pub vel_lo: f32,
    /// Highest velocity (0 to 1).
    pub vel_hi: f32,
    /// Fine tune in semitones.
    pub tune: f32,
    /// Linear gain.
    pub gain: f32,
    /// Start of the part played, fraction of the sample (slices).
    pub start: f32,
    /// End of the part played, fraction of the sample.
    pub end: f32,
}

impl KeyZone {
    /// A zone over every key and velocity, rooted at `root`.
    pub fn new(sample: SampleSource, root: u8) -> Self {
        Self {
            sample,
            root,
            key_lo: 0,
            key_hi: 127,
            vel_lo: 0.0,
            vel_hi: 1.0,
            tune: 0.0,
            gain: 1.0,
            start: 0.0,
            end: 1.0,
        }
    }

    /// True when the zone answers `key` at `velocity`.
    pub fn matches(&self, key: u8, velocity: f32) -> bool {
        (self.key_lo..=self.key_hi).contains(&key)
            && velocity >= self.vel_lo
            && (velocity <= self.vel_hi || self.vel_hi >= 1.0)
    }

    /// Puts the values in range (keys ordered, velocities ordered).
    pub fn sanitize(&mut self) {
        self.root = self.root.min(127);
        self.key_lo = self.key_lo.min(127);
        self.key_hi = self.key_hi.clamp(self.key_lo, 127);
        let f = |v: f32, d: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { d };
        self.vel_lo = f(self.vel_lo, 0.0);
        self.vel_hi = f(self.vel_hi, 1.0).max(self.vel_lo);
        self.tune = if self.tune.is_finite() {
            self.tune.clamp(-48.0, 48.0)
        } else {
            0.0
        };
        self.gain = if self.gain.is_finite() {
            self.gain.clamp(0.0, 4.0)
        } else {
            1.0
        };
        self.start = if self.start.is_finite() {
            self.start.clamp(0.0, 0.999)
        } else {
            0.0
        };
        self.end = if self.end.is_finite() {
            self.end.clamp(self.start + 0.0001, 1.0)
        } else {
            1.0
        };
    }
}

/// Parses a note name such as `C4`, `F#3`, `Bb-1` or a MIDI number in a file name (the last
/// one found), with C4 = 60. Looks at tokens split on spaces, `_`, `-` (before a digit it is a
/// minus only right after a note letter) and `.`.
pub fn key_in_name(name: &str) -> Option<u8> {
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let mut found = None;
    for token in stem.split([' ', '_', '.', '(', ')', '[', ']']) {
        for part in split_dashes(token) {
            if let Some(k) = note_token(part) {
                found = Some(k);
            }
        }
    }
    found
}

/// Splits on `-` except where it is the minus of an octave after a note letter (`C-1`).
fn split_dashes(token: &str) -> Vec<&str> {
    let b = token.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    for i in 0..b.len() {
        if b[i] != b'-' {
            continue;
        }
        let after_note = i > 0 && (b[i - 1].is_ascii_alphabetic() || b[i - 1] == b'#');
        let before_digit = b.get(i + 1).is_some_and(u8::is_ascii_digit);
        if after_note && before_digit && i - from <= 2 {
            continue;
        }
        out.push(&token[from..i]);
        from = i + 1;
    }
    out.push(&token[from..]);
    out
}

fn note_token(t: &str) -> Option<u8> {
    if t.is_empty() {
        return None;
    }
    if t.bytes().all(|c| c.is_ascii_digit()) {
        // A bare MIDI number; short ones only (years and sizes are not keys).
        return (t.len() <= 3)
            .then(|| t.parse::<u8>().ok())
            .flatten()
            .filter(|&k| k <= 127);
    }
    let mut c = t.chars();
    let letter = c.next()?.to_ascii_uppercase();
    let base: i32 = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest: &str = c.as_str();
    let (acc, rest) = if let Some(r) = rest.strip_prefix('#') {
        (1, r)
    } else if let Some(r) = rest.strip_prefix('b').filter(|r| !r.is_empty()) {
        (-1, r)
    } else {
        (0, rest)
    };
    let octave: i32 = rest.parse().ok().filter(|o| (-1..=9).contains(o))?;
    let key = (octave + 1) * 12 + base + acc;
    u8::try_from(key).ok().filter(|&k| k <= 127)
}

/// Velocity layer and round-robin hints in a file name: `v3`, `vel3`, `pp`..`ff`, `soft`,
/// `hard`; and `rr2` for the second round-robin take.
fn layer_hint(name: &str) -> (Option<i32>, Option<u32>) {
    let stem = name
        .rsplit_once('.')
        .map_or(name, |(s, _)| s)
        .to_ascii_lowercase();
    let mut layer = None;
    let mut rr = None;
    for t in stem.split([' ', '_', '-', '.']) {
        let dyn_rank = ["ppp", "pp", "p", "mp", "mf", "f", "ff", "fff"]
            .iter()
            .position(|d| *d == t);
        if let Some(r) = dyn_rank {
            layer = Some(r as i32);
        } else if t == "soft" {
            layer = Some(0);
        } else if t == "med" || t == "medium" {
            layer = Some(4);
        } else if t == "hard" || t == "loud" {
            layer = Some(8);
        } else if let Some(n) = t.strip_prefix("rr").and_then(|n| n.parse().ok()) {
            rr = Some(n);
        } else if let Some(n) = t
            .strip_prefix("vel")
            .or_else(|| t.strip_prefix('v'))
            .and_then(|n| n.parse::<i32>().ok())
        {
            layer = Some(n);
        }
    }
    (layer, rr)
}

/// Maps a folder of samples (source and file name) onto keys: each file's root comes from its
/// name (else the files are laid out chromatically from C4 in name order); each root covers
/// the keys up to half-way to its neighbours; files sharing a root are split into velocity
/// layers by their dynamics hint (or name order), and takes marked `rr1`, `rr2`... of the same
/// layer share its velocity range (the sampler alternates them when round robin is on).
pub fn map_folder(files: &[(SampleSource, String)]) -> Vec<KeyZone> {
    let mut items: Vec<(&SampleSource, &str, Option<u8>)> = files
        .iter()
        .map(|(s, n)| (s, n.as_str(), key_in_name(n)))
        .collect();
    items.sort_by_key(|a| a.1.to_lowercase());
    if items.iter().all(|i| i.2.is_none()) {
        let mut k = 60u8;
        for i in &mut items {
            i.2 = Some(k);
            k = k.saturating_add(1).min(127);
        }
    }
    let mut roots: Vec<u8> = items.iter().filter_map(|i| i.2).collect();
    roots.sort_unstable();
    roots.dedup();
    let mut zones = Vec::new();
    for (ri, &root) in roots.iter().enumerate() {
        let lo = if ri == 0 {
            0
        } else {
            let prev = roots[ri - 1];
            prev + (root - prev) / 2 + 1
        };
        let hi = match roots.get(ri + 1) {
            None => 127,
            Some(&next) => root + (next - root) / 2,
        };
        // Files of this root (files without a key go to the first root).
        let mut group: Vec<(&SampleSource, &str)> = items
            .iter()
            .filter(|i| i.2 == Some(root) || (i.2.is_none() && ri == 0))
            .map(|i| (i.0, i.1))
            .collect();
        group.sort_by_key(|(_, n)| layer_hint(n).0.unwrap_or(i32::MIN));
        // Distinct layers in order; a file without a hint is its own layer.
        let mut layers: Vec<Vec<&SampleSource>> = Vec::new();
        let mut last_hint: Option<i32> = None;
        for (s, n) in &group {
            let hint = layer_hint(n).0;
            if hint.is_some() && hint == last_hint {
                if let Some(l) = layers.last_mut() {
                    l.push(s);
                    continue;
                }
            }
            layers.push(vec![s]);
            last_hint = hint;
        }
        let n = layers.len().max(1) as f32;
        for (li, layer) in layers.iter().enumerate() {
            for s in layer {
                let mut z = KeyZone::new((*s).clone(), root);
                z.key_lo = lo;
                z.key_hi = hi;
                z.vel_lo = if li == 0 { 0.0 } else { li as f32 / n };
                z.vel_hi = if li + 1 == layers.len() {
                    1.0
                } else {
                    (li + 1) as f32 / n - 1e-4
                };
                zones.push(z);
            }
        }
    }
    zones
}

/// Key of a sliced loop's first slice.
pub const FIRST_SLICE_KEY: u8 = 48;
/// Most slices of a loop (keys up to 127).
pub const MAX_SLICES: usize = 128 - FIRST_SLICE_KEY as usize;

/// Zones for the slices of `sample` (`frames` long) that start at `starts` (frames, rising):
/// slice `k` plays on key [`FIRST_SLICE_KEY`]` + k` at its original pitch, from its start to
/// the next slice's.
pub fn slice_zones(sample: &SampleSource, starts: &[usize], frames: usize) -> Vec<KeyZone> {
    let frames = frames.max(1) as f64;
    let n = starts.len().min(MAX_SLICES);
    (0..n)
        .map(|k| {
            let key = FIRST_SLICE_KEY + k as u8;
            let end = starts.get(k + 1).map_or(frames, |&e| e as f64);
            KeyZone {
                key_lo: key,
                key_hi: key,
                start: (starts[k] as f64 / frames) as f32,
                end: (end / frames) as f32,
                ..KeyZone::new(sample.clone(), key)
            }
        })
        .collect()
}

/// A pattern that replays a sliced loop `beats` long: one note per slice zone, at the slice's
/// place in the loop and as long as it. At any tempo each slice starts on its beat.
pub fn slice_notes(zones: &[KeyZone], beats: f64) -> Vec<Note> {
    let ticks = beats * PPQ as f64;
    zones
        .iter()
        .map(|z| {
            let start = (f64::from(z.start) * ticks).round() as i64;
            let end = (f64::from(z.end) * ticks).round() as i64;
            Note::new(start, (end - start).max(1), z.root, 0.8)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn src(n: &str) -> (SampleSource, String) {
        (SampleSource::File(PathBuf::from(n)), n.to_owned())
    }

    #[test]
    fn note_names_in_file_names() {
        assert_eq!(key_in_name("Piano C4.wav"), Some(60));
        assert_eq!(key_in_name("piano_F#3_v2.wav"), Some(54));
        assert_eq!(key_in_name("Bb2-soft.flac"), Some(46));
        assert_eq!(key_in_name("kick_036.wav"), Some(36));
        assert_eq!(key_in_name("bass C-1.wav"), Some(0));
        assert_eq!(key_in_name("pad 2024.wav"), None);
        assert_eq!(key_in_name("snare.wav"), None);
    }

    #[test]
    fn folders_map_to_key_ranges_and_velocity_layers() {
        let z = map_folder(&[
            src("Pno C4 pp.wav"),
            src("Pno C4 ff.wav"),
            src("Pno E4 pp.wav"),
            src("Pno E4 ff.wav"),
            src("Pno G4 ff rr1.wav"),
            src("Pno G4 ff rr2.wav"),
        ]);
        assert_eq!(z.len(), 6);
        let at = |key: u8, v: f32| -> Vec<String> {
            z.iter()
                .filter(|z| z.matches(key, v))
                .map(|z| match &z.sample {
                    SampleSource::File(p) => p.display().to_string(),
                    SampleSource::BuiltIn(_) => String::new(),
                })
                .collect()
        };
        // C4 covers up to half-way to E4; E4 from there to half-way to G4.
        assert_eq!(at(61, 0.2), ["Pno C4 pp.wav"]);
        assert_eq!(at(62, 0.9), ["Pno C4 ff.wav"]);
        assert_eq!(at(63, 0.9), ["Pno E4 ff.wav"]);
        assert_eq!(at(0, 0.9), ["Pno C4 ff.wav"]);
        // The two G4 takes share one layer (round robin).
        assert_eq!(at(127, 0.1), ["Pno G4 ff rr1.wav", "Pno G4 ff rr2.wav"]);
        // Every key and velocity finds a zone.
        for k in 0..=127 {
            for v in [0.0, 0.5, 1.0] {
                assert!(!at(k, v).is_empty(), "{k} {v}");
            }
        }
    }

    #[test]
    fn unnamed_files_go_chromatic() {
        let z = map_folder(&[src("b.wav"), src("a.wav")]);
        assert_eq!((z[0].root, z[1].root), (60, 61));
        assert!(z[0].matches(0, 0.5) && z[1].matches(127, 0.5));
    }

    #[test]
    fn slices_map_to_keys_and_replay_on_the_beat() {
        let src = SampleSource::File("loop.wav".into());
        let z = slice_zones(&src, &[0, 24_000, 60_000], 96_000);
        assert_eq!(z.len(), 3);
        assert_eq!(
            (z[1].key_lo, z[1].root),
            (FIRST_SLICE_KEY + 1, FIRST_SLICE_KEY + 1)
        );
        assert_eq!((z[1].start, z[1].end), (0.25, 0.625));
        assert_eq!(z[2].end, 1.0);
        let n = slice_notes(&z, 4.0);
        assert_eq!(
            n.iter()
                .map(|n| (n.start, n.length, n.key))
                .collect::<Vec<_>>(),
            [
                (0, PPQ, 48),
                (PPQ, PPQ * 3 / 2, 49),
                (PPQ * 5 / 2, PPQ * 3 / 2, 50)
            ]
        );
    }
}
