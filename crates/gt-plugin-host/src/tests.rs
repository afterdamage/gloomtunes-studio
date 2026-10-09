//! Host tests with the GT test plugins, loaded in process.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clack_host::prelude::PluginEntry;
use gt_core::{EffectKind, EffectSlot, PluginKind, Project, MASTER};
use gt_engine::{
    ChannelParams, EngineCommand, EngineConfig, MixerParams, PluginContext, PluginProcessor,
};
use gt_test_plugin::{TestEntry, GAIN_ID, TONE_ID};

use crate::catalog::{list_entry, PluginInfo};
use crate::host::Signals;
use crate::instance::LoadedPlugin;
use crate::processor::ParamOut;
use crate::{PluginHost, PluginStatus};

const RATE: u32 = 48_000;
const FILE: &str = "/test/gt_test_plugin.clap";

fn entry() -> PluginEntry {
    PluginEntry::load_from_clack::<TestEntry>(c"").unwrap()
}

fn info(id: &str) -> PluginInfo {
    list_entry(&entry(), Path::new(FILE))
        .into_iter()
        .find(|p| p.id == id)
        .unwrap()
}

fn loaded(id: &str) -> LoadedPlugin {
    let e = entry();
    LoadedPlugin::create(&e, &info(id), Signals::new(Arc::new(|| {}))).unwrap()
}

fn ctx() -> PluginContext {
    PluginContext::default()
}

#[test]
fn the_test_plugins_are_listed_with_their_kind() {
    let all = list_entry(&entry(), Path::new(FILE));
    assert_eq!(all.len(), 2);
    assert_eq!(info(GAIN_ID).kind(), PluginKind::Effect);
    assert_eq!(info(TONE_ID).kind(), PluginKind::Instrument);
    assert_eq!(info(TONE_ID).vendor, "GloomTunes");
    assert_eq!(info(GAIN_ID).path, PathBuf::from(FILE));
}

#[test]
fn an_effect_processes_with_its_parameters_and_misbehaviour_is_reported() {
    let mut p = loaded(GAIN_ID);
    let (params, values) = p.read_params();
    assert_eq!(params.len(), 2);
    assert_eq!(params[0].name, "Gain");
    assert_eq!(values[0], 0.5, "gain 1 of 0..2");
    assert_eq!(params[1].name, "Test / Misbehave");
    assert_eq!(params[1].steps, 2);
    let (mut proc, _rx) = p.activate(RATE).unwrap();

    let (mut l, mut r) = ([1.0_f32; 32], [-1.0_f32; 32]);
    assert!(proc.process(&mut l, &mut r, &ctx()));
    assert_eq!((l[0], r[31]), (1.0, -1.0));
    // Normalized 0.25 of 0..2 is a gain of 0.5.
    proc.set_param(0, 0.25);
    assert!(proc.process(&mut l, &mut r, &ctx()));
    assert_eq!((l[0], r[31]), (0.5, -0.5));
    // Misbehave = NaN: the plugin "succeeds" with invalid output (the engine checks).
    proc.set_param(1, 0.5);
    assert!(proc.process(&mut l, &mut r, &ctx()));
    assert!(l[0].is_nan());
    // Misbehave = error: process reports failure.
    proc.set_param(1, 1.0);
    let (mut l, mut r) = ([1.0_f32; 32], [1.0_f32; 32]);
    assert!(!proc.process(&mut l, &mut r, &ctx()));
    proc.stop();
    drop(proc);
    assert!(p.try_deactivate());
}

#[test]
fn an_instrument_plays_notes_and_reports_its_own_parameter_changes() {
    let mut p = loaded(TONE_ID);
    let (params, _) = p.read_params();
    assert!(
        !params[2].automatable,
        "read-only parameters are not automatable"
    );
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
    // Releasing fades out.
    proc.release_all(0);
    for _ in 0..200 {
        proc.process(&mut l, &mut r, &ctx());
    }
    assert!(l.iter().all(|&x| x.abs() < 1e-3));
    proc.stop();
}

#[test]
fn state_saves_and_restores_into_a_new_instance() {
    let mut a = loaded(GAIN_ID);
    a.read_params();
    let (mut proc, _rx) = a.activate(RATE).unwrap();
    proc.set_param(0, 0.75);
    let (mut l, mut r) = ([0.0_f32; 32], [0.0_f32; 32]);
    proc.process(&mut l, &mut r, &ctx());
    let state = a.save_state().unwrap();
    assert_eq!(&state[..4], b"GTG1");

    let mut b = loaded(GAIN_ID);
    b.load_state(&state).unwrap();
    let (_, values) = b.read_params();
    assert_eq!(values[0], 0.75);
    assert!(b.load_state(b"junk").is_err());
    let mut tone = loaded(TONE_ID);
    assert!(
        tone.load_state(&state).is_err(),
        "another plugin's state is rejected"
    );
}

