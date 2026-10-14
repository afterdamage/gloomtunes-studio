//! Compressed formats, region export and video (Prompt 26): every file decodes with symphonia
//! and matches the WAV render (lossless exactly, lossy within a stated tolerance).

use std::path::{Path, PathBuf};

use gt_export::video::{self, VideoSettings, VideoStyle};
use gt_export::{
    export, export_with_video, load_samples, marker_regions, render, BitDepth, ExportSettings,
    Format, Mp3Mode, Progress, Range,
};
use gt_project::{file, loader};

const SR: u32 = 48_000;
/// One bar at the fixture's 120 BPM: 2 s.
const BAR: i64 = 3840;

fn fixture() -> gt_core::Project {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference.gloom");
    let mut p = file::load(&path, &temp_dir("samples").join("x"))
        .expect("fixture loads")
        .project;
    p.info.title = "Gloom Reference".into();
    p.info.artist = "GloomTunes".into();
    p.info.genre = "Darkwave".into();
    p
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("gt-formats-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Bars 3 and 4 of the fixture, no tail.
fn settings(format: Format, depth: BitDepth) -> ExportSettings {
    ExportSettings {
        sample_rate: SR,
        depth,
        dither: true,
        range: Range::Ticks {
            start: 2 * BAR,
            end: 4 * BAR,
        },
        tail: false,
        format,
        ..ExportSettings::default()
    }
}

/// Interleaved stereo samples of a decoded file.
fn decode(path: &Path) -> (u32, Vec<f32>) {
    let d = loader::decode(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(d.channels.len(), 2, "{}", path.display());
    let audio = d.channels[0]
        .iter()
        .zip(&d.channels[1])
        .flat_map(|(&l, &r)| [l, r])
        .collect();
    (d.sample_rate, audio)
}

fn export_one(p: &gt_core::Project, s: &ExportSettings, dir: &Path, name: &str) -> PathBuf {
    let out = dir.join(name);
    let written = export(p, s, &out, &Progress::default(), &mut Vec::new()).unwrap();
    assert_eq!(written.len(), 1);
    assert_eq!(
        written[0].extension().unwrap().to_str().unwrap(),
        s.format.extension()
    );
    written[0].clone()
}

#[test]
fn flac_decodes_to_the_same_samples_as_wav() {
    let p = fixture();
    let dir = temp_dir("flac");
    for depth in [BitDepth::Pcm16, BitDepth::Pcm24] {
        let wav = export_one(&p, &settings(Format::Wav, depth), &dir, "a.wav");
        let flac = export_one(&p, &settings(Format::Flac, depth), &dir, "a.flac");
        let (rw, w) = decode(&wav);
        let (rf, f) = decode(&flac);
        assert_eq!((rw, rf), (SR, SR));
        assert_eq!(w.len(), 2 * 4 * SR as usize, "two bars at 120 BPM");
        assert!(w == f, "{depth:?}: lossless means bit-identical");
        assert!(std::fs::metadata(&flac).unwrap().len() < std::fs::metadata(&wav).unwrap().len());
        let bytes = std::fs::read(&flac).unwrap();
        assert!(contains(&bytes, b"TITLE=Gloom Reference"));
        assert!(contains(&bytes, b"GENRE=Darkwave"));
    }
}

/// Error-to-signal ratio in dB of `b` (shifted by `off` frames) against `a`, over `a`'s
/// frames `from..to`.
fn error_db(a: &[f32], b: &[f32], off: usize, from: usize, to: usize) -> f32 {
    let (mut err, mut sig) = (0.0_f64, 0.0_f64);
    for i in from..to.min(b.len() / 2 - off) {
        for c in 0..2 {
            let x = f64::from(a[2 * i + c]);
            let y = f64::from(b[2 * (i + off) + c]);
            err += (x - y) * (x - y);
            sig += x * x;
        }
    }
    (10.0 * (err / sig.max(1e-20)).log10()) as f32
}

/// The offset (in frames, 0 to `max`) that best lines `b` up with `a` (found on a quarter
/// second), and the error-to-signal ratio over the whole file there.
fn best_match(a: &[f32], b: &[f32], max: usize) -> (usize, f32) {
    let off = (0..=max)
        .min_by(|&x, &y| {
            error_db(a, b, x, 24_000, 36_000).total_cmp(&error_db(a, b, y, 24_000, 36_000))
        })
        .unwrap();
    (off, error_db(a, b, off, 0, a.len() / 2))
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn lossy_files_decode_close_to_the_render() {
    let p = fixture();
    let dir = temp_dir("lossy");
    let s = settings(Format::Wav, BitDepth::Float32);
    let (samples, _) = load_samples(&p, SR);
    let reference = render(&p, &samples, &s, &Progress::default(), &mut Vec::new())
        .unwrap()
        .remove(0)
        .audio;
    let frames = reference.len() / 2;
    // (format settings, worst error-to-signal ratio in dB). Perceptual coders do not aim at a
    // small waveform error; these bounds (measured: about -20 to -23 dB) catch a broken encoder
    // (wrong channels, rate, gain or timing gives about 0 dB or worse), not codec quality.
    let cases = [
        (
            ExportSettings {
                format: Format::Ogg,
                ogg_quality: 6.0,
                ..s
            },
            -15.0,
        ),
        (
            ExportSettings {
                format: Format::Mp3,
                mp3: Mp3Mode::Cbr(320),
                ..s
            },
            -15.0,
        ),
        (
            ExportSettings {
                format: Format::Mp3,
                mp3: Mp3Mode::Vbr(2),
                ..s
            },
            -15.0,
        ),
    ];
    for (k, (s, bound)) in cases.into_iter().enumerate() {
        let file = export_one(&p, &s, &dir, &format!("l{k}.{}", s.format.extension()));
        let (rate, audio) = decode(&file);
        assert_eq!(rate, SR);
        let len = audio.len() / 2;
        // Vorbis keeps the exact length; MP3 decoders may add up to one frame of padding and
        // the encoder delay when they skip the gapless info.
        let extra = if s.format == Format::Ogg {
            0
        } else {
            1152 + 2 * 1152
        };
        assert!(
            len >= frames - 1 && len <= frames + extra,
            "{:?}: {len} frames for {frames}",
            s.format
        );
        let (off, db) = best_match(&reference, &audio, if extra == 0 { 0 } else { 2 * 1152 });
        eprintln!("{:?} {:?}: offset {off}, error {db:.1} dB", s.format, s.mp3);
        assert!(db < bound, "{:?}: {db:.1} dB at offset {off}", s.format);
        let bytes = std::fs::read(&file).unwrap();
        match s.format {
            Format::Mp3 => {
                assert_eq!(&bytes[..3], b"ID3");
                assert!(contains(&bytes, b"Gloom Reference") && contains(&bytes, b"TPE1"));
            }
            _ => assert!(contains(&bytes, b"ARTIST=GloomTunes")),
        }
    }
}

#[test]
fn every_marker_region_gets_its_own_file() {
    let mut p = fixture();
    p.playlist.markers.clear();
    p.playlist.add_marker(BAR, "Verse");
    p.playlist.add_marker(0, "Intro");
    p.playlist.add_marker(3 * BAR, "Drop/2");
    p.playlist.add_marker(50 * BAR, "After the end");
    let regions = marker_regions(&p);
    let song_end = gt_export::range_ticks(&p, Range::Song).unwrap().1;
    assert_eq!(
        regions,
        vec![
            ("Intro".to_owned(), 0, BAR),
            ("Verse".to_owned(), BAR, 3 * BAR),
            ("Drop/2".to_owned(), 3 * BAR, song_end),
        ]
    );
    let dir = temp_dir("regions");
    let s = ExportSettings {
        regions: true,
        ..settings(Format::Flac, BitDepth::Pcm24)
    };
    let written = export(
        &p,
        &s,
        &dir.join("song.flac"),
        &Progress::default(),
        &mut Vec::new(),
    )
    .unwrap();
    let names: Vec<_> = written
        .iter()
        .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "song - 01 Intro.flac",
            "song - 02 Verse.flac",
            "song - 03 Drop_2.flac"
        ]
    );
    // Each file holds exactly its region (no tail): the same samples as a render of it.
    let whole = decode(&export_one(
        &p,
        &ExportSettings {
            range: Range::Ticks {
                start: 0,
                end: song_end,
            },
            ..settings(Format::Wav, BitDepth::Float32)
        },
        &dir,
        "whole.wav",
    ))
    .1;
    for ((_, start, end), f) in regions.iter().zip(&written) {
        let (_, a) = decode(f);
        let seconds = (end - start) as f64 / 960.0 / 2.0;
        assert_eq!(
            a.len(),
            2 * (seconds * f64::from(SR)).round() as usize,
            "{f:?}"
        );
        // Bar-aligned regions at 120 BPM start on whole frames of the full render; the
        // 24-bit file is within dither and rounding of it, except where a region cuts notes
        // (releases and effect tails are not carried into the next region).
        let at = 2 * (*start as f64 / 960.0 / 2.0 * f64::from(SR)) as usize;
        let head = &whole[at..at + 2 * 4800];
        let err = head
            .iter()
            .zip(&a)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0_f32, f32::max);
        if *start == 0 {
            assert!(err < 1e-5, "{f:?}: {err}");
        }
    }
}

#[test]
fn video_needs_ffmpeg_and_makes_an_mp4() {
    let Some(_) = video::find_ffmpeg() else {
        eprintln!("no ffmpeg on this machine: only the error path is checked");
        let p = fixture();
        let dir = temp_dir("novideo");
        let r = export_with_video(
            &p,
            &settings(Format::Wav, BitDepth::Pcm16),
            &dir.join("v.wav"),
            &Progress::default(),
            &mut Vec::new(),
            Some(&VideoSettings::default()),
        );
        assert!(r.unwrap_err().to_string().contains("ffmpeg"));
        return;
    };
    let p = fixture();
    let dir = temp_dir("video");
    // A small original picture for the still-image style (binary PPM).
    let img = dir.join("cover.ppm");
    let mut ppm = b"P6 64 36 255\n".to_vec();
    ppm.extend((0..64 * 36).flat_map(|i| [(i % 64 * 4) as u8, 20, 60]));
    std::fs::write(&img, ppm).unwrap();
    for style in [
        VideoStyle::Spectrum,
        VideoStyle::Waveform,
        VideoStyle::Image(img),
    ] {
        let v = VideoSettings {
            style: style.clone(),
            width: 320,
            height: 180,
            fps: 10,
            ..VideoSettings::default()
        };
        let written = export_with_video(
            &p,
            &settings(Format::Mp3, BitDepth::Pcm16),
            &dir.join("v.mp3"),
            &Progress::default(),
            &mut Vec::new(),
            Some(&v),
        )
        .unwrap();
        assert_eq!(written, [dir.join("v.mp3"), dir.join("v.mp4")], "{style:?}");
        let bytes = std::fs::read(dir.join("v.mp4")).unwrap();
        assert_eq!(&bytes[4..8], b"ftyp", "{style:?}");
        assert!(!dir.join("v.audio.wav").exists() && !dir.join("v.part.mp4").exists());
    }
}
