//! The mixer through the public engine API: routing, sends, polarity, delay compensation,
//! effect state across settings changes, and meters.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use gt_core::{EffectKind, EffectSlot, Mixer, SampleData, MASTER};
use gt_engine::{
    create, create_effect, AudioProcessor, ChannelParams, EngineCommand, EngineConfig,
    EngineHandle, MixerParams,
};

const SR: u32 = 48_000;
const FIRST_SEND: usize = gt_core::mixer::FIRST_SEND;
/// A centred channel is -3 dB per side (equal-power pan); mixer strips are unity at centre.
const CENTRE: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn engine() -> (EngineHandle, AudioProcessor) {
    create(EngineConfig {
        sample_rate: SR,
        out_channels: 2,
    })
}

/// Channel `slot` plays an impulse (1.0 then silence) into strip `route` when triggered.
fn impulse_channel(h: &mut EngineHandle, slot: u16, route: usize) {
    let mut v = vec![0.0; 4800];
    v[0] = 1.0;
    h.send(EngineCommand::SetChannelSample {
        slot,
        sample: Some(Arc::new(SampleData::mono(SR, v))),
    })
    .unwrap();
    h.send(EngineCommand::SetChannelParams {
        slot,
        params: Box::new(ChannelParams {
            gain: 1.0,
            route: route as u8,
            ..ChannelParams::default()
        }),
    })
    .unwrap();
}

fn hit(h: &mut EngineHandle, slot: u16) {
    h.send(EngineCommand::NoteOn {
        slot,
        key: 60,
        velocity: 1.0,
    })
    .unwrap();
}

fn mixer(h: &mut EngineHandle, m: &Mixer) {
    h.send(EngineCommand::SetMixer(Box::new(MixerParams::from_mixer(
        m,
    ))))
    .unwrap();
}

/// Renders `frames` and returns the left channel.
fn render(p: &mut AudioProcessor, frames: usize) -> Vec<f32> {
    let mut buf = vec![0.0; 2 * frames];
    p.process(&mut buf);
    buf.chunks_exact(2).map(|f| f[0]).collect()
}

#[test]
fn channels_reach_the_master_through_their_insert() {
    let (mut h, mut p) = engine();
    let mut m = Mixer::new();
    m.strips[3].volume = 0.5;
    m.strips[3].output = 7;
    m.strips[7].volume = 0.5;
    mixer(&mut h, &m);
    impulse_channel(&mut h, 0, 3);
    render(&mut p, 2048); // let the fader ramps settle
    hit(&mut h, 0);
    let out = render(&mut p, 256);
    assert!((out[0] - 0.25 * CENTRE).abs() < 1e-6, "{}", out[0]);
    assert!(out[1..].iter().all(|&v| v == 0.0));
    let peak = h.telemetry().meters[7].peak[0].load(Ordering::Relaxed);
    assert!((peak - 0.25 * CENTRE).abs() < 1e-6);
}

#[test]
fn sends_add_a_post_fader_copy_and_mute_silences() {
    let (mut h, mut p) = engine();
    let mut m = Mixer::new();
    m.strips[1].sends[2] = 0.5;
    m.strips[FIRST_SEND + 2].volume = 1.0;
    mixer(&mut h, &m);
    impulse_channel(&mut h, 0, 1);
    render(&mut p, 2048);
    hit(&mut h, 0);
    let out = render(&mut p, 64);
    assert!((out[0] - 1.5 * CENTRE).abs() < 1e-6, "{}", out[0]);

    m.strips[1].mute = true;
    mixer(&mut h, &m);
    render(&mut p, 2048);
    hit(&mut h, 0);
    let out = render(&mut p, 64);
    assert!(out.iter().all(|&v| v == 0.0));
}

#[test]
fn inverted_copy_cancels() {
    let (mut h, mut p) = engine();
    let mut m = Mixer::new();
    m.strips[2].phase_invert = true;
    mixer(&mut h, &m);
    impulse_channel(&mut h, 0, 1);
    impulse_channel(&mut h, 1, 2);
    render(&mut p, 2048);
    hit(&mut h, 0);
    hit(&mut h, 1);
    let out = render(&mut p, 128);
    assert!(out.iter().all(|&v| v.abs() < 1e-6));
}

