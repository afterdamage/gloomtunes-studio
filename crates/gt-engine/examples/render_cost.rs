//! Measures how long the engine takes to render the demo arrangement (every channel playing a
//! drum sample, the demo mix with its effects, automation and modulators) faster than real
//! time.
//!
//! `cargo run --release -p gt-engine --example render_cost`

use std::sync::Arc;
use std::time::Instant;

use gt_core::{Project, SampleData, MAX_CHANNELS};
use gt_engine::{
    create, create_effect, ChannelParams, EngineCommand, EngineConfig, MixerParams, ModPlan,
    SongSnapshot,
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
    let mut buf = vec![0.0_f32; 2 * 512];
    let blocks = seconds * sr as usize / 512;
    let t0 = Instant::now();
    for _ in 0..blocks {
        p.process(&mut buf);
        h.collect_garbage();
    }
    let dt = t0.elapsed().as_secs_f64();
    println!(
        "{seconds} s of audio in {:.1} ms: {:.2} % of real time",
        dt * 1000.0,
        100.0 * dt / seconds as f64
    );
}
