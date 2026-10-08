//! Mixer effects as document data.
//!
//! An effect slot stores its kind and its parameter values in plain units, indexed in the order
//! of the kind's table below. The engine's DSP takes the same indexes (`gt_dsp::fx`), and a
//! test in gt-engine checks that the two agree. Indexes and keys never change once released.

use crate::param::ParamCurve::{Exp, Linear, Power, Stepped};
use crate::param::ParamUnit::{self, Choice, Db, Hz, Ms, Percent, Plain, Ratio, Seconds};
use crate::param::{ParamCurve, ParamInfo};

/// The built-in effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectKind {
    /// 8-band parametric EQ.
    Eq,
    /// Compressor with sidechain input.
    Compressor,
    /// Tempo-synced (ping-pong) delay.
    Delay,
    /// Reverb.
    Reverb,
    /// Chorus.
    Chorus,
    /// Waveshaping distortion.
    Distortion,
    /// Brickwall limiter.
    Limiter,
    /// Stereo width.
    Width,
}

const fn info(
    key: &'static str,
    name: &'static str,
    (min, max, default): (f32, f32, f32),
    curve: ParamCurve,
    unit: ParamUnit,
) -> ParamInfo {
    ParamInfo {
        key,
        name,
        min,
        max,
        default,
        curve,
        unit,
        choices: &[],
    }
}

const fn choice(
    key: &'static str,
    name: &'static str,
    choices: &'static [&'static str],
    default: f32,
) -> ParamInfo {
    ParamInfo {
        key,
        name,
        min: 0.0,
        max: (choices.len() - 1) as f32,
        default,
        curve: Stepped,
        unit: Choice,
        choices,
    }
}

/// EQ band shapes, indexed by value (matches `gt_dsp::fx::BandType`).
pub const EQ_BAND_TYPES: &[&str] = &[
    "Off",
    "Bell",
    "Low shelf",
    "High shelf",
    "Low cut",
    "High cut",
    "Notch",
];
/// Delay note values, indexed by value (matches `gt_dsp::fx::DELAY_DIVISIONS`).
pub const DELAY_TIMES: &[&str] = &[
    "1/32", "1/16T", "1/16", "1/16.", "1/8T", "1/8", "1/8.", "1/4T", "1/4", "1/4.", "1/2", "1 bar",
];
/// Distortion curves, indexed by value.
pub const DISTORTION_SHAPES: &[&str] = &["Soft", "Hard", "Fold", "Tube"];
const OFF_ON: &[&str] = &["Off", "On"];

/// Number of EQ bands.
pub const EQ_BANDS: usize = 8;

macro_rules! eq_band {
    ($n:literal, $type:expr, $freq:expr, $q:expr) => {
        [
            choice(concat!("b", $n, ".type"), "Type", EQ_BAND_TYPES, $type),
            info(
                concat!("b", $n, ".freq"),
                "Freq",
                (20.0, 20_000.0, $freq),
                Exp,
                Hz,
            ),
            info(
                concat!("b", $n, ".gain"),
                "Gain",
                (-24.0, 24.0, 0.0),
                Linear,
                Db,
            ),
            info(concat!("b", $n, ".q"), "Q", (0.1, 18.0, $q), Exp, Plain),
        ]
    };
}

const fn flatten<const N: usize>(
    bands: [[ParamInfo; 4]; EQ_BANDS],
    output: ParamInfo,
) -> [ParamInfo; N] {
    let mut out = [output; N];
    let mut i = 0;
    while i < EQ_BANDS {
        let mut j = 0;
        while j < 4 {
            out[i * 4 + j] = bands[i][j];
            j += 1;
        }
        i += 1;
    }
    out
}

