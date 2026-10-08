//! Parameter descriptions shared by synth patches, the sampler, the mixer and its effects.
//!
//! A [`ParamInfo`] gives a parameter's stable key, range, default, knob taper and display
//! format, so the UI, preset files and the engine agree on them.

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
    /// Stereo position from -1 (left) to 1 (right), shown as "L 40 %", "C" or "R 100 %".
    Pan,
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
    /// A level in dB (the value is already in dB).
    Db,
    /// A compression ratio, shown as "4.0:1".
    Ratio,
    /// Time in seconds.
    Seconds,
    /// A plain number with two decimals (filter Q).
    Plain,
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
            ParamUnit::Pan => match (v * 100.0).round() as i32 {
                0 => "C".to_owned(),
                p if p < 0 => format!("L {} %", -p),
                p => format!("R {p} %"),
            },
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
            ParamUnit::Db => format!("{v:+.1} dB"),
            ParamUnit::Ratio => format!("{v:.1}:1"),
            ParamUnit::Seconds if v < 10.0 => format!("{v:.2} s"),
            ParamUnit::Seconds => format!("{v:.1} s"),
            ParamUnit::Plain => format!("{v:.2}"),
            ParamUnit::Choice => self
                .choices
                .get(v as usize)
                .copied()
                .unwrap_or("?")
                .to_owned(),
        }
    }
}
