//! Finding plugins: the CLAP and VST3 search folders, scanning a plugin file in a separate
//! process, and the scan cache.
//!
//! Loading a plugin file runs its code, and a broken plugin can crash the process that loads
//! it. So the plugin list is built by running this program again for each new or changed file
//! (`gloomtunes --scan-clap <file>`, see [`scan_child_main`]), which prints what it found as
//! JSON. A crash, hang or garbage output only costs that one file, which is listed with the
//! reason. Results are cached by path, size and modification time in `plugins/catalog.json`
//! in the data folder, so later starts only scan what changed.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use clack_host::prelude::PluginEntry;
use gt_core::{PluginFormat, PluginKind};
use serde::{Deserialize, Serialize};

/// The command-line switch that makes the program scan one file and exit.
pub const SCAN_SWITCH: &str = "--scan-clap";
/// A scan taking longer than this is treated as a hang.
const SCAN_TIMEOUT: Duration = Duration::from_secs(20);
/// Folder depth searched below each plugin folder.
const MAX_DEPTH: usize = 6;

/// One plugin in a plugin file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    /// The plugin's format (older catalogs only had CLAP).
    #[serde(default, with = "format_key")]
    pub format: PluginFormat,
    /// The plugin's id (a VST3 class id in hex).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Vendor.
    #[serde(default)]
    pub vendor: String,
    /// Version text.
    #[serde(default)]
    pub version: String,
    /// One-line description.
    #[serde(default)]
    pub description: String,
    /// CLAP feature tags ("instrument", "audio-effect", "reverb" ...); VST3 sub-categories
    /// are mapped onto them.
    #[serde(default)]
    pub features: Vec<String>,
    /// The file it is in.
    pub path: PathBuf,
}

impl PluginInfo {
    /// Instrument (plays notes) or effect (processes audio), from its feature tags.
    pub fn kind(&self) -> PluginKind {
        if self.features.iter().any(|f| f == "instrument") {
            PluginKind::Instrument
        } else {
            PluginKind::Effect
        }
    }
}

/// What scanning one file gave.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogFile {
    /// The file.
    pub path: PathBuf,
    /// Its size when scanned.
    pub size: u64,
    /// Its modification time when scanned, in seconds since 1970.
    pub modified: u64,
    /// The plugins in it.
    #[serde(default)]
    pub plugins: Vec<PluginInfo>,
    /// Why it could not be scanned, if it could not.
    #[serde(default)]
    pub error: Option<String>,
}

/// Every plugin file found, with its plugins.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    /// Scanned files, sorted by path.
    pub files: Vec<CatalogFile>,
}

impl Catalog {
    /// Reads the cache (empty if missing or unreadable).
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Writes the cache.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    /// Every plugin, sorted by name. A VST3 plugin that also ships as CLAP (same name and
    /// vendor) is left out: the CLAP one is preferred.
    pub fn plugins(&self) -> Vec<&PluginInfo> {
        let key = |p: &PluginInfo| (p.name.to_lowercase(), p.vendor.to_lowercase());
        // CLAP first, then VST3, then AU: a plugin shipped in several formats is offered once.
        let rank = |f: PluginFormat| match f {
            PluginFormat::Clap => 0,
            PluginFormat::Vst3 => 1,
            PluginFormat::Au => 2,
        };
        let mut best: std::collections::HashMap<(String, String), u8> =
            std::collections::HashMap::new();
        for p in self.files.iter().flat_map(|f| &f.plugins) {
            let r = best.entry(key(p)).or_insert(u8::MAX);
            *r = (*r).min(rank(p.format));
        }
        let mut all: Vec<&PluginInfo> = self
            .files
            .iter()
            .flat_map(|f| &f.plugins)
            .filter(|p| best.get(&key(p)) == Some(&rank(p.format)))
            .collect();
        all.sort_by_cached_key(|p| key(p));
        all
    }

