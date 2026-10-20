//! Controller scripts (Prompt 33, D149): a controller mapping can carry a small Rhai script
//! that reacts to the controller's messages.
//!
//! The script defines `fn on_cc(channel, cc, value)` and/or `fn on_note(channel, key,
//! velocity)` (MIDI channels 1 to 16, values 0 to 127); returning `true` means it handled the
//! message, so the mapping's own controls leave it alone. Top-level code runs once when the
//! script loads, and its variables keep their values between calls.
//!
//! What a script can reach is only this: the transport (`play()`, `stop()`, `record()`,
//! `toggle_loop()`, `toggle_metronome()`, `set_tempo(bpm)`, `tempo()`, `playing()`), the mixer
//! (`set_volume(strip, v)`, `set_pan(strip, v)`, with `v` from 0 to 1) and the selected
//! channel (`channel()`, `channel_count()`, `channel_name()`, `select_channel(i)`,
//! `param(key)`, `set_param(key, v)` with keys as in mapping files, `play_note(key, velocity)`,
//! `release_note(key)`), plus `print` to the log. Rhai itself has no file, network or process
//! access, `eval` is switched off, and every call is limited in operations, call depth and
//! data size, so a script that loops forever is stopped instead of freezing the app.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use rhai::{Engine, Scope, AST};

/// Operations one call may take (a few milliseconds at most).
pub const MAX_OPERATIONS: u64 = 200_000;

/// What a script asked for, applied by the app after the call.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptAction {
    Play,
    Stop,
    Record,
    ToggleLoop,
    ToggleMetronome,
    SetTempo(f64),
    SelectChannel(usize),
    /// A parameter key (mapping-file form) and a normalized value.
    SetParam(String, f32),
    PlayNote(u8, f32),
    ReleaseNote(u8),
}

/// What a script can read, filled in by the app before calls.
#[derive(Debug, Clone, Default)]
pub struct ScriptView {
    pub playing: bool,
    pub tempo: f64,
    pub channel: usize,
    pub channel_names: Vec<String>,
    /// Normalized values by key (mapping-file form: `channel/selected/...`, `mixer/N/...`).
    pub values: HashMap<String, f32>,
}

#[derive(Default)]
struct Shared {
    view: ScriptView,
    actions: Vec<ScriptAction>,
}

/// One loaded script.
pub struct Script {
    engine: Engine,
    ast: AST,
    scope: Scope<'static>,
    shared: Rc<RefCell<Shared>>,
    has_cc: bool,
    has_note: bool,
    keys: Vec<String>,
}

impl std::fmt::Debug for Script {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Script").finish_non_exhaustive()
    }
}

fn unit(v: f64) -> f32 {
    v.clamp(0.0, 1.0) as f32
}

