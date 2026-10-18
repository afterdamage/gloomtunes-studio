//! The mixer through the public engine API: routing, sends, polarity, delay compensation,
//! effect state across settings changes, meters, pan laws and routing edits while playing.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use gt_core::{EffectKind, EffectSlot, Mixer, PanLaw, SampleData, MASTER};
use gt_engine::{
    create, create_effect, AudioProcessor, ChannelParams, EngineCommand, EngineConfig,
    EngineHandle, MixerParams,
};

const SR: u32 = 48_000;
/// Inserts used as effect returns in these tests.
const FX_A: usize = 65;
const FX_B: usize = 66;
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
            route: route as u16,
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
    let mut m = Mixer::with_inserts(FX_B);
    assert!(m.add_send(1, FX_B, 0.5, false));
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
    let mut m = Mixer::with_inserts(FX_B);
    m.add_send(1, FX_A, 1.0, false);
    // Sends are post-fader, so keep the fader up and send the dry signal to a muted insert.
    m.strips[1].output = FX_B;
    m.strips[FX_B].mute = true;
    m.strips[FX_A].slots[0] = Some(rv.clone());
    mixer(&mut h, &m);
    h.send(EngineCommand::SetEffect {
        strip: FX_A as u16,
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
            strip: MASTER as u16,
            slot: 3,
            effect: Some(create_effect(&eq, SR as f32)),
        })
        .unwrap();
    }
    h.send(EngineCommand::SetEffect {
        strip: MASTER as u16,
        slot: 3,
        effect: None,
    })
    .unwrap();
    mixer(&mut h, &Mixer::new());
    render(&mut p, 64);
    // Two replaced EQs and one mixer box.
    assert_eq!(h.collect_garbage(), 3);
}

/// Plays a 1 kHz tone through insert 1 holding `slot` (or bypassing it) and returns 4096
/// frames of the master with the reported latency.
fn through_slot(slot: &EffectSlot) -> (Vec<f32>, usize) {
    let (mut h, mut p) = engine();
    let mut m = Mixer::new();
    m.strips[1].slots[0] = Some(slot.clone());
    mixer(&mut h, &m);
    h.send(EngineCommand::SetEffect {
        strip: 1,
        slot: 0,
        effect: Some(create_effect(slot, SR as f32)),
    })
    .unwrap();
    let tone: Vec<f32> = (0..9600)
        .map(|i| 0.5 * (std::f32::consts::TAU * 1000.0 * i as f32 / SR as f32).sin())
        .collect();
    h.send(EngineCommand::SetChannelSample {
        slot: 0,
        sample: Some(Arc::new(SampleData::mono(SR, tone))),
    })
    .unwrap();
    h.send(EngineCommand::SetChannelParams {
        slot: 0,
        params: Box::new(ChannelParams {
            gain: 1.0,
            route: 1,
            ..ChannelParams::default()
        }),
    })
    .unwrap();
    render(&mut p, 2048);
    hit(&mut h, 0);
    let out = render(&mut p, 4096);
    (
        out,
        h.telemetry().latency_frames.load(Ordering::Relaxed) as usize,
    )
}

#[test]
fn a_slot_mixed_fully_dry_is_bit_identical_to_bypass() {
    for kind in EffectKind::ALL {
        let mut off = EffectSlot::new(kind);
        off.enabled = false;
        let mut dry = EffectSlot::new(kind);
        dry.mix = 0.0;
        assert_eq!(through_slot(&off), through_slot(&dry), "{}", kind.key());
    }
}

#[test]
fn a_partial_mix_lines_the_dry_signal_up_with_a_latent_effect() {
    // A gate that is always open with 2 ms look-ahead: wet is the input 96 frames late.
    let gate = EffectSlot::new(EffectKind::Gate)
        .with("threshold", -80.0)
        .with("attack", 0.01)
        .with("lookahead", 2.0);
    let (wet, latency) = through_slot(&gate);
    assert_eq!(latency, 96);
    let mut half = gate.clone();
    half.mix = 0.5;
    let (mixed, latency) = through_slot(&half);
    assert_eq!(latency, 96);
    // Dry and wet line up, so half of each is the same signal (no comb filtering).
    let settled = 1024..4096;
    let err = wet[settled.clone()]
        .iter()
        .zip(&mixed[settled])
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(err < 1e-5, "{err}");
    // Mixed fully dry, the slot adds no latency.
    half.mix = 0.0;
    assert_eq!(through_slot(&half).1, 0);
}

/// A 440 Hz tone on channel 0 into strip `route`, long enough for every test below.
fn tone_channel(h: &mut EngineHandle, route: usize) {
    let tone: Vec<f32> = (0..96_000)
        .map(|i| 0.5 * (std::f32::consts::TAU * 440.0 * i as f32 / SR as f32).sin())
        .collect();
    h.send(EngineCommand::SetChannelSample {
        slot: 0,
        sample: Some(Arc::new(SampleData::mono(SR, tone))),
    })
    .unwrap();
    h.send(EngineCommand::SetChannelParams {
        slot: 0,
        params: Box::new(ChannelParams {
            gain: 1.0,
            route: route as u16,
            ..ChannelParams::default()
        }),
    })
    .unwrap();
}

/// Largest second difference relative to the loudest sample: about 0.003 for a 440 Hz sine
/// (whose level glides), far more where the signal jumps.
fn roughness(x: &[f32]) -> f32 {
    let peak = x.iter().fold(0.0_f32, |m, v| m.max(v.abs()));
    let d2 = x
        .windows(3)
        .map(|w| (w[2] - 2.0 * w[1] + w[0]).abs())
        .fold(0.0, f32::max);
    d2 / peak
}

