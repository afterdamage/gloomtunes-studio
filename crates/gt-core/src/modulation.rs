//! Per-parameter modulation: LFOs and envelope followers attached to any [`ParamId`].
//!
//! A modulator adds `amount · source` to its target's normalized value (0..1 of the knob's
//! travel), on top of the knob or its automation, and the sum is clamped to 0..1. An LFO is
//! bipolar (-1..1), so an amount of 0.25 swings the knob a quarter of its travel either way. An
//! envelope follower is unipolar (0..1): it tracks the level of a mixer strip's output, so a
//! negative amount ducks the target when that strip gets loud. Several modulators on one target
//! add up.

use crate::mixer::STRIPS;
use crate::params::ParamId;

/// Stable identity of a modulator. Never reused within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModulatorId(pub u32);

/// Most modulators a project can hold (the engine preallocates their state).
pub const MAX_MODULATORS: usize = 64;

/// LFO waveform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LfoShape {
    /// Sine.
    #[default]
    Sine,
    /// Triangle.
    Triangle,
    /// Rising saw.
    SawUp,
    /// Falling saw.
    SawDown,
    /// Square.
    Square,
    /// A new random level each cycle.
    SampleHold,
}

impl LfoShape {
    /// Every shape, in menu order.
    pub const ALL: [LfoShape; 6] = [
        Self::Sine,
        Self::Triangle,
        Self::SawUp,
        Self::SawDown,
        Self::Square,
        Self::SampleHold,
    ];

    /// Menu name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Sine => "Sine",
            Self::Triangle => "Triangle",
            Self::SawUp => "Saw up",
            Self::SawDown => "Saw down",
            Self::Square => "Square",
            Self::SampleHold => "S&H",
        }
    }

    /// File key.
    pub fn key(self) -> &'static str {
        match self {
            Self::Sine => "sine",
            Self::Triangle => "triangle",
            Self::SawUp => "saw-up",
            Self::SawDown => "saw-down",
            Self::Square => "square",
            Self::SampleHold => "sample-hold",
        }
    }

    /// Output in -1..1 at `phase` (0..1, wrapped); `held` is the current S&H level.
    pub fn value(self, phase: f64, held: f32) -> f32 {
        let p = phase.rem_euclid(1.0) as f32;
        match self {
            Self::Sine => (p * std::f32::consts::TAU).sin(),
            Self::Triangle => {
                if p < 0.25 {
                    4.0 * p
                } else if p < 0.75 {
                    2.0 - 4.0 * p
                } else {
                    4.0 * p - 4.0
                }
            }
            Self::SawUp => 2.0 * p - 1.0,
            Self::SawDown => 1.0 - 2.0 * p,
            Self::Square => {
                if p < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            Self::SampleHold => held,
        }
    }
}

/// Tempo-synced LFO periods: name and length in quarter notes.
pub const SYNC_RATES: &[(&str, f64)] = &[
    ("4 bars", 16.0),
    ("2 bars", 8.0),
    ("1 bar", 4.0),
    ("1/2", 2.0),
    ("1/2 T", 4.0 / 3.0),
    ("1/4", 1.0),
    ("1/4 T", 2.0 / 3.0),
    ("1/8", 0.5),
    ("1/8 T", 1.0 / 3.0),
    ("1/16", 0.25),
];

/// How fast an LFO runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LfoRate {
    /// Free-running, in Hz (0.01 to 40).
    Hz(f32),
    /// Locked to the song position: one cycle per `SYNC_RATES[i]` quarter notes.
    Sync(usize),
}

impl LfoRate {
    /// Slowest and fastest free rate.
    pub const HZ_RANGE: (f32, f32) = (0.01, 40.0);

    /// Display text: "2.50 Hz" or "1/8".
    pub fn label(self) -> String {
        match self {
            Self::Hz(hz) if hz < 10.0 => format!("{hz:.2} Hz"),
            Self::Hz(hz) => format!("{hz:.1} Hz"),
            Self::Sync(i) => SYNC_RATES.get(i).map_or("?", |r| r.0).to_owned(),
        }
    }

    /// Cycles per quarter note for a synced rate, or `None` for a free one.
    pub fn cycles_per_beat(self) -> Option<f64> {
        match self {
            Self::Sync(i) => Some(1.0 / SYNC_RATES.get(i).map_or(1.0, |r| r.1)),
            Self::Hz(_) => None,
        }
    }
}

/// What drives a modulator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModSourceKind {
    /// A low-frequency oscillator.
    Lfo {
        /// Waveform.
        shape: LfoShape,
        /// Speed.
        rate: LfoRate,
        /// Start phase, 0..1 of a cycle (where a synced LFO is at bar 1).
        phase: f32,
    },
    /// The level of a mixer strip's output.
    Follower {
        /// Strip whose post-fader output is followed.
        strip: usize,
        /// Rise time in milliseconds (to 63 %).
        attack_ms: f32,
        /// Fall time in milliseconds (to 37 %).
        release_ms: f32,
        /// Input gain: a peak level of `1 / gain` drives the follower to 1.
        gain: f32,
    },
}

