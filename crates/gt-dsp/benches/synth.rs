//! Gloom Synth and building-block benchmarks.
//!
//! Run with `cargo bench -p gt-dsp`. Each benchmark renders one second of audio at 48 kHz, so
//! the time per iteration divided by one second is the CPU share of one core.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use gt_dsp::synth::{GloomSynth, SynthSettings};
use gt_dsp::{blep_sample, ladder_g, ladder_k, Ladder, Phase, Wave};

const SR: f32 = 48_000.0;
const SECOND: usize = 48_000;
const BLOCK: usize = 64;

fn render_second(s: &mut GloomSynth) {
    let (mut l, mut r) = ([0.0_f32; BLOCK], [0.0_f32; BLOCK]);
    for _ in 0..SECOND / BLOCK {
        l.fill(0.0);
        r.fill(0.0);
        s.render(&mut l, &mut r);
        black_box((&l, &r));
    }
}

fn synth(c: &mut Criterion) {
    let mut g = c.benchmark_group("gloom_synth_1s");
    g.sample_size(20);
    for (name, unison, notes) in [
        ("1_voice", 1u8, 1u8),
        ("16_voices", 1, 16),
        ("16_voices_unison7", 7, 16),
    ] {
        let settings = SynthSettings {
            unison,
            ..SynthSettings::default()
        };
        g.bench_function(name, |b| {
            b.iter_batched_ref(
                || {
                    let mut s = GloomSynth::new(SR);
                    s.set_settings(&settings);
                    for k in 0..notes {
                        s.note_on(40 + k * 2, 0.9, u64::from(k) + 1);
                    }
                    s
                },
                render_second,
                criterion::BatchSize::LargeInput,
            );
        });
    }
    g.finish();
}

fn parts(c: &mut Criterion) {
    let mut g = c.benchmark_group("parts_1s");
    g.bench_function("polyblep_saw", |b| {
        b.iter(|| {
            let mut p = Phase::default();
            let mut acc = 0.0;
            for _ in 0..SECOND {
                let t = p.advance(0.01);
                acc += blep_sample(Wave::Saw, t, 0.01, 0.5);
            }
            black_box(acc)
        });
    });
    g.bench_function("ladder", |b| {
        let (gg, k) = (ladder_g(1000.0, SR), ladder_k(0.7));
        b.iter(|| {
            let mut f = Ladder::default();
            let mut acc = 0.0;
            for i in 0..SECOND {
                acc += f.process(if i % 100 < 50 { 1.0 } else { -1.0 }, gg, k, 2.0);
            }
            black_box(acc)
        });
    });
    g.finish();
}

criterion_group!(benches, synth, parts);
criterion_main!(benches);