fn sandbox(shared: &Rc<RefCell<Shared>>) -> Engine {
    let mut e = Engine::new();
    e.set_max_operations(MAX_OPERATIONS)
        .set_max_call_levels(32)
        .set_max_expr_depths(64, 32)
        .set_max_string_size(4096)
        .set_max_array_size(4096)
        .set_max_map_size(1024)
        .set_max_variables(256)
        .set_max_functions(256);
    e.disable_symbol("eval");
    e.on_print(|s| log::info!("controller script: {s}"));
    e.on_debug(|s, _, _| log::debug!("controller script: {s}"));
    let push = |s: &Rc<RefCell<Shared>>, a: ScriptAction| s.borrow_mut().actions.push(a);
    macro_rules! action {
        ($name:literal, $a:expr) => {{
            let s = Rc::clone(shared);
            e.register_fn($name, move || push(&s, $a));
        }};
    }
    action!("play", ScriptAction::Play);
    action!("stop", ScriptAction::Stop);
    action!("record", ScriptAction::Record);
    action!("toggle_loop", ScriptAction::ToggleLoop);
    action!("toggle_metronome", ScriptAction::ToggleMetronome);
    let s = Rc::clone(shared);
    e.register_fn("set_tempo", move |bpm: f64| {
        push(&s, ScriptAction::SetTempo(bpm.clamp(10.0, 999.0)));
    });
    let s = Rc::clone(shared);
    e.register_fn("select_channel", move |i: i64| {
        if i >= 0 {
            push(&s, ScriptAction::SelectChannel(i as usize));
        }
    });
    let s = Rc::clone(shared);
    e.register_fn("set_param", move |key: &str, v: f64| {
        push(&s, ScriptAction::SetParam(key.to_owned(), unit(v)));
    });
    let s = Rc::clone(shared);
    e.register_fn("set_volume", move |strip: i64, v: f64| {
        push(
            &s,
            ScriptAction::SetParam(format!("mixer/{}/volume", strip.max(0)), unit(v)),
        );
    });
    let s = Rc::clone(shared);
    e.register_fn("set_pan", move |strip: i64, v: f64| {
        push(
            &s,
            ScriptAction::SetParam(format!("mixer/{}/pan", strip.max(0)), unit(v)),
        );
    });
    let s = Rc::clone(shared);
    e.register_fn("play_note", move |key: i64, velocity: i64| {
        let v = (velocity.clamp(1, 127) as f32) / 127.0;
        push(&s, ScriptAction::PlayNote(key.clamp(0, 127) as u8, v));
    });
    let s = Rc::clone(shared);
    e.register_fn("release_note", move |key: i64| {
        push(&s, ScriptAction::ReleaseNote(key.clamp(0, 127) as u8));
    });
    let s = Rc::clone(shared);
    e.register_fn("tempo", move || s.borrow().view.tempo);
    let s = Rc::clone(shared);
    e.register_fn("playing", move || s.borrow().view.playing);
    let s = Rc::clone(shared);
    e.register_fn("channel", move || s.borrow().view.channel as i64);
    let s = Rc::clone(shared);
    e.register_fn("channel_count", move || {
        s.borrow().view.channel_names.len() as i64
    });
    let s = Rc::clone(shared);
    e.register_fn("channel_name", move || {
        let v = &s.borrow().view;
        v.channel_names.get(v.channel).cloned().unwrap_or_default()
    });
    let s = Rc::clone(shared);
    e.register_fn("param", move |key: &str| {
        f64::from(s.borrow().view.values.get(key).copied().unwrap_or(0.0))
    });
    e
}

