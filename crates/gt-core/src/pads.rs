//! The drum pad instrument (Prompt 23): banks of 16 pads, each with its own sample layers,
//! tuning, envelope, choke group and optional mixer output. Pad `p` of bank `b` plays on key
//! [`FIRST_PAD_KEY`]` + 16·b + p`, so the channel rack's steps (on [`crate::ROOT_KEY`]) play
//! the first pad and the piano roll reaches every pad.

use crate::drum_synth::DrumVoice;
use crate::project::{Adsr, SampleSource};
use crate::ROOT_KEY;

/// Pads in a bank.
pub const PADS_PER_BANK: usize = 16;
/// Most banks in a kit.
pub const MAX_BANKS: usize = 4;
/// Key of the first pad of the first bank.
pub const FIRST_PAD_KEY: u8 = ROOT_KEY;
/// Choke groups (1 to this; 0 is none).
pub const CHOKE_GROUPS: u8 = 8;

/// One sample of a pad, for a range of velocities.
#[derive(Debug, Clone, PartialEq)]
pub struct PadLayer {
    /// The sound.
    pub sample: SampleSource,
    /// Lowest velocity (0 to 1).
    pub vel_lo: f32,
    /// Highest velocity (0 to 1).
    pub vel_hi: f32,
}

/// One pad.
#[derive(Debug, Clone, PartialEq)]
pub struct Pad {
    /// Display name (empty: the first layer's file name).
    pub name: String,
    /// Velocity layers, softest first.
    pub layers: Vec<PadLayer>,
    /// Tuning in semitones.
    pub tune: f32,
    /// Linear gain.
    pub gain: f32,
    /// Pan, -1 to 1.
    pub pan: f32,
    /// Amplitude envelope.
    pub adsr: Adsr,
    /// Choke group: a pad cuts the sounding pads of its group (0: none).
    pub choke: u8,
    /// Mixer insert of its own output (none: the channel's).
    pub output: Option<usize>,
    /// A synthesized sound (Prompt 28) played instead of the layers.
    pub synth: Option<DrumVoice>,
}

impl Default for Pad {
    fn default() -> Self {
        Self {
            name: String::new(),
            layers: Vec::new(),
            tune: 0.0,
            gain: 1.0,
            pan: 0.0,
            adsr: Adsr {
                attack_ms: 0.0,
                decay_ms: 0.0,
                sustain: 1.0,
                release_ms: 30.0,
            },
            choke: 0,
            output: None,
            synth: None,
        }
    }
}

impl Pad {
    /// A pad playing a synthesized sound.
    pub fn synth(name: &str, voice: DrumVoice) -> Self {
        Self {
            name: name.to_owned(),
            synth: Some(voice),
            ..Self::default()
        }
    }

    /// What the pad shows: its name, or its first layer's file (or its synth model).
    pub fn label(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        if let Some(v) = &self.synth {
            return v.model.label().to_owned();
        }
        self.layers.first().map_or_else(String::new, |l| {
            let n = l.sample.display_name();
            n.rsplit_once('.').map_or(n.clone(), |(s, _)| s.to_owned())
        })
    }

    /// Replaces the layers with `samples`, splitting the velocity range evenly (softest first).
    pub fn set_layers(&mut self, samples: Vec<SampleSource>) {
        let n = samples.len().max(1) as f32;
        let last = samples.len().saturating_sub(1);
        self.layers = samples
            .into_iter()
            .enumerate()
            .map(|(i, sample)| PadLayer {
                sample,
                vel_lo: if i == 0 { 0.0 } else { i as f32 / n },
                vel_hi: if i == last {
                    1.0
                } else {
                    (i + 1) as f32 / n - 1e-4
                },
            })
            .collect();
    }
}

/// A drum kit: banks of [`PADS_PER_BANK`] pads.
#[derive(Debug, Clone, PartialEq)]
pub struct DrumKit {
    /// Banks, each exactly [`PADS_PER_BANK`] pads.
    pub banks: Vec<Vec<Pad>>,
}

impl Default for DrumKit {
    fn default() -> Self {
        Self {
            banks: vec![vec![Pad::default(); PADS_PER_BANK]],
        }
    }
}

impl DrumKit {
    /// The drum synth's default kit (Prompt 28): kick, snare, closed and open hat (choked
    /// together), clap, low and high tom and a percussion, on the first eight pads.
    pub fn synth_kit() -> Self {
        let mut k = Self::default();
        let hat = |name: &str, v| Pad {
            choke: 1,
            ..Pad::synth(name, v)
        };
        let pads = [
            Pad::synth("Kick", DrumVoice::KICK),
            Pad::synth("Snare", DrumVoice::SNARE),
            hat("Closed hat", DrumVoice::HAT),
            hat("Open hat", DrumVoice::OPEN_HAT),
            Pad::synth("Clap", DrumVoice::CLAP),
            Pad::synth("Low tom", DrumVoice::TOM),
            Pad::synth(
                "High tom",
                DrumVoice {
                    pitch: 150.0,
                    sweep: 70.0,
                    decay: 0.15,
                    length: 0.45,
                    ..DrumVoice::TOM
                },
            ),
            Pad::synth("Perc", DrumVoice::PERC),
        ];
        for (slot, pad) in k.banks[0].iter_mut().zip(pads) {
            *slot = pad;
        }
        k
    }

