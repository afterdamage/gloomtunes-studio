//! The shared memory between the engine and a bridge child: a file both map, read and written
//! only as 32-bit atomics (so neither side needs locks or `&mut` aliasing).
//!
//! Layout, in 32-bit words: a header ([`Word`]), then [`MAX_EVENTS`] events of four words
//! (kind, frame offset, two arguments), then the left and right audio of one block.

#![allow(unsafe_code)]

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use gt_engine::{PluginContext, RENDER_QUANTUM};
use memmap2::MmapRaw;

/// Header words.
#[derive(Debug, Clone, Copy)]
#[repr(usize)]
pub(super) enum Word {
    Magic = 0,
    /// Bumped by the engine when a block is ready.
    Request = 1,
    /// Set to the request number by the child when the block is done.
    Done = 2,
    /// The child's audio thread should stop.
    Quit = 3,
    Frames = 4,
    /// The plugin's process succeeded (1) or failed (0).
    Ok = 5,
    Events = 6,
    /// 1: reset before the block.
    Reset = 7,
    /// Set by the app when the child died: the engine stops waiting.
    Dead = 8,
    Playing = 9,
    Bar = 10,
    SigNum = 11,
    SigDen = 12,
    /// Four f64 (two words each) and a u64: tempo, beats, bar start, steady frames.
    Bpm = 13,
    Beats = 15,
    BarStart = 17,
    Steady = 19,
}

const MAGIC: u32 = 0x4754_4231; // "GTB1"
const HEADER: usize = 32;
/// Events per block at most (the rest of a block's are dropped).
pub(super) const MAX_EVENTS: usize = 1024;
const EVENTS: usize = HEADER;
const AUDIO: usize = EVENTS + 4 * MAX_EVENTS;
const WORDS: usize = AUDIO + 2 * RENDER_QUANTUM;

/// Kinds of events.
pub(super) mod ev {
    pub const NOTE_ON: u32 = 1;
    pub const NOTE_OFF: u32 = 2;
    pub const RELEASE_ALL: u32 = 3;
    pub const MIDI: u32 = 4;
    pub const KEY_PRESSURE: u32 = 5;
    pub const PARAM: u32 = 6;
    /// Second argument: kind (0 tuning, 1 pressure, 2 timbre) in the high byte, key low.
    pub const NOTE_EXPR: u32 = 7;
}

/// One mapping of the shared file.
pub(super) struct Shm {
    map: MmapRaw,
    path: PathBuf,
    owner: bool,
}

impl std::fmt::Debug for Shm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Shm({})", self.path.display())
    }
}

fn open(path: &Path, create: bool) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(create)
        .open(path)
}

impl Shm {
    /// Creates a new shared file (the app side).
    pub(super) fn create() -> Result<Self, String> {
        use std::sync::atomic::AtomicU64;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "gloomtunes-bridge-{}-{}.shm",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let file = open(&path, true).map_err(|e| format!("{}: {e}", path.display()))?;
        file.set_len((WORDS * 4) as u64)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let map = MmapRaw::map_raw(&file).map_err(|e| e.to_string())?;
        let shm = Self {
            map,
            path,
            owner: true,
        };
        shm.set(Word::Magic, MAGIC);
        Ok(shm)
    }

