//! Measures how long the engine takes to render the demo arrangement (every channel playing a
//! drum sample, the demo mix with its effects, automation and modulators) faster than real
//! time, and the worst single callback.
//!
//! `cargo run --release -p gt-engine --example render_cost`

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use gt_core::{Project, SampleData, MAX_CHANNELS};
use gt_engine::{
    create, create_effect, ChannelParams, EngineCommand, EngineConfig, LoadMeter, MixerParams,
    ModPlan, SongSnapshot,
};

fn main() {
    let sr = 48_000;
    let (mut h, mut p) = create(EngineConfig {
        sample_rate: sr,
        out_channels: 2,
    });
    let project = Project::demo();
    let kick = Arc::new(SampleData::mono(sr, gt_dsp::drums::kick(sr as f32)));
    for slot in 0..MAX_CHANNELS as u16 {
        h.send(EngineCommand::SetChannelSample {
            slot,
            sample: Some(Arc::clone(&kick)),
        })
        .unwrap();
        h.send(EngineCommand::SetChannelParams {
            slot,
            params: Box::new(ChannelParams {
                gain: 0.3,
                route: (slot % 8 + 1) as u8,
                ..ChannelParams::default()
            }),
        })
        .unwrap();
    }
    for (si, strip) in project.mixer.strips.iter().enumerate() {
        for (k, slot) in strip.slots.iter().enumerate() {
            if let Some(slot) = slot {
                h.send(EngineCommand::SetEffect {
                    strip: si as u8,
                    slot: k as u8,
                    effect: Some(create_effect(slot, sr as f32)),
                })
                .unwrap();
            }
        }
    }
    h.send(EngineCommand::SetMixer(Box::new(MixerParams::from_mixer(
        &project.mixer,
    ))))
    .unwrap();
    let song = SongSnapshot::compile_song(&project, |_| Some(Arc::clone(&kick)));
    h.send(EngineCommand::SetSong(Box::new(song))).unwrap();
    h.send(EngineCommand::SetModulation(Box::new(ModPlan::compile(
        &project,
    ))))
    .unwrap();
    h.send(EngineCommand::Play).unwrap();
    let seconds = 30;
    // 256-frame callbacks, like a typical device buffer, timed one by one through the same
    // load meter the device layer uses.
    let frames = 256;
    let mut buf = vec![0.0_f32; 2 * frames];
    let blocks = seconds * sr as usize / frames;
    let mut meter = LoadMeter::new(sr);
    let mut loads = Vec::with_capacity(blocks);
    let t0 = Instant::now();
    for _ in 0..blocks {
        let start = Instant::now();
        p.process(&mut buf);
        loads.push(meter.record(p.telemetry(), start.elapsed().as_nanos() as u64, frames));
        h.collect_garbage();
    }
    let dt = t0.elapsed().as_secs_f64();
    loads.sort_by(f32::total_cmp);
    let pct = |q: f64| 100.0 * loads[((loads.len() - 1) as f64 * q) as usize];
    println!(
        "{seconds} s of audio in {:.1} ms: {:.2} % of real time",
        dt * 1000.0,
        100.0 * dt / seconds as f64
    );
    println!(
        "per {frames}-frame callback: median {:.1} %, p99 {:.1} %, p99.9 {:.1} %, worst {:.1} % \
         of the buffer time; {} over budget",
        pct(0.5),
        pct(0.99),
        pct(0.999),
        pct(1.0),
        p.telemetry().overloads.load(Ordering::Relaxed)
    );
}
