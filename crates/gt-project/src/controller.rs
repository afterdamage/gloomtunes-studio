//! Controller mapping files (Prompt 33, D149): which knob, fader, pad or button of a known
//! controller does what.
//!
//! A mapping is a JSON file (`*.gtmap.json`) in the controllers folder of the app's data
//! folder, plus the generic mapping built in ([`ControllerMap::generic`]). It names the input
//! ports it is for (case-insensitive parts of the port name; none means any port) and lists
//! its controls:
//!
//! ```json
//! {
//!   "name": "My keyboard",
//!   "ports": ["MPK mini"],
//!   "controls": [
//!     { "name": "Knob 1", "type": "knob", "cc": 70, "param": "channel/selected/volume" },
//!     { "name": "Fader 1", "type": "fader", "channel": 1, "cc": 7, "param": "mixer/1/volume" },
//!     { "name": "Endless", "type": "knob", "cc": 71, "relative": true,
//!       "param": ["channel/selected/synth/filter.cutoff", "channel/selected/cutoff"] },
//!     { "name": "Play", "type": "button", "cc": 118, "transport": "play" },
//!     { "name": "Pad 1", "type": "pad", "note": 44, "play_note": 36 },
//!     { "name": "Scene", "type": "button", "note": 40, "script": true }
//!   ],
//!   "script_file": "my keyboard.rhai"
//! }
//! ```
//!
//! A `param` is a parameter key ([`ParamId::key`]) where `channel/selected` stands for the
//! channel selected in the rack and `mixer/selected` for the insert it plays into; with a list,
//! the first that exists on the selected channel is used (a synth's cutoff, else a sampler's).
//! MIDI channels are 1 to 16 and optional (any channel). `relative` reads two's-complement
//! increments (1 to 63 up, 65 to 127 down) from endless encoders. Controls marked `script`, and
//! every controller message of the port, go to the mapping's script ([`script`] or
//! `script_file`) first.

use std::path::Path;

use gt_core::{ChannelId, ParamId};
use serde::Deserialize;

/// What kind of control it is (for display; every kind works the same way).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ControlType {
    #[default]
    Knob,
    Fader,
    Pad,
    Button,
}

/// The message a control sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlSource {
    /// A controller number.
    Cc(u8),
    /// A note (pads, and buttons that send notes).
    Note(u8),
}

/// A transport button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportButton {
    /// Play, or pause while playing.
    Play,
    Stop,
    Record,
    /// Loop on or off.
    Loop,
    /// Metronome on or off.
    Metronome,
    /// Select the next or previous rack channel.
    NextChannel,
    PrevChannel,
    /// Select the next or previous pattern.
    NextPattern,
    PrevPattern,
}

/// What a control does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlAction {
    /// Moves a parameter (the first of the keys that exists).
    Param(Vec<String>),
    /// A transport button (acts when pressed).
    Transport(TransportButton),
    /// A pad that plays this key on the selected channel.
    PlayNote(u8),
    /// Handled by the mapping's script only.
    Script,
}

/// One control of a mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlMapping {
    pub name: String,
    pub kind: ControlType,
    /// MIDI channel 0 to 15, or any.
    pub channel: Option<u8>,
    pub source: ControlSource,
    pub action: ControlAction,
    /// An endless encoder sending increments.
    pub relative: bool,
}

