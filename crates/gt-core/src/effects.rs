//! Mixer effects as document data.
//!
//! An effect slot stores its kind and its parameter values in plain units, indexed in the order
//! of the kind's table below. The engine's DSP takes the same indexes (`gt_dsp::fx`), and a
//! test in gt-engine checks that the two agree. Indexes and keys never change once released.

use crate::param::ParamCurve::{Exp, Linear, Power, Stepped};
use crate::param::ParamUnit::{self, Choice, Db, Hz, Ms, Percent, Plain, Ratio, Seconds};
use crate::param::{ParamCurve, ParamInfo};
use crate::plugin::PluginRef;

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
    /// Multi-mode filter with envelope and LFO.
    Filter,
    /// Gate / expander.
    Gate,
    /// Flanger.
    Flanger,
    /// Phaser.
    Phaser,
    /// Bit and sample-rate reduction.
    Bitcrusher,
    /// Oversampled soft clipper.
    Clipper,
    /// Transient shaper.
    Transient,
    /// A third-party effect plugin (its parameters live in [`EffectSlot::plugin`]).
    Plugin,
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
/// Filter responses, indexed by value (matches `gt_dsp::fx::FilterMode`).
pub const FILTER_MODES: &[&str] = &["Low-pass", "High-pass", "Band-pass", "Notch"];
const FILTER_SLOPES: &[&str] = &["12 dB", "24 dB"];
/// Tempo-synced LFO note values (matches `gt_dsp::fx::LFO_SYNC_BEATS`).
pub const LFO_SYNC: &[&str] = &[
    "1/32", "1/16", "1/8", "1/4T", "1/4", "1/2", "1 bar", "2 bars", "4 bars",
];
/// Free rate or a note value: index 0 is free, the rest are [`LFO_SYNC`] shifted by one.
const RATE_SYNC: &[&str] = &[
    "Free", "1/32", "1/16", "1/8", "1/4T", "1/4", "1/2", "1 bar", "2 bars", "4 bars",
];
const PHASER_STAGES: &[&str] = &["2", "4", "6", "8", "10", "12"];
const CLIP_CURVES: &[&str] = &["Tanh", "Cubic", "Arctan", "Hard"];
const OVERSAMPLING: &[&str] = &["Off", "2x", "4x"];

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

static FILTER: [ParamInfo; 11] = [
    choice("mode", "Mode", FILTER_MODES, 0.0),
    choice("slope", "Slope", FILTER_SLOPES, 0.0),
    info("cutoff", "Cutoff", (20.0, 20_000.0, 2000.0), Exp, Hz),
    info("reso", "Reso", (0.0, 1.0, 0.0), Linear, Percent),
    info("drive", "Drive", (0.0, 24.0, 0.0), Linear, Db),
    info("env", "Env", (-1.0, 1.0, 0.0), Linear, Percent),
    info("attack", "Attack", (0.1, 200.0, 5.0), Exp, Ms),
    info("release", "Release", (5.0, 2000.0, 200.0), Exp, Ms),
    choice("lfo.rate", "LFO rate", LFO_SYNC, 4.0),
    info("lfo.depth", "LFO depth", (0.0, 1.0, 0.0), Linear, Percent),
    info("output", "Output", (-24.0, 12.0, 0.0), Linear, Db),
];

static GATE: [ParamInfo; 8] = [
    info("threshold", "Thresh", (-80.0, 0.0, -40.0), Linear, Db),
    info("range", "Range", (-80.0, 0.0, -80.0), Linear, Db),
    info("ratio", "Ratio", (1.0, 20.0, 20.0), Exp, Ratio),
    info("attack", "Attack", (0.01, 100.0, 0.5), Exp, Ms),
    info("hold", "Hold", (0.0, 1000.0, 20.0), Power(2.0), Ms),
    info("release", "Release", (1.0, 2000.0, 100.0), Exp, Ms),
    info("lookahead", "Look-ahead", (0.0, 10.0, 0.0), Linear, Ms),
    choice("sidechain", "Sidechain", OFF_ON, 0.0),
];

