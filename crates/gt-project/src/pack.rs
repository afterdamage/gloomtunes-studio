//! Sound packs (Prompt 28): a zip holding audio files and a `pack.json` mapping them into
//! presets for the preset player ([`gt_core::Player`]).
//!
//! ```json
//! { "format": 1, "name": "Gloom Keys", "author": "…", "license": "CC0-1.0",
//!   "presets": [ { "name": "Dust Piano", "category": "Keys",
//!     "zones": [ { "file": "samples/c3.flac", "root": 48, "lo": 0, "hi": 53 } ],
//!     "attack_ms": 2, "release_ms": 400, "filter": "lowpass", "cutoff": 6000,
//!     "macros": [ { "name": "Tone", "target": "cutoff", "min": 300, "max": 16000, "value": 0.6 } ]
//!   } ] }
//! ```
//!
//! [`open`] checks everything before anything plays: the manifest's shape and ranges, that every
//! file it names is inside the zip under a safe relative path with an audio extension, and the
//! sizes. The audio is extracted once into a cache folder keyed by the zip's path, size and
//! modification time; zones point there.

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use gt_core::player::MAX_MACROS;
use gt_core::{
    KeyZone, LoopMode, Macro, MacroTarget, Player, SampleSource, SamplerFilterMode, SamplerSettings,
};
use serde::{Deserialize, Serialize};

/// Name of the mapping file inside a pack.
pub const MANIFEST: &str = "pack.json";
/// Pack file extension.
pub const EXTENSION: &str = "gtpack";
/// The manifest format this build reads.
pub const FORMAT: u32 = 1;
/// Most presets in a pack.
pub const MAX_PRESETS: usize = 512;
/// Largest manifest, bytes.
pub const MAX_MANIFEST_BYTES: u64 = 4 << 20;
/// Largest total of audio, bytes once extracted.
pub const MAX_AUDIO_BYTES: u64 = 2 << 30;

/// Why a pack was rejected.
#[derive(Debug)]
pub enum PackError {
    /// Reading or extracting failed.
    Io(std::io::Error),
    /// Not a zip, or a damaged one.
    Zip(String),
    /// No `pack.json` inside.
    NoManifest,
    /// The manifest is not valid JSON of the right shape.
    Manifest(String),
    /// The manifest is well-formed but something in it is wrong.
    Invalid(String),
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Zip(e) => write!(f, "not a readable zip: {e}"),
            Self::NoManifest => write!(f, "no {MANIFEST} inside"),
            Self::Manifest(e) => write!(f, "{MANIFEST}: {e}"),
            Self::Invalid(e) => write!(f, "{MANIFEST}: {e}"),
        }
    }
}

impl std::error::Error for PackError {}

impl From<std::io::Error> for PackError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<zip::result::ZipError> for PackError {
    fn from(e: zip::result::ZipError) -> Self {
        match e {
            zip::result::ZipError::Io(e) => Self::Io(e),
            e => Self::Zip(e.to_string()),
        }
    }
}

/// The manifest, as stored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Must be [`FORMAT`].
    pub format: u32,
    /// Pack name.
    pub name: String,
    /// Who made it.
    #[serde(default)]
    pub author: String,
    /// Licence of the sounds (an SPDX id).
    #[serde(default)]
    pub license: String,
    /// The presets.
    pub presets: Vec<PresetDef>,
}

/// One preset in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PresetDef {
    /// Name, unique in the pack.
    pub name: String,
    /// Browser category.
    #[serde(default)]
    pub category: String,
    /// Key zones.
    pub zones: Vec<ZoneDef>,
    /// Amplitude envelope.
    #[serde(default)]
    pub attack_ms: f32,
    #[serde(default)]
    #[allow(missing_docs)]
    pub decay_ms: f32,
    #[serde(default = "one")]
    #[allow(missing_docs)]
    pub sustain: f32,
    #[serde(default = "release")]
    #[allow(missing_docs)]
    pub release_ms: f32,
    /// `off`, `lowpass`, `highpass` or `bandpass`.
    #[serde(default)]
    pub filter: String,
    /// Filter cutoff, Hz.
    #[serde(default = "cutoff")]
    pub cutoff: f32,
    /// Filter resonance, 0 to 1.
    #[serde(default)]
    pub resonance: f32,
    /// Loop each zone's sample.
    #[serde(default, rename = "loop")]
    pub looped: bool,
    /// Macro knobs, at most [`MAX_MACROS`].
    #[serde(default)]
    pub macros: Vec<MacroDef>,
}

