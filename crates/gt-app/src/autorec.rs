//! Automation recording (Prompt 17, D111): turns control moves during a song-mode recording
//! into passes of automation points, following the latch, touch or write mode.
//!
//! Pure bookkeeping: the app feeds it moves (with the song tick each lands on), releases and
//! the playhead, and writes the finished passes into the playlist with
//! [`gt_core::record_automation`].

use gt_core::{thin, AutoPoint, ParamId, PPQ, THIN_TOLERANCE};
use gt_ui::views::AutoMode;

/// A controller or plugin editor gives no release: its touch ends this long after its last
/// move (one beat).
pub const TOUCH_TIMEOUT: f64 = PPQ as f64;

/// One parameter being written.
#[derive(Debug, Clone)]
struct Pass {
    target: ParamId,
    /// Points at song ticks, rising.
    points: Vec<AutoPoint>,
    /// The mouse holds the control (a release will come).
    held: bool,
    /// Song tick of the last move.
    last: f64,
}

impl Pass {
    fn push(&mut self, at: i64, value: f32) {
        match self.points.last_mut() {
            Some(p) if p.at == at => p.value = value,
            _ => self.points.push(AutoPoint::new(at, value)),
        }
    }

    /// Holds the last value up to `at`.
    fn hold_to(&mut self, at: i64) {
        if let Some(&p) = self.points.last() {
            if at > p.at {
                self.points.push(AutoPoint::new(at, p.value));
            }
        }
    }
}

/// The passes of one recording.
#[derive(Debug, Clone)]
pub struct AutoRecorder {
    mode: AutoMode,
    /// Song tick where recording started (write mode fills from here).
    start: i64,
    /// Playhead last seen.
    now: f64,
    active: Vec<Pass>,
    done: Vec<(ParamId, Vec<AutoPoint>)>,
}

impl AutoRecorder {
    /// Recording in `mode` from song tick `start`.
    pub fn new(mode: AutoMode, start: f64) -> Self {
        Self {
            mode,
            start: start.round() as i64,
            now: start,
            active: Vec::new(),
            done: Vec::new(),
        }
    }

    /// `target` moved to `value` (normalized) at song tick `tick`. `held` is true for the
    /// mouse, which also reports the release; controllers and plugin editors pass false and
    /// their touch ends after [`TOUCH_TIMEOUT`].
    pub fn moved(&mut self, target: ParamId, tick: f64, value: f32, held: bool) {
        let at = tick.max(0.0).round() as i64;
        let i = match self.active.iter().position(|p| p.target == target) {
            Some(i) => i,
            None => {
                let mut pass = Pass {
                    target,
                    points: Vec::new(),
                    held,
                    last: tick,
                };
                if self.mode == AutoMode::Write && at > self.start {
                    pass.push(self.start, value);
                }
                self.active.push(pass);
                self.active.len() - 1
            }
        };
        let pass = &mut self.active[i];
        // A move from before the pass's end (a late timestamp) would fold the line back.
        let at = at.max(pass.points.last().map_or(at, |p| p.at));
        pass.push(at, value);
        pass.held = held;
        pass.last = pass.last.max(tick);
    }

    /// The mouse let go of `target` at song tick `tick`. In touch mode its pass ends there;
    /// latch and write keep its last value going.
    pub fn released(&mut self, target: ParamId, tick: f64) {
        let Some(i) = self.active.iter().position(|p| p.target == target) else {
            return;
        };
        self.active[i].held = false;
        if self.mode == AutoMode::Touch {
            let mut pass = self.active.remove(i);
            pass.hold_to(tick.round() as i64);
            self.close(pass);
        }
    }

    /// The playhead is at `tick`. Ends timed-out touches, and when the playhead jumped back
    /// (a loop) ends every pass where it left; latched ones carry on from where it landed.
    pub fn advance(&mut self, tick: f64) {
        if tick + 1.0 < self.now {
            let end = self.now.round() as i64;
            let latch = self.mode != AutoMode::Touch;
            let mut carry = Vec::new();
            for mut pass in std::mem::take(&mut self.active) {
                if latch || pass.held {
                    pass.hold_to(end);
                    if let Some(p) = pass.points.last() {
                        carry.push((pass.target, p.value, pass.held));
                    }
                }
                self.close(pass);
            }
            for (target, value, held) in carry {
                let at = tick.max(0.0).round() as i64;
                self.active.push(Pass {
                    target,
                    points: vec![AutoPoint::new(at, value)],
                    held,
                    last: tick,
                });
            }
        } else if self.mode == AutoMode::Touch {
            let (gone, keep): (Vec<Pass>, Vec<Pass>) = std::mem::take(&mut self.active)
                .into_iter()
                .partition(|p| !p.held && tick - p.last > TOUCH_TIMEOUT);
            self.active = keep;
            for pass in gone {
                self.close(pass);
            }
        }
        self.now = tick;
    }

    /// Recording stopped at `tick`: every pass ends (latch and write hold their last value to
    /// here, so do controls still held).
    pub fn stop(&mut self, tick: f64) {
        let end = tick.round() as i64;
        for mut pass in std::mem::take(&mut self.active) {
            if self.mode != AutoMode::Touch || pass.held {
                pass.hold_to(end);
            }
            self.close(pass);
        }
    }