fn host() -> PluginHost {
    let dir = std::env::temp_dir().join(format!(
        "gt-host-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let (mut h, report) = PluginHost::new(&dir, PathBuf::from("gloomtunes"), Arc::new(|| {}));
    assert!(report.is_empty());
    h.insert_entry(Path::new(FILE), entry());
    h
}

#[test]
fn the_host_runs_project_plugins_in_the_engine_and_mirrors_their_parameters() {
    let mut host = host();
    let (mut eng, mut audio) = gt_engine::create(EngineConfig {
        sample_rate: RATE,
        out_channels: 2,
    });
    let mut project = Project::empty();
    let tone = host.instantiate(&info(TONE_ID)).unwrap();
    let tone_id = tone.instance;
    assert_eq!(tone.params.len(), 3);
    let ch = project.add_plugin_channel("Tone", tone).unwrap();
    let gain = host.instantiate(&info(GAIN_ID)).unwrap();
    let gain_id = gain.instance;
    project.mixer.strips[MASTER].slots[0] = Some(EffectSlot::plugin(gain));
    assert_eq!(host.status(tone_id), PluginStatus::Waiting);

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
    let loud = buf.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
    assert!(loud > 0.01, "{loud}");

    // The plugin's own parameter change reaches the document.
    let out = host.sync(&mut project, Some(&mut eng), false);
    assert!(out.mirrored);
    let p = project.plugin_by_instance(tone_id).unwrap().1;
    assert!((p.values[2] - 64.0 / 127.0).abs() < 1e-6);

    // A document edit reaches the plugin: gain 0 silences the master.
    project.plugin_by_instance_mut(gain_id).unwrap().values[0] = 0.0;
    host.sync(&mut project, Some(&mut eng), false);
    audio.process(&mut buf);
    audio.process(&mut buf);
    assert!(buf[2 * 200..].iter().all(|&x| x == 0.0));

    // Removing the plugins retires them once the engine hands them back.
    project.mixer.strips[MASTER].slots[0] = None;
    project.channels.clear();
    host.sync(&mut project, Some(&mut eng), false);
    assert_eq!(host.retiring.len(), 2);
    audio.process(&mut buf);
    eng.collect_garbage();
    host.sync(&mut project, Some(&mut eng), false);
    assert!(host.retiring.is_empty(), "both destroyed");
    assert!(host.live.is_empty());
}

#[test]
fn saved_state_is_restored_and_missing_plugins_are_reported() {
    let mut host = host();
    let mut project = Project::empty();
    let mut gain = host.instantiate(&info(GAIN_ID)).unwrap();
    // As if loaded from a file: a fresh instance id and a saved state with gain 1.5.
    let mut state = b"GTG1".to_vec();
    state.extend_from_slice(&1.5_f32.to_le_bytes());
    state.extend_from_slice(&0_u32.to_le_bytes());
    gain.state = Arc::from(state);
    gain.instance = gt_core::PluginInstanceId::fresh();
    let id = gain.instance;
    project.mixer.strips[MASTER].slots[0] = Some(EffectSlot::plugin(gain.clone()));
    let mut missing = gain;
    missing.id = "com.example.nothing".to_owned();
    missing.path = PathBuf::from("/nowhere/nothing.clap");
    missing.instance = gt_core::PluginInstanceId::fresh();
    let missing_id = missing.instance;
    project.mixer.strips[MASTER].slots[1] = Some(EffectSlot::plugin(missing));

    let out = host.sync(&mut project, None, false);
    assert!(out.mirrored);
    assert_eq!(project.plugin_by_instance(id).unwrap().1.values[0], 0.75);
    assert_eq!(host.status(id), PluginStatus::Waiting, "no engine yet");
    assert!(matches!(
        host.status(missing_id),
        PluginStatus::Unavailable(_)
    ));
    assert!(!host.take_notices().is_empty());
    // The missing plugin's entry (and state) stays in the project.
    assert_eq!(
        project.mixer.strips[MASTER].slots[1].as_ref().unwrap().kind,
        EffectKind::Plugin
    );

    // Saving copies the live state into the document.
    project.plugin_by_instance_mut(id).unwrap().state = Arc::from(Vec::new());
    host.store_states(&mut project);
    assert_eq!(project.plugin_by_instance(id).unwrap().1.state.len(), 12);

    // Export gets fresh processors in the same state.
    let mut procs = host.export_processors(&project, 44_100);
    assert_eq!(procs.len(), 1);
    assert_eq!(procs[0].instance, id);
    let (mut l, mut r) = ([1.0_f32; 32], [1.0_f32; 32]);
    assert!(procs[0].processor.process(&mut l, &mut r, &ctx()));
    assert_eq!(l[0], 1.5);
    drop(procs);
    host.finish_export();
    host.sync(&mut project, None, false);
    assert!(host.retiring.is_empty());
    let _ = ParamOut {
        index: 0,
        value: 0.0,
    };
}

#[test]
fn a_failed_plugin_runs_again_with_the_values_edited_while_it_was_bypassed() {
    let mut host = host();
    let (mut eng, mut audio) = gt_engine::create(EngineConfig {
        sample_rate: RATE,
        out_channels: 2,
    });
    let mut project = Project::empty();
    let gain = host.instantiate(&info(GAIN_ID)).unwrap();
    let id = gain.instance;
    project.mixer.strips[MASTER].slots[0] = Some(EffectSlot::plugin(gain));
    host.sync(&mut project, Some(&mut eng), true);
    eng.send(EngineCommand::SetMixer(Box::new(MixerParams::from_mixer(
        &project.mixer,
    ))))
    .unwrap();
    let mut buf = vec![0.0_f32; 2 * 256];

    // Misbehave = NaN: the engine bypasses it.
    project.plugin_by_instance_mut(id).unwrap().values[1] = 0.5;
    host.sync(&mut project, Some(&mut eng), false);
    audio.process(&mut buf);
    host.sync(&mut project, Some(&mut eng), false);
    assert_eq!(host.status(id), PluginStatus::Failed);
    assert!(buf.iter().all(|x| x.is_finite()), "bypassed, not poisoned");

    // Fixed while bypassed, then retried: the fix reaches the plugin first.
    project.plugin_by_instance_mut(id).unwrap().values[1] = 0.0;
    host.sync(&mut project, Some(&mut eng), false);
    host.retry(id, Some(&mut eng));
    host.sync(&mut project, Some(&mut eng), false);
    audio.process(&mut buf);
    audio.process(&mut buf);
    host.sync(&mut project, Some(&mut eng), false);
    assert_eq!(host.status(id), PluginStatus::Running);
}
