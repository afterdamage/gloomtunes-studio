//! Gloom Synth patches as document data.
//!
//! A patch is a flat array of parameter values in plain units (Hz, ms, semitones...) indexed by
//! [`SynthParam`], plus an 8-slot modulation matrix. The index of each parameter and its `key`
//! string never change once released: automation (Prompt 8) addresses parameters by index and
//! preset files by key. [`ParamInfo`] gives each parameter's range, default, knob taper and
//! display format, so the UI, presets and the engine all agree on them.

pub use crate::param::{ParamCurve, ParamInfo, ParamUnit};

/// Oscillator waveform names, indexed by value.
pub const OSC_WAVES: &[&str] = &["Sine", "Triangle", "Saw", "Square"];
/// LFO waveform names, indexed by value.
pub const LFO_WAVES: &[&str] = &["Sine", "Triangle", "Saw", "Square", "S&H"];
/// Sound engines, indexed by value.
pub const ENGINES: &[&str] = &["Gloom", "FM", "Wavetable", "Bass"];
/// FM algorithms, indexed by value (the last is the free matrix).
pub const FM_ALGORITHMS: &[&str] = &[
    "1 Stack",
    "2 Two stacks",
    "3 Three pairs",
    "4 Branch",
    "5 Stack + sine",
    "6 One to five",
    "7 Additive",
    "8 Fan",
    "Free",
];
/// FM operator frequency modes.
pub const OP_MODES: &[&str] = &["Ratio", "Fixed"];
/// Factory wavetables, indexed by value.
pub const WT_TABLES: &[&str] = &[
    "Basic Shapes",
    "Harmonic Sweep",
    "Organ Bars",
    "Vowels",
    "PWM",
    "Dust",
    "Sync Saw",
    "Fold",
];
/// Wavetable warp modes.
pub const WT_WARPS: &[&str] = &["Off", "Sync", "Bend", "PWM"];
/// FM operators.
pub const FM_OPS: usize = 6;
/// Parameters per FM operator, in the order of [`OpField`].
pub const OP_FIELDS: usize = 12;

