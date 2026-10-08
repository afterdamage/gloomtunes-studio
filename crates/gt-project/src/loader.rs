//! Audio file loading: decode with symphonia, convert to `f32`, resample to the engine rate
//! with rubato. Runs on a worker thread, never on the audio thread.
//!
//! Resampling uses rubato's asynchronous sinc resampler: each output sample is a weighted sum of
//! 128 input samples around its position, weighted by a windowed sinc (an ideal low-pass at the
//! lower of the two Nyquist frequencies, made finite by a Blackman-Harris window). That keeps
//! the pass band flat and pushes images and aliases far down, at a cost that only matters at
//! load time. Files already at the target rate are not touched.

use std::fs::File;
use std::path::Path;

use gt_core::SampleData;
use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use symphonia::core::audio::sample::Sample;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// File extensions the browser lists and the loader accepts (lower case).
pub const AUDIO_EXTENSIONS: [&str; 5] = ["wav", "flac", "mp3", "ogg", "oga"];
/// Longest sample accepted, in seconds. Longer files belong in audio clips (Step 7).
pub const MAX_SECONDS: f64 = 600.0;

/// Why a file could not be loaded.
#[derive(Debug)]
pub enum LoadError {
    /// The file could not be opened or read.
    Io(std::io::Error),
    /// Not a supported audio format, or no audio track.
    Unsupported(String),
    /// The file is damaged or could not be decoded.
    Decode(String),
    /// The file decodes to no audio.
    Empty,
    /// Longer than [`MAX_SECONDS`].
    TooLong(f64),
    /// The resampler failed.
    Resample(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot read file: {e}"),
            Self::Unsupported(e) => write!(f, "unsupported audio file: {e}"),
            Self::Decode(e) => write!(f, "cannot decode file: {e}"),
            Self::Empty => write!(f, "file contains no audio"),
            Self::TooLong(s) => write!(
                f,
                "file is {s:.0} s long; samples are limited to {MAX_SECONDS:.0} s"
            ),
            Self::Resample(e) => write!(f, "resampling failed: {e}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// True if `path` has one of the [`AUDIO_EXTENSIONS`].
pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Loads an audio file and resamples it to `target_rate`. Keeps mono files mono; files with
/// more than two channels keep their first two.
pub fn load_sample(path: &Path, target_rate: u32) -> Result<SampleData, LoadError> {
    let decoded = decode(path)?;
    resample(decoded, target_rate)
}

/// Decodes a file at its own sample rate.
pub fn decode(path: &Path) -> Result<SampleData, LoadError> {
    let file = File::open(path).map_err(LoadError::Io)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| LoadError::Unsupported(e.to_string()))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| LoadError::Unsupported("no audio track".into()))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| LoadError::Unsupported("no audio codec parameters".into()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| LoadError::Unsupported(e.to_string()))?;
    let track_id = track.id;

    let mut rate = 0;
    let mut planes: Vec<Vec<f32>> = Vec::new();
    let mut interleaved: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            // Some files end with a truncated packet; keep what decoded.
            Err(SymError::IoError(_)) if !planes.is_empty() => break,
            Err(e) => return Err(LoadError::Decode(e.to_string())),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(SymError::DecodeError(_)) => continue, // skip a damaged packet
            Err(e) => return Err(LoadError::Decode(e.to_string())),
        };
        let spec = buf.spec();
        let channels = spec.channels().count().max(1);
        if planes.is_empty() {
            rate = spec.rate();
            planes = vec![Vec::new(); channels.min(2)];
        }
        interleaved.resize(buf.samples_interleaved(), f32::MID);
        buf.copy_to_slice_interleaved(&mut interleaved);
        for frame in interleaved.chunks_exact(channels) {
            for (plane, &s) in planes.iter_mut().zip(frame) {
                plane.push(s);
            }
        }
        if rate > 0 && planes[0].len() as f64 / f64::from(rate) > MAX_SECONDS {
            return Err(LoadError::TooLong(planes[0].len() as f64 / f64::from(rate)));
        }
    }
    if planes.first().is_none_or(Vec::is_empty) || rate == 0 {
        return Err(LoadError::Empty);
    }
    Ok(SampleData {
        sample_rate: rate,
        channels: planes,
    })
}

