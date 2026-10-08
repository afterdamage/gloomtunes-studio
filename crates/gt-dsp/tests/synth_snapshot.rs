//! Snapshot of a rendered Gloom Synth patch.
//!
//! Renders a short phrase with every section of the synth in use (two oscillators, sub, noise,
//! unison, filter envelope, LFO through the matrix, glide, voice release) and compares a compact
//! summary with the committed snapshot: per 50 ms block, the RMS level of each side in dBFS and
//! the peak. A DSP change that alters the sound shows up as a diff in
//! `tests/snapshots/synth_snapshot__phrase.snap`; if the change is intended, review it and
//! accept it with `cargo insta review` (or `INSTA_UPDATE=always cargo test`).
//!
//! Values are rounded to 0.1 dB, coarse enough that differences in the last bits of `sin`,
//! `exp` or `tan` between platforms do not change the text.

use std::fmt::Write;

use gt_dsp::synth::{GloomSynth, ModDest, ModSlot, ModSource, SynthSettings};
use gt_dsp::{LfoWave, Wave};

fn db(x: f32) -> f32 {
    let v = 20.0 * x.max(1e-6).log10();
    // Round to 0.1 dB; avoid printing "-0.0".
    let r = (v * 10.0).round() / 10.0;
    if r == 0.0 {
        0.0
    } else {
        r
    }
}

#[test]
fn phrase() {
    let sr = 48_000.0;
    let mut s = SynthSettings {
        sub_level: 0.4,
        noise_level: 0.05,
        unison: 3,
        unison_detune_cents: 15.0,
        unison_spread: 0.8,
        cutoff_hz: 400.0,
        resonance: 0.5,
        filter_env_octaves: 4.0,
        drive: 0.3,
        glide_ms: 60.0,
        ..SynthSettings::default()
    };
    s.osc[0].wave = Wave::Saw;
    s.osc[1].wave = Wave::Square;
    s.osc[1].pulse_width = 0.3;
    s.lfo[0].wave = LfoWave::Triangle;
    s.lfo[0].rate_hz = 6.0;
    s.mods[0] = ModSlot {
        source: ModSource::Lfo1,
        dest: ModDest::Cutoff,
        amount: 0.1,
    };
    s.mods[1] = ModSlot {
        source: ModSource::Velocity,
        dest: ModDest::Amp,
        amount: 0.2,
    };
    let mut synth = GloomSynth::new(sr);
    synth.set_settings(&s);

    // (frame, key, velocity); velocity 0 means note-off.
    let events: &[(usize, u8, f32)] = &[
        (0, 45, 0.9),
        (9_600, 45, 0.0),
        (9_600, 52, 0.7),
        (19_200, 52, 0.0),
        (19_200, 57, 1.0),
        (19_200, 60, 0.6),
        (33_600, 57, 0.0),
        (33_600, 60, 0.0),
    ];
    let total = 48_000;
    let (mut l, mut r) = (vec![0.0_f32; total], vec![0.0_f32; total]);
    let mut cursor = 0;
    for (age, &(at, key, vel)) in events.iter().enumerate() {
        synth.render(&mut l[cursor..at], &mut r[cursor..at]);
        cursor = at;
        if vel > 0.0 {
            synth.note_on(key, vel, age as u64 + 1);
        } else {
            synth.note_off(key);
        }
    }
    synth.render(&mut l[cursor..], &mut r[cursor..]);

    let mut out = String::from("block   rms L   rms R    peak\n");
    for (i, (bl, br)) in l.chunks(2400).zip(r.chunks(2400)).enumerate() {
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        let peak = bl.iter().chain(br).fold(0.0_f32, |m, v| m.max(v.abs()));
        writeln!(
            out,
            "{:>5} {:>7.1} {:>7.1} {:>7.1}",
            i,
            db(rms(bl)),
            db(rms(br)),
            db(peak)
        )
        .unwrap();
    }
    write!(out, "voices at end: {}", synth.active_voices()).unwrap();
    insta::assert_snapshot!(out);
}
