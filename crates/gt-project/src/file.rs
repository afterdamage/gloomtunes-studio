//! The project file: a `.gloom` zip container holding `project.json` and, optionally, the
//! audio files the project uses.
//!
//! ```text
//! project.json          { "format": "gloomtunes-project", "schema_version": 8, "project": {...} }
//! samples/<hash>-<name> embedded copies of the audio files, byte for byte (optional)
//! plugins/<hash>.bin    the saved state of each third-party plugin, as the plugin wrote it
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
//!
//! Third-party plugins (Step 11) are stored by format, id and a path hint, with their parameter
//! descriptions and values (the document's mirror) in the JSON and their opaque state blob as a
//! zip entry named by its content hash. A plugin that cannot be found when the project is
//! opened keeps all of this, so saving again loses nothing.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use gt_core::effects::EffectKind;
use gt_core::modulation::SYNC_RATES;
use gt_core::synth::{ModDest, ModSlot, ModSource, SynthParam, SynthPatch, MOD_SLOTS};
use gt_core::{
    Adsr, AudioEdit, AudioInput, AudioTake, AutoPoint, Automation, BuiltInSample, Channel,
    ChannelId, Clip, ClipId, ClipKind, ClipStretch, Curve, CurveKind, CurvePoint, DrumKit,
    EffectSlot, Fade, FadeCurve, Frozen, Gap, Instrument, Interp, KeyZone, LfoRate, LfoShape,
    LfoTarget, LoopMode, Marker, MarkerAction, MidiControl, ModSourceKind, Modulator, ModulatorId,
    Note, NoteCurve, Pad, PadLayer, PanLaw, ParamId, Pattern, PatternId, Playlist, PluginFormat,
    PluginInstanceId, PluginKind, PluginParamInfo, PluginRef, Project, SampleSource, SamplerFilter,
    SamplerFilterMode, SamplerLfo, SamplerSettings, SigChange, StretchMode, TempoMap, TempoPoint,
    Tick, TimeSig, TimeSigMap, Track, TrackId, WarpMarker, FX_SLOTS,
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
const PLUGINS_DIR: &str = "plugins/";

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
        let mut states: Vec<(String, &[u8])> = project
            .plugins()
            .into_iter()
            .filter(|(_, p)| !p.state.is_empty())
            .map(|(_, p)| (state_entry(&p.state), &p.state[..]))
            .collect();
        states.sort_by(|a, b| a.0.cmp(&b.0));
        states.dedup_by(|a, b| a.0 == b.0);
        for (entry, bytes) in states {
            zip.start_file(entry.as_str(), deflate)?;
            zip.write_all(bytes)?;
        }
        zip.finish()?.flush()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(report)
}

