//! The drum synth's sounds as document data (Prompt 28): a recipe and its parameters. The
//! sound itself is made by `gt_dsp::drums::render`.

/// The kind of sound a [`DrumVoice`] makes: which recipe its parameters drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum DrumModel {
    /// A sine falling in pitch, a noise click, soft saturation.
    #[default]
    Kick,
    /// Two damped sines (the head) plus high-passed noise (the wires).
    Snare,
    /// Six inharmonic square waves plus noise, high-passed twice (closed or open by its decay).
    Hat,
    /// Band-passed noise in quick bursts, then a short tail.
    Clap,
    /// The kick recipe tuned and shaped as a tom.
    Tom,
    /// Two square partials through a band-pass (cowbell, rim, wood block).
    Perc,
}

impl DrumModel {
    /// Every model, in menu order.
    pub const ALL: [DrumModel; 6] = [
        Self::Kick,
        Self::Snare,
        Self::Hat,
        Self::Clap,
        Self::Tom,
        Self::Perc,
    ];

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Kick => "Kick",
            Self::Snare => "Snare",
            Self::Hat => "Hat",
            Self::Clap => "Clap",
            Self::Tom => "Tom",
            Self::Perc => "Perc",
        }
    }
}

/// A synthesized drum sound (Prompt 28): a recipe ([`DrumModel`]) and its parameters, all
/// in plain units (seconds, Hz, linear levels). `gt_dsp::drums::render` turns it into a sample;
/// the four built-in drums are [`DrumVoice::KICK`], [`DrumVoice::SNARE`], [`DrumVoice::HAT`]
/// and [`DrumVoice::CLAP`] rendered with their defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrumVoice {
    /// Recipe.
    pub model: DrumModel,
    /// Length of the sound, s.
    pub length: f32,
    /// Body pitch, Hz (kick and tom: where the sweep ends; snare: first head mode; hat: the
    /// lowest of its six partials; perc: first partial).
    pub pitch: f32,
    /// Second pitch, Hz (snare: second head mode; perc: second partial).
    pub pitch2: f32,
    /// Pitch added at the start, Hz, falling away exponentially (kick, tom).
    pub sweep: f32,
    /// Time constant of the pitch fall, s.
    pub sweep_time: f32,
    /// Time constant of the body's decay, s (clap: of each burst).
    pub decay: f32,
    /// Level of the body (clap: of the tail).
    pub tone: f32,
    /// Level of the noise.
    pub noise: f32,
    /// Time constant of the noise decay, s (clap: of the tail).
    pub noise_decay: f32,
    /// Noise filter frequency, Hz.
    pub noise_freq: f32,
    /// Noise filter Q.
    pub noise_q: f32,
    /// Level of the attack click (kick, tom, perc).
    pub click: f32,
    /// Saturation drive (0: clean).
    pub drive: f32,
    /// Clap: time between bursts, s.
    pub spread: f32,
    /// Clap: when the tail starts, s.
    pub tail_at: f32,
    /// Noise seed (the same seed always gives the same sound).
    pub seed: u32,
}

impl DrumVoice {
    /// The built-in kick.
    pub const KICK: Self = Self {
        model: DrumModel::Kick,
        length: 0.55,
        pitch: 48.0,
        pitch2: 0.0,
        sweep: 112.0,
        sweep_time: 0.035,
        decay: 0.16,
        tone: 1.0,
        noise: 0.0,
        noise_decay: 0.0015,
        noise_freq: 1000.0,
        noise_q: 0.7,
        click: 0.35,
        drive: 2.2,
        spread: 0.0,
        tail_at: 0.0,
        seed: 0x6b69_636b,
    };

    /// The built-in snare.
    pub const SNARE: Self = Self {
        model: DrumModel::Snare,
        length: 0.38,
        pitch: 185.0,
        pitch2: 330.0,
        decay: 0.045,
        tone: 0.8,
        noise: 0.9,
        noise_decay: 0.075,
        noise_freq: 1800.0,
        noise_q: 0.7,
        click: 0.0,
        drive: 0.0,
        seed: 0x736e_6172,
        ..Self::KICK
    };

