//! The loopback measurement's analysis (runs once on the UI thread, not in real time): the
//! cross-correlation search over a 48 kHz capture.

use criterion::{criterion_group, criterion_main, Criterion};
use gt_dsp::latency::{measure_delay, signal_at, total_frames};
use std::hint::black_box;

fn latency(c: &mut Criterion) {
    let sr = 48_000.0;
    let capture: Vec<f32> = (0..total_frames(sr))
        .map(|i| i.checked_sub(700).map_or(0.0, |j| signal_at(j, sr) * 0.4))
        .collect();
    c.bench_function("measure_delay_48k", |b| {
        b.iter(|| measure_delay(black_box(&capture), sr))
    });
}

criterion_group!(benches, latency);
criterion_main!(benches);
