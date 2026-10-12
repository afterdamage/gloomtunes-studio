//! Audio clip editing (Prompt 14): fades and their shapes, automatic crossfades, reverse,
//! the de-click when playback starts inside a clip, and chased sampler one-shots.

use std::sync::Arc;

use gt_core::{
    AudioEdit, ClipKind, Fade, FadeCurve, Project, SampleData, SampleSource, TempoMap, Tick,
};
use gt_engine::{
    create, ChannelParams, EngineCommand, EngineConfig, NoteKind, NoteSpan, SongEvent, SongSnapshot,
};

const SR: u32 = 48_000;

/// Renders `seconds` of the left channel after sending the song and playing from `from`.
fn render(project: &Project, sample: &Arc<SampleData>, from: Tick, seconds: f64) -> Vec<f32> {
    let (mut h, mut p) = create(EngineConfig {
        sample_rate: SR,
        out_channels: 2,
    });
    let song = SongSnapshot::compile_song(project, |_| Some(Arc::clone(sample)));
    h.send(EngineCommand::SetTempoMap(Box::new(project.tempo.clone())))
        .unwrap();
    h.send(EngineCommand::SetSong(Box::new(song))).unwrap();
    h.send(EngineCommand::SetMetronome(false)).unwrap();
    h.send(EngineCommand::Locate(from)).unwrap();
    h.send(EngineCommand::Play).unwrap();
    let mut out = vec![0.0; 2 * (seconds * f64::from(SR)) as usize];
    for chunk in out.chunks_mut(2 * 441) {
        p.process(chunk);
    }
    out.chunks_exact(2).map(|f| f[0]).collect()
}

fn clip(p: &mut Project, start: i64, length: i64, edit: AudioEdit) -> gt_core::ClipId {
    let t = p.playlist.tracks[0].id;
    p.playlist.add_clip(
        t,
        start,
        length,
        ClipKind::Audio {
            source: SampleSource::BuiltIn(gt_core::BuiltInSample::Kick),
            gain: 1.0,
            gaps: Vec::new(),
            takes: Vec::new(),
            edit,
        },
    )
}

fn dc(seconds: f64) -> Arc<SampleData> {
    Arc::new(SampleData::mono(
        SR,
        vec![0.5; (seconds * f64::from(SR)) as usize],
    ))
}

#[test]
fn fades_follow_their_curves() {
    for curve in FadeCurve::ALL {
        let mut p = Project::empty();
        // 120 BPM: a 960-tick (0.5 s) fade in and out on a 2 s clip of DC.
        let fade = Fade { len: 960, curve };
        clip(
            &mut p,
            0,
            3840,
            AudioEdit {
                fade_in: fade,
                fade_out: fade,
                ..AudioEdit::default()
            },
        );
        let out = render(&p, &dc(3.0), Tick(0), 2.1);
        let half = SR as usize / 2;
        for k in [0, half / 4, half / 2, 3 * half / 4] {
            let x = k as f64 / half as f64;
            let want = 0.5 * curve.gain(x) as f32;
            assert!((out[k] - want).abs() < 1e-4, "{curve:?} in at {k}");
            let back = 4 * half - k;
            let want_out = 0.5 * curve.gain(1.0 - (half - k) as f64 / half as f64) as f32;
            assert!(
                (out[back - 1] - want_out).abs() < 2e-3,
                "{curve:?} out at {back}: {} vs {want_out}",
                out[back - 1]
            );
        }
        assert!((out[SR as usize] - 0.5).abs() < 1e-6, "full level between");
    }
}

#[test]
fn overlapping_clips_crossfade_at_constant_power() {
    let mut p = Project::empty();
    // Two clips on one track overlap by one beat (960 ticks).
    clip(&mut p, 0, 3840, AudioEdit::default());
    clip(&mut p, 2880, 3840, AudioEdit::default());
    let song = SongSnapshot::compile_song(&p, |_| Some(dc(4.0)));
    let (a, b) = (&song.audio[0], &song.audio[1]);
    let (x0, x1) = (b.start_s, a.end_s);
    assert!((x1 - x0 - 0.5).abs() < 1e-9, "the overlap is the crossfade");
    for i in 0..=100 {
        let t = x0 + (x1 - x0) * f64::from(i) / 100.0;
        let power = a.fade_gain(t).powi(2) + b.fade_gain(t).powi(2);
        assert!((power - 1.0).abs() < 1e-9, "at {t}: {power}");
    }
    assert_eq!(a.fade_gain(x0 - 0.01), 1.0);
    assert_eq!(b.fade_gain(x1 + 0.01), 1.0);

    // A longer fade the user drew wins over the automatic one.
    let mut q = p.clone();
    let id = q.playlist.clips[1].id;
    if let Some(ClipKind::Audio { edit, .. }) = q.playlist.clip_mut(id).map(|c| &mut c.kind) {
        edit.fade_in = Fade {
            len: 1920,
            curve: FadeCurve::Linear,
        };
    }
    let song = SongSnapshot::compile_song(&q, |_| Some(dc(4.0)));
    let b = &song.audio[1];
    assert!((b.fade_gain(b.start_s + 0.5) - 0.5).abs() < 1e-9);
}