static EQ: [ParamInfo; EQ_BANDS * 4 + 1] = flatten(
    [
        eq_band!(1, 0.0, 30.0, 0.707),
        eq_band!(2, 2.0, 100.0, 0.707),
        eq_band!(3, 1.0, 250.0, 1.0),
        eq_band!(4, 1.0, 700.0, 1.0),
        eq_band!(5, 1.0, 1800.0, 1.0),
        eq_band!(6, 1.0, 4000.0, 1.0),
        eq_band!(7, 3.0, 9000.0, 0.707),
        eq_band!(8, 0.0, 16_000.0, 0.707),
    ],
    info("output", "Output", (-24.0, 24.0, 0.0), Linear, Db),
);

static COMPRESSOR: [ParamInfo; 8] = [
    info("threshold", "Thresh", (-60.0, 0.0, -18.0), Linear, Db),
    info("ratio", "Ratio", (1.0, 20.0, 4.0), Exp, Ratio),
    info("attack", "Attack", (0.1, 200.0, 10.0), Exp, Ms),
    info("release", "Release", (5.0, 2000.0, 120.0), Exp, Ms),
    info("knee", "Knee", (0.0, 24.0, 6.0), Linear, Db),
    info("makeup", "Makeup", (0.0, 24.0, 0.0), Linear, Db),
    info("mix", "Mix", (0.0, 1.0, 1.0), Linear, Percent),
    choice("sidechain", "Sidechain", OFF_ON, 0.0),
];

static DELAY: [ParamInfo; 5] = [
    choice("time", "Time", DELAY_TIMES, 6.0),
    info("feedback", "Feedback", (0.0, 0.95, 0.4), Linear, Percent),
    choice("pingpong", "Ping-pong", OFF_ON, 1.0),
    info("tone", "Tone", (500.0, 20_000.0, 6000.0), Exp, Hz),
    info("mix", "Mix", (0.0, 1.0, 0.3), Linear, Percent),
];

static REVERB: [ParamInfo; 6] = [
    info("size", "Size", (0.0, 1.0, 0.6), Linear, Percent),
    info("decay", "Decay", (0.1, 30.0, 2.5), Exp, Seconds),
    info("damping", "Damping", (500.0, 20_000.0, 6000.0), Exp, Hz),
    info("predelay", "Pre-delay", (0.0, 250.0, 20.0), Power(2.0), Ms),
    info("width", "Width", (0.0, 1.0, 1.0), Linear, Percent),
    info("mix", "Mix", (0.0, 1.0, 0.25), Linear, Percent),
];

static CHORUS: [ParamInfo; 5] = [
    info("rate", "Rate", (0.05, 8.0, 0.6), Exp, Hz),
    info("depth", "Depth", (0.0, 10.0, 3.0), Linear, Ms),
    info("delay", "Delay", (1.0, 30.0, 12.0), Linear, Ms),
    info("spread", "Spread", (0.0, 1.0, 0.5), Linear, Percent),
    info("mix", "Mix", (0.0, 1.0, 0.5), Linear, Percent),
];

static DISTORTION: [ParamInfo; 5] = [
    info("drive", "Drive", (0.0, 48.0, 12.0), Linear, Db),
    choice("shape", "Shape", DISTORTION_SHAPES, 0.0),
    info("tone", "Tone", (500.0, 20_000.0, 12_000.0), Exp, Hz),
    info("output", "Output", (-36.0, 12.0, -6.0), Linear, Db),
    info("mix", "Mix", (0.0, 1.0, 1.0), Linear, Percent),
];

static LIMITER: [ParamInfo; 3] = [
    info("input", "Gain", (0.0, 24.0, 0.0), Linear, Db),
    info("ceiling", "Ceiling", (-24.0, 0.0, -0.3), Linear, Db),
    info("release", "Release", (1.0, 2000.0, 100.0), Exp, Ms),
];

static WIDTH: [ParamInfo; 2] = [
    info("width", "Width", (0.0, 2.0, 1.0), Linear, Percent),
    info("output", "Output", (-24.0, 12.0, 0.0), Linear, Db),
];

