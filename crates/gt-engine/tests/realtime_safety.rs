//! Proves that `AudioProcessor::process` never allocates or frees memory, including while it
//! applies commands, plays the metronome through loop wraps and swaps tempo maps.
//!
//! `assert_no_alloc` installs a global allocator for this test binary only; any allocation or
//! deallocation inside `assert_no_alloc(|| ...)` aborts the test process.

use assert_no_alloc::{assert_no_alloc, AllocDisabler};
use std::sync::Arc;

use gt_core::{Project, SampleData, TempoMap, Tick, TimeSig, MAX_CHANNELS};
use gt_engine::{create, ChannelParams, EngineCommand, EngineConfig, LoopRegion, SongSnapshot};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn process_does_not_allocate() {
    for (sr, ch, block) in [(44_100, 2, 441), (48_000, 1, 64), (96_000, 6, 1024)] {
        let (mut h, mut p) = create(EngineConfig {
            sample_rate: sr,
            out_channels: ch,
        });
        let mut buf = vec![0.0_f32; block * ch];
        let mut run = |p: &mut gt_engine::AudioProcessor, n: usize| {
            for _ in 0..n {
                assert_no_alloc(|| p.process(&mut buf));
            }
        };
        run(&mut p, 10);

        h.send(EngineCommand::SetTestTone(true)).unwrap();
        h.send(EngineCommand::SetTimeSig(TimeSig::new(7, 8)))
            .unwrap();
        h.send(EngineCommand::SetLoop(LoopRegion {
            start: Tick(960),
            end: Tick(960 * 3),
            enabled: true,
        }))
        .unwrap();
        h.send(EngineCommand::Play).unwrap();
        run(&mut p, 200);

        // Tempo swaps: the Box is allocated here, the old one is freed by collect_garbage().
        for bpm in [90.0, 300.0, 61.5] {
            h.send(EngineCommand::SetTempoMap(Box::new(TempoMap::constant(
                bpm,
            ))))
            .unwrap();
            run(&mut p, 50);
            assert_eq!(h.collect_garbage(), 1);
        }

        // Samples, channel settings, a pattern, auditions and the preview voice. Every payload
        // is allocated here and comes back through the garbage queue.
        let kick = Arc::new(SampleData::mono(sr, gt_dsp::drums::kick(sr as f32)));
        let hat = Arc::new(SampleData::mono(sr, gt_dsp::drums::hat(sr as f32)));
        let mut project = Project::demo();
        project.swing = 0.3;
        for slot in 0..MAX_CHANNELS as u16 {
            let sample = if slot % 2 == 0 { &kick } else { &hat };
            h.send(EngineCommand::SetChannelSample {
                slot,
                sample: Some(Arc::clone(sample)),
            })
            .unwrap();
            for _ in 0..10 {
                h.send(EngineCommand::SetChannelParams {
                    slot,
                    params: Box::new(ChannelParams {
                        gain: 0.5,
                        pan: -0.5,
                        looped: slot % 3 == 0,
                        ..ChannelParams::default()
                    }),
                })
                .unwrap();
            }
        }
        h.send(EngineCommand::SetSong(Box::new(SongSnapshot::compile(
            &project,
        ))))
        .unwrap();
        // 704 payload commands, more than the garbage queue holds (512): the engine must wait
        // for collection rather than free anything, and must not lose a command.
        run(&mut p, 200);
        let mut collected = 0;
        for _ in 0..8 {
            collected += h.collect_garbage();
            run(&mut p, 40);
        }
        collected += h.collect_garbage();
        assert_eq!(collected, MAX_CHANNELS * 10, "every params box comes back");
        for k in 0..40 {
            h.send(EngineCommand::NoteOn {
                slot: 5,
                key: 48 + k,
                velocity: 0.9,
            })
            .unwrap();
        }
        h.send(EngineCommand::NoteOff { slot: 5, key: 50 }).unwrap();
        h.send(EngineCommand::PreviewSample(Some(Arc::clone(&hat))))
            .unwrap();
        run(&mut p, 100);
        project.swing = 0.0;
        project.current_pattern_mut().steps = 32;
        h.send(EngineCommand::SetSong(Box::new(SongSnapshot::compile(
            &project,
        ))))
        .unwrap();
        h.send(EngineCommand::SetChannelSample {
            slot: 0,
            sample: None,
        })
        .unwrap();
        run(&mut p, 100);
        h.collect_garbage();

        h.send(EngineCommand::Pause).unwrap();
        h.send(EngineCommand::Locate(Tick(12_345))).unwrap();
        h.send(EngineCommand::Play).unwrap();
        h.send(EngineCommand::FadeOut).unwrap();
        run(&mut p, 200);
        h.send(EngineCommand::Stop).unwrap();
        run(&mut p, 10);
        assert!(h
            .telemetry()
            .silent
            .load(std::sync::atomic::Ordering::Relaxed));
    }
}