macro_rules! synth_params {
    ($( $variant:ident => $key:literal, $name:literal, $min:expr, $max:expr, $def:expr,
        $curve:expr, $unit:ident $(, $choices:expr)? ; )*) => {
        /// Gloom Synth parameters. The discriminant is the stable index.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(usize)]
        pub enum SynthParam { $( #[doc = $name] $variant, )* }

        impl SynthParam {
            /// Every parameter in index order.
            pub const ALL: &'static [SynthParam] = &[ $( SynthParam::$variant, )* ];
            /// Number of parameters.
            pub const COUNT: usize = Self::ALL.len();

            /// Range, default, taper and format.
            pub fn info(self) -> &'static ParamInfo {
                &PARAM_INFO[self as usize]
            }

            /// The parameter with a preset-file key.
            pub fn from_key(key: &str) -> Option<SynthParam> {
                Self::ALL.iter().copied().find(|p| p.info().key == key)
            }
        }

        static PARAM_INFO: [ParamInfo; SynthParam::ALL.len()] = [ $(
            ParamInfo {
                key: $key,
                name: $name,
                min: $min,
                max: $max,
                default: $def,
                curve: $curve,
                unit: ParamUnit::$unit,
                choices: synth_params!(@choices $($choices)?),
            },
        )* ];
    };
    (@choices) => { &[] };
    (@choices $c:expr) => { $c };
}

use ParamCurve::{Exp, Linear, Power, Stepped};

synth_params! {
    Osc1Wave => "osc1.wave", "Wave", 0.0, 3.0, 2.0, Stepped, Choice, OSC_WAVES;
    Osc1Octave => "osc1.octave", "Octave", -3.0, 3.0, 0.0, Stepped, Count;
    Osc1Semi => "osc1.semi", "Semi", -12.0, 12.0, 0.0, Stepped, Semitones;
    Osc1Fine => "osc1.fine", "Fine", -100.0, 100.0, 0.0, Linear, Cents;
    Osc1Level => "osc1.level", "Level", 0.0, 1.0, 0.8, Linear, Percent;
    Osc1Pw => "osc1.pw", "PW", 0.05, 0.95, 0.5, Linear, Percent;
    Osc2Wave => "osc2.wave", "Wave", 0.0, 3.0, 3.0, Stepped, Choice, OSC_WAVES;
    Osc2Octave => "osc2.octave", "Octave", -3.0, 3.0, 0.0, Stepped, Count;
    Osc2Semi => "osc2.semi", "Semi", -12.0, 12.0, 0.0, Stepped, Semitones;
    Osc2Fine => "osc2.fine", "Fine", -100.0, 100.0, 7.0, Linear, Cents;
    Osc2Level => "osc2.level", "Level", 0.0, 1.0, 0.5, Linear, Percent;
    Osc2Pw => "osc2.pw", "PW", 0.05, 0.95, 0.5, Linear, Percent;
    SubLevel => "sub.level", "Sub", 0.0, 1.0, 0.0, Linear, Percent;
    NoiseLevel => "noise.level", "Noise", 0.0, 1.0, 0.0, Linear, Percent;
    UnisonVoices => "unison.voices", "Voices", 1.0, 7.0, 1.0, Stepped, Count;
    UnisonDetune => "unison.detune", "Detune", 0.0, 100.0, 20.0, Power(2.0), Cents;
    UnisonSpread => "unison.spread", "Spread", 0.0, 1.0, 0.5, Linear, Percent;
    Cutoff => "filter.cutoff", "Cutoff", 20.0, 20_000.0, 2500.0, Exp, Hz;
    Resonance => "filter.resonance", "Reso", 0.0, 1.0, 0.25, Linear, Percent;
    FilterEnv => "filter.env", "Env", -8.0, 8.0, 2.0, Linear, Octaves;
    KeyTrack => "filter.keytrack", "Key", 0.0, 1.0, 0.5, Linear, Percent;
    Drive => "filter.drive", "Drive", 0.0, 1.0, 0.1, Linear, Percent;
    AmpAttack => "amp.attack", "Attack", 0.0, 5000.0, 2.0, Power(3.0), Ms;
    AmpDecay => "amp.decay", "Decay", 0.0, 5000.0, 300.0, Power(3.0), Ms;
    AmpSustain => "amp.sustain", "Sustain", 0.0, 1.0, 0.7, Linear, Percent;
    AmpRelease => "amp.release", "Release", 0.0, 8000.0, 200.0, Power(3.0), Ms;
    ModAttack => "mod.attack", "Attack", 0.0, 5000.0, 5.0, Power(3.0), Ms;
    ModDecay => "mod.decay", "Decay", 0.0, 5000.0, 400.0, Power(3.0), Ms;
    ModSustain => "mod.sustain", "Sustain", 0.0, 1.0, 0.2, Linear, Percent;
    ModRelease => "mod.release", "Release", 0.0, 8000.0, 300.0, Power(3.0), Ms;
    Lfo1Wave => "lfo1.wave", "Wave", 0.0, 4.0, 0.0, Stepped, Choice, LFO_WAVES;
    Lfo1Rate => "lfo1.rate", "Rate", 0.02, 20.0, 4.0, Exp, Hz;
    Lfo2Wave => "lfo2.wave", "Wave", 0.0, 4.0, 1.0, Stepped, Choice, LFO_WAVES;
    Lfo2Rate => "lfo2.rate", "Rate", 0.02, 20.0, 0.5, Exp, Hz;
    Glide => "glide", "Glide", 0.0, 2000.0, 0.0, Power(3.0), Ms;
    VelocitySens => "velocity", "Velocity", 0.0, 1.0, 0.6, Linear, Percent;
    Volume => "volume", "Volume", 0.0, 1.0, 0.5, Power(2.0), Gain;
    // Prompt 24: the engine, FM and wavetable parameters (appended: indices are stable).
    Engine => "engine", "Engine", 0.0, 3.0, 0.0, Stepped, Choice, ENGINES;
    FmAlgorithm => "fm.algorithm", "Algorithm", 0.0, 8.0, 0.0, Stepped, Choice, FM_ALGORITHMS;
    FmFeedback => "fm.feedback", "Feedback", 0.0, 1.0, 0.0, Linear, Percent;
    Op1Ratio => "fm1.ratio", "Ratio", 0.125, 32.0, 1.0, Power(2.0), Plain;
    Op1Fixed => "fm1.fixed", "Mode", 0.0, 1.0, 0.0, Stepped, Choice, OP_MODES;
    Op1Freq => "fm1.freq", "Freq", 1.0, 20_000.0, 440.0, Exp, Hz;
    Op1Fine => "fm1.fine", "Fine", -100.0, 100.0, 0.0, Linear, Cents;
    Op1Level => "fm1.level", "Level", 0.0, 1.0, 1.0, Power(2.0), Percent;
    Op1Attack => "fm1.attack", "Attack", 0.0, 5000.0, 1.0, Power(3.0), Ms;
    Op1Decay => "fm1.decay", "Decay", 0.0, 8000.0, 800.0, Power(3.0), Ms;
    Op1Sustain => "fm1.sustain", "Sustain", 0.0, 1.0, 0.6, Linear, Percent;
    Op1Release => "fm1.release", "Release", 0.0, 8000.0, 300.0, Power(3.0), Ms;
    Op1KeyScale => "fm1.keyscale", "Key lvl", -12.0, 12.0, 0.0, Linear, Db;
    Op1RateScale => "fm1.ratescale", "Key rate", 0.0, 1.0, 0.0, Linear, Percent;
    Op1Velocity => "fm1.velocity", "Velocity", 0.0, 1.0, 0.5, Linear, Percent;
    Op2Ratio => "fm2.ratio", "Ratio", 0.125, 32.0, 1.0, Power(2.0), Plain;
    Op2Fixed => "fm2.fixed", "Mode", 0.0, 1.0, 0.0, Stepped, Choice, OP_MODES;
    Op2Freq => "fm2.freq", "Freq", 1.0, 20_000.0, 440.0, Exp, Hz;
    Op2Fine => "fm2.fine", "Fine", -100.0, 100.0, 0.0, Linear, Cents;
    Op2Level => "fm2.level", "Level", 0.0, 1.0, 0.25, Power(2.0), Percent;
    Op2Attack => "fm2.attack", "Attack", 0.0, 5000.0, 1.0, Power(3.0), Ms;
    Op2Decay => "fm2.decay", "Decay", 0.0, 8000.0, 800.0, Power(3.0), Ms;
    Op2Sustain => "fm2.sustain", "Sustain", 0.0, 1.0, 0.6, Linear, Percent;
    Op2Release => "fm2.release", "Release", 0.0, 8000.0, 300.0, Power(3.0), Ms;
    Op2KeyScale => "fm2.keyscale", "Key lvl", -12.0, 12.0, 0.0, Linear, Db;
    Op2RateScale => "fm2.ratescale", "Key rate", 0.0, 1.0, 0.0, Linear, Percent;
    Op2Velocity => "fm2.velocity", "Velocity", 0.0, 1.0, 0.5, Linear, Percent;
    Op3Ratio => "fm3.ratio", "Ratio", 0.125, 32.0, 1.0, Power(2.0), Plain;
    Op3Fixed => "fm3.fixed", "Mode", 0.0, 1.0, 0.0, Stepped, Choice, OP_MODES;
    Op3Freq => "fm3.freq", "Freq", 1.0, 20_000.0, 440.0, Exp, Hz;
    Op3Fine => "fm3.fine", "Fine", -100.0, 100.0, 0.0, Linear, Cents;
    Op3Level => "fm3.level", "Level", 0.0, 1.0, 0.0, Power(2.0), Percent;
    Op3Attack => "fm3.attack", "Attack", 0.0, 5000.0, 1.0, Power(3.0), Ms;
    Op3Decay => "fm3.decay", "Decay", 0.0, 8000.0, 800.0, Power(3.0), Ms;
    Op3Sustain => "fm3.sustain", "Sustain", 0.0, 1.0, 0.6, Linear, Percent;
    Op3Release => "fm3.release", "Release", 0.0, 8000.0, 300.0, Power(3.0), Ms;
    Op3KeyScale => "fm3.keyscale", "Key lvl", -12.0, 12.0, 0.0, Linear, Db;
    Op3RateScale => "fm3.ratescale", "Key rate", 0.0, 1.0, 0.0, Linear, Percent;
    Op3Velocity => "fm3.velocity", "Velocity", 0.0, 1.0, 0.5, Linear, Percent;
    Op4Ratio => "fm4.ratio", "Ratio", 0.125, 32.0, 1.0, Power(2.0), Plain;
    Op4Fixed => "fm4.fixed", "Mode", 0.0, 1.0, 0.0, Stepped, Choice, OP_MODES;
    Op4Freq => "fm4.freq", "Freq", 1.0, 20_000.0, 440.0, Exp, Hz;
    Op4Fine => "fm4.fine", "Fine", -100.0, 100.0, 0.0, Linear, Cents;
    Op4Level => "fm4.level", "Level", 0.0, 1.0, 0.0, Power(2.0), Percent;
    Op4Attack => "fm4.attack", "Attack", 0.0, 5000.0, 1.0, Power(3.0), Ms;
    Op4Decay => "fm4.decay", "Decay", 0.0, 8000.0, 800.0, Power(3.0), Ms;
    Op4Sustain => "fm4.sustain", "Sustain", 0.0, 1.0, 0.6, Linear, Percent;
    Op4Release => "fm4.release", "Release", 0.0, 8000.0, 300.0, Power(3.0), Ms;
    Op4KeyScale => "fm4.keyscale", "Key lvl", -12.0, 12.0, 0.0, Linear, Db;
    Op4RateScale => "fm4.ratescale", "Key rate", 0.0, 1.0, 0.0, Linear, Percent;
    Op4Velocity => "fm4.velocity", "Velocity", 0.0, 1.0, 0.5, Linear, Percent;
    Op5Ratio => "fm5.ratio", "Ratio", 0.125, 32.0, 1.0, Power(2.0), Plain;
    Op5Fixed => "fm5.fixed", "Mode", 0.0, 1.0, 0.0, Stepped, Choice, OP_MODES;
    Op5Freq => "fm5.freq", "Freq", 1.0, 20_000.0, 440.0, Exp, Hz;
    Op5Fine => "fm5.fine", "Fine", -100.0, 100.0, 0.0, Linear, Cents;
    Op5Level => "fm5.level", "Level", 0.0, 1.0, 0.0, Power(2.0), Percent;
    Op5Attack => "fm5.attack", "Attack", 0.0, 5000.0, 1.0, Power(3.0), Ms;
    Op5Decay => "fm5.decay", "Decay", 0.0, 8000.0, 800.0, Power(3.0), Ms;
    Op5Sustain => "fm5.sustain", "Sustain", 0.0, 1.0, 0.6, Linear, Percent;
    Op5Release => "fm5.release", "Release", 0.0, 8000.0, 300.0, Power(3.0), Ms;
    Op5KeyScale => "fm5.keyscale", "Key lvl", -12.0, 12.0, 0.0, Linear, Db;
    Op5RateScale => "fm5.ratescale", "Key rate", 0.0, 1.0, 0.0, Linear, Percent;
    Op5Velocity => "fm5.velocity", "Velocity", 0.0, 1.0, 0.5, Linear, Percent;
    Op6Ratio => "fm6.ratio", "Ratio", 0.125, 32.0, 1.0, Power(2.0), Plain;
    Op6Fixed => "fm6.fixed", "Mode", 0.0, 1.0, 0.0, Stepped, Choice, OP_MODES;
    Op6Freq => "fm6.freq", "Freq", 1.0, 20_000.0, 440.0, Exp, Hz;
    Op6Fine => "fm6.fine", "Fine", -100.0, 100.0, 0.0, Linear, Cents;
    Op6Level => "fm6.level", "Level", 0.0, 1.0, 0.0, Power(2.0), Percent;
    Op6Attack => "fm6.attack", "Attack", 0.0, 5000.0, 1.0, Power(3.0), Ms;
    Op6Decay => "fm6.decay", "Decay", 0.0, 8000.0, 800.0, Power(3.0), Ms;
    Op6Sustain => "fm6.sustain", "Sustain", 0.0, 1.0, 0.6, Linear, Percent;
    Op6Release => "fm6.release", "Release", 0.0, 8000.0, 300.0, Power(3.0), Ms;
    Op6KeyScale => "fm6.keyscale", "Key lvl", -12.0, 12.0, 0.0, Linear, Db;
    Op6RateScale => "fm6.ratescale", "Key rate", 0.0, 1.0, 0.0, Linear, Percent;
    Op6Velocity => "fm6.velocity", "Velocity", 0.0, 1.0, 0.5, Linear, Percent;
    FmMod11 => "fm.m11", "1→1", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod12 => "fm.m12", "2→1", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod13 => "fm.m13", "3→1", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod14 => "fm.m14", "4→1", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod15 => "fm.m15", "5→1", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod16 => "fm.m16", "6→1", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod21 => "fm.m21", "1→2", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod22 => "fm.m22", "2→2", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod23 => "fm.m23", "3→2", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod24 => "fm.m24", "4→2", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod25 => "fm.m25", "5→2", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod26 => "fm.m26", "6→2", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod31 => "fm.m31", "1→3", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod32 => "fm.m32", "2→3", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod33 => "fm.m33", "3→3", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod34 => "fm.m34", "4→3", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod35 => "fm.m35", "5→3", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod36 => "fm.m36", "6→3", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod41 => "fm.m41", "1→4", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod42 => "fm.m42", "2→4", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod43 => "fm.m43", "3→4", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod44 => "fm.m44", "4→4", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod45 => "fm.m45", "5→4", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod46 => "fm.m46", "6→4", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod51 => "fm.m51", "1→5", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod52 => "fm.m52", "2→5", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod53 => "fm.m53", "3→5", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod54 => "fm.m54", "4→5", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod55 => "fm.m55", "5→5", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod56 => "fm.m56", "6→5", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod61 => "fm.m61", "1→6", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod62 => "fm.m62", "2→6", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod63 => "fm.m63", "3→6", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod64 => "fm.m64", "4→6", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod65 => "fm.m65", "5→6", 0.0, 1.0, 0.0, Linear, Percent;
    FmMod66 => "fm.m66", "6→6", 0.0, 1.0, 0.0, Linear, Percent;
    FmOut1 => "fm.out1", "Out 1", 0.0, 1.0, 1.0, Linear, Percent;
    FmOut2 => "fm.out2", "Out 2", 0.0, 1.0, 0.0, Linear, Percent;
    FmOut3 => "fm.out3", "Out 3", 0.0, 1.0, 0.0, Linear, Percent;
    FmOut4 => "fm.out4", "Out 4", 0.0, 1.0, 0.0, Linear, Percent;
    FmOut5 => "fm.out5", "Out 5", 0.0, 1.0, 0.0, Linear, Percent;
    FmOut6 => "fm.out6", "Out 6", 0.0, 1.0, 0.0, Linear, Percent;
    Wt1Table => "wt1.table", "Table", 0.0, 7.0, 0.0, Stepped, Choice, WT_TABLES;
    Wt1Pos => "wt1.pos", "Position", 0.0, 1.0, 0.0, Linear, Percent;
    Wt1Warp => "wt1.warp", "Warp", 0.0, 3.0, 0.0, Stepped, Choice, WT_WARPS;
    Wt1WarpAmt => "wt1.warpamt", "Amount", 0.0, 1.0, 0.0, Linear, Percent;
    Wt2Table => "wt2.table", "Table", 0.0, 7.0, 0.0, Stepped, Choice, WT_TABLES;
    Wt2Pos => "wt2.pos", "Position", 0.0, 1.0, 0.0, Linear, Percent;
    Wt2Warp => "wt2.warp", "Warp", 0.0, 3.0, 0.0, Stepped, Choice, WT_WARPS;
    Wt2WarpAmt => "wt2.warpamt", "Amount", 0.0, 1.0, 0.0, Linear, Percent;
    // Prompt 28: the mono bass-line engine.
    BassAccent => "bass.accent", "Accent", 0.0, 1.0, 0.6, Linear, Percent;
    BassThreshold => "bass.threshold", "Accent at", 0.5, 1.0, 0.79, Linear, Percent;
    BassSlide => "bass.slide", "Slide", 5.0, 500.0, 60.0, Power(2.0), Ms;
}

macro_rules! named_enum {
    ($(#[$m:meta])* $name:ident { $( $variant:ident => $key:literal, $label:literal; )* }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
        pub enum $name {
            #[default]
            $( #[doc = $label] $variant, )*
        }

        impl $name {
            /// Every value in menu order.
            pub const ALL: &'static [$name] = &[ $( $name::$variant, )* ];

            /// Stable identifier used in preset files.
            pub fn key(self) -> &'static str {
                match self { $( $name::$variant => $key, )* }
            }

            /// Display name.
            pub fn label(self) -> &'static str {
                match self { $( $name::$variant => $label, )* }
            }

            /// The value with a preset-file key.
            pub fn from_key(key: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.key() == key)
            }
        }
    };
}

