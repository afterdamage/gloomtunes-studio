//! Drum synth voices (Prompt 28): each default voice's level envelope and brightness, block by
//! block, pinned as a snapshot. Rounded to 0.1 dB (and, for the tonal kick and tom, 100 zero
//! crossings per second) so tiny floating-point differences between platforms do not matter.

use std::fmt::Write;

use gt_dsp::drums::{DrumModel, DrumVoice};

fn db(x: f32) -> f32 {
    (20.0 * x.max(1e-6).log10()).max(-120.0)
}

#[test]
fn default_voices() {
    let sr = 48_000.0;
    let voices = [
        ("kick", DrumVoice::KICK),
        ("snare", DrumVoice::SNARE),
        ("closed hat", DrumVoice::HAT),
        ("open hat", DrumVoice::OPEN_HAT),
        ("clap", DrumVoice::CLAP),
        ("tom", DrumVoice::TOM),
        ("perc", DrumVoice::PERC),
    ];
    assert!(DrumModel::ALL
        .iter()
        .all(|m| voices.iter().any(|(_, v)| v.model == *m)));
    let mut out = String::new();
    for (name, v) in voices {
        let s = gt_dsp::drums::render(&v, sr);
        writeln!(out, "{name}: {} frames", s.len()).unwrap();
        writeln!(out, "  ms   rms dB  zc/s").unwrap();
        for (i, b) in s.chunks(960).take(15).enumerate() {
            let rms = (b.iter().map(|x| x * x).sum::<f32>() / b.len() as f32).sqrt();
            let zc = b
                .windows(2)
                .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
                .count() as f32
                / (b.len() as f32 / sr);
            writeln!(
                out,
                "{:>4} {:>8.1} {:>5}",
                i * 20,
                db(rms),
                if matches!(v.model, DrumModel::Kick | DrumModel::Tom) {
                    format!("{}", (zc / 100.0).round() as i32 * 100)
                } else {
                    "-".to_owned()
                }
            )
            .unwrap();
        }
    }
    insta::assert_snapshot!(out);
}
