//! The FM engine of Gloom Synth (Prompt 24): six sine operators in phase modulation.
//!
//! Operator `i` outputs `a·sin(2π(φ + Σⱼ mᵢⱼ·yⱼ·DEPTH))`: its own phase plus the outputs of
//! the operators that modulate it, so a modulator at level `a` and amount 1 gives a modulation
//! index of `β = 2π·DEPTH·a` radians. Operators run from 6 down to 1 each sample, so a higher
//! operator modulating a lower one acts within the sample; any other route (a lower one
//! modulating a higher one, or an operator itself) uses the previous sample, the feedback
//! averaged over the last two samples to keep it from buzzing.
//!
//! Eight fixed algorithms and a free 6×6 routing matrix with carrier levels. Operator 6 feeds
//! back into itself by the feedback amount in every algorithm. Each operator has its own
//! envelope, level, ratio or fixed frequency, key scaling of level (dB per octave from C4) and
//! of envelope rate, and velocity sensitivity.
//!
//! Where a note's spectrum would reach past Nyquist (Carson's rule on its operators'
//! frequencies and modulation indices, checked when the note starts), its operators run at
//! twice the sample rate and a half-band filter brings them back down, which keeps the
//! aliasing of bright, high-index sounds far below the tone; other notes run at the sample
//! rate.

use std::sync::OnceLock;

use crate::adsr::{Adsr, AdsrParams};
use crate::fx::Downsampler2x;

use super::common::EnvSettings;

/// Operators per voice.
pub const OPS: usize = 6;
/// Number of algorithms, the last being the free matrix.
pub const ALGORITHMS: usize = 9;
/// The free routing algorithm.
pub const FREE: u8 = 8;
/// Phase deviation in cycles of a modulator at full level and amount 1.
pub const DEPTH: f32 = 2.0;
/// Sine table resolution.
const SINE_LEN: usize = 4096;

/// One operator's settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpSettings {
    /// Frequency ratio to the note.
    pub ratio: f32,
    /// Play at `fixed_hz` whatever the note.
    pub fixed: bool,
    /// Fixed frequency in Hz.
    pub fixed_hz: f32,
    /// Fine tune in cents.
    pub fine_cents: f32,
    /// Output level, 0 to 1 (a modulator's sets its index, a carrier's its loudness).
    pub level: f32,
    /// Envelope.
    pub env: EnvSettings,
    /// Level change per octave away from C4, in dB (negative: quieter going up).
    pub key_scale_db: f32,
    /// Envelope speed-up going up the keyboard: 1 halves the times per octave.
    pub rate_scale: f32,
    /// How much velocity changes the level, 0 to 1.
    pub velocity: f32,
}

impl Default for OpSettings {
    fn default() -> Self {
        Self {
            ratio: 1.0,
            fixed: false,
            fixed_hz: 440.0,
            fine_cents: 0.0,
            level: 0.0,
            env: EnvSettings {
                attack_ms: 1.0,
                decay_ms: 800.0,
                sustain: 0.6,
                release_ms: 300.0,
            },
            key_scale_db: 0.0,
            rate_scale: 0.0,
            velocity: 0.5,
        }
    }
}

/// Every FM setting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FmSettings {
    /// The operators, 1 to 6.
    pub ops: [OpSettings; OPS],
    /// Algorithm, 0 to [`FREE`].
    pub algorithm: u8,
    /// Operator 6 feeding back into itself, 0 to 1.
    pub feedback: f32,
    /// Free routing: `matrix[i][j]` is how much operator `j` modulates operator `i`.
    pub matrix: [[f32; OPS]; OPS],
    /// Free routing: each operator's level in the output.
    pub out: [f32; OPS],
}

impl Default for FmSettings {
    fn default() -> Self {
        let mut ops = [OpSettings::default(); OPS];
        ops[0].level = 1.0;
        ops[1].level = 0.25;
        let mut out = [0.0; OPS];
        out[0] = 1.0;
        Self {
            ops,
            algorithm: 0,
            feedback: 0.0,
            matrix: [[0.0; OPS]; OPS],
            out,
        }
    }
}