/// One zone in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ZoneDef {
    /// Path of the audio inside the zip.
    pub file: String,
    /// Key at which it plays at its recorded pitch.
    pub root: u32,
    /// Lowest key.
    #[serde(default)]
    pub lo: u32,
    /// Highest key.
    #[serde(default = "top_key")]
    pub hi: u32,
    /// Lowest velocity, 0 to 1.
    #[serde(default)]
    pub vel_lo: f32,
    /// Highest velocity, 0 to 1.
    #[serde(default = "one")]
    pub vel_hi: f32,
    /// Fine tune, semitones.
    #[serde(default)]
    pub tune: f32,
    /// Linear gain.
    #[serde(default = "one")]
    pub gain: f32,
}

/// One macro in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MacroDef {
    /// Knob label.
    pub name: String,
    /// A [`MacroTarget::key`].
    pub target: String,
    /// Target value at the knob's minimum.
    pub min: f32,
    /// Target value at the knob's maximum.
    pub max: f32,
    /// Starting knob position, 0 to 1.
    #[serde(default = "half")]
    pub value: f32,
}

fn one() -> f32 {
    1.0
}
fn half() -> f32 {
    0.5
}
fn release() -> f32 {
    200.0
}
fn cutoff() -> f32 {
    20_000.0
}
fn top_key() -> u32 {
    127
}

/// An opened pack.
#[derive(Debug, Clone)]
pub struct Pack {
    /// The zip.
    pub path: PathBuf,
    /// Its manifest, checked.
    pub manifest: Manifest,
    /// Where its audio was extracted.
    pub dir: PathBuf,
}

impl Pack {
    /// Preset names, in order.
    pub fn preset_names(&self) -> impl Iterator<Item = &str> {
        self.manifest.presets.iter().map(|p| p.name.as_str())
    }

    /// A player for preset `name`, its macros at their starting positions.
    pub fn player(&self, name: &str) -> Option<Player> {
        let def = self.manifest.presets.iter().find(|p| p.name == name)?;
        Some(Player {
            pack: self.path.clone(),
            preset: def.name.clone(),
            base: self.settings(def),
            macros: def.macros.iter().map(to_macro).collect(),
        })
    }

    /// Fill `player`'s base from this pack, keeping its macros. False if the pack has no preset
    /// of its name.
    pub fn refill(&self, player: &mut Player) -> bool {
        match self
            .manifest
            .presets
            .iter()
            .find(|p| p.name == player.preset)
        {
            Some(def) => {
                player.base = self.settings(def);
                true
            }
            None => false,
        }
    }

    fn settings(&self, def: &PresetDef) -> SamplerSettings {
        let mut s = SamplerSettings::default();
        s.adsr.attack_ms = def.attack_ms;
        s.adsr.decay_ms = def.decay_ms;
        s.adsr.sustain = def.sustain;
        s.adsr.release_ms = def.release_ms;
        s.filter.mode = filter_mode(&def.filter).unwrap_or_default();
        s.filter.cutoff = def.cutoff;
        s.filter.resonance = def.resonance;
        if def.looped {
            s.loop_mode = LoopMode::Loop;
        }
        s.zones = def
            .zones
            .iter()
            .map(|z| KeyZone {
                key_lo: z.lo as u8,
                key_hi: z.hi as u8,
                vel_lo: z.vel_lo,
                vel_hi: z.vel_hi,
                tune: z.tune,
                gain: z.gain,
                ..KeyZone::new(SampleSource::File(self.dir.join(&z.file)), z.root as u8)
            })
            .collect();
        s
    }
}

fn to_macro(m: &MacroDef) -> Macro {
    Macro {
        name: m.name.clone(),
        target: MacroTarget::from_key(&m.target).unwrap_or_default(),
        min: m.min,
        max: m.max,
        value: m.value,
    }
}

fn filter_mode(name: &str) -> Option<SamplerFilterMode> {
    Some(match name {
        "" | "off" => SamplerFilterMode::Off,
        "lowpass" => SamplerFilterMode::LowPass,
        "highpass" => SamplerFilterMode::HighPass,
        "bandpass" => SamplerFilterMode::BandPass,
        _ => return None,
    })
}

/// True for a relative path made only of plain names (no `..`, root, drive or empty parts),
/// with an audio extension.
fn safe_audio_path(p: &str) -> bool {
    let plain = !p.is_empty()
        && !p.contains('\\')
        && !p.contains(':')
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..");
    let audio = Path::new(p)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| crate::AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
    plain && audio
}

