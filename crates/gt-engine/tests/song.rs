//! Song mode end to end: the playlist compiled, sent to the engine and rendered.

use std::sync::Arc;

use gt_core::{
    AutoPoint, AutoTarget, Automation, ClipKind, Project, SampleData, SampleSource, TempoMap,
    TempoPoint, Tick, MASTER,
};
use gt_engine::{create, EngineCommand, EngineConfig, EngineHandle, LoopRegion, SongSnapshot};

const SR: u32 = 48_000;

fn engine() -> (EngineHandle, gt_engine::AudioProcessor) {
    create(EngineConfig {
        sample_rate: SR,
        out_channels: 2,
    })
}

/// Renders `seconds` of stereo output after sending `song` and pressing play.
fn render(project: &Project, sample: &Arc<SampleData>, seconds: f64) -> Vec<f32> {
    let (mut h, mut p) = engine();
    let song = SongSnapshot::compile_song(project, |_| Some(Arc::clone(sample)));
    h.send(EngineCommand::SetTempoMap(Box::new(project.tempo.clone())))
        .unwrap();
    h.send(EngineCommand::SetSong(Box::new(song))).unwrap();
    h.send(EngineCommand::SetMetronome(false)).unwrap();
    h.send(EngineCommand::Play).unwrap();
    let mut out = vec![0.0; 2 * (seconds * f64::from(SR)) as usize];
    // Odd block size: clip edges must not depend on how the device cuts the stream.
    for chunk in out.chunks_mut(2 * 441) {
        p.process(chunk);
    }
    out
}

fn left(out: &[f32]) -> Vec<f32> {
    out.chunks_exact(2).map(|f| f[0]).collect()
}

fn impulse_at(frame: usize, len: usize) -> Arc<SampleData> {
    let mut v = vec![0.0; len];
    v[frame] = 1.0;
    Arc::new(SampleData::mono(SR, v))
}

fn audio_clip(p: &mut Project, start: i64, length: i64, offset: i64) {
    let t = p.playlist.tracks[0].id;
    let c = p.playlist.add_clip(
        t,
        start,
        length,
        ClipKind::Audio {
            source: SampleSource::BuiltIn(gt_core::BuiltInSample::Kick),
            gain: 1.0,
        },
    );
    p.playlist.clip_mut(c).unwrap().offset = offset;
}

#[test]
fn audio_clip_starts_on_its_exact_frame_across_a_tempo_change() {
    let mut p = Project::empty();
    // 120 BPM for one beat (0.5 s), then 60 BPM: tick 1920 is at 0.5 + 1.0 = 1.5 s.
    p.tempo = TempoMap::from_points_lossy(&[
        TempoPoint {
            at: Tick(0),
            bpm: 120.0,
        },
        TempoPoint {
            at: Tick(960),
            bpm: 60.0,
        },
    ]);
    audio_clip(&mut p, 1920, 3840, 0);
    let out = left(&render(&p, &impulse_at(0, 4800), 2.0));
    let at = (1.5 * f64::from(SR)) as usize;
    assert_eq!(out[at], 1.0, "unity, no fade on an uncut start");
    assert!(out.iter().enumerate().all(|(i, &x)| i == at || x == 0.0));
}

#[test]
fn slip_edit_moves_the_audio_inside_the_clip() {
    let mut p = Project::empty();
    // 120 BPM: 960 ticks = 0.5 s. The impulse sits 1 s into the sample; slipping by 0.5 s
    // makes it sound 0.5 s after the clip's start at 1 s.
    audio_clip(&mut p, 1920, 3840, 960);
    let out = left(&render(&p, &impulse_at(SR as usize, 2 * SR as usize), 2.5));
    let at = (1.5 * f64::from(SR)) as usize;
    assert_eq!(out[at], 1.0);
    assert!(out.iter().enumerate().all(|(i, &x)| i == at || x == 0.0));
}

