//! Look-ahead brickwall limiter.

use super::{clamp_param, Effect, FxContext, Smooth};
use crate::db_to_gain;

/// Look-ahead time. The limiter delays the audio by this much (reported as latency).
const LOOKAHEAD_MS: f32 = 1.5;

mod p {
    pub const INPUT: usize = 0;
    pub const CEILING: usize = 1;
    pub const RELEASE: usize = 2;
    pub const COUNT: usize = 3;
}

/// Limiter that never lets a sample past the ceiling.
///
/// For every input sample it computes the gain that would bring that sample to the ceiling.
/// That gain goes through (1) an instant-attack, exponential release, (2) a running minimum over
/// the look-ahead window and (3) a moving average over the same window. Because the audio is
/// delayed by one window, the average has finished ramping down by the time the loud sample
/// comes out: the gain moves smoothly (no clicks) yet is never above what the peak needs.
#[derive(Debug, Clone)]
pub struct Limiter {
    sr: f32,
    window: usize,
    input: Smooth,
    ceiling: f32,
    release_coef: f32,
    release_state: f32,
    // Audio delay lines (capacity `window`).
    delay_l: Vec<f32>,
    delay_r: Vec<f32>,
    delay_pos: usize,
    // Running minimum: a monotonic deque of (index, gain) over the last `window` samples.
    deque: Vec<(u64, f32)>,
    dq_head: usize,
    dq_len: usize,
    // Moving average of the minimum.
    avg_buf: Vec<f32>,
    avg_pos: usize,
    avg_sum: f64,
    n: u64,
    reduction_db: f32,
}

impl Limiter {
    /// A limiter at -0.3 dBFS.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let window = ((LOOKAHEAD_MS * 0.001 * sr).round() as usize).max(1);
        let mut l = Self {
            sr,
            window,
            input: Smooth::new(1.0, 20.0, sr),
            ceiling: db_to_gain(-0.3),
            release_coef: 0.0,
            release_state: 1.0,
            delay_l: vec![0.0; window],
            delay_r: vec![0.0; window],
            delay_pos: 0,
            deque: vec![(0, 1.0); window + 1],
            dq_head: 0,
            dq_len: 0,
            avg_buf: vec![1.0; window],
            avg_pos: 0,
            avg_sum: window as f64,
            n: 0,
            reduction_db: 0.0,
        };
        l.set_param(p::RELEASE, 100.0);
        l
    }

    /// Pushes a gain into the running-minimum window and returns the window's minimum.
    #[inline]
    fn running_min(&mut self, g: f32) -> f32 {
        let cap = self.deque.len();
        // Drop larger-or-equal values from the back: they can never be the minimum again.
        while self.dq_len > 0 {
            let back = (self.dq_head + self.dq_len - 1) % cap;
            if self.deque[back].1 >= g {
                self.dq_len -= 1;
            } else {
                break;
            }
        }
        let slot = (self.dq_head + self.dq_len) % cap;
        self.deque[slot] = (self.n, g);
        self.dq_len += 1;
        // Drop the front once it has left the window.
        let oldest = (self.n + 1).saturating_sub(self.window as u64);
        while self.deque[self.dq_head].0 < oldest {
            self.dq_head = (self.dq_head + 1) % cap;
            self.dq_len -= 1;
        }
        self.n += 1;
        self.deque[self.dq_head].1
    }
}

impl Effect for Limiter {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            p::INPUT => self.input.set(db_to_gain(clamp_param(v, 0.0, 24.0, 0.0))),
            p::CEILING => self.ceiling = db_to_gain(clamp_param(v, -24.0, 0.0, -0.3)),
            p::RELEASE => {
                let ms = clamp_param(v, 1.0, 2000.0, 100.0);
                self.release_coef = 1.0 - (-1.0 / (ms * 0.001 * self.sr)).exp();
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.delay_l.fill(0.0);
        self.delay_r.fill(0.0);
        self.avg_buf.fill(1.0);
        self.avg_sum = self.window as f64;
        self.dq_len = 0;
        self.dq_head = 0;
        self.release_state = 1.0;
        self.input.snap();
        self.reduction_db = 0.0;
    }

