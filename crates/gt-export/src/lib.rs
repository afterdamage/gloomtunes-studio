//! Offline export: renders a project with the same engine code that plays it in real time, and
//! writes WAV files.
//!
//! The render drives a private engine instance ([`gt_engine::create`]) as fast as the CPU
//! allows: the same commands the app sends (song, channels, mixer, effects, modulation), then
//! `Play` and `AudioProcessor::process` in a loop. Because the engine works on a fixed 32-frame
//! grid anchored at the play start and seeds its noise sources at play, the result is
//! bit-identical to what you hear when playing from Stop at the same position (ARCHITECTURE.md
//! §7.5).
//!
//! Options: WAV (16-bit or 24-bit PCM with optional TPDF dither, or 32-bit float), FLAC, Ogg
//! Vorbis or MP3 ([`formats`]); any sample rate; peak normalization; the release and effect
//! tails after the end (until silence, up to a limit); the whole song or the loop region; the
//! full mix, one stem per playlist track, or one file per marker region. [`video`] turns a mix
//! into an MP4 with an external ffmpeg.
//!
//! Third-party plugins come in as processors made by the plugin host for the export (fresh
//! instances in offline mode, in their current state); each pass of a stem export gets them
//! back from the engine and resets them.
//!
//! [`smf`] reads and writes Standard MIDI Files.

#![forbid(unsafe_code)]

pub mod bounce;
pub mod formats;
pub mod packs;
pub mod smf;
pub mod video;

pub use formats::{Format, Mp3Mode, Tags, MP3_BITRATES};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use gt_core::{BuiltInSample, ClipKind, Project, SampleData, SampleSource, Tick, FX_SLOTS};
use gt_engine::{
    create, create_effect_with, AudioProcessor, ChannelParams, EngineCommand, EngineConfig,
    EngineHandle, LoopRegion, MixerParams, ModPlan, PluginBox, SongSnapshot,
};

/// Sample rates offered for export.
pub const SAMPLE_RATES: [u32; 4] = [44_100, 48_000, 88_200, 96_000];

/// Output sample format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BitDepth {
    /// 16-bit integer PCM.
    Pcm16,
    /// 24-bit integer PCM.
    #[default]
    Pcm24,
    /// 32-bit IEEE float (no dither needed, can exceed 0 dBFS).
    Float32,
}

impl BitDepth {
    /// Every format, in menu order.
    pub const ALL: [BitDepth; 3] = [Self::Pcm16, Self::Pcm24, Self::Float32];

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pcm16 => "16-bit",
            Self::Pcm24 => "24-bit",
            Self::Float32 => "32-bit float",
        }
    }
}

/// What part of the timeline to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    /// From the start to the bar line after the last clip.
    Song,
    /// From `start` to `end` (ticks), e.g. the loop region.
    Ticks {
        /// First tick.
        start: i64,
        /// First tick after the range.
        end: i64,
    },
}

/// Export options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportSettings {
    /// Output sample rate (the engine renders at this rate; samples are resampled to it).
    pub sample_rate: u32,
    /// Sample format.
    pub depth: BitDepth,
    /// TPDF dither when writing 16 or 24-bit.
    pub dither: bool,
    /// Scale so the loudest sample peaks at this level (dBFS); `None` keeps the mix level.
    pub normalize_db: Option<f32>,
    /// Part of the timeline.
    pub range: Range,
    /// Keep rendering after the end until the output is silent (at most `max_tail_seconds`).
    pub tail: bool,
    /// Longest tail.
    pub max_tail_seconds: f32,
    /// One file per playlist track (or mixer insert, see `stem_kind`) instead of the mix.
    pub stems: bool,
    /// What a stem is.
    pub stem_kind: StemKind,
    /// One file per marker region (from each marker to the next, the last to the song end)
    /// instead of the range; with `stems`, one file per track and region.
    pub regions: bool,
    /// File format.
    pub format: Format,
    /// Ogg Vorbis quality, 0 to 10.
    pub ogg_quality: f32,
    /// MP3 rate control.
    pub mp3: Mp3Mode,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            depth: BitDepth::Pcm24,
            dither: true,
            normalize_db: None,
            range: Range::Song,
            tail: true,
            max_tail_seconds: 10.0,
            stems: false,
            stem_kind: StemKind::Tracks,
            regions: false,
            format: Format::Wav,
            ogg_quality: 6.0,
            mp3: Mp3Mode::default(),
        }
    }
}

/// What one stem holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StemKind {
    /// One playlist track alone through the whole mix (stems add up to the mix).
    #[default]
    Tracks,
    /// What one mixer insert outputs, after its effects and fader, lined up with the mix
    /// (Prompt 29). Inserts that nothing plays into are skipped, and so are silent ones.
    Inserts,
}

