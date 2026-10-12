//! Cost of audio clip editing (Prompt 14): one second at 48 kHz in 256-frame callbacks with
//! eight overlapping audio clips, plain versus every clip reversed with S-curve fades and an
//! automatic crossfade at each overlap. The difference is what fades and reverse add.

use std::sync::Arc;

use criterion::{criterion_group, criterion_main, Criterion};
use gt_core::{AudioEdit, ClipKind, Fade, FadeCurve, Project, SampleData, SampleSource};
use gt_engine::{create, AudioProcessor, EngineCommand, EngineConfig, SongSnapshot};
use std::hint::black_box;

const SR: u32 = 48_000;
const BLOCK: usize = 256;

fn rig(edited: bool) -> AudioProcessor {
    let mut p = Project::empty();
    let t = p.playlist.tracks[0].id;
    for k in 0..8 {
        let edit = if edited {
            AudioEdit {
                fade_in: Fade {
                    len: 240,
                    curve: FadeCurve::SCurve,
                },
                fade_out: Fade {
                    len: 240,
                    curve: FadeCurve::SCurve,
                },
                reverse: true,
                ..AudioEdit::default()
            }
        } else {
            AudioEdit::default()
        };
        // 120 BPM: clips of 0.25 s every 0.125 s, so each overlaps the next by half.
        p.playlist.add_clip(
            t,
            k * 240,
            480,
            ClipKind::Audio {
                source: SampleSource::BuiltIn(gt_core::BuiltInSample::Kick),
                gain: 1.0,
                gaps: Vec::new(),
                takes: Vec::new(),
                edit,
            },
        );
    }
    let noise: Vec<f32> = (0..SR)
        .map(|i| ((i * 7919) % 101) as f32 / 101.0 - 0.5)
        .collect();
    let sample = Arc::new(SampleData::mono(SR, noise));
    let song = SongSnapshot::compile_song(&p, |_| Some(Arc::clone(&sample)));
    let (mut h, proc_) = create(EngineConfig {
        sample_rate: SR,
        out_channels: 2,
    });
    h.send(EngineCommand::SetSong(Box::new(song))).unwrap();
    h.send(EngineCommand::SetMetronome(false)).unwrap();
    h.send(EngineCommand::Play).unwrap();
    // The handle must outlive the processor's command reads; leaking it keeps the bench simple.
    std::mem::forget(h);
    proc_
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("audio clips, 1 s");
    for (name, edited) in [("plain", false), ("faded + reversed", true)] {
        g.bench_function(name, |b| {
            let mut buf = vec![0.0; 2 * BLOCK];
            b.iter_batched_ref(
                || rig(edited),
                |p| {
                    for _ in 0..SR as usize / BLOCK {
                        p.process(&mut buf);
                    }
                    black_box(&buf);
                },
                criterion::BatchSize::LargeInput,
            );
        });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
