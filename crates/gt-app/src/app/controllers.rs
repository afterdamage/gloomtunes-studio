//! Controller mappings (Prompt 33, D149): each MIDI input port gets a mapping file that names
//! its knobs, faders, pads and transport buttons (the user's own matching one, else the
//! generic one, unless the user picked another or none), and optionally a script.
//! Learned bindings win over mappings.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use gt_core::ParamId;
use gt_engine::{EngineCommand, LiveNote};
use gt_project::controller::{self, ControlAction, ControlSource, ControllerMap, TransportButton};
use gt_ui::views::{PlayState, TransportAction};

use super::GloomApp;
use crate::midi_io::{MidiCc, MidiIo};
use crate::script::{Script, ScriptAction, ScriptView};

/// How often new ports get their mapping (so pads stop playing notes before first use).
const REFRESH_EVERY: Duration = Duration::from_secs(1);

/// The mapping a port uses, with its running script.
struct Active {
    map: usize,
    script: Option<Script>,
}

/// Every mapping, and which one each port uses.
pub(super) struct Controllers {
    /// The user's mappings, then the generic one.
    maps: Vec<ControllerMap>,
    /// Mapping files that could not be read, and script errors.
    pub(super) errors: Vec<String>,
    /// The user's choice by port name: a mapping's name, or "" for none.
    pub(super) chosen: BTreeMap<String, String>,
    /// By port number; `None` for a port without a mapping.
    ports: HashMap<u8, Option<Active>>,
    refreshed: Option<Instant>,
}

/// Where user mapping files go.
pub(super) fn folder() -> std::path::PathBuf {
    crate::library::data_folder().join("controllers")
}

impl Controllers {
    pub(super) fn load(chosen: BTreeMap<String, String>) -> Self {
        let (mut maps, errors) = controller::load_folder(&folder());
        maps.push(ControllerMap::generic());
        Self {
            maps,
            errors,
            chosen,
            ports: HashMap::new(),
            refreshed: None,
        }
    }

    /// Mapping names, for the settings panel.
    pub(super) fn names(&self) -> Vec<String> {
        self.maps.iter().map(|m| m.name.clone()).collect()
    }

    /// The mapping port `name` uses.
    pub(super) fn pick(&self, name: &str) -> Option<usize> {
        if let Some(c) = self.chosen.get(name) {
            return self.maps.iter().position(|m| !c.is_empty() && m.name == *c);
        }
        let generic = self.maps.len() - 1;
        let own = self.maps[..generic]
            .iter()
            .position(|m| !m.ports.is_empty() && m.matches(name));
        Some(own.unwrap_or(generic))
    }

    /// Forgets which port uses what (after a choice changed or the files were reloaded).
    pub(super) fn reset(&mut self, midi: &MidiIo) {
        for port in self.ports.keys() {
            midi.set_mapped_notes(*port, Vec::new());
        }
        self.ports.clear();
        self.refreshed = None;
    }

    /// Reads the mapping files again.
    pub(super) fn reload(&mut self, midi: &MidiIo) {
        self.reset(midi);
        let chosen = std::mem::take(&mut self.chosen);
        *self = Self::load(chosen);
    }

    /// Takes port `number`'s mapping out (put it back with [`Self::put`]), setting it up the
    /// first time.
    fn take(&mut self, midi: &MidiIo, number: u8) -> Option<Active> {
        if let Some(a) = self.ports.get_mut(&number) {
            return a.take();
        }
        let map = midi.port_name(number).and_then(|n| self.pick(&n));
        self.ports.insert(number, None);
        let m = &self.maps[map?];
        midi.set_mapped_notes(number, m.notes().collect());
        let script = m.script.as_deref().and_then(|s| {
            Script::load(s)
                .map_err(|e| self.errors.push(format!("{}: script: {e}", m.name)))
                .ok()
        });
        map.map(|map| Active { map, script })
    }

    fn put(&mut self, number: u8, active: Active) {
        self.ports.insert(number, Some(active));
    }

