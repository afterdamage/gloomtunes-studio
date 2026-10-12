//! Audio input in the engine (Prompt 13): latency calibration through a simulated loopback
//! cable, latency-compensated recording that lands sample-exactly on the song, monitoring,
//! loop passes and punch-ins as takes, and the gap reports when the input or the disk writer
//! falls behind.

use gt_core::{AudioInput, MonitorMode, TempoMap, Tick};
use gt_engine::{
    create, AudioProcessor, CalibrationSink, EngineCommand, EngineConfig, EngineHandle, InputFeed,
    InputRoute, InputRoutes, LoopRegion, RecMark, RecordSink,
};
use rtrb::{Consumer, Producer, RingBuffer};

const SR: u32 = 48_000;
const BLOCK: usize = 256;

/// An engine whose input hears its own left output `cable` frames later, like a loopback
/// cable on an interface whose input callback runs right after the output callback.
struct Loopback {
    h: EngineHandle,
    p: AudioProcessor,
    input: Producer<f32>,
    /// Everything the engine has output, left channel.
    out: Vec<f32>,
    cable: usize,
    /// Input frames pushed so far.
    pushed: usize,
    /// When false the input stops delivering (an input xrun).
    deliver: bool,
}

impl Loopback {
    fn new(cable: usize) -> Self {
        let (mut h, p) = create(EngineConfig {
            sample_rate: SR,
            out_channels: 2,
        });
        let (input, samples) = RingBuffer::new(1 << 16);
        h.send(EngineCommand::SetInput(Some(Box::new(InputFeed {
            samples,
            channels: 1,
            prime_frames: BLOCK,
            max_frames: 4 * BLOCK,
        }))))
        .unwrap();
        Self {
            h,
            p,
            input,
            out: Vec::new(),
            cable,
            pushed: 0,
            deliver: true,
        }
    }

    /// One output callback, then one input callback.
    fn block(&mut self) {
        let mut buf = vec![0.0_f32; BLOCK * 2];
        self.p.process(&mut buf);
        self.out.extend(buf.chunks(2).map(|f| f[0]));
        if !self.deliver {
            return;
        }
        let end = self.out.len();
        while self.pushed < end {
            let s = self
                .pushed
                .checked_sub(self.cable)
                .map_or(0.0, |i| self.out[i]);
            self.input.push(s).unwrap();
            self.pushed += 1;
        }
    }

    fn run(&mut self, frames: usize) {
        for _ in 0..frames.div_ceil(BLOCK) {
            self.block();
            self.h.collect_garbage();
        }
    }
}

fn drain<T>(c: &mut Consumer<T>) -> Vec<T> {
    std::iter::from_fn(|| c.pop().ok()).collect()
}

/// Measures the round trip like the app's Calibrate button.
fn calibrate(lb: &mut Loopback) -> u32 {
    let (samples, mut captured) = RingBuffer::new(1 << 18);
    lb.h.send(EngineCommand::Calibrate(Some(Box::new(CalibrationSink {
        samples,
        channel: 0,
    }))))
    .unwrap();
    lb.run(gt_dsp::latency::total_frames(SR as f32) + 4 * BLOCK);
    lb.h.send(EngineCommand::Calibrate(None)).unwrap();
    let cap = drain(&mut captured);
    gt_dsp::latency::measure_delay(&cap, SR as f32).expect("the loopback is heard")
}

#[test]
fn calibration_measures_the_round_trip() {
    let mut a = Loopback::new(100);
    a.run(8 * BLOCK);
    let mut b = Loopback::new(1100);
    b.run(8 * BLOCK);
    let (da, db) = (calibrate(&mut a), calibrate(&mut b));
    // The cable's extra 1000 frames show up exactly; the rest is the engine's own cushion.
    assert_eq!(db - da, 1000);
    assert!(da >= 100 && da < 100 + 4 * BLOCK as u32, "{da}");
}

