//! The audio-thread half of the engine.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use gt_dsp::{db_to_gain, LinearRamp, SineOsc};

use crate::{EngineConfig, Shared, Telemetry, FADE_SECONDS};

/// Frequency of the Step 1 test tone (concert A).
pub const TEST_TONE_HZ: f32 = 440.0;
/// Level of the Step 1 test tone. -12 dBFS leaves headroom and is comfortable on speakers.
pub const TEST_TONE_DBFS: f32 = -12.0;

/// Renders audio. Lives on the audio thread; every method here is real-time safe:
/// no allocation, no locks, no I/O, no panics for any buffer length.
#[derive(Debug)]
pub struct AudioProcessor {
    shared: Arc<Shared>,
    channels: usize,
    osc: SineOsc,
    gain: LinearRamp,
    tone_gain: f32,
    fade_frames: u32,
    tone_on: bool,
}

impl AudioProcessor {
    pub(crate) fn new(config: EngineConfig, shared: Arc<Shared>) -> Self {
        let sr = config.sample_rate.max(1) as f32;
        Self {
            shared,
            channels: config.out_channels.max(1),
            osc: SineOsc::new(sr, TEST_TONE_HZ),
            gain: LinearRamp::new(0.0),
            tone_gain: db_to_gain(TEST_TONE_DBFS),
            fade_frames: (sr * FADE_SECONDS).round() as u32,
            tone_on: false,
        }
    }

    /// The telemetry this processor publishes (also visible through the `EngineHandle`).
    pub fn telemetry(&self) -> &Telemetry {
        &self.shared.telemetry
    }

    /// Fills `out`, an interleaved buffer of `frames * out_channels` samples. A trailing partial
    /// frame (which a correct device never delivers) is zeroed.
    pub fn process(&mut self, out: &mut [f32]) {
        let ch = self.channels;
        let frames = out.len() / ch;

        // Pick up a start/stop request once per block.
        let want_on = self.shared.control.tone_on.load(Ordering::Relaxed);
        if want_on != self.tone_on {
            self.tone_on = want_on;
            if want_on && self.gain.value() == 0.0 {
                // Start from a zero crossing so the fade-in begins cleanly.
                self.osc.reset();
            }
            let target = if want_on { self.tone_gain } else { 0.0 };
            self.gain.set_target(target, self.fade_frames);
        }

        let mut peak = 0.0_f32;
        if !self.tone_on && self.gain.is_settled() {
            // Stopped and fully faded: output silence without running the oscillator.
            out.fill(0.0);
        } else {
            for frame in out.chunks_exact_mut(ch) {
                let s = self.osc.next_sample() * self.gain.next_value();
                peak = peak.max(s.abs());
                frame.fill(s);
            }
            out[frames * ch..].fill(0.0);
        }

        let t = &self.shared.telemetry;
        t.last_block_frames.store(frames as u32, Ordering::Relaxed);
        t.blocks.fetch_add(1, Ordering::Relaxed);
        t.peak.fetch_max(peak, Ordering::Relaxed);
        let silent = !self.tone_on && self.gain.is_settled();
        t.silent.store(silent, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use crate::{create, AudioProcessor, EngineConfig, EngineHandle};
    use std::sync::atomic::Ordering;

    fn engine(sr: u32, ch: usize) -> (EngineHandle, AudioProcessor) {
        create(EngineConfig {
            sample_rate: sr,
            out_channels: ch,
        })
    }

    #[test]
    fn silent_until_started() {
        let (h, mut p) = engine(48_000, 2);
        let mut buf = vec![1.0; 512];
        p.process(&mut buf);
        assert!(buf.iter().all(|&s| s == 0.0));
        assert!(h.telemetry().silent.load(Ordering::Relaxed));
    }

    #[test]
    fn plays_at_minus_12_dbfs_on_all_channels() {
        let (h, mut p) = engine(48_000, 2);
        h.start_tone();
        let mut buf = vec![0.0; 2 * 48_000];
        p.process(&mut buf);
        // After the fade, peak is the tone gain.
        let peak = buf[2 * 4800..].iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.251_188_64).abs() < 1e-3, "{peak}");
        for f in buf.chunks_exact(2) {
            assert_eq!(f[0], f[1]);
        }
        assert!(!h.telemetry().silent.load(Ordering::Relaxed));
    }

    #[test]
    fn fade_in_and_out_have_no_steps() {
        let (h, mut p) = engine(44_100, 1);
        h.start_tone();
        let mut buf = vec![0.0; 4410];
        p.process(&mut buf);
        h.stop_tone();
        let mut tail = vec![0.0; 4410];
        p.process(&mut tail);
        buf.extend_from_slice(&tail);
        // Largest sample-to-sample change of a 440 Hz sine at amplitude a is about
        // a * 2π * 440 / 44100 ≈ 0.0157; anything far larger would be a click.
        let max_step = buf
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(max_step < 0.02, "{max_step}");
        assert!(tail[tail.len() - 1] == 0.0);
        assert!(h.telemetry().silent.load(Ordering::Relaxed));
    }

    #[test]
    fn odd_block_sizes_and_partial_frames_are_handled() {
        let (h, mut p) = engine(48_000, 2);
        h.start_tone();
        for len in [0, 1, 2, 3, 441 * 2, 1023] {
            let mut buf = vec![9.0; len];
            p.process(&mut buf);
            assert!(buf.iter().all(|s| s.abs() <= 0.26), "len {len}");
        }
        assert_eq!(h.telemetry().last_block_frames.load(Ordering::Relaxed), 511);
    }

    #[test]
    fn telemetry_reports_peak_and_blocks() {
        let (h, mut p) = engine(48_000, 1);
        h.start_tone();
        let mut buf = vec![0.0; 4800];
        p.process(&mut buf);
        p.process(&mut buf);
        let t = h.telemetry();
        assert_eq!(t.blocks.load(Ordering::Relaxed), 2);
        assert!(t.peak.load(Ordering::Relaxed) > 0.2);
    }
}
