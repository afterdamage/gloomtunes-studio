//! Mixer effects as document data.
//!
//! An effect slot stores its kind and its parameter values in plain units, indexed in the order
//! of the kind's table below. The engine's DSP takes the same indexes (`gt_dsp::fx`), and a
//! test in gt-engine checks that the two agree. Indexes and keys never change once released.

use crate::param::ParamCurve::{Exp, Linear, Power, Stepped};
use crate::param::ParamUnit::{
    self, Cents, Choice, Db, Hz, Ms, Percent, Plain, Ratio, Seconds, Semitones,
};
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
    /// Multiband compressor and maximizer.
    Multiband,
    /// Spectrum, oscilloscope, correlation and tuner (passes audio through).
    Analyzer,
    /// Convolution reverb.
    Convolution,
    /// Pitch shifter and pitch correction.
    PitchShift,
    /// Channel vocoder.
    Vocoder,
    /// Tempo-synced stutter and gate.
    Stutter,
    /// A third-party effect plugin (its parameters live in [`EffectSlot::plugin`]).
    Plugin,
    /// A modular patch (Prompt 34, in [`EffectSlot::patch`]).
    Patcher,
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
const MULTIBAND_BANDS: &[&str] = &["3", "4"];
/// Convolution responses, indexed by value: the generated ones, then the user's own file.
pub const IMPULSE_RESPONSES: &[&str] = &["Room", "Hall", "Plate", "Spring", "User file"];
const PITCH_MODES: &[&str] = &["Shift", "Correct"];
/// Note names for a scale root.
pub const NOTE_NAMES: &[&str] = &[
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];
/// Pitch-correction scales (matches `gt_dsp::fx::SCALE_INTERVALS`; the piano roll's scales).
pub const PITCH_SCALES: &[&str] = &[
    "Chromatic",
    "Major",
    "Minor",
    "Harmonic minor",
    "Dorian",
    "Phrygian",
    "Mixolydian",
    "Major pentatonic",
    "Minor pentatonic",
    "Blues",
];
const VOCODER_BANDS: &[&str] = &["8", "12", "16", "24", "32"];
const VOCODER_CARRIERS: &[&str] = &["Saw", "Noise", "Sidechain"];
/// What a stutter step does, indexed by value.
pub const STUTTER_STEPS: &[&str] = &["Off", "On", "Repeat", "Half"];
const STUTTER_LENGTHS: &[&str] = &["1/32", "1/16", "1/8", "1/4"];
/// Steps in the stutter pattern.
pub const STUTTER_PATTERN: usize = 16;

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

macro_rules! mb_band {
    ($n:literal, $threshold:expr) => {
        [
            info(
                concat!("b", $n, ".threshold"),
                concat!("Thresh ", $n),
                (-60.0, 0.0, $threshold),
                Linear,
                Db,
            ),
            info(
                concat!("b", $n, ".ratio"),
                concat!("Ratio ", $n),
                (1.0, 20.0, 2.0),
                Exp,
                Ratio,
            ),
            info(
                concat!("b", $n, ".gain"),
                concat!("Gain ", $n),
                (-24.0, 24.0, 0.0),
                Linear,
                Db,
            ),
        ]
    };
}

const fn multiband_table(
    bands: [[ParamInfo; 3]; 4],
    head: [ParamInfo; 4],
    tail: [ParamInfo; 5],
) -> [ParamInfo; 21] {
    let mut out = [head[0]; 21];
    let mut i = 0;
    while i < 4 {
        out[i] = head[i];
        let mut j = 0;
        while j < 3 {
            out[4 + i * 3 + j] = bands[i][j];
            j += 1;
        }
        i += 1;
    }
    let mut k = 0;
    while k < 5 {
        out[16 + k] = tail[k];
        k += 1;
    }
    out
}

