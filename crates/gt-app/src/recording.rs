//! The disk writer: turns what the engine records into WAV files (Prompt 13).
//!
//! [`start`] makes the two rings of a [`RecordSink`] and a thread that empties them. The engine
//! pushes interleaved frames of every armed track's input and, in order, [`RecMark`]s that
//! start and end takes and report gaps. The thread writes one 32-bit float WAV file per track
//! and take, inserts silence where the engine could not hand frames over (so every file keeps
//! the song's timing), and reports each finished take on a channel the UI polls.
//!
//! Ordering: frames of a quantum are pushed after the marks that come before them, so the
//! thread first notes how many samples are waiting, then reads the marks, then the samples.
//! A mark it has not seen yet was pushed after those samples and refers to later frames.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use gt_core::{AudioInput, Gap, TrackId};
use gt_engine::{RecMark, RecordSink};
use rtrb::{Consumer, RingBuffer};

/// Seconds of audio the disk ring holds: how long the disk may stall before audio is lost.
const RING_SECONDS: usize = 4;
/// How often the thread looks for work when the rings are empty.
const POLL: Duration = Duration::from_millis(5);

/// One armed track: where its audio comes from and what its files are called.
#[derive(Debug, Clone)]
pub struct Lane {
    pub track: TrackId,
    pub input: AudioInput,
    /// File name start, e.g. the track name.
    pub name: String,
}

/// A finished take: one file per lane.
#[derive(Debug, Clone, PartialEq)]
pub struct TakeDone {
    pub take: u32,
    /// Song tick of the first frame.
    pub tick: i64,
    /// Per lane: the track and its file.
    pub files: Vec<(TrackId, PathBuf)>,
    /// Length in frames (gaps included).
    pub frames: u64,
    pub gaps: Vec<Gap>,
}

/// What the writer tells the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum WriterEvent {
    /// A take began at this song tick.
    Started { take: u32, tick: i64 },
    /// A take was written completely.
    Done(TakeDone),
    /// Writing failed; the take's files may be incomplete.
    Failed(String),
}

/// The UI's end of a recording session.
pub struct Session {
    pub events: Receiver<WriterEvent>,
    thread: Option<JoinHandle<()>>,
}

impl Session {
    /// Events since the last call.
    pub fn poll(&self) -> Vec<WriterEvent> {
        self.events.try_iter().collect()
    }

    /// True once the thread has written everything and stopped (the engine let go of the
    /// sink).
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The thread ends by itself once the engine drops the sink; never wait for it here.
        drop(self.thread.take());
    }
}

/// Starts a writer for `lanes`, writing into `dir` at `sample_rate`. Returns the sink to send
/// to the engine with `EngineCommand::SetRecorder`.
pub fn start(
    dir: &Path,
    lanes: Vec<Lane>,
    sample_rate: u32,
    compensation: u32,
    punch: Option<(i64, i64)>,
) -> std::io::Result<(RecordSink, Session)> {
    std::fs::create_dir_all(dir)?;
    let width: usize = lanes.iter().map(|l| l.input.channels()).sum();
    let (samples, rx_samples) =
        RingBuffer::new((sample_rate as usize * RING_SECONDS * width).max(4096));
    let (marks, rx_marks) = RingBuffer::new(1024);
    let sink = RecordSink {
        samples,
        marks,
        lanes: lanes.iter().map(|l| l.input).collect(),
        compensation,
        punch,
    };
    let (tx, events) = channel();
    let writer = Writer {
        dir: dir.to_path_buf(),
        lanes,
        width,
        sample_rate,
        samples: rx_samples,
        marks: rx_marks,
        tx,
        take: None,
    };
    let thread = std::thread::Builder::new()
        .name("gt-disk-writer".into())
        .spawn(move || writer.run())?;
    Ok((
        sink,
        Session {
            events,
            thread: Some(thread),
        },
    ))
}

