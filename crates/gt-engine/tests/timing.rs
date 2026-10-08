//! End-to-end timing: metronome clicks rendered through `AudioProcessor::process` start on the
//! exact expected frame, for any device buffer size.
//!
//! A click's first sample is non-zero (see `gt_dsp::Click`), so an onset is the first non-zero
//! sample after silence. Clicks last 60 ms, shorter than any beat used here.

use gt_core::{TempoMap, TempoPoint, Tick, PPQ};
use gt_engine::{create, EngineCommand, EngineConfig, LoopRegion};

/// Renders `seconds` of mono output, pulling it from the engine in blocks of the given sizes
/// (cycled), after sending `setup` commands.
fn render(sr: u32, seconds: f64, blocks: &[usize], setup: Vec<EngineCommand>) -> Vec<f32> {
    let (mut h, mut p) = create(EngineConfig {
        sample_rate: sr,
        out_channels: 1,
    });
    for c in setup {
        h.send(c).unwrap();
    }
    let total = (seconds * f64::from(sr)) as usize;
    let mut out = vec![0.0_f32; total];
    let mut pos = 0;
    for &b in blocks.iter().cycle() {
        if pos >= total {
            break;
        }
        let n = b.min(total - pos);
        p.process(&mut out[pos..pos + n]);
        pos += n;
    }
    out
}

fn onsets(buf: &[f32]) -> Vec<usize> {
    (0..buf.len())
        .filter(|&i| buf[i] != 0.0 && (i == 0 || buf[i - 1] == 0.0))
        .collect()
}

fn frame(seconds: f64, sr: u32) -> usize {
    (seconds * f64::from(sr) + 0.5).floor() as usize
}

#[test]
fn clicks_start_on_the_exact_frame_at_44k1_48k_96k() {
    for sr in [44_100, 48_000, 96_000] {
        for bpm in [120.0, 137.0] {
            let out = render(
                sr,
                10.0,
                &[256],
                vec![
                    EngineCommand::SetTempoMap(Box::new(TempoMap::constant(bpm))),
                    EngineCommand::Play,
                ],
            );
            let got = onsets(&out);
            let want: Vec<usize> = (0..)
                .map(|k| frame(k as f64 * 60.0 / bpm, sr))
                .take_while(|&f| f < out.len())
                .collect();
            assert_eq!(got, want, "sr {sr} bpm {bpm}");
        }
    }
}

#[test]
fn clicks_follow_a_tempo_change() {
    let sr = 48_000;
    // 120 BPM for one bar (2 s), then 100 BPM.
    let map = TempoMap::new(&[
        TempoPoint {
            at: Tick(0),
            bpm: 120.0,
        },
        TempoPoint {
            at: Tick(4 * PPQ),
            bpm: 100.0,
        },
    ])
    .unwrap();
    let out = render(
        sr,
        8.0,
        &[512],
        vec![
            EngineCommand::SetTempoMap(Box::new(map)),
            EngineCommand::Play,
        ],
    );
    let got = onsets(&out);
    let want: Vec<usize> = (0..)
        .map(|k| {
            if k < 4 {
                frame(k as f64 * 0.5, sr)
            } else {
                frame(2.0 + (k - 4) as f64 * 0.6, sr)
            }
        })
        .take_while(|&f| f < out.len())
        .collect();
    assert_eq!(got, want);
}

#[test]
fn device_buffer_size_does_not_change_the_output() {
    let setup = || {
        vec![
            EngineCommand::SetTempoMap(Box::new(TempoMap::constant(133.0))),
            EngineCommand::SetLoop(LoopRegion {
                start: Tick(PPQ),
                end: Tick(PPQ * 6),
                enabled: true,
            }),
            EngineCommand::Play,
        ]
    };
    let reference = render(44_100, 6.0, &[64], setup());
    for blocks in [&[441][..], &[1024], &[7, 1000, 13, 256], &[1]] {
        let out = render(44_100, 6.0, blocks, setup());
        assert!(out == reference, "blocks {blocks:?} differ");
    }
}

