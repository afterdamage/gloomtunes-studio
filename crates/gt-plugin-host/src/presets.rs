//! Plugin presets (Prompt 18, D114): the plugin's own factory presets (CLAP preset discovery,
//! VST3 program lists and `.vstpreset` files) and user presets saved as state files in the
//! data folder (`plugins/presets/<format>/<plugin id>/<name>.gtpreset`), for both formats.

use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};

use clack_extensions::preset_discovery::prelude::*;
use clack_host::prelude::{HostError, PluginEntry};

use crate::catalog::PluginInfo;

/// File extension of user presets.
pub const USER_EXT: &str = "gtpreset";
/// What a user preset file starts with (followed by the plugin's state).
const USER_MAGIC: &[u8; 4] = b"GTP1";
/// Files looked at per preset folder at most.
const MAX_FILES: usize = 4000;

/// One preset a plugin can load.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    /// Display name.
    pub name: String,
    /// Saved by the user (else the plugin's own).
    pub user: bool,
    pub(crate) source: Source,
}

/// Where a preset comes from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Source {
    /// A user preset file (the plugin's state).
    User(PathBuf),
    /// A CLAP preset: its file (none: inside the plugin) and load key.
    Clap {
        file: Option<CString>,
        key: Option<CString>,
    },
    /// A `.vstpreset` file.
    Vst3File(PathBuf),
    /// A VST3 program: the program-change parameter (index in the parameter list) and its
    /// normalized value.
    Vst3Program { index: usize, value: f32 },
}

/// Turns a name into something safe as a file or folder name.
pub(crate) fn file_name(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.()+&".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches(['.', ' ']).to_owned();
    if s.is_empty() {
        "Preset".to_owned()
    } else {
        s.chars().take(100).collect()
    }
}

/// The folder of `info`'s user presets under the plugin data folder `dir`.
pub(crate) fn user_dir(dir: &Path, info: &PluginInfo) -> PathBuf {
    dir.join("presets")
        .join(info.format.key())
        .join(file_name(&info.id))
}

/// The user presets in `folder`, by name.
pub(crate) fn list_user(folder: &Path) -> Vec<Preset> {
    let mut out: Vec<Preset> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == USER_EXT))
        .map(|p| Preset {
            name: p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            user: true,
            source: Source::User(p),
        })
        .collect();
    out.sort_by_key(|p| p.name.to_lowercase());
    out
}

/// Saves `state` as user preset `name` in `folder`; returns the file.
pub(crate) fn save_user(folder: &Path, name: &str, state: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(folder).map_err(|e| format!("cannot save the preset: {e}"))?;
    let path = folder.join(format!("{}.{USER_EXT}", file_name(name)));
    let mut bytes = USER_MAGIC.to_vec();
    bytes.extend_from_slice(state);
    std::fs::write(&path, bytes).map_err(|e| format!("cannot save the preset: {e}"))?;
    Ok(path)
}

/// Reads a user preset: the plugin state it holds.
pub(crate) fn read_user(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read the preset: {e}"))?;
    match bytes.strip_prefix(USER_MAGIC) {
        Some(state) => Ok(state.to_vec()),
        None => Err("not a GloomTunes preset".to_owned()),
    }
}

/// Files with extension `ext` (any if none) under `folder`, a few levels deep.
pub(crate) fn files_in(folder: &Path, ext: Option<&str>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(folder.to_owned(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if out.len() >= MAX_FILES {
                return out;
            }
            if p.is_dir() {
                if depth < 4 {
                    stack.push((p, depth + 1));
                }
            } else if ext.is_none_or(|x| {
                p.extension()
                    .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case(x))
            }) {
                out.push(p);
            }
        }
    }
    out
}

/// Collects what a CLAP preset provider declares.
#[derive(Default)]
struct ClapIndexer {
    /// Locations: None for presets inside the plugin, else a file or folder.
    locations: Vec<Option<CString>>,
    extensions: Vec<String>,
}

impl IndexerImpl for ClapIndexer {
    fn declare_filetype(&mut self, file_type: FileType) -> Result<(), HostError> {
        if let Some(ext) = file_type.file_extension {
            self.extensions.push(ext.to_string_lossy().into_owned());
        }
        Ok(())
    }

    fn declare_location(&mut self, location: LocationInfo) -> Result<(), HostError> {
        self.locations
            .push(location.location.file_path().map(CStr::to_owned));
        Ok(())
    }

    fn declare_soundpack(&mut self, _soundpack: Soundpack) -> Result<(), HostError> {
        Ok(())
    }
}

