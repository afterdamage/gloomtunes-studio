//! Golden-file test: a stored reference project, loaded and exported, must give a WAV file with
//! a stored hash.
//!
//! The hash covers every byte of the file, so any change to loading, the engine, DSP, mixing,
//! dither or the WAV writer shows up here. Floating-point maths (`sin`, `exp`, `tanh`) can
//! differ in the last bit between operating systems' math libraries, so each OS has its own
//! hash. When a change is meant to alter the sound, listen to the new export, then update the
//! hashes: the failure message prints the new one.
//!
//! `tests/fixtures/reference.gloom` is the demo project saved with schema version 1. Regenerate
//! it only together with a new schema version (the old one must keep loading):
//! `cargo test -p gt-export --test golden -- --ignored regenerate`.

use std::path::{Path, PathBuf};

use gt_export::{export, BitDepth, ExportSettings, Progress, Range};
use gt_project::file;

/// Expected FNV-1a 64 hash of the exported WAV, per operating system.
const EXPECTED: &[(&str, &str)] = &[
    ("linux", "b399cad748bb62c9"),
    ("windows", "e682a81ea7dbbf92"),
];

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference.gloom")
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("gt-golden-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[test]
fn reference_project_exports_to_the_stored_hash() {
    let dir = temp_dir("export");
    let loaded = file::load(&fixture(), &dir.join("samples")).expect("fixture loads");
    assert!(loaded.missing.is_empty() && loaded.warnings.is_empty());
    assert_eq!(loaded.schema_version, 1);
    // Bars 3 and 4 (the drop: drums, bass, filter automation, LFO and follower), 16-bit with
    // dither, the tail rendered until silent.
    let settings = ExportSettings {
        sample_rate: 48_000,
        depth: BitDepth::Pcm16,
        dither: true,
        normalize_db: None,
        range: Range::Ticks {
            start: 2 * 3840,
            end: 4 * 3840,
        },
        tail: true,
        max_tail_seconds: 4.0,
        stems: false,
    };
    let out = dir.join("reference.wav");
    let written = export(&loaded.project, &settings, &out, &Progress::default()).unwrap();
    assert_eq!(written, vec![out.clone()]);
    let bytes = std::fs::read(&out).unwrap();
    // 4 s of music plus a tail; 44-byte header, 4 bytes per frame.
    let frames = (bytes.len() - 44) / 4;
    assert!(
        (192_000..192_000 + 4 * 48_000).contains(&frames),
        "{frames}"
    );
    let got = format!("{:016x}", fnv1a(&bytes));
    let os = std::env::consts::OS;
    let want = EXPECTED.iter().find(|(o, _)| *o == os).map(|(_, h)| *h);
    assert_eq!(
        want,
        Some(got.as_str()),
        "export of the reference project changed on {os}: hash {got} ({frames} frames). If the \
         change is intended, listen to {} and update EXPECTED in tests/golden.rs",
        out.display()
    );
}

#[test]
#[ignore = "rewrites the fixture; run only when the schema version changes"]
fn regenerate() {
    let p = gt_core::Project::demo();
    file::save(&p, &fixture(), file::SaveOptions::default()).unwrap();
}
