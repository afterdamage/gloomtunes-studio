//! The factory sound packs (Prompt 28), rendered from our own synth presets: each source preset
//! is played at five roots an octave apart, encoded as FLAC and mapped into two player presets
//! with eight macros. Everything is CC0.
//!
//! Nothing binary is kept in the repository: the app builds any missing factory pack into its
//! packs folder on first use ([`ensure_factory_packs`]), and the `make_packs` example writes them
//! anywhere.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gt_core::SynthPatch;
use gt_dsp::synth::wavetable::SynthTables;
use gt_dsp::synth::GloomSynth;
use gt_project::pack::{self, MacroDef, Manifest, PackError, PresetDef, ZoneDef, FORMAT};

use crate::formats::Tags;
use crate::{BitDepth, ExportSettings};

/// A pack's files: entry name and bytes.
pub type Files = Vec<(String, Vec<u8>)>;

/// Render rate of the pack samples.
pub const RATE: u32 = 48_000;
/// Roots the sources are sampled at; each zone covers the six keys below and five above it.
pub const ROOTS: [u8; 5] = [36, 48, 60, 72, 84];

/// How the second preset of each source differs from the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// Low-passed, longer release.
    Soft,
    /// Darker filter with a little resonance.
    Dark,
    /// Short decay to a low sustain.
    Short,
}

