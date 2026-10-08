//! Gloom Synth patches as document data.
//!
//! A patch is a flat array of parameter values in plain units (Hz, ms, semitones...) indexed by
//! [`SynthParam`], plus an 8-slot modulation matrix. The index of each parameter and its `key`
//! string never change once released: automation (Prompt 8) addresses parameters by index and
//! preset files by key. [`ParamInfo`] gives each parameter's range, default, knob taper and
//! display format, so the UI, presets and the engine all agree on them.

/// How a knob's travel (0..1) maps to the value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamCurve {
    /// Even steps.
    Linear,
    /// Logarithmic: equal travel for equal ratios (frequencies, rates). Needs `min > 0`.
    Exp,
    /// `min + (max - min)·tᵖ`: more travel for small values (times).
    Power(f32),
    /// Whole numbers only.
    Stepped,
}

/// How a value is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamUnit {
    /// A fraction shown as a percentage.
    Percent,
    /// Bipolar fraction shown as a signed percentage.
    SignedPercent,
    /// Frequency in Hz.
    Hz,
    /// Time in milliseconds.
    Ms,
    /// Semitones.
    Semitones,
    /// Cents.
    Cents,
    /// Octaves.
    Octaves,
    /// Linear gain shown in dB.
    Gain,
    /// A count.
    Count,
    /// One of the parameter's `choices`.
    Choice,
}

/// Description of one parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamInfo {
    /// Stable identifier used in preset files.
    pub key: &'static str,
    /// Short label for a knob.
    pub name: &'static str,
    /// Lowest value.
    pub min: f32,
    /// Highest value.
    pub max: f32,
    /// Value of a new patch (and double-click reset).
    pub default: f32,
    /// Knob taper.
    pub curve: ParamCurve,
    /// Display unit.
    pub unit: ParamUnit,
    /// Names for `ParamUnit::Choice`, indexed by value.
    pub choices: &'static [&'static str],
}

impl ParamInfo {
    /// Clamps (and for stepped parameters, rounds) a value into range.
    pub fn clamp(&self, v: f32) -> f32 {
        let v = if v.is_finite() { v } else { self.default };
        let v = v.clamp(self.min, self.max);
        if matches!(self.curve, ParamCurve::Stepped) {
            v.round()
        } else {
            v
        }
    }

    /// Knob position (0..1) for a value.
    pub fn to_normalized(&self, v: f32) -> f32 {
        let v = self.clamp(v);
        let span = self.max - self.min;
        if span <= 0.0 {
            return 0.0;
        }
        match self.curve {
            ParamCurve::Linear | ParamCurve::Stepped => (v - self.min) / span,
            ParamCurve::Exp => (v / self.min).ln() / (self.max / self.min).ln(),
            ParamCurve::Power(p) => ((v - self.min) / span).powf(1.0 / p),
        }
    }

    /// Value for a knob position (0..1).
    pub fn from_normalized(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        let v = match self.curve {
            ParamCurve::Linear | ParamCurve::Stepped => self.min + t * (self.max - self.min),
            ParamCurve::Exp => self.min * (self.max / self.min).powf(t),
            ParamCurve::Power(p) => self.min + t.powf(p) * (self.max - self.min),
        };
        self.clamp(v)
    }

    /// Text for a value, e.g. "2.50 kHz", "120 ms", "Saw".
    pub fn format(&self, v: f32) -> String {
        let v = self.clamp(v);
        match self.unit {
            ParamUnit::Percent => format!("{:.0} %", v * 100.0),
            ParamUnit::SignedPercent => format!("{:+.0} %", v * 100.0),
            ParamUnit::Hz if v >= 1000.0 => format!("{:.2} kHz", v / 1000.0),
            ParamUnit::Hz if v < 10.0 => format!("{v:.2} Hz"),
            ParamUnit::Hz => format!("{v:.0} Hz"),
            ParamUnit::Ms if v >= 1000.0 => format!("{:.2} s", v / 1000.0),
            ParamUnit::Ms if v < 10.0 => format!("{v:.1} ms"),
            ParamUnit::Ms => format!("{v:.0} ms"),
            ParamUnit::Semitones => format!("{v:+.0} st"),
            ParamUnit::Cents => format!("{v:+.0} ct"),
            ParamUnit::Octaves => format!("{v:+.1} oct"),
            ParamUnit::Gain if v <= 1e-5 => "-inf dB".to_owned(),
            ParamUnit::Gain => format!("{:+.1} dB", 20.0 * v.log10()),
            ParamUnit::Count => format!("{v:.0}"),
            ParamUnit::Choice => self
                .choices
                .get(v as usize)
                .copied()
                .unwrap_or("?")
                .to_owned(),
        }
    }
}

/// Oscillator waveform names, indexed by value.
pub const OSC_WAVES: &[&str] = &["Sine", "Triangle", "Saw", "Square"];
/// LFO waveform names, indexed by value.
pub const LFO_WAVES: &[&str] = &["Sine", "Triangle", "Saw", "Square", "S&H"];

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
}

impl Default for SynthPatch {
    fn default() -> Self {
        Self {
            name: "Init".to_owned(),
            values: std::array::from_fn(|i| SynthParam::ALL[i].info().default),
            mods: [ModSlot::default(); MOD_SLOTS],
        }
    }
}

impl SynthPatch {
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
        vec![
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
    }
}
