//! Tempo-synced stutter and gate.

use super::{clamp_param, DelayLine, Effect, FxContext, Smooth};

/// Steps in the pattern.
pub const STEPS: usize = 16;

mod p {
    use super::STEPS;
    /// Steps 0 to 15 come first.
    pub const LENGTH: usize = STEPS;
    pub const SMOOTH: usize = STEPS + 1;
    pub const MIX: usize = STEPS + 2;
    pub const COUNT: usize = STEPS + 3;
}

/// Step lengths in beats, indexed by the `length` parameter.
const STEP_BEATS: [f64; 4] = [0.125, 0.25, 0.5, 1.0];
/// Longest stretch of past input a step can replay.
const HISTORY_SECONDS: f32 = 4.0;

/// What a step does, by parameter value.
const OFF: u8 = 0;
const ON: u8 = 1;
const REPEAT: u8 = 2;
const HALF: u8 = 3;

/// One way of reading the history, started at frame `anchor`.
#[derive(Debug, Clone, Copy)]
struct Voice {
    kind: u8,
    anchor: u64,
    step: f32,
}

impl Voice {
    /// How far back to read at frame `t`, or `None` for silence.
    #[inline]
    fn delay(&self, t: u64) -> Option<f32> {
        let e = t.saturating_sub(self.anchor) as f32;
        match self.kind {
            OFF => None,
            // The step before the run started, again and again.
            REPEAT => Some(self.step + e - e % self.step),
            // The step's own audio at half speed (an octave down).
            HALF => Some(e * 0.5),
            _ => Some(0.0),
        }
    }
}

/// Stutter/gate: a 16-step pattern at a note value of the song tempo. Each step is off
/// (silence), on (the input), repeat (the audio of the step before the run of repeats,
/// replayed) or half (the step's own audio played at half speed). Steps follow the song
/// position while the transport runs, so the pattern lines up with the bar; when stopped it
/// runs freely. Changes between steps crossfade over the smooth time.
#[derive(Debug, Clone)]
pub struct Stutter {
    sr: f32,
    steps: [u8; STEPS],
    length: usize,
    smooth: usize,
    mix: Smooth,
    hist: [DelayLine; 2],
    t: u64,
    free_beats: f64,
    last_step: i64,
    cur: Voice,
    prev: Voice,
    fade: usize,
    fade_len: usize,
}

impl Stutter {
    /// Every step on, sixteenth notes.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let cap = (HISTORY_SECONDS * sr) as usize;
        let on = Voice {
            kind: ON,
            anchor: 0,
            step: 1.0,
        };
        let mut s = Self {
            sr,
            steps: [ON; STEPS],
            length: 1,
            smooth: 0,
            mix: Smooth::new(1.0, 20.0, sr),
            hist: [DelayLine::new(cap), DelayLine::new(cap)],
            t: 0,
            free_beats: 0.0,
            last_step: -1,
            cur: on,
            prev: on,
            fade: 0,
            fade_len: 1,
        };
        s.set_param(p::SMOOTH, 3.0);
        s
    }

    #[inline]
    fn read(&self, ch: usize, v: &Voice) -> f32 {
        let max = self.hist[ch].max_delay();
        v.delay(self.t).map_or(0.0, |d| {
            // A run of repeats longer than the history loops inside it.
            let d = if d > max { d % max.max(1.0) } else { d };
            self.hist[ch].read(d)
        })
    }
}

impl Effect for Stutter {
    fn param_count(&self) -> usize {
        p::COUNT
    }

    fn set_param(&mut self, index: usize, v: f32) {
        match index {
            i if i < STEPS => self.steps[i] = clamp_param(v, 0.0, 3.0, 1.0).round() as u8,
            p::LENGTH => self.length = clamp_param(v, 0.0, 3.0, 1.0).round() as usize,
            p::SMOOTH => {
                self.smooth = (clamp_param(v, 0.0, 20.0, 3.0) * 0.001 * self.sr) as usize;
            }
            p::MIX => self.mix.set(clamp_param(v, 0.0, 1.0, 1.0)),
            _ => {}
        }
    }

