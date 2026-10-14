//! Render to audio clip, consolidate and freeze (Prompt 14): each result must play back like
//! the clips it came from.

use std::collections::HashMap;
use std::sync::Arc;

use gt_core::effects::EffectKind;
use gt_core::{
    AudioEdit, BuiltInSample, ClipKind, EffectSlot, Project, SampleData, SampleSource, TrackId,
    MASTER,
};
use gt_export::bounce::{self, Bounce};
use gt_export::{load_samples, render, BitDepth, ExportSettings, Progress, Range};

const SR: u32 = 48_000;

/// A drum pattern and an audio clip on track 0, hats on track 1, reverb on the master (so a
/// freeze at the master must join before the master's effects).
fn song(track_insert: usize) -> Project {
    let mut p = Project::empty();
    let kick = p
        .add_channel("Kick", Some(SampleSource::BuiltIn(BuiltInSample::Kick)))
        .unwrap();
    let hat = p
        .add_channel("Hat", Some(SampleSource::BuiltIn(BuiltInSample::Hat)))
        .unwrap();
    for s in [0, 4, 8, 12] {
        p.current_pattern_mut().toggle_step(kick, s);
    }
    let drums = p.current_pattern;
    let hats = p.new_pattern();
    p.select_pattern(hats);
    for s in [2, 6, 10, 14] {
        p.current_pattern_mut().toggle_step(hat, s);
    }
    p.playlist.add_track();
    let (t0, t1) = (p.playlist.tracks[0].id, p.playlist.tracks[1].id);
    p.playlist
        .add_clip(t0, 0, 2 * 3840, ClipKind::Pattern(drums));
    p.playlist
        .add_clip(t1, 0, 2 * 3840, ClipKind::Pattern(hats));
    p.playlist.add_clip(
        t0,
        3840 + 480,
        1920,
        ClipKind::Audio {
            source: SampleSource::BuiltIn(BuiltInSample::Snare),
            gain: 0.8,
            gaps: Vec::new(),
            takes: Vec::new(),
            edit: AudioEdit::default(),
        },
    );
    p.mixer.strips[MASTER].slots[0] = Some(EffectSlot::new(EffectKind::Reverb));
    if track_insert != MASTER {
        p.playlist.tracks[0].insert = track_insert;
        p.mixer.strips[track_insert].slots[0] = Some(EffectSlot::new(EffectKind::Delay));
        // The kick channel plays into the track's insert.
        p.channels[0].insert = track_insert;
    }
    p
}

fn mix(p: &Project, samples: &HashMap<SampleSource, Arc<SampleData>>) -> Vec<f32> {
    let settings = ExportSettings {
        sample_rate: SR,
        depth: BitDepth::Float32,
        dither: false,
        normalize_db: None,
        range: Range::Song,
        tail: true,
        max_tail_seconds: 6.0,
        stems: false,
        ..ExportSettings::default()
    };
    let mut out = render(p, samples, &settings, &Progress::default(), &mut Vec::new()).unwrap();
    out.remove(0).audio
}

fn bounce_of(
    p: &Project,
    samples: &HashMap<SampleSource, Arc<SampleData>>,
    b: &Bounce,
) -> SampleData {
    let audio = bounce::render(p, samples, b, SR, &Progress::default(), &mut Vec::new()).unwrap();
    stereo(audio)
}

fn stereo(audio: Vec<f32>) -> SampleData {
    let (l, r) = audio.chunks_exact(2).map(|f| (f[0], f[1])).unzip();
    SampleData {
        sample_rate: SR,
        channels: vec![l, r],
    }
}

/// Largest difference in dBFS (both renders padded with silence to the same length).
fn diff_db(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().max(b.len());
    let at = |v: &[f32], i: usize| v.get(i).copied().unwrap_or(0.0);
    let d = (0..n).fold(0.0_f32, |m, i| m.max((at(a, i) - at(b, i)).abs()));
    20.0 * f64::from(d.max(1e-30)).log10()
}

fn peak(a: &[f32]) -> f32 {
    a.iter().fold(0.0, |m, x| m.max(x.abs()))
}

fn freeze(p: &mut Project, samples: &mut HashMap<SampleSource, Arc<SampleData>>, t: TrackId) {
    let job = bounce::freeze_job(p, t).unwrap();
    bounce::check_routing(p, t).unwrap();
    let mut audio =
        bounce::render(p, samples, &job, SR, &Progress::default(), &mut Vec::new()).unwrap();
    bounce::trim_silence(&mut audio);
    let src = SampleSource::File(format!("frozen {}.wav", t.0).into());
    samples.insert(src.clone(), Arc::new(stereo(audio)));
    assert!(bounce::apply_freeze(p, t, src));
}