named_enum! {
    /// Modulation matrix sources.
    ModSource {
        Off => "off", "Off";
        Lfo1 => "lfo1", "LFO 1";
        Lfo2 => "lfo2", "LFO 2";
        ModEnv => "modenv", "Mod env";
        AmpEnv => "ampenv", "Amp env";
        Velocity => "velocity", "Velocity";
        Note => "note", "Note";
        Random => "random", "Random";
        ModWheel => "modwheel", "Mod wheel";
        Aftertouch => "aftertouch", "Aftertouch";
        ModX => "modx", "Note mod X";
        ModY => "mody", "Note mod Y";
    }
}

named_enum! {
    /// Modulation matrix destinations.
    ModDest {
        Off => "off", "Off";
        Pitch => "pitch", "Pitch";
        Osc1Pitch => "osc1.pitch", "Osc 1 pitch";
        Osc2Pitch => "osc2.pitch", "Osc 2 pitch";
        Osc1Pw => "osc1.pw", "Osc 1 PW";
        Osc2Pw => "osc2.pw", "Osc 2 PW";
        Osc1Level => "osc1.level", "Osc 1 level";
        Osc2Level => "osc2.level", "Osc 2 level";
        NoiseLevel => "noise.level", "Noise level";
        Cutoff => "cutoff", "Cutoff";
        Resonance => "resonance", "Resonance";
        Pan => "pan", "Pan";
        Amp => "amp", "Amp";
        Lfo1Rate => "lfo1.rate", "LFO 1 rate";
        Lfo2Rate => "lfo2.rate", "LFO 2 rate";
        UnisonDetune => "unison.detune", "Unison detune";
        FmIndex => "fm.index", "FM index";
        FmFeedback => "fm.feedback", "FM feedback";
        Wt1Pos => "wt1.pos", "WT 1 position";
        Wt2Pos => "wt2.pos", "WT 2 position";
        Wt1Warp => "wt1.warp", "WT 1 warp";
        Wt2Warp => "wt2.warp", "WT 2 warp";
    }
}

/// A parameter of each FM operator, in block order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpField {
    /// Frequency ratio.
    Ratio,
    /// Ratio or fixed frequency.
    Fixed,
    /// Fixed frequency.
    Freq,
    /// Fine tune.
    Fine,
    /// Output level.
    Level,
    /// Envelope attack.
    Attack,
    /// Envelope decay.
    Decay,
    /// Envelope sustain.
    Sustain,
    /// Envelope release.
    Release,
    /// Level key scaling.
    KeyScale,
    /// Envelope rate key scaling.
    RateScale,
    /// Velocity sensitivity.
    Velocity,
}

/// Which engines use a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uses {
    /// Every engine (envelopes, LFOs, glide, velocity, volume).
    All,
    /// The engine selector itself.
    Selector,
    /// Gloom and Wavetable (oscillator pitch and level, sub, noise, unison, filter).
    Oscillators,
    /// Gloom and Bass (waveform, pulse width).
    Gloom,
    /// FM only.
    Fm,
    /// Wavetable only.
    Wavetable,
    /// Bass only (accent, slide).
    Bass,
}

impl SynthParam {
    /// Operator `op` (0 to 5)'s `field`.
    pub fn op(op: usize, field: OpField) -> SynthParam {
        Self::ALL[Self::Op1Ratio as usize + op.min(FM_OPS - 1) * OP_FIELDS + field as usize]
    }

    /// How much operator `src` modulates operator `dst` in the free algorithm (0-based).
    pub fn fm_mod(dst: usize, src: usize) -> SynthParam {
        Self::ALL[Self::FmMod11 as usize + dst.min(5) * FM_OPS + src.min(5)]
    }

    /// Operator `op`'s output level in the free algorithm (0-based).
    pub fn fm_out(op: usize) -> SynthParam {
        Self::ALL[Self::FmOut1 as usize + op.min(5)]
    }

    /// Wavetable oscillator `osc` (0 or 1): table, position, warp, warp amount.
    pub fn wt(osc: usize) -> [SynthParam; 4] {
        let b = Self::Wt1Table as usize + osc.min(1) * 4;
        [
            Self::ALL[b],
            Self::ALL[b + 1],
            Self::ALL[b + 2],
            Self::ALL[b + 3],
        ]
    }

    /// Which engines use the parameter.
    pub fn uses(self) -> Uses {
        use SynthParam as P;
        let i = self as usize;
        match self {
            P::Engine => Uses::Selector,
            P::Osc1Wave | P::Osc1Pw | P::Osc2Wave | P::Osc2Pw => Uses::Gloom,
            _ if i <= P::Drive as usize => Uses::Oscillators,
            _ if i < P::Engine as usize => Uses::All,
            _ if i < P::Wt1Table as usize => Uses::Fm,
            _ if i < P::BassAccent as usize => Uses::Wavetable,
            _ => Uses::Bass,
        }
    }

    /// True when engine `engine` (0 Gloom, 1 FM, 2 Wavetable, 3 Bass) uses the parameter.
    pub fn used_by(self, engine: u8) -> bool {
        match self.uses() {
            Uses::All => true,
            Uses::Selector => false,
            Uses::Oscillators => engine != 1,
            Uses::Gloom => engine == 0 || engine == 3,
            Uses::Fm => engine == 1,
            Uses::Wavetable => engine == 2,
            Uses::Bass => engine == 3,
        }
    }
}

/// Number of modulation matrix slots.
pub const MOD_SLOTS: usize = 8;

/// One modulation matrix row: `amount` (-1..1) of `source` added to `dest`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ModSlot {
    /// Source.
    pub source: ModSource,
    /// Destination.
    pub dest: ModDest,
    /// Amount, -1 to 1.
    pub amount: f32,
}