static FLANGER: [ParamInfo; 7] = [
    info("rate", "Rate", (0.02, 10.0, 0.3), Exp, Hz),
    choice("sync", "Sync", RATE_SYNC, 0.0),
    info("depth", "Depth", (0.0, 10.0, 2.0), Linear, Ms),
    info("delay", "Delay", (0.1, 10.0, 1.0), Exp, Ms),
    info("feedback", "Feedback", (-0.95, 0.95, 0.5), Linear, Percent),
    info("spread", "Spread", (0.0, 1.0, 0.25), Linear, Percent),
    info("mix", "Mix", (0.0, 1.0, 0.5), Linear, Percent),
];

static PHASER: [ParamInfo; 8] = [
    choice("stages", "Stages", PHASER_STAGES, 1.0),
    info("rate", "Rate", (0.02, 10.0, 0.5), Exp, Hz),
    choice("sync", "Sync", RATE_SYNC, 0.0),
    info("depth", "Depth", (0.0, 1.0, 0.7), Linear, Percent),
    info("center", "Centre", (50.0, 8000.0, 800.0), Exp, Hz),
    info("feedback", "Feedback", (-0.95, 0.95, 0.0), Linear, Percent),
    info("spread", "Spread", (0.0, 1.0, 0.25), Linear, Percent),
    info("mix", "Mix", (0.0, 1.0, 0.5), Linear, Percent),
];

static BITCRUSHER: [ParamInfo; 4] = [
    info("bits", "Bits", (1.0, 24.0, 8.0), Linear, Plain),
    info("rate", "Rate", (100.0, 48_000.0, 8000.0), Exp, Hz),
    choice("antialias", "Anti-alias", OFF_ON, 1.0),
    info("mix", "Mix", (0.0, 1.0, 1.0), Linear, Percent),
];

static CLIPPER: [ParamInfo; 5] = [
    info("drive", "Drive", (0.0, 24.0, 0.0), Linear, Db),
    choice("curve", "Curve", CLIP_CURVES, 0.0),
    info("ceiling", "Ceiling", (-24.0, 0.0, 0.0), Linear, Db),
    choice("oversample", "Oversample", OVERSAMPLING, 1.0),
    info("output", "Output", (-24.0, 12.0, 0.0), Linear, Db),
];

static TRANSIENT: [ParamInfo; 3] = [
    info("attack", "Attack", (-24.0, 24.0, 0.0), Linear, Db),
    info("sustain", "Sustain", (-24.0, 24.0, 0.0), Linear, Db),
    info("output", "Output", (-24.0, 12.0, 0.0), Linear, Db),
];

/// The wet/dry mix every effect slot has ([`EffectSlot::mix`]).
pub static SLOT_MIX: ParamInfo = info("slot.mix", "Slot mix", (0.0, 1.0, 1.0), Linear, Percent);

/// A named set of values for an effect; parameters it leaves out keep their defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectPreset {
    /// Shown in the slot's preset menu.
    pub name: &'static str,
    /// Parameter keys and plain values.
    pub values: &'static [(&'static str, f32)],
}

