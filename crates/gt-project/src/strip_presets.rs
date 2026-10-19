//! Mixer-strip and effect-chain preset files (Prompt 29).
//!
//! A strip preset (`.gtstrip`) holds a strip's sound without its routing: name, fader, pan,
//! polarity, colour, icon and the ten effect slots. A chain preset (`.gtchain`) holds only the
//! slots. Both are JSON with the slots written exactly as in a project file:
//!
//! ```json
//! { "format": "gloomtunes-strip", "version": 1, "name": "Vocal bus", "volume": 0.8,
//!   "pan": 0.0, "slots": [ { "kind": "eq", "enabled": true, "params": { ... } }, null, ... ] }
//! ```
//!
//! Plugin slots keep the plugin and its parameter values but not its saved state (that lives
//! in a project's container). Unknown effects are dropped with a warning.

use std::path::{Path, PathBuf};

use gt_core::{MixerStrip, StripPreset, FX_SLOTS};
use serde::{Deserialize, Serialize};

use crate::file::{plugin_from_dto, slots_dto, slots_from_dto, EffectDto};
use crate::presets::PresetError;

/// Extension of strip presets.
pub const STRIP_EXTENSION: &str = "gtstrip";
/// Extension of effect-chain presets.
pub const CHAIN_EXTENSION: &str = "gtchain";
const STRIP_FORMAT: &str = "gloomtunes-strip";
const CHAIN_FORMAT: &str = "gloomtunes-chain";
const VERSION: u32 = 1;

/// Which kind of preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The whole strip's sound.
    Strip,
    /// Only the effect slots.
    Chain,
}

