//! Schema versions of `project.json` and the upgrades between them.
//!
//! Every change to the file layout bumps [`CURRENT`] and adds one function to [`MIGRATIONS`]
//! that rewrites a version-N tree into version N+1 (plus a fixture file of the old version in
//! the tests). Upgrades work on the untyped JSON tree, so old layouts never need Rust types.
//!
//! History:
//! - 1 (Step 9): first released layout.
//! - 1 (Step 10): gained the optional `midi_map` list (MIDI learn). No bump: older files simply
//!   have none, and older builds ignore it.
//! - 2 (Step 11): third-party plugins, as a `plugin` instrument type and a `plugin` effect kind
//!   with its plugin, and `plugins/` state entries in the container. Older builds cannot read
//!   a plugin instrument, so the version is bumped and they refuse the file cleanly ("made with
//!   a newer version"). The upgrade from 1 changes nothing: version 1 files have no plugins.
//! - 3 (Prompt 13): audio recording. Tracks and mixer inserts can name a device input
//!   (`"input": "1-2"`) and audio clips can list the gaps of a recording. Older builds would
//!   open such a file and silently drop the inputs, so the version is bumped. The upgrade from
//!   2 changes nothing: version 2 files have no inputs and no recordings. (Prompt 13 part 2
//!   added optional `takes` to audio clips within version 3, before any release.)
//! - 4 (Prompt 14): audio clips gain `fade_in`, `fade_out` and `reverse`. Older builds would
//!   play such clips without their fades or forwards, so the version is bumped. The upgrade
//!   from 3 changes nothing: version 3 clips have no fades and play forwards.
//! - 5 (Prompt 21): notes gain pan, release velocity, fine pitch, mod X/Y, slide, portamento,
//!   mute and a colour group, written as an object instead of the `[start, length, key,
//!   velocity]` array when any is set. Older builds could not read such a note, so the version
//!   is bumped. The upgrade from 4 changes nothing: version 4 notes are all arrays, which read
//!   back with neutral values.
//! - 6 (Prompt 27): patterns of any length (1 to 512 steps) and a colour, layer channels,
//!   channel swing, choke and filter groups and key settings. Older builds would reset odd
//!   lengths to 16 steps and could not read a layer, so the version is bumped. The upgrade
//!   from 5 changes nothing: every new field is optional.
//! - 7 (Prompt 28): drum synth pads, the bass engine and player channels, all new values.
//! - 8 (Prompt 29): a variable number of inserts and free sends. Each strip's `sends` was a
//!   list of four levels into the send buses (strips 65 to 68); it is now a list of
//!   `{ "to", "level", "pre" }`. The upgrade turns every non-zero level `k` into a post-fader
//!   send to strip `65 + k` (the buses become ordinary inserts, so the file keeps its 68
//!   inserts) and rewrites `mixer/N/send/K` parameter keys to `mixer/N/send/{65 + K}`.
//! - 9 (Prompt 30): folder tracks (`folder`, `collapsed`, `parent`), linked automation clips
//!   (`link`) and several arrangements (`arrangements`, `current_arrangement`). Older builds
//!   would drop all but one arrangement, so the version is bumped. The upgrade from 8 changes
//!   nothing: every new field is optional.
//! - 10 (Prompt 31): tempo ramps (`ramp`, `tension` on tempo points), tempo automation clips
//!   (`tempo` target) and marker actions (`action`). Older builds would play ramps as steps
//!   and markers without their actions, so the version is bumped. The upgrade from 9 changes
//!   nothing.

use serde_json::Value;

use crate::file::{FileError, FORMAT};

/// The schema version this build writes.
pub const CURRENT: u32 = 10;

/// One upgrade: rewrites a tree of version `from` into version `from + 1`.
pub type Migration = fn(&mut Value) -> Result<(), String>;

/// Upgrades by version: entry `i` turns version `i + 1` into `i + 2`.
pub const MIGRATIONS: &[Migration] = &[
    v1_to_v2, v2_to_v3, v3_to_v4, v4_to_v5, v5_to_v6, v6_to_v7, v7_to_v8, v8_to_v9, v9_to_v10,
];

/// First send bus before version 8.
const OLD_FIRST_SEND: u64 = 65;

/// Version 2 added plugins; a version 1 tree is already a valid version 2 tree.
fn v1_to_v2(_v: &mut Value) -> Result<(), String> {
    Ok(())
}

/// Version 3 added device inputs and recording gaps, all optional; a version 2 tree is
/// already a valid version 3 tree.
fn v2_to_v3(_v: &mut Value) -> Result<(), String> {
    Ok(())
}

/// Version 4 added clip fades and reverse, all optional; a version 3 tree is already a valid
/// version 4 tree.
fn v3_to_v4(_v: &mut Value) -> Result<(), String> {
    Ok(())
}

/// Version 5 added per-note values, all optional; a version 4 tree is already a valid
/// version 5 tree whose notes are neutral.
fn v4_to_v5(_v: &mut Value) -> Result<(), String> {
    Ok(())
}

/// Version 6 added rack settings and layer channels, all optional; a version 5 tree is
/// already a valid version 6 tree.
fn v5_to_v6(_v: &mut Value) -> Result<(), String> {
    Ok(())
}

/// Version 7 added drum synth pads, the bass engine and player channels, all new values; a
/// version 6 tree is already a valid version 7 tree.
fn v6_to_v7(_v: &mut Value) -> Result<(), String> {
    Ok(())
}