const fn preset(name: &'static str, values: &'static [(&'static str, f32)]) -> EffectPreset {
    EffectPreset { name, values }
}

static FILTER_PRESETS: [EffectPreset; 5] = [
    preset(
        "Warm low-pass",
        &[("cutoff", 1200.0), ("reso", 0.15), ("drive", 6.0)],
    ),
    preset("Thin it out", &[("mode", 1.0), ("cutoff", 250.0)]),
    preset(
        "Telephone",
        &[
            ("mode", 2.0),
            ("cutoff", 1500.0),
            ("reso", 0.3),
            ("drive", 9.0),
        ],
    ),
    preset(
        "Auto-wah",
        &[
            ("mode", 2.0),
            ("cutoff", 300.0),
            ("reso", 0.5),
            ("env", 0.8),
            ("attack", 3.0),
            ("release", 120.0),
        ],
    ),
    preset(
        "Slow sweep",
        &[
            ("slope", 1.0),
            ("cutoff", 800.0),
            ("reso", 0.35),
            ("lfo.rate", 7.0),
            ("lfo.depth", 0.5),
        ],
    ),
];
static GATE_PRESETS: [EffectPreset; 3] = [
    preset(
        "Tight drums",
        &[
            ("threshold", -30.0),
            ("attack", 0.1),
            ("hold", 10.0),
            ("release", 60.0),
        ],
    ),
    preset(
        "Vocal clean-up",
        &[
            ("threshold", -50.0),
            ("range", -20.0),
            ("ratio", 3.0),
            ("release", 200.0),
        ],
    ),
    preset(
        "Trance gate (sidechain)",
        &[("threshold", -30.0), ("sidechain", 1.0), ("release", 30.0)],
    ),
];
static FLANGER_PRESETS: [EffectPreset; 3] = [
    preset("Jet", &[("rate", 0.1), ("depth", 4.0), ("feedback", 0.8)]),
    preset(
        "Bar sweep",
        &[("sync", 7.0), ("depth", 3.0), ("feedback", 0.6)],
    ),
    preset(
        "Metallic",
        &[
            ("delay", 0.3),
            ("depth", 0.5),
            ("feedback", -0.85),
            ("rate", 2.0),
        ],
    ),
];
static PHASER_PRESETS: [EffectPreset; 3] = [
    preset("Classic four", &[]),
    preset(
        "Deep twelve",
        &[
            ("stages", 5.0),
            ("rate", 0.2),
            ("feedback", 0.6),
            ("depth", 0.9),
        ],
    ),
    preset(
        "Eighth-note swirl",
        &[("stages", 2.0), ("sync", 3.0), ("feedback", 0.4)],
    ),
];
static BITCRUSHER_PRESETS: [EffectPreset; 3] = [
    preset(
        "8-bit console",
        &[("bits", 8.0), ("rate", 16_000.0), ("antialias", 0.0)],
    ),
    preset("Dusty sampler", &[("bits", 12.0), ("rate", 26_000.0)]),
    preset(
        "Destroyed",
        &[
            ("bits", 3.0),
            ("rate", 2000.0),
            ("antialias", 0.0),
            ("mix", 0.6),
        ],
    ),
];
static CLIPPER_PRESETS: [EffectPreset; 3] = [
    preset("Gentle glue", &[("drive", 3.0), ("curve", 1.0)]),
    preset(
        "Loud master",
        &[("drive", 6.0), ("ceiling", -0.3), ("oversample", 2.0)],
    ),
    preset(
        "Hard edge",
        &[
            ("drive", 9.0),
            ("curve", 3.0),
            ("oversample", 2.0),
            ("output", -3.0),
        ],
    ),
];
static TRANSIENT_PRESETS: [EffectPreset; 3] = [
    preset("Punchy drums", &[("attack", 8.0), ("sustain", -3.0)]),
    preset("Tame the click", &[("attack", -8.0)]),
    preset("Room boost", &[("sustain", 9.0)]),
];

impl EffectKind {
    /// Every built-in kind, in menu order.
    pub const ALL: [EffectKind; 15] = [
        Self::Eq,
        Self::Compressor,
        Self::Gate,
        Self::Limiter,
        Self::Clipper,
        Self::Transient,
        Self::Filter,
        Self::Distortion,
        Self::Bitcrusher,
        Self::Delay,
        Self::Reverb,
        Self::Chorus,
        Self::Flanger,
        Self::Phaser,
        Self::Width,
    ];

    /// Built-in presets (none for the older effects yet).
    pub fn presets(self) -> &'static [EffectPreset] {
        match self {
            Self::Filter => &FILTER_PRESETS,
            Self::Gate => &GATE_PRESETS,
            Self::Flanger => &FLANGER_PRESETS,
            Self::Phaser => &PHASER_PRESETS,
            Self::Bitcrusher => &BITCRUSHER_PRESETS,
            Self::Clipper => &CLIPPER_PRESETS,
            Self::Transient => &TRANSIENT_PRESETS,
            _ => &[],
        }
    }

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
            Self::Filter => "filter",
            Self::Gate => "gate",
            Self::Flanger => "flanger",
            Self::Phaser => "phaser",
            Self::Bitcrusher => "bitcrusher",
            Self::Clipper => "clipper",
            Self::Transient => "transient",
            Self::Plugin => "plugin",
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
            Self::Filter => "Filter",
            Self::Gate => "Gate / Expander",
            Self::Flanger => "Flanger",
            Self::Phaser => "Phaser",
            Self::Bitcrusher => "Bitcrusher",
            Self::Clipper => "Soft Clipper",
            Self::Transient => "Transient Shaper",
            Self::Plugin => "Plugin",
        }
    }

    /// The kind with a file key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .chain([Self::Plugin])
            .find(|k| k.key() == key)
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
            Self::Filter => &FILTER,
            Self::Gate => &GATE,
            Self::Flanger => &FLANGER,
            Self::Phaser => &PHASER,
            Self::Bitcrusher => &BITCRUSHER,
            Self::Clipper => &CLIPPER,
            Self::Transient => &TRANSIENT,
            Self::Plugin => &[],
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
    /// Values in the order of `kind.params()` (empty for a plugin).
    pub params: Vec<f32>,
    /// Wet/dry mix of the whole slot, 0 to 1 (1 = only the effect). At 0 the slot is skipped
    /// exactly as when bypassed.
    pub mix: f32,
    /// The plugin, when `kind` is [`EffectKind::Plugin`].
    pub plugin: Option<Box<PluginRef>>,
}

