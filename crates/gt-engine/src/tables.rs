//! The wavetables a synth channel reads (Prompt 24, ARCHITECTURE.md D130): the factory bank,
//! shared by every channel, and each oscillator's user table built from a loaded sample. Built
//! off the audio thread and handed over with `SetChannelTables`.

use std::sync::Arc;

use gt_core::{SampleData, SampleSource, SynthPatch};
use gt_dsp::synth::additive::Resynth;
use gt_dsp::synth::wavetable::{SynthTables, Wavetable};

/// What a synth patch's wavetable oscillators need: `None` unless it uses the wavetable engine
/// (or the additive one, whose resynthesis reads the first sample slot, D153). `sample` gives
/// the data of a loaded user table; one not loaded yet falls back to the factory choice.
pub fn synth_tables(
    patch: &SynthPatch,
    sample: impl Fn(&SampleSource) -> Option<Arc<SampleData>>,
) -> Option<Arc<SynthTables>> {
    let mono = |d: &SampleData| -> Vec<f32> {
        d.left()
            .iter()
            .zip(d.right())
            .map(|(l, r)| 0.5 * (l + r))
            .collect()
    };
    if patch.engine() == 4 {
        let mut t = SynthTables::factory();
        t.resynth = patch.tables[0].as_ref().and_then(|s| {
            let d = sample(s)?;
            Resynth::analyze(&s.display_name(), &mono(&d), d.sample_rate as f32).map(Arc::new)
        });
        return Some(Arc::new(t));
    }
    if patch.engine() != 2 {
        return None;
    }
    let mut t = SynthTables::factory();
    for (user, src) in t.user.iter_mut().zip(&patch.tables) {
        *user = src.as_ref().and_then(|s| {
            let d = sample(s)?;
            let mono: Vec<f32> = d
                .left()
                .iter()
                .zip(d.right())
                .map(|(l, r)| 0.5 * (l + r))
                .collect();
            Wavetable::from_audio(&s.display_name(), &mono).map(Arc::new)
        });
    }
    Some(Arc::new(t))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_core::synth::{SynthParam, WT_TABLES};
    use gt_dsp::synth::wavetable::FACTORY_TABLES;

    #[test]
    fn document_and_dsp_agree_on_the_factory_tables() {
        assert_eq!(WT_TABLES, FACTORY_TABLES);
        assert_eq!(
            SynthParam::Wt1Table.info().max as usize + 1,
            FACTORY_TABLES.len()
        );
    }

    #[test]
    fn only_wavetable_patches_get_tables_and_user_tables_load() {
        let mut p = SynthPatch::default();
        assert!(synth_tables(&p, |_| None).is_none());
        p.set(SynthParam::Engine, 2.0);
        p.tables[1] = Some(SampleSource::File("cycle.wav".into()));
        let cycle: Vec<f32> = (0..512)
            .map(|i| (std::f32::consts::TAU * i as f32 / 512.0).sin())
            .collect();
        let data = Arc::new(SampleData::mono(48_000, cycle));
        let t = synth_tables(&p, |_| Some(Arc::clone(&data))).unwrap();
        assert!(t.user[0].is_none());
        assert_eq!(t.get(1, 0).unwrap().name(), "cycle.wav");
        assert_eq!(t.get(0, 3).unwrap().name(), "Vowels");
    }

    #[test]
    fn an_additive_patch_resynthesises_its_sample_and_plays_it() {
        use gt_dsp::synth::GloomSynth;
        let mut p = SynthPatch::default();
        p.set(SynthParam::Engine, 4.0);
        assert!(synth_tables(&p, |_| None).unwrap().resynth.is_none());
        // A 220 Hz tone with a strong third harmonic.
        let tone: Vec<f32> = (0..48_000)
            .map(|i| {
                let w = std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0;
                0.5 * w.sin() + 0.4 * (3.0 * w).sin()
            })
            .collect();
        p.tables[0] = Some(SampleSource::File("voice.wav".into()));
        let data = Arc::new(SampleData::mono(48_000, tone));
        let t = synth_tables(&p, |_| Some(Arc::clone(&data))).unwrap();
        let r = t.resynth.as_ref().expect("analysed");
        assert!((r.f0 - 220.0).abs() < 2.0, "{}", r.f0);
        // Morph 1 (the default): the sample alone; it plays with the sample's balance.
        let mut s = GloomSynth::new(48_000.0);
        s.set_tables(Some(t));
        s.set_settings(&crate::song::synth_settings(&p));
        s.note_on(57, 0.8, 1);
        let (mut l, mut r) = (vec![0.0; 9_600], vec![0.0; 9_600]);
        s.render(&mut l, &mut r);
        let bin = |f: f32| {
            let (mut re, mut im) = (0.0, 0.0);
            for (i, v) in l[4_800..].iter().enumerate() {
                let w = std::f32::consts::TAU * f * i as f32 / 48_000.0;
                re += v * w.cos();
                im += v * w.sin();
            }
            (re * re + im * im).sqrt()
        };
        let (h1, h2, h3) = (bin(220.0), bin(440.0), bin(660.0));
        assert!(h3 > 0.5 * h1 && h2 < 0.1 * h1, "{h1} {h2} {h3}");
    }

    #[test]
    fn every_factory_preset_sounds_and_stays_bounded() {
        use gt_dsp::synth::GloomSynth;
        let tables = Arc::new(gt_dsp::synth::wavetable::SynthTables::factory());
        for patch in SynthPatch::factory() {
            let mut s = GloomSynth::new(48_000.0);
            s.set_tables(Some(Arc::clone(&tables)));
            s.set_settings(&crate::song::synth_settings(&patch));
            s.note_on(60, 0.8, 1);
            s.note_on(67, 0.8, 2);
            let (mut l, mut r) = (vec![0.0; 24_000], vec![0.0; 24_000]);
            s.render(&mut l, &mut r);
            let rms = (l.iter().map(|v| v * v).sum::<f32>() / l.len() as f32).sqrt();
            let peak = l.iter().chain(&r).fold(0.0_f32, |m, v| m.max(v.abs()));
            assert!(rms > 1e-3, "{}: silent ({rms})", patch.name);
            assert!(
                peak.is_finite() && peak < 2.0,
                "{}: peak {peak}",
                patch.name
            );
        }
    }
}