static MULTIBAND: [ParamInfo; 21] = multiband_table(
    [
        mb_band!(1, -18.0),
        mb_band!(2, -18.0),
        mb_band!(3, -18.0),
        mb_band!(4, -18.0),
    ],
    [
        choice("bands", "Bands", MULTIBAND_BANDS, 0.0),
        info("x.low", "Low split", (20.0, 1000.0, 120.0), Exp, Hz),
        info("x.mid", "Mid split", (200.0, 5000.0, 1000.0), Exp, Hz),
        info("x.high", "High split", (1000.0, 16_000.0, 2500.0), Exp, Hz),
    ],
    [
        info("attack", "Attack", (0.1, 200.0, 10.0), Exp, Ms),
        info("release", "Release", (5.0, 2000.0, 150.0), Exp, Ms),
        choice("limiter", "Limiter", OFF_ON, 1.0),
        info("drive", "Drive", (0.0, 24.0, 0.0), Linear, Db),
        info("ceiling", "Ceiling", (-12.0, 0.0, -0.3), Linear, Db),
    ],
);

static ANALYZER: [ParamInfo; 2] = [
    info("slope", "Slope", (0.0, 6.0, 4.5), Linear, Plain),
    choice("hold", "Peak hold", OFF_ON, 1.0),
];

static CONVOLUTION: [ParamInfo; 7] = [
    choice("ir", "Response", IMPULSE_RESPONSES, 0.0),
    info("length", "Length", (0.1, 1.0, 1.0), Linear, Percent),
    info("predelay", "Pre-delay", (0.0, 200.0, 0.0), Power(2.0), Ms),
    info("lowcut", "Low cut", (20.0, 1000.0, 20.0), Exp, Hz),
    info("highcut", "High cut", (1000.0, 20_000.0, 20_000.0), Exp, Hz),
    info("width", "Width", (0.0, 1.0, 1.0), Linear, Percent),
    info("mix", "Mix", (0.0, 1.0, 0.3), Linear, Percent),
];

static PITCH: [ParamInfo; 8] = [
    choice("mode", "Mode", PITCH_MODES, 0.0),
    info("semitones", "Pitch", (-24.0, 24.0, 0.0), Stepped, Semitones),
    info("cents", "Fine", (-100.0, 100.0, 0.0), Linear, Cents),
    choice("formant", "Keep formants", OFF_ON, 0.0),
    choice("root", "Root", NOTE_NAMES, 0.0),
    choice("scale", "Scale", PITCH_SCALES, 0.0),
    info("speed", "Speed", (0.0, 500.0, 20.0), Power(2.0), Ms),
    info("mix", "Mix", (0.0, 1.0, 1.0), Linear, Percent),
];

static VOCODER: [ParamInfo; 9] = [
    choice("bands", "Bands", VOCODER_BANDS, 2.0),
    choice("carrier", "Carrier", VOCODER_CARRIERS, 0.0),
    info("pitch", "Pitch", (30.0, 1000.0, 110.0), Exp, Hz),
    info("width", "Width", (0.25, 2.0, 1.0), Exp, Plain),
    info("attack", "Attack", (0.1, 100.0, 2.0), Exp, Ms),
    info("release", "Release", (5.0, 1000.0, 50.0), Exp, Ms),
    info("shift", "Shift", (-12.0, 12.0, 0.0), Linear, Semitones),
    info("mix", "Mix", (0.0, 1.0, 1.0), Linear, Percent),
    info("output", "Output", (-24.0, 24.0, 0.0), Linear, Db),
];

macro_rules! steps {
    ($($n:literal),*) => {
        [$(choice(concat!("s", $n), concat!("Step ", $n), STUTTER_STEPS, 1.0)),*]
    };
}

const fn stutter_table(
    steps: [ParamInfo; STUTTER_PATTERN],
    tail: [ParamInfo; 3],
) -> [ParamInfo; 19] {
    let mut out = [tail[0]; 19];
    let mut i = 0;
    while i < STUTTER_PATTERN {
        out[i] = steps[i];
        i += 1;
    }
    let mut k = 0;
    while k < 3 {
        out[STUTTER_PATTERN + k] = tail[k];
        k += 1;
    }
    out
}

