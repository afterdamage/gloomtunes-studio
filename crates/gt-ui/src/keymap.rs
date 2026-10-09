//! Global keyboard shortcuts: the commands the user can bind, their defaults, and matching.
//!
//! Only app-wide commands live here. Keys that mean something inside one view (the piano
//! roll's tools, arrow keys, Delete) and the typing keyboard's piano keys stay fixed.

use egui::{Key, KeyboardShortcut, Modifiers};

/// A command that can be bound to keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    /// Play, or pause while playing.
    PlayPause,
    /// Undo the last edit.
    Undo,
    /// Redo.
    Redo,
    /// New project.
    New,
    /// Open a project.
    Open,
    /// Save.
    Save,
    /// Save under a new name.
    SaveAs,
    /// Export audio.
    ExportAudio,
    /// Show the playlist.
    ShowPlaylist,
    /// Show the channel rack.
    ShowRack,
    /// Show the piano roll.
    ShowPianoRoll,
    /// Show the mixer.
    ShowMixer,
    /// Switch between pattern and song mode.
    ToggleSongMode,
    /// Computer keyboard as a piano on/off.
    ToggleTypingKeyboard,
    /// Arm or disarm recording.
    ToggleRecord,
    /// Metronome on/off.
    ToggleMetronome,
    /// Open the settings window.
    Settings,
    /// Open the plugin browser.
    Plugins,
}

impl Command {
    /// Every command, in the order the editor lists them.
    pub const ALL: [Command; 18] = [
        Command::PlayPause,
        Command::ToggleRecord,
        Command::ToggleMetronome,
        Command::ToggleSongMode,
        Command::ToggleTypingKeyboard,
        Command::Undo,
        Command::Redo,
        Command::New,
        Command::Open,
        Command::Save,
        Command::SaveAs,
        Command::ExportAudio,
        Command::ShowPlaylist,
        Command::ShowRack,
        Command::ShowPianoRoll,
        Command::ShowMixer,
        Command::Plugins,
        Command::Settings,
    ];

    /// What the editor shows.
    pub fn label(self) -> &'static str {
        match self {
            Command::PlayPause => "Play / pause",
            Command::Undo => "Undo",
            Command::Redo => "Redo",
            Command::New => "New project",
            Command::Open => "Open project",
            Command::Save => "Save",
            Command::SaveAs => "Save as",
            Command::ExportAudio => "Export audio",
            Command::ShowPlaylist => "Show playlist",
            Command::ShowRack => "Show channel rack",
            Command::ShowPianoRoll => "Show piano roll",
            Command::ShowMixer => "Show mixer",
            Command::ToggleSongMode => "Pattern / song mode",
            Command::ToggleTypingKeyboard => "Typing keyboard on/off",
            Command::ToggleRecord => "Record",
            Command::ToggleMetronome => "Metronome on/off",
            Command::Settings => "Settings",
            Command::Plugins => "Plugin browser",
        }
    }

    /// Stable name used in the settings file.
    pub fn id(self) -> &'static str {
        match self {
            Command::PlayPause => "play_pause",
            Command::Undo => "undo",
            Command::Redo => "redo",
            Command::New => "new",
            Command::Open => "open",
            Command::Save => "save",
            Command::SaveAs => "save_as",
            Command::ExportAudio => "export_audio",
            Command::ShowPlaylist => "show_playlist",
            Command::ShowRack => "show_rack",
            Command::ShowPianoRoll => "show_piano_roll",
            Command::ShowMixer => "show_mixer",
            Command::ToggleSongMode => "toggle_song_mode",
            Command::ToggleTypingKeyboard => "toggle_typing_keyboard",
            Command::ToggleRecord => "toggle_record",
            Command::ToggleMetronome => "toggle_metronome",
            Command::Settings => "settings",
            Command::Plugins => "plugins",
        }
    }

    /// The command named `id` in the settings file.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.id() == id)
    }

    /// File and window commands also work while a text field has focus (they use Ctrl, which
    /// text fields do not need); the others would steal keys from typing.
    pub fn works_while_typing(self) -> bool {
        matches!(
            self,
            Command::New
                | Command::Open
                | Command::Save
                | Command::SaveAs
                | Command::ExportAudio
                | Command::Settings
        )
    }

    /// The shortcuts a fresh install has.
    pub fn defaults(self) -> Vec<KeyboardShortcut> {
        let ctrl = Modifiers::COMMAND;
        let ctrl_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        let none = Modifiers::NONE;
        let k = KeyboardShortcut::new;
        match self {
            Command::PlayPause => vec![k(none, Key::Space)],
            Command::Undo => vec![k(ctrl, Key::Z)],
            Command::Redo => vec![k(ctrl_shift, Key::Z), k(ctrl, Key::Y)],
            Command::New => vec![k(ctrl, Key::N)],
            Command::Open => vec![k(ctrl, Key::O)],
            Command::Save => vec![k(ctrl, Key::S)],
            Command::SaveAs => vec![k(ctrl_shift, Key::S)],
            Command::ExportAudio => vec![k(ctrl_shift, Key::E)],
            Command::ShowPlaylist => vec![k(none, Key::F5)],
            Command::ShowRack => vec![k(none, Key::F6)],
            Command::ShowPianoRoll => vec![k(none, Key::F7)],
            Command::ShowMixer => vec![k(none, Key::F9)],
            Command::ToggleSongMode => vec![k(none, Key::L)],
            Command::ToggleTypingKeyboard => vec![k(ctrl, Key::T)],
            Command::ToggleRecord => vec![k(ctrl, Key::R)],
            Command::ToggleMetronome => Vec::new(),
            Command::Settings => vec![k(ctrl, Key::Comma)],
            Command::Plugins => vec![k(ctrl, Key::P)],
        }
    }
}

