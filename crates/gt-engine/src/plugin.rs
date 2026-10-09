//! Hosted plugins on the audio thread (Step 11, ARCHITECTURE.md §2.6).
//!
//! The engine knows plugins only through [`PluginProcessor`], implemented by `gt-plugin-host`
//! for CLAP, so this crate stays free of any plugin API. Running plugins live in one table of
//! [`MAX_PLUGINS`] entries, each tagged with the document's [`PluginInstanceId`]; channels and
//! effect slots name the instance they use, and the engine looks it up when settings change.
//! Moving a channel or an effect therefore never restarts its plugin.
//!
//! A plugin that reports an error or produces a non-finite sample is bypassed from then on (an
//! effect passes its input through, an instrument goes silent) and flagged in
//! [`crate::Telemetry::plugin_failed`], so one misbehaving plugin cannot poison the mix.

use std::sync::atomic::Ordering;

use gt_core::PluginInstanceId;

use crate::Telemetry;

/// Most plugins running at once.
pub const MAX_PLUGINS: usize = 128;

/// Musical context of a block, for tempo-synced plugins.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PluginContext {
    /// Tempo at the start of the block.
    pub bpm: f64,
    /// The transport is playing.
    pub playing: bool,
    /// Song position at the start of the block, in quarter notes.
    pub beats: f64,
    /// Start of the current bar, in quarter notes.
    pub bar_start_beats: f64,
    /// Index of the current bar (0 for bar 1).
    pub bar: i32,
    /// Time signature.
    pub sig_num: u16,
    /// Time signature denominator.
    pub sig_den: u16,
    /// Frames rendered by the engine before this block (a steady clock).
    pub steady_frames: u64,
}

impl Default for PluginContext {
    fn default() -> Self {
        Self {
            bpm: 120.0,
            playing: false,
            beats: 0.0,
            bar_start_beats: 0.0,
            bar: 0,
            sig_num: 4,
            sig_den: 4,
            steady_frames: 0,
        }
    }
}

/// A plugin as the audio thread drives it. Every method must be real-time safe in the
/// implementation (no allocation, locks or I/O); parameter changes and notes are queued and
/// delivered with the next `process`, at their frame offsets.
pub trait PluginProcessor: Send {
    /// Processes one block (at most [`crate::RENDER_QUANTUM`] frames) in place: `l` and `r`
    /// hold the input for an effect and silence for an instrument, and receive the output.
    /// Returns false if the plugin reported an error.
    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &PluginContext) -> bool;
    /// Queues a note start at frame `offset` of the next block.
    fn note_on(&mut self, offset: u32, key: u8, velocity: f32);
    /// Queues a note release.
    fn note_off(&mut self, offset: u32, key: u8);
    /// Queues releases of every held note.
    fn release_all(&mut self, offset: u32);
    /// Queues a parameter change: `index` in the plugin's parameter list, value normalized.
    fn set_param(&mut self, index: u32, value: f32);
    /// Clears tails and voices (the transport started from stop, or a retry after a failure).
    fn reset(&mut self);
    /// Delay the plugin adds, in frames (for delay compensation).
    fn latency(&self) -> usize;
    /// Called on the audio thread right before the processor is handed back for freeing, so
    /// the plugin hears "stop processing" on the thread that processed it.
    fn stop(&mut self) {}
}

/// A processor and the instance it belongs to. Built on the UI thread, sent with
/// `EngineCommand::SetPlugin`, handed back as garbage.
pub struct PluginBox {
    /// The document's id for the instance.
    pub instance: PluginInstanceId,
    /// The processor.
    pub processor: Box<dyn PluginProcessor>,
}

impl std::fmt::Debug for PluginBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PluginBox({:?})", self.instance)
    }
}

struct Entry {
    plugin: Box<PluginBox>,
    failed: bool,
}

/// The running plugins.
pub(crate) struct PluginTable {
    entries: Vec<Option<Entry>>,
    /// Copy of an effect's input, restored when the effect fails mid-block.
    dry_l: [f32; crate::RENDER_QUANTUM],
    dry_r: [f32; crate::RENDER_QUANTUM],
}

impl PluginTable {
    pub(crate) fn new() -> Self {
        Self {
            entries: (0..MAX_PLUGINS).map(|_| None).collect(),
            dry_l: [0.0; crate::RENDER_QUANTUM],
            dry_r: [0.0; crate::RENDER_QUANTUM],
        }
    }

    /// Index of the entry running `id`.
    pub(crate) fn find(&self, id: PluginInstanceId) -> Option<u8> {
        self.entries
            .iter()
            .position(|e| e.as_ref().is_some_and(|e| e.plugin.instance == id))
            .map(|i| i as u8)
    }