/// Version 8 replaced the four send levels per strip with a list of sends to any strip.
fn v7_to_v8(v: &mut Value) -> Result<(), String> {
    if let Some(strips) = v
        .get_mut("project")
        .and_then(|p| p.get_mut("mixer"))
        .and_then(Value::as_array_mut)
    {
        for strip in strips {
            let Some(levels) = strip.get("sends").and_then(Value::as_array) else {
                continue;
            };
            // Already a list of sends (a newer tree labelled old): leave it.
            if !levels.iter().all(Value::is_number) {
                continue;
            }
            let sends: Vec<Value> = levels
                .iter()
                .enumerate()
                .filter_map(|(k, l)| Some((k, l.as_f64().filter(|&l| l > 0.0)?)))
                .map(|(k, level)| {
                    serde_json::json!({ "to": OLD_FIRST_SEND + k as u64, "level": level })
                })
                .collect();
            strip["sends"] = Value::Array(sends);
        }
    }
    rename_send_keys(v);
    Ok(())
}

fn v8_to_v9(_: &mut Value) -> Result<(), String> {
    Ok(())
}

fn v9_to_v10(_: &mut Value) -> Result<(), String> {
    Ok(())
}

/// Rewrites every string `mixer/N/send/K` (K below 4) in the tree to `mixer/N/send/{65 + K}`.
fn rename_send_keys(v: &mut Value) {
    match v {
        Value::String(s) => {
            let parts: Vec<&str> = s.split('/').collect();
            if let ["mixer", strip, "send", k] = parts[..] {
                // Version 7 had four sends, 0 to 3.
                if let (Ok(_), Ok(k @ 0..4)) = (strip.parse::<u32>(), k.parse::<u64>()) {
                    *s = format!("mixer/{strip}/send/{}", OLD_FIRST_SEND + k);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(rename_send_keys),
        Value::Object(o) => o.values_mut().for_each(rename_send_keys),
        _ => {}
    }
}

/// Checks the format tag and upgrades `v` to [`CURRENT`]. Returns the tree and the version the
/// file had.
pub fn upgrade(v: Value) -> Result<(Value, u32), FileError> {
    upgrade_with(v, CURRENT, MIGRATIONS)
}

/// [`upgrade`] with an explicit target version and migration list (for tests).
pub fn upgrade_with(
    mut v: Value,
    current: u32,
    migrations: &[Migration],
) -> Result<(Value, u32), FileError> {
    if v.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(FileError::Format(
            "the \"format\" field is missing or wrong".to_owned(),
        ));
    }
    let found = v
        .get("schema_version")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|&n| n >= 1)
        .ok_or_else(|| FileError::Format("no valid \"schema_version\"".to_owned()))?;
    if found > current {
        return Err(FileError::TooNew {
            found,
            supported: current,
        });
    }
    for version in found..current {
        let step = migrations
            .get(version as usize - 1)
            .ok_or_else(|| FileError::Format(format!("no upgrade from version {version}")))?;
        step(&mut v).map_err(|e| {
            FileError::Format(format!("upgrading from version {version} failed: {e}"))
        })?;
        v["schema_version"] = Value::from(version + 1);
    }
    Ok((v, found))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn upgrades_run_in_order_and_newer_files_are_refused() {
        // A pretend history: v1 had "bpm" at the top, v2 moved it under "tempo", v3 renamed
        // "tempo" to "speed".
        fn v1_to_v2(v: &mut Value) -> Result<(), String> {
            let bpm = v["bpm"].take();
            v["tempo"] = json!({ "bpm": bpm });
            Ok(())
        }
        fn v2_to_v3(v: &mut Value) -> Result<(), String> {
            let t = v["tempo"].take();
            v["speed"] = t;
            Ok(())
        }
        let steps: &[Migration] = &[v1_to_v2, v2_to_v3];
        let old = json!({ "format": FORMAT, "schema_version": 1, "bpm": 99 });
        let (new, from) = upgrade_with(old, 3, steps).unwrap();
        assert_eq!(from, 1);
        assert_eq!(new["schema_version"], 3);
        assert_eq!(new["speed"]["bpm"], 99);

        let v2 = json!({ "format": FORMAT, "schema_version": 2, "tempo": { "bpm": 80 } });
        assert_eq!(upgrade_with(v2, 3, steps).unwrap().0["speed"]["bpm"], 80);

        let future = json!({ "format": FORMAT, "schema_version": 4 });
        assert!(matches!(
            upgrade_with(future, 3, steps),
            Err(FileError::TooNew {
                found: 4,
                supported: 3
            })
        ));
        assert!(matches!(
            upgrade(json!({ "format": FORMAT })),
            Err(FileError::Format(_))
        ));
    }

    #[test]
    fn version_7_sends_become_sends_to_the_old_buses() {
        let mut v = json!({ "project": {
            "mixer": [
                { "name": "Master", "sends": [0.0, 0.0, 0.0, 0.0] },
                { "name": "Kick", "sends": [0.0, 0.5, 0.0, 0.25] },
            ],
            "playlist": { "clips": [{ "target": "mixer/1/send/3" }] },
            "modulators": [{ "target": "mixer/1/send/0" }, { "target": "mixer/1/volume" }],
        }});
        v7_to_v8(&mut v).unwrap();
        let p = &v["project"];
        assert_eq!(p["mixer"][0]["sends"], json!([]));
        assert_eq!(
            p["mixer"][1]["sends"],
            json!([{ "to": 66, "level": 0.5 }, { "to": 68, "level": 0.25 }])
        );
        assert_eq!(p["playlist"]["clips"][0]["target"], "mixer/1/send/68");
        assert_eq!(p["modulators"][0]["target"], "mixer/1/send/65");
        assert_eq!(p["modulators"][1]["target"], "mixer/1/volume");
    }

    #[test]
    fn every_version_has_an_upgrade() {
        assert_eq!(MIGRATIONS.len() as u32, CURRENT - 1);
    }
}
