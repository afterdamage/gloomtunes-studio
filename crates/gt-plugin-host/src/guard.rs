//! Crash protection for plugin code run in the app's own process.
//!
//! Before a risky main-thread call into a plugin (loading its file, creating, activating,
//! restoring state, opening its editor) the host writes an "in flight" marker naming the
//! plugin file, and removes it afterwards. If the app dies inside that call, the marker is
//! still there at the next start: that file is then quarantined (not loaded again until the
//! user allows it), and the user is told why.
//!
//! The host also keeps a list of the plugin files in use while the app runs, removed at a
//! clean exit. If it is still there at start, the last session ended unexpectedly while those
//! plugins were loaded (a crash during audio processing, for instance); the app mentions them
//! when it offers the recovered project and can open it without them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InFlight {
    path: PathBuf,
    plugin: String,
    action: String,
}

/// What the last session left behind.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CrashReport {
    /// The plugin file the app died in, with what it was doing ("loading", "opening the editor").
    pub quarantined: Option<(PathBuf, String, String)>,
    /// Plugin files in use when the last session ended unexpectedly.
    pub in_use: Vec<PathBuf>,
}

impl CrashReport {
    /// True if there is nothing to report.
    pub fn is_empty(&self) -> bool {
        self.quarantined.is_none() && self.in_use.is_empty()
    }
}

/// Markers and the quarantine list, kept in `plugins/` in the data folder.
pub struct Guard {
    dir: PathBuf,
    /// Quarantined files, with the reason.
    quarantine: BTreeMap<PathBuf, String>,
    in_use: Vec<PathBuf>,
}

impl Guard {
    /// Reads the folder and reports what the last session left behind.
    pub fn open(dir: &Path) -> (Self, CrashReport) {
        let mut guard = Self {
            dir: dir.to_owned(),
            quarantine: std::fs::read(dir.join("quarantine.json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default(),
            in_use: Vec::new(),
        };
        let mut report = CrashReport::default();
        let inflight = dir.join("inflight.json");
        if let Some(f) = std::fs::read(&inflight)
            .ok()
            .and_then(|b| serde_json::from_slice::<InFlight>(&b).ok())
        {
            let reason = format!(
                "GloomTunes closed unexpectedly while {} {}",
                f.action, f.plugin
            );
            log::warn!("quarantining {}: {reason}", f.path.display());
            guard.quarantine.insert(f.path.clone(), reason);
            guard.save_quarantine();
            report.quarantined = Some((f.path, f.plugin, f.action));
        }
        let _ = std::fs::remove_file(&inflight);
        report.in_use = std::fs::read(dir.join("session.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let _ = std::fs::remove_file(dir.join("session.json"));
        (guard, report)
    }

    fn save_quarantine(&self) {
        let _ = std::fs::create_dir_all(&self.dir);
        if let Ok(json) = serde_json::to_vec_pretty(&self.quarantine) {
            let _ = std::fs::write(self.dir.join("quarantine.json"), json);
        }
    }

    /// Why `path` is quarantined, if it is.
    pub fn quarantined(&self, path: &Path) -> Option<&str> {
        self.quarantine.get(path).map(String::as_str)
    }

    /// Every quarantined file with the reason.
    pub fn quarantine(&self) -> impl Iterator<Item = (&Path, &str)> {
        self.quarantine
            .iter()
            .map(|(p, r)| (p.as_path(), r.as_str()))
    }

    /// Lets a quarantined file load again.
    pub fn release(&mut self, path: &Path) {
        if self.quarantine.remove(path).is_some() {
            self.save_quarantine();
        }
    }

    /// Runs `f` (a call into plugin code) with the in-flight marker set.
    pub fn run<T>(&self, path: &Path, plugin: &str, action: &str, f: impl FnOnce() -> T) -> T {
        let marker = self.dir.join("inflight.json");
        let _ = std::fs::create_dir_all(&self.dir);
        if let Ok(json) = serde_json::to_vec(&InFlight {
            path: path.to_owned(),
            plugin: plugin.to_owned(),
            action: action.to_owned(),
        }) {
            let _ = std::fs::write(&marker, json);
        }
        let out = f();
        let _ = std::fs::remove_file(&marker);
        out
    }

    /// Records the plugin files in use (written only when the set changes).
    pub fn set_in_use(&mut self, mut files: Vec<PathBuf>) {
        files.sort();
        files.dedup();
        if files == self.in_use {
            return;
        }
        let path = self.dir.join("session.json");
        if files.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else if let Ok(json) = serde_json::to_vec_pretty(&files) {
            let _ = std::fs::create_dir_all(&self.dir);
            let _ = std::fs::write(&path, json);
        }
        self.in_use = files;
    }

    /// A clean exit: no plugins in use any more.
    pub fn clean_exit(&mut self) {
        self.set_in_use(Vec::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crash_in_plugin_code_quarantines_the_file() {
        let dir = std::env::temp_dir().join(format!("gt-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = PathBuf::from("/plugins/bad.clap");
        let (mut g, report) = Guard::open(&dir);
        assert!(report.is_empty());
        assert_eq!(g.run(&file, "Bad", "loading", || 7), 7);
        assert!(
            !dir.join("inflight.json").exists(),
            "marker removed after the call"
        );
        g.set_in_use(vec![file.clone()]);

        // Simulate dying inside a call: the marker stays behind.
        std::fs::write(
            dir.join("inflight.json"),
            serde_json::to_vec(&InFlight {
                path: file.clone(),
                plugin: "Bad".into(),
                action: "opening the editor of".into(),
            })
            .unwrap(),
        )
        .unwrap();
        drop(g);
        let (mut g, report) = Guard::open(&dir);
        assert_eq!(report.in_use, vec![file.clone()]);
        assert_eq!(report.quarantined.as_ref().unwrap().0, file);
        assert!(g
            .quarantined(&file)
            .unwrap()
            .contains("opening the editor of Bad"));
        // Quarantine survives restarts until released.
        let (g2, report2) = Guard::open(&dir);
        assert!(report2.is_empty());
        assert!(g2.quarantined(&file).is_some());
        g.release(&file);
        let (g3, _) = Guard::open(&dir);
        assert!(g3.quarantined(&file).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