/// A controller mapping.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ControllerMap {
    pub name: String,
    /// Parts of the port names it is for; empty: any port.
    pub ports: Vec<String>,
    pub controls: Vec<ControlMapping>,
    /// Script source, if any.
    pub script: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MapDto {
    name: String,
    #[serde(default)]
    ports: Vec<String>,
    #[serde(default)]
    controls: Vec<ControlDto>,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    script_file: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Keys {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlDto {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type")]
    kind: ControlType,
    #[serde(default)]
    channel: Option<u8>,
    #[serde(default)]
    cc: Option<u8>,
    #[serde(default)]
    note: Option<u8>,
    #[serde(default)]
    param: Option<Keys>,
    #[serde(default)]
    transport: Option<TransportButton>,
    #[serde(default)]
    play_note: Option<u8>,
    #[serde(default)]
    script: bool,
    #[serde(default)]
    relative: bool,
}

/// The mapping every port gets unless a mapping of its own matches.
const GENERIC: &str = include_str!("../controllers/generic.gtmap.json");

/// The file extension of mapping files.
pub const EXTENSION: &str = ".gtmap.json";

impl ControllerMap {
    /// Parses a mapping. `dir` is where a `script_file` is looked for.
    pub fn parse(json: &str, dir: Option<&Path>) -> Result<Self, String> {
        let dto: MapDto = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let mut controls = Vec::new();
        for (i, c) in dto.controls.into_iter().enumerate() {
            let label = if c.name.is_empty() {
                format!("control {}", i + 1)
            } else {
                c.name.clone()
            };
            let source = match (c.cc, c.note) {
                (Some(cc), None) if cc < 128 => ControlSource::Cc(cc),
                (None, Some(n)) if n < 128 => ControlSource::Note(n),
                _ => {
                    return Err(format!(
                        "{label}: give one of \"cc\" or \"note\" (0 to 127)"
                    ))
                }
            };
            let channel = match c.channel {
                None => None,
                Some(ch @ 1..=16) => Some(ch - 1),
                Some(_) => return Err(format!("{label}: \"channel\" is 1 to 16")),
            };
            let actions = [
                c.param.is_some(),
                c.transport.is_some(),
                c.play_note.is_some(),
                c.script,
            ];
            if actions.iter().filter(|&&a| a).count() != 1 {
                return Err(format!(
                    "{label}: give exactly one of \"param\", \"transport\", \"play_note\" or \
                     \"script\""
                ));
            }
            let action = if let Some(keys) = c.param {
                let keys = match keys {
                    Keys::One(k) => vec![k],
                    Keys::Many(k) => k,
                };
                if let Some(bad) = keys
                    .iter()
                    .find(|k| resolve(k, Some(ChannelId(0)), 0).is_none())
                {
                    return Err(format!("{label}: unknown parameter \"{bad}\""));
                }
                ControlAction::Param(keys)
            } else if let Some(t) = c.transport {
                ControlAction::Transport(t)
            } else if let Some(k) = c.play_note {
                ControlAction::PlayNote(k.min(127))
            } else {
                ControlAction::Script
            };
            controls.push(ControlMapping {
                name: c.name,
                kind: c.kind,
                channel,
                source,
                action,
                relative: c.relative,
            });
        }
        let script = match (dto.script, dto.script_file) {
            (Some(s), None) => Some(s),
            (None, Some(f)) => {
                let path = dir.map_or_else(|| Path::new(&f).to_path_buf(), |d| d.join(&f));
                Some(
                    std::fs::read_to_string(&path)
                        .map_err(|e| format!("script {}: {e}", path.display()))?,
                )
            }
            (None, None) => None,
            (Some(_), Some(_)) => return Err("give \"script\" or \"script_file\", not both".into()),
        };
        Ok(Self {
            name: dto.name,
            ports: dto.ports,
            controls,
            script,
        })
    }

    /// The built-in generic mapping.
    pub fn generic() -> Self {
        Self::parse(GENERIC, None).unwrap_or_default()
    }

    /// True when the mapping is for input port `port`.
    pub fn matches(&self, port: &str) -> bool {
        let port = port.to_lowercase();
        self.ports.is_empty() || self.ports.iter().any(|p| port.contains(&p.to_lowercase()))
    }

    /// The control that `source` on MIDI `channel` is.
    pub fn find(&self, channel: u8, source: ControlSource) -> Option<&ControlMapping> {
        self.controls
            .iter()
            .find(|c| c.source == source && c.channel.is_none_or(|ch| ch == channel))
    }

    /// Every note a control of this mapping takes (so it does not also play), with its
    /// channel.
    pub fn notes(&self) -> impl Iterator<Item = (Option<u8>, u8)> + '_ {
        self.controls.iter().filter_map(|c| match c.source {
            ControlSource::Note(n) => Some((c.channel, n)),
            ControlSource::Cc(_) => None,
        })
    }
}

/// The parameter `key` names, with `channel/selected` and `mixer/selected` filled in. `None`
/// for a malformed key, or a selected channel when none is.
pub fn resolve(key: &str, selected: Option<ChannelId>, selected_strip: usize) -> Option<ParamId> {
    let key = if let Some(rest) = key.strip_prefix("channel/selected") {
        format!("channel/{}{rest}", selected?.0)
    } else if let Some(rest) = key.strip_prefix("mixer/selected") {
        format!("mixer/{selected_strip}{rest}")
    } else {
        key.to_owned()
    };
    ParamId::parse(&key)
}

/// The new normalized value of a control: the value itself, or the last one moved by an
/// encoder's increment.
pub fn control_value(relative: bool, value: u8, last: f32) -> f32 {
    if !relative {
        return f32::from(value.min(127)) / 127.0;
    }
    let step = match value & 0x7F {
        v @ 1..=63 => f32::from(v),
        v @ 65..=127 => f32::from(v) - 128.0,
        _ => 0.0,
    };
    (last + step / 128.0).clamp(0.0, 1.0)
}

/// Loads every mapping file in `dir`, sorted by name. Unreadable files come back as errors
/// (file name and reason) and are skipped.
pub fn load_folder(dir: &Path) -> (Vec<ControllerMap>, Vec<String>) {
    let mut maps = Vec::new();
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (maps, errors);
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(EXTENSION))
        .collect();
    paths.sort();
    for p in paths {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
        match std::fs::read_to_string(&p)
            .map_err(|e| e.to_string())
            .and_then(|t| ControllerMap::parse(&t, p.parent()))
        {
            Ok(m) => maps.push(m),
            Err(e) => errors.push(format!("{}: {e}", name.unwrap_or_default())),
        }
    }
    (maps, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generic_mapping_parses_and_covers_knobs_faders_and_transport() {
        let g = ControllerMap::parse(GENERIC, None).unwrap();
        assert!(g.matches("Any Keyboard 25"));
        let kinds: Vec<ControlType> = g.controls.iter().map(|c| c.kind).collect();
        for k in [ControlType::Knob, ControlType::Fader, ControlType::Button] {
            assert!(kinds.contains(&k), "{k:?}");
        }
        let play = g.find(0, ControlSource::Cc(118)).unwrap();
        assert_eq!(play.action, ControlAction::Transport(TransportButton::Play));
        // Every parameter key resolves for a selected channel.
        for c in &g.controls {
            if let ControlAction::Param(keys) = &c.action {
                assert!(keys
                    .iter()
                    .all(|k| resolve(k, Some(ChannelId(3)), 2).is_some()));
            }
        }
    }

    #[test]
    fn a_mapping_file_parses_every_kind_of_control() {
        let json = r#"{
            "name": "Test pad",
            "ports": ["MPK"],
            "controls": [
                { "name": "K1", "type": "knob", "cc": 70, "param": "channel/selected/volume" },
                { "name": "F1", "type": "fader", "channel": 2, "cc": 7, "param": "mixer/1/volume" },
                { "name": "E1", "cc": 71, "relative": true,
                  "param": ["channel/selected/synth/filter.cutoff", "channel/selected/cutoff"] },
                { "name": "Stop", "type": "button", "cc": 117, "transport": "stop" },
                { "name": "P1", "type": "pad", "note": 44, "play_note": 36 },
                { "name": "S", "type": "button", "note": 40, "script": true }
            ],
            "script": "fn on_cc(ch, cc, v) { false }"
        }"#;
        let m = ControllerMap::parse(json, None).unwrap();
        assert!(m.matches("akai mpk mini mk3") && !m.matches("Launchpad"));
        assert_eq!(m.controls.len(), 6);
        // Channel 2 in the file is channel 1 inside; other channels do not match.
        assert!(m.find(1, ControlSource::Cc(7)).is_some());
        assert!(m.find(0, ControlSource::Cc(7)).is_none());
        assert_eq!(
            m.find(9, ControlSource::Note(44)).unwrap().action,
            ControlAction::PlayNote(36)
        );
        assert_eq!(m.notes().collect::<Vec<_>>(), vec![(None, 44), (None, 40)]);
        assert!(m.script.is_some());
        assert_eq!(
            resolve("channel/selected/volume", Some(ChannelId(7)), 0).map(|p| p.key()),
            Some("channel/7/volume".to_owned())
        );
        assert_eq!(
            resolve("mixer/selected/pan", None, 4).map(|p| p.key()),
            Some("mixer/4/pan".to_owned())
        );
        assert_eq!(resolve("channel/selected/volume", None, 0), None);
    }

    #[test]
    fn mistakes_are_reported_with_the_control() {
        let bad = |c: &str| {
            ControllerMap::parse(&format!(r#"{{"name":"x","controls":[{c}]}}"#), None).unwrap_err()
        };
        assert!(bad(r#"{"name":"A","cc":1}"#).contains("A: give exactly one"));
        assert!(bad(r#"{"name":"B","cc":1,"note":2,"script":true}"#).contains("B: give one"));
        assert!(
            bad(r#"{"name":"C","cc":1,"param":"channel/selected/nope"}"#)
                .contains("unknown parameter")
        );
        assert!(bad(r#"{"name":"D","cc":1,"channel":17,"script":true}"#).contains("1 to 16"));
        assert!(bad(r#"{"name":"E","cc":1,"script":true,"colour":1}"#).contains("unknown field"));
    }

    #[test]
    fn encoders_step_and_absolute_controls_scale() {
        assert_eq!(control_value(false, 127, 0.3), 1.0);
        assert_eq!(control_value(false, 0, 0.3), 0.0);
        assert!((control_value(true, 2, 0.5) - (0.5 + 2.0 / 128.0)).abs() < 1e-6);
        assert!((control_value(true, 126, 0.5) - (0.5 - 2.0 / 128.0)).abs() < 1e-6);
        assert_eq!(control_value(true, 64, 0.5), 0.5);
        assert_eq!(control_value(true, 127, 0.0), 0.0);
    }

    #[test]
    fn a_folder_loads_good_files_and_reports_bad_ones() {
        let dir = std::env::temp_dir().join(format!("gt-maps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.rhai"), "fn on_cc(c, n, v) { true }").unwrap();
        std::fs::write(
            dir.join("good.gtmap.json"),
            r#"{"name":"Good","script_file":"b.rhai"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("bad.gtmap.json"), "{").unwrap();
        std::fs::write(dir.join("other.json"), "{").unwrap();
        let (maps, errors) = load_folder(&dir);
        assert_eq!(maps.len(), 1);
        assert_eq!(
            maps[0].script.as_deref(),
            Some("fn on_cc(c, n, v) { true }")
        );
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("bad.gtmap.json"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
