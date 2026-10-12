//! Cost of the stretch engine (Prompt 15): one second of stereo output at 48 kHz, stretched
//! to 1.25 times the length with a pitch shift, as the cache build and the real-time fallback
//! run it.

use criterion::{criterion_group, criterion_main, Criterion};
use gt_dsp::stretch::{render, Signalsmith, TimeMap};
use std::hint::black_box;

const SR: u32 = 48_000;

fn bench(c: &mut Criterion) {
    let src: Vec<f32> = (0..SR as usize)
        .map(|i| ((i * 7919) % 101) as f32 / 101.0 - 0.5)
        .collect();
    c.bench_function("stretch 1 s stereo, x1.25, -3 st", |b| {
        b.iter(|| {
            let map = TimeMap::linear(0.8, SR as usize);
            black_box(render(
                Box::new(Signalsmith::new(SR)),
                &src,
                &src,
                map,
                -3.0,
                SR as usize,
            ))
        });
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