    /// The plugin with `id`, preferring the file at `hint`.
    pub fn find(&self, id: &str, hint: &Path) -> Option<&PluginInfo> {
        let mut found = self
            .files
            .iter()
            .flat_map(|f| &f.plugins)
            .filter(|p| p.id == id);
        let first = found.next()?;
        if first.path == hint {
            return Some(first);
        }
        found.find(|p| p.path == hint).or(Some(first))
    }

    /// The cached result for `path`, if the file has not changed since.
    fn cached(&self, path: &Path, size: u64, modified: u64) -> Option<&CatalogFile> {
        self.files
            .iter()
            .find(|f| f.path == path && f.size == size && f.modified == modified)
    }
}

/// The folders searched for plugins: those in `CLAP_PATH`, then the standard CLAP ones
/// (`~/.clap`, `/usr/lib/clap` and `/usr/local/lib/clap` on Linux; `Common Files\CLAP` and
/// `%LOCALAPPDATA%\Programs\Common\CLAP` on Windows; `~/Library/Audio/Plug-Ins/CLAP` and
/// `/Library/Audio/Plug-Ins/CLAP` on macOS), then the standard VST3 ones (`~/.vst3`,
/// `/usr/lib/vst3` and `/usr/local/lib/vst3`; `Common Files\VST3`;
/// `~/Library/Audio/Plug-Ins/VST3` and `/Library/Audio/Plug-Ins/VST3`).
pub fn standard_folders() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("CLAP_PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if cfg!(windows) {
        if let Some(common) = std::env::var_os("COMMONPROGRAMFILES") {
            dirs.push(PathBuf::from(common).join("CLAP"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(
                PathBuf::from(local)
                    .join("Programs")
                    .join("Common")
                    .join("CLAP"),
            );
        }
        if let Some(common) = std::env::var_os("COMMONPROGRAMFILES") {
            dirs.push(PathBuf::from(common).join("VST3"));
        }
    } else if cfg!(target_os = "macos") {
        for format in ["CLAP", "VST3"] {
            if let Some(home) = std::env::var_os("HOME") {
                dirs.push(
                    PathBuf::from(home)
                        .join("Library/Audio/Plug-Ins")
                        .join(format),
                );
            }
            dirs.push(PathBuf::from("/Library/Audio/Plug-Ins").join(format));
        }
    } else {
        for format in ["clap", "vst3"] {
            if let Some(home) = std::env::var_os("HOME") {
                dirs.push(PathBuf::from(home).join(format!(".{format}")));
            }
            dirs.push(PathBuf::from("/usr/lib").join(format));
            dirs.push(PathBuf::from("/usr/local/lib").join(format));
        }
    }
    dirs.retain(|d| !d.as_os_str().is_empty());
    dirs.dedup();
    dirs
}

/// The format a plugin file is in, from its extension.
pub fn format_of(path: &Path) -> Option<PluginFormat> {
    let ext = path.extension()?;
    if ext.eq_ignore_ascii_case("clap") {
        Some(PluginFormat::Clap)
    } else if ext.eq_ignore_ascii_case("vst3") {
        Some(PluginFormat::Vst3)
    } else {
        None
    }
}

/// A plugin bundle: a folder holding `Contents`, loaded as one plugin file (`.vst3` bundles on
/// every system, `.clap` bundles on macOS).
fn is_bundle(path: &Path) -> bool {
    let ok = match format_of(path) {
        Some(PluginFormat::Vst3) => true,
        Some(PluginFormat::Clap) => cfg!(target_os = "macos"),
        Some(PluginFormat::Au) | None => false,
    };
    ok && path.join("Contents").is_dir()
}

/// Every `.clap` and `.vst3` file or bundle in `dirs` and their subfolders, sorted.
pub fn find_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let known = format_of(&path).is_some();
            // Follows links, so linked plugin files and folders count.
            let meta = std::fs::metadata(&path);
            if known && meta.as_ref().is_ok_and(|m| m.is_file() || is_bundle(&path)) {
                out.push(path);
            } else if depth < MAX_DEPTH && meta.is_ok_and(|m| m.is_dir()) {
                walk(&path, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    for d in dirs {
        walk(d, 0, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// Lists the plugins in a file by loading it into this process. Only for the scan process
/// ([`scan_child_main`]) and tests: a broken plugin can crash the caller.
pub fn scan_in_process(path: &Path) -> Result<Vec<PluginInfo>, String> {
    if format_of(path) == Some(PluginFormat::Vst3) {
        let module = crate::vst3::Module::load(path)?;
        return Ok(module.list(path));
    }
    // SAFETY: loading a plugin file runs its initialization code; that is the point of this
    // function, which runs in a throwaway process.
    #[allow(unsafe_code)]
    let entry = unsafe { PluginEntry::load(path) }.map_err(|e| format!("cannot load: {e}"))?;
    Ok(list_entry(&entry, path))
}

/// The plugins an entry offers.
pub(crate) fn list_entry(entry: &PluginEntry, path: &Path) -> Vec<PluginInfo> {
    let Some(factory) = entry.get_plugin_factory() else {
        return Vec::new();
    };
    let text = |s: Option<&std::ffi::CStr>| {
        s.map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    factory
        .plugin_descriptors()
        .filter_map(|d| {
            Some(PluginInfo {
                format: PluginFormat::Clap,
                id: d.id()?.to_str().ok()?.to_owned(),
                name: text(d.name()),
                vendor: text(d.vendor()),
                version: text(d.version()),
                description: text(d.description()),
                features: d
                    .features()
                    .map(|f| f.to_string_lossy().into_owned())
                    .collect(),
                path: path.to_owned(),
            })
        })
        .collect()
}

/// `PluginFormat` as its file key in the catalog.
mod format_key {
    use gt_core::PluginFormat;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(f: &PluginFormat, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(f.key())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<PluginFormat, D::Error> {
        let key = String::deserialize(d)?;
        PluginFormat::from_key(&key)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown plugin format {key}")))
    }
}

#[derive(Serialize, Deserialize)]
struct ScanOutput {
    #[serde(default)]
    plugins: Vec<PluginInfo>,
    #[serde(default)]
    error: Option<String>,
}

/// The scan process: lists the plugins in `file` as JSON on standard output. Returns the exit
/// code. Called by the app's `main` when started with [`SCAN_SWITCH`].
pub fn scan_child_main(file: &Path) -> i32 {
    let out = match scan_in_process(file) {
        Ok(plugins) => ScanOutput {
            plugins,
            error: None,
        },
        Err(e) => ScanOutput {
            plugins: Vec::new(),
            error: Some(e),
        },
    };
    match serde_json::to_string(&out) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(_) => 1,
    }
}

/// Scans `file` by running `exe --scan-clap file`, with a time limit.
pub fn scan_with_child(exe: &Path, file: &Path) -> Result<Vec<PluginInfo>, String> {
    let mut cmd = Command::new(exe);
    cmd.arg(SCAN_SWITCH)
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("RUST_LOG", "off");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start the scan: {e}"))?;
    // Read the output on a thread so a chatty plugin cannot fill the pipe and stall.
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(out) = stdout.as_mut() {
            let _ = out.read_to_string(&mut s);
        }
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() > SCAN_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the plugin did not finish loading (hung)".to_owned());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(e) => return Err(format!("scan failed: {e}")),
        }
    };
    let text = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("the plugin crashed while loading ({status})"));
    }
    // Plugins may print to standard output too: the result is the last JSON line.
    let out: ScanOutput = text
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str(l).ok())
        .ok_or_else(|| "the scan gave no result".to_owned())?;
    match out.error {
        Some(e) => Err(e),
        None => Ok(out.plugins),
    }
}

/// Size and modification time of a file. For a bundle, the total size and newest time of the
/// binaries in its binary folder (`Contents/MacOS`, `Contents/x86_64-linux` ...), since the
/// bundle folder itself rarely changes on an update.
fn stamp(path: &Path) -> Option<(u64, u64)> {
    let secs = |m: &std::fs::Metadata| {
        m.modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs())
    };
    let m = std::fs::metadata(path).ok()?;
    if !m.is_dir() {
        return Some((m.len(), secs(&m)));
    }
    let (mut size, mut modified) = (0, secs(&m));
    let bin = match format_of(path) {
        Some(PluginFormat::Vst3) => crate::vst3::binary_path(path)?.parent()?.to_owned(),
        _ => path.join("Contents").join("MacOS"),
    };
    for entry in std::fs::read_dir(bin).ok()?.flatten() {
        if let Ok(m) = entry.metadata() {
            size += m.len();
            modified = modified.max(secs(&m));
        }
    }
    Some((size, modified))
}