/// Resamples to `target_rate` (returns the input unchanged when the rates already match).
pub fn resample(input: SampleData, target_rate: u32) -> Result<SampleData, LoadError> {
    if input.sample_rate == target_rate || input.frames() == 0 || target_rate == 0 {
        return Ok(input);
    }
    let channels = input.channels.len();
    let frames_in = input.frames();
    let ratio = f64::from(target_rate) / f64::from(input.sample_rate);
    let params = SincInterpolationParameters::new(128, WindowFunction::BlackmanHarris2)
        .oversampling_factor(256)
        .interpolation(SincInterpolationType::Quadratic);
    let err = |e: &dyn std::fmt::Display| LoadError::Resample(e.to_string());
    let mut resampler =
        Async::<f32>::new_sinc(ratio, 1.0, &params, 1024, channels, FixedAsync::Input)
            .map_err(|e| err(&e))?;
    let out_capacity = resampler.process_all_needed_output_len(frames_in);
    let mut out = vec![vec![0.0_f32; out_capacity]; channels];
    let adapter_in =
        SequentialSliceOfVecs::new(&input.channels, channels, frames_in).map_err(|e| err(&e))?;
    let mut adapter_out =
        SequentialSliceOfVecs::new_mut(&mut out, channels, out_capacity).map_err(|e| err(&e))?;
    let (_, produced) = resampler
        .process_all_into_buffer(&adapter_in, &mut adapter_out, frames_in, None)
        .map_err(|e| err(&e))?;
    let want = ((frames_in as f64 * ratio).round() as usize).min(produced);
    for plane in &mut out {
        plane.truncate(want);
    }
    Ok(SampleData {
        sample_rate: target_rate,
        channels: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gt-loader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn write_wav(
        path: &Path,
        rate: u32,
        channels: u16,
        frames: usize,
        f: impl Fn(usize, u16) -> f32,
    ) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..frames {
            for c in 0..channels {
                w.write_sample((f(i, c) * 32767.0) as i16).unwrap();
            }
        }
        w.finalize().unwrap();
    }

    /// Frequency from interpolated rising zero crossings.
    fn frequency(v: &[f32], rate: u32) -> f64 {
        let mut crossings = Vec::new();
        for (i, w) in v.windows(2).enumerate() {
            if w[0] < 0.0 && w[1] >= 0.0 {
                crossings.push(i as f64 + f64::from(-w[0] / (w[1] - w[0])));
            }
        }
        let n = crossings.len() - 1;
        n as f64 / (crossings[n] - crossings[0]) * f64::from(rate)
    }

    #[test]
    fn wav_at_engine_rate_is_bit_exact() {
        let p = temp_path("same.wav");
        write_wav(&p, 48_000, 1, 480, |i, _| if i == 10 { 0.5 } else { 0.0 });
        let s = load_sample(&p, 48_000).unwrap();
        assert_eq!(s.sample_rate, 48_000);
        assert_eq!(s.frames(), 480);
        assert!((s.left()[10] - 0.5).abs() < 1e-4);
        assert_eq!(s.left()[11], 0.0);
    }

    #[test]
    fn resampling_keeps_length_and_pitch() {
        let p = temp_path("sine.wav");
        let frames = 44_100;
        write_wav(&p, 44_100, 2, frames, |i, c| {
            let f = if c == 0 { 1000.0 } else { 3000.0 };
            0.5 * (TAU * f * i as f32 / 44_100.0).sin()
        });
        for target in [48_000, 96_000, 22_050] {
            let s = load_sample(&p, target).unwrap();
            assert_eq!(s.sample_rate, target);
            assert_eq!(s.channels.len(), 2);
            assert_eq!(s.frames(), target as usize, "rate {target}");
            // Skip the edges, where the filter sees the start/end of the file.
            let mid = target as usize / 10..target as usize * 9 / 10;
            let fl = frequency(&s.left()[mid.clone()], target);
            let fr = frequency(&s.right()[mid.clone()], target);
            assert!((fl - 1000.0).abs() < 0.5, "{target}: {fl}");
            assert!((fr - 3000.0).abs() < 0.5, "{target}: {fr}");
            let peak = s.left()[mid].iter().fold(0.0_f32, |m, x| m.max(x.abs()));
            assert!((peak - 0.5).abs() < 0.01, "{target}: {peak}");
        }
    }

    #[test]
    fn errors_are_reported_not_panicked() {
        assert!(matches!(
            load_sample(Path::new("/definitely/not/here.wav"), 48_000),
            Err(LoadError::Io(_))
        ));
        let p = temp_path("garbage.wav");
        std::fs::write(&p, b"this is not audio at all, just some bytes").unwrap();
        assert!(load_sample(&p, 48_000).is_err());
        let p = temp_path("empty.wav");
        write_wav(&p, 48_000, 1, 0, |_, _| 0.0);
        assert!(load_sample(&p, 48_000).is_err());
    }

    #[test]
    fn extensions_are_case_insensitive() {
        assert!(is_audio_file(Path::new("a/b/Kick.WAV")));
        assert!(is_audio_file(Path::new("loop.flac")));
        assert!(!is_audio_file(Path::new("notes.txt")));
        assert!(!is_audio_file(Path::new("noext")));
    }
}
