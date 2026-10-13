//! Multi-mode filter: a zero-delay-feedback state-variable filter with drive, an envelope
//! follower and a tempo-synced LFO on the cutoff.

use super::{clamp_param, one_pole_coef, sync_hz, Effect, FxContext, Smooth, CONTROL_FRAMES};
use std::f32::consts::{PI, TAU};

mod p {
    pub const MODE: usize = 0;
    pub const SLOPE: usize = 1;
    pub const CUTOFF: usize = 2;
    pub const RESO: usize = 3;
    pub const DRIVE: usize = 4;
    pub const ENV: usize = 5;
    pub const ATTACK: usize = 6;
    pub const RELEASE: usize = 7;
    pub const LFO_RATE: usize = 8;
    pub const LFO_DEPTH: usize = 9;
    pub const OUTPUT: usize = 10;
    pub const COUNT: usize = 11;
}

/// How far the envelope and the LFO move the cutoff at full amount, in octaves.
const MOD_OCTAVES: f32 = 4.0;

/// Filter response, indexed by the `mode` parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    /// Low-pass.
    LowPass,
    /// High-pass.
    HighPass,
    /// Band-pass, unity gain at the centre.
    BandPass,
    /// Notch (band-reject).
    Notch,
}

impl FilterMode {
    /// The mode for a parameter value (0 to 3).
    pub fn from_value(v: f32) -> Self {
        match v.round() as i32 {
            1 => Self::HighPass,
            2 => Self::BandPass,
            3 => Self::Notch,
            _ => Self::LowPass,
        }
    }
}

/// Coefficients of one TPT state-variable stage (Zavalishin's "The Art of VA Filter Design",
/// in Simper's form): the integrators are solved together with the feedback, so the cutoff can
/// move every few samples without the filter blowing up.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SvfCoeffs {
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
}

impl SvfCoeffs {
    pub(crate) fn new(cutoff: f32, q: f32, sr: f32) -> Self {
        let g = (PI * (cutoff / sr).clamp(1e-5, 0.49)).tan();
        let k = 1.0 / q.max(0.05);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        Self {
            k,
            a1,
            a2,
            a3: g * a2,
        }
    }
}

/// One stage's two integrator states.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Svf {
    ic1: f32,
    ic2: f32,
}

impl Svf {
    #[inline]
    pub(crate) fn tick(&mut self, c: &SvfCoeffs, x: f32, mode: FilterMode) -> f32 {
        let v3 = x - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        match mode {
            FilterMode::LowPass => v2,
            FilterMode::HighPass => x - c.k * v1 - v2,
            FilterMode::BandPass => c.k * v1,
            FilterMode::Notch => x - c.k * v1,
        }
    }
}

/// The multi-mode filter effect. 12 dB/oct is one stage; 24 dB/oct runs two in series at the
/// same cutoff and Q. Drive blends in a tanh saturator before the filter (none at 0 dB). The
/// envelope follows the input level and the LFO runs at a note value of the song tempo; both
/// move the cutoff in octaves, updated every [`CONTROL_FRAMES`].
#[derive(Debug, Clone)]
pub struct MultiFilter {
    sr: f32,
    mode: FilterMode,
    steep: bool,
    cutoff: Smooth,
    reso: Smooth,
    drive: Smooth,
    env_amount: Smooth,
    lfo_depth: Smooth,
    output: Smooth,
    attack: f32,
    release: f32,
    lfo_div: usize,
    env: f32,
    phase: f32,
    stages: [[Svf; 2]; 2],
    coeffs: SvfCoeffs,
    countdown: usize,
}

