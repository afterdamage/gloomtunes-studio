//! What every Gloom Synth engine shares (Prompt 24): envelope and LFO settings, the
//! modulation matrix and its sources, the player's expression, and pitch and pan helpers.

use crate::adsr::AdsrParams;
use crate::lfo::LfoWave;

/// Envelope times in milliseconds and sustain level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvSettings {
    /// Attack time.
    pub attack_ms: f32,
    /// Decay time.
    pub decay_ms: f32,
    /// Sustain level, 0 to 1.
    pub sustain: f32,
    /// Release time.
    pub release_ms: f32,
}

impl EnvSettings {
    /// The envelope's per-sample coefficients at `sr`.
    pub fn params(&self, sr: f32) -> AdsrParams {
        AdsrParams::new(
            sr,
            self.attack_ms,
            self.decay_ms,
            self.sustain,
            self.release_ms,
        )
    }
}

/// LFO settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LfoSettings {
    /// Waveform.
    pub wave: LfoWave,
    /// Rate in Hz.
    pub rate_hz: f32,
}

/// Modulation sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModSource {
    /// Unused slot.
    #[default]
    Off,
    /// LFO 1, -1 to 1.
    Lfo1,
    /// LFO 2, -1 to 1.
    Lfo2,
    /// Mod envelope, 0 to 1.
    ModEnv,
    /// Amp envelope, 0 to 1.
    AmpEnv,
    /// Note velocity, 0 to 1.
    Velocity,
    /// Key position around middle C (60), -1 to 1 over ±5 octaves (clamped above key 120).
    Note,
    /// A random value per note, -1 to 1.
    Random,
    /// The mod wheel (CC 1), 0 to 1.
    ModWheel,
    /// Aftertouch, 0 to 1: the channel's, or the key's own if it is larger.
    Aftertouch,
    /// The note's mod X, -1 to 1 (0 for a neutral note).
    ModX,
    /// The note's mod Y, -1 to 1.
    ModY,
}

/// Modulation destinations. Amount 1.0 means the range given for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModDest {
    /// Unused slot.
    #[default]
    Off,
    /// Both oscillators, ±12 semitones.
    Pitch,
    /// Oscillator 1, ±12 semitones.
    Osc1Pitch,
    /// Oscillator 2, ±12 semitones.
    Osc2Pitch,
    /// Oscillator 1 pulse width, ±0.45.
    Osc1Pw,
    /// Oscillator 2 pulse width, ±0.45.
    Osc2Pw,
    /// Oscillator 1 level, ±1.
    Osc1Level,
    /// Oscillator 2 level, ±1.
    Osc2Level,
    /// Noise level, ±1.
    NoiseLevel,
    /// Filter cutoff, ±6 octaves.
    Cutoff,
    /// Filter resonance, ±1.
    Resonance,
    /// Pan, ±1.
    Pan,
    /// Amplitude, ±100 %.
    Amp,
    /// LFO 1 rate, ±4 octaves.
    Lfo1Rate,
    /// LFO 2 rate, ±4 octaves.
    Lfo2Rate,
    /// Unison detune, ±100 cents.
    UnisonDetune,
    /// FM: every modulation amount, ±100 %.
    FmIndex,
    /// FM: operator 6 feedback, ±1.
    FmFeedback,
    /// Wavetable 1 position, ±1.
    Wt1Pos,
    /// Wavetable 2 position, ±1.
    Wt2Pos,
    /// Wavetable 1 warp amount, ±1.
    Wt1Warp,
    /// Wavetable 2 warp amount, ±1.
    Wt2Warp,
    /// LFO 1 depth on this voice, ±100 % (Prompt 34: per-voice modulation).
    Lfo1Depth,
    /// LFO 2 depth on this voice, ±100 %.
    Lfo2Depth,
    /// Mod envelope to cutoff amount on this voice, ±6 octaves.
    ModEnvAmount,
    /// Attack time of this voice's envelopes, ±4 octaves (taken at the note start).
    EnvAttack,
    /// Decay time of this voice's envelopes, ±4 octaves (at the note start).
    EnvDecay,
    /// Release time of this voice's envelopes, ±4 octaves (at the note start).
    EnvRelease,
}

impl ModDest {
    /// Number of destinations (including `Off`).
    pub const COUNT: usize = 28;
}

/// One row of the modulation matrix.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ModSlot {
    /// Source.
    pub source: ModSource,
    /// Destination.
    pub dest: ModDest,
    /// Amount, -1 to 1.
    pub amount: f32,
}

/// Equal-power pan gains scaled so the centre is 1.0 on both sides.
#[inline]
pub(crate) fn pan_unit(pan: f32) -> (f32, f32) {
    let theta = (pan.clamp(-1.0, 1.0) + 1.0) * core::f32::consts::FRAC_PI_4;
    (
        theta.cos() * core::f32::consts::SQRT_2,
        theta.sin() * core::f32::consts::SQRT_2,
    )
}

#[inline]
pub(crate) fn key_hz(semitones: f32) -> f32 {
    440.0 * ((semitones - 69.0) / 12.0).exp2()
}

/// What the player does on top of the notes: pitch bend, mod wheel, aftertouch.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Expression {
    /// Pitch bend in semitones (the bend times the instrument's range).
    pub bend_semis: f32,
    /// Mod wheel, 0 to 1.
    pub mod_wheel: f32,
    /// Channel aftertouch, 0 to 1.
    pub pressure: f32,
}

/// The fractional part of `x` in 0..=1, without a libm call (`floor` is one on x86-64
/// without SSE 4.1). `x` must fit an `i32`.
#[inline]
pub(crate) fn fract(x: f32) -> f32 {
    let f = x - (x as i32) as f32;
    if f < 0.0 {
        f + 1.0
    } else {
        f
    }
}

/// The values of every [`ModSource`] for one voice at one control update.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ModInputs {
    pub lfo: [f32; 2],
    pub mod_env: f32,
    pub amp_env: f32,
    pub velocity: f32,
    pub key: u8,
    pub random: f32,
    pub mod_wheel: f32,
    pub pressure: f32,
    pub mod_x: f32,
    pub mod_y: f32,
}

impl ModInputs {
    #[inline]
    fn value(&self, s: ModSource) -> Option<f32> {
        Some(match s {
            ModSource::Off => return None,
            ModSource::Lfo1 => self.lfo[0],
            ModSource::Lfo2 => self.lfo[1],
            ModSource::ModEnv => self.mod_env,
            ModSource::AmpEnv => self.amp_env,
            ModSource::Velocity => self.velocity,
            ModSource::Note => ((f32::from(self.key) - 60.0) / 60.0).clamp(-1.0, 1.0),
            ModSource::Random => self.random,
            ModSource::ModWheel => self.mod_wheel,
            ModSource::Aftertouch => self.pressure,
            ModSource::ModX => self.mod_x,
            ModSource::ModY => self.mod_y,
        })
    }

    /// The matrix's sum for every destination.
    #[inline]
    pub fn eval(&self, mods: &[ModSlot]) -> [f32; ModDest::COUNT] {
        let mut m = [0.0_f32; ModDest::COUNT];
        for slot in mods {
            if let Some(s) = self.value(slot.source) {
                m[slot.dest as usize] += slot.amount * s;
            }
        }
        m
    }
}
