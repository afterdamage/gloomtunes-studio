//! Program settings saved between runs: `settings.json` in the data folder.
//!
//! Unknown or missing fields fall back to their defaults, so older and newer files both load.
//! A file that cannot be parsed is set aside as `settings.json.bad` and defaults are used.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gt_ui::theme::{color_from_hex, color_to_hex};
use gt_ui::{GloomTheme, Keymap};
use serde::{Deserialize, Serialize};

/// Format version written into the file.
const VERSION: u32 = 1;

/// Everything saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Format version.
    pub version: u32,
    /// The first-run wizard has been finished (or skipped).
    pub first_run_done: bool,
    /// Save crash reports (off unless the user turns it on).
    pub crash_reports: bool,
    pub audio: AudioPrefs,
    pub midi: MidiPrefs,
    /// Shortcuts changed from their defaults: command id to keys ("Ctrl+S, Ctrl+Y"; "" for
    /// none).
    pub shortcuts: BTreeMap<String, String>,
    pub theme: ThemePrefs,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: VERSION,
            first_run_done: false,
            crash_reports: false,
            audio: AudioPrefs::default(),
            midi: MidiPrefs::default(),
            shortcuts: BTreeMap::new(),
            theme: ThemePrefs::from_theme(&GloomTheme::gloom()),
        }
    }
}

/// The audio device choice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioPrefs {
    /// Output device by name; `None` follows the system default.
    pub device: Option<String>,
    /// Requested buffer size in frames.
    pub buffer_size: u32,
    /// Requested sample rate; `None` uses the device default.
    pub sample_rate: Option<u32>,
}

impl Default for AudioPrefs {
    fn default() -> Self {
        Self {
            device: None,
            buffer_size: 256,
            sample_rate: None,
        }
    }
}

/// MIDI input and recording preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MidiPrefs {
    /// Input ports the user switched off, by name.
    pub disabled_ports: Vec<String>,
    pub count_in_bars: u8,
    pub record_click: bool,
    /// Extra recording latency compensation in ms.
    pub extra_ms: f32,
    /// Octave of the typing keyboard.
    pub octave: i32,
}

impl Default for MidiPrefs {
    fn default() -> Self {
        let m = gt_ui::views::MidiPanelModel::default();
        Self {
            disabled_ports: Vec::new(),
            count_in_bars: m.count_in_bars,
            record_click: m.record_click,
            extra_ms: m.extra_ms,
            octave: m.octave,
        }
    }
}

/// The theme: every colour as `#rrggbb` by token name, and the sizes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemePrefs {
    pub colors: BTreeMap<String, String>,
    pub font_size: f32,
    pub radius: u8,
}

impl Default for ThemePrefs {
    fn default() -> Self {
        Self::from_theme(&GloomTheme::gloom())
    }
}

impl ThemePrefs {
    pub fn from_theme(theme: &GloomTheme) -> Self {
        let mut t = theme.clone();
        Self {
            colors: t
                .colors_mut()
                .into_iter()
                .map(|(name, c)| (name.to_owned(), color_to_hex(*c)))
                .collect(),
            font_size: theme.font_size,
            radius: theme.radius,
        }
    }

    /// The Gloom theme with these values applied; bad or unknown entries are ignored.
    pub fn to_theme(&self) -> GloomTheme {
        let mut t = GloomTheme::gloom();
        for (name, c) in t.colors_mut() {
            if let Some(v) = self.colors.get(name).and_then(|h| color_from_hex(h)) {
                *c = v;
            }
        }
        t.font_size = self.font_size;
        t.radius = self.radius;
        t.clamp_sizes();
        t
    }
}

impl Settings {
    /// Where the settings live.
    pub fn path() -> PathBuf {
        crate::library::data_folder().join("settings.json")
    }

    /// Reads the settings, or the defaults when there are none yet.
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                log::warn!("cannot read {}: {e}", path.display());
                return Self::default();
            }
        };
        match serde_json::from_str(&text) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("{} is damaged ({e}); using defaults", path.display());
                let _ = std::fs::rename(path, path.with_extension("json.bad"));
                Self::default()
            }
        }
    }

    /// Writes the settings through a temporary file, so a crash never leaves half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    /// The saved keymap.
    pub fn keymap(&self) -> Keymap {
        Keymap::from_pairs(self.shortcuts.iter().map(|(a, b)| (a.as_str(), b.as_str())))
    }

    /// Stores `keymap` (only what differs from the defaults).
    pub fn set_keymap(&mut self, keymap: &Keymap) {
        self.shortcuts = keymap.to_pairs().into_iter().collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_ui::Command;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gt-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn round_trip_through_the_file() {
        let dir = temp_dir("round");
        let path = dir.join("settings.json");
        let mut s = Settings {
            first_run_done: true,
            crash_reports: true,
            ..Settings::default()
        };
        s.audio = AudioPrefs {
            device: Some("USB Interface".into()),
            buffer_size: 128,
            sample_rate: Some(96_000),
        };
        s.midi.disabled_ports = vec!["Through Port".into()];
        let mut theme = GloomTheme::gloom();
        theme.set_accent(GloomTheme::ACCENTS[3].1);
        theme.font_size = 14.0;
        s.theme = ThemePrefs::from_theme(&theme);
        let mut keys = Keymap::default();
        keys.clear(Command::Plugins);
        s.set_keymap(&keys);
        s.save(&path).unwrap();
        let back = Settings::load(&path);
        assert_eq!(back, s);
        assert_eq!(back.theme.to_theme(), theme);
        assert_eq!(back.keymap(), keys);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_fields_take_defaults_and_damage_is_set_aside() {
        let dir = temp_dir("partial");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"crash_reports": true, "future_field": 3}"#).unwrap();
        let s = Settings::load(&path);
        assert!(s.crash_reports);
        assert!(!s.first_run_done);
        assert_eq!(s.audio, AudioPrefs::default());
        assert_eq!(s.theme.to_theme(), GloomTheme::gloom());

        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        assert!(dir.join("settings.json.bad").exists());
        assert_eq!(Settings::load(&dir.join("none.json")), Settings::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bad_theme_values_are_ignored() {
        let mut p = ThemePrefs::default();
        p.colors.insert("accent".into(), "red".into());
        p.colors.insert("nonsense".into(), "#ffffff".into());
        p.font_size = 99.0;
        let t = p.to_theme();
        assert_eq!(t.accent, GloomTheme::gloom().accent);
        assert_eq!(t.font_size, gt_ui::theme::FONT_SIZE_RANGE.1);
    }
}