/// Why an export failed.
#[derive(Debug)]
pub enum ExportError {
    /// Writing a file failed.
    Io(std::io::Error),
    /// The range is empty (no clips, or an empty loop).
    Empty,
    /// The user cancelled.
    Cancelled,
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Empty => write!(f, "nothing to export: the range is empty"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for ExportError {}

impl From<std::io::Error> for ExportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<hound::Error> for ExportError {
    fn from(e: hound::Error) -> Self {
        match e {
            hound::Error::IoError(e) => Self::Io(e),
            e => Self::Io(std::io::Error::other(e.to_string())),
        }
    }
}

/// Progress shared with the UI thread.
#[derive(Debug, Default)]
pub struct Progress {
    /// Done, in thousandths.
    pub permille: AtomicU32,
    /// Set to stop the export.
    pub cancel: AtomicBool,
}

impl Progress {
    /// Done, 0 to 1.
    pub fn fraction(&self) -> f32 {
        self.permille.load(Ordering::Relaxed) as f32 / 1000.0
    }
}

/// One rendered file: interleaved stereo.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    /// Stem name (track name), or empty for the mix.
    pub name: String,
    /// Interleaved left/right samples.
    pub audio: Vec<f32>,
}

/// Loads a sound at `rate`: a built-in drum is generated, a file decoded and resampled.
pub fn load_source(src: &SampleSource, rate: u32) -> Result<SampleData, String> {
    match src {
        SampleSource::BuiltIn(b) => {
            let sr = rate as f32;
            let data = match b {
                BuiltInSample::Kick => gt_dsp::drums::kick(sr),
                BuiltInSample::Snare => gt_dsp::drums::snare(sr),
                BuiltInSample::Hat => gt_dsp::drums::hat(sr),
                BuiltInSample::Clap => gt_dsp::drums::clap(sr),
            };
            Ok(SampleData::mono(rate, data))
        }
        SampleSource::File(path) => gt_project::load_sample(path, rate).map_err(|e| e.to_string()),
    }
}

/// Every sound the project uses, loaded at `rate`, and the files that failed to load.
pub fn load_samples(
    project: &Project,
    rate: u32,
) -> (HashMap<SampleSource, Arc<SampleData>>, Vec<PathBuf>) {
    let mut out = HashMap::new();
    let mut failed = Vec::new();
    let sources = project.channels.iter().flat_map(|c| c.samples()).chain(
        project.playlist.clips.iter().filter_map(|c| match &c.kind {
            ClipKind::Audio { source, .. } => Some(source),
            _ => None,
        }),
    );
    for src in sources {
        if out.contains_key(src) {
            continue;
        }
        match load_source(src, rate) {
            Ok(d) => {
                out.insert(src.clone(), Arc::new(d));
            }
            Err(_) => {
                if let SampleSource::File(p) = src {
                    failed.push(p.clone());
                }
            }
        }
    }
    failed.sort();
    failed.dedup();
    (out, failed)
}

/// The ticks a range covers in `project`, or `None` if empty.
pub fn range_ticks(project: &Project, range: Range) -> Option<(i64, i64)> {
    let (start, end) = match range {
        Range::Song => {
            let sigs = &project.signatures;
            let end = project.playlist.song_end();
            (0, sigs.bar_start(sigs.bar_of(end - 1) + 1))
        }
        Range::Ticks { start, end } => (start.max(0), end),
    };
    (end > start && project.playlist.song_end() > start).then_some((start, end))
}

/// Renders the mix, or one stem per playlist track with sound clips, as interleaved stereo.
/// `samples` holds every sound at `settings.sample_rate` (see [`load_samples`]); `plugins`
/// the project's plugins, ready for offline rendering at that rate (they are handed back).
pub fn render(
    project: &Project,
    samples: &HashMap<SampleSource, Arc<SampleData>>,
    settings: &ExportSettings,
    progress: &Progress,
    plugins: &mut Vec<Box<PluginBox>>,
) -> Result<Vec<Rendered>, ExportError> {
    let (start, end) = range_ticks(project, settings.range).ok_or(ExportError::Empty)?;
    let base = trimmed(project, end);
    type Job = (String, Project, Option<(u16, gt_engine::Tap)>);
    let jobs: Vec<Job> = match (settings.stems, settings.stem_kind) {
        (false, _) => vec![(String::new(), base, None)],
        (true, StemKind::Tracks) => stem_projects(&base)
            .into_iter()
            .map(|(n, p)| (n, p, None))
            .collect(),
        (true, StemKind::Inserts) => insert_stems(&base)
            .into_iter()
            .map(|(n, i)| (n, base.clone(), Some((i as u16, gt_engine::Tap::Output))))
            .collect(),
    };
    if jobs.is_empty() {
        return Err(ExportError::Empty);
    }
    let n = jobs.len() as u32;
    let mut out = Vec::new();
    for (k, (name, p, tap)) in jobs.into_iter().enumerate() {
        let audio = render_one(
            &p,
            samples,
            settings,
            start,
            end,
            progress,
            (k as u32, n),
            plugins,
            tap,
        )?;
        if tap.is_some() && audio.iter().all(|x| *x == 0.0) {
            continue;
        }
        out.push(Rendered { name, audio });
    }
    if out.is_empty() {
        return Err(ExportError::Empty);
    }
    if let Some(db) = settings.normalize_db {
        // One gain for every file, so stems still add up to the mix.
        let peak = out
            .iter()
            .flat_map(|r| r.audio.iter())
            .fold(0.0_f32, |m, x| m.max(x.abs()));
        if peak > 1e-9 {
            let gain = 10.0_f32.powf(db / 20.0) / peak;
            for r in &mut out {
                for x in &mut r.audio {
                    *x *= gain;
                }
            }
        }
    }
    progress.permille.store(1000, Ordering::Relaxed);
    Ok(out)
}

/// Renders and writes the export. With stems or regions, `path` names the folder-and-prefix:
/// files are written as `<path stem> - NN <track or marker>.<ext>` next to it. The extension
/// follows `settings.format`. Returns the files written.
pub fn export(
    project: &Project,
    settings: &ExportSettings,
    path: &Path,
    progress: &Progress,
    plugins: &mut Vec<Box<PluginBox>>,
) -> Result<Vec<PathBuf>, ExportError> {
    export_with_video(project, settings, path, progress, plugins, None)
}

/// [`export`], then (for a single mix file) a video of it next to it, as `<path>.mp4`.
pub fn export_with_video(
    project: &Project,
    settings: &ExportSettings,
    path: &Path,
    progress: &Progress,
    plugins: &mut Vec<Box<PluginBox>>,
    video: Option<&video::VideoSettings>,
) -> Result<Vec<PathBuf>, ExportError> {
    let (samples, _) = load_samples(project, settings.sample_rate);
    let ext = settings.format.extension();
    let base = Tags::of(&project.info);
    let prefix = path
        .file_stem()
        .map_or_else(|| "export".to_owned(), |s| s.to_string_lossy().into_owned());
    // (file name part, tags, range) per pass.
    let passes: Vec<(Option<String>, Tags, Range)> = if settings.regions {
        let regions = marker_regions(project);
        if regions.is_empty() {
            return Err(ExportError::Empty);
        }
        regions
            .into_iter()
            .map(|(name, start, end)| {
                let title = if base.title.is_empty() {
                    name.clone()
                } else {
                    format!("{} - {name}", base.title)
                };
                let tags = Tags {
                    title,
                    ..base.clone()
                };
                (Some(name), tags, Range::Ticks { start, end })
            })
            .collect()
    } else {
        vec![(None, base.clone(), settings.range)]
    };
    let mut written = Vec::new();
    let mut n = 0;
    for (region, tags, range) in passes {
        let s = ExportSettings { range, ..*settings };
        let rendered = render(project, &samples, &s, progress, plugins)?;
        for r in rendered.iter() {
            n += 1;
            let file = match (&region, settings.stems) {
                (None, false) => path.with_extension(ext),
                (None, true) => {
                    path.with_file_name(format!("{prefix} - {n:02} {}.{ext}", file_safe(&r.name)))
                }
                (Some(m), false) => {
                    path.with_file_name(format!("{prefix} - {n:02} {}.{ext}", file_safe(m)))
                }
                (Some(m), true) => path.with_file_name(format!(
                    "{prefix} - {n:02} {} - {}.{ext}",
                    file_safe(m),
                    file_safe(&r.name)
                )),
            };
            formats::write_audio(&file, &r.audio, settings, n as u64 - 1, &tags)?;
            written.push(file);
        }
        if let (Some(v), None, false) = (video, &region, settings.stems) {
            let out = path.with_extension("mp4");
            progress.permille.store(0, Ordering::Relaxed);
            video::export_video(&rendered[0].audio, settings.sample_rate, v, &out, progress)?;
            written.push(out);
        }
    }
    Ok(written)
}

/// The marker regions: from each marker to the next (the last one to the song end), named
/// after the marker, in time order. Regions that are empty or start after the song are left
/// out.
pub fn marker_regions(project: &Project) -> Vec<(String, i64, i64)> {
    let Some((_, song_end)) = range_ticks(project, Range::Song) else {
        return Vec::new();
    };
    let mut markers: Vec<_> = project.playlist.markers.iter().collect();
    markers.sort_by_key(|m| m.at);
    let mut out = Vec::new();
    for (i, m) in markers.iter().enumerate() {
        let end = markers.get(i + 1).map_or(song_end, |next| next.at);
        if m.at >= 0 && end > m.at && m.at < song_end {
            let name = if m.name.trim().is_empty() {
                format!("Region {}", i + 1)
            } else {
                m.name.trim().to_owned()
            };
            out.push((name, m.at, end.min(song_end)));
        }
    }
    out
}

/// Writes interleaved stereo `audio` as a WAV file. Dither uses a fixed seed (plus `seed`), so
/// the same render always gives the same bytes.
pub fn write_wav(
    path: &Path,
    audio: &[f32],
    settings: &ExportSettings,
    seed: u64,
) -> Result<(), ExportError> {
    let (bits, format) = match settings.depth {
        BitDepth::Pcm16 => (16, hound::SampleFormat::Int),
        BitDepth::Pcm24 => (24, hound::SampleFormat::Int),
        BitDepth::Float32 => (32, hound::SampleFormat::Float),
    };
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: settings.sample_rate,
        bits_per_sample: bits,
        sample_format: format,
    };
    let tmp = path.with_extension("wav.part");
    {
        let mut w = hound::WavWriter::create(&tmp, spec)?;
        match settings.depth {
            BitDepth::Float32 => {
                for &x in audio {
                    w.write_sample(x)?;
                }
            }
            BitDepth::Pcm16 | BitDepth::Pcm24 => {
                let mut dither = Tpdf::new(seed);
                for &x in audio {
                    let d = if settings.dither { dither.next() } else { 0.0 };
                    w.write_sample(quantize(x, bits, d))?;
                }
            }
        }
        w.finalize()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// Rounds `x` (full scale ±1) to a `bits`-bit integer after adding `dither` (in LSBs),
/// clipping at full scale.
pub fn quantize(x: f32, bits: u16, dither: f32) -> i32 {
    let max = ((1_i64 << (bits - 1)) - 1) as f32;
    let v = (f64::from(x) * f64::from(max) + f64::from(dither)).round();
    v.clamp(-f64::from(max) - 1.0, f64::from(max)) as i32
}

/// Triangular-PDF dither: the sum of two independent uniform values, ±1 LSB peak. It makes the
/// rounding error independent of the signal (no distortion on quiet fades) at the cost of a
/// flat noise floor about 4.8 dB above plain rounding.
#[derive(Debug, Clone)]
pub struct Tpdf {
    state: u64,
}

impl Tpdf {
    /// A generator with a fixed seed.
    pub fn new(seed: u64) -> Self {
        Self {
            state: 0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0xD1B5_4A32_D192_ED03) | 1,
        }
    }

    fn uniform(&mut self) -> f32 {
        // xorshift64*
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        let r = self.state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (r >> 40) as f32 / (1u64 << 24) as f32 - 0.5
    }

    /// Next dither value in LSBs, -1 to 1.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> f32 {
        self.uniform() + self.uniform()
    }
}

