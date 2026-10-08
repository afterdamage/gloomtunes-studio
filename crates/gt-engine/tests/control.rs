//! Automation curves and modulators, end to end: the project compiled, sent to the engine and
//! rendered.

use std::sync::Arc;

use gt_core::{
    AutoPoint, Automation, ClipKind, Curve, LfoRate, LfoShape, ModSourceKind, ParamId, Project,
    SampleData, SampleSource, StripParam, MASTER_VOLUME,
};
use gt_engine::{create, EngineCommand, EngineConfig, ModPlan, SongSnapshot};

const SR: u32 = 48_000;
/// 120 BPM: a beat is half a second, 960 ticks.
const BEAT: usize = SR as usize / 2;

/// Renders `seconds` of the left channel of `project` in song mode, with every audio clip
/// playing a DC level of 0.5. `block` is the device buffer size.
fn render(project: &Project, seconds: f64, block: usize) -> Vec<f32> {
    let (mut h, mut p) = create(EngineConfig {
        sample_rate: SR,
        out_channels: 2,
    });
    let dc = Arc::new(SampleData::mono(SR, vec![0.5; 20 * SR as usize]));
    let song = SongSnapshot::compile_song(project, |_| Some(Arc::clone(&dc)));
    h.send(EngineCommand::SetMetronome(false)).unwrap();
    h.send(EngineCommand::SetSong(Box::new(song))).unwrap();
    h.send(EngineCommand::SetModulation(Box::new(ModPlan::compile(
        project,
    ))))
    .unwrap();
    h.send(EngineCommand::Play).unwrap();
    let mut out = vec![0.0; 2 * (seconds * f64::from(SR)) as usize];
    for chunk in out.chunks_mut(2 * block) {
        p.process(chunk);
    }
    out.chunks_exact(2).map(|f| f[0]).collect()
}

/// A project with DC audio on track 1 into `strip` for `bars` bars.
fn dc_project(strip: usize, bars: i64) -> Project {
    let mut p = Project::empty();
    p.playlist.tracks[0].insert = strip;
    let t = p.playlist.tracks[0].id;
    p.playlist.add_clip(
        t,
        0,
        bars * 3840,
        ClipKind::Audio {
            source: SampleSource::BuiltIn(gt_core::BuiltInSample::Kick),
            gain: 1.0,
        },
    );
    p
}

fn automate(p: &mut Project, target: ParamId, start: i64, len: i64, points: Vec<AutoPoint>) {
    let t = p.playlist.tracks[1].id;
    p.playlist.add_clip(
        t,
        start,
        len,
        ClipKind::Automation(Automation { target, points }),
    );
}

#[test]
fn fader_automation_moves_sample_by_sample() {
    // Master fader from silence to unity over one bar, then held.
    let mut p = dc_project(0, 2);
    let unity = MASTER_VOLUME.info().to_normalized(1.0);
    automate(
        &mut p,
        MASTER_VOLUME,
        0,
        3840,
        vec![AutoPoint::new(0, 0.0), AutoPoint::new(3840, unity)],
    );
    let out = render(&p, 2.5, 441);
    let info = MASTER_VOLUME.info();
    let mut worst = 0.0_f32;
    for (i, &x) in out.iter().enumerate().take(4 * BEAT).skip(64) {
        let want = 0.5 * info.from_normalized(unity * i as f32 / (4 * BEAT) as f32);
        worst = worst.max((x - want).abs());
    }
    // Gains ramp between control points 32 frames apart: the cubic fader law is followed
    // with straight pieces, far below anything audible.
    assert!(worst < 2e-4, "{worst}");
    // No steps: consecutive samples differ by less than the steepest slope allows.
    let step = out
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(step < 1e-4, "{step}");
    assert!((out[4 * BEAT + BEAT / 2] - 0.5).abs() < 1e-5);
}

#[test]
fn curve_types_shape_the_automation() {
    let unity = MASTER_VOLUME.info().to_normalized(1.0);
    let level_at_half = |curve: Curve| {
        let mut p = dc_project(0, 2);
        automate(
            &mut p,
            MASTER_VOLUME,
            0,
            3840,
            vec![
                AutoPoint {
                    at: 0,
                    value: 0.0,
                    curve,
                },
                AutoPoint::new(3840, unity),
            ],
        );
        let out = render(&p, 1.2, 512);
        // A quarter of the way through the segment.
        out[BEAT]
    };
    let gain = |t: f32| 0.5 * MASTER_VOLUME.info().from_normalized(t * unity);
    let close = |a: f32, b: f32| (a - b).abs() < 2e-3;
    assert_eq!(level_at_half(Curve::Hold), 0.0);
    assert!(close(level_at_half(Curve::Linear), gain(0.25)));
    assert!(close(
        level_at_half(Curve::Smooth),
        gain(Curve::Smooth.shape(0.25))
    ));
    assert!(close(
        level_at_half(Curve::Bezier(0.7)),
        gain(Curve::Bezier(0.7).shape(0.25))
    ));
    assert!(level_at_half(Curve::Bezier(0.7)) > level_at_half(Curve::Linear));
}