#[test]
fn reverse_plays_the_sample_backwards() {
    let mut p = Project::empty();
    clip(
        &mut p,
        0,
        960,
        AudioEdit {
            reverse: true,
            ..AudioEdit::default()
        },
    );
    let ramp: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
    let sample = Arc::new(SampleData::mono(SR, ramp.clone()));
    let out = render(&p, &sample, Tick(0), 0.1);
    for i in 0..999 {
        assert!(
            (out[i] - ramp[999 - i]).abs() < 1e-5,
            "frame {i}: {}",
            out[i]
        );
    }
    assert!(out[1001..].iter().all(|&x| x == 0.0));
}

#[test]
fn starting_inside_a_clip_fades_in_instead_of_clicking() {
    let mut p = Project::empty();
    clip(&mut p, 0, 7680, AudioEdit::default());
    let out = render(&p, &dc(5.0), Tick(1920), 0.1);
    assert!(out[0].abs() < 0.01, "starts from silence: {}", out[0]);
    assert!((out[200] - 0.5).abs() < 1e-6, "full level after 2 ms");
    let step = out
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(step < 0.5 / 90.0, "{step}");
}

#[test]
fn a_one_shot_already_sounding_is_chased_at_its_offset() {
    // A sampler one-shot (a 2 s ramp) from a short note at tick 0. Starting playback 1 s in
    // must sound like having played from the start.
    let notes = vec![NoteSpan {
        on: 0,
        off: 240,
        slot: 0,
        key: 60,
        velocity: 1.0,
    }];
    let song = || SongSnapshot {
        length: 7680,
        repeat: false,
        events: vec![
            SongEvent {
                tick: 0,
                slot: 0,
                key: 60,
                kind: NoteKind::On { velocity: 1.0 },
            },
            SongEvent {
                tick: 240,
                slot: 0,
                key: 60,
                kind: NoteKind::Off,
            },
        ],
        notes: notes.clone(),
        ..SongSnapshot::default()
    };
    let ramp: Vec<f32> = (0..2 * SR as usize)
        .map(|i| i as f32 / (2.0 * SR as f32))
        .collect();
    let sample = Arc::new(SampleData::mono(SR, ramp));
    let play_from = |from: Tick, seconds: f64| {
        let (mut h, mut p) = create(EngineConfig {
            sample_rate: SR,
            out_channels: 2,
        });
        h.send(EngineCommand::SetChannelSample {
            slot: 0,
            sample: Some(Arc::clone(&sample)),
        })
        .unwrap();
        h.send(EngineCommand::SetChannelParams {
            slot: 0,
            params: Box::new(ChannelParams {
                gain: 1.0,
                ..ChannelParams::default()
            }),
        })
        .unwrap();
        h.send(EngineCommand::SetTempoMap(Box::new(TempoMap::constant(
            120.0,
        ))))
        .unwrap();
        h.send(EngineCommand::SetSong(Box::new(song()))).unwrap();
        h.send(EngineCommand::SetMetronome(false)).unwrap();
        h.send(EngineCommand::Locate(from)).unwrap();
        h.send(EngineCommand::Play).unwrap();
        let mut out = vec![0.0; 2 * (seconds * f64::from(SR)) as usize];
        for chunk in out.chunks_mut(2 * 256) {
            p.process(chunk);
        }
        out.chunks_exact(2).map(|f| f[0]).collect::<Vec<f32>>()
    };
    let whole = play_from(Tick(0), 1.5);
    // 120 BPM: tick 1920 is 1 s.
    let chased = play_from(Tick(1920), 0.5);
    let sr = SR as usize;
    assert!(
        whole[sr + 1000] > 0.1,
        "the one-shot is still sounding at 1 s"
    );
    // Past the voice's short attack, the chased voice is the same sound at the same place.
    for k in (2000..sr / 2).step_by(97) {
        assert!(
            (chased[k] - whole[sr + k]).abs() < 1e-4,
            "frame {k}: {} vs {}",
            chased[k],
            whole[sr + k]
        );
    }
}
