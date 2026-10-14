//! Preset player (Prompt 28): a multi-sample instrument loaded from a sound pack, played
//! through the sampler with up to eight macro knobs on top.
//!
//! The pack's preset gives the base sampler settings; each macro maps its 0..1 knob onto one
//! sampler setting between its `min` and `max`. The engine plays [`Player::sampler`], the base
//! with the macros applied, so turning a knob never loses the preset's own values.

use std::path::PathBuf;

use crate::project::SamplerSettings;
use crate::sampler::{LfoTarget, SamplerFilterMode};

/// Most macros a preset can have.
pub const MAX_MACROS: usize = 8;

/// The sampler setting a macro moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MacroTarget {
    /// Filter cutoff in Hz (moved on a log scale; turns the low-pass on if the filter is off).
    #[default]
    Cutoff,
    /// Filter resonance, 0 to 1.
    Resonance,
    /// Filter envelope amount, octaves.
    FilterEnv,
    /// Amplitude attack, ms.
    Attack,
    /// Amplitude decay, ms.
    Decay,
    /// Amplitude sustain, 0 to 1.
    Sustain,
    /// Amplitude release, ms.
    Release,
    /// Transposition, semitones.
    Tune,
    /// Every zone's gain, linear.
    Volume,
    /// First LFO's rate, Hz.
    LfoRate,
    /// First LFO's depth, 0 to 1 (moves the pitch unless the LFO already has a target).
    LfoDepth,
    /// Sample start, fraction of each zone.
    Start,
}

impl MacroTarget {
    /// Every target.
    pub const ALL: [MacroTarget; 12] = [
        Self::Cutoff,
        Self::Resonance,
        Self::FilterEnv,
        Self::Attack,
        Self::Decay,
        Self::Sustain,
        Self::Release,
        Self::Tune,
        Self::Volume,
        Self::LfoRate,
        Self::LfoDepth,
        Self::Start,
    ];

    /// Name in files and pack mappings.
    pub fn key(self) -> &'static str {
        match self {
            Self::Cutoff => "cutoff",
            Self::Resonance => "resonance",
            Self::FilterEnv => "filter_env",
            Self::Attack => "attack",
            Self::Decay => "decay",
            Self::Sustain => "sustain",
            Self::Release => "release",
            Self::Tune => "tune",
            Self::Volume => "volume",
            Self::LfoRate => "lfo_rate",
            Self::LfoDepth => "lfo_depth",
            Self::Start => "start",
        }
    }

    /// The target named `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.key() == key)
    }

    /// The values the target accepts.
    pub fn range(self) -> (f32, f32) {
        match self {
            Self::Cutoff => (20.0, 20_000.0),
            Self::Resonance | Self::Sustain | Self::LfoDepth => (0.0, 1.0),
            Self::FilterEnv => (-8.0, 8.0),
            Self::Attack | Self::Decay | Self::Release => (0.0, 10_000.0),
            Self::Tune => (-48.0, 48.0),
            Self::Volume => (0.0, 2.0),
            Self::LfoRate => (0.01, 50.0),
            Self::Start => (0.0, 0.99),
        }
    }
}

/// One macro knob.
#[derive(Debug, Clone, PartialEq)]
pub struct Macro {
    /// Label under the knob.
    pub name: String,
    /// What it moves.
    pub target: MacroTarget,
    /// Target value at the knob's minimum.
    pub min: f32,
    /// Target value at the knob's maximum.
    pub max: f32,
    /// Knob position, 0 to 1.
    pub value: f32,
}

impl Macro {
    /// The target value at the knob's position.
    pub fn target_value(&self) -> f32 {
        let v = self.value.clamp(0.0, 1.0);
        if self.target == MacroTarget::Cutoff && self.min > 0.0 && self.max > 0.0 {
            self.min * (self.max / self.min).powf(v)
        } else {
            self.min + (self.max - self.min) * v
        }
    }