static STUTTER: [ParamInfo; 19] = stutter_table(
    steps!(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16),
    [
        choice("length", "Step", STUTTER_LENGTHS, 1.0),
        info("smooth", "Smooth", (0.0, 20.0, 3.0), Linear, Ms),
        info("mix", "Mix", (0.0, 1.0, 1.0), Linear, Percent),
    ],
);

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

static MULTIBAND_PRESETS: [EffectPreset; 3] = [
    preset("Gentle glue", &[("limiter", 0.0)]),
    preset(
        "Loud master",
        &[
            ("b1.threshold", -24.0),
            ("b2.threshold", -20.0),
            ("b4.threshold", -22.0),
            ("b1.ratio", 3.0),
            ("b2.ratio", 2.5),
            ("b4.ratio", 3.0),
            ("drive", 6.0),
            ("ceiling", -0.3),
        ],
    ),
    preset(
        "Tame the lows",
        &[
            ("bands", 1.0),
            ("x.low", 200.0),
            ("b1.threshold", -30.0),
            ("b1.ratio", 4.0),
            ("limiter", 0.0),
        ],
    ),
];
static CONVOLUTION_PRESETS: [EffectPreset; 4] = [
    preset("Small room", &[("length", 0.6), ("mix", 0.2)]),
    preset(
        "Concert hall",
        &[("ir", 1.0), ("predelay", 20.0), ("lowcut", 120.0)],
    ),
    preset(
        "Vocal plate",
        &[
            ("ir", 2.0),
            ("length", 0.7),
            ("lowcut", 200.0),
            ("mix", 0.25),
        ],
    ),
    preset("Surf spring", &[("ir", 3.0), ("mix", 0.4)]),
];
static PITCH_PRESETS: [EffectPreset; 5] = [
    preset("Octave down", &[("semitones", -12.0)]),
    preset("Fifth up (blend)", &[("semitones", 7.0), ("mix", 0.5)]),
    preset(
        "Chipmunk, same vowels",
        &[("semitones", 7.0), ("formant", 1.0)],
    ),
    preset(
        "Natural correction",
        &[("mode", 1.0), ("scale", 1.0), ("speed", 80.0)],
    ),
    preset(
        "Hard tune",
        &[("mode", 1.0), ("scale", 1.0), ("speed", 0.0)],
    ),
];
static VOCODER_PRESETS: [EffectPreset; 3] = [
    preset("Robot voice", &[("bands", 3.0), ("pitch", 98.0)]),
    preset("Whisper", &[("carrier", 1.0), ("bands", 4.0)]),
    preset(
        "Sidechain synth",
        &[("carrier", 2.0), ("bands", 4.0), ("release", 30.0)],
    ),
];
static STUTTER_PRESETS: [EffectPreset; 4] = [
    preset(
        "Trance gate",
        &[
            ("s2", 0.0),
            ("s4", 0.0),
            ("s6", 0.0),
            ("s8", 0.0),
            ("s10", 0.0),
            ("s12", 0.0),
            ("s14", 0.0),
            ("s16", 0.0),
        ],
    ),
    preset(
        "Stutter fill",
        &[
            ("s10", 2.0),
            ("s11", 2.0),
            ("s12", 2.0),
            ("s14", 2.0),
            ("s15", 2.0),
            ("s16", 2.0),
            ("length", 0.0),
        ],
    ),
    preset(
        "Half-time drop",
        &[
            ("s9", 3.0),
            ("s10", 3.0),
            ("s11", 3.0),
            ("s12", 3.0),
            ("s13", 3.0),
            ("s14", 3.0),
            ("s15", 3.0),
            ("s16", 3.0),
            ("length", 2.0),
        ],
    ),
    preset(
        "Off-beat chop",
        &[("s1", 0.0), ("s5", 0.0), ("s9", 0.0), ("s13", 0.0)],
    ),
];

