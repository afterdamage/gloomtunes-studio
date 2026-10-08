//! Tempo-synced stereo delay with ping-pong.

use super::{clamp_param, one_pole_coef, DelayLine, Effect, FxContext, Smooth};

/// Note values the delay time can be set to, in quarter notes (beats), indexed by the time
/// parameter. Names are in `gt_core::effects`.
pub const DELAY_DIVISIONS: [f32; 12] = [
    0.125,     // 1/32
    1.0 / 6.0, // 1/16 triplet
    0.25,      // 1/16
    0.375,     // dotted 1/16
    1.0 / 3.0, // 1/8 triplet
    0.5,       // 1/8
    0.75,      // dotted 1/8
    2.0 / 3.0, // 1/4 triplet
    1.0,       // 1/4
    1.5,       // dotted 1/4
    2.0,       // 1/2
    4.0,       // 1 bar of 4/4
];
/// Longest delay the buffers hold.
const MAX_SECONDS: f32 = 4.0;

mod p {
    pub const TIME: usize = 0;
    pub const FEEDBACK: usize = 1;
    pub const PING_PONG: usize = 2;
    pub const TONE: usize = 3;
    pub const MIX: usize = 4;
    pub const COUNT: usize = 5;
}

/// Delay whose time is a note value at the current tempo. Each repeat passes through a gentle
/// one-pole low-pass ("tone"), so echoes darken as they fade, like tape. In ping-pong mode the
/// input (summed to mono) enters the left line and the lines feed each other, so repeats
/// alternate left and right. Time changes glide, which bends the pitch of echoes in flight
/// instead of clicking.
#[derive(Debug, Clone)]
pub struct Delay {
    sr: f32,
    left: DelayLine,
    right: DelayLine,
    division: usize,
    /// Delay in frames.
    time: Smooth,
    feedback: Smooth,
    ping_pong: bool,
    tone_coef: f32,
    tone_l: f32,
    tone_r: f32,
    mix: Smooth,
    last_bpm: f32,
}

impl Delay {
    /// A dotted-eighth ping-pong delay.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let cap = (MAX_SECONDS * sr) as usize + 4;
        Self {
            sr,
            left: DelayLine::new(cap),
            right: DelayLine::new(cap),
            division: 6,
            time: Smooth::new(0.375 * sr, 60.0, sr),
            feedback: Smooth::new(0.4, 20.0, sr),
            ping_pong: true,
            tone_coef: one_pole_coef(6000.0, sr),
            tone_l: 0.0,
            tone_r: 0.0,
            mix: Smooth::new(0.3, 20.0, sr),
            last_bpm: 0.0,
        }
    }

    fn update_time(&mut self, bpm: f32) {
        let bpm = if bpm.is_finite() {
            bpm.clamp(10.0, 999.0)
        } else {
            120.0
        };
        let frames = DELAY_DIVISIONS[self.division] * 60.0 / bpm * self.sr;
        self.time.set(frames.clamp(1.0, self.left.max_delay()));
        self.last_bpm = bpm;
    }
}