#[test]
fn a_recorded_click_lands_on_its_beat_after_compensation() {
    for cable in [0, 333, 2048] {
        let mut lb = Loopback::new(cable);
        lb.h.send(EngineCommand::SetTempoMap(Box::new(TempoMap::constant(
            137.0,
        ))))
        .unwrap();
        lb.run(8 * BLOCK);
        let d = calibrate(&mut lb);

        let (samples, mut rec) = RingBuffer::new(1 << 20);
        let (marks, mut rec_marks) = RingBuffer::new(64);
        lb.h.send(EngineCommand::SetRecorder(Some(Box::new(RecordSink {
            samples,
            marks,
            lanes: vec![AudioInput::mono(0)],
            compensation: d,
            punch: None,
        }))))
        .unwrap();
        // Start from an odd position: the take begins on the first whole tick after it.
        lb.h.send(EngineCommand::Locate(Tick(1234))).unwrap();
        lb.h.send(EngineCommand::SetMetronome(true)).unwrap();
        lb.run(BLOCK);
        let play_frame = lb.out.len();
        lb.h.send(EngineCommand::Play).unwrap();
        lb.run(SR as usize * 2);
        lb.h.send(EngineCommand::Stop).unwrap();
        lb.run(4 * BLOCK);

        let marks = drain(&mut rec_marks);
        let audio = drain(&mut rec);
        assert_eq!(
            marks.first(),
            Some(&RecMark::Start {
                take: 0,
                tick: 1234
            })
        );
        let Some(RecMark::End { frames }) = marks.last() else {
            panic!("no end mark: {marks:?}");
        };
        assert_eq!(*frames as usize, audio.len());
        assert_eq!(marks.len(), 2, "no gaps expected: {marks:?}");
        // The Play command lands on the next quantum boundary at or after `play_frame`.
        let start = play_frame.next_multiple_of(gt_engine::RENDER_QUANTUM);
        // Sample-exact: what the player heard at each frame of the song is what was recorded
        // for that frame. The metronome clicks make this a click-position test.
        let heard = &lb.out[start..start + audio.len()];
        let clicks = heard.iter().filter(|s| s.abs() > 0.01).count();
        assert!(clicks > 1000, "the metronome should be audible");
        assert_eq!(audio, heard, "cable {cable}");
    }
}

#[test]
fn inputs_are_monitored_as_routed() {
    let mut lb = Loopback::new(0);
    lb.run(8 * BLOCK);
    // Feed a constant into the input instead of the loopback.
    lb.deliver = false;
    let level = |lb: &mut Loopback| {
        for _ in 0..4 * BLOCK {
            lb.input.push(0.25).unwrap();
        }
        lb.run(4 * BLOCK);
        lb.out[lb.out.len() - BLOCK..]
            .iter()
            .fold(0.0_f32, |m, s| m.max(s.abs()))
    };
    let routes = |armed: Option<MonitorMode>| {
        EngineCommand::SetInputRoutes(Box::new(InputRoutes {
            routes: vec![InputRoute {
                input: AudioInput::mono(0),
                strip: 3,
                armed,
            }],
        }))
    };
    assert_eq!(level(&mut lb), 0.0, "not routed: silent");
    lb.h.send(routes(None)).unwrap();
    assert!((level(&mut lb) - 0.25).abs() < 1e-3, "insert input");
    lb.h.send(routes(Some(MonitorMode::Off))).unwrap();
    assert_eq!(level(&mut lb), 0.0);
    lb.h.send(routes(Some(MonitorMode::Auto))).unwrap();
    assert!(level(&mut lb) > 0.2, "auto: heard while stopped");
    lb.h.send(EngineCommand::SetMetronome(false)).unwrap();
    lb.h.send(EngineCommand::Play).unwrap();
    assert_eq!(level(&mut lb), 0.0, "auto: not while playing back");
    lb.h.send(routes(Some(MonitorMode::Mixer))).unwrap();
    assert!(level(&mut lb) > 0.2, "through the mixer: always");
}

