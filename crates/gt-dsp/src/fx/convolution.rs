//! Convolution reverb: partitioned FFT convolution (uniform, and non-uniform for long tails)
//! with generated or loaded impulse responses.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::Noise;

use super::{clamp_param, Biquad, BiquadCoeffs, DelayLine, Effect, FxContext, Smooth};

mod p {
    pub const IR: usize = 0;
    pub const LENGTH: usize = 1;
    pub const PREDELAY: usize = 2;
    pub const LOW_CUT: usize = 3;
    pub const HIGH_CUT: usize = 4;
    pub const WIDTH: usize = 5;
    pub const MIX: usize = 6;
    pub const COUNT: usize = 7;
}

/// Partition length: the reverb's latency in frames.
pub const PARTITION: usize = 256;
/// Longest impulse response, in seconds (longer user files are cut).
const MAX_IR_SECONDS: f32 = 10.0;
const MAX_PREDELAY_MS: f32 = 200.0;

/// Built-in impulse responses, indexed by the `ir` parameter. All are generated here (see
/// [`generate_ir`]); index [`USER_IR`] is the user's own file.
pub const IR_KINDS: usize = 4;
/// The `ir` value for a user file.
pub const USER_IR: usize = 4;

/// One channel of uniformly partitioned overlap-save convolution (UPOLS). The impulse
/// response is cut into partitions of `B` samples, each transformed once (size `2B`). Every
/// `B` input samples, the last `2B` inputs are transformed and pushed into a delay line of
/// spectra; the output block is the inverse transform of the sum of each stored spectrum times
/// its partition, and its second half is played during the next `B` samples (latency `B`).
pub struct Convolver {
    b: usize,
    fwd: Arc<dyn Fft<f32>>,
    inv: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex<f32>>,
    parts: Vec<Vec<Complex<f32>>>,
    fdl: Vec<Vec<Complex<f32>>>,
    head: usize,
    input: Vec<f32>,
    fill: usize,
    out: Vec<f32>,
    work: Vec<Complex<f32>>,
    acc: Vec<Complex<f32>>,
}

impl std::fmt::Debug for Convolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Convolver({} x {})", self.parts.len(), self.b)
    }
}

impl Convolver {
    /// A convolver for `ir` with partitions of `b` samples. Allocates.
    pub fn new(ir: &[f32], b: usize) -> Self {
        let b = b.max(1);
        let n = 2 * b;
        let mut planner = FftPlanner::new();
        let fwd = planner.plan_fft_forward(n);
        let inv = planner.plan_fft_inverse(n);
        let scratch_len = fwd
            .get_inplace_scratch_len()
            .max(inv.get_inplace_scratch_len());
        let mut scratch = vec![Complex::default(); scratch_len];
        let count = ir.len().div_ceil(b).max(1);
        let parts = (0..count)
            .map(|k| {
                let mut buf = vec![Complex::default(); n];
                for (i, &v) in ir.iter().skip(k * b).take(b).enumerate() {
                    buf[i] = Complex::new(v, 0.0);
                }
                fwd.process_with_scratch(&mut buf, &mut scratch);
                buf
            })
            .collect();
        Self {
            b,
            fwd,
            inv,
            scratch,
            parts,
            fdl: vec![vec![Complex::default(); n]; count],
            head: 0,
            input: vec![0.0; n],
            fill: 0,
            out: vec![0.0; b],
            work: vec![Complex::default(); n],
            acc: vec![Complex::default(); n],
        }
    }

    /// Clears the history.
    pub fn reset(&mut self) {
        for s in &mut self.fdl {
            s.fill(Complex::default());
        }
        self.input.fill(0.0);
        self.out.fill(0.0);
        self.fill = 0;
    }

