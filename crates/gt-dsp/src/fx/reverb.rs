//! Feedback-delay-network reverb.

use super::{clamp_param, one_pole_coef, DelayLine, Effect, FxContext, Smooth};

const LINES: usize = 8;
/// Line lengths in milliseconds at size 100 %. Mutually prime-ish lengths spread the echoes so
/// no two lines line up into a metallic repeat.
const LINE_MS: [f32; LINES] = [31.7, 37.3, 41.9, 47.3, 53.1, 59.9, 67.1, 73.7];
/// Input diffusers: short allpasses that smear each input click into a burst before it enters
/// the network, so early reflections sound dense instead of like separate echoes.
const DIFFUSER_MS: [f32; 4] = [4.77, 3.59, 12.7, 9.3];
const DIFFUSION: f32 = 0.6;
const MAX_PREDELAY_MS: f32 = 250.0;
/// Size maps to a length scale of 0.3..1.3.
fn size_scale(size: f32) -> f32 {
    0.3 + size
}

mod p {
    pub const SIZE: usize = 0;
    pub const DECAY: usize = 1;
    pub const DAMPING: usize = 2;
    pub const PREDELAY: usize = 3;
    pub const WIDTH: usize = 4;
    pub const MIX: usize = 5;
    pub const COUNT: usize = 6;
}

#[derive(Debug, Clone)]
struct Allpass {
    line: DelayLine,
    len: usize,
}

impl Allpass {
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        // Schroeder allpass: flat magnitude, smeared phase.
        let delayed = self.line.read_int(self.len - 1);
        let v = x + DIFFUSION * delayed;
        self.line.push(v);
        delayed - DIFFUSION * v
    }
}

/// An 8-line feedback delay network. The lines feed back through a Hadamard matrix, which is
/// orthogonal (it keeps energy, only mixes it), so the decay is set purely by a gain per line:
/// `g = 10^(−3·len / (RT60·fs))` makes every line lose 60 dB in the decay time whatever its
/// length. A one-pole low-pass in each loop ("damping") makes highs die sooner, as in a real
/// room. Left takes the even lines, right the odd ones; width blends them towards mono.
#[derive(Debug, Clone)]
pub struct Reverb {
    sr: f32,
    lines: [DelayLine; LINES],
    damp_state: [f32; LINES],
    diffusers: [Allpass; 4],
    predelay_line: DelayLine,
    predelay: Smooth,
    /// Size scale for the line lengths.
    scale: Smooth,
    decay_s: f32,
    damping_hz: f32,
    damp_coef: f32,
    width: Smooth,
    mix: Smooth,
    gains: [f32; LINES],
    gains_scale: f32,
}

impl Reverb {
    /// A medium room.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let max_scale = size_scale(1.0);
        let lines = LINE_MS.map(|ms| DelayLine::new((ms * 0.001 * sr * max_scale) as usize + 8));
        let diffusers = DIFFUSER_MS.map(|ms| {
            let len = ((ms * 0.001 * sr) as usize).max(2);
            Allpass {
                line: DelayLine::new(len + 4),
                len,
            }
        });
        let mut rv = Self {
            sr,
            lines,
            damp_state: [0.0; LINES],
            diffusers,
            predelay_line: DelayLine::new((MAX_PREDELAY_MS * 0.001 * sr) as usize + 8),
            predelay: Smooth::new(20.0 * 0.001 * sr, 50.0, sr),
            scale: Smooth::new(size_scale(0.6), 200.0, sr),
            decay_s: 2.5,
            damping_hz: 6000.0,
            damp_coef: 0.0,
            width: Smooth::new(1.0, 20.0, sr),
            mix: Smooth::new(0.25, 20.0, sr),
            gains: [0.0; LINES],
            gains_scale: 0.0,
        };
        rv.damp_coef = one_pole_coef(rv.damping_hz, sr);
        rv.update_gains();
        rv
    }

    fn update_gains(&mut self) {
        let scale = self.scale.value();
        for (g, ms) in self.gains.iter_mut().zip(LINE_MS) {
            let len_s = ms * 0.001 * scale;
            *g = 10.0_f32.powf(-3.0 * len_s / self.decay_s);
        }
        self.gains_scale = scale;
    }

    /// In-place 8-point fast Walsh–Hadamard transform, scaled to stay orthogonal.
    #[inline]
    fn hadamard(x: &mut [f32; LINES]) {
        let mut h = 1;
        while h < LINES {
            for i in (0..LINES).step_by(h * 2) {
                for j in i..i + h {
                    let (a, b) = (x[j], x[j + h]);
                    x[j] = a + b;
                    x[j + h] = a - b;
                }
            }
            h *= 2;
        }
        let k = 1.0 / (LINES as f32).sqrt();
        for v in x.iter_mut() {
            *v *= k;
        }
    }
}

