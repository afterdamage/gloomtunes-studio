//! The browser's search index (Prompt 25, ARCHITECTURE.md D132).
//!
//! [`Index::build`] walks the user's root folders once, on a worker thread, and keeps one
//! [`Item`] per audio file, project and preset it finds: its name, the folder it is in, the
//! tempo and key its name gives ("Pad_Am_120bpm.wav") and the user's tags. Each item keeps a
//! lowercase search text, so [`Index::search`] is a plain scan: every word of the query must
//! appear in it. 50,000 files search in a few milliseconds.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What an indexed file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    /// An audio file.
    Sample,
    /// A GloomTunes project.
    Project,
    /// A Gloom Synth preset file.
    SynthPreset,
}

impl ItemKind {
    /// The kind of `path` by its extension, if the index keeps such files.
    pub fn of(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        if crate::AUDIO_EXTENSIONS.contains(&ext.as_str()) {
            Some(Self::Sample)
        } else if ext == crate::file::EXTENSION {
            Some(Self::Project)
        } else if ext == crate::presets::PRESET_EXTENSION {
            Some(Self::SynthPreset)
        } else {
            None
        }
    }
}

/// One indexed file.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Where it is.
    pub path: PathBuf,
    /// File name without extension.
    pub name: String,
    /// The folder it is in, from its root's name down ("Drums/Kicks").
    pub folder: String,
    /// Which root it was found under.
    pub root: usize,
    /// What it is.
    pub kind: ItemKind,
    /// Tempo from the name.
    pub bpm: Option<f32>,
    /// Key from the name ("Am", "F#").
    pub key: Option<String>,
    /// The user's tags.
    pub tags: Vec<String>,
    /// Lowercase name, folder, tempo, key and tags.
    text: String,
    /// Lowercase name, for ranking.
    lower_name: String,
}

impl Item {
    /// Makes an item for `path` (relative folder `folder`), reading tempo and key from its name.
    pub fn new(path: PathBuf, folder: String, root: usize, kind: ItemKind) -> Self {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bpm = bpm_from_name(&name);
        let key = key_from_name(&name);
        let mut item = Self {
            lower_name: name.to_lowercase(),
            path,
            name,
            folder,
            root,
            kind,
            bpm,
            key,
            tags: Vec::new(),
            text: String::new(),
        };
        item.refresh_text();
        item
    }

    fn refresh_text(&mut self) {
        let mut t = format!("{} {}", self.lower_name, self.folder.to_lowercase());
        if let Some(b) = self.bpm {
            t.push_str(&format!(" {}bpm", b.round()));
        }
        if let Some(k) = &self.key {
            t.push_str(" key:");
            t.push_str(&k.to_lowercase());
        }
        for tag in &self.tags {
            t.push_str(" #");
            t.push_str(&tag.to_lowercase());
        }
        self.text = t;
    }
}

/// A search.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    /// Words that must all appear (name, folder, "120bpm", "key:am", "#tag").
    pub text: String,
    /// Only this kind.
    pub kind: Option<ItemKind>,
}

/// Files found under the root folders.
#[derive(Debug, Clone, Default)]
pub struct Index {
    /// The folders walked.
    pub roots: Vec<PathBuf>,
    /// Every file kept, sorted by folder then name.
    pub items: Vec<Item>,
    /// How long the walk took.
    pub elapsed: Duration,
    /// True when the walk stopped at [`MAX_ITEMS`].
    pub truncated: bool,
}

/// Most files indexed.
pub const MAX_ITEMS: usize = 500_000;
/// Deepest folder level walked.
const MAX_DEPTH: usize = 24;