    /// The built-in closed hat.
    pub const HAT: Self = Self {
        model: DrumModel::Hat,
        length: 0.16,
        pitch: 263.0,
        decay: 0.022,
        tone: 0.6,
        noise: 0.4,
        noise_freq: 7000.0,
        noise_q: 0.7,
        click: 0.0,
        drive: 0.0,
        seed: 0x6861_7421,
        ..Self::KICK
    };

    /// An open hat: the closed one ringing longer.
    pub const OPEN_HAT: Self = Self {
        length: 0.7,
        decay: 0.2,
        seed: 0x6f68_6174,
        ..Self::HAT
    };

    /// The built-in clap.
    pub const CLAP: Self = Self {
        model: DrumModel::Clap,
        length: 0.4,
        decay: 0.0035,
        tone: 0.8,
        noise: 1.0,
        noise_decay: 0.07,
        noise_freq: 1100.0,
        noise_q: 1.2,
        click: 0.0,
        drive: 0.0,
        spread: 0.009,
        tail_at: 0.027,
        seed: 0x636c_6170,
        ..Self::KICK
    };

    /// A low tom.
    pub const TOM: Self = Self {
        model: DrumModel::Tom,
        length: 0.6,
        pitch: 95.0,
        sweep: 45.0,
        sweep_time: 0.06,
        decay: 0.2,
        click: 0.12,
        drive: 0.6,
        seed: 0x746f_6d21,
        ..Self::KICK
    };

    /// A cowbell-like percussion.
    pub const PERC: Self = Self {
        model: DrumModel::Perc,
        length: 0.35,
        pitch: 540.0,
        pitch2: 800.0,
        decay: 0.06,
        tone: 1.0,
        noise: 0.0,
        noise_freq: 900.0,
        noise_q: 1.5,
        click: 0.2,
        drive: 0.0,
        seed: 0x7065_7263,
        ..Self::KICK
    };

    /// The defaults of `model`.
    pub fn defaults(model: DrumModel) -> Self {
        match model {
            DrumModel::Kick => Self::KICK,
            DrumModel::Snare => Self::SNARE,
            DrumModel::Hat => Self::HAT,
            DrumModel::Clap => Self::CLAP,
            DrumModel::Tom => Self::TOM,
            DrumModel::Perc => Self::PERC,
        }
    }

    /// Puts every value in a usable range (and non-finite ones back to the model's default).
    pub fn sanitize(&mut self) {
        let d = Self::defaults(self.model);
        let fix = |v: &mut f32, lo: f32, hi: f32, def: f32| {
            *v = if v.is_finite() { v.clamp(lo, hi) } else { def };
        };
        fix(&mut self.length, 0.02, 4.0, d.length);
        fix(&mut self.pitch, 10.0, 12_000.0, d.pitch);
        fix(&mut self.pitch2, 0.0, 12_000.0, d.pitch2);
        fix(&mut self.sweep, 0.0, 2000.0, d.sweep);
        fix(&mut self.sweep_time, 0.0005, 1.0, d.sweep_time);
        fix(&mut self.decay, 0.0005, 4.0, d.decay);
        fix(&mut self.tone, 0.0, 2.0, d.tone);
        fix(&mut self.noise, 0.0, 2.0, d.noise);
        fix(&mut self.noise_decay, 0.0005, 4.0, d.noise_decay);
        fix(&mut self.noise_freq, 20.0, 18_000.0, d.noise_freq.max(20.0));
        fix(&mut self.noise_q, 0.1, 20.0, d.noise_q);
        fix(&mut self.click, 0.0, 2.0, d.click);
        fix(&mut self.drive, 0.0, 10.0, d.drive);
        fix(&mut self.spread, 0.0, 0.1, d.spread);
        fix(&mut self.tail_at, 0.0, 0.5, d.tail_at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_already_in_range_and_bad_values_fall_back() {
        let all = DrumModel::ALL
            .map(DrumVoice::defaults)
            .into_iter()
            .chain([DrumVoice::OPEN_HAT]);
        for v in all {
            let mut s = v;
            s.sanitize();
            assert_eq!(s, v, "{:?}", v.model);
        }
        let mut v = DrumVoice::SNARE;
        v.pitch = f32::NAN;
        v.decay = -1.0;
        v.sanitize();
        assert_eq!(v.pitch, DrumVoice::SNARE.pitch);
        assert!(v.decay > 0.0);
    }
}
