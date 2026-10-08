//! Linear parameter ramp.
//!
//! Jumping a gain from 0 to 1 between two samples puts a step into the waveform, which is heard
//! as a click (a step has energy across the whole spectrum). Ramping the gain linearly over a few
//! milliseconds spreads that change out so it is inaudible. A linear ramp, unlike a one-pole
//! smoother, reaches its target exactly in a known number of samples, which is what start/stop
//! fades need: "silent" means exactly 0.0.

/// Moves linearly from its current value to a target over a fixed number of samples.
#[derive(Debug, Clone)]
pub struct LinearRamp {
    current: f32,
    target: f32,
    step: f32,
    remaining: u32,
}

impl LinearRamp {
    /// Creates a settled ramp at `value`.
    pub fn new(value: f32) -> Self {
        Self {
            current: value,
            target: value,
            step: 0.0,
            remaining: 0,
        }
    }

    /// Starts a ramp from the current value to `target`, reaching it after `frames` samples.
    /// With `frames == 0` the value jumps immediately.
    pub fn set_target(&mut self, target: f32, frames: u32) {
        self.target = target;
        if frames == 0 {
            self.current = target;
            self.remaining = 0;
            self.step = 0.0;
        } else {
            self.remaining = frames;
            self.step = (target - self.current) / frames as f32;
        }
    }

    /// Returns the next value and advances the ramp by one sample.
    #[inline]
    pub fn next_value(&mut self) -> f32 {
        if self.remaining > 0 {
            self.remaining -= 1;
            // Land exactly on the target at the end, free of accumulated rounding.
            self.current = if self.remaining == 0 {
                self.target
            } else {
                self.current + self.step
            };
        }
        self.current
    }

    /// The current value without advancing.
    #[inline]
    pub fn value(&self) -> f32 {
        self.current
    }

    /// The value the ramp is heading to.
    #[inline]
    pub fn target(&self) -> f32 {
        self.target
    }

    /// True once the target has been reached.
    #[inline]
    pub fn is_settled(&self) -> bool {
        self.remaining == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaches_target_exactly_after_n_frames() {
        let mut r = LinearRamp::new(0.0);
        r.set_target(0.251_188_64, 960);
        for _ in 0..959 {
            r.next_value();
            assert!(!r.is_settled());
        }
        assert_eq!(r.next_value(), 0.251_188_64);
        assert!(r.is_settled());
        assert_eq!(r.next_value(), 0.251_188_64);
    }

    #[test]
    fn is_monotonic() {
        let mut r = LinearRamp::new(1.0);
        r.set_target(0.0, 480);
        let mut prev = r.value();
        for _ in 0..480 {
            let v = r.next_value();
            assert!(v <= prev);
            prev = v;
        }
        assert_eq!(prev, 0.0);
    }

    #[test]
    fn retarget_mid_ramp_continues_from_current_value() {
        let mut r = LinearRamp::new(0.0);
        r.set_target(1.0, 100);
        for _ in 0..50 {
            r.next_value();
        }
        let mid = r.value();
        r.set_target(0.0, 100);
        let first = r.next_value();
        assert!((first - mid).abs() < 0.02, "jumped from {mid} to {first}");
    }

    #[test]
    fn zero_frames_jumps() {
        let mut r = LinearRamp::new(0.0);
        r.set_target(0.5, 0);
        assert_eq!(r.value(), 0.5);
        assert!(r.is_settled());
    }
}