/// The name of the key that shortcuts use: Cmd on macOS, Ctrl elsewhere. Both are
/// [`Modifiers::COMMAND`] in egui, so the same shortcut is Cmd+S on a Mac and Ctrl+S on a PC.
pub const COMMAND_KEY: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Ctrl"
};

/// Shortcut text as shown and saved: `Ctrl+Shift+S` (`Cmd+Shift+S` on macOS), `F5`, `Space`.
pub fn format_shortcut(s: &KeyboardShortcut) -> String {
    let m = s.modifiers;
    let mut out = String::new();
    if m.command || m.ctrl || m.mac_cmd {
        out.push_str(COMMAND_KEY);
        out.push('+');
    }
    if m.shift {
        out.push_str("Shift+");
    }
    if m.alt {
        out.push_str("Alt+");
    }
    out.push_str(s.logical_key.name());
    out
}

/// Parses the output of [`format_shortcut`] (case-insensitive modifiers; `Cmd` = `Ctrl`, so
/// a settings file works on every system).
pub fn parse_shortcut(text: &str) -> Option<KeyboardShortcut> {
    let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
    // "Ctrl++" names the Plus key.
    if text.ends_with("++") {
        parts.retain(|p| !p.is_empty());
        parts.push("Plus");
    }
    let key = Key::from_name(parts.pop()?)?;
    let mut modifiers = Modifiers::NONE;
    for p in parts {
        match p.to_ascii_lowercase().as_str() {
            "ctrl" | "cmd" | "command" => modifiers.command = true,
            "shift" => modifiers.shift = true,
            "alt" | "option" => modifiers.alt = true,
            _ => return None,
        }
    }
    Some(KeyboardShortcut::new(modifiers, key))
}

/// A pressed key turned into the shortcut it binds: Ctrl and Cmd both become
/// [`Modifiers::COMMAND`], like the defaults.
pub fn shortcut_from_press(modifiers: Modifiers, key: Key) -> KeyboardShortcut {
    let m = Modifiers {
        alt: modifiers.alt,
        shift: modifiers.shift,
        command: modifiers.command || modifiers.ctrl || modifiers.mac_cmd,
        ctrl: false,
        mac_cmd: false,
    };
    KeyboardShortcut::new(m, key)
}