/// One fixed algorithm: (modulated, modulator) pairs (operators 1 to 6) and the carriers.
type Algorithm = (&'static [(usize, usize)], &'static [usize]);

/// The fixed algorithms.
const ALGOS: [Algorithm; FREE as usize] = [
    (&[(1, 2), (2, 3), (3, 4), (4, 5), (5, 6)], &[1]),
    (&[(1, 2), (2, 3), (4, 5), (5, 6)], &[1, 4]),
    (&[(1, 2), (3, 4), (5, 6)], &[1, 3, 5]),
    (&[(1, 2), (1, 3), (1, 4), (5, 6)], &[1, 5]),
    (&[(2, 3), (3, 4), (4, 5), (5, 6)], &[1, 2]),
    (&[(1, 6), (2, 6), (3, 6), (4, 6), (5, 6)], &[1, 2, 3, 4, 5]),
    (&[], &[1, 2, 3, 4, 5, 6]),
    (&[(1, 4), (2, 4), (3, 4), (4, 5), (5, 6)], &[1, 2, 3]),
];

/// Who modulates whom, resolved from the algorithm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Routing {
    /// `m[i][j]`: operator `j` into operator `i`.
    pub m: [[f32; OPS]; OPS],
    /// Each operator's gain into the output.
    pub carrier: [f32; OPS],
}

impl FmSettings {
    /// The routing of the current algorithm, with modulation and feedback scaled by
    /// `index` (1: as set) and `feedback` added (from the matrix).
    pub fn routing(&self, index: f32, feedback: f32) -> Routing {
        let mut r = Routing {
            m: [[0.0; OPS]; OPS],
            carrier: [0.0; OPS],
        };
        if self.algorithm >= FREE {
            r.m = self.matrix;
            r.carrier = self.out;
        } else {
            let (edges, carriers) = ALGOS[usize::from(self.algorithm)];
            for &(dst, src) in edges {
                r.m[dst - 1][src - 1] = 1.0;
            }
            let g = 1.0 / (carriers.len() as f32).sqrt();
            for &c in carriers {
                r.carrier[c - 1] = g;
            }
        }
        let index = index.max(0.0);
        for row in &mut r.m {
            for v in row.iter_mut() {
                *v *= index;
            }
        }
        r.m[OPS - 1][OPS - 1] += (self.feedback + feedback).clamp(0.0, 1.0);
        r
    }
}

/// Whether operator `op` (0-based) is a carrier in algorithm `algorithm`, for the panel.
pub fn is_carrier(algorithm: u8, op: usize) -> bool {
    match ALGOS.get(usize::from(algorithm)) {
        Some((_, carriers)) => carriers.contains(&(op + 1)),
        None => true,
    }
}

/// The modulators of operator `op` (0-based) in a fixed algorithm, for the panel.
pub fn modulators(algorithm: u8, op: usize) -> impl Iterator<Item = usize> {
    ALGOS
        .get(usize::from(algorithm))
        .map_or(&[][..], |a| a.0)
        .iter()
        .filter(move |e| e.0 == op + 1)
        .map(|e| e.1 - 1)
}

fn sine_table() -> &'static [f32; SINE_LEN + 1] {
    static T: OnceLock<Box<[f32; SINE_LEN + 1]>> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = Box::new([0.0; SINE_LEN + 1]);
        for (i, v) in t.iter_mut().enumerate() {
            *v = (std::f64::consts::TAU * i as f64 / SINE_LEN as f64).sin() as f32;
        }
        t
    })
}

/// Builds the shared tables. Call off the audio thread before the first note.
pub fn warm_up() {
    sine_table();
}

/// sin(2π·t) for |t| below 2²².
#[inline]
fn sine(table: &[f32; SINE_LEN + 1], t: f32) -> f32 {
    // Round to the nearest integer with float adds only: d is in -0.5..=0.5.
    const MAGIC: f32 = 12_582_912.0;
    let d = t - ((t + MAGIC) - MAGIC);
    // sin(2πd) = -sin(2π(d + 0.5)).
    let x = (d + 0.5) * SINE_LEN as f32;
    let i = (x as usize).min(SINE_LEN - 1);
    let f = x - i as f32;
    -(table[i] + (table[i + 1] - table[i]) * f)
}

