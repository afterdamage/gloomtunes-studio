//! The project file: a `.gloom` zip container holding `project.json` and, optionally, the
//! audio files the project uses.
//!
//! ```text
//! project.json          { "format": "gloomtunes-project", "schema_version": 1, "project": {...} }
//! samples/<hash>-<name> embedded copies of the audio files, byte for byte (optional)
//! ```
//!
//! Parameters, effects, LFO shapes and automation targets are stored by their stable text keys,
//! never by enum position, so internal tables can be reordered without breaking files. Reading
//! is tolerant: unknown fields are ignored, missing ones take their defaults, and the result is
//! passed through [`Project::sanitize`]. Files from older schema versions are upgraded by
//! [`crate::migrate`] before they are read.
//!
//! Audio files are referenced by a path relative to the project file (so a project folder can be
//! moved or copied to another machine), with the absolute path kept as a fallback. When loading,
//! a sample is looked for at the relative path, then the absolute path, then in the embedded
//! copies; anything still missing is listed in [`Loaded::missing`] for the relink dialog.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use gt_core::effects::EffectKind;
use gt_core::modulation::SYNC_RATES;
use gt_core::synth::{ModDest, ModSlot, ModSource, SynthParam, SynthPatch, MOD_SLOTS};
use gt_core::{
    Adsr, AutoPoint, Automation, BuiltInSample, Channel, ChannelId, Clip, ClipId, ClipKind, Curve,
    EffectSlot, Instrument, LfoRate, LfoShape, LoopMode, Marker, ModSourceKind, Modulator,
    ModulatorId, Note, ParamId, Pattern, PatternId, Project, SampleSource, SamplerSettings,
    SigChange, TempoMap, TempoPoint, Tick, TimeSig, TimeSigMap, Track, TrackId, FX_SLOTS, SENDS,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::migrate;

/// File extension of projects.
pub const EXTENSION: &str = "gloom";
/// The `format` field of `project.json`.
pub const FORMAT: &str = "gloomtunes-project";
const PROJECT_ENTRY: &str = "project.json";
const SAMPLES_DIR: &str = "samples/";

/// Why a project could not be saved or opened.
#[derive(Debug)]
pub enum FileError {
    /// File system error.
    Io(std::io::Error),
    /// Not a zip container, or a damaged one.
    Container(String),
    /// Not a GloomTunes project, or unreadable JSON.
    Format(String),
    /// Written by a newer version of the program.
    TooNew {
        /// The file's schema version.
        found: u32,
        /// The newest version this build reads.
        supported: u32,
    },
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Container(e) => write!(f, "not a readable .gloom file: {e}"),
            Self::Format(e) => write!(f, "not a GloomTunes project: {e}"),
            Self::TooNew { found, supported } => write!(
                f,
                "made by a newer GloomTunes Studio (file version {found}, this build reads up \
                 to {supported}); update the program to open it"
            ),
        }
    }
}

impl std::error::Error for FileError {}

impl From<std::io::Error> for FileError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<zip::result::ZipError> for FileError {
    fn from(e: zip::result::ZipError) -> Self {
        match e {
            zip::result::ZipError::Io(e) => Self::Io(e),
            e => Self::Container(e.to_string()),
        }
    }
}

/// How to save.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SaveOptions {
    /// Copy every audio file the project uses into the container, so it opens anywhere.
    pub embed_samples: bool,
}

/// What a save did besides writing the project.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SaveReport {
    /// Audio files embedded.
    pub embedded: usize,
    /// Audio files that could not be read for embedding (still referenced by path).
    pub unreadable: Vec<PathBuf>,
}

/// A project read from a file.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// The project, sanitized; audio paths point at files that exist where possible.
    pub project: Project,
    /// Audio files that were found nowhere (the project still references them).
    pub missing: Vec<PathBuf>,
    /// Audio files restored from the container's embedded copies.
    pub extracted: usize,
    /// Things that were dropped or repaired, for the log.
    pub warnings: Vec<String>,
    /// The schema version the file was written with.
    pub schema_version: u32,
}

/// Writes `project` to `path` (replacing it only once the new file is complete).
pub fn save(project: &Project, path: &Path, opts: SaveOptions) -> Result<SaveReport, FileError> {
    let dir = absolute(path.parent().unwrap_or(Path::new(".")));
    let mut report = SaveReport::default();
    let mut embedded: HashMap<PathBuf, (String, Vec<u8>)> = HashMap::new();
    if opts.embed_samples {
        for file in sample_files(project) {
            match std::fs::read(&file) {
                Ok(bytes) => {
                    let name = file
                        .file_name()
                        .map_or_else(|| "sample".to_owned(), |n| n.to_string_lossy().into_owned());
                    let entry = format!(
                        "{SAMPLES_DIR}{:016x}-{}",
                        fnv1a(&bytes),
                        sanitize_name(&name)
                    );
                    embedded.insert(file, (entry, bytes));
                }
                Err(_) => report.unreadable.push(file),
            }
        }
    }
    let names: HashMap<PathBuf, String> = embedded
        .iter()
        .map(|(k, (entry, _))| (k.clone(), entry.clone()))
        .collect();
    let json = serde_json::to_vec_pretty(&to_value(project, Some(&dir), &names))
        .map_err(|e| FileError::Format(e.to_string()))?;

    let tmp = path.with_extension(format!("{EXTENSION}.tmp"));
    {
        let file = std::fs::File::create(&tmp)?;
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
        let deflate = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file(PROJECT_ENTRY, deflate)?;
        zip.write_all(&json)?;
        // Audio files are mostly compressed already; store them as they are.
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);
        let mut entries: Vec<&(String, Vec<u8>)> = embedded.values().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|a, b| a.0 == b.0);
        for (entry, bytes) in entries {
            zip.start_file(entry.as_str(), stored)?;
            zip.write_all(bytes)?;
            report.embedded += 1;
        }
        zip.finish()?.flush()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(report)
}

