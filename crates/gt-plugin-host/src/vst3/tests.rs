//! VST3 host tests with the GT test VST3 plugins: in process through their factory, and from a
//! real `.vst3` bundle built from the test crate's `cdylib`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gt_core::{EffectSlot, PluginFormat, PluginKind, Project, MASTER};
use gt_engine::{ChannelParams, EngineCommand, EngineConfig, MixerParams, PluginContext};
use gt_test_vst3::{GAIN_CID, TONE_CID, TONE_LAST_KEY, TONE_VOLUME};
use vst3::Steinberg::Vst::IComponentHandlerTrait;

use super::*;
use crate::backend::{Entry, Instance};
use crate::{PluginHost, PluginStatus};

const RATE: u32 = 48_000;
const FILE: &str = "/test/GT Test.vst3";

fn module() -> std::rc::Rc<Module> {
    std::rc::Rc::new(Module::from_factory(gt_test_vst3::GetPluginFactory))
}

fn info(cid: &TUID) -> PluginInfo {
    module()
        .list(Path::new(FILE))
        .into_iter()
        .find(|p| p.id == cid_hex(cid))
        .unwrap()
}

fn loaded(cid: &TUID) -> Vst3Plugin {
    Vst3Plugin::create(&module(), &info(cid), Signals::new(Arc::new(|| {}))).unwrap()
}

fn ctx() -> PluginContext {
    PluginContext::default()
}

#[test]
fn classes_are_listed_as_plugins_with_their_kind() {
    let all = module().list(Path::new(FILE));
    assert_eq!(all.len(), 2, "controllers are not plugins");
    let (gain, tone) = (info(&GAIN_CID), info(&TONE_CID));
    assert_eq!(gain.format, PluginFormat::Vst3);
    assert_eq!(gain.kind(), PluginKind::Effect);
    assert_eq!(tone.kind(), PluginKind::Instrument);
    assert_eq!(tone.name, "GT Test Tone VST3");
    assert_eq!(tone.vendor, "GloomTunes");
    assert!(tone.features.contains(&"synth".to_owned()));
    assert_eq!(cid_parse(&tone.id), Some(TONE_CID));
    assert_eq!(cid_parse("nope"), None);
}

#[test]
fn an_effect_processes_with_its_parameters_and_misbehaviour_is_reported() {
    let mut p = loaded(&GAIN_CID);
    let (params, values) = p.read_params();
    assert_eq!(params.len(), 2);
    assert_eq!(params[0].name, "Gain");
    assert_eq!(values[0], 0.5, "gain 1 of 0..2");
    assert_eq!(params[1].steps, 2);
    let (mut proc, _rx) = p.activate(RATE).unwrap();
    assert_eq!(proc.latency(), 16);
    assert!(!p.try_deactivate(), "the engine still has the processor");

    let (mut l, mut r) = ([1.0_f32; 32], [-1.0_f32; 32]);
    assert!(proc.process(&mut l, &mut r, &ctx()));
    assert_eq!((l[0], r[31]), (1.0, -1.0));
    proc.set_param(0, 0.25);
    assert!(proc.process(&mut l, &mut r, &ctx()));
    assert_eq!((l[0], r[31]), (0.5, -0.5));
    proc.set_param(1, 0.5);
    assert!(proc.process(&mut l, &mut r, &ctx()));
    assert!(l[0].is_nan());
    proc.set_param(1, 1.0);
    let (mut l, mut r) = ([1.0_f32; 32], [1.0_f32; 32]);
    assert!(!proc.process(&mut l, &mut r, &ctx()));
    proc.stop();
    drop(proc);
    assert!(p.try_deactivate());
}