/// A copy of `project` with every clip cut at `end`, so nothing past the range plays during
/// the tail. Frozen tracks with clips past `end` play their clips (a freeze render cannot be
/// cut there).
fn trimmed(project: &Project, end: i64) -> Project {
    let mut p = project.clone();
    let pl = &mut p.playlist;
    for t in &mut pl.tracks {
        if pl.clips.iter().any(|c| c.track == t.id && c.end() > end) {
            t.frozen = None;
        }
    }
    pl.clips.retain(|c| c.start < end);
    for c in &mut pl.clips {
        c.length = c.length.min(end - c.start);
    }
    p
}

/// One project per playlist track that holds pattern or audio clips and is audible in the mix:
/// that track alone, with automation-only tracks as they are in the mix.
fn stem_projects(base: &Project) -> Vec<(String, Project)> {
    let pl = &base.playlist;
    let sounding = |t: gt_core::TrackId| {
        pl.clips
            .iter()
            .any(|c| c.track == t && !matches!(c.kind, ClipKind::Automation(_)))
    };
    let audible: Vec<bool> = pl
        .tracks
        .iter()
        .map(|t| pl.is_track_audible(t.id))
        .collect();
    let mut out = Vec::new();
    for (k, track) in pl.tracks.iter().enumerate() {
        if !audible[k] || !sounding(track.id) {
            continue;
        }
        let mut p = base.clone();
        for (i, t) in p.playlist.tracks.iter_mut().enumerate() {
            t.solo = false;
            t.mute = if sounding(t.id) { i != k } else { !audible[i] };
        }
        out.push((track.name.clone(), p));
    }
    out
}