    fn reset(&mut self) {
        for h in &mut self.hist {
            h.clear();
        }
        self.t = 0;
        self.free_beats = 0.0;
        self.last_step = -1;
        self.fade = 0;
        self.mix.snap();
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &FxContext) {
        let bpm = f64::from(ctx.bpm.clamp(10.0, 999.0));
        let beats_per_frame = bpm / 60.0 / f64::from(self.sr);
        let start = ctx.beats.unwrap_or(self.free_beats);
        let step_beats = STEP_BEATS[self.length];
        let step_frames = (step_beats / beats_per_frame) as f32;
        let n = l.len().min(r.len());
        for i in 0..n {
            let pos = start + i as f64 * beats_per_frame;
            let abs = (pos / step_beats).floor() as i64;
            if abs != self.last_step {
                let kind = self.steps[abs.rem_euclid(STEPS as i64) as usize];
                let carry_on = self.last_step >= 0
                    && kind == self.cur.kind
                    && matches!(kind, OFF | ON | REPEAT);
                if !carry_on {
                    let next = Voice {
                        kind,
                        anchor: self.t,
                        step: step_frames.max(1.0),
                    };
                    if self.last_step >= 0 && self.smooth > 0 {
                        self.prev = self.cur;
                        self.fade = self.smooth;
                        self.fade_len = self.smooth;
                    }
                    self.cur = next;
                }
                self.last_step = abs;
            }
            self.hist[0].push(l[i]);
            self.hist[1].push(r[i]);
            let mut wet = [self.read(0, &self.cur), self.read(1, &self.cur)];
            if self.fade > 0 {
                let a = self.fade as f32 / self.fade_len as f32;
                for (ch, w) in wet.iter_mut().enumerate() {
                    *w = *w * (1.0 - a) + self.read(ch, &self.prev) * a;
                }
                self.fade -= 1;
            }
            let mix = self.mix.next_value();
            l[i] = l[i] * (1.0 - mix) + wet[0] * mix;
            r[i] = r[i] * (1.0 - mix) + wet[1] * mix;
            self.t += 1;
        }
        self.free_beats = start + n as f64 * beats_per_frame;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::test_util::{rms, run};

    const SR: f32 = 48_000.0;
    /// A sixteenth at 120 BPM.
    const STEP: usize = 6000;

    fn stutter(pattern: &[u8]) -> Stutter {
        let mut s = Stutter::new(SR);
        for (i, &k) in pattern.iter().enumerate() {
            s.set_param(i, f32::from(k));
        }
        s.set_param(p::SMOOTH, 0.0);
        s.reset();
        s
    }

    fn ramp() -> Vec<f32> {
        (0..8 * STEP).map(|i| i as f32 * 1e-4).collect()
    }

    #[test]
    fn the_gate_follows_the_pattern_in_time() {
        let mut s = stutter(&[ON, OFF, ON, OFF, ON, OFF, ON, OFF]);
        let x = vec![0.5; 8 * STEP];
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut s, &mut l, &mut r, 120.0);
        for k in 0..8 {
            let level = rms(&l[k * STEP + 1..(k + 1) * STEP - 1]);
            let want = if k % 2 == 0 { 0.5 } else { 0.0 };
            assert!((level - want).abs() < 1e-6, "step {k}: {level}");
        }
    }

    #[test]
    fn repeat_replays_the_step_before_and_half_slows_down() {
        let x = ramp();
        let mut s = stutter(&[ON, REPEAT, REPEAT, ON, HALF, HALF, ON, ON]);
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut s, &mut l, &mut r, 120.0);
        // Within a couple of frames (the ramp rises 1e-4 per frame).
        let close = |a: f32, b: f32| (a - b).abs() < 2.5e-4;
        // Steps 1 and 2 both play step 0 again.
        assert!(close(l[STEP + 1000], x[1000]));
        assert!(close(l[2 * STEP + 1000], x[1000]));
        assert!(close(l[3 * STEP + 1000], x[3 * STEP + 1000]));
        // Each half step starts on its own first sample and moves at half speed.
        assert!(close(l[4 * STEP + 2000], x[4 * STEP + 1000]));
        assert!(close(l[5 * STEP + 2000], x[5 * STEP + 1000]));
    }

    #[test]
    fn steps_follow_the_song_position() {
        let mut s = stutter(&[ON, OFF]);
        // Starting a sixteenth into the bar: step 1, which is off.
        let mut l = vec![0.5; 64];
        let mut r = l.clone();
        let ctx = FxContext {
            bpm: 120.0,
            sidechain: None,
            beats: Some(0.25),
        };
        s.process(&mut l, &mut r, &ctx);
        assert!(l.iter().all(|&v| v == 0.0));
        // Two sixteenths in: step 2, on again.
        let mut l = vec![0.5; 64];
        let mut r = l.clone();
        s.process(
            &mut l,
            &mut r,
            &FxContext {
                beats: Some(0.5),
                ..ctx
            },
        );
        assert!(l.iter().all(|&v| v == 0.5));
    }
}