impl Effect for Delay {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            p::TIME => {
                let max = (DELAY_DIVISIONS.len() - 1) as f32;
                self.division = clamp_param(v, 0.0, max, 6.0).round() as usize;
                let bpm = self.last_bpm;
                if bpm > 0.0 {
                    self.update_time(bpm);
                }
            }
            p::FEEDBACK => self.feedback.set(clamp_param(v, 0.0, 0.95, 0.4)),
            p::PING_PONG => self.ping_pong = clamp_param(v, 0.0, 1.0, 1.0) >= 0.5,
            p::TONE => {
                self.tone_coef = one_pole_coef(clamp_param(v, 500.0, 20_000.0, 6000.0), self.sr);
            }
            p::MIX => self.mix.set(clamp_param(v, 0.0, 1.0, 0.3)),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.left.clear();
        self.right.clear();
        self.tone_l = 0.0;
        self.tone_r = 0.0;
        self.time.snap();
        self.feedback.snap();
        self.mix.snap();
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &FxContext) {
        if ctx.bpm != self.last_bpm {
            let first = self.last_bpm == 0.0;
            self.update_time(ctx.bpm);
            if first {
                self.time.snap();
            }
        }
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let d = self.time.next_value();
            let fb = self.feedback.next_value();
            let mix = self.mix.next_value();
            // Read the echoes (d frames ago), then darken them.
            let el = self.left.read(d - 1.0);
            let er = self.right.read(d - 1.0);
            self.tone_l += (el - self.tone_l) * self.tone_coef;
            self.tone_r += (er - self.tone_r) * self.tone_coef;
            let (in_l, in_r) = if self.ping_pong {
                (0.5 * (*x + *y) + self.tone_r * fb, self.tone_l * fb)
            } else {
                (*x + self.tone_l * fb, *y + self.tone_r * fb)
            };
            self.left.push(in_l);
            self.right.push(in_r);
            *x += (el - *x) * mix;
            *y += (er - *y) * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::run;

    fn impulse(n: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; n];
        l[0] = 1.0;
        (l, vec![0.0; n])
    }

    fn peak_at(x: &[f32]) -> usize {
        x.iter()
            .enumerate()
            .skip(1)
            .fold(
                (0, 0.0_f32),
                |m, (i, v)| if v.abs() > m.1 { (i, v.abs()) } else { m },
            )
            .0
    }

    #[test]
    fn quarter_note_at_120_bpm_echoes_after_half_a_second() {
        let sr = 48_000.0;
        let mut d = Delay::new(sr);
        d.set_param(p::TIME, 8.0); // 1/4
        d.set_param(p::PING_PONG, 0.0);
        d.set_param(p::MIX, 1.0);
        d.set_param(p::FEEDBACK, 0.0);
        d.set_param(p::TONE, 20_000.0);
        d.reset();
        let (mut l, mut r) = impulse(30_000);
        run(&mut d, &mut l, &mut r, 120.0);
        assert_eq!(peak_at(&l), 24_000);
        assert!(r.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn ping_pong_alternates_and_fades() {
        let sr = 48_000.0;
        let mut d = Delay::new(sr);
        d.set_param(p::TIME, 5.0); // 1/8 = 12,000 frames at 120 BPM
        d.set_param(p::MIX, 1.0);
        d.set_param(p::FEEDBACK, 0.5);
        d.reset();
        let (mut l, mut r) = impulse(40_000);
        r[0] = 1.0;
        run(&mut d, &mut l, &mut r, 120.0);
        let first_l = l[11_990..12_010]
            .iter()
            .fold(0.0_f32, |m, v| m.max(v.abs()));
        let first_r = r[11_990..12_010]
            .iter()
            .fold(0.0_f32, |m, v| m.max(v.abs()));
        let second_r = r[23_990..24_010]
            .iter()
            .fold(0.0_f32, |m, v| m.max(v.abs()));
        let second_l = l[23_990..24_010]
            .iter()
            .fold(0.0_f32, |m, v| m.max(v.abs()));
        assert!(first_l > 0.3 && first_r < 1e-6, "{first_l} {first_r}");
        assert!(second_r > 0.1 && second_l < 1e-6, "{second_r} {second_l}");
        assert!(second_r < first_l);
    }

    #[test]
    fn follows_tempo_changes() {
        let sr = 48_000.0;
        let mut d = Delay::new(sr);
        d.set_param(p::TIME, 8.0);
        let ctx = FxContext {
            bpm: 120.0,
            sidechain: None,
        };
        let (mut a, mut b) = (vec![0.0; 64], vec![0.0; 64]);
        d.process(&mut a, &mut b, &ctx);
        assert!((d.time.target() - 24_000.0).abs() < 1.0);
        let ctx = FxContext { bpm: 60.0, ..ctx };
        d.process(&mut a, &mut b, &ctx);
        assert!((d.time.target() - 48_000.0).abs() < 1.0);
    }
}
