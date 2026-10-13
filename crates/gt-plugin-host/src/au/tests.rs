//! Audio Unit tests with units every Mac has: Apple's AUDelay and DLSMusicDevice.

use std::sync::Arc;

use gt_engine::PluginContext;

use super::*;

const DELAY: &str = "aufx:dely:appl";
const DLS: &str = "aumu:dls :appl";
const RATE: u32 = 48_000;

fn info(id: &str) -> PluginInfo {
    list()
        .into_iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("{id} is not installed"))
}

fn loaded(id: &str) -> AuPlugin {
    AuPlugin::create(&info(id), Signals::new(Arc::new(|| {}))).unwrap()
}

#[test]
fn system_units_are_listed_with_their_kind() {
    let delay = info(DELAY);
    assert_eq!(delay.format, PluginFormat::Au);
    assert_eq!(delay.kind(), PluginKind::Effect);
    assert_eq!(delay.vendor, "Apple");
    assert_eq!(delay.path, PathBuf::from("au:aufx:dely:appl"));
    assert_eq!(info(DLS).kind(), PluginKind::Instrument);
}

#[test]
fn an_effect_processes_and_its_state_round_trips() {
    let mut a = loaded(DELAY);
    let (params, values) = a.read_params();
    assert!(!params.is_empty());
    assert_eq!(params.len(), values.len());
    assert!(a.value_text(0, values[0]).is_some_and(|t| !t.is_empty()));
    let (mut proc, _rx) = a.activate(RATE).unwrap();
    let ctx = PluginContext::default();
    let (mut l, mut r) = ([0.0_f32; 32], [0.0_f32; 32]);
    l[0] = 1.0;
    r[0] = 1.0;
    assert!(proc.process(&mut l, &mut r, &ctx));
    assert!(l.iter().chain(&r).all(|x| x.is_finite()));
    assert!(l[0] != 0.0, "the dry signal passes");
    // Change every parameter a little, save, and restore into a new instance.
    for (i, v) in values.iter().enumerate() {
        proc.set_param(i as u32, (v + 0.25) % 1.0);
    }
    proc.process(&mut l, &mut r, &ctx);
    drop(proc);
    assert!(a.try_deactivate());
    let (_, changed) = a.read_params();
    let state = a.save_state().unwrap();
    assert_eq!(&state[..4], STATE_MAGIC);
    let mut b = loaded(DELAY);
    b.load_state(&state).unwrap();
    let (_, restored) = b.read_params();
    for (x, y) in changed.iter().zip(&restored) {
        assert!((x - y).abs() < 1e-3, "{changed:?} {restored:?}");
    }
    assert!(b.load_state(b"junk").is_err());
}

#[test]
fn an_instrument_plays_notes() {
    let mut p = loaded(DLS);
    p.read_params();
    let (mut proc, _rx) = p.activate(RATE).unwrap();
    let ctx = PluginContext::default();
    let (mut l, mut r) = ([0.0_f32; 32], [0.0_f32; 32]);
    proc.note_on(0, 60, 1.0);
    let mut loud = 0.0_f32;
    for _ in 0..200 {
        assert!(proc.process(&mut l, &mut r, &ctx));
        loud = l.iter().chain(&r).fold(loud, |m, x| m.max(x.abs()));
    }
    assert!(loud > 1e-3, "{loud}");
    proc.release_all(0);
    proc.process(&mut l, &mut r, &ctx);
}