fn modifier_count(m: Modifiers) -> u8 {
    u8::from(m.command || m.ctrl) + u8::from(m.shift) + u8::from(m.alt)
}

/// The user's shortcuts: zero or more per command.
#[derive(Debug, Clone, PartialEq)]
pub struct Keymap {
    bindings: Vec<(Command, Vec<KeyboardShortcut>)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: Command::ALL.iter().map(|&c| (c, c.defaults())).collect(),
        }
    }
}

impl Keymap {
    /// The shortcuts bound to `cmd`.
    pub fn get(&self, cmd: Command) -> &[KeyboardShortcut] {
        self.bindings
            .iter()
            .find(|(c, _)| *c == cmd)
            .map_or(&[], |(_, s)| s.as_slice())
    }

    /// The first shortcut of `cmd` as text, for menus and tooltips ("" when unbound).
    pub fn text(&self, cmd: Command) -> String {
        self.get(cmd)
            .first()
            .map(format_shortcut)
            .unwrap_or_default()
    }

    /// Makes `shortcut` the only binding of `cmd`. Returns the command that had it before, if
    /// another one did (it loses it: one shortcut runs one command).
    pub fn bind(&mut self, cmd: Command, shortcut: KeyboardShortcut) -> Option<Command> {
        let mut taken = None;
        for (c, list) in &mut self.bindings {
            if *c != cmd && list.contains(&shortcut) {
                list.retain(|s| *s != shortcut);
                taken = Some(*c);
            }
        }
        if let Some((_, list)) = self.bindings.iter_mut().find(|(c, _)| *c == cmd) {
            *list = vec![shortcut];
        }
        taken
    }

    /// Makes `shortcuts` the bindings of `cmd`, taking them from other commands.
    fn set(&mut self, cmd: Command, shortcuts: Vec<KeyboardShortcut>) {
        for (c, list) in &mut self.bindings {
            if *c != cmd {
                list.retain(|s| !shortcuts.contains(s));
            }
        }
        if let Some((_, list)) = self.bindings.iter_mut().find(|(c, _)| *c == cmd) {
            *list = shortcuts;
        }
    }

    /// Removes every binding of `cmd`.
    pub fn clear(&mut self, cmd: Command) {
        if let Some((_, list)) = self.bindings.iter_mut().find(|(c, _)| *c == cmd) {
            list.clear();
        }
    }

    /// Restores `cmd`'s default shortcuts, taking them from other commands if needed.
    pub fn reset(&mut self, cmd: Command) {
        self.set(cmd, cmd.defaults());
    }

    /// The bindings as text pairs for the settings file; commands left at their defaults are
    /// omitted, so changed defaults reach users who never edited them.
    pub fn to_pairs(&self) -> Vec<(String, String)> {
        self.bindings
            .iter()
            .filter(|(c, list)| *list != c.defaults())
            .map(|(c, list)| {
                let text: Vec<String> = list.iter().map(format_shortcut).collect();
                (c.id().to_owned(), text.join(", "))
            })
            .collect()
    }

