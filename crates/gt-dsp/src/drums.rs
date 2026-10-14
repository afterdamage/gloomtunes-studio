//! The four built-in drum sounds, synthesized from scratch at load time.
//!
//! These are original sounds made only from the code below (sines, a noise generator and
//! filters), so they carry no third-party rights. The generated audio is dedicated to the public
//! domain under CC0 1.0; the code that makes it is part of GloomTunes Studio (GPL).
//!
//! Every generator is deterministic (fixed noise seed), returns mono `f32` normalised to a peak
//! of -1 dBFS, starts with its transient on frame 0 and ends on an exact zero.

use std::f32::consts::TAU;

/// Peak level of every generated drum (-1 dBFS).
pub const DRUM_PEAK: f32 = 0.891;
const END_FADE_S: f32 = 0.005;

pub use gt_core::drum_synth::{DrumModel, DrumVoice};

/// Renders `v` at `sample_rate`: mono, deterministic, normalised to [`DRUM_PEAK`], transient
/// on frame 0, ending on an exact zero. Off the audio thread (allocates).
pub fn render(v: &DrumVoice, sample_rate: f32) -> Vec<f32> {
    let sr = sample_rate.max(1.0);
    let n = (v.length * sr) as usize;
    let mut noise = Noise::new(v.seed);
    let v = *v;
    let out = match v.model {
        DrumModel::Kick | DrumModel::Tom => {
            let mut phase = 0.0_f32;
            (0..n)
                .map(|i| {
                    let t = i as f32 / sr;
                    let freq = v.pitch + v.sweep * (-t / v.sweep_time).exp();
                    phase = (phase + freq / sr).fract();
                    let body = (TAU * phase).sin() * (-t / v.decay).exp() * v.tone;
                    let click = noise.next() * (-t / v.noise_decay).exp() * v.click;
                    saturate(body + click, v.drive)
                })
                .collect()
        }
        DrumModel::Snare => {
            let mut hp = Biquad::highpass(sr, v.noise_freq, v.noise_q);
            (0..n)
                .map(|i| {
                    let t = i as f32 / sr;
                    let head = ((TAU * v.pitch * t).sin() * 0.7 + (TAU * v.pitch2 * t).sin() * 0.3)
                        * (-t / v.decay).exp();
                    let wires = hp.process(noise.next()) * (-t / v.noise_decay).exp();
                    saturate(head * v.tone + wires * v.noise, v.drive)
                })
                .collect()
        }
        DrumModel::Hat => {
            const PARTIALS: [f32; 6] = [263.0, 400.0, 421.0, 474.0, 587.0, 845.0];
            let scale = v.pitch / PARTIALS[0];
            let mut hp1 = Biquad::highpass(sr, v.noise_freq, v.noise_q);
            let mut hp2 = Biquad::highpass(sr, v.noise_freq, v.noise_q);
            let mut phases = [0.0_f32; 6];
            (0..n)
                .map(|i| {
                    let t = i as f32 / sr;
                    let mut metal = 0.0;
                    for (p, f) in phases.iter_mut().zip(PARTIALS) {
                        *p = (*p + f * scale / sr).fract();
                        metal += if *p < 0.5 { 1.0 } else { -1.0 };
                    }
                    let x = metal / 6.0 * v.tone + noise.next() * v.noise;
                    hp2.process(hp1.process(x)) * (-t / v.decay).exp()
                })
                .collect()
        }
        DrumModel::Clap => {
            let mut bp = Biquad::bandpass(sr, v.noise_freq, v.noise_q);
            (0..n)
                .map(|i| {
                    let t = i as f32 / sr;
                    let bursts: f32 = [0.0, v.spread, v.spread * 2.0]
                        .iter()
                        .filter(|&&b| t >= b)
                        .map(|&b| (-(t - b) / v.decay).exp())
                        .sum();
                    let tail = if t >= v.tail_at {
                        v.tone * (-(t - v.tail_at) / v.noise_decay).exp()
                    } else {
                        0.0
                    };
                    bp.process(noise.next()) * v.noise * (bursts + tail)
                })
                .collect()
        }
        DrumModel::Perc => {
            let mut bp = Biquad::bandpass(sr, v.noise_freq, v.noise_q);
            let (mut p1, mut p2) = (0.0_f32, 0.0_f32);
            (0..n)
                .map(|i| {
                    let t = i as f32 / sr;
                    p1 = (p1 + v.pitch / sr).fract();
                    p2 = (p2 + v.pitch2 / sr).fract();
                    let sq = |p: f32| if p < 0.5 { 1.0 } else { -1.0 };
                    let body = bp.process((sq(p1) + sq(p2)) * 0.5) * (-t / v.decay).exp();
                    let hiss = noise.next() * (-t / 0.002).exp();
                    saturate(body * v.tone + hiss * (v.click + v.noise), v.drive)
                })
                .collect()
        }
    };
    finish(out, sr)
}