/// Builds a fresh catalog of `files`, reusing `old` results for unchanged files and scanning
/// the rest with `scan`. `skip` names files not to load (quarantined); they are listed with
/// that reason. `progress` gets (done, total, current file).
pub fn rescan(
    old: &Catalog,
    files: &[PathBuf],
    skip: &dyn Fn(&Path) -> Option<String>,
    scan: &dyn Fn(&Path) -> Result<Vec<PluginInfo>, String>,
    progress: &dyn Fn(usize, usize, &Path),
) -> Catalog {
    let mut out = Catalog::default();
    for (i, path) in files.iter().enumerate() {
        progress(i, files.len(), path);
        let Some((size, modified)) = stamp(path) else {
            continue;
        };
        let file = if let Some(reason) = skip(path) {
            CatalogFile {
                path: path.clone(),
                size,
                modified,
                plugins: Vec::new(),
                error: Some(reason),
            }
        } else if let Some(c) = old.cached(path, size, modified) {
            c.clone()
        } else {
            let (plugins, error) = match scan(path) {
                Ok(p) => (p, None),
                Err(e) => (Vec::new(), Some(e)),
            };
            CatalogFile {
                path: path.clone(),
                size,
                modified,
                plugins,
                error,
            }
        };
        out.files.push(file);
    }
    progress(files.len(), files.len(), Path::new(""));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, name: &str, path: &str, features: &[&str]) -> PluginInfo {
        PluginInfo {
            format: format_of(Path::new(path)).unwrap_or_default(),
            id: id.to_owned(),
            name: name.to_owned(),
            vendor: String::new(),
            version: String::new(),
            description: String::new(),
            features: features.iter().map(|s| (*s).to_owned()).collect(),
            path: PathBuf::from(path),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bundles_count_as_plugin_files_and_are_stamped_by_their_binary() {
        let dir = std::env::temp_dir().join(format!("gt-bundles-{}", std::process::id()));
        let bin = dir.join("Synth.clap").join("Contents").join("MacOS");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("Synth"), b"12345").unwrap();
        // A plain folder with the extension but no bundle layout is walked into, not listed.
        std::fs::create_dir_all(dir.join("Odd.clap")).unwrap();
        let files = find_files(std::slice::from_ref(&dir));
        assert_eq!(files, vec![dir.join("Synth.clap")]);
        assert_eq!(stamp(&files[0]).map(|(size, _)| size), Some(5));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cache_skips_unchanged_files_and_lists_failures() {
        let dir = std::env::temp_dir().join(format!("gt-catalog-{}", std::process::id()));
        let sub = dir.join("vendor");
        std::fs::create_dir_all(&sub).unwrap();
        let a = dir.join("a.clap");
        let b = sub.join("b.CLAP");
        let c = dir.join("c.clap");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"bb").unwrap();
        std::fs::write(&c, b"ccc").unwrap();
        std::fs::write(dir.join("readme.txt"), b"").unwrap();
        let files = find_files(std::slice::from_ref(&dir));
        assert_eq!(files, vec![a.clone(), c.clone(), b.clone()]);

        let scans = std::cell::Cell::new(0);
        let scan = |p: &Path| {
            scans.set(scans.get() + 1);
            if p == c {
                Err("crashed".to_owned())
            } else {
                Ok(vec![info(
                    "x.y",
                    "Y",
                    &p.to_string_lossy(),
                    &["instrument"],
                )])
            }
        };
        let skip = |p: &Path| (p == b).then(|| "quarantined".to_owned());
        let first = rescan(&Catalog::default(), &files, &skip, &scan, &|_, _, _| {});
        assert_eq!(scans.get(), 2, "b is skipped");
        assert_eq!(first.files[1].error.as_deref(), Some("crashed"));
        assert_eq!(first.files[2].error.as_deref(), Some("quarantined"));
        assert_eq!(first.plugins().len(), 1);
        assert_eq!(first.plugins()[0].kind(), PluginKind::Instrument);

        // Round trip through the cache file, then nothing changed: nothing is scanned.
        let cache = dir.join("catalog.json");
        first.save(&cache).unwrap();
        let loaded = Catalog::load(&cache);
        assert_eq!(loaded, first);
        let second = rescan(&loaded, &files, &|_| None, &scan, &|_, _, _| {});
        assert_eq!(scans.get(), 2, "unchanged files come from the cache");
        assert_eq!(second.files[..2], first.files[..2]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugins_are_found_by_id_preferring_the_saved_file() {
        let cat = Catalog {
            files: vec![
                CatalogFile {
                    path: PathBuf::from("/a.clap"),
                    size: 1,
                    modified: 1,
                    plugins: vec![info("p", "P", "/a.clap", &["audio-effect"])],
                    error: None,
                },
                CatalogFile {
                    path: PathBuf::from("/b.clap"),
                    size: 1,
                    modified: 1,
                    plugins: vec![info("p", "P", "/b.clap", &[])],
                    error: None,
                },
            ],
        };
        assert_eq!(
            cat.find("p", Path::new("/b.clap")).unwrap().path,
            Path::new("/b.clap")
        );
        assert_eq!(
            cat.find("p", Path::new("/c.clap")).unwrap().path,
            Path::new("/a.clap")
        );
        assert!(cat.find("q", Path::new("/a.clap")).is_none());
        assert_eq!(cat.plugins()[0].kind(), PluginKind::Effect);
    }

    #[test]
    fn a_plugin_shipped_as_clap_and_vst3_is_offered_once_as_clap() {
        let file = |path: &str, plugins| CatalogFile {
            path: PathBuf::from(path),
            size: 1,
            modified: 1,
            plugins,
            error: None,
        };
        let cat = Catalog {
            files: vec![
                file(
                    "/s.clap",
                    vec![info("org.s", "Synth", "/s.clap", &["instrument"])],
                ),
                file(
                    "/S.vst3",
                    vec![
                        info("ABCD", "SYNTH", "/S.vst3", &["instrument"]),
                        info("EF01", "Delay", "/S.vst3", &["audio-effect"]),
                    ],
                ),
            ],
        };
        let names: Vec<(&str, PluginFormat)> = cat
            .plugins()
            .iter()
            .map(|p| (p.name.as_str(), p.format))
            .collect();
        assert_eq!(
            names,
            vec![("Delay", PluginFormat::Vst3), ("Synth", PluginFormat::Clap)]
        );
        // The format survives the cache file; older caches without it read as CLAP.
        let json = serde_json::to_string(&cat).unwrap();
        assert!(json.contains("\"format\":\"vst3\""));
        assert_eq!(serde_json::from_str::<Catalog>(&json).unwrap(), cat);
        let old: PluginInfo =
            serde_json::from_str(r#"{"id":"a","name":"A","path":"/a.clap"}"#).unwrap();
        assert_eq!(old.format, PluginFormat::Clap);
    }
}