/// Check a manifest against the files in its zip (`sizes`: entry name to uncompressed size).
pub fn validate(m: &Manifest, sizes: &HashMap<String, u64>) -> Result<(), PackError> {
    let bad = |s: String| Err(PackError::Invalid(s));
    if m.format != FORMAT {
        return bad(format!("format {} (this version reads {FORMAT})", m.format));
    }
    if m.name.trim().is_empty() {
        return bad("the pack has no name".into());
    }
    if m.presets.is_empty() || m.presets.len() > MAX_PRESETS {
        return bad(format!("{} presets (1 to {MAX_PRESETS})", m.presets.len()));
    }
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    let mut counted = BTreeSet::new();
    for p in &m.presets {
        let at = |what: &str| format!("preset \"{}\": {what}", p.name);
        if p.name.trim().is_empty() {
            return bad("a preset has no name".into());
        }
        if !names.insert(p.name.as_str()) {
            return bad(at("named twice"));
        }
        if p.zones.is_empty() || p.zones.len() > SamplerSettings::MAX_ZONES {
            return bad(at("needs 1 to 256 zones"));
        }
        let finite = [
            p.attack_ms,
            p.decay_ms,
            p.sustain,
            p.release_ms,
            p.cutoff,
            p.resonance,
        ]
        .iter()
        .all(|x| x.is_finite());
        let in_range = (0.0..=10_000.0).contains(&p.attack_ms)
            && (0.0..=10_000.0).contains(&p.decay_ms)
            && (0.0..=1.0).contains(&p.sustain)
            && (0.0..=20_000.0).contains(&p.release_ms)
            && (20.0..=20_000.0).contains(&p.cutoff)
            && (0.0..=1.0).contains(&p.resonance);
        if !finite || !in_range {
            return bad(at("envelope or filter value out of range"));
        }
        if filter_mode(&p.filter).is_none() {
            return bad(at(&format!("unknown filter \"{}\"", p.filter)));
        }
        for z in &p.zones {
            if !safe_audio_path(&z.file) {
                return bad(at(&format!("unsafe or non-audio file \"{}\"", z.file)));
            }
            let Some(&size) = sizes.get(&z.file) else {
                return bad(at(&format!("\"{}\" is not in the pack", z.file)));
            };
            if counted.insert(z.file.as_str()) {
                total += size;
            }
            let keys = z.root <= 127 && z.lo <= z.hi && z.hi <= 127;
            let vel = (0.0..=1.0).contains(&z.vel_lo)
                && (0.0..=1.0).contains(&z.vel_hi)
                && z.vel_lo <= z.vel_hi;
            let other = z.tune.is_finite()
                && z.tune.abs() <= 48.0
                && z.gain.is_finite()
                && (0.0..=4.0).contains(&z.gain);
            if !(keys && vel && other) {
                return bad(at(&format!("zone \"{}\" out of range", z.file)));
            }
        }
        if p.macros.len() > MAX_MACROS {
            return bad(at(&format!(
                "{} macros (at most {MAX_MACROS})",
                p.macros.len()
            )));
        }
        for mc in &p.macros {
            let Some(t) = MacroTarget::from_key(&mc.target) else {
                return bad(at(&format!("unknown macro target \"{}\"", mc.target)));
            };
            let (lo, hi) = t.range();
            let ok = [mc.min, mc.max]
                .iter()
                .all(|x| x.is_finite() && (lo..=hi).contains(x))
                && (0.0..=1.0).contains(&mc.value)
                && !mc.name.trim().is_empty();
            if !ok {
                return bad(at(&format!("macro \"{}\" out of range", mc.name)));
            }
        }
    }
    if total > MAX_AUDIO_BYTES {
        return bad("more than 2 GB of audio".into());
    }
    Ok(())
}

/// Read and check the manifest of the zip in `reader`; returns it with the entry sizes.
pub fn read_manifest<R: Read + Seek>(
    reader: R,
) -> Result<(Manifest, zip::ZipArchive<R>), PackError> {
    let mut zip = zip::ZipArchive::new(reader)?;
    let mut sizes = HashMap::new();
    for i in 0..zip.len() {
        let f = zip.by_index_raw(i)?;
        if f.is_file() {
            sizes.insert(f.name().to_owned(), f.size());
        }
    }
    let manifest = {
        let entry = zip.by_name(MANIFEST).map_err(|_| PackError::NoManifest)?;
        if entry.size() > MAX_MANIFEST_BYTES {
            return Err(PackError::Invalid("larger than 4 MB".into()));
        }
        let mut json = Vec::new();
        entry.take(MAX_MANIFEST_BYTES).read_to_end(&mut json)?;
        serde_json::from_slice::<Manifest>(&json).map_err(|e| PackError::Manifest(e.to_string()))?
    };
    validate(&manifest, &sizes)?;
    Ok((manifest, zip))
}

