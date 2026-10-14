//! Keyboard shortcuts: the commands the user can bind, their defaults, and matching.
//!
//! Global commands work anywhere; view commands (Prompt 25) only inside the piano roll, the
//! playlist or the channel rack, so two views may use the same key for their own commands.
//! The typing keyboard's piano keys stay fixed.

use egui::{Key, KeyboardShortcut, Modifiers};

/// Where a command's keys work: everywhere, or only in one view (Prompt 25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Anywhere in the program.
    Global,
    /// Inside the piano roll.
    PianoRoll,
    /// Inside the playlist.
    Playlist,
    /// Inside the channel rack.
    Rack,
}

impl Scope {
    /// Every scope, in the order the editor lists them.
    pub const ALL: [Scope; 4] = [
        Scope::Global,
        Scope::PianoRoll,
        Scope::Playlist,
        Scope::Rack,
    ];

    /// Heading in the shortcut editor.
    pub fn label(self) -> &'static str {
        match self {
            Scope::Global => "Everywhere",
            Scope::PianoRoll => "Piano roll",
            Scope::Playlist => "Playlist",
            Scope::Rack => "Channel rack",
        }
    }

    /// True when one shortcut cannot serve both scopes: the same view, or either one global
    /// (global keys are read before any view sees them). Different views may share keys.
    pub fn overlaps(self, other: Scope) -> bool {
        self == other || self == Scope::Global || other == Scope::Global
    }
}

/// A command that can be bound to keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    /// Play, or pause while playing.
    PlayPause,
    /// Arm or disarm recording.
    ToggleRecord,
    /// Step recording on/off.
    ToggleStepRecord,
    /// Turn what was just played into a pattern.
    CaptureTake,
    /// Metronome on/off.
    ToggleMetronome,
    /// Tap tempo (Prompt 25).
    TapTempo,
    /// Switch between pattern and song mode.
    ToggleSongMode,
    /// Computer keyboard as a piano on/off.
    ToggleTypingKeyboard,
    /// Undo the last edit.
    Undo,
    /// Redo.
    Redo,
    /// Show the undo history.
    UndoHistory,
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
    /// Show the project info.
    ProjectInfo,
    /// Show the playlist.
    ShowPlaylist,
    /// Show the channel rack.
    ShowRack,
    /// Show the piano roll.
    ShowPianoRoll,
    /// Show or hide the browser.
    ShowBrowser,
    /// Show the mixer.
    ShowMixer,
    /// Open the plugin browser.
    Plugins,
    /// Open the settings window.
    Settings,
    /// Open the help window.
    Help,
    /// Piano roll: delete the selected notes.
    RollDelete,
    /// Piano roll: select every note.
    RollSelectAll,
    /// Piano roll: duplicate the selection after itself.
    RollDuplicate,
    /// Piano roll: quantize.
    RollQuantize,
    /// Piano roll: draw tool.
    RollDrawTool,
    /// Piano roll: select tool.
    RollSelectTool,
    /// Piano roll: mute or unmute the selection.
    RollMute,
    /// Piano roll: move the selection one grid step earlier.
    RollLeft,
    /// Piano roll: move the selection one grid step later.
    RollRight,
    /// Piano roll: move the selection a little earlier.
    RollNudgeLeft,
    /// Piano roll: move the selection a little later.
    RollNudgeRight,
    /// Piano roll: up a semitone (a scale step with scale snap).
    RollUp,
    /// Piano roll: down a semitone (a scale step with scale snap).
    RollDown,
    /// Piano roll: up an octave.
    RollOctaveUp,
    /// Piano roll: down an octave.
    RollOctaveDown,
    /// Piano roll: one grid step shorter.
    RollShorter,
    /// Piano roll: one grid step longer.
    RollLonger,
    /// Piano roll: a little shorter.
    RollFineShorter,
    /// Piano roll: a little longer.
    RollFineLonger,
    /// Piano roll: velocity up 10 %.
    RollVelocityUp,
    /// Piano roll: velocity down 10 %.
    RollVelocityDown,
    /// Piano roll: velocity up 1 %.
    RollFineVelocityUp,
    /// Piano roll: velocity down 1 %.
    RollFineVelocityDown,
    /// Playlist: delete the selected clips.
    ListDelete,
    /// Playlist: select every clip.
    ListSelectAll,
    /// Playlist: duplicate the selection after itself.
    ListDuplicate,
    /// Playlist: mute or unmute the selection.
    ListMute,
    /// Playlist: draw tool.
    ListDrawTool,
    /// Playlist: select tool.
    ListSelectTool,
    /// Playlist: slice tool.
    ListSliceTool,
    /// Channel rack: select the channel above.
    RackUp,
    /// Channel rack: select the channel below.
    RackDown,
    /// Channel rack: mute or unmute the selected channel.
    RackMute,
    /// Channel rack: solo or unsolo the selected channel.
    RackSolo,
    /// Channel rack: select the next pattern.
    RackNextPattern,
    /// Channel rack: select the previous pattern.
    RackPrevPattern,
    /// Channel rack: open the selected channel in the piano roll.
    RackOpenRoll,
}