#[test]
fn an_instrument_plays_notes_reports_its_parameters_and_follows_mapped_controllers() {
    let mut p = loaded(&TONE_CID);
    let (params, _) = p.read_params();
    assert!(!params[2].automatable, "read-only");
    let (mut proc, mut rx) = p.activate(RATE).unwrap();
    let (mut l, mut r) = ([0.0_f32; 32], [0.0_f32; 32]);
    assert!(proc.process(&mut l, &mut r, &ctx()));
    assert!(l.iter().all(|&x| x == 0.0));
    proc.note_on(4, 72, 1.0);
    assert!(proc.process(&mut l, &mut r, &ctx()));
    assert_eq!(l[..4], [0.0; 4], "the note starts at its offset");
    assert!(l.iter().any(|&x| x != 0.0));
    assert_eq!(l, r);
    let out = rx.pop().unwrap();
    assert_eq!(out.index, 2);
    assert!((out.value - 72.0 / 127.0).abs() < 1e-6);
    // The plugin's report reaches its controller (what its editor would show).
    p.plugin_reported(2, out.value);
    let c = p.controller.as_ref().unwrap();
    // SAFETY: a plain query.
    let shown = unsafe { c.getParamNormalized(TONE_LAST_KEY) };
    assert!((shown - f64::from(72.0_f32 / 127.0)).abs() < 1e-6);
    // The mod wheel is mapped to the volume: at 0 the tone is silent.
    proc.midi(0, [0xB0, 1, 0]);
    for _ in 0..50 {
        proc.process(&mut l, &mut r, &ctx());
    }
    assert!(l.iter().all(|&x| x == 0.0));
    proc.stop();
}

#[test]
fn the_sub_output_and_the_midi_echo_reach_the_engine() {
    let mut p = loaded(&TONE_CID);
    p.read_params();
    assert_eq!(p.outputs(), (vec!["Sub".to_owned()], true));
    let (mut proc, _rx) = p.activate(RATE).unwrap();
    assert_eq!(proc.extra_outputs(), 1);
    let (mut l, mut r) = ([0.0_f32; 32], [0.0_f32; 32]);
    proc.note_on(2, 60, 1.0);
    assert!(proc.process(&mut l, &mut r, &ctx()));
    let (sl, sr) = proc.extra_output(0).unwrap();
    assert!(sl.iter().any(|&x| x != 0.0), "the sub output plays");
    assert_eq!(sl, sr);
    assert_ne!(sl, &l[..], "an octave lower than the main output");
    let mut midi = Vec::new();
    proc.midi_out(&mut |at, msg| midi.push((at, msg)));
    assert_eq!(midi, vec![(2, [0x90, 72, 127])]);
    proc.note_off(7, 60);
    proc.process(&mut l, &mut r, &ctx());
    proc.midi_out(&mut |at, msg| midi.push((at, msg)));
    assert_eq!(midi[1], (7, [0x80, 72, 0]));
    proc.stop();
}

#[test]
fn state_round_trips_and_editor_edits_reach_the_document() {
    let mut a = loaded(&GAIN_CID);
    a.read_params();
    let (mut proc, _rx) = a.activate(RATE).unwrap();
    proc.set_param(0, 0.75);
    let (mut l, mut r) = ([0.0_f32; 32], [0.0_f32; 32]);
    proc.process(&mut l, &mut r, &ctx());
    let state = a.save_state().unwrap();
    assert_eq!(&state[..4], STATE_MAGIC);

    let mut b = loaded(&GAIN_CID);
    b.load_state(&state).unwrap();
    let (_, values) = b.read_params();
    assert_eq!(
        values[0], 0.75,
        "the controller follows the component's state"
    );
    assert!(b.load_state(b"junk").is_err());
    let mut tone = loaded(&TONE_CID);
    assert!(
        tone.load_state(&state).is_err(),
        "another plugin's state is rejected"
    );

    // An edit in the editor (performEdit) comes out as (index, value) for the document.
    tone.read_params();
    // SAFETY: a plain call on our own handler.
    unsafe { tone.handler.performEdit(TONE_VOLUME, 0.25) };
    assert_eq!(tone.take_edits(), vec![(0, 0.25)]);
    assert!(Signals::take(&tone.signals.dirty));
}