/// The FM part of a voice.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FmVoice {
    phase: [f32; OPS],
    /// Phase increment per oversampled frame.
    inc: [f32; OPS],
    /// Level from settings, key scaling and velocity (before the envelope).
    gain: [f32; OPS],
    env: [Adsr; OPS],
    y: [f32; OPS],
    y_prev: [f32; OPS],
    routing: Routing,
    /// Per operator, the operators modulating it (from `routing`), and how many.
    srcs: [[u8; OPS]; OPS],
    n_srcs: [u8; OPS],
    /// Runs at twice the sample rate (decided at the note's first control update).
    os: bool,
    decided: bool,
    down: Downsampler2x,
}

impl Default for FmVoice {
    fn default() -> Self {
        Self {
            phase: [0.0; OPS],
            inc: [0.0; OPS],
            gain: [0.0; OPS],
            env: [Adsr::default(); OPS],
            y: [0.0; OPS],
            y_prev: [0.0; OPS],
            routing: FmSettings::default().routing(1.0, 0.0),
            srcs: [[0; OPS]; OPS],
            n_srcs: [0; OPS],
            os: false,
            decided: false,
            down: Downsampler2x::default(),
        }
    }
}

/// An operator's envelope for `key`, its rate scaled.
pub(crate) fn op_env(op: &OpSettings, key: u8, sr: f32) -> AdsrParams {
    let speed = (op.rate_scale.clamp(0.0, 1.0) * (f32::from(key) - 60.0) / 12.0).exp2();
    let e = &op.env;
    AdsrParams::new(
        sr,
        e.attack_ms / speed,
        e.decay_ms / speed,
        e.sustain,
        e.release_ms / speed,
    )
}

impl FmVoice {
    /// A new note: envelopes from the start (or from their level when `stolen`) and the
    /// operator levels for `key` and `velocity`.
    pub fn start(&mut self, fm: &FmSettings, key: u8, velocity: f32, sr: f32, stolen: bool) {
        for (i, op) in fm.ops.iter().enumerate() {
            let p = op_env(op, key, sr);
            if stolen {
                self.env[i].retrigger(p);
            } else {
                self.env[i].trigger(p);
                self.phase[i] = 0.0;
                self.y[i] = 0.0;
                self.y_prev[i] = 0.0;
            }
        }
        if !stolen {
            self.down.reset();
            self.decided = false;
        }
        self.levels(fm, key, velocity);
    }

    /// Settings changed: envelope coefficients and levels follow.
    pub fn update(&mut self, fm: &FmSettings, key: u8, velocity: f32, sr: f32) {
        for (i, op) in fm.ops.iter().enumerate() {
            self.env[i].set_params(op_env(op, key, sr));
        }
        self.levels(fm, key, velocity);
    }

    fn levels(&mut self, fm: &FmSettings, key: u8, velocity: f32) {
        let oct = (f32::from(key) - 60.0) / 12.0;
        for (g, op) in self.gain.iter_mut().zip(&fm.ops) {
            let ks = (op.key_scale_db * oct / 20.0 * core::f32::consts::LOG2_10).exp2();
            let vs = op.velocity.clamp(0.0, 1.0);
            let vel = 1.0 - vs + vs * velocity;
            *g = op.level.clamp(0.0, 1.0) * ks.min(4.0) * vel;
        }
    }

    pub fn release(&mut self, scale: f32) {
        for e in &mut self.env {
            e.release_scaled(scale);
        }
    }

    pub fn reset(&mut self) {
        for e in &mut self.env {
            e.reset();
        }
    }