    /// True when any pad plays a synthesized sound.
    pub fn has_synth(&self) -> bool {
        self.banks.iter().flatten().any(|p| p.synth.is_some())
    }

    /// Key of pad `pad` of bank `bank`.
    pub fn key(bank: usize, pad: usize) -> u8 {
        FIRST_PAD_KEY + (bank * PADS_PER_BANK + pad) as u8
    }

    /// The (bank, pad) a key plays, if any.
    pub fn pad_of(key: u8) -> Option<(usize, usize)> {
        let i = usize::from(key.checked_sub(FIRST_PAD_KEY)?);
        (i < MAX_BANKS * PADS_PER_BANK).then_some((i / PADS_PER_BANK, i % PADS_PER_BANK))
    }

    /// Every pad with its key.
    pub fn pads(&self) -> impl Iterator<Item = (u8, &Pad)> {
        self.banks.iter().enumerate().flat_map(|(b, bank)| {
            bank.iter()
                .enumerate()
                .map(move |(p, pad)| (Self::key(b, p), pad))
        })
    }

    /// Every sample the kit uses.
    pub fn sources(&self) -> impl Iterator<Item = &SampleSource> {
        self.banks
            .iter()
            .flatten()
            .flat_map(|p| p.layers.iter().map(|l| &l.sample))
    }

    /// Every sample the kit uses, mutably.
    pub fn sources_mut(&mut self) -> impl Iterator<Item = &mut SampleSource> {
        self.banks
            .iter_mut()
            .flatten()
            .flat_map(|p| p.layers.iter_mut().map(|l| &mut l.sample))
    }

    /// Fills pads in order from `samples` (one sample per pad), starting at pad 0 of bank 0,
    /// adding banks as needed (up to [`MAX_BANKS`]).
    pub fn fill(&mut self, samples: Vec<SampleSource>) {
        let mut it = samples.into_iter();
        'banks: for b in 0..MAX_BANKS {
            if b >= self.banks.len() {
                if it.len() == 0 {
                    break;
                }
                self.banks.push(vec![Pad::default(); PADS_PER_BANK]);
            }
            for p in 0..PADS_PER_BANK {
                let Some(s) = it.next() else {
                    break 'banks;
                };
                let pad = &mut self.banks[b][p];
                pad.name.clear();
                pad.set_layers(vec![s]);
            }
        }
    }

    /// Puts values in range: 1 to [`MAX_BANKS`] banks of exactly 16 pads.
    pub fn sanitize(&mut self, outputs: usize) {
        self.banks.truncate(MAX_BANKS);
        if self.banks.is_empty() {
            self.banks.push(Vec::new());
        }
        let fin =
            |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        for bank in &mut self.banks {
            bank.resize(PADS_PER_BANK, Pad::default());
            for p in bank {
                p.tune = fin(p.tune, -48.0, 48.0, 0.0);
                p.gain = fin(p.gain, 0.0, 4.0, 1.0);
                p.pan = fin(p.pan, -1.0, 1.0, 0.0);
                let a = &mut p.adsr;
                a.attack_ms = fin(a.attack_ms, 0.0, 10_000.0, 0.0);
                a.decay_ms = fin(a.decay_ms, 0.0, 10_000.0, 0.0);
                a.sustain = fin(a.sustain, 0.0, 1.0, 1.0);
                a.release_ms = fin(a.release_ms, 0.0, 20_000.0, 30.0);
                p.choke = p.choke.min(CHOKE_GROUPS);
                if p.output.is_some_and(|o| o == 0 || o > outputs) {
                    p.output = None;
                }
                if let Some(v) = &mut p.synth {
                    v.sanitize();
                }
                p.layers.truncate(8);
                for l in &mut p.layers {
                    l.vel_lo = fin(l.vel_lo, 0.0, 1.0, 0.0);
                    l.vel_hi = fin(l.vel_hi, 0.0, 1.0, 1.0).max(l.vel_lo);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_pads_map_both_ways() {
        assert_eq!(DrumKit::key(0, 0), ROOT_KEY);
        assert_eq!(DrumKit::key(1, 3), ROOT_KEY + 19);
        assert_eq!(DrumKit::pad_of(ROOT_KEY + 19), Some((1, 3)));
        assert_eq!(DrumKit::pad_of(ROOT_KEY - 1), None);
        assert_eq!(DrumKit::pad_of(ROOT_KEY + 64), None);
        assert!(DrumKit::key(MAX_BANKS - 1, PADS_PER_BANK - 1) <= 127);
    }

    #[test]
    fn fill_spills_into_new_banks_and_layers_split_velocity() {
        let mut k = DrumKit::default();
        let s = |i: usize| SampleSource::File(format!("{i}.wav").into());
        k.fill((0..20).map(s).collect());
        assert_eq!(k.banks.len(), 2);
        assert_eq!(k.banks[1][3].layers[0].sample, s(19));
        assert!(k.banks[1][4].layers.is_empty());
        assert_eq!(k.banks[0][0].label(), "0");
        let mut p = Pad::default();
        p.set_layers(vec![s(1), s(2), s(3)]);
        assert_eq!(p.layers[0].vel_lo, 0.0);
        assert!((p.layers[1].vel_lo - 1.0 / 3.0).abs() < 1e-6);
        assert_eq!(p.layers[2].vel_hi, 1.0);
    }
}