#[test]
fn values_are_written_with_units_and_vstpreset_files_load() {
    let mut a = loaded(&GAIN_CID);
    a.read_params();
    assert_eq!(a.value_text(0, 0.25).as_deref(), Some("-6.0 dB"));
    assert_eq!(a.value_text(1, 0.5).as_deref(), Some("NaN"));
    assert_eq!(a.value_text(7, 0.5), None);

    // A .vstpreset with the gain at 0.75 (the parts of a saved state).
    let (mut proc, _rx) = a.activate(RATE).unwrap();
    proc.set_param(0, 0.75);
    let (mut l, mut r) = ([0.0_f32; 32], [0.0_f32; 32]);
    proc.process(&mut l, &mut r, &ctx());
    let comp = a.save(false).unwrap();
    let cont = a.save(true).unwrap();
    let file = std::env::temp_dir().join(format!("gt-{}.vstpreset", std::process::id()));
    std::fs::write(
        &file,
        presets::write_vstpreset(&cid_hex(&GAIN_CID), &comp, &cont),
    )
    .unwrap();
    let mut b = loaded(&GAIN_CID);
    b.read_params();
    b.load_factory(&Source::Vst3File(file.clone())).unwrap();
    assert_eq!(b.read_params().1[0], 0.75);
    std::fs::write(&file, b"VST3 junk").unwrap();
    assert!(b.load_factory(&Source::Vst3File(file.clone())).is_err());
    let _ = std::fs::remove_file(file);
}