#[test]
fn a_frozen_track_sounds_like_the_track() {
    for insert in [MASTER, 3] {
        let mut p = song(insert);
        let (mut samples, missing) = load_samples(&p, SR);
        assert!(missing.is_empty());
        let before = mix(&p, &samples);
        assert!(peak(&before) > 0.1);
        let t = p.playlist.tracks[0].id;
        freeze(&mut p, &mut samples, t);
        let after = mix(&p, &samples);
        let db = diff_db(&before, &after);
        assert!(
            db < -120.0,
            "insert {insert}: frozen differs by {db:.1} dBFS"
        );

        // Unfreezing plays the clips again.
        p.playlist.track_mut(t).unwrap().frozen = None;
        assert!(diff_db(&before, &mix(&p, &samples)) < -120.0);
    }
}

#[test]
fn freeze_bypasses_the_sources() {
    let mut p = song(MASTER);
    let (mut samples, _) = load_samples(&p, SR);
    let t = p.playlist.tracks[0].id;
    freeze(&mut p, &mut samples, t);
    // With the frozen render silenced the track's clips no longer sound: only the hats remain.
    let frozen = p.playlist.tracks[0].frozen.clone().unwrap();
    samples.insert(
        frozen.source,
        Arc::new(SampleData {
            sample_rate: SR,
            channels: vec![vec![0.0; 16], vec![0.0; 16]],
        }),
    );
    let only_hats = mix(&p, &samples);
    let mut hats = song(MASTER);
    hats.playlist.tracks[0].mute = true;
    assert!(diff_db(&only_hats, &mix(&hats, &samples)) < -120.0);
}

#[test]
fn consolidated_clips_play_like_the_originals() {
    let mut p = song(MASTER);
    // Two audio clips on a third track, overlapping (so they crossfade), one reversed.
    let t = p.playlist.add_track();
    let clip = |p: &mut Project, start, reverse| {
        p.playlist.add_clip(
            t,
            start,
            1920,
            ClipKind::Audio {
                source: SampleSource::BuiltIn(BuiltInSample::Clap),
                gain: 1.0,
                gaps: Vec::new(),
                takes: Vec::new(),
                edit: AudioEdit {
                    reverse,
                    ..AudioEdit::default()
                },
            },
        )
    };
    let a = clip(&mut p, 960, false);
    let b = clip(&mut p, 960 + 1440, true);
    let (mut samples, _) = load_samples(&p, SR);
    let before = mix(&p, &samples);

    let job = bounce::consolidate_job(&p, &[a, b]).unwrap();
    assert_eq!((job.start, job.end), (960, 960 + 1440 + 1920));
    let data = bounce_of(&p, &samples, &job);
    let len = bounce::ticks_for(&p, job.start, data.frames(), SR);
    assert_eq!(len, job.end - job.start);
    let src = SampleSource::File("consolidated.wav".into());
    samples.insert(src.clone(), Arc::new(data));
    bounce::apply_consolidate(&mut p, &job, src, len).unwrap();
    assert_eq!(
        p.playlist.clips.iter().filter(|c| c.track == t).count(),
        1,
        "one clip replaces two"
    );
    let db = diff_db(&before, &mix(&p, &samples));
    assert!(db < -100.0, "consolidated differs by {db:.1} dBFS");
}

#[test]
fn render_to_audio_goes_on_a_new_track_and_mutes_the_source() {
    let mut p = song(MASTER);
    let (mut samples, _) = load_samples(&p, SR);
    let before = mix(&p, &samples);
    let t = p.playlist.tracks[0].id;
    // A time range of track 0 (the loop, say) covering the kick pattern and the snare clip.
    let job = bounce::render_job(&p, t, &[], Some((0, 2 * 3840))).unwrap();
    let data = bounce_of(&p, &samples, &job);
    assert!(peak(data.left()) > 0.1);
    let len = bounce::ticks_for(&p, job.start, data.frames(), SR);
    assert!(len >= 3840, "the tail is kept");
    let src = SampleSource::File("render.wav".into());
    samples.insert(src.clone(), Arc::new(data));
    let (nt, _) = bounce::apply_render(&mut p, &job, src, len).unwrap();
    assert_eq!(
        p.playlist.track_index(nt),
        Some(1),
        "right below the source"
    );
    assert!(p
        .playlist
        .clips
        .iter()
        .filter(|c| c.track == t)
        .all(|c| c.muted));
    let db = diff_db(&before, &mix(&p, &samples));
    assert!(db < -100.0, "render differs by {db:.1} dBFS");
}

#[test]
fn channels_that_bypass_the_tracks_strip_are_refused() {
    let mut p = song(3);
    p.channels[0].insert = 5;
    let t = p.playlist.tracks[0].id;
    let e = bounce::check_routing(&p, t).unwrap_err();
    assert!(e.contains("Kick"), "{e}");
    // A channel whose strip leads into the track's strip is fine.
    p.mixer.strips[5].output = 3;
    assert!(bounce::check_routing(&p, t).is_ok());
}
