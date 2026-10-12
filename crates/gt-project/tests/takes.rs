//! Take bookkeeping of loop recordings (Prompt 13): passes stack into one clip, any take can be
//! picked to play, and every step undoes.

use gt_core::{AudioTake, ClipKind, Gap, Project, SampleSource};
use gt_project::History;

fn take(n: u32, length: i64) -> AudioTake {
    AudioTake {
        source: SampleSource::File(format!("recordings/Vox take {n}.wav").into()),
        gaps: if n == 2 {
            vec![Gap {
                at_s: 0.5,
                len_s: 0.01,
            }]
        } else {
            Vec::new()
        },
        length,
    }
}

/// (playing file, its gaps, clip length, inactive files).
fn state(p: &Project) -> (String, usize, i64, Vec<String>) {
    let c = &p.playlist.clips[0];
    let name = |s: &SampleSource| match s {
        SampleSource::File(f) => f.file_stem().unwrap().to_string_lossy().into_owned(),
        SampleSource::BuiltIn(_) => String::new(),
    };
    let ClipKind::Audio {
        source,
        gaps,
        takes,
        ..
    } = &c.kind
    else {
        panic!("audio clip")
    };
    (
        name(source),
        gaps.len(),
        c.length,
        takes.iter().map(|t| name(&t.source)).collect(),
    )
}

#[test]
fn takes_stack_switch_and_undo() {
    let mut p = Project::empty();
    let track = p.playlist.tracks[0].id;
    let first = take(1, 3840);
    let id = p.playlist.add_clip(
        track,
        3840,
        first.length,
        ClipKind::Audio {
            source: first.source,
            gain: 1.0,
            gaps: first.gaps,
            takes: Vec::new(),
        },
    );
    let mut h = History::new(&p);

    // Two more loop passes stack onto the clip; the newest plays.
    for (n, len) in [(2, 3840), (3, 2000)] {
        assert!(p.playlist.clip_mut(id).unwrap().stack_take(take(n, len)));
        assert!(h.commit(&p, "Record audio"));
    }
    assert_eq!(
        state(&p),
        (
            "Vox take 3".into(),
            0,
            2000,
            vec!["Vox take 1".into(), "Vox take 2".into()]
        )
    );

    // Pick take 2: it plays with its own gaps and length, take 3 joins the inactive ones.
    assert!(p.playlist.clip_mut(id).unwrap().use_take(1));
    assert!(h.commit(&p, "Use take"));
    assert_eq!(
        state(&p),
        (
            "Vox take 2".into(),
            1,
            3840,
            vec!["Vox take 1".into(), "Vox take 3".into()]
        )
    );
    assert!(
        !p.playlist.clip_mut(id).unwrap().use_take(5),
        "no such take"
    );

    // Keep only the take that plays.
    assert_eq!(p.playlist.clip_mut(id).unwrap().clear_takes(), 2);
    assert!(h.commit(&p, "Delete other takes"));
    assert!(state(&p).3.is_empty());

    // Every step undoes and redoes.
    assert_eq!(h.undo(&mut p).as_deref(), Some("Delete other takes"));
    assert_eq!(state(&p).0, "Vox take 2");
    assert_eq!(state(&p).3.len(), 2);
    assert_eq!(h.undo(&mut p).as_deref(), Some("Use take"));
    assert_eq!(state(&p).0, "Vox take 3");
    assert_eq!(h.undo(&mut p).as_deref(), Some("Record audio"));
    assert_eq!(
        state(&p),
        ("Vox take 2".into(), 1, 3840, vec!["Vox take 1".into()])
    );
    assert_eq!(h.redo(&mut p).as_deref(), Some("Record audio"));
    assert_eq!(state(&p).0, "Vox take 3");
}
