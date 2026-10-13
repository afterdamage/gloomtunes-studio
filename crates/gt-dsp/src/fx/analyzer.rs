//! Analyzer: passes audio through unchanged and copies it to a [`Scope`] for the UI.

use std::sync::Arc;

use super::{Effect, FxContext};
use crate::analysis::Scope;

/// The analyzer's audio side. All analysis (spectrum, oscilloscope, correlation, tuner) runs
/// in the UI on what it reads from [`Effect::scope`]; the audio thread only copies samples.
/// Its parameters (spectrum slope and peak hold) are display settings the UI reads from the
/// slot.
#[derive(Debug)]
pub struct Analyzer {
    scope: Arc<Scope>,
}

impl Analyzer {
    /// A new analyzer with an empty tap.
    pub fn new(sample_rate: f32) -> Self {
        Self {
            scope: Arc::new(Scope::new(sample_rate)),
        }
    }
}

impl Effect for Analyzer {
    fn param_count(&self) -> usize {
        2
    }

    fn set_param(&mut self, _index: usize, _value: f32) {}

    fn reset(&mut self) {}

    fn process(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &FxContext) {
        self.scope.push(l, r);
    }

    fn scope(&self) -> Option<Arc<Scope>> {
        Some(Arc::clone(&self.scope))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_passes_through_and_reaches_the_scope() {
        let mut a = Analyzer::new(48_000.0);
        let scope = a.scope().unwrap();
        let x: Vec<f32> = (0..256).map(|i| (i as f32 * 0.1).sin()).collect();
        let (mut l, mut r) = (x.clone(), x.clone());
        crate::fx::test_util::run(&mut a, &mut l, &mut r, 120.0);
        assert_eq!(l, x);
        let (mut sl, mut sr) = (Vec::new(), Vec::new());
        scope.read(256, &mut sl, &mut sr);
        assert_eq!(sl, x);
    }
}
