//! Flanger.

use super::{clamp_param, sync_hz, DelayLine, Effect, FxContext, Smooth};
use std::f32::consts::TAU;

mod p {
    pub const RATE: usize = 0;
    pub const SYNC: usize = 1;
    pub const DEPTH: usize = 2;
    pub const DELAY: usize = 3;
    pub const FEEDBACK: usize = 4;
    pub const SPREAD: usize = 5;
    pub const MIX: usize = 6;
    pub const COUNT: usize = 7;
}

const MAX_MS: f32 = 25.0;

/// LFO rate for a rate in Hz and a sync choice (0 = free, else a note value plus one).
pub(crate) fn lfo_hz(rate: f32, sync: usize, bpm: f32) -> f32 {
    if sync == 0 {
        rate
    } else {
        sync_hz(sync - 1, bpm)
    }
}

/// Flanger: a very short delay swept by a sine LFO (free or at a note value of the tempo) and
/// fed back into itself, mixed with the dry signal so the comb-filter notches sweep. Negative
/// feedback moves the peaks to where the notches were.
#[derive(Debug, Clone)]
pub struct Flanger {
    sr: f32,
    lines: [DelayLine; 2],
    wet: [f32; 2],
    phase: f32,
    rate: f32,
    sync: usize,
    depth: Smooth,
    base: Smooth,
    feedback: Smooth,
    spread: Smooth,
    mix: Smooth,
}

impl Flanger {
    /// A slow, gentle flanger.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let cap = (MAX_MS * 0.001 * sr) as usize + 8;
        let ms = 0.001 * sr;
        Self {
            sr,
            lines: [DelayLine::new(cap), DelayLine::new(cap)],
            wet: [0.0; 2],
            phase: 0.0,
            rate: 0.3,
            sync: 0,
            depth: Smooth::new(2.0 * ms, 20.0, sr),
            base: Smooth::new(1.0 * ms, 50.0, sr),
            feedback: Smooth::new(0.5, 20.0, sr),
            spread: Smooth::new(0.25, 20.0, sr),
            mix: Smooth::new(0.5, 20.0, sr),
        }
    }
}

impl Effect for Flanger {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        let ms = 0.001 * self.sr;
        match index {
            p::RATE => self.rate = clamp_param(v, 0.02, 10.0, 0.3),
            p::SYNC => self.sync = clamp_param(v, 0.0, 9.0, 0.0).round() as usize,
            p::DEPTH => self.depth.set(clamp_param(v, 0.0, 10.0, 2.0) * ms),
            p::DELAY => self.base.set(clamp_param(v, 0.1, 10.0, 1.0) * ms),
            p::FEEDBACK => self.feedback.set(clamp_param(v, -0.95, 0.95, 0.5)),
            p::SPREAD => self.spread.set(clamp_param(v, 0.0, 1.0, 0.25)),
            p::MIX => self.mix.set(clamp_param(v, 0.0, 1.0, 0.5)),
            _ => {}
        }
    }

    fn reset(&mut self) {
        for l in &mut self.lines {
            l.clear();
        }
        self.wet = [0.0; 2];
        self.phase = 0.0;
        for s in [
            &mut self.depth,
            &mut self.base,
            &mut self.feedback,
            &mut self.spread,
            &mut self.mix,
        ] {
            s.snap();
        }
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &FxContext) {
        let inc = lfo_hz(self.rate, self.sync, ctx.bpm) / self.sr;
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let depth = self.depth.next_value();
            let base = self.base.next_value();
            let fb = self.feedback.next_value();
            let offset = self.spread.next_value() * 0.5;
            let mix = self.mix.next_value();
            for (ch, s) in [x, y].into_iter().enumerate() {
                let ph = self.phase - offset * ch as f32;
                let sweep = 0.5 + 0.5 * (TAU * ph).sin();
                let line = &mut self.lines[ch];
                line.push(*s + fb * self.wet[ch]);
                let w = line.read((base + depth * sweep).max(1.0));
                self.wet[ch] = w;
                *s += (w - *s) * mix;
            }
            self.phase += inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{db, rms, run, sine};

    #[test]
    fn the_comb_notch_sweeps_with_the_tempo() {
        let sr = 48_000.0;
        let mut f = Flanger::new(sr);
        // 1 ms fixed delay, no feedback: a notch at 500 Hz.
        f.set_param(p::DEPTH, 0.0);
        f.set_param(p::DELAY, 1.0);
        f.set_param(p::FEEDBACK, 0.0);
        f.reset();
        let level = |f: &mut Flanger, hz: f32| {
            let x = sine(hz, 0.25, sr, 9600);
            let (mut l, mut r) = (x.clone(), x.clone());
            run(f, &mut l, &mut r, 120.0);
            db(rms(&l[4800..]) / rms(&x[4800..]))
        };
        assert!(level(&mut f, 500.0) < -30.0);
        assert!(level(&mut f, 1000.0).abs() < 0.2);
        // Synced to 1 bar at 120 BPM the sweep repeats every 2 s.
        assert!((lfo_hz(0.3, 7, 120.0) - 0.5).abs() < 1e-6);
        assert_eq!(lfo_hz(0.3, 0, 120.0), 0.3);
    }
}