#[test]
fn loop_wrap_restarts_the_bar_on_time() {
    let sr = 96_000;
    // Loop beats 1..3 (ticks 0..1920) at 120 BPM: a click every 0.5 s, wrap every 1 s.
    let out = render(
        sr,
        5.0,
        &[480],
        vec![
            EngineCommand::SetLoop(LoopRegion {
                start: Tick(0),
                end: Tick(2 * PPQ),
                enabled: true,
            }),
            EngineCommand::Play,
        ],
    );
    let got = onsets(&out);
    let want: Vec<usize> = (0..10).map(|k| k * sr as usize / 2).collect();
    assert_eq!(got, want);
}

/// Commands that set up channel 0 with a single-sample click (an impulse) and a pattern.
fn step_setup(bpm: f64, steps: &[u16], swing: f32, sr: u32) -> Vec<EngineCommand> {
    use gt_engine::{ChannelParams, SongSnapshot};
    let mut project = gt_core::Project::empty();
    let c = project.add_channel("imp", None).unwrap();
    for &s in steps {
        project.current_pattern_mut().toggle_step(c, s);
    }
    project.swing = swing;
    let mut impulse = vec![0.0; 2000];
    impulse[0] = 1.0;
    vec![
        EngineCommand::SetMetronome(false),
        EngineCommand::SetTempoMap(Box::new(TempoMap::constant(bpm))),
        EngineCommand::SetChannelSample {
            slot: 0,
            sample: Some(std::sync::Arc::new(gt_core::SampleData::mono(sr, impulse))),
        },
        EngineCommand::SetChannelParams {
            slot: 0,
            params: Box::new(ChannelParams {
                gain: 1.0,
                ..ChannelParams::default()
            }),
        },
        EngineCommand::SetSong(Box::new(SongSnapshot::compile(&project))),
        EngineCommand::Play,
    ]
}

#[test]
fn steps_start_on_the_exact_frame_with_swing_and_any_buffer_size() {
    let steps = [0, 3, 6, 9, 10, 15];
    let swing = 0.5;
    for sr in [44_100, 48_000, 96_000] {
        let bpm = 133.0;
        let step_s = 60.0 / bpm / 4.0;
        let mut want = Vec::new();
        for rep in 0..3 {
            for &s in &steps {
                let delay = if s % 2 == 1 { swing as f64 * 0.5 } else { 0.0 };
                // Swing is rounded to whole ticks: 0.5 * 120 = 60 ticks, exact here.
                let t = (rep * 16 + i64::from(s)) as f64 * step_s + delay * step_s;
                want.push(frame(t, sr));
            }
        }
        let seconds = 3.0 * 16.0 * step_s;
        let reference = render(sr, seconds, &[64], step_setup(bpm, &steps, swing, sr));
        assert_eq!(onsets(&reference), want, "sr {sr}");
        for blocks in [&[1usize][..], &[37, 512, 3], &[1024]] {
            let out = render(sr, seconds, blocks, step_setup(bpm, &steps, swing, sr));
            assert_eq!(out, reference, "sr {sr} blocks {blocks:?}");
        }
    }
}

#[test]
fn steps_restart_on_the_loop_start_frame() {
    let sr = 48_000;
    let mut setup = step_setup(120.0, &[0, 8], 0.0, sr);
    // Loop the first half bar (steps 0..8): step 8 must never sound.
    setup.insert(
        0,
        EngineCommand::SetLoop(LoopRegion {
            start: Tick(0),
            end: Tick(2 * PPQ),
            enabled: true,
        }),
    );
    let out = render(sr, 4.0, &[100], setup);
    // Half a bar at 120 BPM is 1 s.
    assert_eq!(onsets(&out), vec![0, 48_000, 96_000, 144_000]);
}