#[test]
fn delay_compensation_aligns_a_limited_path_with_a_dry_one() {
    let (mut h, mut p) = engine();
    // Insert 1 gets a limiter (1.5 ms look-ahead); insert 2 stays dry.
    let lim = EffectSlot::new(EffectKind::Limiter).with("ceiling", 0.0);
    let mut m = Mixer::new();
    m.strips[1].slots[0] = Some(lim.clone());
    mixer(&mut h, &m);
    h.send(EngineCommand::SetEffect {
        strip: 1,
        slot: 0,
        effect: Some(create_effect(&lim, SR as f32)),
    })
    .unwrap();
    impulse_channel(&mut h, 0, 1);
    let mut quiet = vec![0.0; 4800];
    quiet[0] = 0.25;
    h.send(EngineCommand::SetChannelSample {
        slot: 1,
        sample: Some(Arc::new(SampleData::mono(SR, quiet))),
    })
    .unwrap();
    h.send(EngineCommand::SetChannelParams {
        slot: 1,
        params: Box::new(ChannelParams {
            gain: 1.0,
            route: 2,
            ..ChannelParams::default()
        }),
    })
    .unwrap();
    render(&mut p, 2048);
    let latency = h.telemetry().latency_frames.load(Ordering::Relaxed) as usize;
    assert_eq!(latency, 71);
    hit(&mut h, 0);
    hit(&mut h, 1);
    let out = render(&mut p, 512);
    let nonzero: Vec<usize> = (0..out.len()).filter(|&i| out[i].abs() > 1e-6).collect();
    // Both arrive together, one look-ahead late: the impulse through the limiter (below its
    // ceiling, so unchanged) and the quiet dry one, delayed to match.
    assert_eq!(nonzero, vec![latency], "{nonzero:?}");
    assert!(
        (out[latency] - 1.25 * CENTRE).abs() < 1e-5,
        "{}",
        out[latency]
    );
}

#[test]
fn reverb_tail_survives_mixer_and_channel_changes() {
    let (mut h, mut p) = engine();
    let rv = EffectSlot::new(EffectKind::Reverb)
        .with("mix", 1.0)
        .with("decay", 3.0);
    let mut m = Mixer::new();
    m.strips[1].sends[0] = 1.0;
    // Sends are post-fader, so keep the fader up and send the dry signal to a muted bus.
    m.strips[1].output = FIRST_SEND + 1; // dry signal goes to an unused, muted bus
    m.strips[FIRST_SEND + 1].mute = true;
    m.strips[FIRST_SEND].slots[0] = Some(rv.clone());
    mixer(&mut h, &m);
    h.send(EngineCommand::SetEffect {
        strip: FIRST_SEND as u8,
        slot: 0,
        effect: Some(create_effect(&rv, SR as f32)),
    })
    .unwrap();
    impulse_channel(&mut h, 0, 1);
    render(&mut p, 2048);
    hit(&mut h, 0);
    render(&mut p, 4800);
    // Change the mixer and add a channel while the tail rings.
    m.strips[4].volume = 0.3;
    mixer(&mut h, &m);
    impulse_channel(&mut h, 5, 4);
    let tail = render(&mut p, 9600);
    let energy: f32 = tail.iter().map(|v| v * v).sum();
    assert!(energy > 1e-4, "{energy}");
    h.collect_garbage();
}

#[test]
fn replaced_effects_and_mixer_boxes_come_back_as_garbage() {
    let (mut h, mut p) = engine();
    let eq = EffectSlot::new(EffectKind::Eq);
    for _ in 0..2 {
        h.send(EngineCommand::SetEffect {
            strip: MASTER as u8,
            slot: 3,
            effect: Some(create_effect(&eq, SR as f32)),
        })
        .unwrap();
    }
    h.send(EngineCommand::SetEffect {
        strip: MASTER as u8,
        slot: 3,
        effect: None,
    })
    .unwrap();
    mixer(&mut h, &Mixer::new());
    render(&mut p, 64);
    // Two replaced EQs and one mixer box.
    assert_eq!(h.collect_garbage(), 3);
}