    /// Takes one input sample and returns the output `B` samples behind it.
    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let y = self.out[self.fill];
        self.input[self.b + self.fill] = x;
        self.fill += 1;
        if self.fill == self.b {
            self.block();
            self.fill = 0;
        }
        y
    }

    fn block(&mut self) {
        let (b, n) = (self.b, 2 * self.b);
        for (w, &v) in self.work.iter_mut().zip(&self.input) {
            *w = Complex::new(v, 0.0);
        }
        self.fwd
            .process_with_scratch(&mut self.work, &mut self.scratch);
        let count = self.parts.len();
        self.head = (self.head + count - 1) % count;
        self.fdl[self.head].copy_from_slice(&self.work);
        // The input is real, so only bins 0..=B are summed; the rest mirror them.
        self.acc.fill(Complex::default());
        for (k, part) in self.parts.iter().enumerate() {
            let spectrum = &self.fdl[(self.head + k) % count];
            for ((a, s), h) in self.acc[..=b].iter_mut().zip(spectrum).zip(part) {
                *a += s * h;
            }
        }
        for k in 1..b {
            self.acc[n - k] = self.acc[k].conj();
        }
        self.inv
            .process_with_scratch(&mut self.acc, &mut self.scratch);
        let scale = 1.0 / n as f32;
        for (o, a) in self.out.iter_mut().zip(&self.acc[b..]) {
            *o = a.re * scale;
        }
        self.input.copy_within(b.., 0);
    }
}

/// Head partitions before the long tail partitions take over.
const HEAD_PARTS: usize = 16;

/// Non-uniformly partitioned convolution: the first `16·B` samples of the response run in
/// short partitions of `B` (low latency), the rest in partitions of `16·B`, which cost far
/// less per sample for a long response. The tail's latency of `16·B` is hidden by the head:
/// its input is delayed by `B` so its output lands exactly where the head's part ends.
#[derive(Debug)]
pub struct SplitConvolver {
    head: Convolver,
    tail: Option<Convolver>,
    delay: Vec<f32>,
    pos: usize,
}

impl SplitConvolver {
    /// A convolver for `ir` with a latency of `b` samples. Allocates.
    pub fn new(ir: &[f32], b: usize) -> Self {
        let b = b.max(1);
        let split = HEAD_PARTS * b;
        let tail = (ir.len() > split).then(|| Convolver::new(&ir[split..], split));
        Self {
            head: Convolver::new(&ir[..ir.len().min(split)], b),
            tail,
            delay: vec![0.0; b],
            pos: 0,
        }
    }

    /// Clears the history.
    pub fn reset(&mut self) {
        self.head.reset();
        if let Some(t) = &mut self.tail {
            t.reset();
        }
        self.delay.fill(0.0);
    }

    /// Takes one input sample and returns the output `B` samples behind it.
    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let y = self.head.tick(x);
        match &mut self.tail {
            Some(t) => {
                let late = std::mem::replace(&mut self.delay[self.pos], x);
                self.pos = (self.pos + 1) % self.delay.len();
                y + t.tick(late)
            }
            None => y,
        }
    }
}

