//! Sample library: loads samples on worker threads and caches them for the engine.
//!
//! Each request runs on its own short-lived thread (decoding and resampling can take a while
//! for long files), which also builds the waveform overview and the peak pyramid for the
//! playlist; results come back over a channel that the UI polls once per frame.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use gt_core::{Peaks, SampleData, SampleSource};
use gt_ui::views::{AudioLookup, BrowserEntry};

/// Waveform columns kept per sample for the sampler panel.
const OVERVIEW_COLUMNS: usize = 260;

/// A loaded sample and its drawing data.
pub struct Loaded {
    pub data: Arc<SampleData>,
    pub overview: Vec<(f32, f32)>,
    pub peaks: Peaks,
}

type LoadResult = (SampleSource, Result<Loaded, String>);

pub struct Library {
    cache: HashMap<SampleSource, Loaded>,
    pending: HashSet<SampleSource>,
    errors: HashMap<SampleSource, String>,
    tx: Sender<LoadResult>,
    rx: Receiver<LoadResult>,
}

impl Library {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            cache: HashMap::new(),
            pending: HashSet::new(),
            errors: HashMap::new(),
            tx,
            rx,
        }
    }

    pub fn get(&self, src: &SampleSource) -> Option<&Loaded> {
        self.cache.get(src)
    }

    /// Loading or error text for the sampler panel.
    pub fn status(&self, src: &SampleSource) -> Option<String> {
        if self.pending.contains(src) {
            Some("Loading…".to_owned())
        } else {
            self.errors.get(src).cloned()
        }
    }

    pub fn is_busy(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Starts loading `src` at `rate` Hz unless it is cached or already loading. A failed
    /// file is retried on the next request.
    pub fn request(&mut self, src: &SampleSource, rate: u32) {
        if self.cache.contains_key(src) || self.pending.contains(src) {
            return;
        }
        self.errors.remove(src);
        self.pending.insert(src.clone());
        let tx = self.tx.clone();
        let src = src.clone();
        let spawned = std::thread::Builder::new()
            .name("gt-sample-loader".into())
            .spawn({
                let src = src.clone();
                move || {
                    let result = load(&src, rate).map(|data| Loaded {
                        overview: data.overview(OVERVIEW_COLUMNS),
                        peaks: Peaks::build(&data),
                        data: Arc::new(data),
                    });
                    let _ = tx.send((src, result));
                }
            });
        if let Err(e) = spawned {
            self.pending.remove(&src);
            self.errors.insert(src, format!("cannot start loader: {e}"));
        }
    }

    /// Collects finished loads. Returns the sources that became available.
    pub fn poll(&mut self) -> Vec<SampleSource> {
        let mut ready = Vec::new();
        while let Ok((src, result)) = self.rx.try_recv() {
            self.pending.remove(&src);
            match result {
                Ok(loaded) => {
                    let data = &loaded.data;
                    log::info!(
                        "loaded {} ({:.2} s, {} ch, {} Hz)",
                        src.display_name(),
                        data.seconds(),
                        data.channels.len(),
                        data.sample_rate
                    );
                    self.cache.insert(src.clone(), loaded);
                    ready.push(src);
                }
                Err(e) => {
                    log::warn!("cannot load {}: {e}", src.display_name());
                    self.errors.insert(src, e);
                }
            }
        }
        ready
    }
}

impl AudioLookup for Library {
    fn peaks(&self, src: &SampleSource) -> Option<(&Peaks, u32)> {
        self.cache.get(src).map(|l| (&l.peaks, l.data.sample_rate))
    }
}

/// Loads a sound the same way the export does, so playback and export use identical data.
fn load(src: &SampleSource, rate: u32) -> Result<SampleData, String> {
    gt_export::load_source(src, rate)
}

/// Lists a folder for the browser: sub-folders, then audio files, each sorted by name.
/// Hidden entries (starting with '.') are skipped.
pub fn list_folder(dir: &Path) -> Result<Vec<BrowserEntry>, String> {
    let read = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            dirs.push(BrowserEntry::Dir(name, path));
        } else if gt_project::is_audio_file(&path) {
            files.push(BrowserEntry::File(name, path));
        }
    }
    let key = |e: &BrowserEntry| match e {
        BrowserEntry::Dir(n, _) | BrowserEntry::File(n, _) => n.to_lowercase(),
    };
    dirs.sort_by_key(key);
    files.sort_by_key(key);
    dirs.extend(files);
    Ok(dirs)
}

/// Starting folder for the browser: the user's Music folder if it exists, else home, else the
/// working directory.
pub fn default_folder() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from);
    if let Some(h) = home {
        let music = h.join("Music");
        if music.is_dir() {
            return music;
        }
        return h;
    }
    std::env::current_dir().unwrap_or_default()
}

/// Where the program keeps its own files (presets, autosave, extracted samples):
/// `%APPDATA%\GloomTunes Studio` on Windows, `~/Library/Application Support/GloomTunes Studio`
/// on macOS, `$XDG_DATA_HOME/gloomtunes-studio` (default `~/.local/share/gloomtunes-studio`)
/// elsewhere.
pub fn data_folder() -> std::path::PathBuf {
    use std::path::PathBuf;
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("GloomTunes Studio");
        }
    }
    if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("GloomTunes Studio");
        }
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".local").join("share"))
        })
        .unwrap_or_default();
    data.join("gloomtunes-studio")
}

/// Where the user's Gloom Synth presets are saved: `Presets/Gloom Synth` (Windows, macOS) or
/// `presets/gloom-synth` inside [`data_folder`].
pub fn synth_preset_folder() -> std::path::PathBuf {
    if (cfg!(windows) && std::env::var_os("APPDATA").is_some())
        || (cfg!(target_os = "macos") && std::env::var_os("HOME").is_some())
    {
        data_folder().join("Presets").join("Gloom Synth")
    } else {
        data_folder().join("presets").join("gloom-synth")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_ins_load_at_the_requested_rate() {
        let mut lib = Library::new();
        let src = SampleSource::BuiltIn(gt_core::BuiltInSample::Snare);
        lib.request(&src, 44_100);
        lib.request(&src, 44_100); // deduplicated
        let mut ready = Vec::new();
        for _ in 0..500 {
            ready.extend(lib.poll());
            if !lib.is_busy() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(ready, vec![src.clone()]);
        let l = lib.get(&src).unwrap();
        assert_eq!(l.data.sample_rate, 44_100);
        assert_eq!(l.overview.len(), OVERVIEW_COLUMNS);
        assert_eq!(
            l.peaks.frames(),
            l.data.frames(),
            "peaks built on the loader thread"
        );
        assert!(lib.peaks(&src).is_some());
    }

    #[test]
    fn listing_puts_folders_first_and_filters_files() {
        let dir = std::env::temp_dir().join(format!("gt-list-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Zeta")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        for f in ["b.wav", "A.FLAC", "readme.txt"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let names: Vec<_> = list_folder(&dir)
            .unwrap()
            .into_iter()
            .map(|e| match e {
                BrowserEntry::Dir(n, _) => format!("{n}/"),
                BrowserEntry::File(n, _) => n,
            })
            .collect();
        assert_eq!(names, vec!["Zeta/", "A.FLAC", "b.wav"]);
        assert!(list_folder(&dir.join("missing")).is_err());
    }
}