impl Index {
    /// Walks `roots` (hidden files and folders skipped).
    pub fn build(roots: &[PathBuf]) -> Self {
        let start = Instant::now();
        let mut items = Vec::new();
        let mut truncated = false;
        for (r, root) in roots.iter().enumerate() {
            let base = root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.display().to_string());
            truncated |= walk(root, &base, r, 0, &mut items);
        }
        let mut index = Self::from_items(roots.to_vec(), items);
        index.truncated = truncated;
        index.elapsed = start.elapsed();
        index
    }

    /// An index of `items` (sorted here).
    pub fn from_items(roots: Vec<PathBuf>, mut items: Vec<Item>) -> Self {
        items.sort_by(|a, b| {
            (a.root, &a.folder, &a.lower_name).cmp(&(b.root, &b.folder, &b.lower_name))
        });
        Self {
            roots,
            items,
            elapsed: Duration::ZERO,
            truncated: false,
        }
    }

    /// Sets the user's tags of the file at `path`.
    pub fn set_tags(&mut self, path: &Path, tags: &[String]) {
        for item in self.items.iter_mut().filter(|i| i.path == path) {
            item.tags = tags.to_vec();
            item.refresh_text();
        }
    }

    /// Indices of the items matching `q`, at most `limit`: names starting with the first word
    /// first, then names containing it, then the rest (folder or tag matches), each in index
    /// order.
    pub fn search(&self, q: &Query, limit: usize) -> Vec<usize> {
        let lower = q.text.to_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();
        let mut tiers: [Vec<usize>; 3] = Default::default();
        for (i, item) in self.items.iter().enumerate() {
            if q.kind.is_some_and(|k| k != item.kind) {
                continue;
            }
            if !words.iter().all(|w| item.text.contains(w)) {
                continue;
            }
            let tier = match words.first() {
                Some(w) if item.lower_name.starts_with(w) => 0,
                Some(w) if item.lower_name.contains(w) => 1,
                Some(_) => 2,
                None => 0,
            };
            tiers[tier].push(i);
            if tiers[0].len() >= limit {
                break;
            }
        }
        tiers.into_iter().flatten().take(limit).collect()
    }

    /// Number of items of `kind`.
    pub fn count(&self, kind: ItemKind) -> usize {
        self.items.iter().filter(|i| i.kind == kind).count()
    }
}

/// Adds the kept files under `dir`. Returns true when [`MAX_ITEMS`] was reached.
fn walk(dir: &Path, folder: &str, root: usize, depth: usize, items: &mut Vec<Item>) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut subdirs = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if ft.is_dir() {
            subdirs.push((path, format!("{folder}/{name}")));
        } else if let Some(kind) = ItemKind::of(&path) {
            if items.len() >= MAX_ITEMS {
                return true;
            }
            items.push(Item::new(path, folder.to_owned(), root, kind));
        }
    }
    if depth < MAX_DEPTH {
        for (path, sub) in subdirs {
            if walk(&path, &sub, root, depth + 1, items) {
                return true;
            }
        }
    }
    false
}

