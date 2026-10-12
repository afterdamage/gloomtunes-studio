//! Cost of transient and tempo detection (Prompt 15 part 3): ten seconds of mono audio at
//! 48 kHz, as the clip menu runs them on demand.

use criterion::{criterion_group, criterion_main, Criterion};
use gt_dsp::onsets::{detect, tempo};
use std::hint::black_box;

const SR: u32 = 48_000;

fn bench(c: &mut Criterion) {
    let x: Vec<f32> = (0..10 * SR as usize)
        .map(|i| ((i * 7919) % 101) as f32 / 101.0 - 0.5)
        .collect();
    c.bench_function("onsets 10 s mono", |b| {
        b.iter(|| black_box(detect(&x, SR, 0.5)))
    });
    c.bench_function("tempo 10 s mono", |b| b.iter(|| black_box(tempo(&x, SR))));
}

criterion_group!(benches, bench);
criterion_main!(benches);