impl EffectKind {
    /// Every kind, in menu order.
    pub const ALL: [EffectKind; 8] = [
        Self::Eq,
        Self::Compressor,
        Self::Delay,
        Self::Reverb,
        Self::Chorus,
        Self::Distortion,
        Self::Limiter,
        Self::Width,
    ];

    /// Stable key for files.
    pub fn key(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Compressor => "compressor",
            Self::Delay => "delay",
            Self::Reverb => "reverb",
            Self::Chorus => "chorus",
            Self::Distortion => "distortion",
            Self::Limiter => "limiter",
            Self::Width => "width",
        }
    }

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Eq => "Parametric EQ",
            Self::Compressor => "Compressor",
            Self::Delay => "Delay",
            Self::Reverb => "Reverb",
            Self::Chorus => "Chorus",
            Self::Distortion => "Distortion",
            Self::Limiter => "Limiter",
            Self::Width => "Stereo Width",
        }
    }

    /// The kind with a file key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }

    /// Parameter table, in index order.
    pub fn params(self) -> &'static [ParamInfo] {
        match self {
            Self::Eq => &EQ,
            Self::Compressor => &COMPRESSOR,
            Self::Delay => &DELAY,
            Self::Reverb => &REVERB,
            Self::Chorus => &CHORUS,
            Self::Distortion => &DISTORTION,
            Self::Limiter => &LIMITER,
            Self::Width => &WIDTH,
        }
    }
}

/// One of a mixer strip's effect slots.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectSlot {
    /// Which effect.
    pub kind: EffectKind,
    /// Bypassed when false.
    pub enabled: bool,
    /// Values in the order of `kind.params()`.
    pub params: Vec<f32>,
}

impl EffectSlot {
    /// An enabled effect at its defaults.
    pub fn new(kind: EffectKind) -> Self {
        Self {
            kind,
            enabled: true,
            params: kind.params().iter().map(|p| p.default).collect(),
        }
    }

    /// Builder: sets parameter `key`.
    pub fn with(mut self, key: &str, value: f32) -> Self {
        if let Some(i) = self.kind.params().iter().position(|p| p.key == key) {
            self.params[i] = self.kind.params()[i].clamp(value);
        }
        self
    }

    /// Value of parameter `key`, if the kind has it.
    pub fn get(&self, key: &str) -> Option<f32> {
        let i = self.kind.params().iter().position(|p| p.key == key)?;
        self.params.get(i).copied()
    }

    /// Fixes the parameter count and clamps every value into range.
    pub fn sanitize(&mut self) {
        let table = self.kind.params();
        self.params.resize(table.len(), 0.0);
        for (v, info) in self.params.iter_mut().zip(table) {
            *v = info.clamp(*v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_consistent() {
        for kind in EffectKind::ALL {
            let t = kind.params();
            for (i, p) in t.iter().enumerate() {
                assert!(p.min < p.max, "{} {}", kind.key(), p.key);
                assert_eq!(p.clamp(p.default), p.default, "{} {}", kind.key(), p.key);
                assert!(
                    t[..i].iter().all(|q| q.key != p.key),
                    "duplicate key {} in {}",
                    p.key,
                    kind.key()
                );
            }
            assert_eq!(EffectKind::from_key(kind.key()), Some(kind));
        }
        assert_eq!(EffectKind::Eq.params().len(), 33);
        assert_eq!(EffectKind::Eq.params()[13].key, "b4.freq");
        assert_eq!(EffectKind::Eq.params()[32].key, "output");
    }

    #[test]
    fn slots_build_and_sanitize() {
        let mut s = EffectSlot::new(EffectKind::Reverb).with("decay", 99.0);
        assert_eq!(s.get("decay"), Some(30.0));
        s.params.pop();
        s.params[0] = f32::NAN;
        s.sanitize();
        assert_eq!(s.params.len(), 6);
        assert_eq!(s.params[0], 0.6);
    }
}
