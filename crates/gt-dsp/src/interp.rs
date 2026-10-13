//! Windowed-sinc interpolation for the sampler (Prompt 23, ARCHITECTURE.md D126).
//!
//! Reading a sample at a fractional position `x` is a convolution with a low-pass kernel. The
//! ideal kernel is `sinc(t)`; we cut it to `±N` frames with a Kaiser window (β = 8.6, about
//! 90 dB stop band) and store one side of it, sampled [`OVERSAMPLE`] times per frame, in a table
//! read with linear interpolation. When the voice reads faster than one frame per output frame
//! (pitched up), the kernel is stretched by the step so its cutoff falls to the output
//! Nyquist (`h(t) = c·g(c·t)` with `c = 1/step`): that is what removes the aliasing linear
//! interpolation lets through. The stretch is capped at [`MAX_TAPS`] taps per side, so very
//! high transpositions keep a little aliasing rather than an unbounded cost.

use std::sync::OnceLock;

/// Table points per input frame.
const OVERSAMPLE: usize = 256;
/// Most taps per side of a stretched kernel.
pub const MAX_TAPS: usize = 64;
/// Kaiser window shape.
const BETA: f64 = 8.6;

/// One side of a windowed-sinc kernel `±half` frames wide.
#[derive(Debug)]
pub struct SincTable {
    half: usize,
    rolloff: f64,
    table: Vec<f32>,
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..50 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

impl SincTable {
    fn new(half: usize) -> Self {
        let n = half * OVERSAMPLE + 2;
        let norm = bessel_i0(BETA);
        let table = (0..n)
            .map(|k| {
                let u = k as f64 / OVERSAMPLE as f64;
                if u >= half as f64 {
                    return 0.0;
                }
                let r = u / half as f64;
                let w = bessel_i0(BETA * (1.0 - r * r).max(0.0).sqrt()) / norm;
                let s = if u == 0.0 {
                    1.0
                } else {
                    let a = std::f64::consts::PI * u;
                    a.sin() / a
                };
                (s * w) as f32
            })
            .collect();
        // Kaiser β 8.6 has a main lobe about 5.5 / half frames wide; keep it under Nyquist.
        let rolloff = 1.0 - 2.0 / half as f64;
        Self {
            half,
            rolloff,
            table,
        }
    }

    /// The table for kernel half-width `half` (8 or 32; others round to the nearest). Built
    /// once on first use: call it off the audio thread before playing (the sampler voice does
    /// in its constructor).
    pub fn get(half: usize) -> &'static SincTable {
        static SMALL: OnceLock<SincTable> = OnceLock::new();
        static LARGE: OnceLock<SincTable> = OnceLock::new();
        if half <= 16 {
            SMALL.get_or_init(|| SincTable::new(8))
        } else {
            LARGE.get_or_init(|| SincTable::new(32))
        }
    }

    /// Kernel half-width in frames.
    pub fn half(&self) -> usize {
        self.half
    }

    #[inline]
    fn kernel(&self, u: f32) -> f32 {
        let p = u.abs() * OVERSAMPLE as f32;
        let i = p as usize;
        if i + 1 >= self.table.len() {
            return 0.0;
        }
        let f = p - i as f32;
        let a = self.table[i];
        a + (self.table[i + 1] - a) * f
    }

    /// Reads `left` and `right` at fractional frame `x` for a voice stepping `step` frames
    /// per output frame. `index` maps a frame number to a stored frame (`None` reads as
    /// silence), which is how loops wrap and region ends cut.
    #[inline]
    pub fn read(
        &self,
        left: &[f32],
        right: &[f32],
        x: f64,
        step: f64,
        index: impl Fn(i64) -> Option<usize>,
    ) -> (f32, f32) {
        let half = self.half as f64;
        let step = step.abs();
        // Pitched up, the cutoff sits a little below the output Nyquist so the window's
        // transition band falls short of it too (shorter kernels need more room).
        let c = if step > 1.0 {
            (self.rolloff / step).max(half / MAX_TAPS as f64)
        } else {
            1.0
        };
        let reach = half / c;
        let first = (x - reach).floor() as i64 + 1;
        let last = (x + reach).floor() as i64;
        let cf = c as f32;
        let mono = std::ptr::eq(left, right);
        let (mut l, mut r) = (0.0_f32, 0.0_f32);
        for i in first..=last {
            let Some(k) = index(i) else {
                continue;
            };
            let w = cf * self.kernel(((x - i as f64) * c) as f32);
            l += left.get(k).copied().unwrap_or(0.0) * w;
            if !mono {
                r += right.get(k).copied().unwrap_or(0.0) * w;
            }
        }
        if mono {
            r = l;
        }
        (l, r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(len: usize) -> impl Fn(i64) -> Option<usize> {
        move |i| usize::try_from(i).ok().filter(|&k| k < len)
    }

    #[test]
    fn integer_positions_return_the_stored_frames() {
        let t = SincTable::get(8);
        let d: Vec<f32> = (0..64).map(|i| ((i * 7) % 11) as f32 / 11.0).collect();
        for x in 10..50 {
            let (l, _) = t.read(&d, &d, x as f64, 1.0, idx(d.len()));
            assert!((l - d[x]).abs() < 1e-3, "{x}: {l} vs {}", d[x]);
        }
    }

    #[test]
    fn half_way_on_a_slow_sine_is_accurate() {
        let t = SincTable::get(32);
        let d: Vec<f32> = (0..400).map(|i| (i as f32 * 0.05).sin()).collect();
        for k in 100..300 {
            let x = k as f64 + 0.5;
            let (l, _) = t.read(&d, &d, x, 1.0, idx(d.len()));
            let want = (x as f32 * 0.05).sin();
            assert!((l - want).abs() < 1e-3, "{x}: {l} vs {want}");
        }
    }
}