    fn latency(&self) -> usize {
        self.window - 1
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &FxContext) {
        let mut min_gain = 1.0_f32;
        let w = self.window;
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let gin = self.input.next_value();
            let (a, b) = (*x * gin, *y * gin);
            let peak = a.abs().max(b.abs());
            let need = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };
            // Instant attack, smooth release.
            self.release_state = if need < self.release_state {
                need
            } else {
                self.release_state + (need - self.release_state) * self.release_coef
            };
            let held = self.running_min(self.release_state);
            self.avg_sum += f64::from(held) - f64::from(self.avg_buf[self.avg_pos]);
            self.avg_buf[self.avg_pos] = held;
            self.avg_pos = (self.avg_pos + 1) % w;
            let gain = (self.avg_sum / w as f64) as f32;
            // Delay the audio by window - 1 samples.
            let (dl, dr) = if w == 1 {
                (a, b)
            } else {
                let i = self.delay_pos;
                let out = (self.delay_l[i], self.delay_r[i]);
                self.delay_l[i] = a;
                self.delay_r[i] = b;
                self.delay_pos = (i + 1) % (w - 1);
                out
            };
            // The average guarantees the ceiling up to rounding; clamp the last few ulps.
            let c = self.ceiling;
            *x = (dl * gain).clamp(-c, c);
            *y = (dr * gain).clamp(-c, c);
            min_gain = min_gain.min(gain);
        }
        self.reduction_db = -20.0 * min_gain.max(1e-6).log10();
    }

    fn meter(&self) -> f32 {
        self.reduction_db
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::run;

    #[test]
    fn nothing_passes_the_ceiling() {
        let sr = 48_000.0;
        let mut lim = Limiter::new(sr);
        lim.set_param(p::INPUT, 12.0);
        lim.set_param(p::CEILING, -1.0);
        lim.reset();
        let mut seed = 1_u32;
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32 * 2.0 - 1.0
        };
        let mut l: Vec<f32> = (0..48_000)
            .map(|i| noise() * if i % 7000 < 300 { 1.0 } else { 0.2 })
            .collect();
        let mut r: Vec<f32> = (0..48_000).map(|_| noise() * 0.5).collect();
        run(&mut lim, &mut l, &mut r, 120.0);
        let c = db_to_gain(-1.0);
        assert!(l.iter().chain(&r).all(|v| v.abs() <= c + 1e-6));
        assert!(lim.meter() > 0.0);
    }

    #[test]
    fn quiet_signal_is_only_delayed() {
        let sr = 48_000.0;
        let mut lim = Limiter::new(sr);
        let lat = lim.latency();
        assert_eq!(lat, 71);
        let x: Vec<f32> = (0..1000)
            .map(|i| ((i * 37) % 17) as f32 / 17.0 * 0.5 - 0.25)
            .collect();
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut lim, &mut l, &mut r, 120.0);
        for i in lat..1000 {
            assert!((l[i] - x[i - lat]).abs() < 1e-6, "{i}");
        }
    }

    #[test]
    fn gain_ramps_instead_of_jumping() {
        let sr = 48_000.0;
        let mut lim = Limiter::new(sr);
        lim.set_param(p::CEILING, -6.0);
        lim.reset();
        // A step from quiet to loud: the gain change spreads over the look-ahead window.
        let mut l: Vec<f32> = (0..2000)
            .map(|i| if i < 1000 { 0.1 } else { 1.0 })
            .collect();
        let mut r = l.clone();
        run(&mut lim, &mut l, &mut r, 120.0);
        let lat = lim.latency();
        // Before the loud part arrives, the quiet part is already being turned down gradually.
        let before = &l[1000 + lat - 60..1000 + lat];
        assert!(before.windows(2).all(|w| w[1] <= w[0] + 1e-7));
        assert!(before[0] > before[59]);
    }
}