/// A Gloom Synth sound.
#[derive(Debug, Clone, PartialEq)]
pub struct SynthPatch {
    /// Preset name.
    pub name: String,
    /// Values in plain units, indexed by [`SynthParam`].
    pub values: [f32; SynthParam::COUNT],
    /// Modulation matrix.
    pub mods: [ModSlot; MOD_SLOTS],
    /// User wavetables of the two wavetable oscillators (single-cycle WAVs); each replaces
    /// the oscillator's factory table.
    pub tables: [Option<crate::project::SampleSource>; 2],
}

impl Default for SynthPatch {
    fn default() -> Self {
        Self {
            name: "Init".to_owned(),
            values: std::array::from_fn(|i| SynthParam::ALL[i].info().default),
            mods: [ModSlot::default(); MOD_SLOTS],
            tables: [None, None],
        }
    }
}

impl SynthPatch {
    /// The engine: 0 Gloom, 1 FM, 2 Wavetable.
    pub fn engine(&self) -> u8 {
        self.get(SynthParam::Engine) as u8
    }

    /// A parameter's value.
    pub fn get(&self, p: SynthParam) -> f32 {
        self.values[p as usize]
    }

    /// Sets a parameter, clamped to its range.
    pub fn set(&mut self, p: SynthParam, v: f32) {
        self.values[p as usize] = p.info().clamp(v);
    }

    /// Builder-style `set`.
    pub fn with(mut self, p: SynthParam, v: f32) -> Self {
        self.set(p, v);
        self
    }

    /// Builder-style modulation slot.
    pub fn with_mod(mut self, slot: usize, source: ModSource, dest: ModDest, amount: f32) -> Self {
        if let Some(m) = self.mods.get_mut(slot) {
            *m = ModSlot {
                source,
                dest,
                amount: amount.clamp(-1.0, 1.0),
            };
        }
        self
    }

