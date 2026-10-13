//! Mixer effect benchmarks: one second of stereo audio at 48 kHz in 64-frame blocks (the
//! engine's render quantum), per effect at its default settings.

use criterion::{criterion_group, criterion_main, Criterion};
use gt_dsp::fx::{
    Analyzer, Bitcrusher, Chorus, Compressor, ConvolutionReverb, Delay, Distortion, Effect,
    Flanger, FxContext, Gate, Limiter, MultiFilter, Multiband, ParamEq, Phaser, PitchShifter,
    Reverb, SoftClipper, StereoWidth, Stutter, TransientShaper, Vocoder,
};
use std::hint::black_box;

const SR: f32 = 48_000.0;

fn run(fx: &mut dyn Effect, l: &mut [f32], r: &mut [f32]) {
    let ctx = FxContext {
        bpm: 120.0,
        sidechain: None,
        beats: None,
    };
    for (a, b) in l.chunks_mut(64).zip(r.chunks_mut(64)) {
        fx.process(a, b, &ctx);
    }
}

fn effects(c: &mut Criterion) {
    let input: Vec<f32> = (0..SR as usize)
        .map(|i| (i as f32 * 0.031).sin() * 0.5 + (i as f32 * 0.0071).sin() * 0.3)
        .collect();
    let mut eq_all = ParamEq::new(SR);
    // Every band active: the EQ's worst case.
    for b in 0..8 {
        eq_all.set_param(b * 4, 1.0);
        eq_all.set_param(b * 4 + 2, 3.0);
    }
    let mut multiband_4 = Multiband::new(SR);
    multiband_4.set_param(0, 1.0);
    // The longest generated response: a 3.5 s hall.
    let mut hall = ConvolutionReverb::new(SR);
    hall.set_param(0, 1.0);
    hall.prepare(None);
    let mut correct = PitchShifter::new(SR);
    correct.set_param(0, 1.0);
    let mut vocoder_32 = Vocoder::new(SR);
    vocoder_32.set_param(0, 4.0);
    let cases: Vec<(&str, Box<dyn Effect>)> = vec![
        ("eq_8_bands", Box::new(eq_all)),
        ("compressor", Box::new(Compressor::new(SR))),
        ("delay", Box::new(Delay::new(SR))),
        ("reverb", Box::new(Reverb::new(SR))),
        ("chorus", Box::new(Chorus::new(SR))),
        ("distortion", Box::new(Distortion::new(SR))),
        ("limiter", Box::new(Limiter::new(SR))),
        ("width", Box::new(StereoWidth::new(SR))),
        ("filter", Box::new(MultiFilter::new(SR))),
        ("gate", Box::new(Gate::new(SR))),
        ("flanger", Box::new(Flanger::new(SR))),
        ("phaser", Box::new(Phaser::new(SR))),
        ("bitcrusher", Box::new(Bitcrusher::new(SR))),
        ("clipper_2x", Box::new(SoftClipper::new(SR))),
        ("transient", Box::new(TransientShaper::new(SR))),
        ("multiband_4_bands", Box::new(multiband_4)),
        ("analyzer", Box::new(Analyzer::new(SR))),
        ("convolution_hall", Box::new(hall)),
        ("pitch_shift", Box::new(PitchShifter::new(SR))),
        ("pitch_correct", Box::new(correct)),
        ("vocoder_32_bands", Box::new(vocoder_32)),
        ("stutter", Box::new(Stutter::new(SR))),
    ];
    let mut group = c.benchmark_group("fx_1s_48k");
    for (name, mut fx) in cases {
        let (mut l, mut r) = (input.clone(), input.clone());
        group.bench_function(name, |b| {
            b.iter(|| {
                l.copy_from_slice(&input);
                r.copy_from_slice(&input);
                run(fx.as_mut(), &mut l, &mut r);
                black_box(l[100] + r[200])
            })
        });
    }
    group.finish();
}

criterion_group!(benches, effects);
criterion_main!(benches);