impl Kind {
    /// File extension.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Strip => STRIP_EXTENSION,
            Self::Chain => CHAIN_EXTENSION,
        }
    }

    fn format(self) -> &'static str {
        match self {
            Self::Strip => STRIP_FORMAT,
            Self::Chain => CHAIN_FORMAT,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct PresetFile {
    format: String,
    version: u32,
    name: String,
    #[serde(default = "unity", skip_serializing_if = "Option::is_none")]
    volume: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pan: Option<f32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    phase_invert: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<u8>,
    slots: Vec<Option<EffectDto>>,
}

fn unity() -> Option<f32> {
    Some(MixerStrip::DEFAULT_VOLUME)
}

/// The preset as JSON; a chain preset leaves out everything but the name and the slots.
/// `dir` is where the file will be, for relative effect-file paths.
pub fn to_json(p: &StripPreset, kind: Kind, dir: Option<&Path>) -> String {
    let strip = kind == Kind::Strip;
    let f = PresetFile {
        format: kind.format().to_owned(),
        version: VERSION,
        name: p.name.clone(),
        volume: strip.then_some(p.volume),
        pan: strip.then_some(p.pan),
        phase_invert: strip && p.phase_invert,
        color: p.color.filter(|_| strip),
        icon: p.icon.filter(|_| strip),
        slots: slots_dto(&p.slots, dir),
    };
    serde_json::to_string_pretty(&f).expect("preset serializes")
}

/// Reads a preset of `kind` (a strip preset also reads as a chain). Returns the preset and
/// warnings about what was dropped.
pub fn from_json(
    text: &str,
    kind: Kind,
    dir: Option<&Path>,
) -> Result<(StripPreset, Vec<String>), PresetError> {
    let f: PresetFile =
        serde_json::from_str(text).map_err(|e| PresetError::Format(e.to_string()))?;
    let ok = f.format == kind.format() || (kind == Kind::Chain && f.format == STRIP_FORMAT);
    if !ok {
        return Err(PresetError::Format(format!(
            "\"{}\" is not a {} preset",
            f.format,
            kind.extension()
        )));
    }
    if f.version > VERSION {
        return Err(PresetError::Format(format!(
            "made by a newer version (preset version {})",
            f.version
        )));
    }
    let mut warnings = Vec::new();
    let mut plugin = |d, w: &mut Vec<String>| plugin_from_dto(d, w).map(|(p, _state)| p);
    let slots = slots_from_dto(f.slots, dir, &mut warnings, &mut plugin);
    let fin = |v: Option<f32>, lo: f32, hi: f32, d: f32| {
        v.filter(|x| x.is_finite()).map_or(d, |x| x.clamp(lo, hi))
    };
    let preset = StripPreset {
        name: f.name.chars().take(64).collect(),
        volume: fin(
            f.volume,
            0.0,
            MixerStrip::MAX_VOLUME,
            MixerStrip::DEFAULT_VOLUME,
        ),
        pan: fin(f.pan, -1.0, 1.0, 0.0),
        phase_invert: f.phase_invert,
        color: f.color,
        icon: f.icon.filter(|&k| k < gt_core::STRIP_ICONS),
        slots,
    };
    debug_assert_eq!(preset.slots.len(), FX_SLOTS);
    Ok((preset, warnings))
}

/// Writes `p` to `dir/<name>.<ext>` (creating `dir`) and returns the path.
pub fn save(dir: &Path, p: &StripPreset, kind: Kind) -> Result<PathBuf, PresetError> {
    std::fs::create_dir_all(dir).map_err(PresetError::Io)?;
    let stem: String = p
        .name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let stem = stem.trim();
    let stem = if stem.is_empty() { "Strip" } else { stem };
    let path = dir.join(format!("{stem}.{}", kind.extension()));
    std::fs::write(&path, to_json(p, kind, Some(dir))).map_err(PresetError::Io)?;
    Ok(path)
}

/// Reads a preset file.
pub fn load(path: &Path, kind: Kind) -> Result<(StripPreset, Vec<String>), PresetError> {
    let text = std::fs::read_to_string(path).map_err(PresetError::Io)?;
    from_json(&text, kind, path.parent())
}

/// Preset files of `kind` in `dir`, sorted by name.
pub fn list(dir: &Path, kind: Kind) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case(kind.extension()))
        })
        .collect();
    v.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_core::{EffectKind, EffectSlot, Project};

    #[test]
    fn strip_and_chain_presets_round_trip() {
        let p = Project::demo();
        let mut s = p.mixer.strips[gt_core::mixer::DEMO_REVERB].preset();
        s.slots[3] = Some(EffectSlot::new(EffectKind::Eq).with("b2.gain", 4.0));
        s.color = Some([1, 2, 3]);
        s.icon = Some(4);
        s.volume = 0.5;
        let (back, w) = from_json(&to_json(&s, Kind::Strip, None), Kind::Strip, None).unwrap();
        assert!(w.is_empty());
        assert_eq!(back, s);
        // A chain keeps only the slots; a strip preset also loads as a chain.
        let (chain, _) = from_json(&to_json(&s, Kind::Chain, None), Kind::Chain, None).unwrap();
        assert_eq!(chain.slots, s.slots);
        assert_eq!(
            (chain.volume, chain.color),
            (MixerStrip::DEFAULT_VOLUME, None)
        );
        assert!(from_json(&to_json(&s, Kind::Strip, None), Kind::Chain, None).is_ok());
        assert!(from_json(&to_json(&s, Kind::Chain, None), Kind::Strip, None).is_err());
    }

    #[test]
    fn bad_presets_are_refused_or_repaired() {
        assert!(from_json("{", Kind::Strip, None).is_err());
        let text = r#"{ "format": "gloomtunes-strip", "version": 1, "name": "x",
            "volume": 99.0, "pan": -7.0, "icon": 200,
            "slots": [ { "kind": "time-machine", "enabled": true, "params": {} } ] }"#;
        let (p, w) = from_json(text, Kind::Strip, None).unwrap();
        assert_eq!(p.volume, MixerStrip::MAX_VOLUME);
        assert_eq!((p.pan, p.icon), (-1.0, None));
        assert!(p.slots.iter().all(Option::is_none));
        assert_eq!(w.len(), 1);
        let newer = r#"{ "format": "gloomtunes-chain", "version": 9, "name": "x", "slots": [] }"#;
        assert!(from_json(newer, Kind::Chain, None).is_err());
    }

    #[test]
    fn save_list_and_load() {
        let dir = std::env::temp_dir().join(format!("gt-strip-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = gt_core::MixerStrip::new(3).preset();
        s.name = "Drum: bus".into();
        let path = save(&dir, &s, Kind::Strip).unwrap();
        assert!(path.ends_with("Drum_ bus.gtstrip"));
        save(&dir, &s, Kind::Chain).unwrap();
        assert_eq!(list(&dir, Kind::Strip), vec![path.clone()]);
        assert_eq!(list(&dir, Kind::Chain).len(), 1);
        assert_eq!(load(&path, Kind::Strip).unwrap().0, s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