/// The tempo a file name gives: a number next to "bpm" ("120bpm", "bpm_95", "90 BPM").
pub fn bpm_from_name(name: &str) -> Option<f32> {
    let lower = name.to_ascii_lowercase();
    let b = lower.as_bytes();
    let mut from = 0;
    while let Some(i) = lower[from..].find("bpm").map(|i| i + from) {
        from = i + 3;
        // A number just before (skipping one separator) or just after.
        let before = {
            let mut e = i;
            if e > 0 && matches!(b[e - 1], b' ' | b'_' | b'-' | b'.') {
                e -= 1;
            }
            let s = (0..e).rev().take_while(|&k| b[k].is_ascii_digit()).last();
            s.map(|s| &lower[s..e])
        };
        let after = {
            let mut s = i + 3;
            if s < b.len() && matches!(b[s], b' ' | b'_' | b'-' | b'.') {
                s += 1;
            }
            let n = b[s.min(b.len())..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
            (n > 0).then(|| &lower[s..s + n])
        };
        for digits in [before, after].into_iter().flatten() {
            if let Ok(v) = digits.parse::<f32>() {
                if (40.0..=300.0).contains(&v) {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// The key a file name gives, as "A", "F#m" or "Bb": a word like "Am", "F#", "Cmin",
/// "Ebmajor" or "D minor". A lone capital letter only counts after the word "key".
pub fn key_from_name(name: &str) -> Option<String> {
    let words: Vec<&str> = name
        .split(|c: char| c.is_whitespace() || "_-.()[],".contains(c))
        .filter(|w| !w.is_empty())
        .collect();
    for (i, w) in words.iter().enumerate() {
        let next = words.get(i + 1).copied().unwrap_or("");
        let after_key = i > 0 && words[i - 1].eq_ignore_ascii_case("key");
        if let Some(k) = parse_key(w, next, after_key) {
            return Some(k);
        }
    }
    None
}

fn parse_key(word: &str, next: &str, after_key: bool) -> Option<String> {
    let mut chars = word.chars();
    let letter = chars.next()?;
    if !('A'..='G').contains(&letter) {
        return None;
    }
    let rest: String = chars.collect();
    let (accidental, mode) = match rest.chars().next() {
        Some('#') | Some('♯') => ("#", &rest[rest.chars().next()?.len_utf8()..]),
        Some('b') | Some('♭') if !rest.to_ascii_lowercase().starts_with("bpm") => {
            ("b", &rest[rest.chars().next()?.len_utf8()..])
        }
        _ => ("", rest.as_str()),
    };
    let mode = mode.to_ascii_lowercase();
    let minor = match mode.as_str() {
        "m" | "min" | "minor" => true,
        "maj" | "major" => false,
        "" => match next.to_ascii_lowercase().as_str() {
            "minor" | "min" => true,
            "major" | "maj" => false,
            _ if accidental.is_empty() && !after_key => return None,
            _ => false,
        },
        _ => return None,
    };
    Some(format!(
        "{letter}{accidental}{}",
        if minor { "m" } else { "" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_give_tempo_and_key() {
        let cases: [(&str, Option<f32>, Option<&str>); 12] = [
            ("Pad_Am_120bpm", Some(120.0), Some("Am")),
            ("drum loop 90 BPM", Some(90.0), None),
            ("bpm_174_Reese_F#m", Some(174.0), Some("F#m")),
            ("Bass Cmin 128BPM", Some(128.0), Some("Cm")),
            ("Keys Eb major", None, Some("Eb")),
            ("Lead-D-minor", None, Some("Dm")),
            ("Kick A", None, None),
            ("Vocal key A 95bpm", Some(95.0), Some("A")),
            ("Snare 909", None, None),
            ("Hat bpm", None, None),
            ("Loop 1000bpm", None, None),
            ("Bb_Stab", None, Some("Bb")),
        ];
        for (name, bpm, key) in cases {
            assert_eq!(bpm_from_name(name), bpm, "{name}");
            assert_eq!(key_from_name(name).as_deref(), key, "{name}");
        }
    }

    fn item(path: &str, folder: &str) -> Item {
        Item::new(
            PathBuf::from(path),
            folder.to_owned(),
            0,
            ItemKind::of(Path::new(path)).unwrap(),
        )
    }

    #[test]
    fn search_matches_every_word_and_ranks_names_first() {
        let mut index = Index::from_items(
            vec![PathBuf::from("/s")],
            vec![
                item("/s/Drums/Kick Deep.wav", "s/Drums"),
                item("/s/Drums/Deep Kick.wav", "s/Drums"),
                item("/s/Kicks/Thump.wav", "s/Kicks"),
                item("/s/Loops/Pad_Am_120bpm.wav", "s/Loops"),
                item("/s/Songs/Night.gloom", "s/Songs"),
                item("/s/Presets/Kick Bass.gloomsynth", "s/Presets"),
            ],
        );
        let names = |ix: &Index, v: Vec<usize>| -> Vec<String> {
            v.into_iter().map(|i| ix.items[i].name.clone()).collect()
        };
        let q = |text: &str, kind| Query {
            text: text.to_owned(),
            kind,
        };
        assert_eq!(
            names(
                &index,
                index.search(&q("kick", Some(ItemKind::Sample)), 100)
            ),
            ["Kick Deep", "Deep Kick", "Thump"]
        );
        assert_eq!(
            names(&index, index.search(&q("KICK deep", None), 100)),
            ["Kick Deep", "Deep Kick"],
            "every word, any case; the first word ranks"
        );
        assert_eq!(
            names(&index, index.search(&q("120bpm key:am", None), 100)),
            ["Pad_Am_120bpm"]
        );
        assert_eq!(
            names(&index, index.search(&q("", Some(ItemKind::Project)), 100)),
            ["Night"]
        );
        assert_eq!(index.search(&q("", None), 2).len(), 2);
        // Tags are searchable as soon as they are set.
        index.set_tags(Path::new("/s/Kicks/Thump.wav"), &["Punchy".to_owned()]);
        assert_eq!(
            names(&index, index.search(&q("#punchy", None), 100)),
            ["Thump"]
        );
        assert_eq!(
            index.items[index.search(&q("thump", None), 1)[0]].tags,
            ["Punchy"]
        );
    }

    #[test]
    fn search_is_fast_on_fifty_thousand_files() {
        let words = [
            "kick", "snare", "hat", "pad", "bass", "lead", "vox", "fx", "loop", "perc",
        ];
        let mut items = Vec::with_capacity(50_000);
        for i in 0..50_000usize {
            let w = words[i % words.len()];
            let folder = format!("Library/Pack {}/{}", i / 500, words[(i / 7) % words.len()]);
            let path = format!("/lib/{folder}/{w} {i} {}bpm.wav", 80 + i % 100);
            items.push(item(&path, &folder));
        }
        // What a plain scan of the names and folders finds.
        let plain: Vec<String> = items
            .iter()
            .map(|i| format!("{} {}", i.name, i.folder).to_lowercase())
            .collect();
        let index = Index::from_items(vec![PathBuf::from("/lib")], items);
        let t = Instant::now();
        let mut found = 0;
        let mut expected = 0;
        let queries = ["kick", "pad 120bpm", "pack 42 snare", "zzz", ""];
        for text in queries {
            found += index
                .search(
                    &Query {
                        text: text.to_owned(),
                        kind: None,
                    },
                    usize::MAX,
                )
                .len();
        }
        let per_search = t.elapsed() / queries.len() as u32;
        for text in queries {
            expected += plain
                .iter()
                .filter(|p| text.split_whitespace().all(|w| p.contains(w)))
                .count();
        }
        assert_eq!(found, expected);
        assert!(expected > 50_000);
        // Generous for unoptimised test builds on slow CI machines; release builds take a few
        // milliseconds.
        assert!(per_search < Duration::from_millis(400), "{per_search:?}");
    }

    #[test]
    fn walking_folders_finds_kept_files_and_skips_hidden_ones() {
        let dir = std::env::temp_dir().join(format!("gt-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["Drums/Kicks", ".cache", "Songs"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        for f in [
            "Drums/Kicks/Kick 1.wav",
            "Drums/Kicks/readme.txt",
            "Drums/Snare.FLAC",
            ".cache/hidden.wav",
            "Songs/Track.gloom",
        ] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let index = Index::build(std::slice::from_ref(&dir));
        let base = dir.file_name().unwrap().to_string_lossy().into_owned();
        let got: Vec<(String, String, ItemKind)> = index
            .items
            .iter()
            .map(|i| (i.folder.clone(), i.name.clone(), i.kind))
            .collect();
        assert_eq!(
            got,
            vec![
                (format!("{base}/Drums"), "Snare".into(), ItemKind::Sample),
                (
                    format!("{base}/Drums/Kicks"),
                    "Kick 1".into(),
                    ItemKind::Sample
                ),
                (format!("{base}/Songs"), "Track".into(), ItemKind::Project),
            ]
        );
        assert_eq!(index.count(ItemKind::Sample), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