#[test]
fn routing_changes_while_playing_do_not_click() {
    let (mut h, mut p) = engine();
    let mut m = Mixer::with_inserts(8);
    m.strips[3].volume = 0.0; // a silent destination: rerouting there is a full fade out
    mixer(&mut h, &m);
    tone_channel(&mut h, 1);
    render(&mut p, 2048);
    hit(&mut h, 0);
    let mut out = render(&mut p, 4800);
    let smooth = roughness(&out);
    let mut edit = |m: &Mixer, h: &mut EngineHandle, p: &mut AudioProcessor| {
        mixer(h, m);
        out.extend(render(p, 4800));
    };
    // Output to another insert, a new pre- and post-fader send, a send removed, a send
    // switched to pre-fader, the output into the silent insert and back, more inserts.
    m.strips[1].output = 2;
    edit(&m, &mut h, &mut p);
    assert!(m.add_send(1, 4, 0.8, false) && m.add_send(2, 5, 0.6, true));
    edit(&m, &mut h, &mut p);
    m.strips[1].sends.clear();
    edit(&m, &mut h, &mut p);
    m.strips[2].sends[0].pre = false;
    edit(&m, &mut h, &mut p);
    m.strips[2].output = 3;
    edit(&m, &mut h, &mut p);
    m.strips[2].output = MASTER;
    m.add_insert();
    m.add_insert();
    edit(&m, &mut h, &mut p);
    let rough = roughness(&out);
    assert!(rough < 2.0 * smooth, "click: {rough} against {smooth}");
    assert!(
        out[out.len() - 100..].iter().any(|v| v.abs() > 0.1),
        "still playing"
    );
}

#[test]
fn delay_compensation_covers_sends_and_sidechain_keys() {
    let (mut h, mut p) = engine();
    // Insert 1 has a limiter (71 frames) and sends to insert 2; insert 3 plays dry. Insert 2
    // must hear 1's send and 3's sidechain key aligned, and the master all three.
    let lim = EffectSlot::new(EffectKind::Limiter).with("ceiling", 0.0);
    let mut m = Mixer::with_inserts(4);
    m.strips[1].slots[0] = Some(lim.clone());
    m.strips[1].output = 4;
    m.strips[4].mute = true;
    m.add_send(1, 2, 1.0, true);
    m.strips[2].sidechain = Some(3);
    mixer(&mut h, &m);
    h.send(EngineCommand::SetEffect {
        strip: 1,
        slot: 0,
        effect: Some(create_effect(&lim, SR as f32)),
    })
    .unwrap();
    impulse_channel(&mut h, 0, 1);
    impulse_channel(&mut h, 1, 3);
    render(&mut p, 2048);
    let latency = h.telemetry().latency_frames.load(Ordering::Relaxed) as usize;
    assert_eq!(latency, 71);
    assert_eq!(p.strip_input_delay(3), 0, "3 itself is not delayed");
    hit(&mut h, 0);
    hit(&mut h, 1);
    let out = render(&mut p, 512);
    let nonzero: Vec<usize> = (0..out.len()).filter(|&i| out[i].abs() > 1e-6).collect();
    // The pre-fader send of 1 and the dry 3 meet at the master together.
    assert_eq!(nonzero, vec![latency], "{nonzero:?}");
    assert!(
        (out[latency] - 2.0 * CENTRE).abs() < 1e-5,
        "{}",
        out[latency]
    );
}

#[test]
fn pan_laws_set_the_centre_level() {
    for (law, db) in [
        (PanLaw::Zero, 0.0_f32),
        (PanLaw::Minus3, -3.01),
        (PanLaw::Minus4_5, -4.52),
        (PanLaw::Minus6, -6.02),
    ] {
        let (mut h, mut p) = engine();
        let mut m = Mixer::new();
        m.pan_law = law;
        mixer(&mut h, &m);
        impulse_channel(&mut h, 0, 1);
        render(&mut p, 2048);
        hit(&mut h, 0);
        let out = render(&mut p, 64);
        // Two strips in series (insert 1, master), each applying the law.
        let got = 20.0 * (out[0] / CENTRE).log10();
        assert!((got - 2.0 * db).abs() < 0.05, "{law:?}: {got} dB");
    }
}

#[test]
fn pre_fader_sends_ignore_the_fader_but_not_mute() {
    let (mut h, mut p) = engine();
    let mut m = Mixer::with_inserts(4);
    m.strips[1].volume = 0.0;
    m.add_send(1, 2, 0.5, true);
    mixer(&mut h, &m);
    impulse_channel(&mut h, 0, 1);
    render(&mut p, 2048);
    hit(&mut h, 0);
    let out = render(&mut p, 64);
    assert!((out[0] - 0.5 * CENTRE).abs() < 1e-6, "{}", out[0]);
    m.strips[1].mute = true;
    mixer(&mut h, &m);
    render(&mut p, 2048);
    hit(&mut h, 0);
    assert!(render(&mut p, 64).iter().all(|&v| v == 0.0));
}

#[test]
fn more_inserts_arrive_built_and_play() {
    let (mut h, mut p) = engine();
    let m = Mixer::with_inserts(gt_core::MAX_INSERTS);
    mixer(&mut h, &m);
    impulse_channel(&mut h, 0, gt_core::MAX_INSERTS);
    render(&mut p, 2048);
    hit(&mut h, 0);
    let out = render(&mut p, 64);
    assert!((out[0] - CENTRE).abs() < 1e-6, "{}", out[0]);
    // Fewer again: the strip past the end now plays into the master.
    mixer(&mut h, &Mixer::with_inserts(8));
    render(&mut p, 2048);
    hit(&mut h, 0);
    assert!((render(&mut p, 64)[0] - CENTRE).abs() < 1e-6);
}
