//! Sandbox tests: the test binary runs itself as the bridge child (the `child` test below,
//! with [`CHILD_ENV`] set), loading the GT test plugins in that process.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_core::{EffectSlot, Project, MASTER};
use gt_engine::{EngineCommand, EngineConfig, MixerParams, PluginContext};
use gt_test_plugin::GAIN_ID;

use super::host::{BridgeCommand, BridgeInstance};
use crate::backend::{Entry, Instance};
use crate::host::Signals;
use crate::PluginStatus;

const CHILD_ENV: &str = "GT_BRIDGE_TEST_CHILD";
const RATE: u32 = 48_000;

/// Not a test: the bridge child, when the tests start themselves with [`CHILD_ENV`].
#[test]
fn child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let code = super::child::serve(&|_| Ok(Entry::Clap(crate::tests::entry())));
    std::process::exit(code);
}

fn command() -> BridgeCommand {
    BridgeCommand {
        exe: std::env::current_exe().unwrap(),
        args: [
            "bridge::tests::child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ]
        .map(Into::into)
        .into(),
        env: vec![(CHILD_ENV.into(), "1".into())],
    }
}

fn block(p: &mut dyn gt_engine::PluginProcessor, v: f32) -> (bool, f32) {
    let (mut l, mut r) = ([v; 32], [v; 32]);
    let ok = p.process(&mut l, &mut r, &PluginContext::default());
    (ok, l[0])
}

#[test]
fn a_plugin_in_its_own_process_runs_and_its_crash_is_contained() {
    let signals = Signals::new(Arc::new(|| {}));
    let mut b = BridgeInstance::spawn(
        &command(),
        &crate::tests::info(GAIN_ID),
        Arc::clone(&signals),
    )
    .unwrap();
    let (params, values) = b.read_params();
    assert_eq!(params.len(), 3);
    assert_eq!(params[2].name, "Test / Crash");
    assert_eq!(values[0], 0.5);
    assert_eq!(b.value_text(0, 0.25).as_deref(), Some("-6.0 dB"));
    let state = b.save_state().unwrap();

    let (mut p, _rx) = b.activate(RATE).unwrap();
    assert_eq!(block(&mut *p, 1.0), (true, 1.0));
    p.set_param(0, 0.25);
    // Under load a block can come back late (dry); the change arrives with the next one.
    let until = Instant::now() + Duration::from_secs(5);
    let mut out = block(&mut *p, 1.0);
    while out == (true, 1.0) && Instant::now() < until {
        out = block(&mut *p, 1.0);
    }
    assert_eq!(out, (true, 0.5));
    // A thousand blocks through the handshake.
    let t = Instant::now();
    for _ in 0..1000 {
        assert!(block(&mut *p, 1.0).0);
    }
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());

    // The plugin crashes its process: the block comes back quickly (dry), this process
    // carries on.
    p.set_param(2, 1.0);
    let t = Instant::now();
    assert_eq!(block(&mut *p, 1.0).1, 1.0);
    assert!(t.elapsed() < Duration::from_secs(1));

    // The instance notices, starts a new process with the last state, asks for a restart.
    let until = Instant::now() + Duration::from_secs(10);
    let notice = loop {
        b.main_thread_work(Instant::now());
        if let Some(n) = b.take_notice() {
            break n;
        }
        assert!(Instant::now() < until, "no notice");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(notice.contains("crashed"), "{notice}");
    // The old processor fails from now on (the engine bypasses it).
    assert!(!block(&mut *p, 1.0).0);
    assert!(Signals::take(&signals.restart));
    assert!(
        !b.try_deactivate(),
        "the engine still has the old processor"
    );
    drop(p);
    assert!(b.try_deactivate());
    let (mut p, _rx) = b.activate(RATE).unwrap();
    // Gain 1 again: the state saved before the edits.
    assert_eq!(block(&mut *p, 0.5), (true, 0.5));
    assert_eq!(b.save_state().unwrap(), state);
    drop(p);
    assert!(b.try_deactivate());
}

#[test]
fn the_host_runs_a_sandboxed_effect_and_bypasses_it_when_it_crashes() {
    let mut host = crate::tests::host();
    host.set_bridge(command());
    host.set_sandboxed(GAIN_ID, true);
    assert!(host.is_sandboxed(GAIN_ID));
    let (mut eng, mut audio) = gt_engine::create(EngineConfig {
        sample_rate: RATE,
        out_channels: 2,
    });
    let mut project = Project::empty();
    let gain = host.instantiate(&crate::tests::info(GAIN_ID)).unwrap();
    let id = gain.instance;
    project.mixer.strips[MASTER].slots[0] = Some(EffectSlot::plugin(gain));
    host.sync(&mut project, Some(&mut eng), true);
    assert_eq!(host.status(id), PluginStatus::Running);
    eng.send(EngineCommand::SetMixer(Box::new(MixerParams::from_mixer(
        &project.mixer,
    ))))
    .unwrap();
    let mut buf = vec![0.0_f32; 2 * 256];
    audio.process(&mut buf);
    host.sync(&mut project, Some(&mut eng), false);
    assert_eq!(host.status(id), PluginStatus::Running);

    // Crash: bypassed, the mix stays finite, and the user hears about it.
    project.plugin_by_instance_mut(id).unwrap().values[2] = 1.0;
    host.sync(&mut project, Some(&mut eng), false);
    audio.process(&mut buf);
    assert!(buf.iter().all(|x| x.is_finite()));
    let until = Instant::now() + Duration::from_secs(10);
    let mut notices = Vec::new();
    while notices.is_empty() {
        host.sync(&mut project, Some(&mut eng), false);
        notices = host.take_notices();
        assert!(Instant::now() < until, "no notice");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(notices[0].contains("crashed"), "{notices:?}");

    // Switched off again, then back in the app's process: it runs with its state.
    project.plugin_by_instance_mut(id).unwrap().values[2] = 0.0;
    host.set_sandboxed(GAIN_ID, false);
    for _ in 0..20 {
        host.sync(&mut project, Some(&mut eng), false);
        audio.process(&mut buf);
        eng.collect_garbage();
        if host.status(id) == PluginStatus::Running && host.retiring.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(host.status(id), PluginStatus::Running);
    assert!(!host.is_sandboxed(GAIN_ID));
}