impl MultiFilter {
    /// An open low-pass.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let mut f = Self {
            sr,
            mode: FilterMode::LowPass,
            steep: false,
            cutoff: Smooth::new(2000.0, 20.0, sr),
            reso: Smooth::new(0.0, 20.0, sr),
            drive: Smooth::new(0.0, 20.0, sr),
            env_amount: Smooth::new(0.0, 20.0, sr),
            lfo_depth: Smooth::new(0.0, 20.0, sr),
            output: Smooth::new(1.0, 20.0, sr),
            attack: one_pole_coef(1000.0 / (TAU * 5.0), sr),
            release: one_pole_coef(1000.0 / (TAU * 200.0), sr),
            lfo_div: 4,
            env: 0.0,
            phase: 0.0,
            stages: [[Svf::default(); 2]; 2],
            coeffs: SvfCoeffs::new(2000.0, 0.707, sr),
            countdown: 0,
        };
        f.reset();
        f
    }

    fn update(&mut self, bpm: f32) {
        let n = CONTROL_FRAMES;
        let cutoff = self.cutoff.advance(n);
        let q = std::f32::consts::FRAC_1_SQRT_2 * 2f32.powf(self.reso.advance(n) * 5.0);
        let lfo = (TAU * self.phase).sin();
        self.phase = (self.phase + sync_hz(self.lfo_div, bpm) * n as f32 / self.sr).fract();
        // The envelope opens over the top 60 dB of level, so quiet and loud parts both move it.
        let env = ((crate::gain_to_db(self.env.max(1e-6)) + 60.0) / 60.0).clamp(0.0, 1.0);
        let octaves =
            (self.env_amount.advance(n) * env + self.lfo_depth.advance(n) * lfo) * MOD_OCTAVES;
        let f = (cutoff * octaves.exp2()).clamp(20.0, 0.45 * self.sr);
        self.coeffs = SvfCoeffs::new(f, q, self.sr);
    }
}

