//! Proves that `AudioProcessor::process` never allocates or frees memory.
//!
//! `assert_no_alloc` installs a global allocator for this test binary only; any allocation inside
//! `assert_no_alloc(|| ...)` aborts the test process.

use assert_no_alloc::{assert_no_alloc, AllocDisabler};
use gt_engine::{create, EngineConfig};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn process_does_not_allocate() {
    for (sr, ch) in [(44_100, 2), (48_000, 1), (96_000, 6)] {
        let (handle, mut processor) = create(EngineConfig {
            sample_rate: sr,
            out_channels: ch,
        });
        let mut buf = vec![0.0_f32; 1024 * ch];
        assert_no_alloc(|| processor.process(&mut buf));
        handle.start_tone();
        for _ in 0..200 {
            assert_no_alloc(|| processor.process(&mut buf));
        }
        handle.stop_tone();
        for _ in 0..200 {
            assert_no_alloc(|| processor.process(&mut buf));
        }
        assert!(handle
            .telemetry()
            .silent
            .load(std::sync::atomic::Ordering::Relaxed));
    }
}
