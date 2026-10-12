//! Time-stretching and pitch-shifting (Prompt 15, ARCHITECTURE.md D106).
//!
//! A [`StretchEngine`] turns a stream of stereo input into a stream of output whose length and
//! pitch can differ (a phase vocoder or similar). The engine is a trait so the library can be
//! swapped; [`Signalsmith`] wraps Signalsmith Stretch (MIT).
//!
//! [`StretchStream`] drives an engine deterministically: it always produces output in blocks of
//! [`BLOCK`] frames from the start of the clip, feeding exactly the input that the clip's
//! [`TimeMap`] asks for at each block. The output therefore depends only on the source, the map
//! and the pitch, never on how a caller cuts its requests, so a render made ahead of time on a
//! worker thread and one made while playing give the same samples.
//!
//! Latency: the engine delays its output by `output_latency` frames and looks `input_latency`
//! frames ahead. The stream compensates both: output frame `i` sounds the source around
//! `map(i)`, and the first call renders the `output_latency` frames of warm-up.

use std::f64;

/// Output frames per engine call.
pub const BLOCK: usize = 256;

/// A time-stretch and pitch-shift engine working on interleaved stereo.
pub trait StretchEngine: Send {
    /// Frames of input the engine needs ahead of the output position.
    fn input_latency(&self) -> usize;
    /// Frames by which the output trails the input it reflects.
    fn output_latency(&self) -> usize;
    /// Pitch shift in semitones (fractions are cents).
    fn set_transpose(&mut self, semitones: f32);
    /// Back to the configured state, keeping the pitch.
    fn reset(&mut self);
    /// Pre-roll: primes the engine with `input` (interleaved stereo) at `rate` input frames per
    /// output frame, without producing output.
    fn seek(&mut self, input: &[f32], rate: f64);
    /// Consumes `input` and produces `output` (both interleaved stereo, any lengths).
    fn process(&mut self, input: &[f32], output: &mut [f32]);
}

/// Signalsmith Stretch (MIT) through the `signalsmith-stretch` crate, default preset.
pub struct Signalsmith {
    inner: signalsmith_stretch::Stretch,
}

impl Signalsmith {
    /// A stereo engine for audio at `sample_rate`.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            inner: signalsmith_stretch::Stretch::preset_default(2, sample_rate.max(1)),
        }
    }
}

impl StretchEngine for Signalsmith {
    fn input_latency(&self) -> usize {
        self.inner.input_latency()
    }

    fn output_latency(&self) -> usize {
        self.inner.output_latency()
    }

    fn set_transpose(&mut self, semitones: f32) {
        self.inner.set_transpose_factor_semitones(semitones, None);
    }

    fn reset(&mut self) {
        self.inner.reset();
    }

    fn seek(&mut self, input: &[f32], rate: f64) {
        self.inner.seek(input, rate);
    }

    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        self.inner.process(input, output);
    }
}

/// Where each output frame reads the source: the source frame at every [`BLOCK`]-th output
/// frame, interpolated linearly between them and extended past both ends with the nearest
/// segment's slope.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeMap {
    /// Source frame at output frame `k * BLOCK`; at least two points.
    points: Vec<f64>,
}

impl TimeMap {
    /// A constant ratio: `speed` source frames per output frame, for `out_frames` frames.
    pub fn linear(speed: f64, out_frames: usize) -> Self {
        Self::from_fn(out_frames, |o| o * speed)
    }

    /// Samples `f` (output frame to source frame) every [`BLOCK`] frames over `out_frames`.
    /// `f` should not decrease.
    pub fn from_fn(out_frames: usize, f: impl Fn(f64) -> f64) -> Self {
        let n = out_frames.div_ceil(BLOCK).max(1) + 1;
        Self {
            points: (0..n).map(|k| f((k * BLOCK) as f64)).collect(),
        }
    }

    /// Source frame for output frame `out` (fractional).
    pub fn at(&self, out: f64) -> f64 {
        let p = &self.points;
        let x = out / BLOCK as f64;
        let k = (x.floor().max(0.0) as usize).min(p.len() - 2);
        p[k] + (p[k + 1] - p[k]) * (x - k as f64)
    }