#[test]
fn a_synced_lfo_swings_the_fader_every_beat() {
    // Half-way fader, a sine LFO at 1/4 note with amount 0.25: louder on the first quarter of
    // each beat than on the third.
    let mut p = dc_project(0, 4);
    let half = 0.5;
    MASTER_VOLUME.set(&mut p, MASTER_VOLUME.info().from_normalized(half));
    let id = p
        .add_modulator(
            MASTER_VOLUME,
            ModSourceKind::Lfo {
                shape: LfoShape::Sine,
                rate: LfoRate::Sync(5),
                phase: 0.0,
            },
        )
        .unwrap();
    p.modulator_mut(id).unwrap().amount = 0.25;
    let out = render(&p, 3.0, 300);
    let gain = |t: f32| 0.5 * MASTER_VOLUME.info().from_normalized(t);
    for beat in 1..5 {
        let peak = out[beat * BEAT + BEAT / 4];
        let dip = out[beat * BEAT + 3 * BEAT / 4];
        assert!(
            (peak - gain(half + 0.25)).abs() < 2e-3,
            "beat {beat}: {peak}"
        );
        assert!((dip - gain(half - 0.25)).abs() < 2e-3, "beat {beat}: {dip}");
    }
}

#[test]
fn modulation_adds_to_automation() {
    // Automation holds the fader at a quarter; a square LFO at 1 Hz adds ±0.2 on top.
    let mut p = dc_project(0, 4);
    automate(
        &mut p,
        MASTER_VOLUME,
        0,
        4 * 3840,
        vec![AutoPoint::new(0, 0.25)],
    );
    let id = p
        .add_modulator(
            MASTER_VOLUME,
            ModSourceKind::Lfo {
                shape: LfoShape::Square,
                rate: LfoRate::Hz(1.0),
                phase: 0.0,
            },
        )
        .unwrap();
    p.modulator_mut(id).unwrap().amount = 0.2;
    let out = render(&p, 2.0, 512);
    let gain = |t: f32| 0.5 * MASTER_VOLUME.info().from_normalized(t);
    assert!((out[SR as usize / 4] - gain(0.45)).abs() < 1e-3);
    assert!((out[3 * SR as usize / 4] - gain(0.05)).abs() < 1e-3);
}

#[test]
fn an_envelope_follower_ducks_one_strip_under_another() {
    // Insert 1 gets DC for the first beat of each bar (a "kick"); insert 2 gets DC all along.
    // A follower of insert 1 pulls insert 2's fader down with amount -1.
    let mut p = Project::empty();
    let (kick, pad) = (p.playlist.tracks[0].id, p.playlist.tracks[1].id);
    p.playlist.tracks[0].insert = 1;
    p.playlist.tracks[1].insert = 2;
    let src = || ClipKind::Audio {
        source: SampleSource::BuiltIn(gt_core::BuiltInSample::Kick),
        gain: 1.0,
    };
    for bar in 0..3 {
        p.playlist.add_clip(kick, bar * 3840, 960, src());
    }
    p.playlist.add_clip(pad, 0, 3 * 3840, src());
    let pad_fader = ParamId::Strip {
        strip: 2,
        param: StripParam::Volume,
    };
    let id = p
        .add_modulator(pad_fader, ModSourceKind::default_follower(1))
        .unwrap();
    p.modulator_mut(id).unwrap().amount = -1.0;
    // The output is the kick (0.5 while it plays) plus the ducked pad.
    let both = render(&p, 4.0, 256);
    p.modulators.clear();
    let unducked = render(&p, 4.0, 256);
    let bar = 4 * BEAT;
    // While the kick plays (second half of its beat), the pad is pulled down hard...
    let during = both[bar + BEAT * 3 / 4] - 0.5;
    assert!(during < 0.1, "{during}");
    // ...and between kicks it comes back to its normal level.
    let between = both[bar + 3 * BEAT];
    assert!(
        (between - unducked[bar + 3 * BEAT]).abs() < 0.02,
        "{between}"
    );
}

#[test]
fn modulated_renders_do_not_depend_on_the_device_buffer() {
    // Offline export must match real time: an S&H LFO, a synced triangle and a follower,
    // rendered with two block sizes, give identical output.
    let mut p = dc_project(1, 4);
    p.add_modulator(
        MASTER_VOLUME,
        ModSourceKind::Lfo {
            shape: LfoShape::SampleHold,
            rate: LfoRate::Hz(7.0),
            phase: 0.0,
        },
    );
    p.add_modulator(
        ParamId::strip_pan(1),
        ModSourceKind::Lfo {
            shape: LfoShape::Triangle,
            rate: LfoRate::Sync(7),
            phase: 0.3,
        },
    );
    p.add_modulator(ParamId::strip_volume(1), ModSourceKind::default_follower(1));
    let a = render(&p, 3.0, 441);
    let b = render(&p, 3.0, 64);
    assert_eq!(a, b);
    assert!(a.iter().any(|x| x.abs() > 0.05));
}