/// Scope, settings id, label and default keys of each command, in [`Command::ALL`] order.
const TABLE: [(Command, Scope, &str, &str, &str); 62] = [
    (
        Command::PlayPause,
        Scope::Global,
        "play_pause",
        "Play / pause",
        "Space",
    ),
    (
        Command::ToggleRecord,
        Scope::Global,
        "toggle_record",
        "Record",
        "Ctrl+R",
    ),
    (
        Command::ToggleStepRecord,
        Scope::Global,
        "toggle_step_record",
        "Step record",
        "Ctrl+Shift+R",
    ),
    (
        Command::CaptureTake,
        Scope::Global,
        "capture_take",
        "Capture last take",
        "Ctrl+Shift+T",
    ),
    (
        Command::ToggleMetronome,
        Scope::Global,
        "toggle_metronome",
        "Metronome on/off",
        "",
    ),
    (
        Command::TapTempo,
        Scope::Global,
        "tap_tempo",
        "Tap tempo",
        "",
    ),
    (
        Command::ToggleSongMode,
        Scope::Global,
        "toggle_song_mode",
        "Pattern / song mode",
        "L",
    ),
    (
        Command::ToggleTypingKeyboard,
        Scope::Global,
        "toggle_typing_keyboard",
        "Typing keyboard on/off",
        "Ctrl+T",
    ),
    (Command::Undo, Scope::Global, "undo", "Undo", "Ctrl+Z"),
    (
        Command::Redo,
        Scope::Global,
        "redo",
        "Redo",
        "Ctrl+Shift+Z, Ctrl+Y",
    ),
    (
        Command::UndoHistory,
        Scope::Global,
        "undo_history",
        "Undo history",
        "Ctrl+Alt+Z",
    ),
    (Command::New, Scope::Global, "new", "New project", "Ctrl+N"),
    (
        Command::Open,
        Scope::Global,
        "open",
        "Open project",
        "Ctrl+O",
    ),
    (Command::Save, Scope::Global, "save", "Save", "Ctrl+S"),
    (
        Command::SaveAs,
        Scope::Global,
        "save_as",
        "Save as",
        "Ctrl+Shift+S",
    ),
    (
        Command::ExportAudio,
        Scope::Global,
        "export_audio",
        "Export audio",
        "Ctrl+Shift+E",
    ),
    (
        Command::ProjectInfo,
        Scope::Global,
        "project_info",
        "Project info",
        "Ctrl+Shift+I",
    ),
    (
        Command::ShowPlaylist,
        Scope::Global,
        "show_playlist",
        "Show playlist",
        "F5",
    ),
    (
        Command::ShowRack,
        Scope::Global,
        "show_rack",
        "Show channel rack",
        "F6",
    ),
    (
        Command::ShowPianoRoll,
        Scope::Global,
        "show_piano_roll",
        "Show piano roll",
        "F7",
    ),
    (
        Command::ShowBrowser,
        Scope::Global,
        "show_browser",
        "Show browser",
        "F8",
    ),
    (
        Command::ShowMixer,
        Scope::Global,
        "show_mixer",
        "Show mixer",
        "F9",
    ),
    (
        Command::Plugins,
        Scope::Global,
        "plugins",
        "Plugin browser",
        "Ctrl+P",
    ),
    (
        Command::Settings,
        Scope::Global,
        "settings",
        "Settings",
        "Ctrl+Comma",
    ),
    (Command::Help, Scope::Global, "help", "Help", "F1"),
    (
        Command::RollDelete,
        Scope::PianoRoll,
        "roll.delete",
        "Delete notes",
        "Delete, Backspace",
    ),
    (
        Command::RollSelectAll,
        Scope::PianoRoll,
        "roll.select_all",
        "Select all notes",
        "Ctrl+A",
    ),
    (
        Command::RollDuplicate,
        Scope::PianoRoll,
        "roll.duplicate",
        "Duplicate notes",
        "Ctrl+D",
    ),
    (
        Command::RollQuantize,
        Scope::PianoRoll,
        "roll.quantize",
        "Quantize",
        "Q",
    ),
    (
        Command::RollDrawTool,
        Scope::PianoRoll,
        "roll.draw",
        "Draw tool",
        "P",
    ),
    (
        Command::RollSelectTool,
        Scope::PianoRoll,
        "roll.select",
        "Select tool",
        "E",
    ),
    (
        Command::RollMute,
        Scope::PianoRoll,
        "roll.mute",
        "Mute notes",
        "M",
    ),
    (
        Command::RollLeft,
        Scope::PianoRoll,
        "roll.left",
        "Move left",
        "ArrowLeft",
    ),
    (
        Command::RollRight,
        Scope::PianoRoll,
        "roll.right",
        "Move right",
        "ArrowRight",
    ),
    (
        Command::RollNudgeLeft,
        Scope::PianoRoll,
        "roll.nudge_left",
        "Nudge left",
        "Alt+ArrowLeft",
    ),
    (
        Command::RollNudgeRight,
        Scope::PianoRoll,
        "roll.nudge_right",
        "Nudge right",
        "Alt+ArrowRight",
    ),
    (
        Command::RollUp,
        Scope::PianoRoll,
        "roll.up",
        "Up a step",
        "ArrowUp",
    ),
    (
        Command::RollDown,
        Scope::PianoRoll,
        "roll.down",
        "Down a step",
        "ArrowDown",
    ),
    (
        Command::RollOctaveUp,
        Scope::PianoRoll,
        "roll.octave_up",
        "Up an octave",
        "Shift+ArrowUp",
    ),
    (
        Command::RollOctaveDown,
        Scope::PianoRoll,
        "roll.octave_down",
        "Down an octave",
        "Shift+ArrowDown",
    ),
    (
        Command::RollShorter,
        Scope::PianoRoll,
        "roll.shorter",
        "Shorter",
        "Shift+ArrowLeft",
    ),
    (
        Command::RollLonger,
        Scope::PianoRoll,
        "roll.longer",
        "Longer",
        "Shift+ArrowRight",
    ),
    (
        Command::RollFineShorter,
        Scope::PianoRoll,
        "roll.fine_shorter",
        "A little shorter",
        "Shift+Alt+ArrowLeft",
    ),
    (
        Command::RollFineLonger,
        Scope::PianoRoll,
        "roll.fine_longer",
        "A little longer",
        "Shift+Alt+ArrowRight",
    ),
    (
        Command::RollVelocityUp,
        Scope::PianoRoll,
        "roll.velocity_up",
        "Velocity up",
        "Alt+ArrowUp",
    ),
    (
        Command::RollVelocityDown,
        Scope::PianoRoll,
        "roll.velocity_down",
        "Velocity down",
        "Alt+ArrowDown",
    ),
    (
        Command::RollFineVelocityUp,
        Scope::PianoRoll,
        "roll.fine_velocity_up",
        "Velocity up a little",
        "Shift+Alt+ArrowUp",
    ),
    (
        Command::RollFineVelocityDown,
        Scope::PianoRoll,
        "roll.fine_velocity_down",
        "Velocity down a little",
        "Shift+Alt+ArrowDown",
    ),
    (
        Command::ListDelete,
        Scope::Playlist,
        "playlist.delete",
        "Delete clips",
        "Delete, Backspace",
    ),
    (
        Command::ListSelectAll,
        Scope::Playlist,
        "playlist.select_all",
        "Select all clips",
        "Ctrl+A",
    ),
    (
        Command::ListDuplicate,
        Scope::Playlist,
        "playlist.duplicate",
        "Duplicate clips",
        "Ctrl+D",
    ),
    (
        Command::ListMute,
        Scope::Playlist,
        "playlist.mute",
        "Mute clips",
        "M",
    ),
    (
        Command::ListDrawTool,
        Scope::Playlist,
        "playlist.draw",
        "Draw tool",
        "P",
    ),
    (
        Command::ListSelectTool,
        Scope::Playlist,
        "playlist.select",
        "Select tool",
        "E",
    ),
    (
        Command::ListSliceTool,
        Scope::Playlist,
        "playlist.slice",
        "Slice tool",
        "C",
    ),
    (
        Command::RackUp,
        Scope::Rack,
        "rack.up",
        "Select channel above",
        "ArrowUp",
    ),
    (
        Command::RackDown,
        Scope::Rack,
        "rack.down",
        "Select channel below",
        "ArrowDown",
    ),
    (
        Command::RackMute,
        Scope::Rack,
        "rack.mute",
        "Mute channel",
        "M",
    ),
    (
        Command::RackSolo,
        Scope::Rack,
        "rack.solo",
        "Solo channel",
        "S",
    ),
    (
        Command::RackNextPattern,
        Scope::Rack,
        "rack.next_pattern",
        "Next pattern",
        "Plus, Equals",
    ),
    (
        Command::RackPrevPattern,
        Scope::Rack,
        "rack.prev_pattern",
        "Previous pattern",
        "Minus",
    ),
    (
        Command::RackOpenRoll,
        Scope::Rack,
        "rack.open_roll",
        "Open in piano roll",
        "Enter",
    ),
];