impl EffectKind {
    /// Every built-in kind, in menu order.
    pub const ALL: [EffectKind; 21] = [
        Self::Eq,
        Self::Compressor,
        Self::Multiband,
        Self::Gate,
        Self::Limiter,
        Self::Clipper,
        Self::Transient,
        Self::Filter,
        Self::Distortion,
        Self::Bitcrusher,
        Self::Delay,
        Self::Reverb,
        Self::Convolution,
        Self::Chorus,
        Self::Flanger,
        Self::Phaser,
        Self::Width,
        Self::PitchShift,
        Self::Vocoder,
        Self::Stutter,
        Self::Analyzer,
    ];

    /// True if changing parameter `index` needs the effect built again (on the UI thread):
    /// the convolution reverb's response and its length.
    pub fn rebuilds(self, index: usize) -> bool {
        self == Self::Convolution && index < 2
    }

    /// True if the effect can use an audio file ([`EffectSlot::file`]).
    pub fn uses_file(self) -> bool {
        self == Self::Convolution
    }

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
            Self::Multiband => &MULTIBAND_PRESETS,
            Self::Convolution => &CONVOLUTION_PRESETS,
            Self::PitchShift => &PITCH_PRESETS,
            Self::Vocoder => &VOCODER_PRESETS,
            Self::Stutter => &STUTTER_PRESETS,
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
            Self::Multiband => "multiband",
            Self::Analyzer => "analyzer",
            Self::Convolution => "convolution",
            Self::PitchShift => "pitch",
            Self::Vocoder => "vocoder",
            Self::Stutter => "stutter",
            Self::Plugin => "plugin",
            Self::Patcher => "patcher",
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
            Self::Multiband => "Multiband Maximizer",
            Self::Analyzer => "Analyzer",
            Self::Convolution => "Convolution Reverb",
            Self::PitchShift => "Pitch Shifter",
            Self::Vocoder => "Vocoder",
            Self::Stutter => "Stutter / Gate",
            Self::Plugin => "Plugin",
            Self::Patcher => "Patcher",
        }
    }

    /// The kind with a file key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .chain([Self::Plugin, Self::Patcher])
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
            Self::Multiband => &MULTIBAND,
            Self::Analyzer => &ANALYZER,
            Self::Convolution => &CONVOLUTION,
            Self::PitchShift => &PITCH,
            Self::Vocoder => &VOCODER,
            Self::Stutter => &STUTTER,
            Self::Plugin | Self::Patcher => &[],
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
    /// An audio file the effect uses (a convolution reverb's own impulse response).
    pub file: Option<std::path::PathBuf>,
    /// The patch, when `kind` is [`EffectKind::Patcher`].
    pub patch: Option<Box<crate::patcher::Patch>>,
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
            file: None,
            patch: (kind == EffectKind::Patcher).then(|| Box::new(crate::patcher::Patch::effect())),
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
            file: None,
            patch: None,
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
        if !self.kind.uses_file() {
            self.file = None;
        }
        if let Some(p) = &mut self.plugin {
            p.sanitize();
        }
        match (self.kind, &mut self.patch) {
            (EffectKind::Patcher, Some(p)) => p.sanitize(),
            (EffectKind::Patcher, None) => {
                self.patch = Some(Box::new(crate::patcher::Patch::effect()));
            }
            _ => self.patch = None,
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
        let mb = EffectKind::Multiband.params();
        assert_eq!(mb[4].key, "b1.threshold");
        assert_eq!(mb[15].key, "b4.gain");
        assert_eq!(mb[16].key, "attack");
        let st = EffectKind::Stutter.params();
        assert_eq!(st[15].key, "s16");
        assert_eq!(st[16].key, "length");
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