    /// Puts a plugin into entry `index` (or empties it) and returns the previous one, stopped.
    pub(crate) fn set(
        &mut self,
        index: usize,
        plugin: Option<Box<PluginBox>>,
        telemetry: &Telemetry,
    ) -> Option<Box<PluginBox>> {
        let slot = self.entries.get_mut(index)?;
        let old = std::mem::replace(
            slot,
            plugin.map(|plugin| Entry {
                plugin,
                failed: false,
            }),
        );
        telemetry.plugin_failed[index].store(false, Ordering::Relaxed);
        old.map(|mut e| {
            e.plugin.processor.stop();
            e.plugin
        })
    }

    /// Clears a failure so the plugin runs again (after a reset).
    pub(crate) fn retry(&mut self, index: usize, telemetry: &Telemetry) {
        if let Some(Some(e)) = self.entries.get_mut(index) {
            e.failed = false;
            e.plugin.processor.reset();
            telemetry.plugin_failed[index].store(false, Ordering::Relaxed);
        }
    }

    fn live(&mut self, index: Option<u8>) -> Option<&mut Entry> {
        self.entries
            .get_mut(usize::from(index?))?
            .as_mut()
            .filter(|e| !e.failed)
    }

    pub(crate) fn note_on(&mut self, index: Option<u8>, offset: u32, key: u8, velocity: f32) {
        if let Some(e) = self.live(index) {
            e.plugin.processor.note_on(offset, key, velocity);
        }
    }

    pub(crate) fn note_off(&mut self, index: Option<u8>, offset: u32, key: u8) {
        if let Some(e) = self.live(index) {
            e.plugin.processor.note_off(offset, key);
        }
    }

    pub(crate) fn release_all(&mut self, index: Option<u8>, offset: u32) {
        if let Some(e) = self.live(index) {
            e.plugin.processor.release_all(offset);
        }
    }

    /// Sets a parameter of the plugin running `id`.
    pub(crate) fn set_param(&mut self, id: PluginInstanceId, param: u32, value: f32) {
        let i = self.find(id);
        if let Some(e) = self.live(i) {
            e.plugin.processor.set_param(param, value);
        }
    }

    /// Sets a parameter by entry index.
    pub(crate) fn set_param_at(&mut self, index: u8, param: u32, value: f32) {
        if let Some(e) = self.live(Some(index)) {
            e.plugin.processor.set_param(param, value);
        }
    }

    /// Resets every plugin (playback started from stop).
    pub(crate) fn reset_all(&mut self) {
        for e in self.entries.iter_mut().flatten().filter(|e| !e.failed) {
            e.plugin.processor.reset();
        }
    }

    /// Latency of an entry, 0 if empty or bypassed.
    pub(crate) fn latency(&self, index: Option<u8>) -> usize {
        index
            .and_then(|i| self.entries.get(usize::from(i))?.as_ref())
            .filter(|e| !e.failed)
            .map_or(0, |e| e.plugin.processor.latency())
    }

    /// True if entry `index` holds a plugin that is running.
    pub(crate) fn is_live(&self, index: Option<u8>) -> bool {
        index
            .and_then(|i| self.entries.get(usize::from(i))?.as_ref())
            .is_some_and(|e| !e.failed)
    }

    /// Runs an instrument: `l` and `r` get its output (silence if it is missing or failed).
    pub(crate) fn run_instrument(
        &mut self,
        index: Option<u8>,
        l: &mut [f32],
        r: &mut [f32],
        ctx: &PluginContext,
        telemetry: &Telemetry,
    ) {
        l.fill(0.0);
        r.fill(0.0);
        let Some(i) = index else {
            return;
        };
        let Some(e) = self.live(index) else {
            return;
        };
        if !run_checked(e, l, r, ctx) {
            l.fill(0.0);
            r.fill(0.0);
            telemetry.plugin_failed[usize::from(i)].store(true, Ordering::Relaxed);
        }
    }

    /// Runs an effect in place; a missing or failed plugin passes the input through.
    pub(crate) fn run_effect(
        &mut self,
        index: Option<u8>,
        l: &mut [f32],
        r: &mut [f32],
        ctx: &PluginContext,
        telemetry: &Telemetry,
    ) {
        let n = l.len().min(r.len()).min(self.dry_l.len());
        let Some(i) = index else {
            return;
        };
        let (dry_l, dry_r) = (&mut self.dry_l, &mut self.dry_r);
        let Some(Some(e)) = self.entries.get_mut(usize::from(i)) else {
            return;
        };
        if e.failed {
            return;
        }
        dry_l[..n].copy_from_slice(&l[..n]);
        dry_r[..n].copy_from_slice(&r[..n]);
        if !run_checked(e, &mut l[..n], &mut r[..n], ctx) {
            l[..n].copy_from_slice(&dry_l[..n]);
            r[..n].copy_from_slice(&dry_r[..n]);
            telemetry.plugin_failed[usize::from(i)].store(true, Ordering::Relaxed);
        }
    }
}