/// Collects the presets of one location that are for one plugin.
struct ClapReceiver<'a> {
    plugin_id: &'a str,
    file: Option<CString>,
    /// The preset being described: name, load key, plugin ids seen (and whether one matched).
    current: Option<(String, Option<CString>, bool, bool)>,
    out: Vec<Preset>,
}

impl ClapReceiver<'_> {
    fn finish(&mut self) {
        if let Some((name, key, any_id, ours)) = self.current.take() {
            if ours || !any_id {
                self.out.push(Preset {
                    name,
                    user: false,
                    source: Source::Clap {
                        file: self.file.clone(),
                        key,
                    },
                });
            }
        }
    }
}

impl MetadataReceiverImpl for ClapReceiver<'_> {
    fn on_error(&mut self, _error_code: i32, _error_message: Option<&CStr>) {}

    fn begin_preset(
        &mut self,
        name: Option<&CStr>,
        load_key: Option<&CStr>,
    ) -> Result<(), HostError> {
        self.finish();
        let name = name
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                self.file
                    .as_ref()
                    .and_then(|f| {
                        Path::new(&*f.to_string_lossy())
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                    })
                    .unwrap_or_else(|| "Preset".to_owned())
            });
        self.current = Some((name, load_key.map(CStr::to_owned), false, false));
        Ok(())
    }

    fn add_plugin_id(&mut self, plugin_id: UniversalPluginId) {
        if let Some(c) = self.current.as_mut() {
            c.2 = true;
            c.3 |= plugin_id.abi == c"clap" && plugin_id.id.to_bytes() == self.plugin_id.as_bytes();
        }
    }

    fn set_soundpack_id(&mut self, _soundpack_id: &CStr) {}
    fn set_flags(&mut self, _flags: Flags) {}
    fn add_creator(&mut self, _creator: &CStr) {}
    fn set_description(&mut self, _description: &CStr) {}
    fn set_timestamps(
        &mut self,
        _creation_time: Option<Timestamp>,
        _modification_time: Option<Timestamp>,
    ) {
    }
    fn add_feature(&mut self, _feature: &CStr) {}
    fn add_extra_info(&mut self, _key: &CStr, _value: &CStr) {}
}

/// The factory presets a CLAP plugin file offers for plugin `plugin_id`, through its preset
/// discovery providers.
pub(crate) fn clap_factory(entry: &PluginEntry, plugin_id: &str) -> Vec<Preset> {
    let Some(factory) = entry.get_factory::<PresetDiscoveryFactory>() else {
        return Vec::new();
    };
    let providers: Vec<CString> = factory
        .provider_descriptors()
        .filter_map(|d| d.id().map(CStr::to_owned))
        .collect();
    let host = crate::instance::host_info();
    let mut out = Vec::new();
    for id in providers {
        let Ok(mut provider) = Provider::instantiate(ClapIndexer::default(), entry, &id, &host)
        else {
            continue;
        };
        let locations = std::mem::take(&mut provider.indexer_mut().locations);
        let extensions = provider.indexer().extensions.clone();
        let mut rx = ClapReceiver {
            plugin_id,
            file: None,
            current: None,
            out: Vec::new(),
        };
        for location in locations {
            match location {
                None => {
                    rx.file = None;
                    provider.get_metadata(Location::Plugin, &mut rx);
                    rx.finish();
                }
                Some(path) => {
                    let root = PathBuf::from(&*path.to_string_lossy());
                    let files = if root.is_dir() {
                        if extensions.is_empty() {
                            files_in(&root, None)
                        } else {
                            extensions
                                .iter()
                                .flat_map(|e| files_in(&root, Some(e.trim_start_matches('.'))))
                                .collect()
                        }
                    } else {
                        vec![root]
                    };
                    for f in files {
                        let Ok(c) = CString::new(f.to_string_lossy().into_owned()) else {
                            continue;
                        };
                        rx.file = Some(c.clone());
                        provider.get_metadata(Location::File { path: &c }, &mut rx);
                        rx.finish();
                    }
                }
            }
        }
        out.append(&mut rx.out);
    }
    out
}

