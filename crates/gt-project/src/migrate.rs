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

use serde_json::Value;

use crate::file::{FileError, FORMAT};

/// The schema version this build writes.
pub const CURRENT: u32 = 1;

/// One upgrade: rewrites a tree of version `from` into version `from + 1`.
pub type Migration = fn(&mut Value) -> Result<(), String>;

/// Upgrades by version: entry `i` turns version `i + 1` into `i + 2`. Empty while only
/// version 1 exists.
pub const MIGRATIONS: &[Migration] = &[];

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
    fn every_version_has_an_upgrade() {
        assert_eq!(MIGRATIONS.len() as u32, CURRENT - 1);
    }
}
