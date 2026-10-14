//! Channel rack 2 end to end (Prompt 27): long patterns, choke groups cutting on the exact
//! frame, and layer channels fanning their notes out to their children.

use std::sync::Arc;

use gt_core::{ChannelId, Instrument, LayerMode, Note, Project, SampleData, ROOT_KEY, STEP_TICKS};
use gt_engine::{create, ChannelParams, EngineCommand, EngineConfig, SongSnapshot};

const SR: u32 = 48_000;
/// One step at the default 120 BPM: 1/8 s.
const STEP: usize = SR as usize / 8;

/// Plays the current pattern of `p` for `seconds`, each sampler channel playing a constant
/// sample (one-shot, `len` frames) of the given amplitude; returns the mono output.
fn play(p: &Project, amps: &[f32], len: usize, seconds: f64) -> Vec<f32> {
    let (mut h, mut proc_) = create(EngineConfig {
        sample_rate: SR,
        out_channels: 1,
    });
    h.send(EngineCommand::SetMetronome(false)).unwrap();
    for (i, ch) in p.channels.iter().enumerate() {
        if let Instrument::Sampler(_) = ch.instrument {
            let dc = vec![amps[i]; len];
            h.send(EngineCommand::SetChannelSample {
                slot: i as u16,
                sample: Some(Arc::new(SampleData::mono(SR, dc))),
            })
            .unwrap();
        }
        h.send(EngineCommand::SetChannelParams {
            slot: i as u16,
            params: Box::new(ChannelParams::from_project(p, i)),
        })
        .unwrap();
    }
    h.send(EngineCommand::SetSong(Box::new(SongSnapshot::compile(p))))
        .unwrap();
    h.send(EngineCommand::Play).unwrap();
    let mut out = vec![0.0; (seconds * f64::from(SR)) as usize];
    for chunk in out.chunks_mut(441) {
        proc_.process(chunk);
    }
    out
}

fn step(s: i64, len: i64, velocity: f32) -> Note {
    Note::new(s * STEP_TICKS, len * STEP_TICKS, ROOT_KEY, velocity)
}

fn samplers(p: &mut Project, n: usize) -> Vec<ChannelId> {
    (0..n)
        .map(|i| p.add_channel(&format!("s{i}"), None).unwrap())
        .collect()
}

/// First frames where the output turns on after silence.
fn onsets(x: &[f32]) -> Vec<usize> {
    (0..x.len())
        .filter(|&i| x[i].abs() > 1e-6 && (i == 0 || x[i - 1].abs() <= 1e-6))
        .collect()
}

#[test]
fn long_patterns_play_every_step_and_loop_at_their_own_length() {
    // 512 steps would take a minute of audio in debug builds; 300 already passes every
    // boundary a 256-step limit or an 8-bit counter would trip on.
    let steps = 300_u16;
    let mut p = Project::empty();
    let c = samplers(&mut p, 1)[0];
    let last = i64::from(steps) - 1;
    p.current_pattern_mut().steps = steps;
    p.current_pattern_mut().notes.insert(
        c,
        vec![step(0, 1, 1.0), step(257, 1, 1.0), step(last, 1, 1.0)],
    );
    assert_eq!(
        p.current_pattern().length_ticks(),
        i64::from(steps) * STEP_TICKS
    );
    let seconds = (usize::from(steps) + 2) as f64 / 8.0;
    let out = play(&p, &[0.5], STEP / 2, seconds);
    let got = onsets(&out);
    let want = [0, 257, last as usize, usize::from(steps)].map(|s| s * STEP);
    assert_eq!(got.len(), 4, "{steps} steps: {got:?}");
    for (g, w) in got.iter().zip(want) {
        // The sampler's attack starts from zero: the first sample may be silent.
        assert!(
            (w..=w + 1).contains(g),
            "{steps} steps: onset {g}, want {w}"
        );
    }
}