/// The standard folders for `.vstpreset` files of a plugin by `vendor` called `name`.
pub(crate) fn vst3_folders(vendor: &str, name: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    if cfg!(windows) {
        if let Some(h) = &home {
            roots.push(h.join("Documents").join("VST3 Presets"));
        }
        if let Some(p) = std::env::var_os("PROGRAMDATA") {
            roots.push(PathBuf::from(p).join("VST3 Presets"));
        }
    } else if cfg!(target_os = "macos") {
        if let Some(h) = &home {
            roots.push(h.join("Library/Audio/Presets"));
        }
        roots.push(PathBuf::from("/Library/Audio/Presets"));
    } else {
        if let Some(h) = &home {
            roots.push(h.join(".vst3/presets"));
        }
        roots.push(PathBuf::from("/usr/share/vst3/presets"));
        roots.push(PathBuf::from("/usr/local/share/vst3/presets"));
    }
    roots
        .into_iter()
        .map(|r| r.join(file_name(vendor)).join(file_name(name)))
        .collect()
}

/// The parts of a `.vstpreset` file: the class id it is for (32 hex digits) and the component
/// and controller states.
pub(crate) fn parse_vstpreset(bytes: &[u8]) -> Option<(String, Vec<u8>, Vec<u8>)> {
    let le_i32 = |at: usize| Some(i32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let le_i64 = |at: usize| Some(i64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?));
    if bytes.get(..4)? != b"VST3" {
        return None;
    }
    let class = String::from_utf8_lossy(bytes.get(8..40)?).into_owned();
    let list = usize::try_from(le_i64(40)?).ok()?;
    if bytes.get(list..list + 4)? != b"List" {
        return None;
    }
    let count = usize::try_from(le_i32(list + 4)?).ok()?.min(64);
    let (mut comp, mut cont) = (None, None);
    for i in 0..count {
        let at = list + 8 + i * 20;
        let id = bytes.get(at..at + 4)?;
        let offset = usize::try_from(le_i64(at + 4)?).ok()?;
        let size = usize::try_from(le_i64(at + 12)?).ok()?;
        let data = bytes.get(offset..offset.checked_add(size)?)?.to_vec();
        match id {
            b"Comp" => comp = Some(data),
            b"Cont" => cont = Some(data),
            _ => {}
        }
    }
    Some((class, comp?, cont.unwrap_or_default()))
}

/// Writes a `.vstpreset` file's bytes (for tests, and so user presets could be shared later).
#[cfg(test)]
pub(crate) fn write_vstpreset(class: &str, comp: &[u8], cont: &[u8]) -> Vec<u8> {
    let mut out = b"VST3".to_vec();
    out.extend_from_slice(&1i32.to_le_bytes());
    let mut id = class.as_bytes().to_vec();
    id.resize(32, b'0');
    out.extend_from_slice(&id);
    let list = 48 + comp.len() + cont.len();
    out.extend_from_slice(&(list as i64).to_le_bytes());
    out.extend_from_slice(comp);
    out.extend_from_slice(cont);
    out.extend_from_slice(b"List");
    out.extend_from_slice(&2i32.to_le_bytes());
    for (tag, off, len) in [
        (b"Comp", 48, comp.len()),
        (b"Cont", 48 + comp.len(), cont.len()),
    ] {
        out.extend_from_slice(tag);
        out.extend_from_slice(&(off as i64).to_le_bytes());
        out.extend_from_slice(&(len as i64).to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_become_safe_file_names() {
        assert_eq!(file_name("Bass / Sub: 1"), "Bass _ Sub_ 1");
        assert_eq!(file_name("  ..  "), "Preset");
        assert_eq!(file_name("../x"), "_x");
    }

    #[test]
    fn user_presets_are_saved_and_listed_by_name() {
        let dir = std::env::temp_dir().join(format!("gt-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        save_user(&dir, "b Lead", b"two").unwrap();
        let a = save_user(&dir, "A pad", b"one").unwrap();
        let list = list_user(&dir);
        let names: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["A pad", "b Lead"]);
        assert!(list.iter().all(|p| p.user));
        assert_eq!(read_user(&a).unwrap(), b"one");
        std::fs::write(dir.join("junk.gtpreset"), b"nope").unwrap();
        assert!(read_user(&dir.join("junk.gtpreset")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vstpreset_files_round_trip() {
        let bytes = write_vstpreset("0123456789ABCDEF0123456789ABCDEF", b"comp", b"ctrl");
        let (class, comp, cont) = parse_vstpreset(&bytes).unwrap();
        assert_eq!(class, "0123456789ABCDEF0123456789ABCDEF");
        assert_eq!(comp, b"comp");
        assert_eq!(cont, b"ctrl");
        assert!(parse_vstpreset(&bytes[..30]).is_none());
        assert!(parse_vstpreset(b"VST2").is_none());
    }
}
