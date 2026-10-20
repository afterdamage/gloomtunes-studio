//! Performance mode (Prompt 31): a clip launched while playing starts on the exact frame of
//! its launch point, and the clip it replaces stops there, whatever the device buffer size.

use std::sync::Arc;

use gt_core::performance::{grid_row, launch_point, LaunchQuant, Performance};
use gt_core::{ClipKind, Project, SampleData, SampleSource, TempoMap, Tick};
use gt_engine::{create, EngineCommand, EngineConfig, SongSnapshot};

const SR: u32 = 48_000;
/// 120 BPM: a beat is 24 000 frames.
const BEAT: usize = 24_000;

fn impulse_at(frame: usize) -> Arc<SampleData> {
    let mut v = vec![0.0; BEAT];
    v[frame] = 1.0;
    Arc::new(SampleData::mono(SR, v))
}

/// Track 0 has two one-beat audio cells: "a" clicks at its start, "b" 100 frames in.
fn project() -> Project {
    let mut p = Project::empty();
    p.tempo = TempoMap::constant(120.0);
    let t = p.playlist.tracks[0].id;
    for (start, name) in [(0, "a.wav"), (3840, "b.wav")] {
        p.playlist.add_clip(
            t,
            start,
            960,
            ClipKind::Audio {
                source: SampleSource::File(name.into()),
                gain: 1.0,
                gaps: Vec::new(),
                takes: Vec::new(),
                edit: gt_core::AudioEdit::default(),
            },
        );
    }
    p
}

/// Plays "a" from the start, launches "b" once `launch_after` frames have played, and returns
/// the frames of every click in `seconds`.
fn perform(quant: LaunchQuant, launch_after: usize, block: usize, seconds: f64) -> Vec<usize> {
    let p = project();
    let t = p.playlist.tracks[0].id;
    let row = grid_row(&p.playlist, t);
    let (a, b) = (impulse_at(0), impulse_at(100));
    let samples = move |s: &SampleSource| match s {
        SampleSource::File(f) if f.ends_with("a.wav") => Some(Arc::clone(&a)),
        _ => Some(Arc::clone(&b)),
    };
    let horizon = 16 * 3840;
    let mut perf = Performance {
        quant,
        ..Performance::default()
    };
    perf.launch(t, Some(row[0]), 0, &p.signatures);
    let song = |perf: &Performance| {
        Box::new(SongSnapshot::compile_song(
            &p.performance_project(perf, horizon),
            &samples,
        ))
    };
    let (mut h, mut proc_) = create(EngineConfig {
        sample_rate: SR,
        out_channels: 1,
    });
    h.send(EngineCommand::SetTempoMap(Box::new(p.tempo.clone())))
        .unwrap();
    h.send(EngineCommand::SetSong(song(&perf))).unwrap();
    h.send(EngineCommand::SetMetronome(false)).unwrap();
    h.send(EngineCommand::Play).unwrap();
    let total = (seconds * f64::from(SR)) as usize;
    let mut out = vec![0.0_f32; total];
    let mut pos = 0;
    let mut launched = false;
    while pos < total {
        if !launched && pos >= launch_after {
            launched = true;
            // The app's way: the playhead now, plus a margin, rounded up to the launch point.
            let now = Tick((pos as f64 / BEAT as f64 * 960.0) as i64);
            let at = launch_point(quant, now, 160, &p.signatures);
            perf.launch(t, Some(row[1]), at, &p.signatures);
            h.send(EngineCommand::UpdateSong(song(&perf))).unwrap();
        }
        let n = block.min(total - pos);
        proc_.process(&mut out[pos..pos + n]);
        pos += n;
    }
    (0..total).filter(|&i| out[i].abs() > 0.5).collect()
}

#[test]
fn a_launch_waits_for_the_next_bar_and_starts_on_its_exact_frame() {
    for block in [64, 441, 1024] {
        let clicks = perform(LaunchQuant::Bar, 30_000, block, 3.5);
        // "a" loops every beat until bar 2 (frame 96 000), then "b" every beat.
        let want = vec![
            0,
            BEAT,
            2 * BEAT,
            3 * BEAT,
            4 * BEAT + 100,
            5 * BEAT + 100,
            6 * BEAT + 100,
        ];
        assert_eq!(clicks, want, "block {block}");
    }
}

#[test]
fn a_beat_launch_takes_the_next_beat() {
    let clicks = perform(LaunchQuant::Beat, 30_000, 333, 2.5);
    // Launched at beat 1.25 (plus the margin): beat 2.
    assert_eq!(
        clicks,
        vec![0, BEAT, 2 * BEAT + 100, 3 * BEAT + 100, 4 * BEAT + 100]
    );
}
