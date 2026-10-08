//! Gloom Synth preset files.
//!
//! A preset is a small JSON file (extension `.gloomsynth`) that names every parameter by its
//! stable key, so files stay readable and survive new parameters being added:
//!
//! ```json
//! { "format": "gloomtunes-synth-preset", "version": 1, "name": "Gloom Bass",
//!   "params": { "filter.cutoff": 180.0, "osc1.wave": 2.0, ... },
//!   "mods": [ { "source": "lfo1", "dest": "cutoff", "amount": 0.45 } ] }
//! ```
//!
//! Missing parameters take their default and unknown keys are ignored. Values are clamped into
//! range on load.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gt_core::synth::{ModDest, ModSlot, ModSource, SynthParam, SynthPatch, MOD_SLOTS};
use serde::{Deserialize, Serialize};

/// File extension of preset files.
pub const PRESET_EXTENSION: &str = "gloomsynth";
const FORMAT: &str = "gloomtunes-synth-preset";
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct PresetFile {
    format: String,
    version: u32,
    name: String,
    params: BTreeMap<String, f32>,
    #[serde(default)]
    mods: Vec<ModFile>,
}

#[derive(Serialize, Deserialize)]
struct ModFile {
    source: String,
    dest: String,
    amount: f32,
}

/// Why a preset could not be read or written.
#[derive(Debug)]
pub enum PresetError {
    /// File system error.
    Io(std::io::Error),
    /// Not valid JSON, or not a preset.
    Format(String),
}

impl std::fmt::Display for PresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PresetError::Io(e) => write!(f, "{e}"),
            PresetError::Format(e) => write!(f, "not a Gloom Synth preset: {e}"),
        }
    }
}

impl std::error::Error for PresetError {}

/// The preset as JSON text.
pub fn to_json(patch: &SynthPatch) -> String {
    let file = PresetFile {
        format: FORMAT.to_owned(),
        version: VERSION,
        name: patch.name.clone(),
        params: SynthParam::ALL
            .iter()
            .map(|&p| (p.info().key.to_owned(), patch.get(p)))
            .collect(),
        mods: patch
            .mods
            .iter()
            .filter(|m| m.source != ModSource::Off || m.dest != ModDest::Off)
            .map(|m| ModFile {
                source: m.source.key().to_owned(),
                dest: m.dest.key().to_owned(),
                amount: m.amount,
            })
            .collect(),
    };
    serde_json::to_string_pretty(&file).unwrap_or_default()
}

/// Reads a preset from JSON text.
pub fn from_json(text: &str) -> Result<SynthPatch, PresetError> {
    let file: PresetFile =
        serde_json::from_str(text).map_err(|e| PresetError::Format(e.to_string()))?;
    if file.format != FORMAT {
        return Err(PresetError::Format(format!(
            "format is \"{}\"",
            file.format
        )));
    }
    // Only v1 exists. A later version may rename or reshape fields, which the tolerant reading
    // below would silently default; older versions get a migration here when there are any.
    if file.version != VERSION {
        return Err(PresetError::Format(format!(
            "version {} (this build reads version {VERSION})",
            file.version
        )));
    }
    let mut patch = SynthPatch {
        name: file.name,
        ..SynthPatch::default()
    };
    for (key, v) in &file.params {
        if let Some(p) = SynthParam::from_key(key) {
            patch.set(p, *v);
        }
    }
    for (slot, m) in patch.mods.iter_mut().zip(file.mods.iter().take(MOD_SLOTS)) {
        *slot = ModSlot {
            source: ModSource::from_key(&m.source).unwrap_or_default(),
            dest: ModDest::from_key(&m.dest).unwrap_or_default(),
            amount: m.amount,
        };
    }
    patch.sanitize();
    Ok(patch)
}

/// Writes `patch` to `dir/<name>.gloomsynth` (creating `dir`), returning the path. Characters
/// that are not allowed in file names are replaced.
pub fn save(dir: &Path, patch: &SynthPatch) -> Result<PathBuf, PresetError> {
    std::fs::create_dir_all(dir).map_err(PresetError::Io)?;
    let stem: String = patch
        .name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let stem = stem.trim();
    let stem = if stem.is_empty() { "Preset" } else { stem };
    let path = dir.join(format!("{stem}.{PRESET_EXTENSION}"));
    std::fs::write(&path, to_json(patch)).map_err(PresetError::Io)?;
    Ok(path)
}

/// Reads a preset file.
pub fn load(path: &Path) -> Result<SynthPatch, PresetError> {
    let text = std::fs::read_to_string(path).map_err(PresetError::Io)?;
    from_json(&text)
}

/// Preset files in `dir`, sorted by name. A missing folder gives an empty list.
pub fn list(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case(PRESET_EXTENSION))
        })
        .collect();
    v.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_factory_preset_round_trips() {
        for p in SynthPatch::factory() {
            let back = from_json(&to_json(&p)).unwrap();
            assert_eq!(back, p, "{}", p.name);
        }
    }

    #[test]
    fn missing_and_unknown_keys_and_bad_values_are_tolerated() {
        let text = r#"{ "format": "gloomtunes-synth-preset", "version": 1, "name": "X",
            "params": { "filter.cutoff": 99999.0, "from.the.future": 1.0 },
            "mods": [ { "source": "lfo2", "dest": "nowhere", "amount": 5.0 } ] }"#;
        let p = from_json(text).unwrap();
        assert_eq!(p.get(SynthParam::Cutoff), 20_000.0);
        assert_eq!(
            p.get(SynthParam::Resonance),
            SynthParam::Resonance.info().default
        );
        assert_eq!(p.mods[0].source, ModSource::Lfo2);
        assert_eq!(p.mods[0].dest, ModDest::Off);
        assert_eq!(p.mods[0].amount, 1.0);
        assert!(from_json("{}").is_err());
        assert!(from_json(r#"{"format":"other","version":1,"name":"","params":{}}"#).is_err());
        let future = r#"{"format":"gloomtunes-synth-preset","version":2,"name":"","params":{}}"#;
        assert!(from_json(future).is_err());
    }

    #[test]
    fn save_list_load() {
        let dir = std::env::temp_dir().join(format!("gt-presets-{}", std::process::id()));
        let mut p = SynthPatch::factory()[2].clone();
        p.name = "My: pad?".to_owned();
        let path = save(&dir, &p).unwrap();
        assert_eq!(path.file_name().unwrap(), "My_ pad_.gloomsynth");
        assert_eq!(list(&dir), vec![path.clone()]);
        assert_eq!(load(&path).unwrap(), p);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(list(&dir).is_empty());
    }
}