    /// Sets up the mappings of ports seen since the last call, once a second.
    pub(super) fn refresh(&mut self, midi: &MidiIo) {
        if self.refreshed.is_some_and(|t| t.elapsed() < REFRESH_EVERY) {
            return;
        }
        self.refreshed = Some(Instant::now());
        for p in midi.ports() {
            let Some(n) = midi.port_number(&p.name).and_then(|n| u8::try_from(n).ok()) else {
                continue;
            };
            if !self.ports.contains_key(&n) {
                if let Some(a) = self.take(midi, n) {
                    self.put(n, a);
                }
            }
        }
    }

    /// The name of the mapping port `name` uses ("" for none).
    pub(super) fn in_use(&self, name: &str) -> String {
        self.pick(name)
            .map(|i| self.maps[i].name.clone())
            .unwrap_or_default()
    }
}

impl GloomApp {
    /// A controller message or mapped note no learned binding took. True if a mapping or
    /// script took it; `moved` is set when a parameter changed.
    pub(super) fn controller_message(&mut self, cc: MidiCc, moved: &mut bool) -> bool {
        let Some(mut active) = self.controllers.take(&self.midi, cc.port) else {
            return false;
        };
        let used = self.run_mapping(&mut active, cc, moved);
        self.controllers.put(cc.port, active);
        used
    }

    fn run_mapping(&mut self, active: &mut Active, cc: MidiCc, moved: &mut bool) -> bool {
        if let Some(script) = &mut active.script {
            script.set_view(self.script_view(script.keys()));
            let result = if cc.note {
                script.on_note(cc.channel, cc.cc, cc.value)
            } else {
                script.on_cc(cc.channel, cc.cc, cc.value)
            };
            match result {
                Ok((handled, actions)) => {
                    for a in actions {
                        self.script_action(a, cc.at_ns, moved);
                    }
                    if handled {
                        return true;
                    }
                }
                Err(e) => {
                    log::warn!("controller script: {e}");
                    self.toast(format!("Controller script: {e}"));
                }
            }
        }
        let source = if cc.note {
            ControlSource::Note(cc.cc)
        } else {
            ControlSource::Cc(cc.cc)
        };
        let Some(control) = self.controllers.maps[active.map]
            .find(cc.channel, source)
            .cloned()
        else {
            // A mapped note never plays, even when only the script wanted it.
            return cc.note;
        };
        let pressed = cc.value > 0;
        match control.action {
            ControlAction::Param(keys) => {
                if let Some(p) = keys.iter().find_map(|k| self.resolve_key(k)) {
                    let last = p.normalized(&self.project);
                    let t = controller::control_value(control.relative, cc.value, last);
                    self.set_mapped_param(p, t, cc.at_ns, moved);
                }
            }
            ControlAction::Transport(b) if pressed => self.controller_button(b),
            ControlAction::Transport(_) | ControlAction::Script => {}
            ControlAction::PlayNote(key) => {
                let n = if pressed {
                    LiveNote::on(key, f32::from(cc.value) / 127.0)
                } else {
                    LiveNote::off(key)
                };
                self.live.send(n);
            }
        }
        true
    }

    /// The parameter a mapping key names, if it exists now.
    fn resolve_key(&self, key: &str) -> Option<ParamId> {
        let selected = self.project.channels.get(self.rack.selected).map(|c| c.id);
        controller::resolve(key, selected, self.mixer_state.selected)
            .filter(|p| p.is_valid(&self.project))
    }

    fn set_mapped_param(&mut self, p: ParamId, t: f32, at_ns: u64, moved: &mut bool) {
        let v = p.info().from_normalized(t);
        if p.get(&self.project) != Some(v) && p.set(&mut self.project, v) {
            self.auto.cc_moved(p, at_ns, t);
            *moved = true;
        }
    }

