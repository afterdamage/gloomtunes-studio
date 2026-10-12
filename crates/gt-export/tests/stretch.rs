//! Stretched audio clips (Prompt 15): tempo follow puts the audio's beats on the project's
//! beats through tempo changes, the export renders exactly what playback plays, and a clip
//! whose render is not ready yet still plays at the right speed.

use std::collections::HashMap;
use std::sync::Arc;

use gt_core::{
    AudioEdit, ClipKind, ClipStretch, Project, SampleData, SampleSource, StretchMode, TempoMap,
    TempoPoint, Tick,
};
use gt_engine::{create, EngineCommand, EngineConfig, SongSnapshot};
use gt_export::{render, BitDepth, ExportSettings, Progress, Range};

const SR: u32 = 48_000;

/// Eight beats at 100 BPM: a short burst on every beat.
fn loop_100() -> Arc<SampleData> {
    let beat = SR as usize * 60 / 100;
    let mut v = vec![0.0_f32; 8 * beat];
    for b in 0..8 {
        for i in 0..64 {
            v[b * beat + i] = if i % 2 == 0 { 0.8 } else { -0.8 };
        }
    }
    Arc::new(SampleData::mono(SR, v))
}

fn project(stretch: ClipStretch, tempo: TempoMap) -> (Project, SampleSource) {
    let mut p = Project::empty();
    p.tempo = tempo;
    let src = SampleSource::File("loop 100.wav".into());
    let t = p.playlist.tracks[0].id;
    p.playlist.add_clip(
        t,
        0,
        8 * 960,
        ClipKind::Audio {
            source: src.clone(),
            gain: 1.0,
            gaps: Vec::new(),
            takes: Vec::new(),
            edit: AudioEdit {
                stretch,
                ..AudioEdit::default()
            },
        },
    );
    (p, src)
}

fn follow(bpm: f64) -> ClipStretch {
    ClipStretch {
        mode: StretchMode::TempoFollow,
        bpm,
        ..ClipStretch::default()
    }
}

fn export(p: &Project, samples: &HashMap<SampleSource, Arc<SampleData>>) -> Vec<f32> {
    let settings = ExportSettings {
        sample_rate: SR,
        depth: BitDepth::Float32,
        dither: false,
        normalize_db: None,
        range: Range::Song,
        tail: false,
        max_tail_seconds: 0.0,
        stems: false,
    };
    let out = render(p, samples, &settings, &Progress::default(), &mut Vec::new()).unwrap();
    out[0].audio.chunks_exact(2).map(|f| f[0]).collect()
}

/// Plays the song in the engine the way the app does: renders from the cache, odd blocks.
fn play(p: &Project, sample: &Arc<SampleData>, frames: usize, cached: bool) -> Vec<f32> {
    let lookup = |_: &SampleSource| Some(Arc::clone(sample));
    let renders: HashMap<u64, Arc<SampleData>> = if cached {
        gt_engine::stretch::jobs(p, lookup)
            .into_iter()
            .map(|j| (j.key, Arc::new(j.render(sample))))
            .collect()
    } else {
        HashMap::new()
    };
    let song = SongSnapshot::compile_song_with(p, lookup, |k| renders.get(&k).cloned());
    let (mut h, mut proc_) = create(EngineConfig {
        sample_rate: SR,
        out_channels: 2,
    });
    h.send(EngineCommand::SetTempoMap(Box::new(p.tempo.clone())))
        .unwrap();
    h.send(EngineCommand::SetSong(Box::new(song))).unwrap();
    h.send(EngineCommand::SetMetronome(false)).unwrap();
    h.send(EngineCommand::Play).unwrap();
    let mut out = vec![0.0; 2 * frames];
    for chunk in out.chunks_mut(2 * 441) {
        proc_.process(chunk);
    }
    out.chunks_exact(2).map(|f| f[0]).collect()
}

/// Where the burst nearest `at` starts (first sample above 0.2 within `win` frames), relative
/// to `at`.
fn onset_near(x: &[f32], at: usize, win: usize) -> i64 {
    let lo = at.saturating_sub(win);
    let hi = (at + win).min(x.len());
    let i = (lo..hi).find(|&i| x[i].abs() > 0.2).unwrap_or(hi);
    i as i64 - at as i64
}

#[test]
fn tempo_follow_puts_the_beats_on_the_project_beats() {
    // 120 BPM for 4 beats, then 90 BPM: the 100 BPM loop must follow both.
    let tempo = TempoMap::from_points_lossy(&[
        TempoPoint {
            at: Tick(0),
            bpm: 120.0,
        },
        TempoPoint {
            at: Tick(4 * 960),
            bpm: 90.0,
        },
    ]);
    let (p, src) = project(follow(100.0), tempo.clone());
    let samples = HashMap::from([(src, loop_100())]);
    let out = export(&p, &samples);
    let want_len = tempo.tick_to_seconds(8.0 * 960.0) * f64::from(SR);
    assert!(
        (out.len() as f64 - want_len).abs() < 2.0,
        "{} frames",
        out.len()
    );
    for b in 1..8 {
        let at = (tempo.tick_to_seconds(f64::from(b) * 960.0) * f64::from(SR)) as usize;
        let off = onset_near(&out, at, 2400);
        // Within 5 ms of the beat (the engine's transient smearing is a few ms).
        assert!(off.abs() < 240, "beat {b} is {off} frames off");
    }
}

#[test]
fn the_export_matches_playback() {
    let tempo = TempoMap::from_points_lossy(&[TempoPoint {
        at: Tick(0),
        bpm: 128.0,
    }]);
    for stretch in [
        follow(100.0),
        ClipStretch {
            mode: StretchMode::Manual,
            ratio: 1.3,
            semitones: -3.25,
            bpm: 0.0,
        },
    ] {
        let (p, src) = project(stretch, tempo.clone());
        let sample = loop_100();
        let samples = HashMap::from([(src, Arc::clone(&sample))]);
        let offline = export(&p, &samples);
        let live = play(&p, &sample, offline.len(), true);
        assert_eq!(offline, live, "{stretch:?}");
    }
}

#[test]
fn without_its_render_a_clip_plays_resampled_at_the_right_speed() {
    let tempo = TempoMap::from_points_lossy(&[TempoPoint {
        at: Tick(0),
        bpm: 120.0,
    }]);
    let (p, _) = project(follow(100.0), tempo.clone());
    let out = play(&p, &loop_100(), 4 * SR as usize, false);
    // Beats every 0.5 s, exactly (resampling keeps timing, only the pitch is off).
    for b in 1..8 {
        let off = onset_near(&out, b * SR as usize / 2, 1200);
        assert!(off.abs() <= 2, "beat {b} is {off} frames off");
    }
}

#[test]
fn bpm_is_guessed_from_a_whole_number_of_beats() {
    let g = ClipStretch::guess_bpm(8.0 * 60.0 / 100.0).unwrap();
    assert!((g - 100.0).abs() < 1e-9);
    let g = ClipStretch::guess_bpm(2.0).unwrap(); // 4 beats at 120
    assert!((g - 120.0).abs() < 1e-9);
    assert_eq!(ClipStretch::guess_bpm(0.0), None);
}