struct OpenTake {
    take: u32,
    tick: i64,
    files: Vec<(TrackId, PathBuf)>,
    writers: Vec<hound::WavWriter<BufWriter<File>>>,
    /// Take frames written so far (silence for gaps included).
    frames: u64,
    gaps: Vec<Gap>,
    error: Option<String>,
}

struct Writer {
    dir: PathBuf,
    lanes: Vec<Lane>,
    width: usize,
    sample_rate: u32,
    samples: Consumer<f32>,
    marks: Consumer<RecMark>,
    tx: Sender<WriterEvent>,
    take: Option<OpenTake>,
}

impl Writer {
    fn run(mut self) {
        loop {
            let waiting = self.samples.slots() / self.width.max(1);
            let mut busy = false;
            while let Ok(m) = self.marks.pop() {
                busy = true;
                self.on_mark(m);
            }
            if waiting > 0 {
                busy = true;
                self.copy(waiting as u64);
            }
            if !busy {
                if self.marks.is_abandoned() && self.samples.is_empty() && self.marks.is_empty() {
                    break;
                }
                std::thread::sleep(POLL);
            }
        }
        // The engine let go in the middle of a take (the device was reopened): keep what
        // arrived.
        if let Some(t) = self.take.as_ref() {
            let frames = t.frames;
            self.finish(frames);
        }
    }

    fn on_mark(&mut self, m: RecMark) {
        match m {
            RecMark::Start { take, tick } => {
                if let Some(t) = self.take.as_ref() {
                    let frames = t.frames;
                    self.finish(frames);
                }
                self.open(take, tick);
            }
            RecMark::Gap { at, frames, fill } => {
                self.copy_until(at);
                let sr = f64::from(self.sample_rate.max(1));
                if fill {
                    self.silence(u64::from(frames));
                }
                if let Some(t) = self.take.as_mut() {
                    let at_s = at as f64 / sr;
                    let len_s = f64::from(frames) / sr;
                    // A continuous slip marks every quantum; report it as one gap. Gaps are
                    // adjacent when the next starts where the last ended, within a quantum.
                    let quantum = 64.0 / sr;
                    match t.gaps.last_mut() {
                        Some(last) if at_s - (last.at_s + last.len_s) <= quantum => {
                            last.len_s = (at_s + len_s - last.at_s).max(last.len_s);
                        }
                        _ => t.gaps.push(Gap { at_s, len_s }),
                    }
                }
            }
            RecMark::End { frames } => {
                self.copy_until(frames);
                self.finish(frames);
            }
        }
    }

