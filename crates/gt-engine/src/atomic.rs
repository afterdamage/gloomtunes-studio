//! Lock-free `f32` cell.

use std::sync::atomic::{AtomicU32, Ordering};

/// An `f32` stored as its bit pattern in an `AtomicU32`. Loads and stores are wait-free.
#[derive(Debug, Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    /// Creates a cell holding `value`.
    pub fn new(value: f32) -> Self {
        Self(AtomicU32::new(value.to_bits()))
    }

    /// Reads the value.
    #[inline]
    pub fn load(&self, order: Ordering) -> f32 {
        f32::from_bits(self.0.load(order))
    }

    /// Writes the value.
    #[inline]
    pub fn store(&self, value: f32, order: Ordering) {
        self.0.store(value.to_bits(), order);
    }

    /// Atomically replaces the value, returning the previous one.
    #[inline]
    pub fn swap(&self, value: f32, order: Ordering) -> f32 {
        f32::from_bits(self.0.swap(value.to_bits(), order))
    }

    /// Stores `max(current, value)`. Wait-free in practice: a compare-and-swap loop that only
    /// retries if another thread wrote in between.
    #[inline]
    pub fn fetch_max(&self, value: f32, order: Ordering) {
        let mut current = self.0.load(Ordering::Relaxed);
        while value > f32::from_bits(current) {
            match self
                .0
                .compare_exchange_weak(current, value.to_bits(), order, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_maxes() {
        let a = AtomicF32::new(0.25);
        assert_eq!(a.load(Ordering::Relaxed), 0.25);
        a.fetch_max(0.1, Ordering::Relaxed);
        assert_eq!(a.load(Ordering::Relaxed), 0.25);
        a.fetch_max(0.5, Ordering::Relaxed);
        assert_eq!(a.swap(0.0, Ordering::Relaxed), 0.5);
        assert_eq!(a.load(Ordering::Relaxed), 0.0);
    }
}