    /// Maps an existing shared file (the child side).
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        let file = open(path, false).map_err(|e| format!("{}: {e}", path.display()))?;
        let map = MmapRaw::map_raw(&file).map_err(|e| e.to_string())?;
        let shm = Self {
            map,
            path: path.to_owned(),
            owner: false,
        };
        if shm.map.len() < WORDS * 4 || shm.get(Word::Magic) != MAGIC {
            return Err("not a bridge file".to_owned());
        }
        Ok(shm)
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Removes the file once both sides have mapped it (where the system allows that).
    pub(super) fn unlink(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    pub(super) fn word(&self, i: usize) -> &AtomicU32 {
        assert!(i < WORDS);
        // SAFETY: the map is at least WORDS * 4 bytes (checked when mapping), page aligned,
        // and lives as long as `self`; every access to it goes through atomics, so the other
        // process writing at the same time is not a data race.
        unsafe { AtomicU32::from_ptr(self.map.as_mut_ptr().cast::<u32>().add(i)) }
    }

    pub(super) fn get(&self, w: Word) -> u32 {
        self.word(w as usize).load(Ordering::Relaxed)
    }

    pub(super) fn set(&self, w: Word, v: u32) {
        self.word(w as usize).store(v, Ordering::Relaxed);
    }

    fn set_f64(&self, w: Word, v: f64) {
        let b = v.to_bits();
        self.set_u64(w, b);
    }

    fn set_u64(&self, w: Word, b: u64) {
        let i = w as usize;
        self.word(i).store(b as u32, Ordering::Relaxed);
        self.word(i + 1).store((b >> 32) as u32, Ordering::Relaxed);
    }

    fn get_u64(&self, w: Word) -> u64 {
        let i = w as usize;
        u64::from(self.word(i).load(Ordering::Relaxed))
            | u64::from(self.word(i + 1).load(Ordering::Relaxed)) << 32
    }

    fn get_f64(&self, w: Word) -> f64 {
        f64::from_bits(self.get_u64(w))
    }

    pub(super) fn write_event(&self, n: usize, e: [u32; 4]) {
        for (k, v) in e.into_iter().enumerate() {
            self.word(EVENTS + 4 * n + k).store(v, Ordering::Relaxed);
        }
    }

    pub(super) fn event(&self, n: usize) -> [u32; 4] {
        std::array::from_fn(|k| self.word(EVENTS + 4 * n + k).load(Ordering::Relaxed))
    }

    pub(super) fn write_audio(&self, l: &[f32], r: &[f32]) {
        for (i, (a, b)) in l.iter().zip(r).take(RENDER_QUANTUM).enumerate() {
            self.word(AUDIO + i).store(a.to_bits(), Ordering::Relaxed);
            self.word(AUDIO + RENDER_QUANTUM + i)
                .store(b.to_bits(), Ordering::Relaxed);
        }
    }

    pub(super) fn read_audio(&self, l: &mut [f32], r: &mut [f32]) {
        for (i, (a, b)) in l.iter_mut().zip(r).take(RENDER_QUANTUM).enumerate() {
            *a = f32::from_bits(self.word(AUDIO + i).load(Ordering::Relaxed));
            *b = f32::from_bits(
                self.word(AUDIO + RENDER_QUANTUM + i)
                    .load(Ordering::Relaxed),
            );
        }
    }

    pub(super) fn write_context(&self, c: &PluginContext) {
        self.set(Word::Playing, u32::from(c.playing));
        self.set(Word::Bar, c.bar as u32);
        self.set(Word::SigNum, u32::from(c.sig_num));
        self.set(Word::SigDen, u32::from(c.sig_den));
        self.set_f64(Word::Bpm, c.bpm);
        self.set_f64(Word::Beats, c.beats);
        self.set_f64(Word::BarStart, c.bar_start_beats);
        self.set_u64(Word::Steady, c.steady_frames);
    }

    pub(super) fn context(&self) -> PluginContext {
        PluginContext {
            bpm: self.get_f64(Word::Bpm),
            playing: self.get(Word::Playing) != 0,
            beats: self.get_f64(Word::Beats),
            bar_start_beats: self.get_f64(Word::BarStart),
            bar: self.get(Word::Bar) as i32,
            sig_num: self.get(Word::SigNum) as u16,
            sig_den: self.get(Word::SigDen) as u16,
            steady_frames: self.get_u64(Word::Steady),
        }
    }

    /// Publishes request `seq` (after the block's data) and wakes the child.
    pub(super) fn request(&self, seq: u32) {
        self.word(Word::Request as usize)
            .store(seq, Ordering::Release);
        self.wake(Word::Request);
    }

    /// Waits until the request number differs from `seen` (or a little while passed); the
    /// child's audio thread.
    pub(super) fn wait_request(&self, seen: u32, idle: std::time::Duration) {
        let w = self.word(Word::Request as usize);
        for _ in 0..2000 {
            if w.load(Ordering::Acquire) != seen {
                return;
            }
            std::hint::spin_loop();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = idle;
            let ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 20_000_000,
            };
            // SAFETY: a futex wait on a word of the shared map (process-shared futex), with a
            // timeout; it returns at once if the word no longer holds `seen`.
            unsafe {
                libc::syscall(
                    libc::SYS_futex,
                    w.as_ptr(),
                    libc::FUTEX_WAIT,
                    seen,
                    &ts as *const libc::timespec,
                );
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            // No portable cross-process wake-up: yield while blocks keep coming, sleep when
            // the engine has gone quiet.
            if idle < std::time::Duration::from_millis(50) {
                std::thread::yield_now();
            } else {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    /// Wakes a waiter on `w` (Linux futex; elsewhere waiters poll).
    pub(super) fn wake(&self, w: Word) {
        #[cfg(target_os = "linux")]
        // SAFETY: a futex wake on a word of the shared map; it never blocks.
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                self.word(w as usize).as_ptr(),
                libc::FUTEX_WAKE,
                1,
            );
        }
        #[cfg(not(target_os = "linux"))]
        let _ = w;
    }

    pub(super) fn done(&self) -> u32 {
        self.word(Word::Done as usize).load(Ordering::Acquire)
    }

    /// Marks request `seq` done (after the output).
    pub(super) fn finish(&self, seq: u32) {
        self.word(Word::Done as usize).store(seq, Ordering::Release);
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        if self.owner {
            self.unlink();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_see_the_same_words() {
        let a = Shm::create().unwrap();
        let b = Shm::open(a.path()).unwrap();
        let ctx = PluginContext {
            bpm: 133.5,
            playing: true,
            beats: 17.25,
            bar_start_beats: 16.0,
            bar: 4,
            sig_num: 7,
            sig_den: 8,
            steady_frames: 1 << 40,
        };
        a.write_context(&ctx);
        assert_eq!(b.context(), ctx);
        a.write_audio(&[0.5; 32], &[-0.25; 32]);
        let (mut l, mut r) = ([0.0; 32], [0.0; 32]);
        b.read_audio(&mut l, &mut r);
        assert_eq!((l[31], r[0]), (0.5, -0.25));
        a.write_event(3, [ev::NOTE_ON, 7, 60, 1.0f32.to_bits()]);
        assert_eq!(b.event(3), [1, 7, 60, 0x3F80_0000]);
        a.request(5);
        b.wait_request(4, std::time::Duration::ZERO);
        b.finish(5);
        assert_eq!(a.done(), 5);
        // The creator removes the file.
        let path = a.path().to_owned();
        drop(b);
        drop(a);
        assert!(!path.exists());
    }
}