/// The folder a pack's audio is extracted to, under `cache`: one per zip path, size and
/// modification time, so a changed pack extracts afresh.
fn cache_dir(path: &Path, cache: &Path) -> std::io::Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    let meta = std::fs::metadata(path)?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    meta.len().hash(&mut h);
    meta.modified().ok().hash(&mut h);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(40)
        .collect();
    Ok(cache.join(format!("{stem}-{:016x}", h.finish())))
}

/// The default cache folder for extracted packs.
pub fn default_cache() -> PathBuf {
    std::env::temp_dir().join("gloomtunes-packs")
}

/// Open the pack at `path`, checking it and extracting its audio under `cache` (skipped when
/// already there).
pub fn open(path: &Path, cache: &Path) -> Result<Pack, PackError> {
    let file = std::fs::File::open(path)?;
    let (manifest, mut zip) = read_manifest(std::io::BufReader::new(file))?;
    let dir = cache_dir(path, cache)?;
    let files: BTreeSet<&str> = manifest
        .presets
        .iter()
        .flat_map(|p| p.zones.iter().map(|z| z.file.as_str()))
        .collect();
    for name in files {
        let out = dir.join(name);
        if out.is_file() {
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut src = zip.by_name(name)?;
        let tmp = out.with_extension("part");
        let mut f = std::fs::File::create(&tmp)?;
        std::io::copy(&mut (&mut src).take(MAX_AUDIO_BYTES), &mut f)?;
        drop(f);
        std::fs::rename(&tmp, &out)?;
    }
    Ok(Pack {
        path: path.to_path_buf(),
        manifest,
        dir,
    })
}

/// Write a pack: `manifest` plus `files` (entry name, bytes). Checks it first.
pub fn write(
    path: &Path,
    manifest: &Manifest,
    files: &[(String, Vec<u8>)],
) -> Result<(), PackError> {
    let sizes = files
        .iter()
        .map(|(n, b)| (n.clone(), b.len() as u64))
        .collect();
    validate(manifest, &sizes)?;
    let json =
        serde_json::to_vec_pretty(manifest).map_err(|e| PackError::Manifest(e.to_string()))?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(std::fs::File::create(path)?));
    let deflate = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file(MANIFEST, deflate)?;
    zip.write_all(&json)?;
    for (name, bytes) in files {
        zip.start_file(name.as_str(), stored)?;
        zip.write_all(bytes)?;
    }
    zip.finish()?.flush()?;
    Ok(())
}