/// Generates built-in impulse response `kind` (0 room, 1 hall, 2 plate, 3 spring) at
/// `length` (0.1 to 1 of its full size) as a stereo pair with unit energy per side.
///
/// Rooms and halls are a few early reflections and then noise decaying at the room's RT60
/// (-60 dB time) while a low-pass closes, since air and walls absorb highs first. The plate is
/// dense and bright from the first sample. The spring is a train of echoes along the spring,
/// each smeared by a chain of all-pass filters into the familiar downward chirp.
pub fn generate_ir(kind: usize, length: f32, sr: f32) -> [Vec<f32>; 2] {
    // (RT60 s, full length s, early reflections, start and end of the closing low-pass, Hz)
    let (rt60, full, early, bright, dark): (f32, f32, usize, f32, f32) = match kind {
        1 => (2.6, 3.5, 8, 7000.0, 1500.0),
        2 => (1.8, 2.5, 0, 12_000.0, 4000.0),
        3 => (1.4, 2.0, 0, 5000.0, 2500.0),
        _ => (0.6, 0.9, 6, 9000.0, 2500.0),
    };
    let frames = ((full * length.clamp(0.1, 1.0)) * sr) as usize;
    let mut out = [vec![0.0_f32; frames], vec![0.0_f32; frames]];
    for (ch, buf) in out.iter_mut().enumerate() {
        let mut rng = Noise::new(0x9E37_79B9 ^ (kind as u32 * 7919 + ch as u32 * 104_729 + 1));
        let decay = (-6.91 / (rt60 * sr)).exp(); // e^-6.91 = -60 dB
        if kind == 3 {
            // Spring: echoes every 33 ms (one trip along the spring), dispersed below.
            let trip = (0.033 * sr) as usize;
            let mut g = 1.0;
            let mut i = (0.004 * sr) as usize + ch * 7;
            while i < frames {
                buf[i] = g * if (i / trip) % 2 == 0 { 1.0 } else { -1.0 };
                g *= decay.powi(trip as i32);
                i += trip;
            }
            let mut stages = [(0.0_f32, 0.0_f32); 60];
            let a = -0.6;
            for v in buf.iter_mut() {
                let mut s = *v + rng.next_value() * 0.002;
                for (x1, y1) in stages.iter_mut() {
                    let y = a * s + *x1 - a * *y1;
                    *x1 = s;
                    *y1 = y;
                    s = y;
                }
                *v = s;
            }
        } else {
            let mut g = 1.0_f32;
            for v in buf.iter_mut() {
                *v = rng.next_value() * g;
                g *= decay;
            }
            // Early reflections: sparse, louder taps in the first 40 ms.
            for k in 0..early {
                let at = ((0.004 + 0.036 * (k as f32 + rng.next_value().abs()) / early as f32) * sr)
                    as usize;
                if at < frames {
                    buf[at] += 3.0 * (1.0 - k as f32 / early as f32) * rng.next_value().signum();
                }
            }
        }
        // The low-pass closes from `bright` to `dark` over the response.
        let mut lp = 0.0_f32;
        for (i, v) in buf.iter_mut().enumerate() {
            let t = i as f32 / frames.max(1) as f32;
            let fc = bright * (dark / bright).powf(t);
            let c = 1.0 - (-std::f32::consts::TAU * fc / sr).exp();
            lp += (*v - lp) * c;
            *v = lp;
        }
        // Fade the last 10 % so a shortened response does not end in a click.
        let fade = frames / 10;
        for (i, v) in buf.iter_mut().rev().take(fade).enumerate() {
            *v *= i as f32 / fade.max(1) as f32;
        }
        let energy: f32 = buf.iter().map(|v| v * v).sum();
        let norm = 1.0 / energy.max(1e-12).sqrt();
        for v in buf.iter_mut() {
            *v *= norm;
        }
    }
    out
}

/// Convolution reverb. The impulse response is built in [`Effect::prepare`] (on the UI thread):
/// changing the response or its length rebuilds the effect. Pre-delay, the low and high cut
/// on the wet signal, width and mix move freely.
#[derive(Debug)]
pub struct ConvolutionReverb {
    sr: f32,
    kind: usize,
    length: f32,
    conv: [SplitConvolver; 2],
    predelay: [DelayLine; 2],
    predelay_frames: Smooth,
    low_cut: [Biquad; 2],
    high_cut: [Biquad; 2],
    /// The cuts at their ends of the range are off.
    cuts: (bool, bool),
    width: Smooth,
    mix: Smooth,
    dry: [DelayLine; 2],
}

impl ConvolutionReverb {
    /// A room, ready to play (the response is generated here).
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let cap = (MAX_PREDELAY_MS * 0.001 * sr) as usize + 8;
        let mut c = Self {
            sr,
            kind: 0,
            length: 1.0,
            conv: [
                SplitConvolver::new(&[1.0], PARTITION),
                SplitConvolver::new(&[1.0], PARTITION),
            ],
            predelay: [DelayLine::new(cap), DelayLine::new(cap)],
            predelay_frames: Smooth::new(0.0, 50.0, sr),
            low_cut: [Biquad::default(); 2],
            high_cut: [Biquad::default(); 2],
            cuts: (false, false),
            width: Smooth::new(1.0, 20.0, sr),
            mix: Smooth::new(0.3, 20.0, sr),
            dry: [DelayLine::new(PARTITION + 4), DelayLine::new(PARTITION + 4)],
        };
        c.set_param(p::LOW_CUT, 20.0);
        c.set_param(p::HIGH_CUT, 20_000.0);
        c.prepare(None);
        c
    }

    /// A reverb with a given response (tests).
    pub fn with_ir(sample_rate: f32, l: &[f32], r: &[f32]) -> Self {
        let mut c = Self::new(sample_rate);
        c.conv = [
            SplitConvolver::new(l, PARTITION),
            SplitConvolver::new(r, PARTITION),
        ];
        c
    }
}