    /// The default keymap with the saved pairs applied. Unknown commands and unparsable keys
    /// are skipped.
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut map = Self::default();
        for (id, text) in pairs {
            let Some(cmd) = Command::from_id(id) else {
                continue;
            };
            let shortcuts: Vec<_> = text.split(',').filter_map(parse_shortcut).collect();
            map.set(cmd, shortcuts);
        }
        map
    }

    /// Consumes this frame's key presses that match a binding and returns their commands.
    /// More specific shortcuts are tried first, so Ctrl+Shift+S is not taken for Ctrl+S.
    /// With `typing` (a text field has focus) only [`Command::works_while_typing`] commands
    /// are matched.
    pub fn take_pressed(&self, ctx: &egui::Context, typing: bool) -> Vec<Command> {
        let mut all: Vec<(Command, KeyboardShortcut)> = self
            .bindings
            .iter()
            .filter(|(c, _)| !typing || c.works_while_typing())
            .flat_map(|(c, list)| list.iter().map(move |s| (*c, *s)))
            .collect();
        all.sort_by_key(|(_, s)| std::cmp::Reverse(modifier_count(s.modifiers)));
        let mut out = Vec::new();
        ctx.input_mut(|i| {
            for (c, s) in &all {
                if i.consume_shortcut(s) && !out.contains(c) {
                    out.push(*c);
                }
            }
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_round_trip_as_text() {
        for c in Command::ALL {
            for s in c.defaults() {
                let text = format_shortcut(&s);
                assert_eq!(parse_shortcut(&text), Some(s), "{text}");
            }
        }
        assert_eq!(
            format_shortcut(&KeyboardShortcut::new(
                Modifiers::COMMAND | Modifiers::SHIFT,
                Key::S
            )),
            format!("{COMMAND_KEY}+Shift+S")
        );
        assert_eq!(
            parse_shortcut("cmd+alt+F2"),
            Some(KeyboardShortcut::new(
                Modifiers::COMMAND | Modifiers::ALT,
                Key::F2
            ))
        );
        assert_eq!(parse_shortcut("Hyper+X"), None);
        assert_eq!(parse_shortcut("Ctrl+"), None);
    }

    #[test]
    fn defaults_do_not_collide() {
        let mut seen = Vec::new();
        for c in Command::ALL {
            for s in c.defaults() {
                assert!(!seen.contains(&s), "{} bound twice", format_shortcut(&s));
                seen.push(s);
            }
        }
        for c in Command::ALL {
            assert_eq!(Command::from_id(c.id()), Some(c));
        }
    }

    #[test]
    fn binding_takes_the_shortcut_from_another_command() {
        let mut map = Keymap::default();
        let f5 = KeyboardShortcut::new(Modifiers::NONE, Key::F5);
        assert_eq!(
            map.bind(Command::ShowMixer, f5),
            Some(Command::ShowPlaylist)
        );
        assert!(map.get(Command::ShowPlaylist).is_empty());
        assert_eq!(map.get(Command::ShowMixer), &[f5]);
        map.reset(Command::ShowPlaylist);
        assert_eq!(map.get(Command::ShowPlaylist), &[f5]);
        assert!(map.get(Command::ShowMixer).is_empty());
    }

    #[test]
    fn pairs_keep_only_changes_and_restore_them() {
        let mut map = Keymap::default();
        assert!(map.to_pairs().is_empty());
        map.bind(
            Command::ToggleMetronome,
            KeyboardShortcut::new(Modifiers::NONE, Key::M),
        );
        map.clear(Command::Plugins);
        let pairs = map.to_pairs();
        assert_eq!(
            pairs,
            vec![
                ("toggle_metronome".to_owned(), "M".to_owned()),
                ("plugins".to_owned(), String::new()),
            ]
        );
        let back = Keymap::from_pairs(pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())));
        assert_eq!(back, map);
        // Several shortcuts per command survive too.
        let redo = Keymap::from_pairs([("redo", "Ctrl+Shift+Z, Ctrl+Y"), ("nonsense", "X")]);
        assert_eq!(redo, Keymap::default());
    }

    #[test]
    fn specific_shortcuts_win_over_their_prefix() {
        let ctx = egui::Context::default();
        let map = Keymap::default();
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: Key::S,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::COMMAND | Modifiers::SHIFT,
        });
        let mut got = Vec::new();
        let mut out = ctx.run_ui(input, |ui| got = map.take_pressed(ui.ctx(), false));
        out.textures_delta.clear();
        assert_eq!(got, vec![Command::SaveAs]);
    }

    #[test]
    fn typing_blocks_plain_keys_but_not_file_commands() {
        let ctx = egui::Context::default();
        let map = Keymap::default();
        let press = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let mut input = egui::RawInput::default();
        input.events.push(press(Key::Space, Modifiers::NONE));
        input.events.push(press(Key::S, Modifiers::COMMAND));
        let mut got = Vec::new();
        let mut out = ctx.run_ui(input, |ui| got = map.take_pressed(ui.ctx(), true));
        out.textures_delta.clear();
        assert_eq!(got, vec![Command::Save]);
    }
}