impl Effect for Reverb {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            p::SIZE => self.scale.set(size_scale(clamp_param(v, 0.0, 1.0, 0.6))),
            p::DECAY => {
                self.decay_s = clamp_param(v, 0.1, 30.0, 2.5);
                self.update_gains();
            }
            p::DAMPING => {
                self.damping_hz = clamp_param(v, 500.0, 20_000.0, 6000.0);
                self.damp_coef = one_pole_coef(self.damping_hz, self.sr);
            }
            p::PREDELAY => self
                .predelay
                .set(clamp_param(v, 0.0, MAX_PREDELAY_MS, 20.0) * 0.001 * self.sr),
            p::WIDTH => self.width.set(clamp_param(v, 0.0, 1.0, 1.0)),
            p::MIX => self.mix.set(clamp_param(v, 0.0, 1.0, 0.25)),
            _ => {}
        }
    }

    fn reset(&mut self) {
        for l in &mut self.lines {
            l.clear();
        }
        for a in &mut self.diffusers {
            a.line.clear();
        }
        self.predelay_line.clear();
        self.damp_state = [0.0; LINES];
        self.predelay.snap();
        self.scale.snap();
        self.width.snap();
        self.mix.snap();
        self.update_gains();
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &FxContext) {
        // Line lengths glide with size; refresh the decay gains once per block when they moved.
        let scale = self.scale.advance(l.len());
        if (scale - self.gains_scale).abs() > 1e-4 {
            self.update_gains();
        }
        let ms_to_frames = 0.001 * self.sr * scale;
        let lens = LINE_MS.map(|ms| ms * ms_to_frames);
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let input = 0.5 * (*x + *y);
            self.predelay_line.push(input);
            let mut d = self.predelay_line.read(self.predelay.next_value());
            for a in &mut self.diffusers {
                d = a.process(d);
            }
            let mut v = [0.0; LINES];
            for (i, line) in self.lines.iter().enumerate() {
                v[i] = line.read(lens[i] - 1.0);
            }
            let (mut wl, mut wr) = (0.0, 0.0);
            for i in (0..LINES).step_by(2) {
                wl += v[i];
                wr += v[i + 1];
            }
            for (i, s) in v.iter_mut().enumerate() {
                self.damp_state[i] += (*s - self.damp_state[i]) * self.damp_coef;
                *s = self.damp_state[i] * self.gains[i];
            }
            Self::hadamard(&mut v);
            for (i, line) in self.lines.iter_mut().enumerate() {
                line.push(v[i] + d);
            }
            // Mid/side width: 0 is mono, 1 keeps the full spread.
            let w = self.width.next_value();
            let mid = 0.5 * (wl + wr);
            let side = 0.5 * (wl - wr) * w;
            let gain = 0.35;
            let mix = self.mix.next_value();
            *x += ((mid + side) * gain - *x) * mix;
            *y += ((mid - side) * gain - *y) * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{rms, run};

    fn energy_db(x: &[f32]) -> f32 {
        20.0 * rms(x).max(1e-12).log10()
    }

    #[test]
    fn tail_decays_at_the_set_time() {
        let sr = 48_000.0;
        let mut rv = Reverb::new(sr);
        rv.set_param(p::DECAY, 1.0);
        rv.set_param(p::DAMPING, 20_000.0);
        rv.set_param(p::MIX, 1.0);
        rv.set_param(p::PREDELAY, 0.0);
        rv.reset();
        let n = (2.0 * sr) as usize;
        let mut l = vec![0.0; n];
        let mut r = vec![0.0; n];
        for v in l.iter_mut().take(480) {
            *v = 0.5;
        }
        r.copy_from_slice(&l);
        run(&mut rv, &mut l, &mut r, 120.0);
        // Compare 100 ms windows half a second apart: RT60 1 s means 30 dB per 0.5 s.
        let w = 4800;
        let a = energy_db(&l[12_000..12_000 + w]);
        let b = energy_db(&l[36_000..36_000 + w]);
        assert!((a - b - 30.0).abs() < 4.0, "{a} {b}");
        // Stereo: the two sides differ.
        let diff: f32 = l.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 1.0);
    }

    #[test]
    fn width_zero_is_mono_and_long_decay_stays_bounded() {
        let sr = 48_000.0;
        let mut rv = Reverb::new(sr);
        rv.set_param(p::WIDTH, 0.0);
        rv.set_param(p::DECAY, 30.0);
        rv.set_param(p::MIX, 1.0);
        rv.reset();
        let mut l: Vec<f32> = (0..96_000)
            .map(|i| ((i * 7919) % 13) as f32 / 13.0 - 0.5)
            .collect();
        let mut r = l.clone();
        run(&mut rv, &mut l, &mut r, 120.0);
        assert!(l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-6));
        assert!(l.iter().all(|v| v.abs() < 10.0));
    }
}