/// The inserts that something plays into (a channel, a pad or plugin output, a track with
/// sound clips, another strip or a device input), with their stem names.
fn insert_stems(base: &Project) -> Vec<(String, usize)> {
    let m = &base.mixer;
    let mut fed = vec![false; m.strips.len()];
    let mut mark = |i: usize| {
        if let Some(f) = fed.get_mut(i) {
            *f = true;
        }
    };
    for c in &base.channels {
        mark(c.insert);
        if let Some(k) = c.drums() {
            k.pads().filter_map(|(_, p)| p.output).for_each(&mut mark);
        }
        if let Some(p) = c.plugin() {
            p.outputs
                .iter()
                .filter_map(|o| o.route)
                .for_each(|r| mark(usize::from(r)));
        }
    }
    let pl = &base.playlist;
    for t in &pl.tracks {
        if pl
            .clips
            .iter()
            .any(|c| c.track == t.id && !matches!(c.kind, ClipKind::Automation(_)))
        {
            mark(t.insert);
        }
    }
    for (i, s) in m.strips.iter().enumerate().skip(1) {
        if s.input.is_some() {
            mark(i);
        }
    }
    // Whatever a fed strip outputs or sends to is fed too; the order puts sources first.
    for i in m.processing_order().unwrap_or_default() {
        if i != gt_core::MASTER && fed[i] {
            let s = &m.strips[i];
            fed[s.output] = true;
            for x in &s.sends {
                fed[x.to] = true;
            }
        }
    }
    let audible = m.audible();
    (1..m.strips.len())
        .filter(|&i| fed[i] && audible[i])
        .map(|i| (format!("{i} {}", m.strips[i].name), i))
        .collect()
}

/// Sends a command, rendering silent quanta while the queue is full.
fn send(h: &mut EngineHandle, p: &mut AudioProcessor, mut cmd: EngineCommand) {
    let mut scratch = [0.0_f32; 64];
    loop {
        match h.send(cmd) {
            Ok(()) => return,
            Err(c) => {
                cmd = c;
                p.process(&mut scratch);
                h.collect_garbage();
            }
        }
    }
}