impl Script {
    /// Compiles `source` and runs its top-level code once.
    pub fn load(source: &str) -> Result<Self, String> {
        let shared = Rc::new(RefCell::new(Shared::default()));
        let engine = sandbox(&shared);
        let ast = engine.compile(source).map_err(|e| e.to_string())?;
        let has = |name: &str| {
            ast.iter_functions()
                .any(|f| f.name == name && f.params.len() == 3)
        };
        let (has_cc, has_note) = (has("on_cc"), has("on_note"));
        let mut scope = Scope::new();
        engine
            .run_ast_with_scope(&mut scope, &ast)
            .map_err(|e| e.to_string())?;
        // Top-level calls (a script that plays on load) are dropped: nothing is set up yet.
        shared.borrow_mut().actions.clear();
        // String literals that look like parameter keys: what `param(key)` may read.
        let mut keys: Vec<String> = source
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|k| k.contains('/'))
            .map(str::to_owned)
            .collect();
        keys.sort();
        keys.dedup();
        Ok(Self {
            engine,
            ast,
            scope,
            shared,
            has_cc,
            has_note,
            keys,
        })
    }

    /// The parameter keys the script names, whose values [`ScriptView::values`] should hold.
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    /// Updates what the script reads.
    pub fn set_view(&mut self, view: ScriptView) {
        self.shared.borrow_mut().view = view;
    }

    fn call(
        &mut self,
        name: &str,
        args: (i64, i64, i64),
    ) -> Result<(bool, Vec<ScriptAction>), String> {
        let options = rhai::CallFnOptions::new().rewind_scope(false);
        let result = self.engine.call_fn_with_options::<rhai::Dynamic>(
            options,
            &mut self.scope,
            &self.ast,
            name,
            args,
        );
        let actions = std::mem::take(&mut self.shared.borrow_mut().actions);
        let handled = result.map_err(|e| e.to_string())?;
        // Anything but `true` leaves the message to the mapping.
        Ok((handled.as_bool().unwrap_or(false), actions))
    }

    /// A controller moved. MIDI channel 0 to 15 (the script sees 1 to 16).
    pub fn on_cc(
        &mut self,
        channel: u8,
        cc: u8,
        value: u8,
    ) -> Result<(bool, Vec<ScriptAction>), String> {
        if !self.has_cc {
            return Ok((false, Vec::new()));
        }
        self.call(
            "on_cc",
            (i64::from(channel) + 1, i64::from(cc), i64::from(value)),
        )
    }

    /// A note of a control marked `script` (velocity 0 for its release).
    pub fn on_note(
        &mut self,
        channel: u8,
        key: u8,
        velocity: u8,
    ) -> Result<(bool, Vec<ScriptAction>), String> {
        if !self.has_note {
            return Ok((false, Vec::new()));
        }
        self.call(
            "on_note",
            (i64::from(channel) + 1, i64::from(key), i64::from(velocity)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_see_the_view_and_ask_for_actions() {
        let mut s = Script::load(
            r#"
            let presses = 0;
            fn on_cc(ch, cc, v) {
                if cc == 20 { set_volume(ch, v / 127.0); return true; }
                if cc == 21 && v > 0 { if playing() { stop() } else { play() } return true; }
                if cc == 22 { set_param("channel/selected/pan", param("channel/selected/pan") + 0.25); }
                if cc == 23 { set_tempo(tempo() * 2.0); select_channel(channel() + 1); }
                false
            }
            fn on_note(ch, key, vel) {
                if vel > 0 { play_note(key + 12, vel) } else { release_note(key + 12) }
                true
            }
            "#,
        )
        .unwrap();
        assert_eq!(s.keys(), ["channel/selected/pan"]);
        let mut view = ScriptView {
            tempo: 120.0,
            channel: 2,
            channel_names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            ..ScriptView::default()
        };
        view.values.insert("channel/selected/pan".into(), 0.5);
        s.set_view(view);
        assert_eq!(
            s.on_cc(3, 20, 127).unwrap(),
            (
                true,
                vec![ScriptAction::SetParam("mixer/4/volume".into(), 1.0)]
            )
        );
        assert_eq!(
            s.on_cc(0, 21, 127).unwrap(),
            (true, vec![ScriptAction::Play])
        );
        assert_eq!(
            s.on_cc(0, 22, 1).unwrap(),
            (
                false,
                vec![ScriptAction::SetParam("channel/selected/pan".into(), 0.75)]
            )
        );
        assert_eq!(
            s.on_cc(0, 23, 1).unwrap().1,
            vec![
                ScriptAction::SetTempo(240.0),
                ScriptAction::SelectChannel(3)
            ]
        );
        assert_eq!(
            s.on_note(0, 60, 100).unwrap(),
            (true, vec![ScriptAction::PlayNote(72, 100.0 / 127.0)])
        );
        assert_eq!(
            s.on_note(0, 60, 0).unwrap().1,
            vec![ScriptAction::ReleaseNote(72)]
        );
    }

    #[test]
    fn a_runaway_script_is_stopped_and_stays_usable() {
        let mut s = Script::load(
            "fn on_cc(ch, cc, v) { if cc == 1 { loop { } } if cc == 2 { on_cc(ch, cc, v) } play(); true }",
        )
        .unwrap();
        let t = std::time::Instant::now();
        let err = s.on_cc(0, 1, 0).unwrap_err();
        assert!(err.to_lowercase().contains("too many operations"), "{err}");
        assert!(t.elapsed().as_secs() < 2);
        // Endless recursion hits the call depth limit.
        assert!(s.on_cc(0, 2, 0).is_err());
        // The next call works again.
        assert_eq!(s.on_cc(0, 3, 0).unwrap(), (true, vec![ScriptAction::Play]));
    }

    #[test]
    fn the_sandbox_has_no_eval_and_reports_errors() {
        assert!(Script::load("eval(\"play()\")").is_err());
        assert!(Script::load("fn on_cc(a, b, c) { ").is_err());
        // Runtime errors come back as errors, not panics; no hook means not handled.
        let mut s = Script::load("fn on_note(a, b, c) { undefined_fn() }").unwrap();
        assert!(s.on_note(0, 1, 1).is_err());
        assert_eq!(s.on_cc(0, 1, 1).unwrap(), (false, vec![]));
        // Big data is refused.
        let mut s =
            Script::load("fn on_cc(a, b, c) { let x = \"ab\"; for i in 0..20 { x += x; } }")
                .unwrap();
        assert!(s.on_cc(0, 0, 0).is_err());
    }
}