fn host() -> PluginHost {
    let dir = std::env::temp_dir().join(format!(
        "gt-host-vst3-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let (mut h, _) = PluginHost::new(&dir, PathBuf::from("gloomtunes"), Arc::new(|| {}));
    h.insert_entry(Path::new(FILE), Entry::Vst3(module()));
    h
}

#[test]
fn the_host_runs_vst3_plugins_in_the_engine_like_clap_ones() {
    let mut host = host();
    let (mut eng, mut audio) = gt_engine::create(EngineConfig {
        sample_rate: RATE,
        out_channels: 2,
    });
    let mut project = Project::empty();
    let tone = host.instantiate(&info(&TONE_CID)).unwrap();
    assert_eq!(tone.format, PluginFormat::Vst3);
    let tone_id = tone.instance;
    let ch = project.add_plugin_channel("Tone", tone).unwrap();
    let gain = host.instantiate(&info(&GAIN_CID)).unwrap();
    let gain_id = gain.instance;
    project.mixer.strips[MASTER].slots[0] = Some(EffectSlot::plugin(gain));
    host.sync(&mut project, Some(&mut eng), true);
    assert_eq!(host.status(tone_id), PluginStatus::Running);
    assert_eq!(host.status(gain_id), PluginStatus::Running);
    let i = project.channel_index(ch).unwrap();
    eng.send(EngineCommand::SetChannelParams {
        slot: i as u16,
        params: Box::new(ChannelParams::from_channel(&project.channels[i], false)),
    })
    .unwrap();
    eng.send(EngineCommand::SetMixer(Box::new(MixerParams::from_mixer(
        &project.mixer,
    ))))
    .unwrap();
    eng.send(EngineCommand::NoteOn {
        slot: i as u16,
        key: 64,
        velocity: 1.0,
    })
    .unwrap();
    let mut buf = vec![0.0_f32; 2 * 256];
    audio.process(&mut buf);
    audio.process(&mut buf);
    assert!(buf.iter().any(|x| x.abs() > 0.01));

    let out = host.sync(&mut project, Some(&mut eng), false);
    assert!(out.mirrored);
    let p = project.plugin_by_instance(tone_id).unwrap().1;
    assert!((p.values[2] - 64.0 / 127.0).abs() < 1e-6);

    // Saving and reloading through the document keeps the state.
    project.plugin_by_instance_mut(gain_id).unwrap().values[0] = 0.0;
    host.sync(&mut project, Some(&mut eng), false);
    audio.process(&mut buf);
    host.store_states(&mut project);
    let saved = project.plugin_by_instance(gain_id).unwrap().1.clone();
    assert_eq!(&saved.state[..4], STATE_MAGIC);
    let mut again = saved.clone();
    again.instance = gt_core::PluginInstanceId::fresh();
    project.mixer.strips[MASTER].slots[1] = Some(EffectSlot::plugin(again.clone()));
    host.sync(&mut project, Some(&mut eng), false);
    assert_eq!(host.status(again.instance), PluginStatus::Running);
    assert_eq!(
        project.plugin_by_instance(again.instance).unwrap().1.values[0],
        0.0
    );

    project.mixer.strips[MASTER].slots = Default::default();
    project.channels.clear();
    host.sync(&mut project, Some(&mut eng), false);
    audio.process(&mut buf);
    eng.collect_garbage();
    host.sync(&mut project, Some(&mut eng), false);
    assert!(host.retiring.is_empty() && host.live.is_empty());
}

/// The test crate's `cdylib`, next to the test binary's folder.
fn built_library() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let name = if cfg!(windows) {
        "gt_test_vst3.dll"
    } else if cfg!(target_os = "macos") {
        "libgt_test_vst3.dylib"
    } else {
        "libgt_test_vst3.so"
    };
    exe.ancestors()
        .skip(1)
        .take(2)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

#[test]
fn a_real_bundle_is_found_scanned_loaded_and_played() {
    let Some(lib) = built_library() else {
        // `cargo test --workspace` (CI) builds the test plugin's cdylib; a single-crate run
        // may not have.
        assert!(
            std::env::var_os("CI").is_none(),
            "the gt-test-vst3 cdylib was not built"
        );
        return;
    };
    let dir = std::env::temp_dir().join(format!("gt-vst3-bundle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let bundle = dir.join("GT Test.vst3");
    let bin_dir = bundle.join("Contents").join(if cfg!(windows) {
        "x86_64-win".to_owned()
    } else if cfg!(target_os = "macos") {
        "MacOS".to_owned()
    } else {
        format!("{}-linux", std::env::consts::ARCH)
    });
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bin_name = if cfg!(windows) {
        "GT Test.vst3"
    } else if cfg!(target_os = "macos") {
        "GT Test"
    } else {
        "GT Test.so"
    };
    std::fs::copy(&lib, bin_dir.join(bin_name)).unwrap();

    let files = crate::catalog::find_files(std::slice::from_ref(&dir));
    assert_eq!(files, vec![bundle.clone()]);
    let found = crate::catalog::scan_in_process(&bundle).unwrap();
    assert_eq!(found.len(), 2);
    let gain = found.iter().find(|p| p.id == cid_hex(&GAIN_CID)).unwrap();
    assert_eq!(gain.path, bundle);

    let Entry::Vst3(m) = Entry::load(&bundle, PluginFormat::Vst3).unwrap() else {
        unreachable!()
    };
    let mut p = Vst3Plugin::create(&m, gain, Signals::new(Arc::new(|| {}))).unwrap();
    p.read_params();
    let (mut proc, _rx) = p.activate(RATE).unwrap();
    proc.set_param(0, 1.0);
    let (mut l, mut r) = ([0.5_f32; 32], [0.5_f32; 32]);
    assert!(proc.process(&mut l, &mut r, &ctx()));
    assert_eq!(l[0], 1.0);
    proc.stop();
    drop(proc);
    let state = p.save_state().unwrap();
    drop(p);
    let mut q = Vst3Plugin::create(&m, gain, Signals::new(Arc::new(|| {}))).unwrap();
    q.load_state(&state).unwrap();
    assert_eq!(q.read_params().1[0], 1.0);
    drop(q);

    // The multi-output instrument from the bundle.
    let tone = found.iter().find(|p| p.id == cid_hex(&TONE_CID)).unwrap();
    let mut t = Vst3Plugin::create(&m, tone, Signals::new(Arc::new(|| {}))).unwrap();
    t.read_params();
    assert_eq!(t.outputs().0, ["Sub"]);
    let (mut proc, _rx) = t.activate(RATE).unwrap();
    proc.note_on(0, 60, 1.0);
    let (mut l, mut r) = ([0.0_f32; 32], [0.0_f32; 32]);
    assert!(proc.process(&mut l, &mut r, &ctx()));
    assert!(proc.extra_output(0).unwrap().0.iter().any(|&x| x != 0.0));
    proc.stop();
    drop(proc);
    drop(t);
    drop(m);
    let _ = std::fs::remove_dir_all(&dir);
}