/// A factory pack: file name, display name, how long each note is held (seconds), the variant
/// and the synth presets it samples.
pub struct FactoryPack {
    /// File name in the packs folder.
    pub file: &'static str,
    /// Pack name.
    pub name: &'static str,
    hold: f32,
    variant: Variant,
    category: &'static str,
    sources: [&'static str; 8],
}

/// The three factory packs.
pub const FACTORY_PACKS: [FactoryPack; 3] = [
    FactoryPack {
        file: "Gloom Keys.gtpack",
        name: "Gloom Keys",
        hold: 1.2,
        variant: Variant::Soft,
        category: "Keys",
        sources: [
            "FM Electric Piano",
            "FM Bell",
            "FM Marimba",
            "FM Glass Keys",
            "Glass Pluck",
            "Velvet Keys",
            "Fog Bell",
            "WT Digital Keys",
        ],
    },
    FactoryPack {
        file: "Gloom Pads.gtpack",
        name: "Gloom Pads",
        hold: 2.5,
        variant: Variant::Dark,
        category: "Pads",
        sources: [
            "Dark Pad",
            "Drift Pad",
            "WT Morph Pad",
            "WT Choir Pad",
            "Rust Strings",
            "Cathedral Pad",
            "FM Pad",
            "Hollow Choir",
        ],
    },
    FactoryPack {
        file: "Gloom Bass and Leads.gtpack",
        name: "Gloom Bass and Leads",
        hold: 1.0,
        variant: Variant::Short,
        category: "Bass and leads",
        sources: [
            "Gloom Bass",
            "Iron Bass",
            "FM Bass",
            "WT Reese",
            "Ember Lead",
            "Moth Lead",
            "WT Vowel Lead",
            "FM Brass",
        ],
    },
];

/// Stereo interleaved render of `patch` playing `key`: held `hold` seconds, then released until
/// silent (at most 4 s more), trimmed and faded at the end.
pub fn render_note(patch: &SynthPatch, key: u8, hold: f32) -> Vec<f32> {
    let mut synth = GloomSynth::new(RATE as f32);
    synth.set_tables(Some(Arc::new(SynthTables::factory())));
    synth.set_settings(&gt_engine::song::synth_settings(patch));
    const BLOCK: usize = 256;
    let (mut l, mut r) = ([0.0f32; BLOCK], [0.0f32; BLOCK]);
    let mut out = Vec::new();
    let held = (hold * RATE as f32) as usize;
    let most = held + 4 * RATE as usize;
    synth.note_on(key, 0.8, 1);
    let mut released = false;
    while out.len() / 2 < most {
        if !released && out.len() / 2 >= held {
            synth.note_off(key);
            released = true;
        }
        l.fill(0.0);
        r.fill(0.0);
        synth.render(&mut l, &mut r);
        for (a, b) in l.iter().zip(&r) {
            out.push(*a);
            out.push(*b);
        }
        if released && !synth.is_active() {
            break;
        }
    }
    let last = out
        .iter()
        .rposition(|x| x.abs() > 1e-4)
        .map_or(0, |i| i / 2 + 1);
    out.truncate(last * 2);
    let fade = (RATE as usize / 100).min(last);
    for i in 0..fade {
        let g = i as f32 / fade as f32;
        let f = last - 1 - i;
        out[2 * f] *= g;
        out[2 * f + 1] *= g;
    }
    out
}

fn flac(audio: &[f32]) -> Result<Vec<u8>, PackError> {
    let settings = ExportSettings {
        sample_rate: RATE,
        depth: BitDepth::Pcm16,
        dither: false,
        ..ExportSettings::default()
    };
    let tags = Tags {
        title: String::new(),
        artist: "GloomTunes Studio (CC0-1.0)".into(),
        genre: String::new(),
    };
    crate::formats::flac_bytes(audio, &settings, 0, &tags)
        .map_err(|e| PackError::Io(std::io::Error::other(e.to_string())))
}

/// The sample files of `source`, peak-normalized together to -2 dBFS.
fn source_files(name: &str, patch: &SynthPatch, hold: f32) -> Result<Files, PackError> {
    let mut notes: Vec<Vec<f32>> = ROOTS.iter().map(|&k| render_note(patch, k, hold)).collect();
    let peak = notes.iter().flatten().fold(0.0f32, |m, x| m.max(x.abs()));
    if peak > 0.0 {
        let g = 0.794 / peak;
        notes.iter_mut().flatten().for_each(|x| *x *= g);
    }
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    ROOTS
        .iter()
        .zip(&notes)
        .map(|(k, n)| Ok((format!("samples/{slug}-{k}.flac"), flac(n)?)))
        .collect()
}

fn zones(files: &[(String, Vec<u8>)]) -> Vec<ZoneDef> {
    files
        .iter()
        .zip(ROOTS)
        .enumerate()
        .map(|(i, ((file, _), root))| ZoneDef {
            file: file.clone(),
            root: u32::from(root),
            lo: if i == 0 { 0 } else { u32::from(root) - 6 },
            hi: if i + 1 == ROOTS.len() {
                127
            } else {
                u32::from(root) + 5
            },
            vel_lo: 0.0,
            vel_hi: 1.0,
            tune: 0.0,
            gain: 1.0,
        })
        .collect()
}

/// Knob position that gives `v` on a `min..max` knob (log for the cutoff).
fn position(target: &str, min: f32, max: f32, v: f32) -> f32 {
    let p = if target == "cutoff" {
        (v / min).ln() / (max / min).ln()
    } else {
        (v - min) / (max - min)
    };
    p.clamp(0.0, 1.0)
}

/// The eight macros, set to the preset's own values.
fn macros(p: &PresetDef) -> Vec<MacroDef> {
    let cutoff = if p.filter.is_empty() {
        20_000.0
    } else {
        p.cutoff
    };
    [
        ("Tone", "cutoff", 150.0, 20_000.0, cutoff),
        ("Reso", "resonance", 0.0, 0.85, p.resonance),
        ("Attack", "attack", 0.0, 2000.0, p.attack_ms),
        ("Decay", "decay", 0.0, 3000.0, p.decay_ms),
        ("Sustain", "sustain", 0.0, 1.0, p.sustain),
        ("Release", "release", 10.0, 4000.0, p.release_ms),
        ("Vibrato", "lfo_depth", 0.0, 0.03, 0.0),
        ("Level", "volume", 0.0, 1.5, 1.0),
    ]
    .into_iter()
    .map(|(name, target, min, max, v)| MacroDef {
        name: name.into(),
        target: target.into(),
        min,
        max,
        value: position(target, min, max, v),
    })
    .collect()
}

fn preset_name(source: &str) -> String {
    source
        .strip_prefix("FM ")
        .or_else(|| source.strip_prefix("WT "))
        .unwrap_or(source)
        .to_owned()
}

/// The manifest and files of `spec`, rendering `limit` of its sources (all when `None`).
pub fn build(spec: &FactoryPack, limit: Option<usize>) -> Result<(Manifest, Files), PackError> {
    let factory = SynthPatch::factory();
    let mut presets = Vec::new();
    let mut files = Vec::new();
    for source in spec.sources.iter().take(limit.unwrap_or(usize::MAX)) {
        let patch = factory
            .iter()
            .find(|p| p.name == *source)
            .ok_or_else(|| PackError::Invalid(format!("no factory preset \"{source}\"")))?;
        let these = source_files(source, patch, spec.hold)?;
        let name = preset_name(source);
        let natural = PresetDef {
            name: name.clone(),
            category: spec.category.into(),
            zones: zones(&these),
            attack_ms: 0.0,
            decay_ms: 0.0,
            sustain: 1.0,
            release_ms: if spec.variant == Variant::Dark {
                1200.0
            } else {
                250.0
            },
            filter: String::new(),
            cutoff: 20_000.0,
            resonance: 0.0,
            looped: false,
            macros: Vec::new(),
        };
        let other = match spec.variant {
            Variant::Soft => PresetDef {
                name: format!("{name} Soft"),
                attack_ms: 15.0,
                release_ms: 900.0,
                filter: "lowpass".into(),
                cutoff: 2500.0,
                ..natural.clone()
            },
            Variant::Dark => PresetDef {
                name: format!("{name} Dark"),
                attack_ms: 300.0,
                release_ms: 2500.0,
                filter: "lowpass".into(),
                cutoff: 900.0,
                resonance: 0.3,
                ..natural.clone()
            },
            Variant::Short => PresetDef {
                name: format!("{name} Short"),
                decay_ms: 300.0,
                sustain: 0.25,
                release_ms: 80.0,
                filter: "lowpass".into(),
                cutoff: 4000.0,
                resonance: 0.2,
                ..natural.clone()
            },
        };
        for mut p in [natural, other] {
            p.macros = macros(&p);
            presets.push(p);
        }
        files.extend(these);
    }
    let manifest = Manifest {
        format: FORMAT,
        name: spec.name.into(),
        author: "GloomTunes Studio".into(),
        license: "CC0-1.0".into(),
        presets,
    };
    Ok((manifest, files))
}

/// Writes every factory pack missing from `dir`; returns the paths written.
pub fn ensure_factory_packs(dir: &Path) -> Result<Vec<PathBuf>, PackError> {
    std::fs::create_dir_all(dir)?;
    let mut written = Vec::new();
    for spec in &FACTORY_PACKS {
        let path = dir.join(spec.file);
        if path.is_file() {
            continue;
        }
        let (manifest, files) = build(spec, None)?;
        let tmp = path.with_extension("part");
        pack::write(&tmp, &manifest, &files)?;
        std::fs::rename(&tmp, &path)?;
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_source_exists_and_names_are_unique() {
        let factory = SynthPatch::factory();
        let mut names = Vec::new();
        for spec in &FACTORY_PACKS {
            for s in spec.sources {
                assert!(factory.iter().any(|p| p.name == s), "{s}");
                names.push(preset_name(s));
            }
        }
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n);
    }

    #[test]
    fn a_built_pack_opens_and_its_samples_decode() {
        let dir = std::env::temp_dir().join(format!("gt-factory-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (manifest, files) = build(&FACTORY_PACKS[0], Some(1)).unwrap();
        assert_eq!(manifest.presets.len(), 2);
        assert!(manifest.presets.iter().all(|p| p.macros.len() == 8));
        let path = dir.join("keys.gtpack");
        pack::write(&path, &manifest, &files).unwrap();
        let opened = pack::open(&path, &dir.join("cache")).unwrap();
        let player = opened.player("Electric Piano").unwrap();
        assert_eq!(player.base.zones.len(), ROOTS.len());
        // The macros start at the preset's own values.
        let s = player.sampler();
        assert!(
            (s.adsr.release_ms - 250.0).abs() < 0.5,
            "{}",
            s.adsr.release_ms
        );
        assert!((s.adsr.sustain - 1.0).abs() < 1e-6);
        for z in &player.base.zones {
            let gt_core::SampleSource::File(f) = &z.sample else {
                panic!()
            };
            let d = gt_project::load_sample(f, RATE).unwrap();
            assert!(d.frames() > RATE as usize / 2, "{}", f.display());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notes_are_audible_and_end_silent() {
        let patch = SynthPatch::factory()
            .into_iter()
            .find(|p| p.name == "Glass Pluck")
            .unwrap();
        let n = render_note(&patch, 60, 0.5);
        let peak = n.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.01, "peak {peak}");
        assert!(n[n.len() - 2..].iter().all(|x| x.abs() < 1e-3));
    }
}