impl Command {
    /// Every command, in the order the editor lists them.
    pub const ALL: [Command; 62] = {
        let mut all = [Command::PlayPause; 62];
        let mut i = 0;
        while i < 62 {
            all[i] = TABLE[i].0;
            i += 1;
        }
        all
    };

    fn row(self) -> &'static (Command, Scope, &'static str, &'static str, &'static str) {
        &TABLE[self as usize]
    }

    /// Where the command's keys work.
    pub fn scope(self) -> Scope {
        self.row().1
    }

    /// What the editor shows.
    pub fn label(self) -> &'static str {
        crate::i18n::tr(self.row().3)
    }

    /// Stable name used in the settings file.
    pub fn id(self) -> &'static str {
        self.row().2
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
                | Command::Help
        )
    }

    /// The shortcuts a fresh install has.
    pub fn defaults(self) -> Vec<KeyboardShortcut> {
        self.row().4.split(',').filter_map(parse_shortcut).collect()
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

/// Makes `keymap` the one views read with [`current`] (the app calls this when it changes).
pub fn install(ctx: &egui::Context, keymap: std::sync::Arc<Keymap>) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(KEYMAP_ID), keymap));
}

/// The keymap the app installed, or the defaults.
pub fn current(ctx: &egui::Context) -> std::sync::Arc<Keymap> {
    ctx.data(|d| d.get_temp::<std::sync::Arc<Keymap>>(egui::Id::new(KEYMAP_ID)))
        .unwrap_or_else(|| {
            static DEFAULT: std::sync::OnceLock<std::sync::Arc<Keymap>> =
                std::sync::OnceLock::new();
            std::sync::Arc::clone(DEFAULT.get_or_init(Default::default))
        })
}