/// `tanh` saturation of `x` driven by `drive`; at drive 0 the sound passes clean.
#[inline]
fn saturate(x: f32, drive: f32) -> f32 {
    if drive > 0.0 {
        (drive * x).tanh()
    } else {
        x
    }
}

/// Kick: a sine whose pitch falls exponentially from 160 Hz to 48 Hz (the "boom"), with a short
/// noise click on top for definition, softly saturated.
pub fn kick(sample_rate: f32) -> Vec<f32> {
    render(&DrumVoice::KICK, sample_rate)
}

/// Snare: two damped sines (the drum head, 185 Hz and 330 Hz) plus high-passed noise (the
/// snare wires) with a longer decay.
pub fn snare(sample_rate: f32) -> Vec<f32> {
    render(&DrumVoice::SNARE, sample_rate)
}

/// Closed hi-hat: six square waves at inharmonic frequencies (a metallic cluster, the classic
/// analogue recipe) mixed with noise, band-limited by a high-pass at 7 kHz, very short decay.
pub fn hat(sample_rate: f32) -> Vec<f32> {
    render(&DrumVoice::HAT, sample_rate)
}

/// Clap: band-passed noise (around 1.1 kHz) shaped by three quick bursts 9 ms apart, the
/// several hands of a real clap, followed by a short diffuse tail.
pub fn clap(sample_rate: f32) -> Vec<f32> {
    render(&DrumVoice::CLAP, sample_rate)
}

/// Normalises to [`DRUM_PEAK`] and fades the last few milliseconds to an exact zero.
fn finish(mut v: Vec<f32>, sr: f32) -> Vec<f32> {
    let peak = v.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    if peak > 0.0 {
        let g = DRUM_PEAK / peak;
        v.iter_mut().for_each(|s| *s *= g);
    }
    let fade = ((END_FADE_S * sr) as usize).clamp(1, v.len().max(1));
    let len = v.len();
    for (k, s) in v[len.saturating_sub(fade)..].iter_mut().enumerate() {
        *s *= 1.0 - (k + 1) as f32 / fade as f32;
    }
    v
}

/// Deterministic white noise in -1..1 (xorshift32).
struct Noise(u32);