    /// Source frames per output frame around output frame `out`.
    pub fn speed(&self, out: f64) -> f64 {
        let p = &self.points;
        let k = ((out / BLOCK as f64).floor().max(0.0) as usize).min(p.len() - 2);
        ((p[k + 1] - p[k]) / BLOCK as f64).max(1e-6)
    }

    /// Output frames the map was made for (rounded up to whole blocks).
    pub fn out_frames(&self) -> usize {
        (self.points.len() - 1) * BLOCK
    }

    /// Most source frames any block reads, for sizing buffers.
    pub fn max_block_input(&self) -> usize {
        self.points
            .windows(2)
            .map(|w| (w[1] - w[0]).abs().ceil() as usize + 2)
            .max()
            .unwrap_or(BLOCK)
            .max(BLOCK)
    }
}

/// Deterministic streaming stretch of one stereo source (see the module docs). Allocates only
/// in [`StretchStream::new`].
pub struct StretchStream {
    engine: Box<dyn StretchEngine>,
    map: TimeMap,
    /// Output frame the current run of blocks started at (0, or where [`StretchStream::seek_to`]
    /// jumped to).
    base: usize,
    /// Next source frame to feed (relative to the source start, may be past its end).
    cursor: i64,
    /// Raw output blocks produced so far.
    raw_blocks: usize,
    /// Output frames delivered so far (after the latency is dropped).
    delivered: usize,
    /// Raw output frames still to drop for the output latency.
    to_drop: usize,
    started: bool,
    input: Vec<f32>,
    block: Vec<f32>,
    /// Unread frames of `block`, from `block_pos`.
    block_pos: usize,
}

impl StretchStream {
    /// A stream reading through `map`, pitched by `semitones`.
    pub fn new(mut engine: Box<dyn StretchEngine>, map: TimeMap, semitones: f32) -> Self {
        engine.reset();
        engine.set_transpose(semitones);
        let cap = 2 * (map.max_block_input() + engine.input_latency() + BLOCK);
        let to_drop = engine.output_latency();
        Self {
            engine,
            map,
            base: 0,
            cursor: 0,
            raw_blocks: 0,
            delivered: 0,
            to_drop,
            started: false,
            input: Vec::with_capacity(cap),
            block: vec![0.0; 2 * BLOCK],
            block_pos: BLOCK,
        }
    }

    /// Output frames delivered so far.
    pub fn position(&self) -> usize {
        self.delivered
    }

    /// Jumps to output frame `out` (playback located into the clip) without allocating. The
    /// engine restarts there as a fresh stream would at the start, so the output after a jump
    /// sounds the same source but is not bit-identical to an uninterrupted run; the first block
    /// after it renders the engine's latency worth of warm-up.
    pub fn seek_to(&mut self, out: usize) {
        self.engine.reset();
        self.base = out;
        self.cursor = 0;
        self.raw_blocks = 0;
        self.delivered = out;
        self.to_drop = self.engine.output_latency();
        self.started = false;
        self.block_pos = BLOCK;
    }

    /// Writes the next `out_l.len()` frames (same length as `out_r`) reading the source
    /// `left`/`right` (the same slice twice for mono). Frames outside the source read as silence.
    pub fn fill(&mut self, left: &[f32], right: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len().min(out_r.len());
        let mut i = 0;
        while i < n {
            if self.block_pos >= BLOCK {
                self.next_block(left, right);
                continue;
            }
            let take = (BLOCK - self.block_pos).min(n - i);
            for k in 0..take {
                let f = self.block_pos + k;
                out_l[i + k] = self.block[2 * f];
                out_r[i + k] = self.block[2 * f + 1];
            }
            self.block_pos += take;
            self.delivered += take;
            i += take;
        }
    }