/// Consumes the presses of `scope`'s commands with the installed keymap.
pub fn take_view(ui: &egui::Ui, scope: Scope) -> Vec<Command> {
    current(ui.ctx()).take_view(ui.ctx(), scope)
}

const KEYMAP_ID: &str = "gt-keymap";

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
    /// another one did where both work (it loses it: one shortcut runs one command there).
    pub fn bind(&mut self, cmd: Command, shortcut: KeyboardShortcut) -> Option<Command> {
        let mut taken = None;
        for (c, list) in &mut self.bindings {
            if *c != cmd && c.scope().overlaps(cmd.scope()) && list.contains(&shortcut) {
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
            if *c != cmd && c.scope().overlaps(cmd.scope()) {
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

    /// Shortcuts bound to two commands that work in the same place, as (shortcut, first,
    /// second). Binding never makes one, so this stays empty.
    pub fn conflicts(&self) -> Vec<(KeyboardShortcut, Command, Command)> {
        let mut out = Vec::new();
        for (i, (a, la)) in self.bindings.iter().enumerate() {
            for (b, lb) in &self.bindings[i + 1..] {
                if !a.scope().overlaps(b.scope()) {
                    continue;
                }
                for s in la {
                    if lb.contains(s) {
                        out.push((*s, *a, *b));
                    }
                }
            }
        }
        out
    }

    /// Consumes this frame's key presses that match a global binding and returns their
    /// commands. More specific shortcuts are tried first, so Ctrl+Shift+S is not taken for
    /// Ctrl+S. With `typing` (a text field has focus) only [`Command::works_while_typing`]
    /// commands are matched.
    pub fn take_pressed(&self, ctx: &egui::Context, typing: bool) -> Vec<Command> {
        self.take(ctx, Scope::Global, typing)
    }

    /// Consumes the key presses bound to commands of a view's `scope` and returns them, the
    /// most specific shortcut first (so Shift+Up is not also read as Up). Nothing is taken
    /// while a text field has focus.
    pub fn take_view(&self, ctx: &egui::Context, scope: Scope) -> Vec<Command> {
        if ctx.text_edit_focused() {
            return Vec::new();
        }
        self.take(ctx, scope, false)
    }

    fn take(&self, ctx: &egui::Context, scope: Scope, typing: bool) -> Vec<Command> {
        let mut all: Vec<(Command, KeyboardShortcut)> = self
            .bindings
            .iter()
            .filter(|(c, _)| c.scope() == scope && (!typing || c.works_while_typing()))
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
        assert_eq!(Keymap::default().conflicts(), vec![]);
        for (i, c) in Command::ALL.into_iter().enumerate() {
            assert_eq!(Command::from_id(c.id()), Some(c));
            assert_eq!(c as usize, i, "table order");
            for s in c.defaults() {
                assert_eq!(parse_shortcut(&format_shortcut(&s)), Some(s));
            }
        }
        // Every row of the table parses.
        for (c, _, _, _, keys) in TABLE {
            assert_eq!(
                c.defaults().len(),
                keys.split(',').filter(|k| !k.trim().is_empty()).count(),
                "{keys}"
            );
        }
    }

    #[test]
    fn views_share_keys_but_not_with_global_commands() {
        let mut map = Keymap::default();
        let m = KeyboardShortcut::new(Modifiers::NONE, Key::M);
        // M mutes in the roll, the playlist and the rack at once.
        for c in [Command::RollMute, Command::ListMute, Command::RackMute] {
            assert_eq!(map.get(c), &[m]);
        }
        // Binding it in one view leaves the other views alone...
        assert_eq!(map.bind(Command::RollQuantize, m), Some(Command::RollMute));
        assert!(map.get(Command::RollMute).is_empty());
        assert_eq!(map.get(Command::ListMute), &[m]);
        // ...but a global binding takes it from every view.
        assert!(map.bind(Command::ToggleMetronome, m).is_some());
        for c in [Command::RollQuantize, Command::ListMute, Command::RackMute] {
            assert!(map.get(c).is_empty(), "{c:?}");
        }
        // A view binding takes a global key back.
        let space = KeyboardShortcut::new(Modifiers::NONE, Key::Space);
        assert_eq!(
            map.bind(Command::RackOpenRoll, space),
            Some(Command::PlayPause)
        );
        assert_eq!(map.conflicts(), vec![]);
        // Saved and loaded, the same keys come back.
        let pairs = map.to_pairs();
        let back = Keymap::from_pairs(pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())));
        assert_eq!(back, map);
    }

    #[test]
    fn view_keys_are_read_per_view_most_specific_first() {
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
        input
            .events
            .push(press(Key::ArrowUp, Modifiers::SHIFT | Modifiers::ALT));
        input.events.push(press(Key::ArrowUp, Modifiers::SHIFT));
        input.events.push(press(Key::Space, Modifiers::NONE));
        let mut got = (Vec::new(), Vec::new(), Vec::new());
        let mut out = ctx.run_ui(input, |ui| {
            got.0 = map.take_view(ui.ctx(), Scope::Playlist);
            got.1 = map.take_view(ui.ctx(), Scope::PianoRoll);
            got.2 = map.take_pressed(ui.ctx(), false);
        });
        out.textures_delta.clear();
        assert!(got.0.is_empty());
        assert_eq!(
            got.1,
            vec![Command::RollFineVelocityUp, Command::RollOctaveUp]
        );
        assert_eq!(got.2, vec![Command::PlayPause]);
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
            KeyboardShortcut::new(Modifiers::NONE, Key::F12),
        );
        map.clear(Command::Plugins);
        let pairs = map.to_pairs();
        assert_eq!(
            pairs,
            vec![
                ("toggle_metronome".to_owned(), "F12".to_owned()),
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