    fn close(&mut self, pass: Pass) {
        if !pass.points.is_empty() {
            self.done
                .push((pass.target, thin(&pass.points, THIN_TOLERANCE)));
        }
    }

    /// Finished passes (thinned), oldest first.
    pub fn take_done(&mut self) -> Vec<(ParamId, Vec<AutoPoint>)> {
        std::mem::take(&mut self.done)
    }

    /// Parameters being written now (their automation should not play over the control).
    pub fn writing(&self) -> Vec<ParamId> {
        self.active.iter().map(|p| p.target).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: ParamId = gt_core::MASTER_VOLUME;

    fn at(points: &[AutoPoint]) -> Vec<(i64, f32)> {
        points.iter().map(|p| (p.at, p.value)).collect()
    }

    /// Plays from tick 0 to 4000 with the mouse grabbing at 1000, moving to 0.8 at 1500 and
    /// letting go at 2000.
    fn grab(mode: AutoMode) -> Vec<(i64, f32)> {
        let mut r = AutoRecorder::new(mode, 0.0);
        for t in (0..=4000).step_by(100) {
            let t = f64::from(t);
            r.advance(t);
            if t == 1000.0 {
                r.moved(P, t, 0.2, true);
            }
            if t == 1500.0 {
                r.moved(P, t, 0.8, true);
            }
            if t == 2000.0 {
                r.released(P, t);
            }
            if (1000.0..2000.0).contains(&t) {
                assert_eq!(r.writing(), vec![P]);
            }
        }
        r.stop(4000.0);
        let mut done = r.take_done();
        assert_eq!(done.len(), 1);
        at(&done.remove(0).1)
    }

    #[test]
    fn touch_writes_while_held_latch_until_stop_write_from_the_start() {
        assert_eq!(
            grab(AutoMode::Touch),
            vec![(1000, 0.2), (1500, 0.8), (2000, 0.8)]
        );
        assert_eq!(
            grab(AutoMode::Latch),
            vec![(1000, 0.2), (1500, 0.8), (4000, 0.8)]
        );
        assert_eq!(
            grab(AutoMode::Write),
            vec![(0, 0.2), (1000, 0.2), (1500, 0.8), (4000, 0.8)]
        );
    }

    #[test]
    fn controller_touches_end_after_the_timeout_and_loops_split_passes() {
        let mut r = AutoRecorder::new(AutoMode::Touch, 0.0);
        r.moved(P, 100.0, 0.5, false);
        r.advance(100.0 + TOUCH_TIMEOUT);
        assert_eq!(r.writing(), vec![P]);
        r.advance(101.0 + TOUCH_TIMEOUT);
        assert!(r.writing().is_empty());
        assert_eq!(at(&r.take_done()[0].1), vec![(100, 0.5)]);

        // Latched across a loop: the pass ends at the loop end and goes on from its start.
        let mut r = AutoRecorder::new(AutoMode::Latch, 0.0);
        r.advance(500.0);
        r.moved(P, 500.0, 0.3, false);
        r.advance(3000.0);
        r.advance(0.0);
        r.advance(400.0);
        r.stop(400.0);
        let done = r.take_done();
        assert_eq!(at(&done[0].1), vec![(500, 0.3), (3000, 0.3)]);
        assert_eq!(at(&done[1].1), vec![(0, 0.3), (400, 0.3)]);
    }

    #[test]
    fn controller_moves_land_on_the_ticks_their_timestamps_play_at() {
        // The engine published: at clock 1 s the next callback starts at tick 960, playing
        // 120 BPM (1.92 ticks per ms).
        let anchor = gt_engine::ClockAnchor::default();
        let rate = 120.0 / 60.0 * PPQ as f64 / 1e9;
        anchor.publish(1_000_000_000, 960.0, rate);
        let mut r = AutoRecorder::new(AutoMode::Touch, 900.0);
        // A controller sweeping every 5 ms, read by the UI a frame later (all at once).
        let stamps: Vec<(u64, f32)> = (0..=20)
            .map(|k| (1_000_000_000 + k * 5_000_000, k as f32 / 20.0))
            .collect();
        for &(ns, v) in &stamps {
            let tick = anchor.tick_at(ns).unwrap();
            r.moved(P, tick, v, false);
        }
        r.stop(2000.0);
        let pts = r.take_done().remove(0).1;
        // A straight sweep thins to its ends, each exactly where its message plays.
        assert_eq!(at(&pts), vec![(960, 0.0), (1152, 1.0)]);
        // Each recorded value sits on the line at its message's tick.
        for (k, &(ns, v)) in stamps.iter().enumerate() {
            let tick = 960.0 + (ns - 1_000_000_000) as f64 * rate;
            assert!((tick - (960.0 + 9.6 * k as f64)).abs() < 1e-6);
            assert!((gt_core::playlist::value_at(&pts, tick) - v).abs() < 0.01);
        }
    }

    #[test]
    fn dense_moves_are_thinned() {
        let mut r = AutoRecorder::new(AutoMode::Touch, 0.0);
        for k in 0..=100 {
            let t = f64::from(k * 10);
            r.moved(P, t, k as f32 / 100.0, true);
        }
        r.released(P, 1000.0);
        let pts = at(&r.take_done()[0].1);
        assert_eq!(pts, vec![(0, 0.0), (1000, 1.0)]);
    }
}