    fn open(&mut self, take: u32, tick: i64) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: self.sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut t = OpenTake {
            take,
            tick,
            files: Vec::new(),
            writers: Vec::new(),
            frames: 0,
            gaps: Vec::new(),
            error: None,
        };
        for lane in &self.lanes {
            let path = unique_path(&self.dir, &lane.name);
            let spec = hound::WavSpec {
                channels: lane.input.channels() as u16,
                ..spec
            };
            match hound::WavWriter::create(&path, spec) {
                Ok(w) => {
                    t.writers.push(w);
                    t.files.push((lane.track, path));
                }
                Err(e) => {
                    t.error = Some(format!("cannot create {}: {e}", path.display()));
                    break;
                }
            }
        }
        let _ = self.tx.send(WriterEvent::Started { take, tick });
        self.take = Some(t);
    }

    /// Copies waiting frames until the take has `frames` frames (waiting briefly for frames
    /// the engine has pushed but this thread cannot see yet).
    fn copy_until(&mut self, frames: u64) {
        for _ in 0..200 {
            let have = self.take.as_ref().map_or(frames, |t| t.frames);
            if have >= frames {
                return;
            }
            let waiting = (self.samples.slots() / self.width.max(1)) as u64;
            if waiting == 0 {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            self.copy(waiting.min(frames - have));
        }
    }

    /// Moves up to `n` frames from the ring into the files.
    fn copy(&mut self, n: u64) {
        let width = self.width.max(1);
        let Ok(chunk) = self.samples.read_chunk(n as usize * width) else {
            return;
        };
        let (a, b) = chunk.as_slices();
        if let Some(t) = self.take.as_mut() {
            if t.error.is_none() {
                let mut it = a.iter().chain(b);
                'frames: for _ in 0..n {
                    for (w, lane) in t.writers.iter_mut().zip(&self.lanes) {
                        for _ in 0..lane.input.channels() {
                            let Some(&s) = it.next() else { break 'frames };
                            if let Err(e) = w.write_sample(s) {
                                t.error = Some(e.to_string());
                                break 'frames;
                            }
                        }
                    }
                }
            }
            t.frames += n;
        }
        chunk.commit_all();
    }

    fn silence(&mut self, n: u64) {
        if let Some(t) = self.take.as_mut() {
            for (w, lane) in t.writers.iter_mut().zip(&self.lanes) {
                for _ in 0..n * lane.input.channels() as u64 {
                    if let Err(e) = w.write_sample(0.0_f32) {
                        t.error = Some(e.to_string());
                        break;
                    }
                }
            }
            t.frames += n;
        }
    }

    fn finish(&mut self, frames: u64) {
        let Some(mut t) = self.take.take() else {
            return;
        };
        for w in t.writers.drain(..) {
            if let Err(e) = w.finalize() {
                t.error.get_or_insert_with(|| e.to_string());
            }
        }
        let event = match t.error {
            Some(e) => WriterEvent::Failed(e),
            None => WriterEvent::Done(TakeDone {
                take: t.take,
                tick: t.tick,
                files: t.files,
                frames: frames.max(t.frames),
                gaps: t.gaps,
            }),
        };
        let _ = self.tx.send(event);
    }
}

/// `<dir>/<name> take <n>.wav` with the first `n` not in use.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim();
    let base = if clean.is_empty() { "Recording" } else { clean };
    (1..)
        .map(|n| dir.join(format!("{base} take {n}.wav")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| dir.join(format!("{base}.wav")))
}

/// Where recordings of a project go: `recordings` next to the project file, or in the data
/// folder for a project that was never saved.
pub fn folder(project_path: Option<&Path>, data: &Path) -> PathBuf {
    match project_path.and_then(Path::parent) {
        Some(dir) => dir.join("recordings"),
        None => data.join("recordings"),
    }
}