    /// Control-rate update: operator frequencies from the note's frequency `note_hz` and the
    /// routing with the matrix's index and feedback.
    pub fn control(&mut self, fm: &FmSettings, note_hz: f32, sr: f32, index: f32, feedback: f32) {
        let mut hz = [0.0_f32; OPS];
        for (h, op) in hz.iter_mut().zip(&fm.ops) {
            *h = if op.fixed {
                op.fixed_hz
            } else {
                note_hz * op.ratio
            } * (op.fine_cents / 1200.0).exp2();
        }
        self.routing = fm.routing(1.0 + index, feedback);
        if !self.decided {
            self.decided = true;
            // Carson's rule: each modulator j widens its targets by (βⱼ + 1)·fⱼ.
            let mut widest = 0.0_f32;
            for i in 0..OPS {
                let mut bw = hz[i];
                for (j, &m) in self.routing.m[i].iter().enumerate() {
                    let beta = core::f32::consts::TAU * DEPTH * m * self.gain[j];
                    if beta > 0.0 {
                        bw += (beta + 1.0) * hz[j];
                    }
                }
                widest = widest.max(bw);
            }
            self.os = widest > 0.4 * sr;
        }
        let per = if self.os { 0.5 / sr } else { 1.0 / sr };
        for (inc, h) in self.inc.iter_mut().zip(hz) {
            *inc = (h * per).min(0.5);
        }
        for i in 0..OPS {
            let mut n = 0;
            for (j, &m) in self.routing.m[i].iter().enumerate() {
                if m != 0.0 {
                    self.srcs[i][n] = j as u8;
                    n += 1;
                }
            }
            self.n_srcs[i] = n as u8;
        }
    }

    /// One output-rate sample (from two oversampled ones when the note needs it).
    #[inline]
    pub fn tick(&mut self) -> f32 {
        let table = sine_table();
        let mut a = [0.0_f32; OPS];
        for (i, e) in self.env.iter_mut().enumerate() {
            a[i] = self.gain[i] * e.next_level();
        }
        let r = &self.routing;
        let mut out = [0.0_f32; 2];
        let n = if self.os { 2 } else { 1 };
        for o in &mut out[..n] {
            for i in (0..OPS).rev() {
                let mut pm = 0.0;
                for &j in &self.srcs[i][..usize::from(self.n_srcs[i])] {
                    let j = usize::from(j);
                    // Higher operators already ran this sample; lower ones hold the previous
                    // sample's output.
                    let y = if j == i {
                        0.5 * (self.y[j] + self.y_prev[j])
                    } else {
                        self.y[j]
                    };
                    pm += r.m[i][j] * y;
                }
                let ph = self.phase[i];
                let y = a[i] * sine(table, ph + pm * DEPTH);
                self.y_prev[i] = self.y[i];
                self.y[i] = y;
                let next = ph + self.inc[i];
                self.phase[i] = if next >= 1.0 { next - 1.0 } else { next };
            }
            *o = self
                .y
                .iter()
                .zip(&r.carrier)
                .map(|(y, c)| y * c)
                .sum::<f32>();
        }
        if self.os {
            self.down.process(out)
        } else {
            out[0]
        }
    }

    /// True while any carrier's envelope is still sounding.
    pub fn is_sounding(&self) -> bool {
        self.env
            .iter()
            .zip(&self.routing.carrier)
            .any(|(e, &c)| c != 0.0 && e.is_active())
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    /// Bessel function of the first kind, by its series.
    fn bessel_j(n: u32, x: f64) -> f64 {
        let mut sum = 0.0;
        let mut fact_k = 1.0;
        for k in 0..30u32 {
            if k > 0 {
                fact_k *= f64::from(k);
            }
            let fact_nk: f64 = (1..=n + k).map(f64::from).product();
            let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
            sum += sign / (fact_k * fact_nk) * (x / 2.0).powi((2 * k + n) as i32);
        }
        sum
    }

    /// Amplitude of the component at `hz` in `x` (a whole number of cycles long).
    fn amplitude(x: &[f32], hz: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, &v) in x.iter().enumerate() {
            let w = std::f64::consts::TAU * hz * i as f64 / f64::from(SR);
            re += f64::from(v) * w.cos();
            im += f64::from(v) * w.sin();
        }
        2.0 * (re * re + im * im).sqrt() / x.len() as f64
    }