    /// Renders one raw block into `block`, then drops what the output latency asks for.
    fn next_block(&mut self, left: &[f32], right: &[f32]) {
        let len = left.len().min(right.len()) as i64;
        let read = |f: i64| -> (f32, f32) {
            if (0..len).contains(&f) {
                (left[f as usize], right[f as usize])
            } else {
                (0.0, 0.0)
            }
        };
        let in_lat = self.engine.input_latency() as i64;
        if !self.started {
            self.started = true;
            let start = self.map.at(self.base as f64).floor() as i64;
            self.input.clear();
            for f in start..start + in_lat {
                let (l, r) = read(f);
                self.input.push(l);
                self.input.push(r);
            }
            let speed = self.map.speed(self.base as f64);
            self.engine.seek(&self.input, speed);
            self.cursor = start + in_lat;
        }
        let end_out = (self.base + (self.raw_blocks + 1) * BLOCK) as f64;
        let want = (self.map.at(end_out).floor() as i64 + in_lat).max(self.cursor);
        self.input.clear();
        for f in self.cursor..want {
            let (l, r) = read(f);
            self.input.push(l);
            self.input.push(r);
        }
        self.cursor = want;
        self.engine.process(&self.input, &mut self.block);
        self.raw_blocks += 1;
        let drop = self.to_drop.min(BLOCK);
        self.to_drop -= drop;
        self.block_pos = drop;
    }
}