/// Loads the whole project into a fresh engine, like the app does when it syncs.
// The engine takes plugins boxed (they travel through its command queue as boxes).
#[allow(clippy::vec_box)]
fn load_engine(
    h: &mut EngineHandle,
    p: &mut AudioProcessor,
    project: &Project,
    samples: &HashMap<SampleSource, Arc<SampleData>>,
    rate: u32,
    plugins: &mut Vec<Box<PluginBox>>,
) {
    send(h, p, EngineCommand::SetMetronome(false));
    for (i, mut plugin) in plugins.drain(..).enumerate() {
        plugin.processor.reset();
        send(
            h,
            p,
            EngineCommand::SetPlugin {
                index: i as u8,
                plugin: Some(plugin),
            },
        );
    }
    send(
        h,
        p,
        EngineCommand::SetTempoMap(Box::new(project.tempo.clone())),
    );
    send(
        h,
        p,
        EngineCommand::SetSignatures(Box::new(project.signatures.clone())),
    );
    for (i, ch) in project.channels.iter().enumerate() {
        // Time-stretch mode: render the fitted sample now, as playback does on a worker.
        let fitted = ch.sampler().and_then(|s| {
            let d = samples.get(s.sample.as_ref()?)?;
            let job = gt_engine::stretch::fit_job(project, s, d)?;
            Some(Arc::new(job.render(d)))
        });
        let (sample, zones) =
            gt_engine::zones::channel_sources(ch, |src| samples.get(src).map(Arc::clone), fitted);
        send(
            h,
            p,
            EngineCommand::SetChannelSample {
                slot: i as u16,
                sample,
            },
        );
        send(
            h,
            p,
            EngineCommand::SetChannelZones {
                slot: i as u16,
                zones,
            },
        );
        if let gt_core::Instrument::Synth(patch) = &ch.instrument {
            let tables =
                gt_engine::tables::synth_tables(patch, |src| samples.get(src).map(Arc::clone));
            if tables.is_some() {
                send(
                    h,
                    p,
                    EngineCommand::SetChannelTables {
                        slot: i as u16,
                        tables,
                    },
                );
            }
        }
        send(
            h,
            p,
            EngineCommand::SetChannelParams {
                slot: i as u16,
                params: Box::new(ChannelParams::from_project(project, i)),
            },
        );
    }
    send(
        h,
        p,
        EngineCommand::SetMixer(Box::new(MixerParams::from_mixer(&project.mixer))),
    );
    for (si, strip) in project.mixer.strips.iter().enumerate() {
        for k in 0..FX_SLOTS {
            if let Some(slot) = &strip.slots[k] {
                // The user's impulse response; a file that does not load leaves the built-in.
                let user = (gt_core::effects::IMPULSE_RESPONSES.len() - 1) as f32;
                let file = slot
                    .file
                    .as_deref()
                    .filter(|_| slot.get("ir") == Some(user))
                    .and_then(|f| gt_project::load_sample(f, rate).ok());
                send(
                    h,
                    p,
                    EngineCommand::SetEffect {
                        strip: si as u16,
                        slot: k as u8,
                        effect: Some(create_effect_with(slot, rate as f32, file.as_ref())),
                    },
                );
            }
        }
    }
    // Stretched clips: the same renders the app caches (D106), made here before playing.
    let renders: HashMap<u64, Arc<SampleData>> =
        gt_engine::stretch::jobs(project, |src| samples.get(src).map(Arc::clone))
            .into_iter()
            .filter_map(|j| {
                let src = samples.get(&j.source)?;
                Some((j.key, Arc::new(j.render(src))))
            })
            .collect();
    let song = SongSnapshot::compile_song_with(
        project,
        |src| samples.get(src).map(Arc::clone),
        |key| renders.get(&key).map(Arc::clone),
    );
    send(h, p, EngineCommand::SetSong(Box::new(song)));
    send(
        h,
        p,
        EngineCommand::SetModulation(Box::new(ModPlan::compile(project))),
    );
    // Song mode with no loop: play straight through and past the end for the tail.
    send(
        h,
        p,
        EngineCommand::SetLoop(LoopRegion {
            start: Tick(0),
            end: Tick(0),
            enabled: false,
        }),
    );
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::vec_box)]
fn render_one(
    project: &Project,
    samples: &HashMap<SampleSource, Arc<SampleData>>,
    settings: &ExportSettings,
    start: i64,
    end: i64,
    progress: &Progress,
    (job, jobs): (u32, u32),
    plugins: &mut Vec<Box<PluginBox>>,
    tap: Option<(u16, gt_engine::Tap)>,
) -> Result<Vec<f32>, ExportError> {
    let rate = settings.sample_rate;
    let (mut h, mut p) = create(EngineConfig {
        sample_rate: rate,
        out_channels: 2,
    });
    load_engine(&mut h, &mut p, project, samples, rate, plugins);
    p.set_tap(tap);
    // The plugins come back for the next pass however this one ends.
    let audio = render_pass(
        &mut h,
        &mut p,
        project,
        settings,
        start,
        end,
        progress,
        (job, jobs),
    );
    plugins.extend(p.take_plugins());
    // What reaches a strip's input is held back by its delay compensation; a clip made from it
    // must start on time.
    if let (Ok(a), Some((strip, gt_engine::Tap::Input))) = (&audio, tap) {
        let d = 2 * p.strip_input_delay(strip);
        let mut a = a.clone();
        a.drain(..d.min(a.len()));
        return Ok(a);
    }
    // An insert's output is earlier than the mix by the delay compensation after it: line the
    // stem up with the mix.
    if let (Ok(a), Some((strip, gt_engine::Tap::Output))) = (&audio, tap) {
        let d = 2 * p
            .mix_latency()
            .saturating_sub(p.strip_output_latency(strip));
        let mut out = vec![0.0; d.min(a.len())];
        out.extend_from_slice(&a[..a.len() - out.len()]);
        return Ok(out);
    }
    audio
}