impl Effect for MultiFilter {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        let ms = |v: f32| one_pole_coef(1000.0 / (TAU * v), self.sr);
        match index {
            p::MODE => self.mode = FilterMode::from_value(clamp_param(v, 0.0, 3.0, 0.0)),
            p::SLOPE => self.steep = clamp_param(v, 0.0, 1.0, 0.0) >= 0.5,
            p::CUTOFF => self.cutoff.set(clamp_param(v, 20.0, 20_000.0, 2000.0)),
            p::RESO => self.reso.set(clamp_param(v, 0.0, 1.0, 0.0)),
            p::DRIVE => self.drive.set(clamp_param(v, 0.0, 24.0, 0.0)),
            p::ENV => self.env_amount.set(clamp_param(v, -1.0, 1.0, 0.0)),
            p::ATTACK => self.attack = ms(clamp_param(v, 0.1, 200.0, 5.0)),
            p::RELEASE => self.release = ms(clamp_param(v, 5.0, 2000.0, 200.0)),
            p::LFO_RATE => self.lfo_div = clamp_param(v, 0.0, 8.0, 4.0).round() as usize,
            p::LFO_DEPTH => self.lfo_depth.set(clamp_param(v, 0.0, 1.0, 0.0)),
            p::OUTPUT => self
                .output
                .set(crate::db_to_gain(clamp_param(v, -24.0, 12.0, 0.0))),
            _ => {}
        }
    }

    fn reset(&mut self) {
        for s in [
            &mut self.cutoff,
            &mut self.reso,
            &mut self.drive,
            &mut self.env_amount,
            &mut self.lfo_depth,
            &mut self.output,
        ] {
            s.snap();
        }
        self.stages = [[Svf::default(); 2]; 2];
        self.env = 0.0;
        self.phase = 0.0;
        self.countdown = 0;
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &FxContext) {
        let (mode, steep) = (self.mode, self.steep);
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            if self.countdown == 0 {
                self.update(ctx.bpm);
                self.countdown = CONTROL_FRAMES;
            }
            self.countdown -= 1;
            let level = x.abs().max(y.abs());
            let c = if level > self.env {
                self.attack
            } else {
                self.release
            };
            self.env += (level - self.env) * c;
            let drive_db = self.drive.next_value();
            let out = self.output.next_value();
            let co = self.coeffs;
            for (ch, s) in [x, y].into_iter().enumerate() {
                let mut v = *s;
                if drive_db > 0.0 {
                    let g = crate::db_to_gain(drive_db);
                    v += (((g * v).tanh() / g.sqrt()) - v) * (drive_db / 24.0).min(1.0);
                }
                v = self.stages[ch][0].tick(&co, v, mode);
                if steep {
                    v = self.stages[ch][1].tick(&co, v, mode);
                }
                *s = v * out;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{db, rms, run, sine};

    const SR: f32 = 48_000.0;

    /// Gain in dB of a steady sine at `freq` through a filter set up by `setup`.
    fn response(setup: &[(usize, f32)], freq: f32) -> f32 {
        let mut f = MultiFilter::new(SR);
        for &(i, v) in setup {
            f.set_param(i, v);
        }
        f.reset();
        let x = sine(freq, 0.25, SR, 24_000);
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut f, &mut l, &mut r, 120.0);
        db(rms(&l[12_000..]) / rms(&x[12_000..]))
    }

    fn near(got: f32, want: f32, tol: f32) {
        assert!(
            (got - want).abs() <= tol,
            "got {got:.2} dB, want {want} ± {tol}"
        );
    }

    #[test]
    fn every_mode_has_its_response() {
        let fc = 1000.0;
        for steep in [0.0, 1.0] {
            let at = |mode: f32, f: f32| {
                response(&[(p::MODE, mode), (p::SLOPE, steep), (p::CUTOFF, fc)], f)
            };
            let per_stage = |db: f32| if steep > 0.0 { 2.0 * db } else { db };
            // A TPT stage at Q 0.707 is a Butterworth low-pass at warped frequencies:
            // |H|² = 1 / (1 + (w/wc)⁴) with w = tan(πf/sr). High-pass swaps w and wc.
            let w = |f: f32| (PI * f / SR).tan();
            let butter = |a: f32, b: f32| per_stage(-10.0 * (1.0 + (a / b).powi(4)).log10());
            // (Deep in the stopband the measurement has a floor around -70 dB.)
            let check = |got: f32, want: f32| match want {
                w if w > -40.0 => near(got, w, 0.15),
                w if w > -65.0 => near(got, w, 2.0),
                _ => assert!(got < -60.0, "{got}"),
            };
            for f in [50.0, 500.0, fc, 2.0 * fc, 8.0 * fc, 20.0 * fc] {
                check(at(0.0, f), butter(w(f), w(fc)));
                check(at(1.0, f), butter(w(fc), w(f)));
            }
            // That is -3 dB per stage at the cutoff and 12 dB/oct per stage above it.
            near(at(0.0, fc), per_stage(-3.01), 0.2);
            // Band-pass: unity at the centre, falling on both sides.
            near(at(2.0, fc), 0.0, 0.1);
            assert!(at(2.0, fc / 10.0) < per_stage(-16.0));
            assert!(at(2.0, fc * 10.0) < per_stage(-16.0));
            // Notch: deep at the centre, flat away from it.
            assert!(at(3.0, fc) < -40.0, "{}", at(3.0, fc));
            near(at(3.0, fc / 20.0), 0.0, 0.1);
            near(at(3.0, fc * 15.0), 0.0, 0.2);
        }
        // Resonance lifts the cutoff.
        let peak = response(&[(p::CUTOFF, 1000.0), (p::RESO, 0.6)], 1000.0);
        assert!(peak > 9.0, "{peak}");
    }

    #[test]
    fn the_envelope_and_the_lfo_move_the_cutoff() {
        // A closed low-pass passes a high tone only when the envelope opens it.
        let tone = |env: f32| {
            response(
                &[(p::CUTOFF, 200.0), (p::ENV, env), (p::ATTACK, 1.0)],
                3000.0,
            )
        };
        assert!(tone(1.0) > tone(0.0) + 20.0);
        // The LFO swings the level of a tone near the cutoff once per beat at 1/4.
        let mut f = MultiFilter::new(SR);
        f.set_param(p::CUTOFF, 1000.0);
        f.set_param(p::LFO_RATE, 4.0);
        f.set_param(p::LFO_DEPTH, 0.5);
        f.reset();
        let x = sine(2000.0, 0.25, SR, 48_000);
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut f, &mut l, &mut r, 120.0);
        // At 120 BPM a beat is 0.5 s: loud at a quarter beat (LFO up), quiet at three quarters.
        let q = |beat: f32| {
            let at = (beat * 24_000.0) as usize;
            rms(&l[at - 600..at + 600])
        };
        assert!(db(q(0.25) / q(0.75)) > 20.0, "{} {}", q(0.25), q(0.75));
    }

    #[test]
    fn drive_saturates_only_when_turned_up() {
        assert!((response(&[(p::CUTOFF, 20_000.0)], 100.0)).abs() < 0.1);
        let driven = response(&[(p::CUTOFF, 20_000.0), (p::DRIVE, 24.0)], 100.0);
        assert!(driven.abs() > 1.0, "{driven}");
    }
}