/// Renders `frames` frames of `left`/`right` through `map` in one go (the cache build). Gives
/// exactly what a [`StretchStream`] would deliver for the same arguments.
pub fn render(
    engine: Box<dyn StretchEngine>,
    left: &[f32],
    right: &[f32],
    map: TimeMap,
    semitones: f32,
    frames: usize,
) -> (Vec<f32>, Vec<f32>) {
    let mut s = StretchStream::new(engine, map, semitones);
    let mut l = vec![0.0; frames];
    let mut r = vec![0.0; frames];
    s.fill(left, right, &mut l, &mut r);
    (l, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn sine(hz: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (0.5 * (2.0 * f64::consts::PI * hz * i as f64 / f64::from(SR)).sin()) as f32)
            .collect()
    }

    #[test]
    fn maps_interpolate_and_extend() {
        let m = TimeMap::linear(0.5, 1000);
        assert_eq!(m.at(512.0), 256.0);
        assert_eq!(m.at(100.0), 50.0);
        assert!((m.at(5000.0) - 2500.0).abs() < 1e-9, "extended");
        assert!((m.speed(3.0) - 0.5).abs() < 1e-12);
        assert!(m.out_frames() >= 1000);
    }

    /// Frequency of the strongest FFT bin of `x` (Hann window, parabolic interpolation).
    fn peak_hz(x: &[f32]) -> f64 {
        use rustfft::num_complex::Complex;
        let n = x.len();
        let mut buf: Vec<Complex<f64>> = x
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let w = 0.5 - 0.5 * (2.0 * f64::consts::PI * i as f64 / n as f64).cos();
                Complex::new(f64::from(v) * w, 0.0)
            })
            .collect();
        rustfft::FftPlanner::new()
            .plan_fft_forward(n)
            .process(&mut buf);
        let mag: Vec<f64> = buf[..n / 2].iter().map(|c| c.norm()).collect();
        let k = (1..n / 2 - 1)
            .max_by(|&a, &b| mag[a].total_cmp(&mag[b]))
            .unwrap();
        let (a, b, c) = (mag[k - 1].ln(), mag[k].ln(), mag[k + 1].ln());
        let d = 0.5 * (a - c) / (a - 2.0 * b + c);
        (k as f64 + d) * f64::from(SR) / n as f64
    }

    #[test]
    fn pitch_shifts_by_the_set_interval_and_stretch_keeps_pitch() {
        let src = sine(440.0, 2 * SR as usize);
        let cases = [
            (0.5, 7.0),
            (1.0, 12.0),
            (1.5, 0.0),
            (1.0, -5.0),
            (1.0, 0.37),
        ];
        for (speed, semis) in cases {
            let frames = (2.0 * f64::from(SR) / speed) as usize;
            let (l, _) = render(
                Box::new(Signalsmith::new(SR)),
                &src,
                &src,
                TimeMap::linear(speed, frames),
                semis as f32,
                frames,
            );
            let mid = &l[frames / 8..frames / 8 + 32768];
            let want = 440.0 * 2f64.powf(semis / 12.0);
            let got = peak_hz(mid);
            let cents = 1200.0 * (got / want).log2();
            assert!(
                cents.abs() < 5.0,
                "speed {speed} shift {semis}: {got:.2} Hz, {cents:.2} cents off"
            );
        }
    }

    #[test]
    fn stretched_events_land_where_the_map_puts_them() {
        // Clicks every 0.25 s; stretched to 1.6 times the length they must come every 0.4 s.
        let mut src = vec![0.0_f32; 2 * SR as usize];
        for k in 0..8 {
            for i in 0..48 {
                src[k * SR as usize / 4 + i] = if i % 2 == 0 { 0.9 } else { -0.9 };
            }
        }
        let frames = (3.2 * f64::from(SR)) as usize;
        let (l, _) = render(
            Box::new(Signalsmith::new(SR)),
            &src,
            &src,
            TimeMap::linear(1.0 / 1.6, frames),
            0.0,
            frames,
        );
        assert_eq!(l.len(), frames, "exact duration");
        for k in 1..7 {
            let want = (k as f64 * 0.4 * f64::from(SR)) as usize;
            let win = &l[want - 2400..want + 2400];
            let at = win
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                .unwrap()
                .0 as i64
                - 2400;
            assert!(at.abs() < 480, "click {k} is {at} frames off");
        }
    }

    #[test]
    fn the_stream_does_not_depend_on_how_it_is_read() {
        let src = sine(330.0, SR as usize);
        let map = TimeMap::linear(0.75, 2 * SR as usize);
        let (a, _) = render(
            Box::new(Signalsmith::new(SR)),
            &src,
            &src,
            map.clone(),
            3.0,
            SR as usize,
        );
        let mut s = StretchStream::new(Box::new(Signalsmith::new(SR)), map, 3.0);
        let mut b = Vec::new();
        let mut k = 0;
        // Odd, changing request sizes, like device callbacks and engine quanta.
        while b.len() < SR as usize {
            let n = [31, 32, 441, 7, 1000][k % 5].min(SR as usize - b.len());
            let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
            s.fill(&src, &src, &mut l, &mut r);
            b.extend_from_slice(&l);
            k += 1;
        }
        assert_eq!(a, b);
        assert_eq!(s.position(), SR as usize);
    }

    #[test]
    fn a_jump_lands_on_the_map_and_a_jump_home_starts_over() {
        // Clicks every 0.25 s stretched by 1.6: after jumping into the middle they still come
        // every 0.4 s, and jumping back to 0 gives the uninterrupted render again.
        let mut src = vec![0.0_f32; 2 * SR as usize];
        for k in 0..8 {
            for i in 0..48 {
                src[k * SR as usize / 4 + i] = if i % 2 == 0 { 0.9 } else { -0.9 };
            }
        }
        let frames = (3.2 * f64::from(SR)) as usize;
        let map = TimeMap::linear(1.0 / 1.6, frames);
        let mut s = StretchStream::new(Box::new(Signalsmith::new(SR)), map.clone(), 0.0);
        let jump = SR as usize; // 1.0 s: between the clicks at 0.8 and 1.2 s
        s.seek_to(jump);
        let (mut l, mut r) = (vec![0.0; frames - jump], vec![0.0; frames - jump]);
        s.fill(&src, &src, &mut l, &mut r);
        assert_eq!(s.position(), frames);
        for k in 3..7 {
            let want = (k as f64 * 0.4 * f64::from(SR)) as usize - jump;
            let at = (want - 2400..want + 2400)
                .max_by(|&a, &b| l[a].abs().total_cmp(&l[b].abs()))
                .unwrap() as i64
                - want as i64;
            assert!(
                at.abs() < 480,
                "click {k} is {at} frames off after the jump"
            );
        }
        let (whole, _) = render(
            Box::new(Signalsmith::new(SR)),
            &src,
            &src,
            map,
            0.0,
            SR as usize,
        );
        s.seek_to(0);
        let (mut l, mut r) = (vec![0.0; SR as usize], vec![0.0; SR as usize]);
        s.fill(&src, &src, &mut l, &mut r);
        assert_eq!(l, whole);
    }
}