#[allow(clippy::too_many_arguments)]
fn render_pass(
    h: &mut EngineHandle,
    p: &mut AudioProcessor,
    project: &Project,
    settings: &ExportSettings,
    start: i64,
    end: i64,
    progress: &Progress,
    (job, jobs): (u32, u32),
) -> Result<Vec<f32>, ExportError> {
    let rate = settings.sample_rate;
    // Let every queued command land while stopped, then start exactly on a quantum.
    let mut scratch = vec![0.0_f32; 2 * 256];
    for _ in 0..8 {
        p.process(&mut scratch);
        h.collect_garbage();
    }
    send(h, p, EngineCommand::Locate(Tick(start)));
    send(h, p, EngineCommand::Play);

    let tempo = &project.tempo;
    let seconds = tempo.tick_to_seconds(end as f64) - tempo.tick_to_seconds(start as f64);
    let frames = (seconds * f64::from(rate)).round() as usize;
    let max_tail = if settings.tail {
        (settings.max_tail_seconds.max(0.0) * rate as f32) as usize
    } else {
        0
    };
    const BLOCK: usize = 1024;
    let mut audio: Vec<f32> = Vec::with_capacity(2 * (frames + max_tail.min(rate as usize * 4)));
    let mut buf = vec![0.0_f32; 2 * BLOCK];
    let report = |done: usize| {
        let f = (job as f64 + (done as f64 / frames.max(1) as f64).min(1.0)) / jobs as f64;
        progress
            .permille
            .store((f * 990.0) as u32, Ordering::Relaxed);
    };
    while audio.len() < 2 * frames {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        let n = BLOCK.min(frames - audio.len() / 2);
        p.process(&mut buf[..2 * n]);
        h.collect_garbage();
        audio.extend_from_slice(&buf[..2 * n]);
        report(audio.len() / 2);
    }
    // Tail: until half a second stays below -90 dBFS, or the limit.
    let quiet = 10.0_f32.powf(-90.0 / 20.0);
    let window = rate as usize / 2;
    let mut silent_run = 0;
    let mut tail = 0;
    while tail < max_tail && silent_run < window {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        let n = BLOCK.min(max_tail - tail);
        p.process(&mut buf[..2 * n]);
        h.collect_garbage();
        for f in buf[..2 * n].chunks_exact(2) {
            if f[0].abs().max(f[1].abs()) < quiet {
                silent_run += 1;
            } else {
                silent_run = 0;
            }
        }
        audio.extend_from_slice(&buf[..2 * n]);
        tail += n;
    }
    // Drop the silent half second the detector waited for (keep a short fade-out room).
    if settings.tail && silent_run >= window {
        let keep = audio.len() / 2 - silent_run + (rate as usize / 20).min(silent_run);
        audio.truncate(2 * keep.max(frames));
    }
    Ok(audio)
}