/// Reads a project. Embedded audio that is needed (not found on disk) is written to
/// `extract_dir` (created if needed) and the project points there.
pub fn load(path: &Path, extract_dir: &Path) -> Result<Loaded, FileError> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))?;
    let mut json = Vec::new();
    zip.by_name(PROJECT_ENTRY)
        .map_err(|_| FileError::Format(format!("no {PROJECT_ENTRY} inside")))?
        .read_to_end(&mut json)?;
    let value: Value =
        serde_json::from_slice(&json).map_err(|e| FileError::Format(e.to_string()))?;
    let dir = absolute(path.parent().unwrap_or(Path::new(".")));
    let mut loaded = from_value(value, Some(&dir))?;

    // Look for each audio file: relative path, absolute path, embedded copy.
    let mut resolved: HashMap<PathBuf, PathBuf> = HashMap::new();
    let refs = std::mem::take(&mut loaded.refs);
    for (key, r) in &refs {
        let found = r
            .relative
            .iter()
            .chain(&r.absolute)
            .find(|p| p.is_file())
            .cloned()
            .or_else(|| {
                let entry = r.embedded.as_ref()?;
                let out = extract_dir.join(entry.trim_start_matches(SAMPLES_DIR));
                if !out.is_file() {
                    let mut src = zip.by_name(entry).ok()?;
                    std::fs::create_dir_all(extract_dir).ok()?;
                    let tmp = out.with_extension("part");
                    let mut f = std::fs::File::create(&tmp).ok()?;
                    std::io::copy(&mut src, &mut f).ok()?;
                    drop(f);
                    std::fs::rename(&tmp, &out).ok()?;
                }
                loaded.extracted += 1;
                Some(out)
            });
        match found {
            Some(p) => {
                resolved.insert(key.clone(), p);
            }
            None => loaded.missing.push(key.clone()),
        }
    }
    for (from, to) in &resolved {
        if from != to {
            relink(&mut loaded.project, from, to);
        }
    }
    loaded.missing.sort();
    Ok(Loaded {
        project: loaded.project,
        missing: loaded.missing,
        extracted: loaded.extracted,
        warnings: loaded.warnings,
        schema_version: loaded.schema_version,
    })
}