#[test]
fn a_choke_group_cuts_the_other_channel_on_the_exact_frame() {
    let build = |choke: u8| {
        let mut p = Project::empty();
        let ids = samplers(&mut p, 2);
        for ch in &mut p.channels {
            ch.choke = choke;
        }
        let pat = p.current_pattern_mut();
        // The open hat rings for a whole bar; the closed hat (silent here) comes at step 4.
        pat.notes.insert(ids[0], vec![step(0, 16, 1.0)]);
        pat.notes.insert(ids[1], vec![step(4, 1, 1.0)]);
        play(&p, &[0.5, 0.0], SR as usize, 1.0)
    };
    let free = build(0);
    let choked = build(1);
    let at = 4 * STEP;
    let steady = free[at - 1];
    assert!(steady > 0.1);
    assert_eq!(choked[..at], free[..at], "nothing changes before the choke");
    assert_eq!(free[at], steady, "without a group the note rings on");
    assert!(
        choked[at] < steady,
        "the fade starts on the choking note's frame"
    );
    // A short fade (5 ms), then silence.
    assert!(choked[at + SR as usize / 100..]
        .iter()
        .all(|v| v.abs() < 1e-6));
    // Fades down without a click.
    let max_step = choked[at - 1..at + 480]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(max_step < steady / 50.0, "{max_step}");
}

/// The steady output of each note (in the middle of steps 1, 5, 9, 13) of a layer playing
/// three children with amplitudes 1, 2 and 4 (times 0.1).
fn layer_levels(mode: LayerMode, split: bool, velocities: [f32; 4]) -> Vec<f32> {
    let mut p = Project::empty();
    let layer = p.add_layer_channel("layer").unwrap();
    let kids = samplers(&mut p, 3);
    let l = p.channels[0].layer_mut().unwrap();
    l.children = kids.clone();
    l.mode = mode;
    l.split = split;
    let notes = (0..4)
        .map(|k| step(4 * k as i64, 2, velocities[k]))
        .collect();
    p.current_pattern_mut().notes.insert(layer, notes);
    let out = play(&p, &[0.0, 0.1, 0.2, 0.4], 5 * STEP / 2, 2.0);
    // Between notes everything has been released.
    for k in 0..4 {
        assert!(out[(4 * k + 3) * STEP + STEP / 2].abs() < 1e-4, "gap {k}");
    }
    (0..4).map(|k| out[(4 * k + 1) * STEP]).collect()
}

#[test]
fn a_layer_channel_fans_each_note_out_to_its_children() {
    let all = layer_levels(LayerMode::All, false, [0.8; 4]);
    let unit = all[0] / 7.0;
    assert!(unit > 0.01);
    let units = |v: Vec<f32>| -> Vec<u32> { v.iter().map(|x| (x / unit).round() as u32).collect() };
    assert_eq!(units(all), [7, 7, 7, 7], "every child");
    // Velocity split: soft, middle and hard thirds pick one child each (level scales with
    // velocity, so compare against the same velocity played by All).
    let split = layer_levels(LayerMode::All, true, [0.2, 0.5, 0.9, 0.2]);
    let all_v = layer_levels(LayerMode::All, false, [0.2, 0.5, 0.9, 0.2]);
    let picked: Vec<u32> = split
        .iter()
        .zip(&all_v)
        .map(|(s, a)| (s / a * 7.0).round() as u32)
        .collect();
    assert_eq!(picked, [1, 2, 4, 1], "velocity split");
    assert_eq!(
        units(layer_levels(LayerMode::RoundRobin, false, [0.8; 4])),
        [1, 2, 4, 1],
        "round robin"
    );
    let random = units(layer_levels(LayerMode::Random, false, [0.8; 4]));
    assert!(random.iter().all(|u| [1, 2, 4].contains(u)), "{random:?}");
    assert_eq!(
        random,
        units(layer_levels(LayerMode::Random, false, [0.8; 4])),
        "random is the same on every render"
    );
}