    fn controller_button(&mut self, b: TransportButton) {
        match b {
            TransportButton::Play => self.on_transport(TransportAction::PlayPause),
            TransportButton::Stop => self.on_transport(TransportAction::Stop),
            TransportButton::Record => self.on_transport(TransportAction::ToggleRecord),
            TransportButton::Loop => {
                self.transport.loop_enabled = !self.transport.loop_enabled;
                self.on_transport(TransportAction::LoopChanged);
            }
            TransportButton::Metronome => {
                self.transport.metronome = !self.transport.metronome;
                self.on_transport(TransportAction::SetMetronome(self.transport.metronome));
            }
            TransportButton::NextChannel => self.step_channel(1),
            TransportButton::PrevChannel => self.step_channel(-1),
            TransportButton::NextPattern => self.step_pattern(1),
            TransportButton::PrevPattern => self.step_pattern(-1),
        }
    }

    fn step_channel(&mut self, by: isize) {
        let n = self.project.channels.len();
        if n > 0 {
            self.rack.selected = self.rack.selected.saturating_add_signed(by).min(n - 1);
        }
    }

    fn step_pattern(&mut self, by: isize) {
        let ids: Vec<_> = self.project.patterns.iter().map(|p| p.id).collect();
        let Some(i) = ids
            .iter()
            .position(|&id| id == self.project.current_pattern)
        else {
            return;
        };
        let j = i.saturating_add_signed(by).min(ids.len() - 1);
        if j != i {
            self.project.select_pattern(ids[j]);
            self.push_song();
        }
    }

    fn script_view(&self, keys: &[String]) -> ScriptView {
        ScriptView {
            playing: self.transport.state == PlayState::Playing,
            tempo: self.transport.bpm,
            channel: self.rack.selected,
            channel_names: self
                .project
                .channels
                .iter()
                .map(|c| c.name.clone())
                .collect(),
            values: keys
                .iter()
                .filter_map(|k| Some((k.clone(), self.resolve_key(k)?.normalized(&self.project))))
                .collect(),
        }
    }

    fn script_action(&mut self, a: ScriptAction, at_ns: u64, moved: &mut bool) {
        let playing = self.transport.state == PlayState::Playing;
        match a {
            ScriptAction::Play if !playing => self.send(EngineCommand::Play),
            ScriptAction::Stop => self.on_transport(TransportAction::Stop),
            ScriptAction::Play => {}
            ScriptAction::Record => self.controller_button(TransportButton::Record),
            ScriptAction::ToggleLoop => self.controller_button(TransportButton::Loop),
            ScriptAction::ToggleMetronome => self.controller_button(TransportButton::Metronome),
            ScriptAction::SetTempo(bpm) => self.on_transport(TransportAction::SetBpm(bpm)),
            ScriptAction::SelectChannel(i) => {
                if i < self.project.channels.len() {
                    self.rack.selected = i;
                }
            }
            ScriptAction::SetParam(key, t) => {
                if let Some(p) = self.resolve_key(&key) {
                    self.set_mapped_param(p, t, at_ns, moved);
                }
            }
            ScriptAction::PlayNote(key, v) => {
                self.live.send(LiveNote::on(key, v));
            }
            ScriptAction::ReleaseNote(key) => {
                self.live.send(LiveNote::off(key));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_pick_their_own_mapping_then_the_generic_one() {
        let own = ControllerMap::parse(
            r#"{ "name": "Pad box", "ports": ["padbox"], "controls": [] }"#,
            None,
        )
        .unwrap();
        let mut c = Controllers {
            maps: vec![own, ControllerMap::generic()],
            errors: Vec::new(),
            chosen: BTreeMap::new(),
            ports: HashMap::new(),
            refreshed: None,
        };
        assert_eq!(c.in_use("PadBox MIDI 1"), "Pad box");
        assert_eq!(c.in_use("Some keyboard"), "Generic controller");
        c.chosen.insert("Some keyboard".into(), "Pad box".into());
        c.chosen.insert("PadBox MIDI 1".into(), String::new());
        assert_eq!(c.in_use("Some keyboard"), "Pad box");
        assert_eq!(c.pick("PadBox MIDI 1"), None);
        // A picked mapping that is gone means none.
        c.chosen.insert("Other".into(), "Deleted".into());
        assert_eq!(c.pick("Other"), None);
    }
}