fn file_safe(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.trim().is_empty() {
        "track".to_owned()
    } else {
        s.trim().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_rounds_and_clips() {
        assert_eq!(quantize(0.0, 16, 0.0), 0);
        assert_eq!(quantize(1.0, 16, 0.0), 32_767);
        assert_eq!(quantize(-1.0, 16, 0.0), -32_767);
        assert_eq!(quantize(-2.0, 16, 0.0), -32_768);
        assert_eq!(quantize(5.0, 24, 0.0), 8_388_607);
        assert_eq!(quantize(0.6 / 32_767.0, 16, 0.0), 1);
        assert_eq!(quantize(0.4 / 32_767.0, 16, 0.0), 0);
        assert_eq!(quantize(0.0, 16, 0.6), 1);
    }

    #[test]
    fn tpdf_dither_is_triangular_and_repeatable() {
        let mut d = Tpdf::new(0);
        let v: Vec<f32> = (0..100_000).map(|_| d.next()).collect();
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        let var = v.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / v.len() as f32;
        assert!(mean.abs() < 0.01, "{mean}");
        // Two uniform(-0.5, 0.5): variance 2/12.
        assert!((var - 1.0 / 6.0).abs() < 0.005, "{var}");
        assert!(v.iter().all(|x| x.abs() <= 1.0));
        // Values near the centre are about twice as common as near ±0.5.
        let centre = v.iter().filter(|x| x.abs() < 0.1).count();
        let side = v.iter().filter(|x| (x.abs() - 0.5).abs() < 0.05).count();
        assert!(centre as f32 > 1.6 * side as f32, "{centre} {side}");
        let mut again = Tpdf::new(0);
        assert_eq!(again.next(), v[0]);
    }

    #[test]
    fn dither_turns_a_quiet_tone_into_noise_not_distortion() {
        // A sine at 0.3 LSB rounds to all zeros without dither; with it the tone survives in
        // the average.
        let lsb = 1.0 / 32_767.0;
        let tone = |i: usize| 0.3 * lsb * (i as f32 * 0.01).sin();
        assert!((0..10_000).all(|i| quantize(tone(i), 16, 0.0) == 0));
        let mut d = Tpdf::new(1);
        let mut corr = 0.0;
        for i in 0..200_000 {
            corr += quantize(tone(i), 16, d.next()) as f32 * (i as f32 * 0.01).sin();
        }
        let gain = corr / (200_000.0 * 0.5);
        assert!((gain - 0.3).abs() < 0.05, "{gain}");
    }

    fn short_song() -> Project {
        // The demo's first two bars (the intro).
        Project::demo()
    }

    #[test]
    fn insert_stems_line_up_and_add_up_to_the_mix() {
        let mut project = short_song();
        // Plain routing (every insert straight to the master, no master effects), with a
        // latent gate on the kick's insert so the others must be delayed to line up.
        for s in &mut project.mixer.strips {
            s.slots = Default::default();
            s.sends.clear();
            s.sidechain = None;
            s.output = gt_core::MASTER;
        }
        let kick = project.channels[0].insert;
        project.mixer.strips[kick].slots[0] = Some(
            gt_core::EffectSlot::new(gt_core::EffectKind::Gate)
                .with("threshold", -80.0)
                .with("attack", 0.01)
                .with("lookahead", 2.0),
        );
        let settings = ExportSettings {
            range: Range::Ticks {
                start: 2 * 3840,
                end: 3 * 3840,
            },
            tail: false,
            ..ExportSettings::default()
        };
        let (samples, _) = load_samples(&project, 48_000);
        let go = |s: &ExportSettings| {
            render(&project, &samples, s, &Progress::default(), &mut Vec::new()).unwrap()
        };
        let mix = go(&settings);
        let stems = go(&ExportSettings {
            stems: true,
            stem_kind: StemKind::Inserts,
            ..settings
        });
        assert!(stems.len() >= 3, "{}", stems.len());
        assert!(stems.iter().any(|s| s.name == format!("{kick} Kick")));
        let mut sum = vec![0.0_f32; mix[0].audio.len()];
        for s in &stems {
            assert_eq!(s.audio.len(), sum.len(), "{}", s.name);
            for (d, x) in sum.iter_mut().zip(&s.audio) {
                *d += x;
            }
        }
        let err = sum
            .iter()
            .zip(&mix[0].audio)
            .fold(0.0_f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(err < 1e-3, "{err}");
    }

    #[test]
    fn every_factory_demo_renders_loud_and_clean() {
        for demo in &gt_core::demos::DEMOS {
            let project = (demo.build)();
            // Two bars from the third marker, where every demo has most of its parts.
            let start = project.playlist.markers.get(2).map_or(0, |m| m.at);
            let settings = ExportSettings {
                range: Range::Ticks {
                    start,
                    end: start + 2 * 3840,
                },
                tail: false,
                ..ExportSettings::default()
            };
            let (samples, missing) = load_samples(&project, settings.sample_rate);
            assert!(missing.is_empty(), "{}", demo.name);
            let out = render(
                &project,
                &samples,
                &settings,
                &Progress::default(),
                &mut Vec::new(),
            )
            .unwrap();
            let audio = &out[0].audio;
            let peak = audio.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            let rms = (audio.iter().map(|x| x * x).sum::<f32>() / audio.len() as f32).sqrt();
            assert!(audio.iter().all(|x| x.is_finite()), "{}", demo.name);
            assert!(peak > 0.2 && peak <= 1.0, "{}: peak {peak}", demo.name);
            eprintln!("{}: peak {peak:.2}, rms {rms:.3}", demo.name);
            assert!(rms > 0.02, "{}: rms {rms}", demo.name);
        }
    }

    #[test]
    fn export_matches_real_time_playback() {
        // The same engine, played in device-sized blocks from Stop, gives the same samples.
        let project = short_song();
        let settings = ExportSettings {
            range: Range::Ticks {
                start: 0,
                end: 3840,
            },
            tail: false,
            ..ExportSettings::default()
        };
        let (samples, missing) = load_samples(&project, settings.sample_rate);
        assert!(missing.is_empty());
        let offline = render(
            &project,
            &samples,
            &settings,
            &Progress::default(),
            &mut Vec::new(),
        )
        .unwrap();
        let offline = &offline[0].audio;
        assert_eq!(offline.len(), 2 * 96_000);
        assert!(offline.iter().any(|x| x.abs() > 0.1));

        let (mut h, mut p) = create(EngineConfig {
            sample_rate: 48_000,
            out_channels: 2,
        });
        let base = trimmed(&project, 3840);
        load_engine(&mut h, &mut p, &base, &samples, 48_000, &mut Vec::new());
        let mut scratch = vec![0.0_f32; 2 * 441];
        for _ in 0..20 {
            p.process(&mut scratch);
        }
        h.send(EngineCommand::Locate(Tick(0))).unwrap();
        h.send(EngineCommand::Play).unwrap();
        let mut live = Vec::new();
        while live.len() < offline.len() {
            p.process(&mut scratch);
            live.extend_from_slice(&scratch);
        }
        live.truncate(offline.len());
        // The device-sized callbacks only change where the 32-frame grid sits relative to the
        // command; the engine's quantum FIFO makes the output start later by at most a block.
        let lag = (0..2 * 441).step_by(2).find(|&l| {
            live[l..]
                .iter()
                .zip(offline.iter())
                .take(20_000)
                .all(|(a, b)| a == b)
        });
        assert!(lag.is_some(), "live playback differs from the export");
    }

    #[test]
    fn tails_ring_out_and_stop_at_silence() {
        let project = short_song();
        let base = ExportSettings {
            range: Range::Ticks {
                start: 0,
                end: 3840,
            },
            ..ExportSettings::default()
        };
        let (samples, _) = load_samples(&project, 48_000);
        let render_len = |s: ExportSettings| {
            render(
                &project,
                &samples,
                &s,
                &Progress::default(),
                &mut Vec::new(),
            )
            .unwrap()[0]
                .audio
                .len()
                / 2
        };
        let cut = render_len(ExportSettings {
            tail: false,
            ..base
        });
        let with_tail = render_len(base);
        let capped = render_len(ExportSettings {
            max_tail_seconds: 0.1,
            ..base
        });
        assert_eq!(cut, 96_000);
        assert!(
            with_tail > cut + 4_800,
            "the reverb and delay ring on: {with_tail}"
        );
        assert!(
            with_tail < cut + 10 * 48_000,
            "and stop once silent: {with_tail}"
        );
        assert_eq!(capped, cut + 4_800);
    }

    /// A plugin effect that halves its input, counting resets.
    struct Half(Arc<AtomicU32>);

    impl gt_engine::PluginProcessor for Half {
        fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &gt_engine::PluginContext) -> bool {
            l.iter_mut().chain(r.iter_mut()).for_each(|x| *x *= 0.5);
            true
        }
        fn note_on(&mut self, _: u32, _: u8, _: f32) {}
        fn note_off(&mut self, _: u32, _: u8) {}
        fn release_all(&mut self, _: u32) {}
        fn set_param(&mut self, _: u32, _: f32) {}
        fn reset(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn latency(&self) -> usize {
            0
        }
    }

    #[test]
    fn plugins_render_and_come_back_for_every_pass() {
        use gt_core::{PluginKind, PluginRef, MASTER};
        let mut project = short_song();
        let fx = PluginRef::new("x", "Half", "", PluginKind::Effect, PathBuf::new());
        let id = fx.instance;
        project.mixer.strips[MASTER].slots[FX_SLOTS - 1] = Some(gt_core::EffectSlot::plugin(fx));
        let settings = ExportSettings {
            range: Range::Ticks {
                start: 0,
                end: 3840,
            },
            tail: false,
            ..ExportSettings::default()
        };
        let (samples, _) = load_samples(&project, 48_000);
        let resets = Arc::new(AtomicU32::new(0));
        let mut plugins = vec![Box::new(PluginBox {
            instance: id,
            processor: Box::new(Half(Arc::clone(&resets))),
        })];
        let dry = render(
            &project,
            &samples,
            &settings,
            &Progress::default(),
            &mut Vec::new(),
        )
        .unwrap();
        let wet = render(
            &project,
            &samples,
            &settings,
            &Progress::default(),
            &mut plugins,
        )
        .unwrap();
        assert_eq!(plugins.len(), 1, "handed back");
        let (d, w) = (&dry[0].audio, &wet[0].audio);
        assert!(d.iter().zip(w).all(|(d, w)| (d * 0.5 - w).abs() < 1e-6));
        // Stems: one pass per track, each starting from a reset plugin.
        let before = resets.load(Ordering::Relaxed);
        let stems = ExportSettings {
            stems: true,
            ..settings
        };
        let n = render(
            &project,
            &samples,
            &stems,
            &Progress::default(),
            &mut plugins,
        )
        .unwrap()
        .len();
        assert!(resets.load(Ordering::Relaxed) >= before + n as u32);
        assert_eq!(plugins.len(), 1);
    }

    #[test]
    fn stems_add_up_to_the_mix_and_normalize_together() {
        let project = short_song();
        let settings = ExportSettings {
            range: Range::Ticks {
                start: 0,
                end: 3840,
            },
            tail: false,
            ..ExportSettings::default()
        };
        let (samples, _) = load_samples(&project, 48_000);
        let mix = render(
            &project,
            &samples,
            &settings,
            &Progress::default(),
            &mut Vec::new(),
        )
        .unwrap();
        let stems = render(
            &project,
            &samples,
            &ExportSettings {
                stems: true,
                ..settings
            },
            &Progress::default(),
            &mut Vec::new(),
        )
        .unwrap();
        // The intro has one sounding track.
        assert_eq!(
            stems.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["Intro"]
        );
        assert_eq!(stems[0].audio, mix[0].audio);

        let loud = render(
            &project,
            &samples,
            &ExportSettings {
                normalize_db: Some(-1.0),
                ..settings
            },
            &Progress::default(),
            &mut Vec::new(),
        )
        .unwrap();
        let peak = loud[0].audio.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
        assert!((20.0 * peak.log10() + 1.0).abs() < 1e-3, "{peak}");
    }

    #[test]
    fn whole_song_stems_cover_every_sounding_track() {
        let project = short_song();
        let names: Vec<String> = stem_projects(&trimmed(&project, i64::MAX))
            .into_iter()
            .map(|s| s.0)
            .collect();
        assert_eq!(names, ["Beat", "Intro", "Break"]);
        assert_eq!(range_ticks(&project, Range::Song), Some((0, 12 * 3840)));
        assert_eq!(range_ticks(&Project::empty(), Range::Song), None);
    }

    #[test]
    fn cancel_stops_the_render() {
        let project = short_song();
        let (samples, _) = load_samples(&project, 48_000);
        let progress = Progress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        let r = render(
            &project,
            &samples,
            &ExportSettings::default(),
            &progress,
            &mut Vec::new(),
        );
        assert!(matches!(r, Err(ExportError::Cancelled)));
    }
}