/// Processes and checks the output; marks the entry failed on an error or a non-finite sample.
fn run_checked(e: &mut Entry, l: &mut [f32], r: &mut [f32], ctx: &PluginContext) -> bool {
    let ok =
        e.plugin.processor.process(l, r, ctx) && l.iter().chain(r.iter()).all(|x| x.is_finite());
    if !ok {
        e.failed = true;
    }
    ok
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A test plugin: an effect that scales by its parameter 0 (an instrument outputs a
    /// constant while a note is held), or misbehaves on demand.
    pub(crate) struct FakePlugin {
        pub gain: f32,
        pub held: u32,
        pub nan: bool,
        pub latency: usize,
    }

    impl PluginProcessor for FakePlugin {
        fn process(&mut self, l: &mut [f32], r: &mut [f32], _ctx: &PluginContext) -> bool {
            for x in l.iter_mut().chain(r.iter_mut()) {
                *x = if self.nan {
                    f32::NAN
                } else if self.held > 0 {
                    *x + 0.5 * self.gain
                } else {
                    *x * self.gain
                };
            }
            true
        }
        fn note_on(&mut self, _offset: u32, _key: u8, _velocity: f32) {
            self.held += 1;
        }
        fn note_off(&mut self, _offset: u32, _key: u8) {
            self.held = self.held.saturating_sub(1);
        }
        fn release_all(&mut self, _offset: u32) {
            self.held = 0;
        }
        fn set_param(&mut self, index: u32, value: f32) {
            if index == 0 {
                self.gain = value;
            } else {
                self.nan = value > 0.5;
            }
        }
        fn reset(&mut self) {
            self.held = 0;
        }
        fn latency(&self) -> usize {
            self.latency
        }
    }

    pub(crate) fn fake(id: PluginInstanceId, gain: f32) -> Box<PluginBox> {
        Box::new(PluginBox {
            instance: id,
            processor: Box::new(FakePlugin {
                gain,
                held: 0,
                nan: false,
                latency: 0,
            }),
        })
    }

    #[test]
    fn a_failing_effect_is_bypassed_and_flagged() {
        let t = Telemetry::default();
        let mut table = PluginTable::new();
        let id = PluginInstanceId(42);
        assert!(table.set(3, Some(fake(id, 0.5)), &t).is_none());
        let i = table.find(id);
        assert_eq!(i, Some(3));
        let ctx = PluginContext::default();
        let (mut l, mut r) = ([1.0; 32], [1.0; 32]);
        table.run_effect(i, &mut l, &mut r, &ctx, &t);
        assert_eq!(l[0], 0.5);
        table.set_param(id, 1, 1.0);
        let (mut l, mut r) = ([1.0; 32], [1.0; 32]);
        table.run_effect(i, &mut l, &mut r, &ctx, &t);
        assert_eq!(l[5], 1.0, "the input passes through");
        assert!(t.plugin_failed[3].load(Ordering::Relaxed));
        assert!(!table.is_live(i));
        table.set_param_at(3, 1, 0.0); // ignored while failed
        table.retry(3, &t);
        assert!(!t.plugin_failed[3].load(Ordering::Relaxed));
        let old = table.set(3, None, &t).unwrap();
        assert_eq!(old.instance, id);
        assert_eq!(table.find(id), None);
    }

    #[test]
    fn instruments_get_notes_and_render_silence_when_missing() {
        let t = Telemetry::default();
        let mut table = PluginTable::new();
        let id = PluginInstanceId(7);
        table.set(0, Some(fake(id, 1.0)), &t);
        let ctx = PluginContext::default();
        let (mut l, mut r) = ([9.0; 32], [9.0; 32]);
        table.note_on(Some(0), 0, 60, 1.0);
        table.run_instrument(Some(0), &mut l, &mut r, &ctx, &t);
        assert_eq!(l[0], 0.5);
        table.release_all(Some(0), 0);
        table.run_instrument(Some(0), &mut l, &mut r, &ctx, &t);
        assert_eq!(l[0], 0.0);
        let (mut l, mut r) = ([9.0; 32], [9.0; 32]);
        table.run_instrument(None, &mut l, &mut r, &ctx, &t);
        assert!(l.iter().all(|&x| x == 0.0));
    }
}
