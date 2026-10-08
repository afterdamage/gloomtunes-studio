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