impl Noise {
    fn new(seed: u32) -> Self {
        Self(seed.max(1))
    }

    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// Second-order filter (RBJ cookbook), transposed direct form II.
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn from(b: [f32; 3], a: [f32; 3]) -> Self {
        Self {
            b0: b[0] / a[0],
            b1: b[1] / a[0],
            b2: b[2] / a[0],
            a1: a[1] / a[0],
            a2: a[2] / a[0],
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn highpass(sr: f32, f: f32, q: f32) -> Self {
        let w = TAU * f / sr;
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        Self::from(
            [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0],
            [1.0 + alpha, -2.0 * c, 1.0 - alpha],
        )
    }

    fn bandpass(sr: f32, f: f32, q: f32) -> Self {
        let w = TAU * f / sr;
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        Self::from([alpha, 0.0, -alpha], [1.0 + alpha, -2.0 * c, 1.0 - alpha])
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

#[cfg(test)]
mod legacy {
    //! The generators before Prompt 28, kept to prove the voice model reproduces them.
    use super::*;

    /// Kick: a sine whose pitch falls exponentially from 160 Hz to 48 Hz (the "boom"), with a short
    /// noise click on top for definition, softly saturated.
    pub(super) fn kick(sample_rate: f32) -> Vec<f32> {
        let sr = sample_rate.max(1.0);
        let n = (0.55 * sr) as usize;
        let mut noise = Noise::new(0x6b69_636b);
        let mut phase = 0.0_f32;
        let out = (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                let freq = 48.0 + 112.0 * (-t / 0.035).exp();
                phase = (phase + freq / sr).fract();
                let body = (TAU * phase).sin() * (-t / 0.16).exp();
                let click = noise.next() * (-t / 0.0015).exp() * 0.35;
                (2.2 * (body + click)).tanh()
            })
            .collect();
        finish(out, sr)
    }

    /// Snare: two damped sines (the drum head, 185 Hz and 330 Hz) plus high-passed noise (the
    /// snare wires) with a longer decay.
    pub(super) fn snare(sample_rate: f32) -> Vec<f32> {
        let sr = sample_rate.max(1.0);
        let n = (0.38 * sr) as usize;
        let mut noise = Noise::new(0x736e_6172);
        let mut hp = Biquad::highpass(sr, 1800.0, 0.7);
        let out = (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                let head = ((TAU * 185.0 * t).sin() * 0.7 + (TAU * 330.0 * t).sin() * 0.3)
                    * (-t / 0.045).exp();
                let wires = hp.process(noise.next()) * (-t / 0.075).exp();
                head * 0.8 + wires * 0.9
            })
            .collect();
        finish(out, sr)
    }

    /// Closed hi-hat: six square waves at inharmonic frequencies (a metallic cluster, the classic
    /// analogue recipe) mixed with noise, band-limited by a high-pass at 7 kHz, very short decay.
    pub(super) fn hat(sample_rate: f32) -> Vec<f32> {
        const PARTIALS: [f32; 6] = [263.0, 400.0, 421.0, 474.0, 587.0, 845.0];
        let sr = sample_rate.max(1.0);
        let n = (0.16 * sr) as usize;
        let mut noise = Noise::new(0x6861_7421);
        let mut hp1 = Biquad::highpass(sr, 7000.0, 0.7);
        let mut hp2 = Biquad::highpass(sr, 7000.0, 0.7);
        let mut phases = [0.0_f32; 6];
        let out = (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                let mut metal = 0.0;
                for (p, f) in phases.iter_mut().zip(PARTIALS) {
                    *p = (*p + f / sr).fract();
                    metal += if *p < 0.5 { 1.0 } else { -1.0 };
                }
                let x = metal / 6.0 * 0.6 + noise.next() * 0.4;
                hp2.process(hp1.process(x)) * (-t / 0.022).exp()
            })
            .collect();
        finish(out, sr)
    }

    /// Clap: band-passed noise (around 1.1 kHz) shaped by three quick bursts 9 ms apart, the
    /// several hands of a real clap, followed by a short diffuse tail.
    pub(super) fn clap(sample_rate: f32) -> Vec<f32> {
        let sr = sample_rate.max(1.0);
        let n = (0.4 * sr) as usize;
        let mut noise = Noise::new(0x636c_6170);
        let mut bp = Biquad::bandpass(sr, 1100.0, 1.2);
        let out = (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                let bursts: f32 = [0.0, 0.009, 0.018]
                    .iter()
                    .filter(|&&b| t >= b)
                    .map(|&b| (-(t - b) / 0.0035).exp())
                    .sum();
                let tail = if t >= 0.027 {
                    0.8 * (-(t - 0.027) / 0.07).exp()
                } else {
                    0.0
                };
                bp.process(noise.next()) * (bursts + tail)
            })
            .collect();
        finish(out, sr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_voice_model_reproduces_the_built_in_drums_exactly() {
        for sr in [44_100.0, 48_000.0] {
            assert!(kick(sr) == legacy::kick(sr), "kick");
            assert!(snare(sr) == legacy::snare(sr), "snare");
            assert!(hat(sr) == legacy::hat(sr), "hat");
            assert!(clap(sr) == legacy::clap(sr), "clap");
        }
    }

    type Gen = fn(f32) -> Vec<f32>;
    const ALL: [(&str, Gen); 4] = [
        ("kick", kick),
        ("snare", snare),
        ("hat", hat),
        ("clap", clap),
    ];

    /// Zero crossings per second: a rough brightness measure.
    fn crossing_rate(v: &[f32], sr: f32) -> f32 {
        let n = v
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        n as f32 / (v.len() as f32 / sr)
    }

    #[test]
    fn drums_are_normalised_start_loud_and_end_silent() {
        for sr in [44_100.0, 48_000.0, 96_000.0] {
            for (name, g) in ALL {
                let v = g(sr);
                let peak = v.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
                assert!((peak - DRUM_PEAK).abs() < 1e-3, "{name} {peak}");
                assert_eq!(*v.last().unwrap(), 0.0, "{name}");
                assert!(v.iter().all(|s| s.is_finite()), "{name}");
                // The transient is in the first 25 ms.
                let early = v[..(0.025 * sr) as usize]
                    .iter()
                    .fold(0.0_f32, |m, s| m.max(s.abs()));
                assert!(early > 0.5, "{name} {early}");
            }
        }
    }

    #[test]
    fn drums_are_deterministic() {
        for (name, g) in ALL {
            assert_eq!(g(48_000.0), g(48_000.0), "{name}");
        }
    }

    #[test]
    fn spectral_character_is_plausible() {
        let sr = 48_000.0;
        let k = crossing_rate(&kick(sr)[..4800], sr);
        let s = crossing_rate(&snare(sr)[..4800], sr);
        let h = crossing_rate(&hat(sr)[..4800], sr);
        assert!(k < 300.0, "kick {k}");
        assert!(h > 8000.0, "hat {h}");
        assert!(k < s && s < h, "{k} {s} {h}");
    }
}