    /// Clamps every value into range (after loading a file).
    pub fn sanitize(&mut self) {
        for (i, v) in self.values.iter_mut().enumerate() {
            *v = SynthParam::ALL[i].info().clamp(*v);
        }
        for m in &mut self.mods {
            m.amount = if m.amount.is_finite() {
                m.amount.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
    }

    /// The built-in presets. All original.
    pub fn factory() -> Vec<SynthPatch> {
        use SynthParam as P;
        let named = |name: &str| SynthPatch {
            name: name.to_owned(),
            ..SynthPatch::default()
        };
        let mut out = vec![
            named("Init"),
            // Two saws an octave apart with a sub, a short filter envelope: a round, plucky bass.
            named("Gloom Bass")
                .with(P::Osc1Wave, 2.0)
                .with(P::Osc2Wave, 2.0)
                .with(P::Osc2Octave, -1.0)
                .with(P::Osc2Fine, 4.0)
                .with(P::Osc2Level, 0.6)
                .with(P::SubLevel, 0.5)
                .with(P::Cutoff, 180.0)
                .with(P::Resonance, 0.35)
                .with(P::FilterEnv, 3.5)
                .with(P::KeyTrack, 0.3)
                .with(P::Drive, 0.4)
                .with(P::AmpAttack, 1.0)
                .with(P::AmpDecay, 400.0)
                .with(P::AmpSustain, 0.8)
                .with(P::AmpRelease, 80.0)
                .with(P::ModAttack, 0.0)
                .with(P::ModDecay, 220.0)
                .with(P::ModSustain, 0.1)
                .with(P::ModRelease, 120.0)
                .with(P::VelocitySens, 0.5)
                .with(P::Volume, 0.55),
            // Wide detuned saws, slow attack and release, a slow LFO breathing on the cutoff.
            named("Dark Pad")
                .with(P::Osc1Wave, 2.0)
                .with(P::Osc2Wave, 2.0)
                .with(P::Osc2Fine, -9.0)
                .with(P::Osc2Level, 0.7)
                .with(P::UnisonVoices, 5.0)
                .with(P::UnisonDetune, 22.0)
                .with(P::UnisonSpread, 0.9)
                .with(P::Cutoff, 900.0)
                .with(P::Resonance, 0.2)
                .with(P::FilterEnv, 1.0)
                .with(P::AmpAttack, 900.0)
                .with(P::AmpDecay, 1500.0)
                .with(P::AmpSustain, 0.85)
                .with(P::AmpRelease, 2500.0)
                .with(P::ModAttack, 1500.0)
                .with(P::ModDecay, 3000.0)
                .with(P::ModSustain, 0.5)
                .with(P::ModRelease, 2500.0)
                .with(P::Lfo1Rate, 0.15)
                .with(P::VelocitySens, 0.2)
                .with(P::Volume, 0.4)
                .with_mod(0, ModSource::Lfo1, ModDest::Cutoff, 0.15),
            // Square and triangle, no sustain, the filter snapping shut: a glassy pluck.
            named("Glass Pluck")
                .with(P::Osc1Wave, 3.0)
                .with(P::Osc1Pw, 0.3)
                .with(P::Osc2Wave, 1.0)
                .with(P::Osc2Octave, 1.0)
                .with(P::Osc2Fine, 0.0)
                .with(P::Osc2Level, 0.4)
                .with(P::Cutoff, 600.0)
                .with(P::Resonance, 0.45)
                .with(P::FilterEnv, 5.0)
                .with(P::KeyTrack, 0.8)
                .with(P::AmpAttack, 0.5)
                .with(P::AmpDecay, 500.0)
                .with(P::AmpSustain, 0.0)
                .with(P::AmpRelease, 400.0)
                .with(P::ModAttack, 0.0)
                .with(P::ModDecay, 180.0)
                .with(P::ModSustain, 0.0)
                .with(P::VelocitySens, 0.8)
                .with(P::Volume, 0.5)
                .with_mod(0, ModSource::Velocity, ModDest::Cutoff, 0.3),
            // Detuned saw and square with glide and a light vibrato from LFO 1.
            named("Ember Lead")
                .with(P::Osc1Wave, 2.0)
                .with(P::Osc2Wave, 3.0)
                .with(P::Osc2Fine, 12.0)
                .with(P::Osc2Level, 0.6)
                .with(P::UnisonVoices, 3.0)
                .with(P::UnisonDetune, 12.0)
                .with(P::Cutoff, 1800.0)
                .with(P::Resonance, 0.3)
                .with(P::FilterEnv, 2.5)
                .with(P::Drive, 0.3)
                .with(P::AmpAttack, 4.0)
                .with(P::AmpSustain, 0.9)
                .with(P::AmpRelease, 250.0)
                .with(P::ModAttack, 600.0)
                .with(P::ModDecay, 800.0)
                .with(P::ModSustain, 0.6)
                .with(P::Lfo1Rate, 5.5)
                .with(P::Glide, 80.0)
                .with(P::Volume, 0.45)
                .with_mod(0, ModSource::Lfo1, ModDest::Pitch, 0.012),
            // Sub-heavy bass with a square LFO opening the filter for a wobble.
            named("Sub Wobble")
                .with(P::Osc1Wave, 2.0)
                .with(P::Osc2Wave, 3.0)
                .with(P::Osc2Octave, -1.0)
                .with(P::Osc2Fine, 0.0)
                .with(P::Osc2Level, 0.5)
                .with(P::SubLevel, 0.8)
                .with(P::Cutoff, 220.0)
                .with(P::Resonance, 0.55)
                .with(P::FilterEnv, 0.0)
                .with(P::KeyTrack, 0.2)
                .with(P::Drive, 0.6)
                .with(P::AmpAttack, 2.0)
                .with(P::AmpSustain, 1.0)
                .with(P::AmpRelease, 60.0)
                .with(P::Lfo1Wave, 1.0)
                .with(P::Lfo1Rate, 3.0)
                .with(P::VelocitySens, 0.2)
                .with(P::Volume, 0.45)
                .with_mod(0, ModSource::Lfo1, ModDest::Cutoff, 0.45),
            // Filtered noise with a resonant sweep: a dark riser or wind.
            named("Night Wind")
                .with(P::Osc1Level, 0.0)
                .with(P::Osc2Level, 0.0)
                .with(P::NoiseLevel, 0.7)
                .with(P::Cutoff, 500.0)
                .with(P::Resonance, 0.75)
                .with(P::FilterEnv, 3.0)
                .with(P::KeyTrack, 1.0)
                .with(P::AmpAttack, 1200.0)
                .with(P::AmpSustain, 0.8)
                .with(P::AmpRelease, 2000.0)
                .with(P::ModAttack, 3000.0)
                .with(P::ModSustain, 1.0)
                .with(P::Lfo2Rate, 0.2)
                .with(P::Volume, 0.5)
                .with_mod(0, ModSource::Lfo2, ModDest::Cutoff, 0.2)
                .with_mod(1, ModSource::Lfo2, ModDest::Pan, 0.6),
        ];
        out.extend(Self::fm_factory());
        out.extend(Self::wavetable_factory());
        out.extend(Self::bass_factory());
        out
    }

    /// Builder-style FM operator `o` (1 to 6): ratio, level and envelope (ms, sustain).
    pub fn with_op(mut self, o: usize, ratio: f32, level: f32, adsr: [f32; 4]) -> Self {
        let op = o.clamp(1, FM_OPS) - 1;
        let f = |field| SynthParam::op(op, field);
        for (field, v) in [
            (OpField::Ratio, ratio),
            (OpField::Level, level),
            (OpField::Attack, adsr[0]),
            (OpField::Decay, adsr[1]),
            (OpField::Sustain, adsr[2]),
            (OpField::Release, adsr[3]),
        ] {
            self.set(f(field), v);
        }
        self
    }

    /// Builder-style operator setting.
    pub fn with_opx(self, o: usize, field: OpField, v: f32) -> Self {
        self.with(SynthParam::op(o.clamp(1, FM_OPS) - 1, field), v)
    }

    /// The bass-line factory presets (Prompt 28). All original: one oscillator into a
    /// resonant filter with a decaying envelope, set per line by this table.
    fn bass_factory() -> Vec<SynthPatch> {
        use SynthParam as P;
        // (name, wave 2 saw / 3 square, cutoff Hz, resonance, env octaves, mod decay ms,
        //  accent, slide ms, drive, sub, amp decay ms, amp sustain)
        #[allow(clippy::type_complexity)]
        let lines: [(&str, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32); 20] = [
            (
                "Acid Line",
                2.0,
                260.0,
                0.82,
                4.5,
                260.0,
                0.7,
                60.0,
                0.35,
                0.0,
                1200.0,
                0.9,
            ),
            (
                "Acid Square",
                3.0,
                240.0,
                0.8,
                4.2,
                240.0,
                0.7,
                60.0,
                0.3,
                0.0,
                1200.0,
                0.9,
            ),
            (
                "Squelch", 2.0, 180.0, 0.9, 5.5, 180.0, 0.9, 70.0, 0.5, 0.0, 1000.0, 0.9,
            ),
            (
                "Rubber Acid",
                2.0,
                320.0,
                0.7,
                3.5,
                420.0,
                0.6,
                90.0,
                0.25,
                0.2,
                1500.0,
                0.9,
            ),
            (
                "Night Crawler",
                3.0,
                150.0,
                0.75,
                4.0,
                300.0,
                0.8,
                80.0,
                0.6,
                0.0,
                1100.0,
                0.9,
            ),
            (
                "Warehouse",
                2.0,
                200.0,
                0.86,
                5.0,
                200.0,
                0.8,
                55.0,
                0.7,
                0.0,
                900.0,
                0.85,
            ),
            (
                "Soft Acid",
                2.0,
                400.0,
                0.55,
                2.5,
                350.0,
                0.4,
                80.0,
                0.1,
                0.0,
                1400.0,
                0.9,
            ),
            (
                "Dub Sub Line",
                3.0,
                120.0,
                0.3,
                1.5,
                300.0,
                0.3,
                120.0,
                0.2,
                0.7,
                2000.0,
                1.0,
            ),
            (
                "Pluck Line",
                2.0,
                300.0,
                0.6,
                4.0,
                120.0,
                0.6,
                50.0,
                0.2,
                0.2,
                260.0,
                0.0,
            ),
            (
                "Short Stab",
                3.0,
                280.0,
                0.65,
                4.5,
                90.0,
                0.7,
                40.0,
                0.3,
                0.0,
                180.0,
                0.0,
            ),
            (
                "Rave Bounce",
                2.0,
                350.0,
                0.78,
                4.0,
                160.0,
                0.75,
                45.0,
                0.45,
                0.3,
                700.0,
                0.6,
            ),
            (
                "Gloom Crawl",
                2.0,
                110.0,
                0.88,
                5.5,
                520.0,
                0.85,
                110.0,
                0.55,
                0.3,
                1600.0,
                0.9,
            ),
            (
                "Hollow Square",
                3.0,
                500.0,
                0.5,
                2.0,
                260.0,
                0.5,
                70.0,
                0.15,
                0.0,
                1300.0,
                0.9,
            ),
            (
                "Distorted 303ish",
                2.0,
                230.0,
                0.84,
                4.8,
                230.0,
                0.85,
                60.0,
                0.95,
                0.0,
                1200.0,
                0.9,
            ),
            (
                "Deep Roll",
                3.0,
                90.0,
                0.6,
                3.0,
                380.0,
                0.5,
                100.0,
                0.4,
                0.6,
                1500.0,
                0.9,
            ),
            (
                "Chirp", 2.0, 600.0, 0.92, 3.0, 70.0, 0.9, 35.0, 0.3, 0.0, 400.0, 0.4,
            ),
            (
                "Mellow Walk",
                3.0,
                700.0,
                0.3,
                1.2,
                400.0,
                0.3,
                90.0,
                0.05,
                0.4,
                1200.0,
                0.8,
            ),
            (
                "Wet Snake",
                2.0,
                160.0,
                0.93,
                6.0,
                330.0,
                0.8,
                140.0,
                0.4,
                0.0,
                1400.0,
                0.9,
            ),
            (
                "Tight Electro",
                3.0,
                260.0,
                0.7,
                4.0,
                110.0,
                0.6,
                40.0,
                0.5,
                0.5,
                300.0,
                0.3,
            ),
            (
                "Basement Acid",
                2.0,
                140.0,
                0.8,
                5.2,
                280.0,
                1.0,
                65.0,
                0.8,
                0.2,
                1100.0,
                0.9,
            ),
        ];
        lines
            .iter()
            .map(
                |&(
                    name,
                    wave,
                    cutoff,
                    reso,
                    env,
                    mdecay,
                    accent,
                    slide,
                    drive,
                    sub,
                    decay,
                    sus,
                )| {
                    SynthPatch {
                        name: name.to_owned(),
                        ..SynthPatch::default()
                    }
                    .with(P::Engine, 3.0)
                    .with(P::Osc1Wave, wave)
                    .with(P::Osc1Level, 0.9)
                    .with(P::Osc2Level, 0.0)
                    .with(P::SubLevel, sub)
                    .with(P::Cutoff, cutoff)
                    .with(P::Resonance, reso)
                    .with(P::FilterEnv, env)
                    .with(P::KeyTrack, 0.2)
                    .with(P::Drive, drive)
                    .with(P::AmpAttack, 1.0)
                    .with(P::AmpDecay, decay)
                    .with(P::AmpSustain, sus)
                    .with(P::AmpRelease, 30.0)
                    .with(P::ModAttack, 0.0)
                    .with(P::ModDecay, mdecay)
                    .with(P::ModSustain, 0.0)
                    .with(P::ModRelease, 60.0)
                    .with(P::VelocitySens, 0.0)
                    .with(P::Volume, 0.55)
                    .with(P::BassAccent, accent)
                    .with(P::BassSlide, slide)
                },
            )
            .collect()
    }

    /// The FM factory presets (Prompt 24). All original.
    fn fm_factory() -> Vec<SynthPatch> {
        use ModDest as D;
        use ModSource as S;
        use SynthParam as P;
        // FM sounds take their shape from the operator envelopes: the shared amp envelope
        // only gates (instant attack, full sustain, a release longer than the operators').
        let fm = |name: &str, algorithm: f32| {
            let mut p = SynthPatch {
                name: name.to_owned(),
                ..SynthPatch::default()
            }
            .with(P::Engine, 1.0)
            .with(P::FmAlgorithm, algorithm)
            .with(P::AmpAttack, 0.0)
            .with(P::AmpDecay, 0.0)
            .with(P::AmpSustain, 1.0)
            .with(P::AmpRelease, 3000.0)
            .with(P::VelocitySens, 0.3)
            .with(P::Volume, 0.5);
            for o in 1..=FM_OPS {
                p = p.with_op(o, 1.0, 0.0, [1.0, 800.0, 0.6, 300.0]);
            }
            p
        };
        vec![
            // Two stacks: a soft body and a bright tine that fades fast.
            fm("FM Electric Piano", 1.0)
                .with_op(1, 1.0, 0.9, [0.0, 2200.0, 0.0, 500.0])
                .with_op(2, 1.0, 0.3, [0.0, 1400.0, 0.0, 400.0])
                .with_opx(2, OpField::Velocity, 0.9)
                .with_op(3, 14.0, 0.06, [0.0, 220.0, 0.0, 100.0])
                .with_op(4, 1.0, 0.6, [0.0, 3000.0, 0.05, 600.0])
                .with_opx(4, OpField::Fine, 3.0)
                .with_op(5, 1.0, 0.2, [0.0, 1600.0, 0.05, 400.0])
                .with_op(6, 1.0, 0.08, [0.0, 900.0, 0.0, 300.0])
                .with(P::FmFeedback, 0.2)
                .with(P::VelocitySens, 0.7),
            // Three pairs at inharmonic ratios ringing out.
            fm("FM Bell", 2.0)
                .with_op(1, 1.0, 0.9, [0.0, 4500.0, 0.0, 3000.0])
                .with_op(2, 3.5, 0.35, [0.0, 3200.0, 0.0, 2500.0])
                .with_op(3, 2.0, 0.6, [0.0, 3000.0, 0.0, 2000.0])
                .with_op(4, 5.19, 0.25, [0.0, 2000.0, 0.0, 1500.0])
                .with_op(5, 4.0, 0.35, [0.0, 1800.0, 0.0, 1200.0])
                .with_op(6, 7.07, 0.2, [0.0, 1200.0, 0.0, 800.0])
                .with_opx(1, OpField::KeyScale, -2.0),
            // A short stack with a quick index decay: round and punchy.
            fm("FM Bass", 0.0)
                .with_op(1, 1.0, 1.0, [0.0, 700.0, 0.7, 80.0])
                .with_op(2, 1.0, 0.45, [0.0, 320.0, 0.2, 80.0])
                .with_op(3, 2.0, 0.15, [0.0, 150.0, 0.0, 60.0])
                .with(P::VelocitySens, 0.5)
                .with(P::Volume, 0.55),
            // A sine body plus a stack that swells in, like a section.
            fm("FM Brass", 4.0)
                .with_op(1, 1.0, 0.45, [40.0, 600.0, 0.8, 200.0])
                .with_op(2, 1.0, 0.8, [60.0, 500.0, 0.8, 200.0])
                .with_op(3, 1.0, 0.4, [90.0, 500.0, 0.6, 200.0])
                .with_op(4, 1.0, 0.2, [120.0, 400.0, 0.5, 200.0])
                .with(P::AmpRelease, 400.0)
                .with(P::Lfo1Rate, 5.0)
                .with_mod(0, S::ModWheel, D::Pitch, 0.01)
                .with_mod(1, S::Velocity, D::FmIndex, 0.4),
            // Six sine drawbars.
            fm("FM Organ", 6.0)
                .with_op(1, 0.5, 0.6, [5.0, 0.0, 1.0, 80.0])
                .with_op(2, 1.0, 0.6, [5.0, 0.0, 1.0, 80.0])
                .with_op(3, 1.5, 0.35, [5.0, 0.0, 1.0, 80.0])
                .with_op(4, 2.0, 0.5, [5.0, 0.0, 1.0, 80.0])
                .with_op(5, 3.0, 0.3, [5.0, 0.0, 1.0, 80.0])
                .with_op(6, 4.0, 0.25, [5.0, 0.0, 1.0, 80.0])
                .with(P::FmFeedback, 0.1)
                .with(P::AmpRelease, 150.0)
                .with(P::Lfo1Rate, 6.5)
                .with_mod(0, S::Lfo1, D::Pitch, 0.004),
            // A woody hit: a fast high modulator on a decaying sine.
            fm("FM Marimba", 2.0)
                .with_op(1, 1.0, 0.9, [0.0, 700.0, 0.0, 400.0])
                .with_op(2, 4.0, 0.3, [0.0, 110.0, 0.0, 60.0])
                .with_op(3, 3.0, 0.25, [0.0, 300.0, 0.0, 150.0])
                .with_op(4, 1.0, 0.1, [0.0, 200.0, 0.0, 100.0])
                .with(P::VelocitySens, 0.8),
            // A nasal stack with key scaling: thinner up the keyboard.
            fm("FM Clav", 0.0)
                .with_op(1, 1.0, 1.0, [0.0, 900.0, 0.3, 60.0])
                .with_op(2, 3.0, 0.5, [0.0, 400.0, 0.2, 60.0])
                .with_op(3, 1.0, 0.2, [0.0, 200.0, 0.0, 60.0])
                .with_opx(2, OpField::KeyScale, -3.0)
                .with_opx(2, OpField::Velocity, 1.0),
            // Two detuned stacks swelling slowly.
            fm("FM Pad", 1.0)
                .with_op(1, 1.0, 0.7, [800.0, 2000.0, 0.8, 2000.0])
                .with_op(2, 2.0, 0.2, [1200.0, 2500.0, 0.6, 2000.0])
                .with_opx(2, OpField::Fine, 7.0)
                .with_op(3, 1.0, 0.1, [1500.0, 2000.0, 0.5, 2000.0])
                .with_op(4, 1.0, 0.7, [800.0, 2000.0, 0.8, 2000.0])
                .with_opx(4, OpField::Fine, -7.0)
                .with_op(5, 3.0, 0.15, [1000.0, 2000.0, 0.5, 2000.0])
                .with_op(6, 1.0, 0.1, [1000.0, 2000.0, 0.5, 2000.0])
                .with(P::FmFeedback, 0.1)
                .with(P::Lfo1Rate, 0.3)
                .with_mod(0, S::Lfo1, D::FmIndex, 0.3),
            // Breathy: a low-index stack with vibrato on the mod wheel.
            fm("FM Flute", 4.0)
                .with_op(1, 1.0, 0.75, [80.0, 0.0, 1.0, 120.0])
                .with_op(2, 1.0, 0.2, [120.0, 400.0, 0.6, 120.0])
                .with_op(3, 2.0, 0.05, [60.0, 200.0, 0.3, 100.0])
                .with(P::Lfo1Rate, 5.0)
                .with_mod(0, S::ModWheel, D::Pitch, 0.012)
                .with_mod(1, S::Lfo1, D::Amp, 0.04),
            // A sine whose pitch drops an octave in 50 ms.
            fm("FM Kick", 0.0)
                .with_op(1, 1.0, 1.0, [0.0, 350.0, 0.0, 50.0])
                .with_op(2, 1.0, 0.2, [0.0, 40.0, 0.0, 20.0])
                .with(P::ModAttack, 0.0)
                .with(P::ModDecay, 60.0)
                .with(P::ModSustain, 0.0)
                .with_mod(0, S::ModEnv, D::Pitch, 1.0)
                .with(P::Volume, 0.7),
            // A tone plus a noisy feedback operator.
            fm("FM Snare", 5.0)
                .with_op(1, 1.0, 0.6, [0.0, 160.0, 0.0, 60.0])
                .with_op(2, 1.6, 0.3, [0.0, 120.0, 0.0, 60.0])
                .with_op(6, 1.41, 0.7, [0.0, 180.0, 0.0, 60.0])
                .with(P::FmFeedback, 0.95)
                .with(P::Volume, 0.6),
            // Fixed high frequencies in a noisy pair.
            fm("FM Hat", 2.0)
                .with_op(1, 1.0, 0.5, [0.0, 70.0, 0.0, 40.0])
                .with_opx(1, OpField::Fixed, 1.0)
                .with_opx(1, OpField::Freq, 7900.0)
                .with_op(2, 1.0, 0.8, [0.0, 70.0, 0.0, 40.0])
                .with_opx(2, OpField::Fixed, 1.0)
                .with_opx(2, OpField::Freq, 11_300.0)
                .with_op(5, 1.0, 0.25, [0.0, 50.0, 0.0, 30.0])
                .with_opx(5, OpField::Fixed, 1.0)
                .with_opx(5, OpField::Freq, 9600.0)
                .with_op(6, 1.0, 0.7, [0.0, 50.0, 0.0, 30.0])
                .with(P::FmFeedback, 1.0),
            // The simplest pluck: an octave modulator snapping shut.
            fm("FM Pluck", 0.0)
                .with_op(1, 1.0, 0.9, [0.0, 500.0, 0.0, 200.0])
                .with_op(2, 2.0, 0.5, [0.0, 160.0, 0.0, 100.0])
                .with_opx(2, OpField::Velocity, 0.9),
            // One modulator fanned out over three carriers at rising ratios.
            fm("FM Glass Keys", 7.0)
                .with_op(1, 1.0, 0.6, [0.0, 2500.0, 0.0, 1500.0])
                .with_op(2, 3.0, 0.4, [0.0, 1800.0, 0.0, 1200.0])
                .with_op(3, 5.0, 0.3, [0.0, 1200.0, 0.0, 900.0])
                .with_op(4, 7.0, 0.25, [0.0, 800.0, 0.0, 500.0])
                .with_op(5, 1.0, 0.1, [0.0, 600.0, 0.0, 400.0]),
            // An LFO pumping the modulation index.
            fm("FM Growl Bass", 0.0)
                .with_op(1, 1.0, 1.0, [0.0, 0.0, 1.0, 80.0])
                .with_op(2, 0.5, 0.45, [0.0, 0.0, 1.0, 80.0])
                .with_op(3, 1.0, 0.3, [0.0, 0.0, 1.0, 80.0])
                .with(P::Lfo1Wave, 1.0)
                .with(P::Lfo1Rate, 2.0)
                .with_mod(0, S::Lfo1, D::FmIndex, 0.8)
                .with(P::Volume, 0.5),
            // A sub-octave sine with a short click on top.
            fm("FM Sub Thump", 4.0)
                .with_op(1, 0.5, 1.0, [0.0, 450.0, 0.4, 100.0])
                .with_op(2, 1.0, 0.4, [0.0, 200.0, 0.0, 80.0])
                .with_op(3, 1.0, 0.2, [0.0, 80.0, 0.0, 50.0]),
            // Formant-like branches with a slow vibrato.
            fm("FM Choir", 3.0)
                .with_op(1, 1.0, 0.7, [300.0, 0.0, 1.0, 900.0])
                .with_op(2, 2.0, 0.15, [300.0, 0.0, 1.0, 900.0])
                .with_op(3, 3.0, 0.1, [300.0, 0.0, 1.0, 900.0])
                .with_op(4, 4.0, 0.08, [300.0, 0.0, 1.0, 900.0])
                .with_op(5, 2.0, 0.3, [400.0, 0.0, 1.0, 900.0])
                .with_op(6, 1.0, 0.1, [400.0, 0.0, 1.0, 900.0])
                .with(P::FmFeedback, 0.2)
                .with(P::Lfo2Rate, 4.5)
                .with_mod(0, S::Lfo2, D::Pitch, 0.006),
            // A full stack with feedback, gliding.
            fm("FM Metal Lead", 0.0)
                .with_op(1, 1.0, 0.9, [5.0, 0.0, 1.0, 150.0])
                .with_op(2, 1.0, 0.45, [5.0, 600.0, 0.7, 150.0])
                .with_op(3, 2.0, 0.3, [5.0, 600.0, 0.6, 150.0])
                .with_op(4, 1.0, 0.2, [5.0, 600.0, 0.6, 150.0])
                .with_op(5, 3.0, 0.15, [5.0, 600.0, 0.5, 150.0])
                .with_op(6, 1.0, 0.2, [5.0, 600.0, 0.5, 150.0])
                .with(P::FmFeedback, 0.6)
                .with(P::Glide, 60.0)
                .with(P::Volume, 0.4),
            // Plucked strings in three pairs.
            fm("FM Harp", 2.0)
                .with_op(1, 1.0, 0.8, [0.0, 2000.0, 0.0, 1500.0])
                .with_op(2, 2.0, 0.3, [0.0, 600.0, 0.0, 400.0])
                .with_op(3, 2.0, 0.4, [0.0, 1500.0, 0.0, 1000.0])
                .with_opx(3, OpField::Fine, 5.0)
                .with_op(4, 1.0, 0.2, [0.0, 400.0, 0.0, 300.0])
                .with_op(5, 4.0, 0.15, [0.0, 800.0, 0.0, 600.0])
                .with_op(6, 1.0, 0.1, [0.0, 300.0, 0.0, 200.0]),
            // Free routing: a cross-modulated pair on carrier 1, the index on LFO 1.
            fm("FM Wobble Free", 8.0)
                .with_op(1, 1.0, 1.0, [0.0, 0.0, 1.0, 100.0])
                .with_op(2, 0.5, 0.5, [0.0, 0.0, 1.0, 100.0])
                .with_op(3, 1.5, 0.3, [0.0, 0.0, 1.0, 100.0])
                .with(P::fm_mod(0, 1), 0.6)
                .with(P::fm_mod(1, 2), 0.4)
                .with(P::fm_mod(2, 0), 0.2)
                .with(P::Lfo1Rate, 1.5)
                .with_mod(0, S::Lfo1, D::FmIndex, 0.9),
            // A high modulator shared by bright little carriers.
            fm("FM Toy Box", 5.0)
                .with_op(1, 1.0, 0.6, [0.0, 900.0, 0.0, 600.0])
                .with_op(2, 2.0, 0.4, [0.0, 700.0, 0.0, 500.0])
                .with_op(3, 4.0, 0.2, [0.0, 500.0, 0.0, 400.0])
                .with_op(6, 9.0, 0.2, [0.0, 400.0, 0.0, 300.0]),
            // Pairs at near-octaves: a steel pan.
            fm("FM Steel Drum", 2.0)
                .with_op(1, 1.0, 0.9, [0.0, 1200.0, 0.0, 800.0])
                .with_op(2, 1.5, 0.35, [0.0, 300.0, 0.0, 200.0])
                .with_op(3, 2.01, 0.4, [0.0, 900.0, 0.0, 600.0])
                .with_op(4, 3.0, 0.15, [0.0, 200.0, 0.0, 150.0]),
        ]
    }

    /// The wavetable factory presets (Prompt 24). All original.
    fn wavetable_factory() -> Vec<SynthPatch> {
        use ModDest as D;
        use ModSource as S;
        use SynthParam as P;
        let wt = |name: &str, table: f32, pos: f32| {
            SynthPatch {
                name: name.to_owned(),
                ..SynthPatch::default()
            }
            .with(P::Engine, 2.0)
            .with(P::Wt1Table, table)
            .with(P::Wt1Pos, pos)
            .with(P::Osc1Level, 0.8)
            .with(P::Osc2Level, 0.0)
            .with(P::Osc2Fine, 0.0)
            .with(P::Cutoff, 6000.0)
            .with(P::Resonance, 0.15)
            .with(P::FilterEnv, 0.0)
            .with(P::Volume, 0.45)
        };
        let osc2 = |p: SynthPatch, table: f32, pos: f32, level: f32, fine: f32| {
            p.with(P::Wt2Table, table)
                .with(P::Wt2Pos, pos)
                .with(P::Osc2Level, level)
                .with(P::Osc2Fine, fine)
        };
        let pad = |p: SynthPatch| {
            p.with(P::AmpAttack, 800.0)
                .with(P::AmpDecay, 1500.0)
                .with(P::AmpSustain, 0.85)
                .with(P::AmpRelease, 2000.0)
        };
        vec![
            // Shapes morphing slowly on two detuned oscillators.
            pad(osc2(wt("WT Morph Pad", 0.0, 0.3), 1.0, 0.2, 0.5, 7.0))
                .with(P::UnisonVoices, 3.0)
                .with(P::Cutoff, 3000.0)
                .with(P::Lfo1Rate, 0.12)
                .with_mod(0, S::Lfo1, D::Wt1Pos, 0.3)
                .with_mod(1, S::Lfo1, D::Wt2Pos, -0.2),
            // Vowels drifting under a gliding lead.
            wt("WT Vowel Lead", 3.0, 0.0)
                .with(P::Lfo1Rate, 0.7)
                .with(P::Lfo2Rate, 5.0)
                .with(P::Glide, 60.0)
                .with(P::AmpSustain, 0.9)
                .with_mod(0, S::Lfo1, D::Wt1Pos, 0.5)
                .with_mod(1, S::ModWheel, D::Pitch, 0.01),
            // The mod envelope sweeping the harmonics shut.
            wt("WT Sweep Bass", 1.0, 0.2)
                .with(P::SubLevel, 0.5)
                .with(P::Cutoff, 900.0)
                .with(P::Resonance, 0.3)
                .with(P::ModAttack, 0.0)
                .with(P::ModDecay, 250.0)
                .with(P::ModSustain, 0.0)
                .with(P::AmpSustain, 0.8)
                .with(P::AmpRelease, 80.0)
                .with_mod(0, S::ModEnv, D::Wt1Pos, 0.6),
            // Drawbars, held.
            wt("WT Organ", 2.0, 0.6)
                .with(P::AmpAttack, 5.0)
                .with(P::AmpSustain, 1.0)
                .with(P::AmpRelease, 100.0)
                .with(P::Lfo2Rate, 6.0)
                .with_mod(0, S::Lfo2, D::Pitch, 0.004),
            // Pulse widths moving under wide unison.
            pad(osc2(wt("WT PWM Strings", 4.0, 0.3), 4.0, 0.5, 0.6, -9.0))
                .with(P::UnisonVoices, 5.0)
                .with(P::UnisonDetune, 18.0)
                .with(P::UnisonSpread, 0.8)
                .with(P::Cutoff, 4000.0)
                .with(P::Lfo1Rate, 0.4)
                .with_mod(0, S::Lfo1, D::Wt1Pos, 0.25)
                .with_mod(1, S::Lfo1, D::Wt2Pos, -0.25),
            // Noise-derived spectra crawling.
            wt("WT Dust Texture", 5.0, 0.0)
                .with(P::NoiseLevel, 0.1)
                .with(P::Cutoff, 2500.0)
                .with(P::AmpAttack, 1500.0)
                .with(P::AmpSustain, 0.9)
                .with(P::AmpRelease, 3000.0)
                .with(P::Lfo2Rate, 0.1)
                .with_mod(0, S::Lfo2, D::Wt1Pos, 0.4)
                .with_mod(1, S::Lfo1, D::Pan, 0.3),
            // Sync ratios swept by the envelope.
            wt("WT Sync Lead", 6.0, 0.1)
                .with(P::UnisonVoices, 2.0)
                .with(P::UnisonDetune, 10.0)
                .with(P::Drive, 0.3)
                .with(P::ModAttack, 0.0)
                .with(P::ModDecay, 700.0)
                .with(P::ModSustain, 0.2)
                .with_mod(0, S::ModEnv, D::Wt1Pos, 0.6),
            // Harder playing folds the sine further.
            wt("WT Fold Bass", 7.0, 0.1)
                .with(P::SubLevel, 0.4)
                .with(P::Cutoff, 1200.0)
                .with(P::AmpRelease, 80.0)
                .with_mod(0, S::Velocity, D::Wt1Pos, 0.6),
            // A saw bent by a falling envelope.
            wt("WT Bend Pluck", 0.0, 0.66)
                .with(P::Wt1Warp, 2.0)
                .with(P::Wt1WarpAmt, 0.2)
                .with(P::AmpDecay, 500.0)
                .with(P::AmpSustain, 0.0)
                .with(P::AmpRelease, 300.0)
                .with(P::ModAttack, 0.0)
                .with(P::ModDecay, 300.0)
                .with(P::ModSustain, 0.0)
                .with_mod(0, S::ModEnv, D::Wt1Warp, 0.7),
            // A square whose pulse width the LFO moves.
            wt("WT PWM Bass", 0.0, 1.0)
                .with(P::Wt1Warp, 3.0)
                .with(P::Wt1WarpAmt, 0.3)
                .with(P::SubLevel, 0.3)
                .with(P::Cutoff, 1500.0)
                .with(P::Lfo1Rate, 0.8)
                .with_mod(0, S::Lfo1, D::Wt1Warp, 0.4),
            // Classic hard sync with the envelope on the ratio.
            wt("WT Hard Sync", 0.0, 0.66)
                .with(P::Wt1Warp, 1.0)
                .with(P::Wt1WarpAmt, 0.2)
                .with(P::ModAttack, 0.0)
                .with(P::ModDecay, 600.0)
                .with(P::ModSustain, 0.1)
                .with_mod(0, S::ModEnv, D::Wt1Warp, 0.6),
            // Two vowel tables an octave apart, the mod wheel on the second.
            pad(osc2(wt("WT Choir Pad", 3.0, 0.5), 3.0, 0.2, 0.4, 5.0))
                .with(P::Osc2Octave, 1.0)
                .with(P::UnisonVoices, 4.0)
                .with(P::Lfo1Rate, 0.25)
                .with_mod(0, S::Lfo1, D::Wt1Pos, 0.3)
                .with_mod(1, S::ModWheel, D::Wt2Pos, 0.8),
            // Bright drawbars and harmonics ringing out.
            osc2(wt("WT Glass Bells", 2.0, 1.0), 1.0, 1.0, 0.3, 0.0)
                .with(P::Osc2Octave, 1.0)
                .with(P::Cutoff, 8000.0)
                .with(P::AmpDecay, 2500.0)
                .with(P::AmpSustain, 0.0)
                .with(P::AmpRelease, 2000.0),
            // Two saws a little apart, through a low filter.
            osc2(wt("WT Reese", 0.0, 0.66), 0.0, 0.66, 0.8, -12.0)
                .with(P::UnisonVoices, 3.0)
                .with(P::UnisonDetune, 12.0)
                .with(P::Cutoff, 500.0)
                .with(P::Resonance, 0.3)
                .with(P::AmpRelease, 100.0)
                .with(P::Lfo2Rate, 0.3)
                .with_mod(0, S::Lfo2, D::Cutoff, 0.1),
            // Vowels chopped by a square LFO.
            wt("WT Formant Wobble", 3.0, 0.2)
                .with(P::Lfo1Wave, 3.0)
                .with(P::Lfo1Rate, 3.0)
                .with(P::SubLevel, 0.3)
                .with_mod(0, S::Lfo1, D::Wt1Pos, 0.5),
            // A sync table plucked, brighter when struck harder.
            wt("WT Digital Keys", 6.0, 0.2)
                .with(P::AmpDecay, 900.0)
                .with(P::AmpSustain, 0.3)
                .with(P::ModAttack, 0.0)
                .with(P::ModDecay, 400.0)
                .with(P::ModSustain, 0.0)
                .with(P::Cutoff, 3000.0)
                .with_mod(0, S::ModEnv, D::Wt1Pos, 0.4)
                .with_mod(1, S::Velocity, D::Cutoff, 0.3),
            // Noise spectra rising over three seconds.
            wt("WT Noise Sweep", 5.0, 0.0)
                .with(P::ModAttack, 3000.0)
                .with(P::ModSustain, 1.0)
                .with(P::KeyTrack, 1.0)
                .with(P::AmpAttack, 500.0)
                .with(P::AmpSustain, 1.0)
                .with(P::AmpRelease, 1500.0)
                .with_mod(0, S::ModEnv, D::Wt1Pos, 1.0),
            // A plain sine with a sub: for low ends.
            wt("WT Soft Sine Sub", 0.0, 0.0)
                .with(P::SubLevel, 0.3)
                .with(P::Cutoff, 1500.0)
                .with(P::AmpRelease, 120.0)
                .with(P::Volume, 0.55),
            // Harmonics added one by one as the note holds.
            wt("WT Harmonic Rise", 1.0, 0.0)
                .with(P::ModAttack, 2000.0)
                .with(P::ModSustain, 1.0)
                .with(P::AmpSustain, 1.0)
                .with(P::AmpRelease, 600.0)
                .with_mod(0, S::ModEnv, D::Wt1Pos, 1.0),
            // A folded lead, aftertouch folding harder.
            wt("WT Folded Lead", 7.0, 0.15)
                .with(P::Glide, 40.0)
                .with(P::AmpSustain, 0.9)
                .with(P::Drive, 0.2)
                .with_mod(0, S::Aftertouch, D::Wt1Pos, 0.6)
                .with_mod(1, S::ModWheel, D::Wt1Pos, 0.6),
            // Drawbars pulled by the mod wheel.
            wt("WT Organ Swell", 2.0, 0.0)
                .with(P::AmpAttack, 5.0)
                .with(P::AmpSustain, 1.0)
                .with(P::AmpRelease, 100.0)
                .with_mod(0, S::ModWheel, D::Wt1Pos, 1.0),
            // A pulse table bent by a slow LFO.
            pad(wt("WT Bent Pad", 4.0, 0.4))
                .with(P::Wt1Warp, 2.0)
                .with(P::Wt1WarpAmt, 0.2)
                .with(P::UnisonVoices, 3.0)
                .with(P::Lfo2Rate, 0.15)
                .with_mod(0, S::Lfo2, D::Wt1Warp, 0.3),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_defaults_in_range() {
        let mut keys: Vec<_> = SynthParam::ALL.iter().map(|p| p.info().key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), SynthParam::COUNT);
        for &p in SynthParam::ALL {
            let i = p.info();
            assert_eq!(i.clamp(i.default), i.default, "{}", i.key);
            assert_eq!(SynthParam::from_key(i.key), Some(p));
        }
    }

    #[test]
    fn normalized_round_trips() {
        for &p in SynthParam::ALL {
            let i = p.info();
            for t in [0.0, 0.25, 0.5, 0.9, 1.0] {
                let v = i.from_normalized(t);
                let back = i.from_normalized(i.to_normalized(v));
                assert!((back - v).abs() <= 1e-3 * (i.max - i.min), "{} {t}", i.key);
            }
        }
        let cutoff = SynthParam::Cutoff.info();
        // Logarithmic: half-way is the geometric mean, about 632 Hz.
        assert!((cutoff.from_normalized(0.5) - 632.5).abs() < 1.0);
    }

    #[test]
    fn formats() {
        assert_eq!(SynthParam::Cutoff.info().format(2500.0), "2.50 kHz");
        assert_eq!(SynthParam::Osc1Wave.info().format(2.0), "Saw");
        assert_eq!(SynthParam::AmpRelease.info().format(1500.0), "1.50 s");
        assert_eq!(SynthParam::Osc2Fine.info().format(7.0), "+7 ct");
        assert_eq!(SynthParam::Volume.info().format(1.0), "+0.0 dB");
    }

    #[test]
    fn engine_parameters_are_grouped() {
        assert_eq!(
            SynthParam::op(5, OpField::Velocity),
            SynthParam::Op6Velocity
        );
        assert_eq!(SynthParam::fm_mod(2, 4), SynthParam::FmMod35);
        assert_eq!(SynthParam::fm_out(5), SynthParam::FmOut6);
        assert_eq!(SynthParam::wt(1)[3], SynthParam::Wt2WarpAmt);
        assert!(SynthParam::Cutoff.used_by(2) && !SynthParam::Cutoff.used_by(1));
        assert!(SynthParam::Osc1Wave.used_by(0) && !SynthParam::Osc1Wave.used_by(2));
        assert!(SynthParam::AmpRelease.used_by(1));
        assert!(SynthParam::FmFeedback.used_by(1) && !SynthParam::FmFeedback.used_by(0));
        assert!(SynthParam::Wt2Pos.used_by(2) && !SynthParam::Engine.used_by(0));
    }

    #[test]
    fn factory_presets_are_valid_and_named_uniquely() {
        let f = SynthPatch::factory();
        let mut names: Vec<_> = f.iter().map(|p| p.name.clone()).collect();
        names.dedup();
        assert_eq!(names.len(), f.len());
        for p in &f {
            let mut s = p.clone();
            s.sanitize();
            assert_eq!(&s, p, "{}", p.name);
        }
        assert_eq!(ModDest::from_key("cutoff"), Some(ModDest::Cutoff));
        // Prompt 24: twenty or more of each new engine.
        for engine in [1, 2, 3] {
            assert!(f.iter().filter(|p| p.engine() == engine).count() >= 20);
        }
        let mut all: Vec<_> = f.iter().map(|p| p.name.as_str()).collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), f.len());
    }
}