/// Every audio file the project references (channels and audio clips), without duplicates.
pub fn sample_files(project: &Project) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = project
        .channels
        .iter()
        .filter_map(|c| c.sample())
        .chain(project.playlist.clips.iter().filter_map(|c| match &c.kind {
            ClipKind::Audio { source, .. } => Some(source),
            _ => None,
        }))
        .filter_map(|s| match s {
            SampleSource::File(p) => Some(p.clone()),
            SampleSource::BuiltIn(_) => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Points every use of the audio file `from` (channels and audio clips) at `to`. Returns how
/// many references changed.
pub fn relink(project: &mut Project, from: &Path, to: &Path) -> usize {
    let mut n = 0;
    let mut fix = |s: &mut SampleSource| {
        if matches!(s, SampleSource::File(p) if p == from) {
            *s = SampleSource::File(to.to_path_buf());
            n += 1;
        }
    };
    for c in &mut project.channels {
        if let Some(Some(s)) = c.sampler_mut().map(|s| s.sample.as_mut()) {
            fix(s);
        }
    }
    for c in &mut project.playlist.clips {
        if let ClipKind::Audio { source, .. } = &mut c.kind {
            fix(source);
        }
    }
    n
}

/// Searches `dir` and its subfolders (at most `max_depth` levels down) for files with the
/// names of `missing`. Returns the first match for each name found.
pub fn find_by_name(
    dir: &Path,
    missing: &[PathBuf],
    max_depth: usize,
) -> HashMap<PathBuf, PathBuf> {
    let wanted: HashMap<std::ffi::OsString, Vec<&PathBuf>> =
        missing.iter().fold(HashMap::new(), |mut m, p| {
            if let Some(n) = p.file_name() {
                m.entry(n.to_ascii_lowercase()).or_default().push(p);
            }
            m
        });
    let mut found = HashMap::new();
    let mut stack = vec![(dir.to_path_buf(), 0)];
    let mut visited = 0;
    while let Some((d, depth)) = stack.pop() {
        visited += 1;
        if visited > 20_000 || found.len() == missing.len() {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() {
                if depth < max_depth {
                    stack.push((path, depth + 1));
                }
            } else if let Some(olds) = wanted.get(&e.file_name().to_ascii_lowercase()) {
                for old in olds {
                    found.entry((*old).clone()).or_insert_with(|| path.clone());
                }
            }
        }
    }
    found
}

// ---------------------------------------------------------------------------------------------
// Document <-> JSON

#[derive(Serialize, Deserialize)]
struct FileDto {
    format: String,
    schema_version: u32,
    #[serde(default)]
    app_version: String,
    project: ProjectDto,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct ProjectDto {
    next_id: u32,
    current_pattern: u32,
    swing: f32,
    tempo: Vec<TempoDto>,
    signatures: Vec<SigDto>,
    channels: Vec<ChannelDto>,
    patterns: Vec<PatternDto>,
    mixer: Vec<StripDto>,
    playlist: PlaylistDto,
    modulators: Vec<ModulatorDto>,
    /// MIDI learn bindings (added in Step 10; absent in older files).
    midi_map: Vec<MidiBindingDto>,
}

#[derive(Serialize, Deserialize)]
struct MidiBindingDto {
    /// MIDI channel, 1 to 16 as musicians count them.
    channel: u8,
    cc: u8,
    param: String,
}

#[derive(Serialize, Deserialize)]
struct TempoDto {
    tick: i64,
    bpm: f64,
}

#[derive(Serialize, Deserialize)]
struct SigDto {
    bar: i64,
    num: u8,
    den: u8,
}

#[derive(Serialize, Deserialize)]
struct ChannelDto {
    id: u32,
    name: String,
    volume: f32,
    pan: f32,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    solo: bool,
    #[serde(default)]
    insert: usize,
    instrument: InstrumentDto,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum InstrumentDto {
    Sampler {
        sample: Option<SampleDto>,
        pitch: f32,
        start: f32,
        end: f32,
        #[serde(rename = "loop")]
        looped: bool,
        attack_ms: f32,
        decay_ms: f32,
        sustain: f32,
        release_ms: f32,
    },
    Synth {
        preset_name: String,
        params: BTreeMap<String, f32>,
        mods: Vec<SynthModDto>,
    },
}

#[derive(Serialize, Deserialize)]
struct SynthModDto {
    source: String,
    dest: String,
    amount: f32,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum SampleDto {
    BuiltIn {
        name: String,
    },
    File {
        /// Relative to the project file, '/'-separated, when it can be expressed so.
        #[serde(default)]
        path: Option<String>,
        /// Where the file was when saved.
        #[serde(default)]
        absolute: Option<String>,
        /// Zip entry of the embedded copy.
        #[serde(default)]
        embedded: Option<String>,
    },
}

#[derive(Serialize, Deserialize)]
struct PatternDto {
    id: u32,
    name: String,
    steps: u16,
    #[serde(default)]
    notes: Vec<ChannelNotesDto>,
}

#[derive(Serialize, Deserialize)]
struct ChannelNotesDto {
    channel: u32,
    /// `[start, length, key, velocity]` per note.
    notes: Vec<(i64, i64, u8, f32)>,
}

#[derive(Serialize, Deserialize)]
struct StripDto {
    name: String,
    volume: f32,
    pan: f32,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    solo: bool,
    #[serde(default)]
    phase_invert: bool,
    #[serde(default)]
    output: usize,
    #[serde(default)]
    sends: Vec<f32>,
    #[serde(default)]
    sidechain: Option<usize>,
    #[serde(default)]
    slots: Vec<Option<EffectDto>>,
}

#[derive(Serialize, Deserialize)]
struct EffectDto {
    kind: String,
    enabled: bool,
    params: BTreeMap<String, f32>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct PlaylistDto {
    next_id: u32,
    tracks: Vec<TrackDto>,
    clips: Vec<ClipDto>,
    markers: Vec<MarkerDto>,
}

#[derive(Serialize, Deserialize)]
struct TrackDto {
    id: u32,
    name: String,
    color: [u8; 3],
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    solo: bool,
    #[serde(default)]
    insert: usize,
}

#[derive(Serialize, Deserialize)]
struct ClipDto {
    id: u32,
    track: u32,
    start: i64,
    length: i64,
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    muted: bool,
    content: ClipContentDto,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum ClipContentDto {
    Pattern {
        pattern: u32,
    },
    Audio {
        source: SampleDto,
        gain: f32,
    },
    Automation {
        target: String,
        points: Vec<PointDto>,
    },
}

#[derive(Serialize, Deserialize)]
struct PointDto {
    at: i64,
    value: f32,
    curve: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tension: Option<f32>,
}

#[derive(Serialize, Deserialize)]
struct MarkerDto {
    at: i64,
    name: String,
}

#[derive(Serialize, Deserialize)]
struct ModulatorDto {
    id: u32,
    target: String,
    enabled: bool,
    amount: f32,
    source: ModSourceDto,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum ModSourceDto {
    Lfo {
        shape: String,
        /// Free rate in Hz, or absent when synced.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hz: Option<f32>,
        /// Synced period name ("1/4", "1 bar"), or absent when free.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sync: Option<String>,
        phase: f32,
    },
    Follower {
        strip: usize,
        attack_ms: f32,
        release_ms: f32,
        gain: f32,
    },
}

/// The project as the JSON tree written to `project.json`. Audio paths are made relative to
/// `dir` when given; `embedded` maps audio files to their zip entries.
pub fn to_value(
    project: &Project,
    dir: Option<&Path>,
    embedded: &HashMap<PathBuf, String>,
) -> Value {
    let file = FileDto {
        format: FORMAT.to_owned(),
        schema_version: migrate::CURRENT,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        project: project_dto(project, dir, embedded),
    };
    serde_json::to_value(file).unwrap_or(Value::Null)
}

fn sample_dto(
    s: &SampleSource,
    dir: Option<&Path>,
    embedded: &HashMap<PathBuf, String>,
) -> SampleDto {
    match s {
        SampleSource::BuiltIn(b) => SampleDto::BuiltIn {
            name: builtin_key(*b).to_owned(),
        },
        SampleSource::File(p) => SampleDto::File {
            path: dir.and_then(|d| relative_path(d, p)),
            absolute: Some(p.to_string_lossy().into_owned()),
            embedded: embedded.get(p).cloned(),
        },
    }
}

fn project_dto(p: &Project, dir: Option<&Path>, embedded: &HashMap<PathBuf, String>) -> ProjectDto {
    let channels = p
        .channels
        .iter()
        .map(|c| ChannelDto {
            id: c.id.0,
            name: c.name.clone(),
            volume: c.volume,
            pan: c.pan,
            mute: c.mute,
            solo: c.solo,
            insert: c.insert,
            instrument: match &c.instrument {
                Instrument::Sampler(s) => InstrumentDto::Sampler {
                    sample: s.sample.as_ref().map(|s| sample_dto(s, dir, embedded)),
                    pitch: s.pitch,
                    start: s.start,
                    end: s.end,
                    looped: s.loop_mode == LoopMode::Loop,
                    attack_ms: s.adsr.attack_ms,
                    decay_ms: s.adsr.decay_ms,
                    sustain: s.adsr.sustain,
                    release_ms: s.adsr.release_ms,
                },
                Instrument::Synth(patch) => InstrumentDto::Synth {
                    preset_name: patch.name.clone(),
                    params: SynthParam::ALL
                        .iter()
                        .map(|&q| (q.info().key.to_owned(), patch.get(q)))
                        .collect(),
                    // Every slot, in order: automation addresses rows by index.
                    mods: patch
                        .mods
                        .iter()
                        .map(|m| SynthModDto {
                            source: m.source.key().to_owned(),
                            dest: m.dest.key().to_owned(),
                            amount: m.amount,
                        })
                        .collect(),
                },
            },
        })
        .collect();
    let patterns = p
        .patterns
        .iter()
        .map(|pat| PatternDto {
            id: pat.id.0,
            name: pat.name.clone(),
            steps: pat.steps,
            notes: pat
                .notes
                .iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(ch, v)| ChannelNotesDto {
                    channel: ch.0,
                    notes: v
                        .iter()
                        .map(|n| (n.start, n.length, n.key, n.velocity))
                        .collect(),
                })
                .collect(),
        })
        .collect();
    let mixer = p
        .mixer
        .strips
        .iter()
        .map(|s| StripDto {
            name: s.name.clone(),
            volume: s.volume,
            pan: s.pan,
            mute: s.mute,
            solo: s.solo,
            phase_invert: s.phase_invert,
            output: s.output,
            sends: s.sends.to_vec(),
            sidechain: s.sidechain,
            slots: s
                .slots
                .iter()
                .map(|slot| {
                    slot.as_ref().map(|e| EffectDto {
                        kind: e.kind.key().to_owned(),
                        enabled: e.enabled,
                        params: e
                            .kind
                            .params()
                            .iter()
                            .zip(&e.params)
                            .map(|(info, &v)| (info.key.to_owned(), v))
                            .collect(),
                    })
                })
                .collect(),
        })
        .collect();
    let pl = &p.playlist;
    let playlist = PlaylistDto {
        next_id: pl.id_counter(),
        tracks: pl
            .tracks
            .iter()
            .map(|t| TrackDto {
                id: t.id.0,
                name: t.name.clone(),
                color: t.color,
                mute: t.mute,
                solo: t.solo,
                insert: t.insert,
            })
            .collect(),
        clips: pl
            .clips
            .iter()
            .map(|c| ClipDto {
                id: c.id.0,
                track: c.track.0,
                start: c.start,
                length: c.length,
                offset: c.offset,
                muted: c.muted,
                content: match &c.kind {
                    ClipKind::Pattern(id) => ClipContentDto::Pattern { pattern: id.0 },
                    ClipKind::Audio { source, gain } => ClipContentDto::Audio {
                        source: sample_dto(source, dir, embedded),
                        gain: *gain,
                    },
                    ClipKind::Automation(a) => ClipContentDto::Automation {
                        target: a.target.key(),
                        points: a
                            .points
                            .iter()
                            .map(|q| PointDto {
                                at: q.at,
                                value: q.value,
                                curve: curve_key(q.curve).to_owned(),
                                tension: match q.curve {
                                    Curve::Bezier(t) => Some(t),
                                    _ => None,
                                },
                            })
                            .collect(),
                    },
                },
            })
            .collect(),
        markers: pl
            .markers
            .iter()
            .map(|m| MarkerDto {
                at: m.at,
                name: m.name.clone(),
            })
            .collect(),
    };
    let modulators = p
        .modulators
        .iter()
        .map(|m| ModulatorDto {
            id: m.id.0,
            target: m.target.key(),
            enabled: m.enabled,
            amount: m.amount,
            source: match m.source {
                ModSourceKind::Lfo { shape, rate, phase } => ModSourceDto::Lfo {
                    shape: shape.key().to_owned(),
                    hz: match rate {
                        LfoRate::Hz(hz) => Some(hz),
                        LfoRate::Sync(_) => None,
                    },
                    sync: match rate {
                        LfoRate::Sync(i) => SYNC_RATES.get(i).map(|r| r.0.to_owned()),
                        LfoRate::Hz(_) => None,
                    },
                    phase,
                },
                ModSourceKind::Follower {
                    strip,
                    attack_ms,
                    release_ms,
                    gain,
                } => ModSourceDto::Follower {
                    strip,
                    attack_ms,
                    release_ms,
                    gain,
                },
            },
        })
        .collect();
    ProjectDto {
        next_id: p.id_counter(),
        current_pattern: p.current_pattern.0,
        swing: p.swing,
        tempo: p
            .tempo
            .points()
            .map(|t| TempoDto {
                tick: t.at.0,
                bpm: t.bpm,
            })
            .collect(),
        signatures: p
            .signatures
            .changes()
            .map(|c| SigDto {
                bar: c.bar,
                num: c.sig.num,
                den: c.sig.den,
            })
            .collect(),
        channels,
        patterns,
        mixer,
        playlist,
        modulators,
        midi_map: p
            .midi_map
            .iter()
            .map(|b| MidiBindingDto {
                channel: b.channel + 1,
                cc: b.cc,
                param: b.param.key(),
            })
            .collect(),
    }
}

/// Where the file says an audio file is, before looking on disk.
#[derive(Debug, Clone, Default)]
struct SampleRefs {
    relative: Option<PathBuf>,
    absolute: Option<PathBuf>,
    embedded: Option<String>,
}

/// A project read from JSON, before its audio files are looked for.
pub struct Parsed {
    /// The project. Audio files reference their saved absolute path (or the relative one when
    /// the file had no absolute path).
    pub project: Project,
    refs: Vec<(PathBuf, SampleRefs)>,
    missing: Vec<PathBuf>,
    extracted: usize,
    /// Things dropped or repaired.
    pub warnings: Vec<String>,
    /// Schema version of the file.
    pub schema_version: u32,
}

/// Reads the JSON tree of a project (any supported schema version). Audio paths given
/// relative are resolved against `dir`.
pub fn from_value(value: Value, dir: Option<&Path>) -> Result<Parsed, FileError> {
    let (value, version) = migrate::upgrade(value)?;
    let file: FileDto =
        serde_json::from_value(value).map_err(|e| FileError::Format(e.to_string()))?;
    let mut warnings = Vec::new();
    let mut refs: Vec<(PathBuf, SampleRefs)> = Vec::new();
    let mut sample = |s: SampleDto, warnings: &mut Vec<String>| -> Option<SampleSource> {
        match s {
            SampleDto::BuiltIn { name } => {
                let b = BuiltInSample::ALL
                    .into_iter()
                    .find(|b| builtin_key(*b) == name);
                if b.is_none() {
                    warnings.push(format!("unknown built-in sample \"{name}\""));
                }
                b.map(SampleSource::BuiltIn)
            }
            SampleDto::File {
                path,
                absolute,
                embedded,
            } => {
                let relative = path
                    .filter(|p| !p.is_empty())
                    .and_then(|p| dir.map(|d| d.join(from_slashes(&p))));
                let absolute = absolute.filter(|p| !p.is_empty()).map(PathBuf::from);
                let key = absolute.clone().or_else(|| relative.clone())?;
                if !refs.iter().any(|(k, _)| *k == key) {
                    refs.push((
                        key.clone(),
                        SampleRefs {
                            relative,
                            absolute,
                            embedded,
                        },
                    ));
                }
                Some(SampleSource::File(key))
            }
        }
    };

    let d = file.project;
    let mut p = Project::empty();
    p.patterns.clear();
    p.swing = d.swing;
    let tempo: Vec<TempoPoint> = d
        .tempo
        .iter()
        .map(|t| TempoPoint {
            at: Tick(t.tick),
            bpm: t.bpm,
        })
        .collect();
    p.tempo = if tempo.is_empty() {
        TempoMap::default()
    } else {
        TempoMap::from_points_lossy(&tempo)
    };
    let sigs: Vec<SigChange> = d
        .signatures
        .iter()
        .map(|s| SigChange {
            bar: s.bar,
            sig: TimeSig::new(s.num, s.den),
        })
        .collect();
    p.signatures = TimeSigMap::new(&sigs);

    for c in d.channels {
        let instrument = match c.instrument {
            InstrumentDto::Sampler {
                sample: src,
                pitch,
                start,
                end,
                looped,
                attack_ms,
                decay_ms,
                sustain,
                release_ms,
            } => Instrument::Sampler(SamplerSettings {
                sample: src.and_then(|s| sample(s, &mut warnings)),
                pitch,
                start,
                end,
                loop_mode: if looped {
                    LoopMode::Loop
                } else {
                    LoopMode::OneShot
                },
                adsr: Adsr {
                    attack_ms,
                    decay_ms,
                    sustain,
                    release_ms,
                },
            }),
            InstrumentDto::Synth {
                preset_name,
                params,
                mods,
            } => {
                let mut patch = SynthPatch {
                    name: preset_name,
                    ..SynthPatch::default()
                };
                for (key, v) in &params {
                    match SynthParam::from_key(key) {
                        Some(q) => patch.set(q, *v),
                        None => warnings.push(format!("unknown synth parameter \"{key}\"")),
                    }
                }
                for (slot, m) in patch.mods.iter_mut().zip(mods.iter().take(MOD_SLOTS)) {
                    *slot = ModSlot {
                        source: ModSource::from_key(&m.source).unwrap_or(ModSource::Off),
                        dest: ModDest::from_key(&m.dest).unwrap_or(ModDest::Off),
                        amount: m.amount,
                    };
                }
                Instrument::Synth(Box::new(patch))
            }
        };
        p.channels.push(Channel {
            id: ChannelId(c.id),
            name: c.name,
            volume: c.volume,
            pan: c.pan,
            mute: c.mute,
            solo: c.solo,
            instrument,
            insert: c.insert,
        });
    }
    for pat in d.patterns {
        let mut notes = BTreeMap::new();
        for cn in pat.notes {
            let v: Vec<Note> = cn
                .notes
                .iter()
                .map(|&(start, length, key, velocity)| Note {
                    start,
                    length,
                    key,
                    velocity,
                })
                .collect();
            notes.insert(ChannelId(cn.channel), v);
        }
        p.patterns.push(Pattern {
            id: PatternId(pat.id),
            name: pat.name,
            steps: pat.steps,
            notes,
        });
    }
    p.current_pattern = PatternId(d.current_pattern);

    for (strip, sd) in p.mixer.strips.iter_mut().zip(d.mixer) {
        strip.name = sd.name;
        strip.volume = sd.volume;
        strip.pan = sd.pan;
        strip.mute = sd.mute;
        strip.solo = sd.solo;
        strip.phase_invert = sd.phase_invert;
        strip.output = sd.output;
        for (to, from) in strip.sends.iter_mut().zip(sd.sends.iter().take(SENDS)) {
            *to = *from;
        }
        strip.sidechain = sd.sidechain;
        for (slot, ed) in strip
            .slots
            .iter_mut()
            .zip(sd.slots.into_iter().take(FX_SLOTS))
        {
            *slot = ed.and_then(|ed| {
                let Some(kind) = EffectKind::from_key(&ed.kind) else {
                    warnings.push(format!("unknown effect \"{}\" removed", ed.kind));
                    return None;
                };
                let mut e = EffectSlot::new(kind);
                e.enabled = ed.enabled;
                for (key, v) in &ed.params {
                    e = e.with(key, *v);
                }
                Some(e)
            });
        }
    }

    let pl = &mut p.playlist;
    pl.tracks.clear();
    for t in d.playlist.tracks {
        pl.tracks.push(Track {
            id: TrackId(t.id),
            name: t.name,
            color: t.color,
            mute: t.mute,
            solo: t.solo,
            insert: t.insert,
        });
    }
    for c in d.playlist.clips {
        let kind = match c.content {
            ClipContentDto::Pattern { pattern } => Some(ClipKind::Pattern(PatternId(pattern))),
            ClipContentDto::Audio { source, gain } => {
                sample(source, &mut warnings).map(|source| ClipKind::Audio { source, gain })
            }
            ClipContentDto::Automation { target, points } => match ParamId::parse(&target) {
                Some(target) => Some(ClipKind::Automation(Automation {
                    target,
                    points: points
                        .iter()
                        .map(|q| AutoPoint {
                            at: q.at,
                            value: q.value,
                            curve: curve_from_key(&q.curve, q.tension),
                        })
                        .collect(),
                })),
                None => {
                    warnings.push(format!(
                        "automation clip for unknown parameter \"{target}\" removed"
                    ));
                    None
                }
            },
        };
        if let Some(kind) = kind {
            pl.clips.push(Clip {
                id: ClipId(c.id),
                track: TrackId(c.track),
                start: c.start,
                length: c.length,
                offset: c.offset,
                muted: c.muted,
                kind,
            });
        }
    }
    for m in d.playlist.markers {
        pl.markers.push(Marker {
            at: m.at,
            name: m.name,
        });
    }
    pl.set_id_counter(d.playlist.next_id);

    for m in d.modulators {
        let Some(target) = ParamId::parse(&m.target) else {
            warnings.push(format!(
                "modulator for unknown parameter \"{}\" removed",
                m.target
            ));
            continue;
        };
        let source = match m.source {
            ModSourceDto::Lfo {
                shape,
                hz,
                sync,
                phase,
            } => ModSourceKind::Lfo {
                shape: LfoShape::ALL
                    .into_iter()
                    .find(|s| s.key() == shape)
                    .unwrap_or_default(),
                rate: match (
                    sync.and_then(|n| SYNC_RATES.iter().position(|r| r.0 == n)),
                    hz,
                ) {
                    (Some(i), _) => LfoRate::Sync(i),
                    (None, Some(hz)) => LfoRate::Hz(hz),
                    (None, None) => LfoRate::Sync(5),
                },
                phase,
            },
            ModSourceDto::Follower {
                strip,
                attack_ms,
                release_ms,
                gain,
            } => ModSourceKind::Follower {
                strip,
                attack_ms,
                release_ms,
                gain,
            },
        };
        p.modulators.push(Modulator {
            id: ModulatorId(m.id),
            target,
            source,
            amount: m.amount,
            enabled: m.enabled,
        });
    }
    for b in d.midi_map {
        match ParamId::parse(&b.param) {
            Some(param) if (1..=16).contains(&b.channel) => p.midi_map.push(gt_core::MidiBinding {
                channel: b.channel - 1,
                cc: b.cc,
                param,
            }),
            _ => warnings.push(format!(
                "MIDI binding of controller {} to \"{}\" removed",
                b.cc, b.param
            )),
        }
    }
    p.set_id_counter(d.next_id);
    p.sanitize();
    Ok(Parsed {
        project: p,
        refs,
        missing: Vec::new(),
        extracted: 0,
        warnings,
        schema_version: version,
    })
}

fn builtin_key(b: BuiltInSample) -> &'static str {
    match b {
        BuiltInSample::Kick => "kick",
        BuiltInSample::Snare => "snare",
        BuiltInSample::Hat => "hat",
        BuiltInSample::Clap => "clap",
    }
}

fn curve_key(c: Curve) -> &'static str {
    match c {
        Curve::Hold => "hold",
        Curve::Linear => "linear",
        Curve::Smooth => "smooth",
        Curve::Bezier(_) => "bezier",
    }
}

fn curve_from_key(key: &str, tension: Option<f32>) -> Curve {
    match key {
        "hold" => Curve::Hold,
        "smooth" => Curve::Smooth,
        "bezier" => Curve::Bezier(tension.unwrap_or(0.0)),
        _ => Curve::Linear,
    }
}

/// 64-bit FNV-1a, to name embedded files by content.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// A file name safe inside a zip and on every file system.
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn absolute(p: &Path) -> PathBuf {
    if p.as_os_str().is_empty() {
        return std::env::current_dir().unwrap_or_default();
    }
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `target` relative to the folder `base`, '/'-separated, or `None` when there is no relative
/// path (different drive on Windows, or either is not absolute).
fn relative_path(base: &Path, target: &Path) -> Option<String> {
    if !base.is_absolute() || !target.is_absolute() {
        return None;
    }
    let b: Vec<Component> = base.components().collect();
    let t: Vec<Component> = target.components().collect();
    if b.first() != t.first() {
        return None;
    }
    let common = b.iter().zip(&t).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_owned(), b.len() - common).collect();
    for c in &t[common..] {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

fn from_slashes(p: &str) -> PathBuf {
    p.split('/').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gt-file-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_demo_round_trips_exactly() {
        let dir = temp_dir("demo");
        let path = dir.join("demo.gloom");
        let p = Project::demo();
        save(&p, &path, SaveOptions::default()).unwrap();
        let loaded = load(&path, &dir.join("x")).unwrap();
        assert_eq!(loaded.project, p);
        assert!(loaded.missing.is_empty() && loaded.warnings.is_empty());
        assert_eq!(loaded.schema_version, migrate::CURRENT);
        // New ids continue after the saved ones.
        let mut a = p.clone();
        let mut b = loaded.project;
        assert_eq!(a.new_pattern(), b.new_pattern());
        assert_eq!(a.playlist.add_track(), b.playlist.add_track());
    }

    #[test]
    fn every_parameter_and_curve_survives() {
        let mut p = Project::demo();
        // Change every parameter of the project to a non-default, in-range value.
        for id in ParamId::all(&p) {
            let info = id.info();
            id.set(&mut p, info.from_normalized(0.37));
        }
        let mut c = p.playlist.clips.clone();
        for clip in &mut c {
            if let ClipKind::Automation(a) = &mut clip.kind {
                a.points[0].curve = Curve::Hold;
            }
        }
        p.playlist.clips = c;
        let pan = ParamId::Channel {
            channel: p.channels[1].id,
            param: gt_core::ChannelParam::Pan,
        };
        assert!(p.learn_midi(9, 74, pan));
        assert!(p.learn_midi(0, 1, gt_core::MASTER_VOLUME));
        // Start = end is not a valid sampler range; loading repairs it, so compare with the
        // repaired project.
        p.sanitize();
        let v = to_value(&p, None, &HashMap::new());
        let back = from_value(v, None).unwrap();
        let q = &back.project;
        assert_eq!(q.channels, p.channels);
        assert_eq!(q.mixer, p.mixer);
        assert_eq!(q.playlist, p.playlist);
        assert_eq!(q.modulators, p.modulators);
        assert_eq!(q.midi_map, p.midi_map);
        assert_eq!(q, &p);
    }

    #[test]
    fn relative_paths_survive_moving_the_project_folder() {
        let dir = temp_dir("rel");
        std::fs::create_dir_all(dir.join("a/samples")).unwrap();
        std::fs::write(dir.join("a/samples/hit.wav"), b"RIFF").unwrap();
        let mut p = Project::empty();
        p.add_channel(
            "Hit",
            Some(SampleSource::File(dir.join("a/samples/hit.wav"))),
        );
        save(&p, &dir.join("a/song.gloom"), SaveOptions::default()).unwrap();
        std::fs::rename(dir.join("a"), dir.join("b")).unwrap();
        let loaded = load(&dir.join("b/song.gloom"), &dir.join("x")).unwrap();
        assert!(loaded.missing.is_empty());
        assert_eq!(
            loaded.project.channels[0].sample(),
            Some(&SampleSource::File(
                dir.join("b").join("samples").join("hit.wav")
            ))
        );
    }

    #[test]
    fn embedded_samples_are_restored_when_the_originals_are_gone() {
        let dir = temp_dir("embed");
        let wav = dir.join("loop.wav");
        std::fs::write(&wav, b"RIFF....fake").unwrap();
        let mut p = Project::empty();
        p.add_channel("Loop", Some(SampleSource::File(wav.clone())));
        let path = dir.join("song.gloom");
        let report = save(
            &p,
            &path,
            SaveOptions {
                embed_samples: true,
            },
        )
        .unwrap();
        assert_eq!(report.embedded, 1);
        std::fs::remove_file(&wav).unwrap();
        let loaded = load(&path, &dir.join("extracted")).unwrap();
        assert!(loaded.missing.is_empty());
        assert_eq!(loaded.extracted, 1);
        let Some(SampleSource::File(got)) = loaded.project.channels[0].sample() else {
            panic!()
        };
        assert!(got.starts_with(dir.join("extracted")));
        assert_eq!(std::fs::read(got).unwrap(), b"RIFF....fake");
    }

    #[test]
    fn missing_samples_are_listed_and_can_be_relinked() {
        let dir = temp_dir("missing");
        let gone = dir.join("old").join("kick.wav");
        let mut p = Project::empty();
        p.add_channel("Kick", Some(SampleSource::File(gone.clone())));
        let path = dir.join("song.gloom");
        save(&p, &path, SaveOptions::default()).unwrap();
        let mut loaded = load(&path, &dir.join("x")).unwrap();
        assert_eq!(loaded.missing, vec![gone.clone()]);
        std::fs::create_dir_all(dir.join("new/deep")).unwrap();
        std::fs::write(dir.join("new/deep/KICK.wav"), b"x").unwrap();
        let found = find_by_name(&dir.join("new"), &loaded.missing, 4);
        let to = &found[&gone];
        assert_eq!(relink(&mut loaded.project, &gone, to), 1);
        assert_eq!(sample_files(&loaded.project), vec![to.clone()]);
    }

    #[test]
    fn damaged_and_foreign_files_are_rejected_with_a_reason() {
        let dir = temp_dir("bad");
        let path = dir.join("bad.gloom");
        std::fs::write(&path, b"not a zip").unwrap();
        assert!(matches!(load(&path, &dir), Err(FileError::Container(_))));
        let v =
            serde_json::json!({ "format": "something-else", "schema_version": 1, "project": {} });
        assert!(matches!(from_value(v, None), Err(FileError::Format(_))));
    }

    #[test]
    fn bad_values_are_repaired_and_unknown_keys_dropped() {
        let mut v = to_value(&Project::demo(), None, &HashMap::new());
        let pr = &mut v["project"];
        pr["channels"][0]["volume"] = serde_json::json!(1e9);
        pr["mixer"][0]["slots"][9]["kind"] = serde_json::json!("time-machine");
        pr["playlist"]["clips"][4]["content"]["target"] = serde_json::json!("nowhere/1");
        pr["future_field"] = serde_json::json!(42);
        let back = from_value(v, None).unwrap();
        assert_eq!(back.project.channels[0].volume, Channel::MAX_VOLUME);
        assert!(back.project.mixer.strips[0].slots[9].is_none());
        assert_eq!(back.warnings.len(), 2, "{:?}", back.warnings);
    }

    #[test]
    fn relative_path_cases() {
        let base = std::env::temp_dir().join("p");
        let t = base.join("s").join("a.wav");
        assert_eq!(relative_path(&base, &t).as_deref(), Some("s/a.wav"));
        let up = base.parent().unwrap().join("lib").join("b.wav");
        assert_eq!(relative_path(&base, &up).as_deref(), Some("../lib/b.wav"));
        assert_eq!(relative_path(Path::new("rel"), &t), None);
    }
}
