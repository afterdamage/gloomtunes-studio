//! Proves that `AudioProcessor::process` never allocates or frees memory, including while it
//! applies commands, plays the metronome through loop wraps and swaps tempo maps.
//!
//! `assert_no_alloc` installs a global allocator for this test binary only; any allocation or
//! deallocation inside `assert_no_alloc(|| ...)` aborts the test process.

use assert_no_alloc::{assert_no_alloc, AllocDisabler};
use gt_core::{TempoMap, Tick, TimeSig};
use gt_engine::{create, EngineCommand, EngineConfig, LoopRegion};

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