    /// Clamp everything into range.
    pub fn sanitize(&mut self) {
        let (lo, hi) = self.target.range();
        let fin = |x: f32, d: f32| if x.is_finite() { x.clamp(lo, hi) } else { d };
        self.min = fin(self.min, lo);
        self.max = fin(self.max, hi);
        self.value = if self.value.is_finite() {
            self.value.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.name.truncate(32);
    }
}

/// A preset-player channel.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Player {
    /// The pack file (a zip).
    pub pack: PathBuf,
    /// Name of the pack's preset this plays.
    pub preset: String,
    /// The preset's sampler settings, before the macros; its zones point into the pack's
    /// extracted samples.
    pub base: SamplerSettings,
    /// Macro knobs, at most [`MAX_MACROS`].
    pub macros: Vec<Macro>,
}

impl Player {
    /// The sampler settings to play: the base with every macro applied in order (a later
    /// macro on the same target wins).
    pub fn sampler(&self) -> SamplerSettings {
        let mut s = self.base.clone();
        for m in &self.macros {
            let v = m.target_value();
            match m.target {
                MacroTarget::Cutoff => {
                    if s.filter.mode == SamplerFilterMode::Off {
                        s.filter.mode = SamplerFilterMode::LowPass;
                    }
                    s.filter.cutoff = v;
                }
                MacroTarget::Resonance => s.filter.resonance = v,
                MacroTarget::FilterEnv => s.filter.env_amount = v,
                MacroTarget::Attack => s.adsr.attack_ms = v,
                MacroTarget::Decay => s.adsr.decay_ms = v,
                MacroTarget::Sustain => s.adsr.sustain = v,
                MacroTarget::Release => s.adsr.release_ms = v,
                MacroTarget::Tune => s.pitch = v,
                MacroTarget::Volume => s.zones.iter_mut().for_each(|z| z.gain *= v),
                MacroTarget::LfoRate => s.lfos[0].rate = v,
                MacroTarget::LfoDepth => {
                    if s.lfos[0].target == LfoTarget::Off {
                        s.lfos[0].target = LfoTarget::Pitch;
                    }
                    s.lfos[0].amount = v;
                }
                MacroTarget::Start => s.start = v.min(s.end - 0.01).max(0.0),
            }
        }
        s
    }

    /// Display name: the preset's.
    pub fn label(&self) -> &str {
        &self.preset
    }

    /// Clamp the macros (the base is sanitized like a sampler's).
    pub fn sanitize(&mut self) {
        self.macros.truncate(MAX_MACROS);
        self.macros.iter_mut().for_each(Macro::sanitize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn knob(target: MacroTarget, min: f32, max: f32, value: f32) -> Macro {
        Macro {
            name: target.key().into(),
            target,
            min,
            max,
            value,
        }
    }

    #[test]
    fn macros_move_their_targets_without_touching_the_base() {
        let mut p = Player::default();
        p.macros
            .push(knob(MacroTarget::Cutoff, 200.0, 20_000.0, 0.5));
        p.macros
            .push(knob(MacroTarget::Release, 10.0, 1010.0, 0.25));
        let s = p.sampler();
        assert_eq!(s.filter.mode, SamplerFilterMode::LowPass);
        assert!(
            (s.filter.cutoff - 2000.0).abs() < 1.0,
            "log scale: {}",
            s.filter.cutoff
        );
        assert!((s.adsr.release_ms - 260.0).abs() < 1e-3);
        assert_eq!(p.base, SamplerSettings::default());
    }

    #[test]
    fn sanitize_keeps_eight_macros_in_range() {
        let mut p = Player {
            macros: vec![knob(MacroTarget::Resonance, -3.0, f32::NAN, 9.0); 12],
            ..Player::default()
        };
        p.sanitize();
        assert_eq!(p.macros.len(), MAX_MACROS);
        assert_eq!(
            (p.macros[0].min, p.macros[0].max, p.macros[0].value),
            (0.0, 1.0, 1.0)
        );
        assert!(MacroTarget::ALL
            .iter()
            .all(|t| MacroTarget::from_key(t.key()) == Some(*t)));
    }
}
