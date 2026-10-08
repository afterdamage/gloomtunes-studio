//! One-pole parameter smoothing.

/// Glides towards a target with a one-pole low-pass: each sample closes a fixed fraction of the
/// remaining distance, so a 20 ms time constant gets within 1 % in about 92 ms. Unlike a linear
/// ramp it can be retargeted at any time without a corner in the slope.
#[derive(Debug, Clone, Copy)]
pub struct Smooth {
    value: f32,
    target: f32,
    coef: f32,
}

impl Smooth {
    /// Starts settled at `value`, with time constant `ms` at `sample_rate`.
    pub fn new(value: f32, ms: f32, sample_rate: f32) -> Self {
        let samples = (ms * 0.001 * sample_rate).max(1.0);
        Self {
            value,
            target: value,
            coef: 1.0 - (-1.0 / samples).exp(),
        }
    }

    /// Sets the value to glide to.
    #[inline]
    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    /// Jumps to the target.
    #[inline]
    pub fn snap(&mut self) {
        self.value = self.target;
    }

    /// The target.
    #[inline]
    pub fn target(&self) -> f32 {
        self.target
    }

    /// The current value without advancing.
    #[inline]
    pub fn value(&self) -> f32 {
        self.value
    }

    /// Advances one sample and returns the new value.
    #[inline]
    pub fn next_value(&mut self) -> f32 {
        self.value += (self.target - self.value) * self.coef;
        self.value
    }

    /// Advances `n` samples at once (for control-rate updates) and returns the new value.
    #[inline]
    pub fn advance(&mut self, n: usize) -> f32 {
        let keep = (1.0 - self.coef).powi(n as i32);
        self.value = self.target + (self.value - self.target) * keep;
        if (self.value - self.target).abs() <= 1e-6 * self.target.abs().max(1e-3) {
            self.value = self.target;
        }
        self.value
    }

    /// True once the value has reached the target.
    #[inline]
    pub fn is_settled(&self) -> bool {
        self.value == self.target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_sample_and_block_advance_agree() {
        let mut a = Smooth::new(0.0, 20.0, 48_000.0);
        let mut b = a;
        a.set(1.0);
        b.set(1.0);
        for _ in 0..160 {
            a.next_value();
        }
        b.advance(160);
        assert!((a.value() - b.value()).abs() < 1e-4);
        // One time constant (960 samples) closes 63 % of the gap.
        let mut c = Smooth::new(0.0, 20.0, 48_000.0);
        c.set(1.0);
        c.advance(960);
        assert!((c.value() - 0.632).abs() < 0.01, "{}", c.value());
    }
}
