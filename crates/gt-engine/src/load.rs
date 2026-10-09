//! CPU load of the audio callback, measured by the device layer around each `process` call.

use std::sync::atomic::Ordering;

use crate::Telemetry;

/// Time constant of the smoothed load shown in the UI, in seconds.
const SMOOTHING_S: f64 = 0.3;

/// Turns callback timings into the load figures in [`Telemetry`]: a smoothed load, the
/// worst callback since the UI last read it, and a count of callbacks that took longer than
/// the audio they produced (each one is very likely an audible dropout).
///
/// Load is time spent rendering divided by the duration of the rendered audio: 1.0 means the
/// callback used its whole budget. Real-time safe: no allocation, no locks, no panics.
#[derive(Debug, Clone)]
pub struct LoadMeter {
    ns_per_frame: f64,
    smoothed: f64,
}

impl LoadMeter {
    /// A meter for a stream at `sample_rate` Hz.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            ns_per_frame: 1e9 / f64::from(sample_rate.max(1)),
            smoothed: 0.0,
        }
    }

    /// Records one callback that took `elapsed_ns` to render `frames` frames and returns its
    /// load.
    pub fn record(&mut self, t: &Telemetry, elapsed_ns: u64, frames: usize) -> f32 {
        if frames == 0 {
            return 0.0;
        }
        let budget_ns = frames as f64 * self.ns_per_frame;
        let load = elapsed_ns as f64 / budget_ns;
        let k = 1.0 - (-budget_ns * 1e-9 / SMOOTHING_S).exp();
        self.smoothed += (load - self.smoothed) * k;
        t.cpu_load.store(self.smoothed as f32, Ordering::Relaxed);
        t.cpu_peak.fetch_max(load as f32, Ordering::Relaxed);
        if load > 1.0 {
            t.overloads.fetch_add(1, Ordering::Relaxed);
        }
        load as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_is_render_time_over_buffer_time() {
        let t = Telemetry::default();
        let mut m = LoadMeter::new(48_000);
        // 480 frames last 10 ms; 2.5 ms of work is 25 %.
        let load = m.record(&t, 2_500_000, 480);
        assert!((load - 0.25).abs() < 1e-6);
        assert!((t.cpu_peak.load(Ordering::Relaxed) - 0.25).abs() < 1e-6);
        assert_eq!(t.overloads.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn smoothed_load_settles_within_a_second() {
        let t = Telemetry::default();
        let mut m = LoadMeter::new(48_000);
        // 256-frame callbacks at 40 % for one second.
        let budget = 256.0 * 1e9 / 48_000.0;
        for _ in 0..(48_000 / 256) {
            m.record(&t, (budget * 0.4) as u64, 256);
        }
        let s = t.cpu_load.load(Ordering::Relaxed);
        assert!((s - 0.4).abs() < 0.02, "smoothed {s}");
    }

    #[test]
    fn late_callbacks_count_as_overloads() {
        let t = Telemetry::default();
        let mut m = LoadMeter::new(44_100);
        m.record(&t, 1_000_000, 441); // 10 ms of audio in 1 ms
        m.record(&t, 11_000_000, 441); // 11 ms: too late
        m.record(&t, 1, 0); // empty callbacks are ignored
        assert_eq!(t.overloads.load(Ordering::Relaxed), 1);
        assert!(t.cpu_peak.load(Ordering::Relaxed) > 1.0);
    }
}