impl EffectSlot {
    /// An enabled effect at its defaults.
    pub fn new(kind: EffectKind) -> Self {
        Self {
            kind,
            enabled: true,
            params: kind.params().iter().map(|p| p.default).collect(),
            mix: 1.0,
            plugin: None,
        }
    }

    /// An enabled slot holding an effect plugin.
    pub fn plugin(plugin: PluginRef) -> Self {
        Self {
            kind: EffectKind::Plugin,
            enabled: true,
            params: Vec::new(),
            mix: 1.0,
            plugin: Some(Box::new(plugin)),
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

    /// Sets every parameter to its default, then applies `preset`.
    pub fn apply_preset(&mut self, preset: &EffectPreset) {
        for (v, p) in self.params.iter_mut().zip(self.kind.params()) {
            *v = p.default;
        }
        for &(key, value) in preset.values {
            if let Some(i) = self.kind.params().iter().position(|p| p.key == key) {
                self.params[i] = self.kind.params()[i].clamp(value);
            }
        }
    }

    /// Fixes the parameter count and clamps every value into range.
    pub fn sanitize(&mut self) {
        self.mix = if self.mix.is_finite() {
            self.mix.clamp(0.0, 1.0)
        } else {
            1.0
        };
        if self.kind != EffectKind::Plugin {
            self.plugin = None;
        }
        if let Some(p) = &mut self.plugin {
            p.sanitize();
        }
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
            for preset in kind.presets() {
                for (key, value) in preset.values {
                    let p = t.iter().find(|p| p.key == *key);
                    assert!(
                        p.is_some(),
                        "{} preset {} has no {key}",
                        kind.key(),
                        preset.name
                    );
                    assert_eq!(p.unwrap().clamp(*value), *value, "{} {key}", preset.name);
                }
            }
        }
        assert_eq!(EffectKind::Filter.params()[8].choices.len(), 9);
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
