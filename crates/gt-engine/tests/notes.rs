//! Per-note values end to end (Prompt 21): pattern notes compiled, sent to the engine and
//! played by the sampler.

use std::sync::Arc;

use gt_core::{Note, Project, SampleData, ROOT_KEY};
use gt_engine::{create, ChannelParams, EngineCommand, EngineConfig, SongSnapshot};

const SR: u32 = 48_000;

/// Plays the current pattern of `p` (a sine sample on channel 0) for `seconds`; returns the
/// left and right outputs.
fn play(p: &Project, seconds: f64) -> (Vec<f32>, Vec<f32>) {
    let (mut h, mut proc_) = create(EngineConfig {
        sample_rate: SR,
        out_channels: 2,
    });
    let sine: Vec<f32> = (0..4 * SR)
        .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / SR as f32).sin() * 0.5)
        .collect();
    h.send(EngineCommand::SetMetronome(false)).unwrap();
    h.send(EngineCommand::SetChannelSample {
        slot: 0,
        sample: Some(Arc::new(SampleData::mono(SR, sine))),
    })
    .unwrap();
    h.send(EngineCommand::SetChannelParams {
        slot: 0,
        params: Box::new(ChannelParams {
            gain: 1.0,
            ..Default::default()
        }),
    })
    .unwrap();
    h.send(EngineCommand::SetSong(Box::new(SongSnapshot::compile(p))))
        .unwrap();
    h.send(EngineCommand::Play).unwrap();
    let mut out = vec![0.0; 2 * (seconds * f64::from(SR)) as usize];
    for chunk in out.chunks_mut(2 * 441) {
        proc_.process(chunk);
    }
    (
        out.chunks_exact(2).map(|f| f[0]).collect(),
        out.chunks_exact(2).map(|f| f[1]).collect(),
    )
}

fn project(notes: Vec<Note>) -> Project {
    let mut p = Project::empty();
    let c = p.add_channel("sine", None).unwrap();
    p.current_pattern_mut().notes.insert(c, notes);
    p
}

/// Frequency from rising zero crossings, interpolated, over `x`.
fn pitch_hz(x: &[f32]) -> f32 {
    let ups: Vec<f32> = x
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] <= 0.0 && w[1] > 0.0)
        .map(|(i, w)| i as f32 + w[0] / (w[0] - w[1]))
        .collect();
    let (Some(a), Some(b)) = (ups.first(), ups.last()) else {
        return 0.0;
    };
    (ups.len() - 1) as f32 * SR as f32 / (b - a)
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[test]
fn per_note_fine_pitch_and_pan_are_in_the_rendered_output() {
    // 120 BPM: 960 ticks = 0.5 s. One note, 30 cents sharp, hard left.
    let mut n = Note::new(0, 1920, ROOT_KEY, 1.0);
    n.fine = 30.0;
    n.pan = -1.0;
    let (l, r) = play(&project(vec![n]), 0.8);
    let f = pitch_hz(&l[2400..36_000]);
    let want = 440.0 * (30.0_f32 / 1200.0).exp2();
    assert!((f - want).abs() < 0.2, "{f} vs {want}");
    assert!(rms(&l[2400..36_000]) > 0.1);
    assert!(rms(&r[2400..36_000]) < 1e-6, "{}", rms(&r));
    // Neutral values change nothing: centred, at the sample's pitch.
    let (l, r) = play(&project(vec![Note::new(0, 1920, ROOT_KEY, 1.0)]), 0.8);
    assert!((pitch_hz(&l[2400..36_000]) - 440.0).abs() < 0.2);
    assert!((rms(&l) - rms(&r)).abs() < 1e-6);
}

#[test]
fn muted_notes_are_silent() {
    let mut n = Note::new(0, 1920, ROOT_KEY, 1.0);
    n.muted = true;
    let (l, r) = play(&project(vec![n]), 0.5);
    assert!(l.iter().chain(&r).all(|&x| x == 0.0));
}

#[test]
fn a_slide_note_glides_the_held_note_over_its_own_length() {
    // A long note, then at 0.5 s a slide note an octave up, 0.5 s long: one voice, which
    // reaches 880 Hz at 1 s and stays there.
    let base = Note::new(0, 3840, ROOT_KEY, 1.0);
    let mut slide = Note::new(960, 960, ROOT_KEY + 12, 1.0);
    slide.slide = true;
    let (l, _) = play(&project(vec![base, slide]), 1.6);
    let at = |s: f64| (s * f64::from(SR)) as usize;
    let f0 = pitch_hz(&l[at(0.1)..at(0.45)]);
    assert!((f0 - 440.0).abs() < 0.5, "{f0}");
    // Halfway through the slide: half an octave up (linear in semitones).
    let mid = pitch_hz(&l[at(0.74)..at(0.76)]);
    let want = 440.0 * 0.5_f32.exp2();
    assert!((mid - want).abs() < 6.0, "{mid} vs {want}");
    let f1 = pitch_hz(&l[at(1.05)..at(1.5)]);
    assert!((f1 - 880.0).abs() < 0.5, "{f1}");
}