impl Effect for ConvolutionReverb {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            p::IR => self.kind = clamp_param(v, 0.0, USER_IR as f32, 0.0).round() as usize,
            p::LENGTH => self.length = clamp_param(v, 0.1, 1.0, 1.0),
            p::PREDELAY => self
                .predelay_frames
                .set(clamp_param(v, 0.0, MAX_PREDELAY_MS, 0.0) * 0.001 * self.sr),
            p::LOW_CUT => {
                let f = clamp_param(v, 20.0, 1000.0, 20.0);
                self.cuts.0 = f > 20.0;
                let c = BiquadCoeffs::high_pass(f, std::f32::consts::FRAC_1_SQRT_2, self.sr);
                for b in &mut self.low_cut {
                    b.c = c;
                }
            }
            p::HIGH_CUT => {
                let f = clamp_param(v, 1000.0, 20_000.0, 20_000.0);
                self.cuts.1 = f < 20_000.0;
                let f = f.min(0.45 * self.sr);
                let c = BiquadCoeffs::low_pass(f, std::f32::consts::FRAC_1_SQRT_2, self.sr);
                for b in &mut self.high_cut {
                    b.c = c;
                }
            }
            p::WIDTH => self.width.set(clamp_param(v, 0.0, 1.0, 1.0)),
            p::MIX => self.mix.set(clamp_param(v, 0.0, 1.0, 0.3)),
            _ => {}
        }
    }

    fn prepare(&mut self, user_ir: Option<(&[f32], &[f32])>) {
        let max = (MAX_IR_SECONDS * self.sr) as usize;
        let [l, r] = match user_ir {
            Some((l, r)) if self.kind == USER_IR && !l.is_empty() => {
                let cut = |x: &[f32]| {
                    let n = (x.len() as f32 * self.length) as usize;
                    x[..n.clamp(1, max).min(x.len())].to_vec()
                };
                [cut(l), cut(if r.is_empty() { l } else { r })]
            }
            _ => generate_ir(self.kind.min(IR_KINDS - 1), self.length, self.sr),
        };
        self.conv = [
            SplitConvolver::new(&l, PARTITION),
            SplitConvolver::new(&r, PARTITION),
        ];
    }

    fn reset(&mut self) {
        for c in &mut self.conv {
            c.reset();
        }
        for d in self.predelay.iter_mut().chain(&mut self.dry) {
            d.clear();
        }
        for b in self.low_cut.iter_mut().chain(&mut self.high_cut) {
            b.reset();
        }
        self.predelay_frames.snap();
        self.width.snap();
        self.mix.snap();
    }

    fn latency(&self) -> usize {
        PARTITION
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &FxContext) {
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let pd = self.predelay_frames.next_value();
            let mut wet = [0.0; 2];
            for (ch, s) in [*x, *y].into_iter().enumerate() {
                self.predelay[ch].push(s);
                self.dry[ch].push(s);
                let input = if pd < 0.5 {
                    s
                } else {
                    self.predelay[ch].read(pd)
                };
                let w = self.conv[ch].tick(input);
                let w = if self.cuts.0 {
                    self.low_cut[ch].process(w)
                } else {
                    w
                };
                wet[ch] = if self.cuts.1 {
                    self.high_cut[ch].process(w)
                } else {
                    w
                };
            }
            // Width: blend each side towards the mid.
            let width = self.width.next_value();
            let mid = 0.5 * (wet[0] + wet[1]);
            let wl = mid + (wet[0] - mid) * width;
            let wr = mid + (wet[1] - mid) * width;
            let mix = self.mix.next_value();
            // The dry signal is delayed by the latency so it lines up with the wet.
            let dl = self.dry[0].read_int(PARTITION);
            let dr = self.dry[1].read_int(PARTITION);
            *x = dl * (1.0 - mix) + wl * mix;
            *y = dr * (1.0 - mix) + wr * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitioned_convolution_matches_direct_convolution() {
        let mut rng = Noise::new(12345);
        for (ir_len, b) in [(1, 8), (100, 16), (1000, 64), (3000, 256)] {
            let ir: Vec<f32> = (0..ir_len).map(|_| rng.next_value()).collect();
            let x: Vec<f32> = (0..4000).map(|_| rng.next_value()).collect();
            let mut c = Convolver::new(&ir, b);
            let y: Vec<f32> = x.iter().map(|&v| c.tick(v)).collect();
            for n in 0..x.len() - b {
                let direct: f32 = (0..ir.len().min(n + 1)).map(|k| ir[k] * x[n - k]).sum();
                assert!(
                    (y[n + b] - direct).abs() < 1e-3,
                    "ir {ir_len}, b {b}, n {n}: {} vs {direct}",
                    y[n + b]
                );
            }
        }
    }

    #[test]
    fn split_convolution_matches_direct_convolution() {
        let mut rng = Noise::new(99);
        for (ir_len, b) in [(500, 64), (1024, 64), (1025, 64), (7000, 32)] {
            let ir: Vec<f32> = (0..ir_len).map(|_| rng.next_value()).collect();
            let x: Vec<f32> = (0..12_000).map(|_| rng.next_value()).collect();
            let mut c = SplitConvolver::new(&ir, b);
            let y: Vec<f32> = x.iter().map(|&v| c.tick(v)).collect();
            for n in 0..x.len() - b {
                let direct: f32 = (0..ir.len().min(n + 1)).map(|k| ir[k] * x[n - k]).sum();
                assert!(
                    (y[n + b] - direct).abs() < 2e-3,
                    "ir {ir_len}, b {b}, n {n}: {} vs {direct}",
                    y[n + b]
                );
            }
        }
    }

    #[test]
    fn generated_responses_decay_and_differ_per_side() {
        let sr = 48_000.0;
        for kind in 0..IR_KINDS {
            let [l, r] = generate_ir(kind, 1.0, sr);
            let energy = |x: &[f32]| x.iter().map(|v| v * v).sum::<f32>();
            assert!((energy(&l) - 1.0).abs() < 1e-3);
            assert_ne!(l, r, "{kind}");
            // The last tenth holds far less energy than the first.
            let n = l.len() / 10;
            assert!(energy(&l[l.len() - n..]) < 0.01 * energy(&l[..n]), "{kind}");
        }
        // Shorter is shorter.
        assert!(generate_ir(1, 0.5, sr)[0].len() < generate_ir(1, 1.0, sr)[0].len());
    }

    #[test]
    fn the_reverb_lines_dry_up_with_wet_and_loads_a_user_response() {
        let sr = 48_000.0;
        // A response that is a single impulse: the wet signal is the dry signal.
        let mut c = ConvolutionReverb::with_ir(sr, &[1.0], &[1.0]);
        c.set_param(p::MIX, 0.5);
        c.reset();
        let x: Vec<f32> = (0..2048).map(|i| (i as f32 * 0.05).sin()).collect();
        let (mut l, mut r) = (x.clone(), x.clone());
        crate::fx::test_util::run(&mut c, &mut l, &mut r, 120.0);
        for n in 0..x.len() - PARTITION {
            assert!((l[n + PARTITION] - x[n]).abs() < 1e-4, "{n}");
        }
        // A user file replaces the built-in response.
        let mut c = ConvolutionReverb::new(sr);
        c.set_param(p::IR, USER_IR as f32);
        c.prepare(Some((&[0.5; 10], &[])));
        assert_eq!(c.conv[0].head.parts.len(), 1);
        assert!(c.conv[0].tail.is_none());
    }
}