#[test]
fn clip_end_cuts_the_audio_with_a_short_fade() {
    let mut p = Project::empty();
    // DC for 2 s, clip one beat (0.5 s) long from tick 0.
    audio_clip(&mut p, 0, 960, 0);
    let dc = Arc::new(SampleData::mono(SR, vec![0.5; 2 * SR as usize]));
    let out = left(&render(&p, &dc, 1.0));
    assert_eq!(out[100], 0.5, "no fade-in at the start of the audio");
    assert_eq!(out[SR as usize / 2 - 200], 0.5);
    assert!(out[SR as usize / 2 - 3] < 0.02, "fading out");
    assert!(out[SR as usize / 2..].iter().all(|&x| x == 0.0));
    // No step larger than the fade allows.
    let step = out
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(step < 0.5 / 90.0, "{step}");
}

#[test]
fn automation_moves_the_master_fader() {
    let mut p = Project::empty();
    audio_clip(&mut p, 0, 4 * 3840, 0);
    let t = p.playlist.tracks[1].id;
    // Master at unity for the first second, then down to -inf over 0 ticks (a step), held.
    let unity = gt_core::playlist::volume_to_norm(1.0);
    p.playlist.add_clip(
        t,
        0,
        3840,
        ClipKind::Automation(Automation {
            target: AutoTarget::StripVolume(MASTER),
            points: vec![
                AutoPoint {
                    at: 0,
                    value: unity,
                },
                AutoPoint {
                    at: 1920,
                    value: unity,
                },
                AutoPoint {
                    at: 1920,
                    value: 0.0,
                },
            ],
        }),
    );
    let dc = Arc::new(SampleData::mono(SR, vec![0.5; 3 * SR as usize]));
    let out = left(&render(&p, &dc, 2.0));
    assert!((out[SR as usize / 2] - 0.5).abs() < 1e-6);
    // The fader ramps down within about 10 ms + one quantum of 1 s.
    assert!(out[SR as usize + 1000..].iter().all(|&x| x == 0.0));
}

#[test]
fn song_mode_notes_do_not_repeat_and_loop_wraps_replay_them() {
    // One kick at tick 0 of a pattern placed once at bar 2: in song mode it plays once per
    // pass, not every pattern length.
    let mut p = Project::empty();
    let c = p.add_channel("k", None).unwrap();
    p.current_pattern_mut().toggle_step(c, 0);
    let id = p.current_pattern;
    let t = p.playlist.tracks[0].id;
    p.playlist.add_clip(t, 3840, 3840, ClipKind::Pattern(id));
    let song = SongSnapshot::compile_song(&p, |_| None);
    let (mut h, mut proc_) = engine();
    h.send(EngineCommand::SetMetronome(false)).unwrap();
    let s = Arc::new(SampleData::mono(SR, {
        let mut v = vec![0.0; 100];
        v[0] = 1.0;
        v
    }));
    h.send(EngineCommand::SetChannelSample {
        slot: 0,
        sample: Some(s),
    })
    .unwrap();
    h.send(EngineCommand::SetChannelParams {
        slot: 0,
        params: Box::new(gt_engine::ChannelParams {
            gain: 1.0,
            ..Default::default()
        }),
    })
    .unwrap();
    h.send(EngineCommand::SetSong(Box::new(song))).unwrap();
    h.send(EngineCommand::SetLoop(LoopRegion {
        start: Tick(0),
        end: Tick(2 * 3840),
        enabled: true,
    }))
    .unwrap();
    h.send(EngineCommand::Play).unwrap();
    // 120 BPM: a bar is 2 s; the loop is 4 s. Render 9 s: hits at 2, 6 s (and none at 4, 8).
    // Velocity 100/127 squared, equal-power centre: about 0.44.
    let mut out = vec![0.0; 2 * 9 * SR as usize];
    for chunk in out.chunks_mut(2 * 512) {
        proc_.process(chunk);
    }
    let hits: Vec<usize> = left(&out)
        .iter()
        .enumerate()
        .filter(|(_, &x)| x.abs() > 0.3)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(hits, vec![2 * SR as usize, 6 * SR as usize]);
}