/// Fill every player's sound from its pack (extracted under `cache`). A pack that is missing
/// or broken leaves its player silent, with a warning.
pub fn resolve_packs(project: &mut Project, cache: &Path, warnings: &mut Vec<String>) {
    let mut opened: HashMap<PathBuf, Option<crate::pack::Pack>> = HashMap::new();
    for ch in &mut project.channels {
        let Some(pl) = ch.player_mut() else {
            continue;
        };
        let pack = opened.entry(pl.pack.clone()).or_insert_with(|| {
            crate::pack::open(&pl.pack, cache)
                .inspect_err(|e| warnings.push(format!("pack {}: {e}", pl.pack.display())))
                .ok()
        });
        if let Some(pack) = pack {
            if !pack.refill(pl) {
                warnings.push(format!(
                    "pack {} has no preset \"{}\"",
                    pl.pack.display(),
                    pl.preset
                ));
            }
        }
    }
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
    resolve_packs(
        &mut loaded.project,
        &crate::pack::default_cache(),
        &mut loaded.warnings,
    );

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
    for (id, entry) in std::mem::take(&mut loaded.plugin_states) {
        let mut bytes = Vec::new();
        let read = zip
            .by_name(&entry)
            .map_err(|e| e.to_string())
            .and_then(|mut f| f.read_to_end(&mut bytes).map_err(|e| e.to_string()));
        match (read, loaded.project.plugin_by_instance_mut(id)) {
            (Ok(_), Some(p)) => p.state = std::sync::Arc::from(bytes),
            (Err(e), Some(p)) => loaded.warnings.push(format!(
                "the saved state of {} could not be read: {e}",
                p.name
            )),
            _ => {}
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

/// Every audio file the project references (channels, audio clips and their takes), without
/// duplicates.
pub fn sample_files(project: &Project) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = project
        .channels
        .iter()
        // A player's audio comes from its pack.
        .filter(|c| c.player().is_none())
        .flat_map(|c| c.samples())
        .chain(project.playlists().flat_map(|pl| &pl.clips).flat_map(|c| {
            match &c.kind {
                ClipKind::Audio { source, takes, .. } => std::iter::once(source)
                    .chain(takes.iter().map(|t| &t.source))
                    .collect(),
                _ => Vec::new(),
            }
        }))
        .chain(
            project
                .playlists()
                .flat_map(|pl| &pl.tracks)
                .filter_map(|t| t.frozen.as_ref().map(|f| &f.source)),
        )
        .filter_map(|s| match s {
            SampleSource::File(p) => Some(p.clone()),
            SampleSource::BuiltIn(_) => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Points every use of the audio file `from` (channels, audio clips and takes) at `to`. Returns how
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
        c.samples_mut().for_each(&mut fix);
    }
    for pl in project.playlists_mut() {
        for c in &mut pl.clips {
            if let ClipKind::Audio { source, takes, .. } = &mut c.kind {
                fix(source);
                for t in takes {
                    fix(&mut t.source);
                }
            }
        }
        for t in &mut pl.tracks {
            if let Some(f) = &mut t.frozen {
                fix(&mut f.source);
            }
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
    /// Mixer pan law ("-3"; schema 8), left out at 0 dB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pan_law: Option<String>,
    playlist: PlaylistDto,
    /// Every arrangement (schema 9), left out when there is only the default one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    arrangements: Vec<ArrangementDto>,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    current_arrangement: usize,
    modulators: Vec<ModulatorDto>,
    /// MIDI learn bindings (added in Step 10; absent in older files).
    midi_map: Vec<MidiBindingDto>,
    /// MIDI inputs playing fixed channels (Prompt 16).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    midi_routes: Vec<MidiRouteDto>,
    /// Title, artist, genre and notes (Prompt 25).
    #[serde(skip_serializing_if = "InfoDto::is_empty")]
    info: InfoDto,
}

#[derive(Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
struct InfoDto {
    #[serde(skip_serializing_if = "String::is_empty")]
    title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    artist: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    genre: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    notes: String,
}

impl InfoDto {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Serialize, Deserialize)]
struct MidiRouteDto {
    port: String,
    /// 1 to 16, or absent for every channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    channel: Option<u8>,
    target: u32,
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
    /// Ramp to the next point ("linear", "smooth", "bezier"; schema 10), absent for a step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ramp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tension: Option<f32>,
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
    #[serde(default = "default_bend")]
    bend_range: f32,
    /// Own swing (Prompt 27); absent: the rack's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    swing: Option<f32>,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    choke: u8,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    group: String,
    /// Key settings of non-sampler instruments; absent when default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keys: Option<KeysDto>,
    instrument: InstrumentDto,
}

#[derive(Serialize, Deserialize)]
struct KeysDto {
    root: u8,
    lo: u8,
    hi: u8,
    #[serde(default)]
    mono: bool,
    #[serde(default)]
    voices: u8,
}

fn default_bend() -> f32 {
    Channel::DEFAULT_BEND
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
        /// Sampler 2 settings (Prompt 23); absent in older files and when all are default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sampler2: Option<Sampler2Dto>,
    },
    Synth {
        preset_name: String,
        params: BTreeMap<String, f32>,
        mods: Vec<SynthModDto>,
        /// User wavetables of the two wavetable oscillators (Prompt 24).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tables: Vec<Option<SampleDto>>,
    },
    /// A third-party plugin (Step 11).
    Plugin { plugin: PluginDto },
    /// Drum pads (Prompt 23).
    Drums { banks: Vec<Vec<PadDto>> },
    /// A layer channel (Prompt 27): channel ids it plays.
    Layer {
        children: Vec<u32>,
        #[serde(default)]
        mode: String,
        #[serde(default)]
        split: bool,
    },
    /// A preset player (Prompt 28): its pack, preset and macros; the sound comes from the pack.
    Player {
        /// Pack path relative to the project folder.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pack: Option<String>,
        /// Absolute pack path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pack_absolute: Option<String>,
        preset: String,
        #[serde(default)]
        macros: Vec<MacroDto>,
    },
    /// A modular patch (Prompt 34).
    Patcher { patch: PatchDto },
    /// A MIDI out channel (Prompt 33): the port by name and what to send.
    MidiOut {
        #[serde(default)]
        port: String,
        #[serde(default)]
        channel: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        program: Option<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bank: Option<u16>,
        #[serde(default)]
        latency_ms: f32,
    },
}

#[derive(Serialize, Deserialize)]
struct MacroDto {
    name: String,
    target: String,
    min: f32,
    max: f32,
    value: f32,
}

#[derive(Serialize, Deserialize)]
struct PadLayerDto {
    sample: SampleDto,
    vel_lo: f32,
    vel_hi: f32,
}

#[derive(Serialize, Deserialize)]
struct PadDto {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    layers: Vec<PadLayerDto>,
    tune: f32,
    gain: f32,
    pan: f32,
    env: EnvDto,
    #[serde(default)]
    choke: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    synth: Option<DrumVoiceDto>,
}

/// A drum synth voice (Prompt 28): its model by name and every parameter in plain units.
#[derive(Serialize, Deserialize)]
struct DrumVoiceDto {
    model: String,
    length: f32,
    pitch: f32,
    pitch2: f32,
    sweep: f32,
    sweep_time: f32,
    decay: f32,
    tone: f32,
    noise: f32,
    noise_decay: f32,
    noise_freq: f32,
    noise_q: f32,
    click: f32,
    drive: f32,
    spread: f32,
    tail_at: f32,
    seed: u32,
}

impl DrumVoiceDto {
    fn of(v: &gt_core::DrumVoice) -> Self {
        Self {
            model: v.model.label().to_ascii_lowercase(),
            length: v.length,
            pitch: v.pitch,
            pitch2: v.pitch2,
            sweep: v.sweep,
            sweep_time: v.sweep_time,
            decay: v.decay,
            tone: v.tone,
            noise: v.noise,
            noise_decay: v.noise_decay,
            noise_freq: v.noise_freq,
            noise_q: v.noise_q,
            click: v.click,
            drive: v.drive,
            spread: v.spread,
            tail_at: v.tail_at,
            seed: v.seed,
        }
    }

    /// The voice; an unknown model plays as a kick.
    fn voice(&self) -> gt_core::DrumVoice {
        let model = gt_core::DrumModel::ALL
            .into_iter()
            .find(|m| m.label().eq_ignore_ascii_case(&self.model))
            .unwrap_or_default();
        gt_core::DrumVoice {
            model,
            length: self.length,
            pitch: self.pitch,
            pitch2: self.pitch2,
            sweep: self.sweep,
            sweep_time: self.sweep_time,
            decay: self.decay,
            tone: self.tone,
            noise: self.noise,
            noise_decay: self.noise_decay,
            noise_freq: self.noise_freq,
            noise_q: self.noise_q,
            click: self.click,
            drive: self.drive,
            spread: self.spread,
            tail_at: self.tail_at,
            seed: self.seed,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct EnvDto {
    attack_ms: f32,
    decay_ms: f32,
    sustain: f32,
    release_ms: f32,
}

impl EnvDto {
    fn of(a: &Adsr) -> Self {
        Self {
            attack_ms: a.attack_ms,
            decay_ms: a.decay_ms,
            sustain: a.sustain,
            release_ms: a.release_ms,
        }
    }

    fn adsr(&self) -> Adsr {
        Adsr {
            attack_ms: self.attack_ms,
            decay_ms: self.decay_ms,
            sustain: self.sustain,
            release_ms: self.release_ms,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct LfoDto {
    shape: String,
    rate: f32,
    target: String,
    amount: f32,
}

#[derive(Serialize, Deserialize)]
struct ZoneDto {
    sample: SampleDto,
    root: u8,
    key_lo: u8,
    key_hi: u8,
    vel_lo: f32,
    vel_hi: f32,
    #[serde(default)]
    tune: f32,
    #[serde(default = "unity")]
    gain: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    start: f32,
    #[serde(default = "unity", skip_serializing_if = "is_one")]
    end: f32,
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

fn unity() -> f32 {
    1.0
}

/// Sampler 2 settings (Prompt 23).
#[derive(Serialize, Deserialize)]
struct Sampler2Dto {
    interp: String,
    reverse: bool,
    filter: String,
    cutoff: f32,
    resonance: f32,
    filter_env_amount: f32,
    filter_env: EnvDto,
    lfos: Vec<LfoDto>,
    root_key: u8,
    key_lo: u8,
    key_hi: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    zones: Vec<ZoneDto>,
    round_robin: bool,
    mono: bool,
    legato: bool,
    glide_ms: f32,
    voices: u8,
    stretch_beats: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    slice_beats: f32,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct PluginDto {
    format: String,
    id: String,
    name: String,
    #[serde(default)]
    vendor: String,
    kind: String,
    /// Where the plugin file was when saved (plugins are found by id; this breaks ties).
    #[serde(default)]
    path: String,
    /// Zip entry of the plugin's saved state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    /// The parameters as the plugin described them, with their values (normalized 0..1).
    #[serde(default)]
    params: Vec<PluginParamDto>,
    /// Extra stereo outputs (multi-output instruments) and their inserts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    outputs: Vec<PluginOutputDto>,
    /// The plugin sends MIDI.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    sends_midi: bool,
    /// Channel id that plays its MIDI output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    midi_out: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct PluginOutputDto {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route: Option<u16>,
}

#[derive(Serialize, Deserialize)]
struct PluginParamDto {
    id: u32,
    name: String,
    default: f32,
    #[serde(default)]
    steps: u32,
    #[serde(default)]
    automatable: bool,
    #[serde(default)]
    hidden: bool,
    value: f32,
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
    /// Colour (Prompt 27); absent: the default.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    color: u8,
    #[serde(default)]
    notes: Vec<ChannelNotesDto>,
    /// Recorded controller data (Prompt 16); absent in older files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    controls: Vec<ChannelControlsDto>,
    /// Pattern automation (Prompt 17); absent in older files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    automation: Vec<PatternAutoDto>,
}

#[derive(Serialize, Deserialize)]
struct PatternAutoDto {
    target: String,
    points: Vec<PointDto>,
}

#[derive(Serialize, Deserialize)]
struct ChannelControlsDto {
    channel: u32,
    /// `[tick, kind, value]` per point; kinds 0 bend, 1 sustain, 2 mod wheel, 3 aftertouch.
    points: Vec<(i64, u8, f32)>,
}

#[derive(Serialize, Deserialize)]
struct ChannelNotesDto {
    channel: u32,
    /// `[start, length, key, velocity]` per plain note, an object for a note with per-note
    /// values (schema 5).
    notes: Vec<NoteDto>,
}

/// A note in the file: the compact array when every per-note value is neutral.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum NoteDto {
    Plain((i64, i64, u8, f32)),
    Full(FullNoteDto),
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

fn neutral() -> f32 {
    Note::NEUTRAL
}

fn is_neutral(v: &f32) -> bool {
    *v == Note::NEUTRAL
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

#[derive(Serialize, Deserialize)]
struct FullNoteDto {
    start: i64,
    length: i64,
    key: u8,
    velocity: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pan: f32,
    #[serde(default = "neutral", skip_serializing_if = "is_neutral")]
    release: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    fine: f32,
    #[serde(default = "neutral", skip_serializing_if = "is_neutral")]
    mod_x: f32,
    #[serde(default = "neutral", skip_serializing_if = "is_neutral")]
    mod_y: f32,
    #[serde(default, skip_serializing_if = "is_false")]
    slide: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    porta: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    muted: bool,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    color: u8,
    /// Polyphonic automation (schema 12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    curve: Option<NoteCurveDto>,
}

/// A note's expression curve: its kind and `[position, value]` points.
#[derive(Serialize, Deserialize)]
struct NoteCurveDto {
    kind: String,
    points: Vec<(f32, f32)>,
}

impl NoteCurveDto {
    fn of(c: &NoteCurve) -> Option<Self> {
        (!c.is_off()).then(|| Self {
            kind: c.kind.key().to_owned(),
            points: c.points().iter().map(|p| (p.at, p.value)).collect(),
        })
    }

    fn curve(&self) -> NoteCurve {
        let Some(kind) = CurveKind::from_key(&self.kind) else {
            return NoteCurve::default();
        };
        let points: Vec<CurvePoint> = self
            .points
            .iter()
            .map(|&(at, value)| CurvePoint { at, value })
            .collect();
        NoteCurve::new(kind, &points)
    }
}

impl NoteDto {
    fn of(n: &Note) -> Self {
        if n.is_plain() {
            return Self::Plain((n.start, n.length, n.key, n.velocity));
        }
        Self::Full(FullNoteDto {
            start: n.start,
            length: n.length,
            key: n.key,
            velocity: n.velocity,
            pan: n.pan,
            release: n.release,
            fine: n.fine,
            mod_x: n.mod_x,
            mod_y: n.mod_y,
            slide: n.slide,
            porta: n.porta,
            muted: n.muted,
            color: n.color,
            curve: NoteCurveDto::of(&n.curve),
        })
    }

    fn note(&self) -> Note {
        let mut n = match *self {
            Self::Plain((start, length, key, velocity)) => Note::new(start, length, key, velocity),
            Self::Full(ref f) => Note {
                pan: f.pan,
                release: f.release,
                fine: f.fine,
                mod_x: f.mod_x,
                mod_y: f.mod_y,
                slide: f.slide,
                porta: f.porta,
                muted: f.muted,
                color: f.color,
                curve: f
                    .curve
                    .as_ref()
                    .map(NoteCurveDto::curve)
                    .unwrap_or_default(),
                ..Note::new(f.start, f.length, f.key, f.velocity)
            },
        };
        n.sanitize();
        n
    }
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
    /// Sends to other strips (schema 8; see `migrate::v7_to_v8`).
    #[serde(default)]
    sends: Vec<SendDto>,
    #[serde(default)]
    sidechain: Option<usize>,
    #[serde(default)]
    slots: Vec<Option<EffectDto>>,
    /// Live device input ("1", "1-2"; schema 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input: Option<String>,
    /// Strip colour (Prompt 29).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<[u8; 3]>,
    /// Strip icon (Prompt 29).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<u8>,
}

#[derive(Serialize, Deserialize)]
struct SendDto {
    to: usize,
    level: f32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pre: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct EffectDto {
    kind: String,
    enabled: bool,
    params: BTreeMap<String, f32>,
    /// The plugin of a plugin slot (Step 11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plugin: Option<PluginDto>,
    /// Slot wet/dry mix (Prompt 19); left out at 100 %.
    #[serde(default = "full_mix", skip_serializing_if = "is_full_mix")]
    mix: f32,
    /// The effect's audio file (a convolution response, Prompt 20): relative to the project
    /// file when it could be, and absolute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_absolute: Option<String>,
    /// A patcher slot's patch (Prompt 34).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    patch: Option<PatchDto>,
}

fn full_mix() -> f32 {
    1.0
}

fn is_full_mix(v: &f32) -> bool {
    *v == 1.0
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
    /// Recording input ("1", "1-2"; schema 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input: Option<String>,
    /// Freeze render (schema 4, optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frozen: Option<FrozenDto>,
    /// Folder track (schema 9).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    folder: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    collapsed: bool,
    /// Folder the track is in (schema 9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct FrozenDto {
    source: SampleDto,
    start: i64,
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
    /// Linked automation clips share a nonzero link (schema 9).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    link: u32,
    content: ClipContentDto,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_zero_usize(v: &usize) -> bool {
    *v == 0
}

/// One arrangement (schema 9). The current one has no content: it is the file's top-level
/// tempo, signatures and playlist.
#[derive(Serialize, Deserialize)]
struct ArrangementDto {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content: Option<ArrangementContentDto>,
}

#[derive(Serialize, Deserialize)]
struct ArrangementContentDto {
    tempo: Vec<TempoDto>,
    signatures: Vec<SigDto>,
    playlist: PlaylistDto,
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
        /// Where a recording lost input: `[seconds into the file, seconds of silence]`
        /// (schema 3).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        gaps: Vec<(f64, f64)>,
        /// Inactive takes of a loop recording (schema 3).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        takes: Vec<TakeDto>,
        /// Fades as `[ticks, curve]` (schema 4).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fade_in: Option<(i64, String)>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fade_out: Option<(i64, String)>,
        /// Plays backwards (schema 4).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        reverse: bool,
        /// Speed and pitch (schema 4, optional).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stretch: Option<Box<StretchDto>>,
    },
    Automation {
        target: String,
        points: Vec<PointDto>,
    },
}

#[derive(Serialize, Deserialize)]
struct StretchDto {
    mode: String,
    #[serde(default = "one")]
    ratio: f64,
    #[serde(default)]
    semitones: f32,
    #[serde(default)]
    bpm: f64,
    /// Warp markers as (source seconds, beat).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warps: Vec<(f64, f64)>,
}

fn one() -> f64 {
    1.0
}

fn stretch_dto(s: &ClipStretch) -> Option<StretchDto> {
    (*s != ClipStretch::default()).then(|| StretchDto {
        mode: s.mode.key().to_owned(),
        ratio: s.ratio,
        semitones: s.semitones,
        bpm: s.bpm,
        warps: s.warps.iter().map(|w| (w.source_s, w.beat)).collect(),
    })
}

fn fade_dto(f: Fade) -> Option<(i64, String)> {
    (f.len > 0).then(|| (f.len, f.curve.key().to_owned()))
}

#[derive(Serialize, Deserialize)]
struct TakeDto {
    source: SampleDto,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    gaps: Vec<(f64, f64)>,
    length: i64,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    action: Option<String>,
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
    Midi {
        control: String,
    },
    /// Schema 12.
    Formula {
        text: String,
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

/// The Sampler 2 part of a sampler, or none when it is all default (older files read the
/// same).
fn sampler2_dto(
    s: &SamplerSettings,
    dir: Option<&Path>,
    embedded: &HashMap<PathBuf, String>,
) -> Option<Sampler2Dto> {
    let d = SamplerSettings::default();
    let plain = s.interp == d.interp
        && s.reverse == d.reverse
        && s.filter == d.filter
        && s.lfos == d.lfos
        && s.root_key == d.root_key
        && s.key_lo == d.key_lo
        && s.key_hi == d.key_hi
        && s.zones.is_empty()
        && s.round_robin == d.round_robin
        && s.mono == d.mono
        && s.legato == d.legato
        && s.glide_ms == d.glide_ms
        && s.voices == d.voices
        && s.stretch_beats == d.stretch_beats
        && s.slice_beats == d.slice_beats;
    (!plain).then(|| Sampler2Dto {
        interp: s.interp.key().to_owned(),
        reverse: s.reverse,
        filter: s.filter.mode.name().to_owned(),
        cutoff: s.filter.cutoff,
        resonance: s.filter.resonance,
        filter_env_amount: s.filter.env_amount,
        filter_env: EnvDto::of(&s.filter.env),
        lfos: s
            .lfos
            .iter()
            .map(|l| LfoDto {
                shape: l.shape.key().to_owned(),
                rate: l.rate,
                target: l.target.name().to_owned(),
                amount: l.amount,
            })
            .collect(),
        root_key: s.root_key,
        key_lo: s.key_lo,
        key_hi: s.key_hi,
        zones: s
            .zones
            .iter()
            .map(|z| ZoneDto {
                sample: sample_dto(&z.sample, dir, embedded),
                root: z.root,
                key_lo: z.key_lo,
                key_hi: z.key_hi,
                vel_lo: z.vel_lo,
                vel_hi: z.vel_hi,
                tune: z.tune,
                gain: z.gain,
                start: z.start,
                end: z.end,
            })
            .collect(),
        round_robin: s.round_robin,
        mono: s.mono,
        legato: s.legato,
        glide_ms: s.glide_ms,
        voices: s.voices,
        stretch_beats: s.stretch_beats,
        slice_beats: s.slice_beats,
    })
}

fn playlist_dto(
    pl: &Playlist,
    dir: Option<&Path>,
    embedded: &HashMap<PathBuf, String>,
) -> PlaylistDto {
    PlaylistDto {
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
                folder: t.folder,
                collapsed: t.collapsed,
                parent: t.parent.map(|p| p.0),
                input: t.input.map(AudioInput::key),
                frozen: t.frozen.as_ref().map(|f| FrozenDto {
                    source: sample_dto(&f.source, dir, embedded),
                    start: f.start,
                }),
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
                link: c.link,
                content: match &c.kind {
                    ClipKind::Pattern(id) => ClipContentDto::Pattern { pattern: id.0 },
                    ClipKind::Audio {
                        source,
                        gain,
                        gaps,
                        takes,
                        edit,
                    } => ClipContentDto::Audio {
                        source: sample_dto(source, dir, embedded),
                        gain: *gain,
                        gaps: gaps.iter().map(|g| (g.at_s, g.len_s)).collect(),
                        takes: takes
                            .iter()
                            .map(|t| TakeDto {
                                source: sample_dto(&t.source, dir, embedded),
                                gaps: t.gaps.iter().map(|g| (g.at_s, g.len_s)).collect(),
                                length: t.length,
                            })
                            .collect(),
                        fade_in: fade_dto(edit.fade_in),
                        fade_out: fade_dto(edit.fade_out),
                        reverse: edit.reverse,
                        stretch: stretch_dto(&edit.stretch).map(Box::new),
                    },
                    ClipKind::Automation(a) => ClipContentDto::Automation {
                        target: a.target.key(),
                        points: points_dto(&a.points),
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
                action: m.action.key(),
            })
            .collect(),
    }
}

fn tempo_dto(tempo: &TempoMap) -> Vec<TempoDto> {
    tempo
        .points()
        .map(|t| TempoDto {
            tick: t.at.0,
            bpm: t.bpm,
            ramp: (t.ramp != Curve::Hold).then(|| curve_key(t.ramp).to_owned()),
            tension: match t.ramp {
                Curve::Bezier(x) => Some(x),
                _ => None,
            },
        })
        .collect()
}

fn sigs_dto(sigs: &TimeSigMap) -> Vec<SigDto> {
    sigs.changes()
        .map(|c| SigDto {
            bar: c.bar,
            num: c.sig.num,
            den: c.sig.den,
        })
        .collect()
}

fn tempo_from(d: &[TempoDto]) -> TempoMap {
    let tempo: Vec<TempoPoint> = d
        .iter()
        .map(|t| TempoPoint {
            at: Tick(t.tick),
            bpm: t.bpm,
            ramp: t
                .ramp
                .as_deref()
                .map_or(Curve::Hold, |k| curve_from_key(k, t.tension)),
        })
        .collect();
    if tempo.is_empty() {
        TempoMap::default()
    } else {
        TempoMap::from_points_lossy(&tempo)
    }
}

fn sigs_from(d: &[SigDto]) -> TimeSigMap {
    let sigs: Vec<SigChange> = d
        .iter()
        .map(|s| SigChange {
            bar: s.bar,
            sig: TimeSig::new(s.num, s.den),
        })
        .collect();
    TimeSigMap::new(&sigs)
}

/// The arrangement list, left out of files with one arrangement of the default name.
fn arrangements_dto(
    p: &Project,
    dir: Option<&Path>,
    embedded: &HashMap<PathBuf, String>,
) -> Vec<ArrangementDto> {
    if p.arrangements.len() == 1 && p.arrangements[0].name == "Arrangement 1" {
        return Vec::new();
    }
    p.arrangements
        .iter()
        .enumerate()
        .map(|(i, a)| ArrangementDto {
            name: a.name.clone(),
            content: (i != p.current_arrangement).then(|| ArrangementContentDto {
                tempo: tempo_dto(&a.tempo),
                signatures: sigs_dto(&a.signatures),
                playlist: playlist_dto(&a.playlist, dir, embedded),
            }),
        })
        .collect()
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
            bend_range: c.bend_range,
            swing: c.swing,
            choke: c.choke,
            group: c.group.clone(),
            keys: (c.keys != gt_core::KeyLimits::default()).then_some(KeysDto {
                root: c.keys.root,
                lo: c.keys.lo,
                hi: c.keys.hi,
                mono: c.keys.mono,
                voices: c.keys.voices,
            }),
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
                    sampler2: sampler2_dto(s, dir, embedded),
                },
                Instrument::Plugin(plugin) => InstrumentDto::Plugin {
                    plugin: plugin_dto(plugin),
                },
                Instrument::Patcher(patch) => InstrumentDto::Patcher {
                    patch: patch_dto(patch),
                },
                Instrument::Drums(kit) => InstrumentDto::Drums {
                    banks: kit
                        .banks
                        .iter()
                        .map(|bank| {
                            bank.iter()
                                .map(|p| PadDto {
                                    name: p.name.clone(),
                                    layers: p
                                        .layers
                                        .iter()
                                        .map(|l| PadLayerDto {
                                            sample: sample_dto(&l.sample, dir, embedded),
                                            vel_lo: l.vel_lo,
                                            vel_hi: l.vel_hi,
                                        })
                                        .collect(),
                                    tune: p.tune,
                                    gain: p.gain,
                                    pan: p.pan,
                                    env: EnvDto::of(&p.adsr),
                                    choke: p.choke,
                                    output: p.output,
                                    synth: p.synth.as_ref().map(DrumVoiceDto::of),
                                })
                                .collect()
                        })
                        .collect(),
                },
                Instrument::Layer(l) => InstrumentDto::Layer {
                    children: l.children.iter().map(|c| c.0).collect(),
                    mode: match l.mode {
                        gt_core::LayerMode::All => "all",
                        gt_core::LayerMode::Random => "random",
                        gt_core::LayerMode::RoundRobin => "round-robin",
                    }
                    .to_owned(),
                    split: l.split,
                },
                Instrument::MidiOut(m) => InstrumentDto::MidiOut {
                    port: m.port.clone(),
                    channel: m.channel,
                    program: m.program,
                    bank: m.bank,
                    latency_ms: m.latency_ms,
                },
                Instrument::Player(pl) => InstrumentDto::Player {
                    pack: dir.and_then(|d| relative_path(d, &pl.pack)),
                    pack_absolute: Some(pl.pack.to_string_lossy().into_owned()),
                    preset: pl.preset.clone(),
                    macros: pl
                        .macros
                        .iter()
                        .map(|m| MacroDto {
                            name: m.name.clone(),
                            target: m.target.key().to_owned(),
                            min: m.min,
                            max: m.max,
                            value: m.value,
                        })
                        .collect(),
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
                    tables: if patch.tables.iter().all(Option::is_none) {
                        Vec::new()
                    } else {
                        patch
                            .tables
                            .iter()
                            .map(|t| t.as_ref().map(|t| sample_dto(t, dir, embedded)))
                            .collect()
                    },
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
            color: pat.color,
            notes: pat
                .notes
                .iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(ch, v)| ChannelNotesDto {
                    channel: ch.0,
                    notes: v.iter().map(NoteDto::of).collect(),
                })
                .collect(),
            controls: pat
                .controls
                .iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(ch, v)| ChannelControlsDto {
                    channel: ch.0,
                    points: v.iter().map(|c| (c.tick, c.kind.code(), c.value)).collect(),
                })
                .collect(),
            automation: pat
                .automation
                .iter()
                .map(|a| PatternAutoDto {
                    target: a.target.key(),
                    points: points_dto(&a.points),
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
            sends: s
                .sends
                .iter()
                .map(|x| SendDto {
                    to: x.to,
                    level: x.level,
                    pre: x.pre,
                })
                .collect(),
            sidechain: s.sidechain,
            input: s.input.map(AudioInput::key),
            slots: slots_dto(&s.slots, dir),
            color: s.color,
            icon: s.icon,
        })
        .collect();
    let playlist = playlist_dto(&p.playlist, dir, embedded);
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
                ModSourceKind::Midi { control } => ModSourceDto::Midi {
                    control: control.key().to_owned(),
                },
                ModSourceKind::Formula => ModSourceDto::Formula {
                    text: p.formula(m.id).to_owned(),
                },
            },
        })
        .collect();
    ProjectDto {
        next_id: p.id_counter(),
        current_pattern: p.current_pattern.0,
        swing: p.swing,
        tempo: tempo_dto(&p.tempo),
        signatures: sigs_dto(&p.signatures),
        channels,
        patterns,
        mixer,
        pan_law: (p.mixer.pan_law != PanLaw::Zero).then(|| p.mixer.pan_law.key().to_owned()),
        playlist,
        arrangements: arrangements_dto(p, dir, embedded),
        current_arrangement: p.current_arrangement,
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
        midi_routes: p
            .midi_routes
            .iter()
            .map(|r| MidiRouteDto {
                port: r.port.clone(),
                channel: r.channel.map(|c| c + 1),
                target: r.target.0,
            })
            .collect(),
        info: InfoDto {
            title: p.info.title.clone(),
            artist: p.info.artist.clone(),
            genre: p.info.genre.clone(),
            notes: p.info.notes.clone(),
        },
    }
}

fn playlist_from_dto(
    d: PlaylistDto,
    sample: &mut impl FnMut(SampleDto, &mut Vec<String>) -> Option<SampleSource>,
    warnings: &mut Vec<String>,
) -> Playlist {
    let mut pl = Playlist::empty();
    let pl = &mut pl;
    for t in d.tracks {
        pl.tracks.push(Track {
            id: TrackId(t.id),
            name: t.name,
            color: t.color,
            mute: t.mute,
            solo: t.solo,
            insert: t.insert,
            input: t.input.as_deref().and_then(AudioInput::from_key),
            folder: t.folder,
            collapsed: t.collapsed,
            parent: t.parent.map(TrackId),
            frozen: t.frozen.and_then(|f| {
                Some(Frozen {
                    source: sample(f.source, warnings)?,
                    start: f.start,
                })
            }),
        });
    }
    for c in d.clips {
        let kind = match c.content {
            ClipContentDto::Pattern { pattern } => Some(ClipKind::Pattern(PatternId(pattern))),
            ClipContentDto::Audio {
                source,
                gain,
                gaps,
                takes,
                fade_in,
                fade_out,
                reverse,
                stretch,
            } => {
                let fade = |f: Option<(i64, String)>| {
                    f.map_or_else(Fade::default, |(len, curve)| Fade {
                        len,
                        curve: FadeCurve::from_key(&curve),
                    })
                };
                let edit = AudioEdit {
                    fade_in: fade(fade_in),
                    fade_out: fade(fade_out),
                    reverse,
                    stretch: stretch.map_or_else(ClipStretch::default, |s| ClipStretch {
                        mode: StretchMode::from_key(&s.mode),
                        ratio: s.ratio,
                        semitones: s.semitones,
                        bpm: s.bpm,
                        warps: s
                            .warps
                            .iter()
                            .map(|&(source_s, beat)| WarpMarker { source_s, beat })
                            .collect(),
                    }),
                };
                let to_gaps = |g: Vec<(f64, f64)>| {
                    g.into_iter()
                        .map(|(at_s, len_s)| Gap { at_s, len_s })
                        .collect::<Vec<_>>()
                };
                let takes = takes
                    .into_iter()
                    .filter_map(|t| {
                        let source = sample(t.source, warnings)?;
                        Some(AudioTake {
                            source,
                            gaps: to_gaps(t.gaps),
                            length: t.length,
                        })
                    })
                    .collect();
                sample(source, warnings).map(|source| ClipKind::Audio {
                    source,
                    gain,
                    gaps: to_gaps(gaps),
                    takes,
                    edit,
                })
            }
            ClipContentDto::Automation { target, points } => match ParamId::parse(&target) {
                Some(target) => Some(ClipKind::Automation(Automation {
                    target,
                    points: points_from_dto(&points),
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
                link: c.link,
                kind,
            });
        }
    }
    for m in d.markers {
        pl.markers.push(Marker {
            at: m.at,
            name: m.name,
            action: m
                .action
                .as_deref()
                .map_or(MarkerAction::None, MarkerAction::from_key),
        });
    }
    pl.set_id_counter(d.next_id);
    std::mem::take(pl)
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
    /// Plugins whose state is in a zip entry.
    plugin_states: Vec<(PluginInstanceId, String)>,
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

    let mut plugin_states = Vec::new();
    let mut plugin = |dto: PluginDto, warnings: &mut Vec<String>| -> Option<PluginRef> {
        let (p, state) = plugin_from_dto(dto, warnings)?;
        if let Some(entry) = state {
            plugin_states.push((p.instance, entry));
        }
        Some(p)
    };
    let d = file.project;
    let mut p = Project::empty();
    p.patterns.clear();
    p.swing = d.swing;
    p.tempo = tempo_from(&d.tempo);
    p.signatures = sigs_from(&d.signatures);

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
                sampler2,
            } => {
                let mut s = SamplerSettings::default();
                if let Some(x) = sampler2 {
                    s.interp = Interp::from_key(&x.interp);
                    s.reverse = x.reverse;
                    s.filter = SamplerFilter {
                        mode: SamplerFilterMode::from_name(&x.filter),
                        cutoff: x.cutoff,
                        resonance: x.resonance,
                        env_amount: x.filter_env_amount,
                        env: x.filter_env.adsr(),
                    };
                    for (l, d) in s.lfos.iter_mut().zip(&x.lfos) {
                        *l = SamplerLfo {
                            shape: LfoShape::ALL
                                .into_iter()
                                .find(|k| k.key() == d.shape)
                                .unwrap_or_default(),
                            rate: d.rate,
                            target: LfoTarget::from_name(&d.target),
                            amount: d.amount,
                        };
                    }
                    s.root_key = x.root_key;
                    s.key_lo = x.key_lo;
                    s.key_hi = x.key_hi;
                    s.zones = x
                        .zones
                        .into_iter()
                        .filter_map(|z| {
                            Some(KeyZone {
                                sample: sample(z.sample, &mut warnings)?,
                                root: z.root,
                                key_lo: z.key_lo,
                                key_hi: z.key_hi,
                                vel_lo: z.vel_lo,
                                vel_hi: z.vel_hi,
                                tune: z.tune,
                                gain: z.gain,
                                start: z.start,
                                end: z.end,
                            })
                        })
                        .collect();
                    s.round_robin = x.round_robin;
                    s.mono = x.mono;
                    s.legato = x.legato;
                    s.glide_ms = x.glide_ms;
                    s.voices = x.voices;
                    s.stretch_beats = x.stretch_beats;
                    s.slice_beats = x.slice_beats;
                }
                Instrument::Sampler(SamplerSettings {
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
                    ..s
                })
            }
            InstrumentDto::Synth {
                preset_name,
                params,
                mods,
                tables,
            } => {
                let mut patch = SynthPatch {
                    name: preset_name,
                    ..SynthPatch::default()
                };
                for (t, d) in patch.tables.iter_mut().zip(tables) {
                    *t = d.and_then(|d| sample(d, &mut warnings));
                }
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
            InstrumentDto::Plugin { plugin: dto } => match plugin(dto, &mut warnings) {
                Some(pl) => Instrument::Plugin(Box::new(pl)),
                None => continue,
            },
            InstrumentDto::Patcher { patch } => {
                Instrument::Patcher(Box::new(patch_from_dto(patch, &mut warnings, &mut plugin)))
            }
            InstrumentDto::Drums { banks } => {
                let banks = banks
                    .into_iter()
                    .map(|bank| {
                        bank.into_iter()
                            .map(|d| Pad {
                                name: d.name,
                                layers: d
                                    .layers
                                    .into_iter()
                                    .filter_map(|l| {
                                        Some(PadLayer {
                                            sample: sample(l.sample, &mut warnings)?,
                                            vel_lo: l.vel_lo,
                                            vel_hi: l.vel_hi,
                                        })
                                    })
                                    .collect(),
                                tune: d.tune,
                                gain: d.gain,
                                pan: d.pan,
                                adsr: d.env.adsr(),
                                choke: d.choke,
                                output: d.output,
                                synth: d.synth.as_ref().map(DrumVoiceDto::voice),
                            })
                            .collect()
                    })
                    .collect();
                Instrument::Drums(Box::new(DrumKit { banks }))
            }
            InstrumentDto::Layer {
                children,
                mode,
                split,
            } => Instrument::Layer(gt_core::LayerSettings {
                children: children.into_iter().map(ChannelId).collect(),
                mode: match mode.as_str() {
                    "random" => gt_core::LayerMode::Random,
                    "round-robin" => gt_core::LayerMode::RoundRobin,
                    _ => gt_core::LayerMode::All,
                },
                split,
            }),
            InstrumentDto::MidiOut {
                port,
                channel,
                program,
                bank,
                latency_ms,
            } => Instrument::MidiOut(gt_core::MidiOutSettings {
                port,
                channel,
                program,
                bank,
                latency_ms,
            }),
            InstrumentDto::Player {
                pack,
                pack_absolute,
                preset,
                macros,
            } => {
                // The base settings come from the pack once it is found ([`resolve_packs`]);
                // the saved path is the absolute one, the relative one beside the project.
                let relative = pack.zip(dir).map(|(r, d)| d.join(r));
                let pack = pack_absolute
                    .map(PathBuf::from)
                    .filter(|p| p.is_file())
                    .or(relative)
                    .unwrap_or_default();
                let macros = macros
                    .into_iter()
                    .filter_map(|m| {
                        let target = gt_core::MacroTarget::from_key(&m.target);
                        if target.is_none() {
                            warnings.push(format!("unknown macro target \"{}\" dropped", m.target));
                        }
                        Some(gt_core::Macro {
                            name: m.name,
                            target: target?,
                            min: m.min,
                            max: m.max,
                            value: m.value,
                        })
                    })
                    .collect();
                Instrument::Player(Box::new(gt_core::Player {
                    pack,
                    preset,
                    base: SamplerSettings::default(),
                    macros,
                }))
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
            bend_range: if c.bend_range.is_finite() {
                c.bend_range.clamp(0.0, Channel::MAX_BEND)
            } else {
                Channel::DEFAULT_BEND
            },
            swing: c.swing,
            choke: c.choke,
            group: c.group,
            keys: c
                .keys
                .map_or_else(gt_core::KeyLimits::default, |k| gt_core::KeyLimits {
                    root: k.root,
                    lo: k.lo,
                    hi: k.hi,
                    mono: k.mono,
                    voices: k.voices,
                }),
        });
    }
    for pat in d.patterns {
        let mut notes = BTreeMap::new();
        for cn in pat.notes {
            let v: Vec<Note> = cn.notes.iter().map(NoteDto::note).collect();
            notes.insert(ChannelId(cn.channel), v);
        }
        // Unknown controller kinds (from a newer version) are dropped.
        let controls = pat
            .controls
            .into_iter()
            .map(|cc| {
                let v = cc
                    .points
                    .iter()
                    .filter_map(|&(tick, kind, value)| {
                        Some(gt_core::ControlPoint {
                            tick,
                            kind: gt_core::ControlKind::from_code(kind)?,
                            value,
                        })
                    })
                    .collect();
                (ChannelId(cc.channel), v)
            })
            .collect();
        let mut automation = Vec::new();
        for a in pat.automation {
            match ParamId::parse(&a.target) {
                Some(target) => automation.push(Automation {
                    target,
                    points: points_from_dto(&a.points),
                }),
                None => warnings.push(format!(
                    "pattern automation for unknown parameter \"{}\" removed",
                    a.target
                )),
            }
        }
        p.patterns.push(Pattern {
            id: PatternId(pat.id),
            name: pat.name,
            steps: pat.steps,
            color: pat.color,
            notes,
            controls,
            automation,
        });
    }
    p.current_pattern = PatternId(d.current_pattern);

    let strips = d.mixer.len().clamp(2, gt_core::STRIPS);
    p.mixer.strips = (0..strips).map(gt_core::MixerStrip::new).collect();
    p.mixer.pan_law = d
        .pan_law
        .as_deref()
        .and_then(PanLaw::from_key)
        .unwrap_or_default();
    for (strip, sd) in p.mixer.strips.iter_mut().zip(d.mixer) {
        strip.name = sd.name;
        strip.volume = sd.volume;
        strip.pan = sd.pan;
        strip.mute = sd.mute;
        strip.solo = sd.solo;
        strip.phase_invert = sd.phase_invert;
        strip.output = sd.output;
        strip.sends = sd
            .sends
            .iter()
            .take(gt_core::MAX_SENDS)
            .map(|x| gt_core::Send {
                to: x.to,
                level: x.level,
                pre: x.pre,
            })
            .collect();
        strip.sidechain = sd.sidechain;
        strip.input = sd.input.as_deref().and_then(AudioInput::from_key);
        strip.color = sd.color;
        strip.icon = sd.icon;
        strip.slots = slots_from_dto(sd.slots, dir, &mut warnings, &mut plugin);
    }

    p.playlist = playlist_from_dto(d.playlist, &mut sample, &mut warnings);
    if !d.arrangements.is_empty() {
        p.arrangements = d
            .arrangements
            .into_iter()
            .map(|a| match a.content {
                Some(c) => gt_core::Arrangement {
                    name: a.name,
                    playlist: playlist_from_dto(c.playlist, &mut sample, &mut warnings),
                    tempo: tempo_from(&c.tempo),
                    signatures: sigs_from(&c.signatures),
                },
                None => gt_core::Arrangement::named(&a.name),
            })
            .collect();
        p.current_arrangement = d.current_arrangement;
    }

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
            ModSourceDto::Midi { control } => ModSourceKind::Midi {
                control: MidiControl::from_key(&control),
            },
            ModSourceDto::Formula { text } => {
                p.formulas.insert(ModulatorId(m.id), text);
                ModSourceKind::Formula
            }
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
    for r in d.midi_routes {
        let target = ChannelId(r.target);
        if p.channel_index(target).is_some() && r.channel.is_none_or(|c| (1..=16).contains(&c)) {
            p.midi_routes.push(gt_core::MidiRoute {
                port: r.port,
                channel: r.channel.map(|c| c - 1),
                target,
            });
        }
    }
    p.info = gt_core::ProjectInfo {
        title: d.info.title,
        artist: d.info.artist,
        genre: d.info.genre,
        notes: d.info.notes,
    };
    p.set_id_counter(d.next_id);
    p.sanitize();
    Ok(Parsed {
        project: p,
        refs,
        plugin_states,
        missing: Vec::new(),
        extracted: 0,
        warnings,
        schema_version: version,
    })
}

/// Zip entry name of a plugin state (named by content, so identical states are stored once).
fn state_entry(state: &[u8]) -> String {
    format!("{PLUGINS_DIR}{:016x}.bin", fnv1a(state))
}

/// Effect slots for a file; `dir` is the folder the file sits in (for relative paths).
pub(crate) fn slots_dto(
    slots: &[Option<EffectSlot>; FX_SLOTS],
    dir: Option<&Path>,
) -> Vec<Option<EffectDto>> {
    slots
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
                plugin: e.plugin.as_deref().map(plugin_dto),
                mix: e.mix,
                file: e
                    .file
                    .as_deref()
                    .and_then(|f| dir.and_then(|d| relative_path(d, f))),
                file_absolute: e.file.as_ref().map(|f| f.to_string_lossy().into_owned()),
                patch: e.patch.as_deref().map(patch_dto),
            })
        })
        .collect()
}

/// Effect slots read from a file; unknown effects are dropped with a warning.
pub(crate) fn slots_from_dto(
    slots: Vec<Option<EffectDto>>,
    dir: Option<&Path>,
    warnings: &mut Vec<String>,
    plugin: &mut impl FnMut(PluginDto, &mut Vec<String>) -> Option<PluginRef>,
) -> [Option<EffectSlot>; FX_SLOTS] {
    let mut out: [Option<EffectSlot>; FX_SLOTS] = Default::default();
    for (slot, ed) in out.iter_mut().zip(slots.into_iter().take(FX_SLOTS)) {
        *slot = ed.and_then(|ed| {
            let Some(kind) = EffectKind::from_key(&ed.kind) else {
                warnings.push(format!("unknown effect \"{}\" removed", ed.kind));
                return None;
            };
            if kind == EffectKind::Plugin {
                let mut e = EffectSlot::plugin(plugin(ed.plugin?, warnings)?);
                e.enabled = ed.enabled;
                e.mix = ed.mix;
                e.sanitize();
                return Some(e);
            }
            let mut e = EffectSlot::new(kind);
            e.enabled = ed.enabled;
            e.mix = ed.mix;
            if let Some(patch) = ed.patch {
                e.patch = Some(Box::new(patch_from_dto(patch, warnings, plugin)));
            }
            // The relative path wins when the file is there (the folder moved).
            let relative = ed
                .file
                .filter(|f| !f.is_empty())
                .and_then(|f| dir.map(|d| d.join(from_slashes(&f))));
            let absolute = ed
                .file_absolute
                .filter(|f| !f.is_empty())
                .map(PathBuf::from);
            e.file = match (relative, absolute) {
                (Some(r), _) if r.exists() => Some(r),
                (r, a) => a.or(r),
            };
            e.sanitize();
            for (key, v) in &ed.params {
                e = e.with(key, *v);
            }
            Some(e)
        });
    }
    out
}

/// A patch in a file (Prompt 34).
#[derive(Serialize, Deserialize)]
pub(crate) struct PatchDto {
    nodes: Vec<PatchNodeDto>,
    wires: Vec<PatchWireDto>,
}

#[derive(Serialize, Deserialize)]
struct PatchNodeDto {
    id: u16,
    kind: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    params: BTreeMap<String, f32>,
    #[serde(default)]
    pos: [f32; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    synth: Option<PatchSynthDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plugin: Option<PluginDto>,
}

/// A synth module's sound (no user wavetables).
#[derive(Serialize, Deserialize)]
struct PatchSynthDto {
    preset_name: String,
    params: BTreeMap<String, f32>,
    mods: Vec<SynthModDto>,
}

#[derive(Serialize, Deserialize)]
struct PatchWireDto {
    from: u16,
    port: u8,
    to: u16,
    /// An input port, or
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input: Option<u8>,
    /// a parameter: its key for a built-in module, `id:N` for a plugin's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    param: Option<String>,
    amount: f32,
}

fn patch_dto(p: &gt_core::Patch) -> PatchDto {
    use gt_core::patcher::WireEnd;
    let param_key = |to: gt_core::NodeId, i: u16| -> Option<String> {
        let n = p.node(to)?;
        match &n.plugin {
            Some(pl) => pl
                .params
                .get(usize::from(i))
                .map(|q| format!("id:{}", q.id)),
            None => n
                .kind
                .params()
                .get(usize::from(i))
                .map(|q| q.key.to_owned()),
        }
    };
    PatchDto {
        nodes: p
            .nodes
            .iter()
            .map(|n| PatchNodeDto {
                id: n.id.0,
                kind: n.kind.key(),
                params: n
                    .kind
                    .params()
                    .iter()
                    .zip(&n.params)
                    .map(|(q, &v)| (q.key.to_owned(), v))
                    .collect(),
                pos: n.pos,
                synth: n.synth.as_deref().map(|s| PatchSynthDto {
                    preset_name: s.name.clone(),
                    params: SynthParam::ALL
                        .iter()
                        .map(|&q| (q.info().key.to_owned(), s.get(q)))
                        .collect(),
                    mods: s
                        .mods
                        .iter()
                        .map(|m| SynthModDto {
                            source: m.source.key().to_owned(),
                            dest: m.dest.key().to_owned(),
                            amount: m.amount,
                        })
                        .collect(),
                }),
                plugin: n.plugin.as_deref().map(plugin_dto),
            })
            .collect(),
        wires: p
            .wires
            .iter()
            .filter_map(|w| {
                let (input, param) = match w.end {
                    WireEnd::Input(i) => (Some(i), None),
                    WireEnd::Param(i) => (None, Some(param_key(w.to, i)?)),
                };
                Some(PatchWireDto {
                    from: w.from.0,
                    port: w.port,
                    to: w.to.0,
                    input,
                    param,
                    amount: w.amount,
                })
            })
            .collect(),
    }
}

fn patch_from_dto(
    d: PatchDto,
    warnings: &mut Vec<String>,
    plugin: &mut impl FnMut(PluginDto, &mut Vec<String>) -> Option<PluginRef>,
) -> gt_core::Patch {
    use gt_core::patcher::{ModuleKind, Node, NodeId, Patch, Wire, WireEnd};
    let mut p = Patch {
        nodes: Vec::new(),
        wires: Vec::new(),
    };
    for nd in d.nodes {
        let Some(kind) = ModuleKind::from_key(&nd.kind) else {
            warnings.push(format!("unknown patch module \"{}\" removed", nd.kind));
            continue;
        };
        let mut n = Node::new(NodeId(nd.id), kind, nd.pos);
        for (v, q) in n.params.iter_mut().zip(kind.params()) {
            if let Some(&x) = nd.params.get(q.key) {
                *v = x;
            }
        }
        if let (Some(sd), Some(s)) = (nd.synth, n.synth.as_deref_mut()) {
            s.name = sd.preset_name;
            for (key, v) in &sd.params {
                if let Some(q) = SynthParam::from_key(key) {
                    s.set(q, *v);
                }
            }
            for (slot, m) in s.mods.iter_mut().zip(sd.mods.iter().take(MOD_SLOTS)) {
                *slot = ModSlot {
                    source: ModSource::from_key(&m.source).unwrap_or(ModSource::Off),
                    dest: ModDest::from_key(&m.dest).unwrap_or(ModDest::Off),
                    amount: m.amount,
                };
            }
        }
        if kind == ModuleKind::Plugin {
            match nd.plugin.and_then(|dto| plugin(dto, warnings)) {
                Some(pl) => n.plugin = Some(Box::new(pl)),
                None => continue,
            }
        }
        p.nodes.push(n);
    }
    for w in d.wires {
        let to = NodeId(w.to);
        let end = match (w.input, w.param) {
            (Some(i), _) => WireEnd::Input(i),
            (None, Some(key)) => {
                let Some(n) = p.node(to) else { continue };
                let index = match (&n.plugin, key.strip_prefix("id:")) {
                    (Some(pl), Some(id)) => {
                        let id: u32 = id.parse().unwrap_or(u32::MAX);
                        pl.params.iter().position(|q| q.id == id)
                    }
                    (None, None) => n.kind.params().iter().position(|q| q.key == key),
                    _ => None,
                };
                match index {
                    Some(i) => WireEnd::Param(i as u16),
                    None => continue,
                }
            }
            (None, None) => continue,
        };
        p.wires.push(Wire {
            from: NodeId(w.from),
            port: w.port,
            to,
            end,
            amount: w.amount,
        });
    }
    p.sanitize();
    p
}

fn plugin_dto(p: &PluginRef) -> PluginDto {
    PluginDto {
        format: p.format.key().to_owned(),
        id: p.id.clone(),
        name: p.name.clone(),
        vendor: p.vendor.clone(),
        kind: match p.kind {
            PluginKind::Instrument => "instrument",
            PluginKind::Effect => "effect",
        }
        .to_owned(),
        path: p.path.to_string_lossy().into_owned(),
        state: (!p.state.is_empty()).then(|| state_entry(&p.state)),
        params: p
            .params
            .iter()
            .zip(&p.values)
            .map(|(q, &value)| PluginParamDto {
                id: q.id,
                name: q.name.clone(),
                default: q.default,
                steps: q.steps,
                automatable: q.automatable,
                hidden: q.hidden,
                value,
            })
            .collect(),
        outputs: p
            .outputs
            .iter()
            .map(|o| PluginOutputDto {
                name: o.name.clone(),
                route: o.route,
            })
            .collect(),
        sends_midi: p.sends_midi,
        midi_out: p.midi_out.map(|c| c.0),
    }
}

/// A plugin entry and the zip entry of its state, if it has one.
pub(crate) fn plugin_from_dto(
    d: PluginDto,
    warnings: &mut Vec<String>,
) -> Option<(PluginRef, Option<String>)> {
    let Some(format) = PluginFormat::from_key(&d.format) else {
        warnings.push(format!(
            "plugin {} in the unsupported format \"{}\" removed",
            d.name, d.format
        ));
        return None;
    };
    let kind = if d.kind == "instrument" {
        PluginKind::Instrument
    } else {
        PluginKind::Effect
    };
    let mut p = PluginRef::new(&d.id, &d.name, &d.vendor, kind, PathBuf::from(&d.path));
    p.format = format;
    p.values = d.params.iter().map(|q| q.value).collect();
    p.params = d
        .params
        .into_iter()
        .map(|q| PluginParamInfo {
            id: q.id,
            name: q.name,
            default: q.default,
            steps: q.steps,
            automatable: q.automatable,
            hidden: q.hidden,
        })
        .collect::<Vec<_>>()
        .into();
    p.outputs = d
        .outputs
        .into_iter()
        .take(gt_core::MAX_EXTRA_OUTPUTS)
        .map(|o| gt_core::PluginOutput {
            name: o.name,
            route: o.route.filter(|&r| usize::from(r) < gt_core::STRIPS),
        })
        .collect();
    p.sends_midi = d.sends_midi;
    p.midi_out = d.midi_out.map(ChannelId);
    p.sanitize();
    Some((p, d.state))
}

fn builtin_key(b: BuiltInSample) -> &'static str {
    match b {
        BuiltInSample::Kick => "kick",
        BuiltInSample::Snare => "snare",
        BuiltInSample::Hat => "hat",
        BuiltInSample::Clap => "clap",
    }
}

fn points_dto(points: &[AutoPoint]) -> Vec<PointDto> {
    points
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
        .collect()
}

fn points_from_dto(points: &[PointDto]) -> Vec<AutoPoint> {
    points
        .iter()
        .map(|q| AutoPoint {
            at: q.at,
            value: q.value,
            curve: curve_from_key(&q.curve, q.tension),
        })
        .collect()
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
        p.channels[0].bend_range = 12.0;
        // Prompt 29: more inserts, a pre-fader send, pan law, strip colour and icon.
        p.add_insert();
        p.mixer.add_send(2, 67, 0.4, true);
        p.mixer.pan_law = gt_core::PanLaw::Minus4_5;
        p.mixer.strips[3].color = Some([10, 20, 30]);
        p.mixer.strips[3].icon = Some(2);
        let ch0 = p.channels[0].id;
        p.current_pattern_mut().controls.insert(
            ch0,
            vec![
                gt_core::ControlPoint {
                    tick: 0,
                    kind: gt_core::ControlKind::Sustain,
                    value: 1.0,
                },
                gt_core::ControlPoint {
                    tick: 480,
                    kind: gt_core::ControlKind::Bend,
                    value: -0.5,
                },
            ],
        );
        p.current_pattern_mut().automation.push(Automation {
            target: pan,
            points: vec![
                AutoPoint::new(0, 0.2),
                AutoPoint {
                    at: 960,
                    value: 0.8,
                    curve: Curve::Smooth,
                },
                AutoPoint::new(3840, 0.5),
            ],
        });
        p.midi_routes = vec![
            gt_core::MidiRoute {
                port: "Keys".into(),
                channel: None,
                target: p.channels[1].id,
            },
            gt_core::MidiRoute {
                port: "Pads".into(),
                channel: Some(9),
                target: p.channels[0].id,
            },
        ];
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
    fn formulas_modulator_targets_and_note_curves_round_trip() {
        let mut p = Project::demo();
        let lfo = p.modulators[0].id;
        let f = p
            .add_modulator(
                ParamId::Modulator {
                    id: lfo,
                    field: gt_core::ModField::Rate,
                },
                ModSourceKind::Formula,
            )
            .unwrap();
        p.formulas.insert(f, "tri(beat / 4) * wheel".to_owned());
        let ch = p.channels[0].id;
        let pat = p.current_pattern_mut();
        let n = &mut pat.notes.get_mut(&ch).unwrap()[0];
        n.curve = NoteCurve::new(
            CurveKind::Pressure,
            &[
                CurvePoint {
                    at: 0.0,
                    value: 0.0,
                },
                CurvePoint {
                    at: 0.5,
                    value: 1.0,
                },
            ],
        );
        p.sanitize();
        let v = to_value(&p, None, &HashMap::new());
        let text = v.to_string();
        assert!(
            text.contains(&format!("modulator/{}/rate", lfo.0)),
            "{text}"
        );
        let back = from_value(v, None).unwrap();
        assert_eq!(back.project.formulas, p.formulas);
        assert_eq!(back.project.modulators, p.modulators);
        assert_eq!(back.project, p);
    }

    #[test]
    fn patcher_channels_and_effects_round_trip() {
        use gt_core::patcher::{ModuleKind as M, WireEnd as E};
        let mut p = Project::demo();
        let ch = p.add_patcher_channel("Patch").unwrap();
        let i = p.channel_index(ch).unwrap();
        let patch = p.channels[i].patch_mut().unwrap();
        let lfo = patch.add(M::Lfo, [10.0, 300.0]).unwrap();
        let filter = patch.nodes[5].id;
        patch.connect(lfo, 0, filter, E::Param(1), -0.25).unwrap();
        let s = patch.add(M::Synth, [400.0, 300.0]).unwrap();
        *patch.node_mut(s).unwrap().synth.as_deref_mut().unwrap() = SynthPatch::factory().remove(3);
        let mut slot = EffectSlot::new(EffectKind::Patcher);
        let fx = slot.patch.as_deref_mut().unwrap();
        let d = fx.add(M::Effect(EffectKind::Delay), [200.0, 60.0]).unwrap();
        fx.node_mut(d).unwrap().params[1] = 0.6;
        p.mixer.strips[1].slots[3] = Some(slot);
        p.sanitize();
        let v = to_value(&p, None, &HashMap::new());
        let back = from_value(v, None).unwrap();
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);
        assert_eq!(back.project.channels[i], p.channels[i]);
        assert_eq!(
            back.project.mixer.strips[1].slots[3],
            p.mixer.strips[1].slots[3]
        );
        assert_eq!(back.project, p);
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
    fn recordings_keep_their_inputs_gaps_and_travel_embedded() {
        let dir = temp_dir("rec");
        std::fs::create_dir_all(dir.join("song/recordings")).unwrap();
        let take = dir.join("song/recordings/Vox take 1.wav");
        std::fs::write(&take, b"RIFF....take").unwrap();
        let other = dir.join("song/recordings/Vox take 2.wav");
        std::fs::write(&other, b"RIFF....other").unwrap();
        let frozen = dir.join("song/recordings/Drums freeze 1.wav");
        std::fs::write(&frozen, b"RIFF....frozen").unwrap();
        let mut p = Project::empty();
        let t = p.playlist.tracks[0].id;
        p.playlist.tracks[2].frozen = Some(Frozen {
            source: SampleSource::File(frozen.clone()),
            start: 0,
        });
        p.playlist.tracks[0].input = Some(AudioInput::stereo(2));
        p.playlist.tracks[1].input = Some(AudioInput::mono(0));
        p.mixer.strips[4].input = Some(AudioInput::mono(7));
        let gaps = vec![
            Gap {
                at_s: 1.25,
                len_s: 0.01,
            },
            Gap {
                at_s: 3.5,
                len_s: 0.0,
            },
        ];
        let edit = AudioEdit {
            fade_in: Fade {
                len: 480,
                curve: FadeCurve::SCurve,
            },
            fade_out: Fade {
                len: 240,
                curve: FadeCurve::Exponential,
            },
            reverse: true,
            stretch: ClipStretch {
                mode: StretchMode::TempoFollow,
                ratio: 1.0,
                semitones: -2.5,
                bpm: 92.5,
                warps: vec![
                    WarpMarker {
                        source_s: 0.25,
                        beat: 0.5,
                    },
                    WarpMarker {
                        source_s: 1.5,
                        beat: 2.0,
                    },
                ],
            },
        };
        p.playlist.add_clip(
            t,
            1234,
            9600,
            ClipKind::Audio {
                source: SampleSource::File(take.clone()),
                gain: 1.0,
                gaps: gaps.clone(),
                takes: vec![AudioTake {
                    source: SampleSource::File(other.clone()),
                    gaps: Vec::new(),
                    length: 4800,
                }],
                edit: edit.clone(),
            },
        );
        let path = dir.join("song/song.gloom");
        let report = save(
            &p,
            &path,
            SaveOptions {
                embed_samples: true,
            },
        )
        .unwrap();
        assert_eq!(report.embedded, 3, "both takes and the freeze are embedded");
        let json = to_value(&p, Some(&dir.join("song")), &HashMap::new());
        let track = &json["project"]["playlist"]["tracks"][0];
        assert_eq!(track["input"], "3-4");
        assert_eq!(json["project"]["mixer"][4]["input"], "8");
        assert_eq!(
            json["project"]["playlist"]["clips"][0]["content"]["source"]["path"],
            "recordings/Vox take 1.wav"
        );

        // The project folder moves to another machine without the recordings folder.
        std::fs::remove_file(&take).unwrap();
        std::fs::remove_file(&other).unwrap();
        std::fs::remove_file(&frozen).unwrap();
        let loaded = load(&path, &dir.join("extracted")).unwrap();
        assert!(loaded.missing.is_empty());
        assert_eq!(loaded.extracted, 3);
        let q = &loaded.project;
        assert_eq!(q.playlist.tracks[0].input, Some(AudioInput::stereo(2)));
        assert_eq!(q.playlist.tracks[1].input, Some(AudioInput::mono(0)));
        assert_eq!(q.playlist.tracks[2].input, None);
        let Some(Frozen {
            source: SampleSource::File(f),
            start: 0,
        }) = &q.playlist.tracks[2].frozen
        else {
            panic!("the track stays frozen")
        };
        assert_eq!(std::fs::read(f).unwrap(), b"RIFF....frozen");
        assert_eq!(q.playlist.tracks[0].frozen, None);
        assert_eq!(q.mixer.strips[4].input, Some(AudioInput::mono(7)));
        let ClipKind::Audio {
            source: SampleSource::File(got),
            gaps: back,
            takes,
            edit: edit_back,
            ..
        } = &q.playlist.clips[0].kind
        else {
            panic!("an audio clip")
        };
        assert_eq!(back, &gaps);
        assert_eq!(edit_back, &edit, "fades and reverse survive");
        assert_eq!(std::fs::read(got).unwrap(), b"RIFF....take");
        let [AudioTake {
            source: SampleSource::File(got_other),
            length: 4800,
            ..
        }] = takes.as_slice()
        else {
            panic!("one inactive take: {takes:?}")
        };
        assert_eq!(std::fs::read(got_other).unwrap(), b"RIFF....other");
        assert_eq!(sample_files(q).len(), 3);

        // Relinking finds a recording like any other audio file.
        let mut q = q.clone();
        let moved = dir.join("elsewhere.wav");
        assert_eq!(relink(&mut q, got, &moved), 1);
        assert_eq!(
            relink(&mut q, got_other, &moved),
            1,
            "inactive takes relink too"
        );
        let Some(f) = q.playlist.tracks[2]
            .frozen
            .as_ref()
            .map(|f| f.source.clone())
        else {
            panic!("frozen")
        };
        let SampleSource::File(f) = f else {
            panic!("a file")
        };
        assert_eq!(relink(&mut q, &f, &moved), 1, "freezes relink too");
        assert_eq!(sample_files(&q), vec![moved]);
    }

    #[test]
    fn version_2_files_open_without_inputs() {
        let mut v = to_value(&Project::demo(), None, &HashMap::new());
        v["schema_version"] = 2.into();
        let back = from_value(v, None).unwrap();
        assert_eq!(back.schema_version, 2);
        assert_eq!(back.project, Project::demo());
        assert!(back
            .project
            .playlist
            .tracks
            .iter()
            .all(|t| t.input.is_none()));
        // Inputs a file cannot name are dropped, not invented.
        let mut v = to_value(&Project::demo(), None, &HashMap::new());
        v["project"]["playlist"]["tracks"][0]["input"] = "2-4".into();
        v["project"]["mixer"][0]["input"] = "1".into();
        let back = from_value(v, None).unwrap();
        assert_eq!(back.project.playlist.tracks[0].input, None);
        assert_eq!(
            back.project.mixer.strips[0].input, None,
            "never on the master"
        );
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
    fn new_effects_and_slot_mix_round_trip() {
        let mut p = Project::demo();
        let mut slot = EffectSlot::new(EffectKind::Filter).with("cutoff", 440.0);
        slot.mix = 0.25;
        p.mixer.strips[2].slots[0] = Some(slot.clone());
        p.mixer.strips[2].slots[1] = Some(EffectSlot::new(EffectKind::Gate));
        let v = to_value(&p, None, &HashMap::new());
        // A full mix is left out of the file.
        assert!(v["project"]["mixer"][2]["slots"][1].get("mix").is_none());
        let back = from_value(v, None).unwrap().project;
        assert_eq!(back.mixer.strips[2].slots[0], Some(slot));
        assert_eq!(back.mixer.strips[2].slots[1].as_ref().unwrap().mix, 1.0);
    }

    #[test]
    fn drum_kits_round_trip() {
        let mut p = Project::demo();
        let id = p.add_drum_channel("Kit").unwrap();
        let i = p.channel_index(id).unwrap();
        let k = p.channels[i].drums_mut().unwrap();
        k.banks
            .push(vec![Pad::default(); gt_core::pads::PADS_PER_BANK]);
        let pad = &mut k.banks[1][5];
        pad.name = "Hat".into();
        pad.set_layers(vec![
            SampleSource::File("/tmp/k/hat soft.wav".into()),
            SampleSource::File("/tmp/k/hat hard.wav".into()),
        ]);
        pad.tune = -2.0;
        pad.gain = 0.5;
        pad.pan = 0.25;
        pad.adsr.decay_ms = 120.0;
        pad.adsr.sustain = 0.0;
        pad.choke = 2;
        pad.output = Some(4);
        let want = p.channels[i].instrument.clone();
        let v = to_value(&p, None, &HashMap::new());
        let back = from_value(v, None).unwrap().project;
        assert_eq!(back.channels[i].instrument, want);
    }

    #[test]
    fn midi_out_channels_round_trip() {
        let mut p = Project::demo();
        let id = p.add_midi_out_channel("Hardware").unwrap();
        let i = p.channel_index(id).unwrap();
        let m = p.channels[i].midi_out_mut().unwrap();
        m.port = "USB MIDI 1".into();
        m.channel = 9;
        m.program = Some(17);
        m.bank = Some(300);
        m.latency_ms = 4.5;
        let want = p.channels[i].instrument.clone();
        let v = to_value(&p, None, &HashMap::new());
        let back = from_value(v, None).unwrap().project;
        assert_eq!(back.channels[i].instrument, want);
    }

    #[test]
    fn the_factory_demos_round_trip() {
        for demo in &gt_core::demos::DEMOS {
            let p = (demo.build)();
            let v = to_value(&p, None, &HashMap::new());
            let back = from_value(v, None).unwrap();
            assert!(
                back.warnings.is_empty(),
                "{}: {:?}",
                demo.name,
                back.warnings
            );
            assert_eq!(back.project, p, "{}", demo.name);
        }
    }

    #[test]
    fn drum_synth_pads_round_trip() {
        let mut p = Project::empty();
        let id = p.add_drum_synth_channel("Synth kit").unwrap();
        let i = p.channel_index(id).unwrap();
        let pad = &mut p.channels[i].drums_mut().unwrap().banks[0][2];
        let v = pad.synth.as_mut().unwrap();
        v.pitch *= 1.5;
        v.seed = 7;
        let want = p.channels[i].instrument.clone();
        let v = to_value(&p, None, &HashMap::new());
        let back = from_value(v, None).unwrap().project;
        assert_eq!(back.channels[i].instrument, want);
    }

    #[test]
    fn players_save_their_pack_preset_and_macros_and_reload_the_sound() {
        let dir = temp_dir("player");
        let pack_path = dir.join("keys.gtpack");
        crate::pack::write(
            &pack_path,
            &crate::pack::tests::manifest(),
            &crate::pack::tests::files(),
        )
        .unwrap();
        let pack = crate::pack::open(&pack_path, &dir.join("cache")).unwrap();
        let mut player = pack.player("Keys").unwrap();
        player.macros[0].value = 0.9;
        let mut p = Project::empty();
        let id = p.add_player_channel("Keys", player).unwrap();
        let i = p.channel_index(id).unwrap();
        save(&p, &dir.join("song.gloom"), SaveOptions::default()).unwrap();
        let loaded = load(&dir.join("song.gloom"), &dir.join("x")).unwrap();
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let back = loaded.project.channels[i].player().unwrap();
        let want = p.channels[i].player().unwrap();
        assert_eq!((&back.pack, &back.preset), (&want.pack, &want.preset));
        assert_eq!(back.macros, want.macros);
        assert_eq!(back.base.zones.len(), 2);
        assert!(back
            .base
            .sources()
            .all(|s| matches!(s, SampleSource::File(f) if f.is_file())));
        // A missing pack loads silent, with a warning.
        std::fs::remove_file(&pack_path).unwrap();
        let loaded = load(&dir.join("song.gloom"), &dir.join("x")).unwrap();
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.project.channels[i]
            .player()
            .unwrap()
            .base
            .zones
            .is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_info_round_trips_and_stays_out_when_empty() {
        let mut p = Project::empty();
        let v = to_value(&p, None, &HashMap::new());
        assert!(v["project"].get("info").is_none());
        p.info = gt_core::ProjectInfo {
            title: "Night Drive".into(),
            artist: "Vasko".into(),
            genre: "Darkwave".into(),
            notes: "Bridge needs a new lead.\nMaster at -14 LUFS.".into(),
        };
        let v = to_value(&p, None, &HashMap::new());
        let back = from_value(v, None).unwrap().project;
        assert_eq!(back.info, p.info);
    }

    #[test]
    fn tempo_ramps_and_marker_actions_round_trip() {
        let mut p = Project::demo();
        p.tempo = TempoMap::from_points_lossy(&[
            TempoPoint::linear(0, 90.0),
            TempoPoint {
                at: Tick(4 * 3840),
                bpm: 140.0,
                ramp: Curve::Bezier(-0.4),
            },
            TempoPoint::step(8 * 3840, 120.0),
        ]);
        let a = p.playlist.add_marker(3840, "Drop");
        let b = p.playlist.add_marker(7680, "Out");
        p.playlist.markers[a].action = MarkerAction::Loop;
        p.playlist.markers[b].action = MarkerAction::Skip(a);
        let back = from_value(to_value(&p, None, &HashMap::new()), None)
            .unwrap()
            .project;
        assert_eq!(back, p);
        assert_eq!(
            back.tempo.points().nth(1).unwrap().ramp,
            Curve::Bezier(-0.4)
        );
    }

    #[test]
    fn arrangements_folders_and_links_round_trip() {
        let mut p = Project::demo();
        let v = to_value(&p, None, &HashMap::new());
        assert!(
            v["project"].get("arrangements").is_none(),
            "one default arrangement"
        );
        let ids: Vec<TrackId> = p.playlist.tracks.iter().map(|t| t.id).collect();
        let g = p.playlist.group_tracks(&ids[1..3]).unwrap();
        p.playlist.track_mut(g).unwrap().collapsed = true;
        let auto = p
            .playlist
            .clips
            .iter()
            .find(|c| matches!(c.kind, ClipKind::Automation(_)))
            .unwrap()
            .id;
        p.playlist.duplicate_clips(&[auto], 3840);
        p.duplicate_arrangement(0).unwrap();
        p.tempo = TempoMap::constant(100.0);
        p.playlist.markers.clear();
        p.add_arrangement().unwrap();
        p.rename_arrangement(2, "Club mix");
        p.switch_arrangement(1);
        let back = from_value(to_value(&p, None, &HashMap::new()), None)
            .unwrap()
            .project;
        assert_eq!(back, p);
        assert_eq!(back.current_arrangement, 1);
        assert!(
            back.arrangement(0)
                .unwrap()
                .playlist
                .track(g)
                .unwrap()
                .collapsed
        );
        assert_eq!(back.arrangement(2).unwrap().name, "Club mix");
    }

    #[test]
    fn fm_and_wavetable_patches_round_trip() {
        use gt_core::synth::{OpField, SynthParam};
        let mut p = Project::empty();
        for mut patch in SynthPatch::factory() {
            if patch.engine() == 2 {
                patch.tables[1] = Some(SampleSource::File("/tmp/x/cycle.wav".into()));
            }
            p.add_synth_channel(&patch.name.clone(), patch);
        }
        let fm = SynthPatch::default()
            .with(SynthParam::Engine, 1.0)
            .with(SynthParam::op(4, OpField::Ratio), 3.5)
            .with(SynthParam::fm_mod(2, 5), 0.4);
        p.add_synth_channel("fm", fm);
        let want: Vec<_> = p.channels.iter().map(|c| c.instrument.clone()).collect();
        let v = to_value(&p, None, &HashMap::new());
        let back = from_value(v, None).unwrap().project;
        let got: Vec<_> = back.channels.iter().map(|c| c.instrument.clone()).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn sampler2_settings_and_zones_round_trip() {
        let mut p = Project::demo();
        let i = p
            .channels
            .iter()
            .position(|c| c.sampler().is_some())
            .unwrap();
        // A plain sampler writes no Sampler 2 block.
        let v = to_value(&p, None, &HashMap::new());
        assert!(v["project"]["channels"][i]["instrument"]
            .get("sampler2")
            .is_none());
        let s = p.channels[i].sampler_mut().unwrap();
        s.interp = Interp::Best;
        s.reverse = true;
        s.filter.mode = SamplerFilterMode::BandPass;
        s.filter.cutoff = 1234.0;
        s.filter.env_amount = -3.0;
        s.lfos[1] = SamplerLfo {
            shape: LfoShape::SawUp,
            rate: 2.5,
            target: LfoTarget::Pan,
            amount: 0.75,
        };
        s.root_key = 48;
        s.key_lo = 36;
        s.key_hi = 72;
        s.mono = true;
        s.legato = true;
        s.glide_ms = 80.0;
        s.voices = 4;
        s.round_robin = true;
        s.stretch_beats = 8.0;
        let mut z = KeyZone::new(SampleSource::File("/tmp/x/C4 pp.wav".into()), 60);
        z.vel_hi = 0.5;
        z.tune = -0.25;
        s.zones = vec![z];
        let want = s.clone();
        let v = to_value(&p, None, &HashMap::new());
        let back = from_value(v, None).unwrap().project;
        assert_eq!(back.channels[i].sampler(), Some(&want));
    }

    #[test]
    fn event_lane_data_round_trips() {
        let mut p = Project::demo();
        let ch = *p.patterns[0].notes.keys().next().unwrap();
        let target = gt_core::ParamId::strip_pan(1);
        let pat = &mut p.patterns[0];
        pat.notes.get_mut(&ch).unwrap()[0].release = 0.3;
        let controls = vec![
            gt_core::ControlPoint {
                tick: 0,
                kind: gt_core::ControlKind::Bend,
                value: 0.25,
            },
            gt_core::ControlPoint {
                tick: 240,
                kind: gt_core::ControlKind::Cc(74),
                value: 0.75,
            },
        ];
        pat.controls.insert(ch, controls.clone());
        pat.automation.push(gt_core::Automation {
            target,
            points: vec![
                gt_core::AutoPoint::new(0, 0.1),
                gt_core::AutoPoint::new(960, 0.9),
            ],
        });
        let v = to_value(&p, None, &HashMap::new());
        let back = from_value(v, None).unwrap().project;
        let pat = &back.patterns[0];
        assert_eq!(pat.controls[&ch], controls);
        assert_eq!(pat.automation, p.patterns[0].automation);
        assert_eq!(pat.notes[&ch][0].release, 0.3);
    }

    #[test]
    fn per_note_values_round_trip_and_old_notes_read_neutral() {
        let mut p = Project::demo();
        let (ch, notes) = p.patterns[0].notes.iter_mut().next().unwrap();
        let ch = *ch;
        let plain = notes[0];
        notes[1].pan = -0.5;
        notes[1].fine = 25.0;
        notes[1].mod_x = 0.9;
        notes[1].slide = true;
        notes[1].muted = true;
        notes[1].color = 3;
        let full = notes[1];
        let v = to_value(&p, None, &HashMap::new());
        let list = &v["project"]["patterns"][0]["notes"][0]["notes"];
        // A plain note stays a compact array; only set values are written.
        assert!(list[0].is_array());
        assert_eq!(list[1]["fine"], 25.0);
        assert!(list[1].get("mod_y").is_none());
        // A version 4 file (arrays only) reads with neutral values.
        let mut old = v.clone();
        old["schema_version"] = 4.into();
        old["project"]["patterns"][0]["notes"][0]["notes"][1] =
            serde_json::json!([full.start, full.length, full.key, full.velocity]);
        let back = from_value(old, None).unwrap().project;
        let n = back.patterns[0].notes[&ch][1];
        assert!(n.is_plain());
        let back = from_value(v, None).unwrap().project;
        assert_eq!(back.patterns[0].notes[&ch][0], plain);
        assert_eq!(back.patterns[0].notes[&ch][1], full);
    }

    #[test]
    fn a_convolution_response_file_follows_the_project_folder() {
        let dir = temp_dir("ir");
        std::fs::create_dir_all(dir.join("a/irs")).unwrap();
        std::fs::write(dir.join("a/irs/church.wav"), b"RIFF").unwrap();
        let mut p = Project::demo();
        let mut slot = EffectSlot::new(EffectKind::Convolution).with("ir", 4.0);
        slot.file = Some(dir.join("a/irs/church.wav"));
        p.mixer.strips[3].slots[0] = Some(slot);
        // Another kind never keeps a file.
        let mut eq = EffectSlot::new(EffectKind::Eq);
        eq.file = Some(dir.join("x.wav"));
        p.mixer.strips[3].slots[1] = Some(eq);
        let v = to_value(&p, Some(&dir.join("a")), &HashMap::new());
        assert_eq!(
            v["project"]["mixer"][3]["slots"][0]["file"],
            "irs/church.wav"
        );
        std::fs::rename(dir.join("a"), dir.join("b")).unwrap();
        let back = from_value(v, Some(&dir.join("b"))).unwrap().project;
        let s = back.mixer.strips[3].slots[0].as_ref().unwrap();
        assert_eq!(
            s.file.as_deref(),
            Some(dir.join("b/irs/church.wav").as_path())
        );
        assert_eq!(s.get("ir"), Some(4.0));
        assert!(back.mixer.strips[3].slots[1]
            .as_ref()
            .unwrap()
            .file
            .is_none());
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
    fn plugins_round_trip_with_their_state_and_parameters() {
        use gt_core::{PluginKind, PluginParamInfo, PluginRef, MASTER};
        let dir = temp_dir("plugins");
        let path = dir.join("p.gloom");
        let mut p = Project::demo();
        let mut synth = PluginRef::new(
            "com.example.synth",
            "Synth",
            "Example",
            PluginKind::Instrument,
            PathBuf::from("/usr/lib/clap/synth.clap"),
        );
        synth.params = std::sync::Arc::from(vec![
            PluginParamInfo {
                id: 7,
                name: "Cutoff".into(),
                default: 0.5,
                steps: 0,
                automatable: true,
                hidden: false,
            },
            PluginParamInfo {
                id: 9,
                name: "Mode".into(),
                default: 0.0,
                steps: 3,
                automatable: true,
                hidden: false,
            },
        ]);
        synth.values = vec![0.25, 2.0 / 3.0];
        synth.state = std::sync::Arc::from(vec![1u8, 2, 3, 250]);
        synth.set_output_names(vec!["Sub".into(), "Drums".into()]);
        synth.outputs[1].route = Some(3);
        synth.sends_midi = true;
        synth.midi_out = Some(p.channels[0].id);
        let ch = p.add_plugin_channel("Synth", synth.clone()).unwrap();
        let mut fx = PluginRef::new(
            "com.example.fx",
            "Fx",
            "",
            PluginKind::Effect,
            PathBuf::new(),
        );
        fx.state = synth.state.clone(); // the same state is stored once
        let mut slot = EffectSlot::plugin(fx);
        slot.enabled = false;
        p.mixer.strips[MASTER].slots[2] = Some(slot);

        save(&p, &path, SaveOptions::default()).unwrap();
        let loaded = load(&path, &dir.join("x")).unwrap();
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let q = &loaded.project;
        let back = q.channels[q.channel_index(ch).unwrap()].plugin().unwrap();
        assert_eq!(back.id, synth.id);
        assert_eq!(back.kind, PluginKind::Instrument);
        assert_eq!(back.path, synth.path);
        assert_eq!(back.params, synth.params);
        assert_eq!(back.values, synth.values);
        assert_eq!(back.state, synth.state);
        assert_eq!(
            back.outputs, synth.outputs,
            "extra outputs and their inserts"
        );
        assert!(back.sends_midi);
        assert_eq!(back.midi_out, synth.midi_out);
        assert_ne!(
            back.instance, synth.instance,
            "running instances are per session"
        );
        let s = q.mixer.strips[MASTER].slots[2].as_ref().unwrap();
        assert!(!s.enabled);
        assert_eq!(s.plugin.as_ref().unwrap().state, synth.state);
        let zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        assert_eq!(
            zip.file_names()
                .filter(|n| n.starts_with(PLUGINS_DIR))
                .count(),
            1
        );

        // Everything but the per-session instance ids is unchanged.
        let mut a = p.clone();
        let mut b = loaded.project.clone();
        for proj in [&mut a, &mut b] {
            for (owner, _) in proj
                .plugins()
                .into_iter()
                .map(|(o, r)| (o, r.instance))
                .collect::<Vec<_>>()
            {
                proj.plugin_mut(owner).unwrap().instance = gt_core::PluginInstanceId(0);
            }
        }
        assert_eq!(a, b);
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