    #[test]
    fn a_simple_pair_matches_the_bessel_spectrum() {
        // Carrier at 4·f, modulator at f (f = 250 Hz): sidebands every 250 Hz around 1 kHz,
        // the n-th with amplitude |Jₙ(β)|.
        let beta = 1.5_f64;
        let mut fm = FmSettings::default();
        fm.algorithm = 2; // 2 → 1, 4 → 3, 6 → 5: use the first pair only.
        for op in &mut fm.ops {
            op.level = 0.0;
            op.velocity = 0.0;
            op.env = EnvSettings {
                attack_ms: 0.0,
                decay_ms: 0.0,
                sustain: 1.0,
                release_ms: 10.0,
            };
        }
        fm.ops[0].ratio = 4.0;
        fm.ops[0].level = 1.0;
        fm.ops[1].ratio = 1.0;
        fm.ops[1].level = (beta / (std::f64::consts::TAU * f64::from(DEPTH))) as f32;
        warm_up();
        let mut v = FmVoice::default();
        v.start(&fm, 60, 1.0, SR, false);
        v.control(&fm, 250.0, SR, 0.0, 0.0);
        // The pair's carrier gain is 1/√3 (three carriers in the algorithm).
        let g = f64::from(v.routing.carrier[0]);
        let x: Vec<f32> = (0..48_000).map(|_| v.tick()).collect();
        let x = &x[4800..4800 + 38_400];
        for n in 0..=3u32 {
            let want = bessel_j(n, beta).abs() * g;
            let got = amplitude(x, 1000.0 + 250.0 * f64::from(n)).max(0.0);
            let below = amplitude(x, 1000.0 - 250.0 * f64::from(n));
            assert!((got - want).abs() < 0.01 * g, "n={n}: {got} vs {want}");
            assert!((below - want).abs() < 0.01 * g, "n=-{n}: {below} vs {want}");
        }
    }

    #[test]
    fn feedback_brightens_and_index_scales_modulation() {
        let mut fm = FmSettings::default();
        fm.feedback = 0.0;
        let r = fm.routing(1.0, 0.0);
        assert_eq!(r.m[0][1], 1.0);
        assert_eq!(r.m[5][5], 0.0);
        let r = fm.routing(0.5, 0.3);
        assert_eq!(r.m[0][1], 0.5);
        assert!((r.m[5][5] - 0.3).abs() < 1e-6);
        fm.algorithm = FREE;
        fm.matrix[2][4] = 0.7;
        let r = fm.routing(1.0, 0.0);
        assert_eq!(r.m[2][4], 0.7);
        assert_eq!(r.carrier, fm.out);
        assert!(is_carrier(1, 3) && !is_carrier(1, 4));
        assert_eq!(modulators(3, 0).collect::<Vec<_>>(), [1, 2, 3]);
    }

    #[test]
    fn key_scaling_and_rate_scaling() {
        let mut fm = FmSettings::default();
        fm.ops[0].key_scale_db = -6.0;
        fm.ops[0].velocity = 0.0;
        let mut v = FmVoice::default();
        v.start(&fm, 72, 1.0, SR, false);
        // One octave up at -6 dB per octave: half the level.
        assert!((v.gain[0] - 0.5).abs() < 0.01, "{}", v.gain[0]);
        let mut op = fm.ops[0];
        op.rate_scale = 1.0;
        let e = op.env;
        assert_eq!(
            op_env(&op, 72, SR),
            AdsrParams::new(
                SR,
                e.attack_ms / 2.0,
                e.decay_ms / 2.0,
                e.sustain,
                e.release_ms / 2.0
            )
        );
    }

    #[test]
    fn only_wide_spectra_run_oversampled() {
        let mut fm = FmSettings::default();
        fm.ops[1].level = 1.0;
        fm.ops[1].ratio = 3.0;
        let mut v = FmVoice::default();
        v.start(&fm, 48, 1.0, SR, false);
        v.control(&fm, 130.0, SR, 0.0, 0.0);
        assert!(!v.os);
        v.start(&fm, 96, 1.0, SR, false);
        v.control(&fm, 2093.0, SR, 0.0, 0.0);
        assert!(v.os);
        // Decided once per note.
        v.control(&fm, 100.0, SR, 0.0, 0.0);
        assert!(v.os);
    }
}