impl ModSourceKind {
    /// A sine LFO at 1/4 note, phase 0.
    pub fn default_lfo() -> Self {
        Self::Lfo {
            shape: LfoShape::Sine,
            rate: LfoRate::Sync(5),
            phase: 0.0,
        }
    }

    /// A follower of `strip` with a fast attack and a 150 ms release.
    pub fn default_follower(strip: usize) -> Self {
        Self::Follower {
            strip,
            attack_ms: 5.0,
            release_ms: 150.0,
            gain: 2.0,
        }
    }

    /// "LFO" or "Follower".
    pub fn name(&self) -> &'static str {
        match self {
            Self::Lfo { .. } => "LFO",
            Self::Follower { .. } => "Follower",
        }
    }

    /// Clamps every field into range (after loading a file or a sloppy edit).
    pub fn sanitize(&mut self) {
        let fin = |v: f32, d: f32| if v.is_finite() { v } else { d };
        match self {
            Self::Lfo { rate, phase, .. } => {
                *phase = fin(*phase, 0.0).rem_euclid(1.0);
                *rate = match *rate {
                    LfoRate::Hz(hz) => {
                        LfoRate::Hz(fin(hz, 1.0).clamp(LfoRate::HZ_RANGE.0, LfoRate::HZ_RANGE.1))
                    }
                    LfoRate::Sync(i) => LfoRate::Sync(i.min(SYNC_RATES.len() - 1)),
                };
            }
            Self::Follower {
                strip,
                attack_ms,
                release_ms,
                gain,
            } => {
                *strip = (*strip).min(STRIPS - 1);
                *attack_ms = fin(*attack_ms, 5.0).clamp(0.1, 1000.0);
                *release_ms = fin(*release_ms, 150.0).clamp(1.0, 5000.0);
                *gain = fin(*gain, 1.0).clamp(0.1, 100.0);
            }
        }
    }
}

/// One modulator attached to a parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Modulator {
    /// Identity.
    pub id: ModulatorId,
    /// The parameter it moves.
    pub target: ParamId,
    /// What drives it.
    pub source: ModSourceKind,
    /// Depth in normalized units, -1 to 1.
    pub amount: f32,
    /// Off: the modulator is kept but does nothing.
    pub enabled: bool,
}

impl Modulator {
    /// Clamps every field into range.
    pub fn sanitize(&mut self) {
        self.amount = if self.amount.is_finite() {
            self.amount.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        self.source.sanitize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_cover_minus_one_to_one() {
        for shape in LfoShape::ALL {
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for k in 0..1000 {
                let v = shape.value(k as f64 / 1000.0, 0.5);
                lo = lo.min(v);
                hi = hi.max(v);
            }
            if shape == LfoShape::SampleHold {
                assert_eq!((lo, hi), (0.5, 0.5));
            } else {
                assert!(
                    lo <= -0.99 && hi >= 0.99 && lo >= -1.0 && hi <= 1.0,
                    "{shape:?}"
                );
            }
        }
        // Sine and triangle start at 0 rising; the saws at their ends.
        assert!(LfoShape::Triangle.value(0.0, 0.0).abs() < 1e-6);
        assert!((LfoShape::Triangle.value(0.25, 0.0) - 1.0).abs() < 1e-6);
        assert!((LfoShape::Triangle.value(0.75, 0.0) + 1.0).abs() < 1e-6);
        assert_eq!(LfoShape::SawUp.value(0.0, 0.0), -1.0);
        assert_eq!(LfoShape::SawDown.value(0.0, 0.0), 1.0);
        assert_eq!(
            LfoShape::Sine.value(1.25, 0.0),
            LfoShape::Sine.value(0.25, 0.0)
        );
    }

    #[test]
    fn rates_and_sanitize() {
        assert_eq!(LfoRate::Sync(5).label(), "1/4");
        assert_eq!(LfoRate::Sync(7).cycles_per_beat(), Some(2.0));
        assert_eq!(LfoRate::Hz(2.5).label(), "2.50 Hz");
        let mut s = ModSourceKind::Lfo {
            shape: LfoShape::Sine,
            rate: LfoRate::Hz(f32::NAN),
            phase: 1.25,
        };
        s.sanitize();
        assert_eq!(
            s,
            ModSourceKind::Lfo {
                shape: LfoShape::Sine,
                rate: LfoRate::Hz(1.0),
                phase: 0.25
            }
        );
        let mut f = ModSourceKind::default_follower(999);
        f.sanitize();
        assert!(matches!(f, ModSourceKind::Follower { strip, .. } if strip == STRIPS - 1));
    }
}
