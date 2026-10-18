//! Cost of audio input in the engine (Prompt 13): one second at 48 kHz in 256-frame
//! callbacks, idle versus reading an 8-channel input, monitoring four routes and recording
//! four stereo tracks. The difference is what recording adds to a callback.

use criterion::{criterion_group, criterion_main, Criterion};
use gt_core::{AudioInput, MonitorMode};
use gt_engine::{
    create, AudioProcessor, EngineCommand, EngineConfig, EngineHandle, InputFeed, InputRoute,
    InputRoutes, RecordSink,
};
use std::hint::black_box;

const SR: u32 = 48_000;
const BLOCK: usize = 256;
const IN_CH: usize = 8;

struct Rig {
    h: EngineHandle,
    p: AudioProcessor,
    input: rtrb::Producer<f32>,
    rec: Option<rtrb::Consumer<f32>>,
    buf: Vec<f32>,
}

fn rig(recording: bool) -> Rig {
    let (mut h, p) = create(EngineConfig {
        sample_rate: SR,
        out_channels: 2,
    });
    let (input, samples) = rtrb::RingBuffer::new(SR as usize * IN_CH);
    let mut rec = None;
    if recording {
        h.send(EngineCommand::SetInput(Some(Box::new(InputFeed {
            samples,
            channels: IN_CH,
            prime_frames: BLOCK,
            max_frames: 4 * BLOCK,
        }))))
        .unwrap();
        let pairs: Vec<AudioInput> = (0..4).map(|k| AudioInput::stereo(2 * k)).collect();
        h.send(EngineCommand::SetInputRoutes(Box::new(InputRoutes {
            routes: pairs
                .iter()
                .enumerate()
                .map(|(k, &input)| InputRoute {
                    input,
                    strip: 1 + k as u16,
                    armed: Some(MonitorMode::Mixer),
                })
                .collect(),
        })))
        .unwrap();
        let (samples, consumer) = rtrb::RingBuffer::new(SR as usize * IN_CH * 2);
        let (marks, _) = rtrb::RingBuffer::new(64);
        h.send(EngineCommand::SetRecorder(Some(Box::new(RecordSink {
            samples,
            marks,
            lanes: pairs,
            compensation: 256,
            punch: None,
        }))))
        .unwrap();
        rec = Some(consumer);
    }
    h.send(EngineCommand::SetMetronome(false)).unwrap();
    h.send(EngineCommand::Play).unwrap();
    Rig {
        h,
        p,
        input,
        rec,
        buf: vec![0.0; BLOCK * 2],
    }
}

fn second(r: &mut Rig) {
    for _ in 0..SR as usize / BLOCK {
        let fill = BLOCK * IN_CH;
        if let Ok(chunk) = r.input.write_chunk_uninit(fill.min(r.input.slots())) {
            chunk.fill_from_iter((0..fill).map(|i| (i as f32 * 0.01).sin() * 0.3));
        }
        r.p.process(&mut r.buf);
        black_box(&r.buf);
        if let Some(c) = r.rec.as_mut() {
            let n = c.slots();
            if let Ok(chunk) = c.read_chunk(n) {
                chunk.commit_all();
            }
        }
    }
    r.h.collect_garbage();
}

fn input(c: &mut Criterion) {
    let mut g = c.benchmark_group("engine_input_1s_48k");
    let mut idle = rig(false);
    g.bench_function("idle", |b| b.iter(|| second(&mut idle)));
    let mut rec = rig(true);
    g.bench_function("8ch_in_4_routes_4_stereo_tracks", |b| {
        b.iter(|| second(&mut rec))
    });
    g.finish();
}

criterion_group!(benches, input);
criterion_main!(benches);