#[test]
fn gaps_are_reported_and_keep_the_timing() {
    let mut lb = Loopback::new(0);
    lb.run(8 * BLOCK);
    // A ring for only a quarter second that nobody empties: the disk writer is stuck.
    let (samples, mut rec) = RingBuffer::new(SR as usize / 4);
    let (marks, mut rec_marks) = RingBuffer::new(4096);
    lb.h.send(EngineCommand::SetRecorder(Some(Box::new(RecordSink {
        samples,
        marks,
        lanes: vec![AudioInput::mono(0), AudioInput::stereo(0)],
        compensation: 0,
        punch: None,
    }))))
    .unwrap();
    lb.h.send(EngineCommand::Play).unwrap();
    lb.run(SR as usize / 4);
    // The input stops for a while (an xrun), then comes back.
    lb.deliver = false;
    lb.run(8 * BLOCK);
    lb.pushed = lb.out.len();
    lb.deliver = true;
    lb.run(SR as usize / 4);
    lb.h.send(EngineCommand::Stop).unwrap();
    lb.run(4 * BLOCK);

    let marks = drain(&mut rec_marks);
    let audio = drain(&mut rec);
    let mut filled = 0;
    let mut silent = 0;
    let mut end = None;
    for m in &marks {
        match *m {
            RecMark::Gap {
                frames, fill: true, ..
            } => filled += u64::from(frames),
            RecMark::Gap { fill: false, .. } => silent += 1,
            RecMark::End { frames } => end = Some(frames),
            RecMark::Start { .. } => {}
        }
    }
    let end = end.expect("the take ended");
    // Three channels per frame (a mono lane and a stereo lane).
    assert_eq!(audio.len() % 3, 0);
    assert_eq!(
        audio.len() as u64 / 3 + filled,
        end,
        "every frame is written or a gap"
    );
    assert!(filled > 0, "the full ring was reported");
    assert!(silent > 0, "the input xrun was reported");
    assert_eq!(audio.len(), SR as usize / 4 / 3 * 3);
}

/// Records with a measured compensation; returns (frame where playback started, marks, audio).
fn record_with(
    lb: &mut Loopback,
    punch: Option<(i64, i64)>,
    from: Tick,
    seconds: f64,
) -> (usize, Vec<RecMark>, Vec<f32>) {
    lb.h.send(EngineCommand::SetTempoMap(Box::new(TempoMap::constant(
        120.0,
    ))))
    .unwrap();
    lb.run(8 * BLOCK);
    let d = calibrate(lb);
    let (samples, mut rec) = RingBuffer::new(1 << 21);
    let (marks, mut rec_marks) = RingBuffer::new(256);
    lb.h.send(EngineCommand::SetRecorder(Some(Box::new(RecordSink {
        samples,
        marks,
        lanes: vec![AudioInput::mono(0)],
        compensation: d,
        punch,
    }))))
    .unwrap();
    lb.h.send(EngineCommand::Locate(from)).unwrap();
    lb.h.send(EngineCommand::SetMetronome(true)).unwrap();
    lb.run(BLOCK);
    let start = lb.out.len().next_multiple_of(gt_engine::RENDER_QUANTUM);
    lb.h.send(EngineCommand::Play).unwrap();
    lb.run((f64::from(SR) * seconds) as usize);
    lb.h.send(EngineCommand::Stop).unwrap();
    lb.run(4 * BLOCK);
    (start, drain(&mut rec_marks), drain(&mut rec))
}

/// (tick, frames) of each take.
fn takes(marks: &[RecMark]) -> Vec<(i64, u64)> {
    let mut out = Vec::new();
    let mut tick = None;
    for m in marks {
        match *m {
            RecMark::Start { tick: t, .. } => tick = Some(t),
            RecMark::End { frames } => out.push((tick.take().expect("start before end"), frames)),
            RecMark::Gap { .. } => panic!("no gaps expected: {marks:?}"),
        }
    }
    out
}

#[test]
fn loop_passes_become_takes_that_meet_exactly() {
    let mut lb = Loopback::new(333);
    // Loop beats 4 to 8 (2 s at 120 BPM); start two beats before it.
    lb.h.send(EngineCommand::SetLoop(LoopRegion {
        start: Tick(3840),
        end: Tick(7680),
        enabled: true,
    }))
    .unwrap();
    let (start, marks, audio) = record_with(&mut lb, None, Tick(1920), 6.5);
    let takes = takes(&marks);
    let pass = u64::from(SR) * 2;
    assert_eq!(
        takes,
        vec![
            (1920, u64::from(SR) + pass),
            (3840, pass),
            (3840, takes[2].1),
        ],
    );
    assert!(takes[2].1 > pass / 2 && takes[2].1 < pass);
    // With each take's tail, the passes cover what the player heard without a hole or an
    // overlap: laid end to end they are the performance, sample for sample.
    assert_eq!(audio, &lb.out[start..start + audio.len()]);
}

#[test]
fn punch_records_only_between_the_markers() {
    let mut lb = Loopback::new(2048);
    // Punch in at beat 4, out at beat 6 (1 s), playing straight through from bar 1.
    let (start, marks, audio) = record_with(&mut lb, Some((3840, 5760)), Tick(0), 4.0);
    assert_eq!(takes(&marks), vec![(3840, u64::from(SR))]);
    let at = start + 2 * SR as usize;
    assert_eq!(audio, &lb.out[at..at + SR as usize]);
}