/// Copies the recordings `project` uses from `from` (the data folder's recordings, where an
/// unsaved project records) into `to` (next to the project file) and points the project at
/// the copies. The originals stay, so undo steps from before the save still find their files.
/// Returns how many files were copied.
pub fn adopt(project: &mut gt_core::Project, from: &Path, to: &Path) -> std::io::Result<usize> {
    if from == to {
        return Ok(0);
    }
    let mine: Vec<PathBuf> = gt_project::file::sample_files(project)
        .into_iter()
        .filter(|p| p.parent() == Some(from))
        .collect();
    if mine.is_empty() {
        return Ok(0);
    }
    std::fs::create_dir_all(to)?;
    for old in &mine {
        let name = old.file_name().unwrap_or_default().to_string_lossy();
        let (stem, ext) = name.rsplit_once('.').unwrap_or((&name, "wav"));
        let mut new = to.join(&*name);
        let mut n = 2;
        while new.exists() {
            new = to.join(format!("{stem} ({n}).{ext}"));
            n += 1;
        }
        std::fs::copy(old, &new)?;
        gt_project::file::relink(project, old, &new);
    }
    Ok(mine.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtrb::Producer;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gt-rec-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn lanes() -> Vec<Lane> {
        vec![
            Lane {
                track: TrackId(1),
                input: AudioInput::mono(0),
                name: "Vox".into(),
            },
            Lane {
                track: TrackId(2),
                input: AudioInput::stereo(1),
                name: "Gtr/DI".into(),
            },
        ]
    }

    fn read(path: &Path) -> (u16, Vec<f32>) {
        let mut r = hound::WavReader::open(path).unwrap();
        let spec = r.spec();
        assert_eq!(spec.sample_format, hound::SampleFormat::Float);
        assert_eq!(spec.bits_per_sample, 32);
        let v = r.samples::<f32>().map(Result::unwrap).collect();
        (spec.channels, v)
    }

    /// The value of channel `c` at take frame `k`: unique, so misplaced samples show.
    fn value(k: u64, c: usize) -> f32 {
        (k as f32) * 0.001 + c as f32 * 0.25
    }

    /// Pushes like the engine: frames when they fit, a filled gap when they do not.
    fn push_quantum(
        samples: &mut Producer<f32>,
        marks: &mut Producer<RecMark>,
        frame: u64,
        n: u64,
    ) {
        let needed = n as usize * 3;
        match samples.write_chunk_uninit(needed) {
            Ok(chunk) => {
                chunk.fill_from_iter(
                    (frame..frame + n).flat_map(|k| (0..3).map(move |c| value(k, c))),
                );
            }
            Err(_) => marks
                .push(RecMark::Gap {
                    at: frame,
                    frames: n as u32,
                    fill: true,
                })
                .unwrap(),
        }
    }

    fn wait_done(session: &Session) -> Vec<TakeDone> {
        let mut done = Vec::new();
        for _ in 0..2000 {
            for e in session.poll() {
                match e {
                    WriterEvent::Done(t) => done.push(t),
                    WriterEvent::Failed(e) => panic!("{e}"),
                    WriterEvent::Started { .. } => {}
                }
            }
            if session.finished() {
                done.extend(session.poll().into_iter().filter_map(|e| match e {
                    WriterEvent::Done(t) => Some(t),
                    _ => None,
                }));
                return done;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the writer did not finish");
    }

    /// Checks a finished take's files: every frame holds its value, or silence inside a gap.
    fn check_take(t: &TakeDone) {
        let (ch_a, a) = read(&t.files[0].1);
        let (ch_b, b) = read(&t.files[1].1);
        assert_eq!((ch_a, ch_b), (1, 2));
        assert_eq!(a.len() as u64, t.frames);
        assert_eq!(b.len() as u64, 2 * t.frames);
        let sr = 48_000.0;
        let in_gap = |k: u64| {
            t.gaps.iter().any(|g| {
                (k as f64) >= g.at_s * sr - 0.5 && (k as f64) < (g.at_s + g.len_s) * sr - 0.5
            })
        };
        for k in 0..t.frames {
            let want = |c| if in_gap(k) { 0.0 } else { value(k, c) };
            assert_eq!(a[k as usize], want(0), "frame {k}");
            assert_eq!(b[2 * k as usize], want(1), "frame {k}");
            assert_eq!(b[2 * k as usize + 1], want(2), "frame {k}");
        }
    }

    #[test]
    fn writes_every_frame_in_order_while_the_engine_keeps_pushing() {
        let dir = temp_dir("load");
        let (mut sink, session) = start(&dir, lanes(), 48_000, 0, None).unwrap();
        assert_eq!(sink.frame_width(), 3);
        // Like real time but 50 times faster: 2 minutes of three channels in a few seconds,
        // in 32-frame quanta, from another thread. Two takes.
        let pusher = std::thread::spawn(move || {
            let mut total = Vec::new();
            for take in 0..2 {
                sink.marks
                    .push(RecMark::Start {
                        take,
                        tick: 960 * i64::from(take),
                    })
                    .unwrap();
                let frames = 48_000 * 60_u64;
                let mut f = 0;
                while f < frames {
                    push_quantum(&mut sink.samples, &mut sink.marks, f, 32);
                    f += 32;
                    if f % (32 * 50) == 0 {
                        std::thread::sleep(Duration::from_micros(660));
                    }
                }
                sink.marks.push(RecMark::End { frames: f }).unwrap();
                total.push(f);
            }
            total
        });
        let lengths = pusher.join().unwrap();
        let done = wait_done(&session);
        assert_eq!(done.len(), 2);
        for (t, len) in done.iter().zip(lengths) {
            assert_eq!(t.frames, len);
            check_take(t);
        }
        assert_eq!(done[1].tick, 960);
        assert!(done[0].files[0].1.ends_with("Vox take 1.wav"));
        assert!(done[1].files[1].1.ends_with("Gtr_DI take 2.wav"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_stalled_disk_becomes_silence_at_the_right_place() {
        let dir = temp_dir("stall");
        let (sink, session) = start(&dir, lanes(), 48_000, 0, None).unwrap();
        // Replace the sink's rings with tiny ones so the writer falls behind at once.
        let (mut samples, rx) = RingBuffer::new(32 * 3 * 4);
        let (mut marks, rx_marks) = RingBuffer::new(4096);
        let (tx, events) = channel();
        let writer = Writer {
            dir: dir.clone(),
            lanes: lanes(),
            width: 3,
            sample_rate: 48_000,
            samples: rx,
            marks: rx_marks,
            tx,
            take: None,
        };
        drop((sink, session));
        marks.push(RecMark::Start { take: 0, tick: 0 }).unwrap();
        // The writer is not running yet: everything after four quanta becomes a gap.
        let mut f = 0;
        while f < 32 * 100 {
            push_quantum(&mut samples, &mut marks, f, 32);
            f += 32;
        }
        let thread = std::thread::spawn(move || writer.run());
        std::thread::sleep(Duration::from_millis(50));
        while f < 32 * 200 {
            push_quantum(&mut samples, &mut marks, f, 32);
            f += 32;
            std::thread::sleep(Duration::from_micros(200));
        }
        marks.push(RecMark::End { frames: f }).unwrap();
        drop((samples, marks));
        thread.join().unwrap();
        let done: Vec<TakeDone> = events
            .try_iter()
            .filter_map(|e| match e {
                WriterEvent::Done(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(done.len(), 1);
        let t = &done[0];
        assert_eq!(t.frames, f);
        assert!(!t.gaps.is_empty());
        assert!((t.gaps[0].at_s * 48_000.0 - 128.0).abs() < 1e-6);
        check_take(t);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn names_never_overwrite_and_folders_follow_the_project() {
        let dir = temp_dir("names");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Bass take 1.wav"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "Bass"), dir.join("Bass take 2.wav"));
        assert_eq!(unique_path(&dir, "  "), dir.join("Recording take 1.wav"));
        let data = Path::new("/data");
        assert_eq!(
            folder(Some(Path::new("/songs/a.gloom")), data),
            Path::new("/songs/recordings")
        );
        assert_eq!(folder(None, data), Path::new("/data/recordings"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_copies_the_recordings_next_to_the_project() {
        use gt_core::{AudioTake, ClipKind, Project, SampleSource};
        let dir = temp_dir("adopt");
        let data = dir.join("data/recordings");
        let song = dir.join("song/recordings");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&song).unwrap();
        let (a, b) = (data.join("Vox take 1.wav"), data.join("Vox take 2.wav"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        std::fs::write(song.join("Vox take 1.wav"), b"older").unwrap();
        let mut p = Project::empty();
        let t = p.playlist.tracks[0].id;
        p.playlist.add_clip(
            t,
            0,
            960,
            ClipKind::Audio {
                source: SampleSource::File(a.clone()),
                gain: 1.0,
                gaps: Vec::new(),
                takes: vec![AudioTake {
                    source: SampleSource::File(b.clone()),
                    gaps: Vec::new(),
                    length: 960,
                }],
            },
        );
        assert_eq!(adopt(&mut p, &data, &song).unwrap(), 2);
        let files = gt_project::file::sample_files(&p);
        assert_eq!(
            files,
            vec![song.join("Vox take 1 (2).wav"), song.join("Vox take 2.wav")],
            "an existing file is never overwritten"
        );
        assert_eq!(std::fs::read(&files[0]).unwrap(), b"a");
        assert!(a.exists(), "the originals stay for undo");
        assert_eq!(
            adopt(&mut p, &data, &song).unwrap(),
            0,
            "nothing left to copy"
        );
    }
}