/// The packs (`*.gtpack`) in `dir`, sorted by name.
pub fn list(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == EXTENSION))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn wav() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::new(&mut out, spec).unwrap();
        for i in 0..480 {
            w.write_sample(((i as f32 * 0.1).sin() * 8000.0) as i16)
                .unwrap();
        }
        w.finalize().unwrap();
        out.into_inner()
    }

    pub(crate) fn manifest() -> Manifest {
        Manifest {
            format: FORMAT,
            name: "Test".into(),
            author: "GloomTunes".into(),
            license: "CC0-1.0".into(),
            presets: vec![PresetDef {
                name: "Keys".into(),
                category: "Keys".into(),
                zones: vec![
                    ZoneDef {
                        file: "samples/low.wav".into(),
                        root: 48,
                        lo: 0,
                        hi: 59,
                        vel_lo: 0.0,
                        vel_hi: 1.0,
                        tune: 0.0,
                        gain: 1.0,
                    },
                    ZoneDef {
                        file: "samples/high.wav".into(),
                        root: 72,
                        lo: 60,
                        hi: 127,
                        vel_lo: 0.0,
                        vel_hi: 1.0,
                        tune: 0.0,
                        gain: 1.0,
                    },
                ],
                attack_ms: 1.0,
                decay_ms: 100.0,
                sustain: 0.7,
                release_ms: 300.0,
                filter: "lowpass".into(),
                cutoff: 5000.0,
                resonance: 0.2,
                looped: false,
                macros: vec![MacroDef {
                    name: "Tone".into(),
                    target: "cutoff".into(),
                    min: 300.0,
                    max: 16_000.0,
                    value: 0.5,
                }],
            }],
        }
    }

    pub(crate) fn files() -> Vec<(String, Vec<u8>)> {
        vec![
            ("samples/low.wav".into(), wav()),
            ("samples/high.wav".into(), wav()),
        ]
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gt-pack-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A zip with exactly these entries, unchecked.
    fn raw_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (n, b) in entries {
            zip.start_file(*n, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn a_written_pack_opens_and_plays_its_zones() {
        let d = tmp("ok");
        let path = d.join("test.gtpack");
        write(&path, &manifest(), &files()).unwrap();
        let pack = open(&path, &d.join("cache")).unwrap();
        assert_eq!(pack.preset_names().collect::<Vec<_>>(), ["Keys"]);
        let p = pack.player("Keys").unwrap();
        assert_eq!(p.base.zones.len(), 2);
        assert_eq!(p.macros.len(), 1);
        for z in &p.base.zones {
            let SampleSource::File(f) = &z.sample else {
                panic!()
            };
            assert!(f.is_file() && f.starts_with(&pack.dir));
            assert!(crate::load_sample(f, 48_000).is_ok());
        }
        assert_eq!(p.base.filter.mode, SamplerFilterMode::LowPass);
        // Opening again reuses the extracted audio.
        let again = open(&path, &d.join("cache")).unwrap();
        assert_eq!(again.dir, pack.dir);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn malformed_packs_are_rejected() {
        let d = tmp("bad");
        let cache = d.join("cache");
        let json = |m: &Manifest| serde_json::to_vec(m).unwrap();
        let low = wav();
        type Entries = Vec<(String, Vec<u8>)>;
        let cases: Vec<(&str, Entries)> = {
            let with = |m: Manifest| {
                let mut v = vec![(MANIFEST.to_owned(), json(&m))];
                v.extend(files());
                v
            };
            let mut traversal = manifest();
            traversal.presets[0].zones[0].file = "../evil.wav".into();
            let mut absolute = manifest();
            absolute.presets[0].zones[0].file = "/etc/evil.wav".into();
            let mut not_audio = manifest();
            not_audio.presets[0].zones[0].file = "samples/run.exe".into();
            let mut missing = manifest();
            missing.presets[0].zones[0].file = "samples/gone.wav".into();
            let mut keys = manifest();
            keys.presets[0].zones[0].hi = 200;
            let mut vel = manifest();
            vel.presets[0].zones[0].vel_lo = 0.9;
            vel.presets[0].zones[0].vel_hi = 0.1;
            let mut macros = manifest();
            macros.presets[0].macros = vec![macros.presets[0].macros[0].clone(); 9];
            let mut target = manifest();
            target.presets[0].macros[0].target = "warp".into();
            let mut range = manifest();
            range.presets[0].macros[0].max = 1e9;
            let mut format = manifest();
            format.format = 99;
            let mut twice = manifest();
            twice.presets.push(twice.presets[0].clone());
            let mut empty = manifest();
            empty.presets[0].zones.clear();
            let mut filter = manifest();
            filter.presets[0].filter = "comb".into();
            vec![
                ("traversal", with(traversal)),
                ("absolute", with(absolute)),
                ("not audio", with(not_audio)),
                ("missing file", with(missing)),
                ("key range", with(keys)),
                ("velocity", with(vel)),
                ("nine macros", with(macros)),
                ("unknown target", with(target)),
                ("macro range", with(range)),
                ("format", with(format)),
                ("duplicate preset", with(twice)),
                ("no zones", with(empty)),
                ("filter", with(filter)),
                ("no manifest", vec![("samples/low.wav".into(), low.clone())]),
                (
                    "not json",
                    vec![(MANIFEST.into(), b"{\"format\": 1, \"name\":".to_vec())],
                ),
                (
                    "unknown field",
                    vec![(
                        MANIFEST.into(),
                        br#"{"format":1,"name":"x","presets":[],"evil":1}"#.to_vec(),
                    )],
                ),
            ]
        };
        for (what, entries) in &cases {
            let path = d.join("bad.gtpack");
            let refs: Vec<(&str, &[u8])> =
                entries.iter().map(|(n, b)| (n.as_str(), &b[..])).collect();
            raw_zip(&path, &refs);
            assert!(open(&path, &cache).is_err(), "{what} was accepted");
        }
        // Not a zip at all.
        let path = d.join("junk.gtpack");
        std::fs::write(&path, b"definitely not a zip").unwrap();
        assert!(matches!(open(&path, &cache), Err(PackError::Zip(_))));
        // Nothing escaped the cache.
        assert!(!d.join("evil.wav").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
