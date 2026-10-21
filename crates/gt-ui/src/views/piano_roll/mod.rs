//! Piano roll: a custom-painted note editor for one channel of one pattern, plus any other
//! channels the user marks editable (Prompt 21).
//!
//! Layout: toolbar and the selection's note properties on top; below them a keyboard on the
//! left, a bar ruler on top, the note grid, and a lane at the bottom (velocity, another note
//! property, or a pattern automation line).
//!
//! Mouse:
//! - Draw tool: click empty space to add a note (snapped, last used length and colour; with
//!   scale snap, on the nearest key of the scale), drag it to move. Ctrl+drag on empty space
//!   box-selects.
//! - Select tool: drag on empty space box-selects; Shift adds to the selection.
//! - Drag a note to move the selection, drag its right edge to resize it. Ctrl+drag on the
//!   right edge (or any edge drag with Stretch on) scales the whole selection in time.
//! - Right-click or right-drag deletes notes.
//! - Bottom lane: drag to paint the shown property (only selected notes, when there is a
//!   selection).
//! - Wheel scrolls (Shift: sideways), Ctrl+wheel zooms time, Alt+wheel zooms keys.
//!
//! Keys (while the pointer is over the roll): Delete, Ctrl+A, Ctrl+C/X/V (paste lands at the
//! mouse over the grid, else at the playhead), Ctrl+D (duplicate after), M mute, Q quantize,
//! P draw tool, E select tool. Arrows: Left/Right move by snap (Alt: fine), Shift+Left/Right
//! change length by snap (Shift+Alt: fine); Up/Down transpose (a scale degree with scale snap;
//! Shift: an octave), Alt+Up/Down velocity by 10 % (Shift+Alt: 1 %). Undo/redo is handled by
//! the app.
//!
//! Performance: notes are kept sorted by start, so only notes that can reach the visible range
//! are visited (binary search back by the longest note length). Ghost notes are culled the
//! same way. While a drag is in progress, or while other channels are editable (their notes
//! follow the edited channel's in the working list), a linear range check is used instead.

use egui::{pos2, vec2, Color32, Pos2, Rect, Response, RichText, Sense, Stroke, Ui};
use gt_core::{AutoPoint, Automation, ControlKind, ControlPoint, Note, ParamId, PPQ};

use crate::keymap::{Command, Scope};
use crate::GloomTheme;

mod lanes;
mod tools;
use crate::i18n::tr;
use lanes::LaneSnapshot;
pub use lanes::{Lane, LaneDraw};

const KEYBOARD_W: f32 = 46.0;
const RULER_H: f32 = 18.0;
const VELOCITY_H: f32 = 64.0;
const RESIZE_GRIP_PX: f32 = 6.0;
const MIN_PX_PER_BEAT: f32 = 6.0;
const MAX_PX_PER_BEAT: f32 = 600.0;
const MIN_KEY_H: f32 = 5.0;
const MAX_KEY_H: f32 = 40.0;
const KEYS: i32 = 128;
/// Fine step for nudging and length keys, in ticks (a 1/384 note).
const FINE_TICKS: i64 = PPQ / 96;

/// Grid snap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snap {
    /// No snap (1 tick).
    Off,
    /// A straight division of the whole note: 4, 8, 16 or 32.
    Straight(u8),
    /// A triplet division: 4, 8, 16 or 32 (three in the space of two).
    Triplet(u8),
}

impl Snap {
    /// Snap choices in menu order.
    pub const ALL: [Snap; 9] = [
        Snap::Straight(4),
        Snap::Straight(8),
        Snap::Straight(16),
        Snap::Straight(32),
        Snap::Triplet(4),
        Snap::Triplet(8),
        Snap::Triplet(16),
        Snap::Triplet(32),
        Snap::Off,
    ];

    /// Grid size in ticks.
    pub fn ticks(self) -> i64 {
        match self {
            Snap::Off => 1,
            Snap::Straight(d) => PPQ * 4 / i64::from(d.max(1)),
            Snap::Triplet(d) => PPQ * 4 * 2 / (3 * i64::from(d.max(1))),
        }
    }

    /// Menu label.
    pub fn label(self) -> String {
        match self {
            Snap::Off => "Off".to_owned(),
            Snap::Straight(d) => format!("1/{d}"),
            Snap::Triplet(d) => format!("1/{d}T"),
        }
    }

    /// `t` rounded down to the grid.
    pub fn floor(self, t: i64) -> i64 {
        let g = self.ticks();
        t.div_euclid(g) * g
    }

    /// `t` rounded to the nearest grid line.
    pub fn round(self, t: i64) -> i64 {
        let g = self.ticks();
        (t + g / 2).div_euclid(g) * g
    }
}

/// Scales for highlighting rows. Intervals in semitones from the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleKind {
    /// No highlighting.
    Off,
    /// Ionian.
    Major,
    /// Aeolian.
    Minor,
    /// Natural minor with a raised 7th.
    HarmonicMinor,
    /// Minor with a raised 6th.
    Dorian,
    /// Minor with a lowered 2nd.
    Phrygian,
    /// Major with a lowered 7th.
    Mixolydian,
    /// Five-note major.
    MajorPentatonic,
    /// Five-note minor.
    MinorPentatonic,
    /// Minor pentatonic plus the flat fifth.
    Blues,
}

impl ScaleKind {
    /// All choices in menu order.
    pub const ALL: [ScaleKind; 10] = [
        ScaleKind::Off,
        ScaleKind::Major,
        ScaleKind::Minor,
        ScaleKind::HarmonicMinor,
        ScaleKind::Dorian,
        ScaleKind::Phrygian,
        ScaleKind::Mixolydian,
        ScaleKind::MajorPentatonic,
        ScaleKind::MinorPentatonic,
        ScaleKind::Blues,
    ];

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            ScaleKind::Off => "No scale",
            ScaleKind::Major => "Major",
            ScaleKind::Minor => "Minor",
            ScaleKind::HarmonicMinor => "Harmonic minor",
            ScaleKind::Dorian => "Dorian",
            ScaleKind::Phrygian => "Phrygian",
            ScaleKind::Mixolydian => "Mixolydian",
            ScaleKind::MajorPentatonic => "Major pentatonic",
            ScaleKind::MinorPentatonic => "Minor pentatonic",
            ScaleKind::Blues => "Blues",
        }
    }

    fn intervals(self) -> &'static [u8] {
        match self {
            ScaleKind::Off => &[],
            ScaleKind::Major => &[0, 2, 4, 5, 7, 9, 11],
            ScaleKind::Minor => &[0, 2, 3, 5, 7, 8, 10],
            ScaleKind::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            ScaleKind::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            ScaleKind::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
            ScaleKind::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
            ScaleKind::MajorPentatonic => &[0, 2, 4, 7, 9],
            ScaleKind::MinorPentatonic => &[0, 3, 5, 7, 10],
            ScaleKind::Blues => &[0, 3, 5, 6, 7, 10],
        }
    }

    /// True if `key` is in this scale on `root` (0 = C). Always false for `Off`.
    pub fn contains(self, root: u8, key: u8) -> bool {
        let degree = (i32::from(key) - i32::from(root)).rem_euclid(12) as u8;
        self.intervals().contains(&degree)
    }

    fn has(self, root: u8, key: i32) -> bool {
        (0..KEYS).contains(&key) && self.contains(root, key as u8)
    }

    /// `key` moved onto the scale: itself if it is in it, else the next key of the scale in
    /// direction `dir` (up if positive, down if negative; the other way at the end of the
    /// range), or the nearest one when `dir` is 0 (ties go down). Unchanged for `Off`.
    pub fn snap(self, root: u8, key: i32, dir: i32) -> i32 {
        if self == ScaleKind::Off || self.has(root, key) {
            return key;
        }
        let first = |sign: i32| (1..12).map(|d| key + d * sign).find(|&k| self.has(root, k));
        match dir.signum() {
            0 => {
                let (down, up) = (first(-1), first(1));
                match (down, up) {
                    (Some(d), Some(u)) if u - key < key - d => u,
                    (Some(d), _) => d,
                    (None, u) => u.unwrap_or(key),
                }
            }
            s => first(s).or_else(|| first(-s)).unwrap_or(key),
        }
    }

    /// The next key of the scale from `key` in direction `dir` (±1), if there is one.
    pub fn step(self, root: u8, key: i32, dir: i32) -> Option<i32> {
        if self == ScaleKind::Off {
            let k = key + dir;
            return (0..KEYS).contains(&k).then_some(k);
        }
        (1..=12)
            .map(|d| key + d * dir.signum())
            .find(|&k| self.has(root, k))
    }
}

/// A note property shown and painted in the bottom lane, as 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteProp {
    /// Note-on velocity.
    Velocity,
    /// Release velocity.
    Release,
    /// Stereo position (0.5 centre).
    Pan,
    /// Fine pitch, -100 to +100 cents (0.5 none).
    Fine,
    /// Generic modulation X.
    ModX,
    /// Generic modulation Y.
    ModY,
}

impl NoteProp {
    /// All choices in menu order.
    pub const ALL: [NoteProp; 6] = [
        NoteProp::Velocity,
        NoteProp::Release,
        NoteProp::Pan,
        NoteProp::Fine,
        NoteProp::ModX,
        NoteProp::ModY,
    ];

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            NoteProp::Velocity => "Velocity",
            NoteProp::Release => "Release",
            NoteProp::Pan => "Pan",
            NoteProp::Fine => "Fine pitch",
            NoteProp::ModX => "Mod X",
            NoteProp::ModY => "Mod Y",
        }
    }

    /// The value as 0 to 1.
    pub fn get(self, n: &Note) -> f32 {
        match self {
            NoteProp::Velocity => n.velocity,
            NoteProp::Release => n.release,
            NoteProp::Pan => (n.pan + 1.0) * 0.5,
            NoteProp::Fine => (n.fine + 100.0) / 200.0,
            NoteProp::ModX => n.mod_x,
            NoteProp::ModY => n.mod_y,
        }
    }

    /// Sets the value from 0 to 1.
    pub fn set(self, n: &mut Note, v: f32) {
        let v = v.clamp(0.0, 1.0);
        match self {
            NoteProp::Velocity => n.velocity = v,
            NoteProp::Release => n.release = v,
            NoteProp::Pan => n.pan = v * 2.0 - 1.0,
            NoteProp::Fine => n.fine = (v * 200.0 - 100.0).round(),
            NoteProp::ModX => n.mod_x = v,
            NoteProp::ModY => n.mod_y = v,
        }
    }

    /// The neutral value, where the lane's stems start.
    fn neutral(self) -> f32 {
        match self {
            NoteProp::Velocity => 0.0,
            _ => 0.5,
        }
    }
}

/// Fill colour of note colour group `group` (0 is the theme accent).
pub fn group_color(theme: &GloomTheme, group: u8) -> Color32 {
    if group == 0 {
        return theme.accent;
    }
    let hue = f32::from(group - 1) / f32::from(Note::COLORS - 1);
    egui::ecolor::Hsva::new(hue, 0.55, 0.85, 1.0).into()
}

/// Scales `notes` in time so the span `[from, to)` becomes `[from, new_end)`: starts move away
/// from `from` and lengths grow by the same factor (rounded, at least one tick).
pub fn stretch_notes(notes: &[Note], from: i64, to: i64, new_end: i64) -> Vec<Note> {
    let span = (to - from).max(1) as f64;
    let f = ((new_end - from).max(1) as f64) / span;
    notes
        .iter()
        .map(|n| {
            let start = from + ((n.start - from) as f64 * f).round() as i64;
            let end = from + ((n.start + n.length - from) as f64 * f).round() as i64;
            Note {
                start,
                length: (end - start).max(1),
                ..*n
            }
        })
        .collect()
}

/// Note name for a MIDI key, FL-style octave numbering (60 = C5).
pub fn key_name(key: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!("{}{}", NAMES[usize::from(key % 12)], key / 12)
}

fn is_black(key: i32) -> bool {
    matches!(key.rem_euclid(12), 1 | 3 | 6 | 8 | 10)
}

/// Editing tool for the left mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// Click adds notes.
    Draw,
    /// Drag selects.
    Select,
}

#[derive(Debug, Clone)]
enum Drag {
    /// Moving the selection; `anchor` is the grabbed note.
    Move {
        anchor: usize,
        grab_tick: i64,
        grab_key: i32,
        originals: Vec<(usize, Note)>,
        last_key: u8,
    },
    /// Resizing the selection by the grabbed note's end.
    Resize {
        anchor: usize,
        originals: Vec<(usize, Note)>,
    },
    /// Scaling the selection in time; it spans `[from, to)` before the drag.
    Stretch {
        originals: Vec<(usize, Note)>,
        from: i64,
        to: i64,
    },
    /// Rubber-band selection from a grid position.
    Box { from: Pos2, additive: bool },
    /// Right-button erasing.
    Erase,
    /// Painting a note property freehand.
    Velocity,
    /// Drawing the shown controller freehand; `last` is the tick drawn last.
    Control { last: i64 },
    /// A line or curve in the lane from `from` (tick, lane value).
    LaneLine {
        from: (i64, f32),
        snapshot: LaneSnapshot,
    },
    /// Drawing the shown automation line; `last` is the tick drawn last.
    Automation { last: i64 },
}

/// View and editing state of the piano roll that is not part of the document.
#[derive(Debug, Clone)]
pub struct PianoRollState {
    /// Horizontal zoom: points per quarter note.
    pub px_per_beat: f32,
    /// Vertical zoom: row height in points.
    pub key_height: f32,
    /// Tick at the left edge of the grid.
    pub scroll_tick: f64,
    /// Rows scrolled from the top (key 127).
    pub scroll_rows: f32,
    /// Grid snap.
    pub snap: Snap,
    /// Left-button tool.
    pub tool: Tool,
    /// Scale for row highlighting.
    pub scale: ScaleKind,
    /// Scale root, 0 = C.
    pub scale_root: u8,
    /// Show other channels' notes.
    pub ghosts: bool,
    /// Drawn and moved notes land on keys of the scale.
    pub scale_snap: bool,
    /// Show only rows in the scale or with notes.
    pub fold: bool,
    /// Colour group for new notes.
    pub color: u8,
    /// Dragging a note's end scales the selection in time.
    pub stretch: bool,
    /// What the bottom lane shows.
    pub lane: Lane,
    /// How lane drags draw.
    pub lane_draw: LaneDraw,
    /// Bend of curve drags in the lane, -1 to 1.
    pub lane_tension: f32,
    /// Controller number picked in the lane menu.
    pub cc_pick: u8,
    /// Chord placed by drawing a note (None draws single notes).
    pub chord: Option<gt_core::note_tools::ChordKind>,
    /// Inversion of the chord stamp.
    pub chord_inversion: u8,
    tools: tools::ToolSettings,
    dialog: Option<tools::ToolDialog>,
    pending_tool: Option<tools::ToolKind>,
    /// Other channels whose notes are edited with the channel's (by channel id).
    pub editable: Vec<u32>,
    /// Selection flags, parallel to the edited notes.
    pub selected: Vec<bool>,
    /// Quantize strength, 0 to 1.
    pub quantize_strength: f32,
    /// Humanize timing range in ticks (±).
    pub humanize_ticks: i64,
    /// Humanize velocity range (± fraction).
    pub humanize_velocity: f32,
    last_length: i64,
    last_velocity: f32,
    /// Copied notes and the channel each came from.
    clipboard: Vec<(Note, u32)>,
    /// Which list each working note belongs to: 0 the edited channel, k the k-th editable
    /// ghost of this frame. Parallel to `selected`.
    owner: Vec<u16>,
    drag: Option<Drag>,
    audition_key: Option<u8>,
    /// What the roll is editing; selection resets when it changes.
    target: Option<(u32, u32)>,
    /// Scroll to the notes on the next frame (set when the target changes).
    frame_notes: bool,
    /// Geometry of the last frame (for tests).
    #[cfg(test)]
    last_geo: Option<Geo>,
}

impl Default for PianoRollState {
    fn default() -> Self {
        Self {
            px_per_beat: 64.0,
            key_height: 14.0,
            scroll_tick: 0.0,
            scroll_rows: (127 - 79) as f32,
            snap: Snap::Straight(16),
            tool: Tool::Draw,
            scale: ScaleKind::Off,
            scale_root: 0,
            ghosts: true,
            scale_snap: false,
            fold: false,
            color: 0,
            stretch: false,
            lane: Lane::default(),
            lane_draw: LaneDraw::Freehand,
            lane_tension: 0.5,
            cc_pick: 7,
            chord: None,
            chord_inversion: 0,
            tools: tools::ToolSettings::default(),
            dialog: None,
            pending_tool: None,
            editable: Vec::new(),
            selected: Vec::new(),
            quantize_strength: 1.0,
            humanize_ticks: 20,
            humanize_velocity: 0.1,
            last_length: PPQ / 4,
            last_velocity: gt_core::project::DEFAULT_VELOCITY,
            clipboard: Vec::new(),
            owner: Vec::new(),
            drag: None,
            audition_key: None,
            target: None,
            frame_notes: false,
            #[cfg(test)]
            last_geo: None,
        }
    }
}

impl PianoRollState {
    /// Tells the roll which (pattern, channel) it edits; a change clears the selection and
    /// any drag in progress.
    pub fn set_target(&mut self, pattern: u32, channel: u32) {
        if self.target != Some((pattern, channel)) {
            if self.target.map(|t| t.1) != Some(channel) {
                self.editable.clear();
            }
            self.target = Some((pattern, channel));
            self.frame_notes = true;
            self.selected.clear();
            self.drag = None;
            self.dialog = None;
            self.pending_tool = None;
        }
    }

    /// True while a mouse gesture or a tool window is changing notes (the app commits undo
    /// steps after it).
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some() || self.dialog.is_some()
    }

    /// Abandons any gesture in progress, e.g. before the notes are replaced by undo or when the
    /// roll is hidden mid-drag. Returns the note-off for a key still being auditioned.
    pub fn cancel(&mut self) -> Option<PianoRollAction> {
        self.drag = None;
        self.dialog = None;
        self.audition_key
            .take()
            .map(|key| PianoRollAction::AuditionOff { key })
    }
}

/// Another channel's notes in the same pattern: drawn as ghosts, or edited along with the
/// channel's when the user marks it editable.
pub struct GhostChannel<'a> {
    /// Channel id.
    pub id: u32,
    /// Channel name.
    pub name: &'a str,
    /// Its notes, sorted by start.
    pub notes: &'a mut Vec<Note>,
}

/// What the roll edits and shows.
pub struct PianoRollView<'a> {
    /// The edited notes (kept sorted by start, key between frames).
    pub notes: &'a mut Vec<Note>,
    /// Other channels' notes in the same pattern.
    pub ghosts: Vec<GhostChannel<'a>>,
    /// Pattern length in ticks (shown as the end of the loop).
    pub pattern_len: i64,
    /// Ticks per bar.
    pub bar_ticks: i64,
    /// Playhead in pattern ticks while playing.
    pub playhead: Option<i64>,
    /// Name of the edited channel.
    pub channel_name: &'a str,
    /// The channel's controller data in this pattern, sorted by tick (drawn and edited in
    /// the bottom lane).
    pub controls: &'a mut Vec<ControlPoint>,
    /// The pattern's automation lines (D112), drawn and edited in the bottom lane.
    pub automation: &'a mut Vec<Automation>,
    /// Their parameters' names, in the same order.
    pub automation_names: Vec<String>,
    /// Parameters of the channel's instrument that can get a pattern automation line, with
    /// their names.
    pub params: Vec<(ParamId, String)>,
}

/// Things the app must act on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PianoRollAction {
    /// Notes changed.
    Changed,
    /// Start hearing a key on the edited channel.
    AuditionOn {
        /// MIDI key.
        key: u8,
        /// Velocity.
        velocity: f32,
    },
    /// Stop hearing a key.
    AuditionOff {
        /// MIDI key.
        key: u8,
    },
    /// Quantize the selection (or all notes) with the state's snap and strength.
    Quantize,
    /// Humanize the selection (or all notes) with the state's amounts.
    Humanize,
    /// Delete the channel's recorded data for one controller, or all of it.
    ClearControls(Option<ControlKind>),
    /// The pattern's automation changed.
    AutomationChanged,
    /// Give the pattern an automation line for this parameter and show it in the lane.
    AutomateParam(ParamId),
}

/// Geometry of the editing area for one frame.
#[derive(Debug, Clone)]
struct Geo {
    grid: Rect,
    px_per_tick: f32,
    key_h: f32,
    scroll_tick: f64,
    scroll_rows: f32,
    /// Folded view: the keys shown, top row first (highest key). `None` shows all 128.
    rows: Option<Vec<i32>>,
}

impl Geo {
    fn x(&self, tick: i64) -> f32 {
        self.grid.left() + ((tick as f64 - self.scroll_tick) as f32) * self.px_per_tick
    }
    fn tick(&self, x: f32) -> i64 {
        (self.scroll_tick + f64::from((x - self.grid.left()) / self.px_per_tick)).floor() as i64
    }
    fn row_count(&self) -> i32 {
        self.rows.as_ref().map_or(KEYS, |r| r.len() as i32)
    }
    /// Row of `key` from the top (a hidden key sits where it would be).
    fn row(&self, key: i32) -> i32 {
        match &self.rows {
            None => KEYS - 1 - key,
            Some(r) => r.partition_point(|&k| k > key) as i32,
        }
    }
    fn y(&self, key: i32) -> f32 {
        self.grid.top() + (self.row(key) as f32 - self.scroll_rows) * self.key_h
    }
    fn key(&self, y: f32) -> i32 {
        let row = ((y - self.grid.top()) / self.key_h + self.scroll_rows).floor() as i32;
        let row = row.clamp(0, self.row_count() - 1);
        match &self.rows {
            None => KEYS - 1 - row,
            Some(r) => r[row as usize],
        }
    }
    /// Keys of the rows on screen, bottom to top.
    fn visible_keys(&self) -> Vec<i32> {
        let first = self.scroll_rows.floor().max(0.0) as i32;
        let last = (self.scroll_rows + self.grid.height() / self.key_h).ceil() as i32;
        let rows = first..=last.min(self.row_count() - 1);
        match &self.rows {
            None => rows.rev().map(|r| KEYS - 1 - r).collect(),
            Some(keys) => rows.rev().map(|r| keys[r as usize]).collect(),
        }
    }
    fn note_rect(&self, n: &Note) -> Rect {
        let y = self.y(i32::from(n.key));
        Rect::from_min_max(
            pos2(self.x(n.start), y + 1.0),
            pos2(
                self.x(n.start + n.length.max(1)).max(self.x(n.start) + 2.0),
                y + self.key_h - 1.0,
            ),
        )
    }
    fn tick_range(&self) -> (i64, i64) {
        (
            self.tick(self.grid.left()) - 1,
            self.tick(self.grid.right()) + 1,
        )
    }
}

/// Indices of notes that can be visible in `[t0, t1]`. `sorted` enables the binary search.
fn visible_range(notes: &[Note], t0: i64, t1: i64, sorted: bool) -> Vec<usize> {
    if !sorted {
        return (0..notes.len())
            .filter(|&i| notes[i].start <= t1 && notes[i].start + notes[i].length >= t0)
            .collect();
    }
    let max_len = notes.iter().map(|n| n.length).max().unwrap_or(0);
    let lo = notes.partition_point(|n| n.start < t0 - max_len);
    let hi = notes.partition_point(|n| n.start <= t1);
    (lo..hi.max(lo))
        .filter(|&i| notes[i].start + notes[i].length >= t0)
        .collect()
}

/// Draws the piano roll and edits `view.notes` (and the editable ghosts' notes) in place.
pub fn piano_roll(
    ui: &mut Ui,
    theme: &GloomTheme,
    st: &mut PianoRollState,
    mut view: PianoRollView<'_>,
) -> Vec<PianoRollAction> {
    let mut actions = Vec::new();
    // One working list: the channel's notes, then each editable ghost's.
    st.editable
        .retain(|id| view.ghosts.iter().any(|g| g.id == *id));
    let editable: Vec<usize> = (0..view.ghosts.len())
        .filter(|&i| st.editable.contains(&view.ghosts[i].id))
        .collect();
    let mut notes = std::mem::take(view.notes);
    st.owner.clear();
    st.owner.resize(notes.len(), 0);
    let mut owner_ids = vec![st.target.map_or(u32::MAX, |t| t.1)];
    for (k, &gi) in editable.iter().enumerate() {
        let g = &mut view.ghosts[gi];
        st.owner
            .extend(std::iter::repeat_n(k as u16 + 1, g.notes.len()));
        notes.append(g.notes);
        owner_ids.push(g.id);
    }
    st.selected.resize(notes.len(), false);
    let notes = &mut notes;
    let mut changed = toolbar(ui, theme, st, &mut view, notes, &mut actions);
    if let Some(kind) = st.pending_tool.take() {
        tools::open(st, notes, kind);
    }
    changed |= tools::tool_window(ui.ctx(), theme, st, notes, view.bar_ticks);
    // A tool window owns the notes until it closes.
    let tool_open = st.dialog.is_some();
    if !tool_open {
        changed |= note_panel(ui, theme, st, notes);
    }
    ui.add_space(4.0);

    let area = ui.available_rect_before_wrap();
    let area = Rect::from_min_size(area.min, vec2(area.width(), area.height().max(160.0)));
    ui.allocate_rect(area, Sense::hover());
    let grid = Rect::from_min_max(
        pos2(area.left() + KEYBOARD_W, area.top() + RULER_H),
        pos2(area.right(), area.bottom() - VELOCITY_H - 4.0),
    );
    let lane = Rect::from_min_max(
        pos2(grid.left(), area.bottom() - VELOCITY_H),
        pos2(grid.right(), area.bottom()),
    );
    let keyboard = Rect::from_min_max(
        pos2(area.left(), grid.top()),
        pos2(grid.left(), grid.bottom()),
    );

    let rows = st.fold.then(|| {
        fold_rows(
            st,
            notes,
            view.ghosts
                .iter()
                .filter(|g| st.ghosts && !st.editable.contains(&g.id))
                .map(|g| g.notes.as_slice()),
        )
    });
    let rows = rows.flatten();
    let row_count = rows.as_ref().map_or(KEYS, |r| r.len() as i32);
    // Zoom and scroll first, so this frame draws with the new view.
    let hovered = ui.rect_contains_pointer(area);
    if hovered {
        handle_wheel(ui, st, grid);
    }
    if std::mem::take(&mut st.frame_notes) {
        frame_notes(st, notes, rows.as_deref(), grid);
    }
    clamp_scroll(st, grid, row_count);
    let geo = Geo {
        grid,
        px_per_tick: st.px_per_beat / PPQ as f32,
        key_h: st.key_height,
        scroll_tick: st.scroll_tick,
        scroll_rows: st.scroll_rows,
        rows,
    };

    #[cfg(test)]
    {
        st.last_geo = Some(geo.clone());
    }
    let grid_resp = ui.interact(grid, ui.id().with("roll_grid"), Sense::click_and_drag());
    let lane_resp = ui.interact(lane, ui.id().with("roll_lane"), Sense::click_and_drag());
    let key_resp = ui.interact(keyboard, ui.id().with("roll_keys"), Sense::click_and_drag());

    let single = editable.is_empty();
    if !tool_open {
        changed |= grid_input(ui, st, &geo, &grid_resp, notes, single, &mut actions);
        let (lane_changed, auto_changed) = lanes::lane_input(
            ui,
            st,
            &geo,
            lane,
            &lane_resp,
            notes,
            view.controls,
            view.automation,
            view.pattern_len,
        );
        changed |= lane_changed;
        if auto_changed {
            actions.push(PianoRollAction::AutomationChanged);
        }
    }
    keyboard_input(ui, st, &geo, &key_resp, &mut actions);
    if hovered && !tool_open && !ui.ctx().text_edit_focused() {
        changed |= key_commands(ui, st, &geo, view.playhead, notes, &owner_ids, &mut actions);
    }
    if changed {
        actions.push(PianoRollAction::Changed);
    }

    let sorted = st.drag.is_none() && st.dialog.is_none() && single;
    {
        let ghosts: Vec<&[Note]> = view
            .ghosts
            .iter()
            .filter(|g| !st.editable.contains(&g.id))
            .map(|g| g.notes.as_slice())
            .collect();
        paint(
            ui,
            theme,
            st,
            &geo,
            area,
            notes,
            &ghosts,
            view.pattern_len,
            view.bar_ticks,
            view.playhead,
            sorted,
        );
    }
    lanes::paint_lane(
        ui,
        theme,
        st,
        &geo,
        area,
        lane,
        notes,
        view.controls,
        view.automation,
        sorted,
    );

    // Hand each note back to its channel.
    let notes = std::mem::take(notes);
    if single {
        *view.notes = notes;
    } else {
        let mut lists = split_owners(st, notes);
        for (k, &gi) in editable.iter().enumerate().rev() {
            *view.ghosts[gi].notes = lists.pop().unwrap_or_default();
            debug_assert_eq!(lists.len(), k + 1);
        }
        *view.notes = lists.pop().unwrap_or_default();
    }
    actions
}

/// Splits the working list into one list per owner (each keeps its order) and renumbers the
/// selection and any drag in progress to the order the next frame merges them in.
fn split_owners(st: &mut PianoRollState, notes: Vec<Note>) -> Vec<Vec<Note>> {
    let owners = st
        .owner
        .iter()
        .map(|&o| usize::from(o) + 1)
        .max()
        .unwrap_or(1);
    let mut counts = vec![0usize; owners];
    for &o in &st.owner {
        counts[usize::from(o)] += 1;
    }
    let mut next: Vec<usize> = counts
        .iter()
        .scan(0, |acc, &c| {
            let start = *acc;
            *acc += c;
            Some(start)
        })
        .collect();
    let mut lists: Vec<Vec<Note>> = counts.iter().map(|&c| Vec::with_capacity(c)).collect();
    let mut to = vec![0usize; notes.len()];
    for (i, n) in notes.into_iter().enumerate() {
        let o = usize::from(st.owner[i]);
        to[i] = next[o];
        next[o] += 1;
        lists[o].push(n);
    }
    let mut selected = vec![false; to.len()];
    for (i, &j) in to.iter().enumerate() {
        selected[j] = st.selected[i];
    }
    st.selected = selected;
    st.owner = lists
        .iter()
        .enumerate()
        .flat_map(|(o, l)| std::iter::repeat_n(o as u16, l.len()))
        .collect();
    match &mut st.drag {
        Some(Drag::Move {
            anchor, originals, ..
        })
        | Some(Drag::Resize { anchor, originals }) => {
            *anchor = to.get(*anchor).copied().unwrap_or(usize::MAX);
            for (j, _) in originals.iter_mut() {
                *j = to.get(*j).copied().unwrap_or(usize::MAX);
            }
        }
        Some(Drag::Stretch { originals, .. }) => {
            for (j, _) in originals.iter_mut() {
                *j = to.get(*j).copied().unwrap_or(usize::MAX);
            }
        }
        _ => {}
    }
    lists
}

/// Keys the folded view shows, highest first: the scale's and every key with a note. `None`
/// when that leaves nothing (no scale, no notes).
fn fold_rows<'n>(
    st: &PianoRollState,
    notes: &[Note],
    ghosts: impl Iterator<Item = &'n [Note]>,
) -> Option<Vec<i32>> {
    let mut used = [false; KEYS as usize];
    for n in notes.iter().chain(ghosts.flat_map(|g| g.iter())) {
        used[usize::from(n.key.min(127))] = true;
    }
    let rows: Vec<i32> = (0..KEYS)
        .rev()
        .filter(|&k| used[k as usize] || st.scale.contains(st.scale_root, k as u8))
        .collect();
    (!rows.is_empty()).then_some(rows)
}

/// Starts a new toolbar row when a combo box `width` wide would not fit on this one (a
/// combo box lays out in a group of its own, so a wrapping row cannot wrap it by itself).
pub(super) fn fit(ui: &mut Ui, width: f32) {
    if ui.available_size_before_wrap().x < width + 2.0 * ui.spacing().interact_size.y {
        ui.end_row();
    }
}

fn toolbar(
    ui: &mut Ui,
    theme: &GloomTheme,
    st: &mut PianoRollState,
    view: &mut PianoRollView<'_>,
    notes: &mut [Note],
    actions: &mut Vec<PianoRollAction>,
) -> bool {
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(view.channel_name)
                .color(theme.accent)
                .strong(),
        );
        ui.separator();
        for (tool, label, tip) in [
            (Tool::Draw, "Draw", "Click to add notes (P)"),
            (Tool::Select, "Select", "Drag to select notes (E)"),
        ] {
            if ui
                .add(egui::Button::selectable(st.tool == tool, label))
                .on_hover_text(tip)
                .clicked()
            {
                st.tool = tool;
            }
        }
        ui.separator();
        ui.label(dim(tr("Snap")));
        fit(ui, 56.0);
        egui::ComboBox::from_id_salt("roll_snap")
            .width(56.0)
            .selected_text(st.snap.label())
            .show_ui(ui, |ui| {
                for s in Snap::ALL {
                    ui.selectable_value(&mut st.snap, s, s.label());
                }
            });
        ui.separator();
        ui.label(dim(tr("Scale")));
        fit(ui, 40.0);
        egui::ComboBox::from_id_salt("roll_scale_root")
            .width(40.0)
            .selected_text(key_name(st.scale_root).trim_end_matches('0').to_owned())
            .show_ui(ui, |ui| {
                for r in 0..12u8 {
                    let name = key_name(r).trim_end_matches('0').to_owned();
                    ui.selectable_value(&mut st.scale_root, r, name);
                }
            });
        fit(ui, 120.0);
        egui::ComboBox::from_id_salt("roll_scale")
            .width(120.0)
            .selected_text(st.scale.name())
            .show_ui(ui, |ui| {
                for s in ScaleKind::ALL {
                    ui.selectable_value(&mut st.scale, s, s.name());
                }
            });
        if st.scale != ScaleKind::Off {
            ui.toggle_value(&mut st.scale_snap, tr("Snap"))
                .on_hover_text(tr("Drawn and moved notes land on keys of the scale"));
        }
        ui.toggle_value(&mut st.fold, tr("Fold"))
            .on_hover_text(tr("Show only rows in the scale or with notes"));
        ui.toggle_value(&mut st.stretch, tr("Stretch"))
            .on_hover_text(tr(
                "Dragging a note's end scales the whole selection in time (also Ctrl+drag)",
            ));
        let swatch = |ui: &mut Ui, g: u8, selected: bool| {
            let (r, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
            ui.painter().rect_filled(r, 2.0, group_color(theme, g));
            if selected {
                ui.painter().rect_stroke(
                    r,
                    2.0,
                    Stroke::new(1.5, theme.text),
                    egui::StrokeKind::Outside,
                );
            }
            resp
        };
        let any_sel = st.selected.iter().any(|&s| s);
        ui.menu_button(
            RichText::new("■").color(group_color(theme, st.color)),
            |ui| {
                ui.label(dim(tr("Colour for new notes and the selection")));
                ui.horizontal_wrapped(|ui| {
                    ui.set_max_width(8.0 * 18.0);
                    for g in 0..Note::COLORS {
                        if swatch(ui, g, st.color == g).clicked() {
                            st.color = g;
                            if any_sel {
                                for (n, _) in notes.iter_mut().zip(&st.selected).filter(|(_, &s)| s)
                                {
                                    n.color = g;
                                }
                                changed = true;
                            }
                        }
                    }
                });
                if any_sel && ui.button(tr("Select this colour")).clicked() {
                    for (s, n) in st.selected.iter_mut().zip(notes.iter()) {
                        *s = n.color == st.color;
                    }
                }
            },
        )
        .response
        .on_hover_text(tr("Note colour group"));
        if !view.controls.is_empty() {
            ui.menu_button(tr("Controllers"), |ui| {
                for kind in ControlKind::ALL {
                    let n = view.controls.iter().filter(|c| c.kind == kind).count();
                    if n > 0
                        && ui
                            .button(format!("Clear {} ({n} points)", kind.name()))
                            .clicked()
                    {
                        actions.push(PianoRollAction::ClearControls(Some(kind)));
                    }
                }
                if ui.button(tr("Clear all")).clicked() {
                    actions.push(PianoRollAction::ClearControls(None));
                }
            })
            .response
            .on_hover_text(tr(
                "Recorded pitch bend, sustain, mod wheel and aftertouch (drawn in the velocity \
                 lane)",
            ));
        }
        lanes::lane_selector(ui, theme, st, view, actions);
        tools::tools_menu(ui, st);
        if ui
            .add(egui::Button::selectable(st.ghosts, tr("Ghosts")))
            .on_hover_text(tr("Show other channels' notes in this pattern"))
            .clicked()
        {
            st.ghosts = !st.ghosts;
        }
        if !view.ghosts.is_empty() {
            let n = st.editable.len();
            let label = if n == 0 {
                "Edit channels".to_owned()
            } else {
                format!("Edit channels ({})", n + 1)
            };
            ui.menu_button(label, |ui| {
                ui.label(dim(tr("Edit these channels' notes along with this one")));
                for g in &view.ghosts {
                    let mut on = st.editable.contains(&g.id);
                    if ui.checkbox(&mut on, g.name).changed() {
                        if on {
                            st.editable.push(g.id);
                        } else {
                            st.editable.retain(|&id| id != g.id);
                        }
                        st.drag = None;
                    }
                }
            })
            .response
            .on_hover_text(tr("Ghost channels whose notes can be edited here too"));
        }
        ui.separator();
        if ui
            .button(tr("Quantize"))
            .on_hover_text(tr(
                "Snap note starts of the selection (or all notes) to the grid (Q)",
            ))
            .clicked()
        {
            actions.push(PianoRollAction::Quantize);
        }
        let mut q = st.quantize_strength * 100.0;
        if ui
            .add(
                egui::DragValue::new(&mut q)
                    .range(0.0..=100.0)
                    .suffix(" %")
                    .speed(1.0),
            )
            .on_hover_text(tr("Quantize strength"))
            .changed()
        {
            st.quantize_strength = q / 100.0;
        }
        if ui
            .button(tr("Humanize"))
            .on_hover_text(tr(
                "Randomly shift timing and velocity of the selection (or all notes)",
            ))
            .clicked()
        {
            actions.push(PianoRollAction::Humanize);
        }
        ui.add(
            egui::DragValue::new(&mut st.humanize_ticks)
                .range(0..=240)
                .suffix(tr(" ticks"))
                .speed(0.5),
        )
        .on_hover_text(tr("Humanize timing range (±, 960 ticks per beat)"));
        let mut hv = st.humanize_velocity * 100.0;
        if ui
            .add(
                egui::DragValue::new(&mut hv)
                    .range(0.0..=100.0)
                    .suffix(tr(" % vel"))
                    .speed(0.5),
            )
            .on_hover_text(tr("Humanize velocity range (±)"))
            .changed()
        {
            st.humanize_velocity = hv / 100.0;
        }
        let n_sel = st.selected.iter().filter(|&&s| s).count();
        ui.label(dim(&format!("{} notes, {} selected", notes.len(), n_sel)));
    });
    changed
}

/// The selection's per-note values: shows the first selected note's, and an edit applies to
/// every selected note.
fn note_panel(
    ui: &mut Ui,
    theme: &GloomTheme,
    st: &mut PianoRollState,
    notes: &mut [Note],
) -> bool {
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    // The row keeps its height with nothing selected, so the grid does not jump mid-gesture.
    let first = st.selected.iter().position(|&s| s);
    let Some(mut n) = first.and_then(|i| notes.get(i).copied()) else {
        ui.horizontal(|ui| {
            ui.set_min_height(ui.spacing().interact_size.y);
            ui.label(dim(tr(
                "Select notes to edit their pan, release, fine pitch, mod X/Y, slide and porta",
            )));
        });
        return false;
    };
    let before = n;
    ui.horizontal_wrapped(|ui| {
        ui.set_min_height(ui.spacing().interact_size.y);
        ui.label(dim(tr("Note")));
        let pct = |ui: &mut Ui, v: &mut f32, label: &str, tip: &str| {
            let mut x = *v * 100.0;
            if ui
                .add(
                    egui::DragValue::new(&mut x)
                        .range(0.0..=100.0)
                        .prefix(format!("{label} "))
                        .suffix(" %")
                        .speed(0.5),
                )
                .on_hover_text(tip)
                .changed()
            {
                *v = x / 100.0;
            }
        };
        pct(ui, &mut n.velocity, "Vel", "Velocity");
        pct(
            ui,
            &mut n.release,
            "Rel",
            "Release velocity: higher releases faster (50 % neutral)",
        );
        let mut pan = n.pan * 100.0;
        if ui
            .add(
                egui::DragValue::new(&mut pan)
                    .range(-100.0..=100.0)
                    .prefix(tr("Pan "))
                    .speed(0.5),
            )
            .on_hover_text(tr("Pan, -100 (left) to 100 (right)"))
            .changed()
        {
            n.pan = pan / 100.0;
        }
        ui.add(
            egui::DragValue::new(&mut n.fine)
                .range(-100.0..=100.0)
                .prefix(tr("Fine "))
                .suffix(tr(" ct"))
                .speed(0.5),
        )
        .on_hover_text(tr("Fine pitch in cents"));
        pct(
            ui,
            &mut n.mod_x,
            "X",
            "Mod X: Gloom Synth cutoff, sampler tone (50 % neutral)",
        );
        pct(
            ui,
            &mut n.mod_y,
            "Y",
            "Mod Y: Gloom Synth resonance, sampler start (50 % neutral)",
        );
        ui.toggle_value(&mut n.slide, tr("Slide")).on_hover_text(tr(
            "Glide the sounding note to this key over this note's length",
        ));
        ui.toggle_value(&mut n.porta, tr("Porta")).on_hover_text(tr(
            "Start at the previous note's pitch and glide to this one",
        ));
        ui.toggle_value(&mut n.muted, tr("Mute (M)"));
    });
    if n == before {
        return false;
    }
    // Apply only the fields that changed, so a mixed selection keeps the rest.
    for (m, _) in notes.iter_mut().zip(&st.selected).filter(|(_, &s)| s) {
        macro_rules! copy {
            ($($f:ident),*) => {$(if n.$f != before.$f { m.$f = n.$f; })*};
        }
        copy!(velocity, release, pan, fine, mod_x, mod_y, slide, porta, muted);
        m.sanitize();
    }
    true
}

fn handle_wheel(ui: &Ui, st: &mut PianoRollState, grid: Rect) {
    let (scroll, zoom, alt, pointer) = ui.input(|i| {
        (
            i.smooth_scroll_delta,
            i.zoom_delta(),
            i.modifiers.alt,
            i.pointer.hover_pos(),
        )
    });
    let Some(p) = pointer else {
        return;
    };
    if zoom != 1.0 {
        // Zoom time around the pointer.
        let px_per_tick = st.px_per_beat / PPQ as f32;
        let at = st.scroll_tick + f64::from((p.x - grid.left()) / px_per_tick);
        st.px_per_beat = (st.px_per_beat * zoom).clamp(MIN_PX_PER_BEAT, MAX_PX_PER_BEAT);
        let px_per_tick = st.px_per_beat / PPQ as f32;
        st.scroll_tick = at - f64::from((p.x - grid.left()) / px_per_tick);
    } else if alt && scroll.y != 0.0 {
        // Zoom keys around the pointer.
        let row = (p.y - grid.top()) / st.key_height + st.scroll_rows;
        st.key_height = (st.key_height * (1.0 + scroll.y * 0.004)).clamp(MIN_KEY_H, MAX_KEY_H);
        st.scroll_rows = row - (p.y - grid.top()) / st.key_height;
    } else {
        st.scroll_rows -= scroll.y / st.key_height;
        st.scroll_tick -= f64::from(scroll.x / (st.px_per_beat / PPQ as f32));
    }
}

/// Centres the rows on the notes, and scrolls back in time when they start before the left
/// edge or after the right one (a newly shown channel opens on its notes).
fn frame_notes(st: &mut PianoRollState, notes: &[Note], rows: Option<&[i32]>, grid: Rect) {
    let (Some(lo), Some(hi)) = (
        notes.iter().map(|n| i32::from(n.key)).min(),
        notes.iter().map(|n| i32::from(n.key)).max(),
    ) else {
        return;
    };
    let row = |key: i32| match rows {
        None => (KEYS - 1 - key) as f32,
        Some(r) => r.partition_point(|&k| k > key) as f32,
    };
    let mid = (row(lo) + row(hi)) * 0.5;
    st.scroll_rows = mid - grid.height() / st.key_height * 0.5;
    let first = notes.iter().map(|n| n.start).min().unwrap_or(0);
    let px_per_tick = f64::from(st.px_per_beat) / PPQ as f64;
    let last_visible = st.scroll_tick + f64::from(grid.width()) / px_per_tick;
    if (first as f64) < st.scroll_tick || first as f64 > last_visible {
        st.scroll_tick = (first - first.rem_euclid(PPQ * 4)) as f64;
    }
}

fn clamp_scroll(st: &mut PianoRollState, grid: Rect, rows: i32) {
    let visible_rows = grid.height() / st.key_height;
    st.scroll_rows = st
        .scroll_rows
        .clamp(0.0, (rows as f32 - visible_rows).max(0.0));
    st.scroll_tick = st.scroll_tick.max(0.0);
}

fn selected_indices(st: &PianoRollState) -> Vec<usize> {
    (0..st.selected.len()).filter(|&i| st.selected[i]).collect()
}

fn hit_note(geo: &Geo, notes: &[Note], p: Pos2, sorted: bool) -> Option<(usize, bool)> {
    let t = geo.tick(p.x);
    let candidates = visible_range(notes, t - 1, t + 1, sorted);
    // Last drawn is on top.
    candidates.into_iter().rev().find_map(|i| {
        let r = geo.note_rect(&notes[i]);
        r.expand2(vec2(2.0, 0.0)).contains(p).then(|| {
            // The grip is at most 30 % of the note, so short notes stay easy to grab and move.
            let grip = p.x >= r.right() - RESIZE_GRIP_PX.min(r.width() * 0.3);
            (i, grip)
        })
    })
}

fn start_audition(
    st: &mut PianoRollState,
    key: u8,
    velocity: f32,
    actions: &mut Vec<PianoRollAction>,
) {
    if let Some(k) = st.audition_key.take() {
        actions.push(PianoRollAction::AuditionOff { key: k });
    }
    st.audition_key = Some(key);
    actions.push(PianoRollAction::AuditionOn { key, velocity });
}

fn stop_audition(st: &mut PianoRollState, actions: &mut Vec<PianoRollAction>) {
    if let Some(k) = st.audition_key.take() {
        actions.push(PianoRollAction::AuditionOff { key: k });
    }
}

/// Sorts the working notes by start and key, keeping selection and owners with them.
fn sort_notes(notes: &mut Vec<Note>, st: &mut PianoRollState) {
    st.selected.resize(notes.len(), false);
    st.owner.resize(notes.len(), 0);
    let mut all: Vec<(Note, bool, u16)> = notes
        .drain(..)
        .zip(st.selected.drain(..))
        .zip(st.owner.drain(..))
        .map(|((n, s), o)| (n, s, o))
        .collect();
    all.sort_by_key(|(n, _, o)| (n.start, n.key, *o));
    for (n, s, o) in all {
        notes.push(n);
        st.selected.push(s);
        st.owner.push(o);
    }
}

fn delete_where(
    notes: &mut Vec<Note>,
    st: &mut PianoRollState,
    kill: impl Fn(usize) -> bool,
) -> bool {
    let before = notes.len();
    st.selected.resize(before, false);
    st.owner.resize(before, 0);
    let flags: Vec<bool> = (0..before).map(|i| !kill(i)).collect();
    let mut it = flags.iter();
    notes.retain(|_| *it.next().unwrap_or(&true));
    let mut it = flags.iter();
    st.selected.retain(|_| *it.next().unwrap_or(&true));
    let mut it = flags.iter();
    st.owner.retain(|_| *it.next().unwrap_or(&true));
    notes.len() != before
}

/// Adds a note to the working list for `owner`.
fn push_note(notes: &mut Vec<Note>, st: &mut PianoRollState, n: Note, owner: u16, selected: bool) {
    st.selected.resize(notes.len(), false);
    st.owner.resize(notes.len(), 0);
    notes.push(n);
    st.selected.push(selected);
    st.owner.push(owner);
}

fn grid_input(
    ui: &Ui,
    st: &mut PianoRollState,
    geo: &Geo,
    resp: &Response,
    notes: &mut Vec<Note>,
    single: bool,
    actions: &mut Vec<PianoRollAction>,
) -> bool {
    let (pos, primary_pressed, secondary_pressed, primary_down, secondary_down, released, mods) =
        ui.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.primary_pressed(),
                i.pointer.secondary_pressed(),
                i.pointer.primary_down(),
                i.pointer.secondary_down(),
                i.pointer.any_released(),
                i.modifiers,
            )
        });
    let Some(p) = pos else {
        return false;
    };
    let mut changed = false;
    let sorted = st.drag.is_none() && single;
    let scale = if st.scale_snap {
        st.scale
    } else {
        ScaleKind::Off
    };

    // Cursor feedback.
    if resp.hovered() && st.drag.is_none() {
        match hit_note(geo, notes, p, sorted) {
            Some((_, true)) => ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal),
            Some(_) => ui.ctx().set_cursor_icon(egui::CursorIcon::Grab),
            None => {}
        }
    }

    if resp.hovered() && primary_pressed && st.drag.is_none() {
        match hit_note(geo, notes, p, sorted) {
            Some((i, grip)) => {
                if !st.selected[i] {
                    if !mods.shift {
                        st.selected.iter_mut().for_each(|s| *s = false);
                    }
                    st.selected[i] = true;
                }
                let originals: Vec<_> = selected_indices(st)
                    .into_iter()
                    .map(|j| (j, notes[j]))
                    .collect();
                st.last_length = notes[i].length;
                st.last_velocity = notes[i].velocity;
                if grip && (mods.command || st.stretch) {
                    let from = originals.iter().map(|(_, n)| n.start).min().unwrap_or(0);
                    let to = originals
                        .iter()
                        .map(|(_, n)| n.start + n.length)
                        .max()
                        .unwrap_or(from + 1);
                    st.drag = Some(Drag::Stretch {
                        originals,
                        from,
                        to,
                    });
                } else if grip {
                    st.drag = Some(Drag::Resize {
                        anchor: i,
                        originals,
                    });
                } else {
                    start_audition(st, notes[i].key, notes[i].velocity, actions);
                    st.drag = Some(Drag::Move {
                        anchor: i,
                        grab_tick: geo.tick(p.x),
                        grab_key: geo.key(p.y),
                        originals,
                        last_key: notes[i].key,
                    });
                }
            }
            None if st.tool == Tool::Draw && !mods.command => {
                let key = scale.snap(st.scale_root, geo.key(p.y), 0) as u8;
                let mut note = Note::new(
                    st.snap.floor(geo.tick(p.x)).max(0),
                    st.last_length.max(1),
                    key,
                    st.last_velocity,
                );
                note.color = st.color;
                st.selected.iter_mut().for_each(|s| *s = false);
                let stamp = match st.chord {
                    Some(c) => {
                        gt_core::note_tools::chord_stamp(&note, c, usize::from(st.chord_inversion))
                    }
                    None => vec![note],
                };
                let first = notes.len();
                for n in stamp {
                    push_note(notes, st, n, 0, true);
                }
                let originals: Vec<_> = (first..notes.len()).map(|j| (j, notes[j])).collect();
                let anchor = originals
                    .iter()
                    .find(|(_, n)| n.key == key)
                    .map_or(first, |(j, _)| *j);
                changed = true;
                start_audition(st, key, note.velocity, actions);
                st.drag = Some(Drag::Move {
                    anchor,
                    grab_tick: geo.tick(p.x),
                    grab_key: geo.key(p.y),
                    originals,
                    last_key: key,
                });
            }
            None => {
                if !mods.shift {
                    st.selected.iter_mut().for_each(|s| *s = false);
                }
                st.drag = Some(Drag::Box {
                    from: p,
                    additive: mods.shift,
                });
            }
        }
    } else if resp.hovered() && secondary_pressed && st.drag.is_none() {
        st.drag = Some(Drag::Erase);
    }

    // A drag holds note indices; drop it if the list shrank under it (defensive: the app
    // cancels drags before replacing notes).
    let stale = match &st.drag {
        Some(
            Drag::Move { originals, .. }
            | Drag::Resize { originals, .. }
            | Drag::Stretch { originals, .. },
        ) => originals.iter().any(|(j, _)| *j >= notes.len()),
        _ => false,
    };
    if stale {
        st.drag = None;
    }

    match &mut st.drag {
        Some(Drag::Move {
            anchor,
            grab_tick,
            grab_key,
            originals,
            last_key,
        }) if primary_down => {
            let anchor_orig = originals.iter().find(|(j, _)| j == anchor).map(|(_, n)| *n);
            if let Some(a) = anchor_orig {
                let raw = geo.tick(p.x) - *grab_tick;
                let mut dt = if st.snap == Snap::Off {
                    raw
                } else {
                    st.snap.round(a.start + raw) - a.start
                };
                let min_start = originals.iter().map(|(_, n)| n.start).min().unwrap_or(0);
                dt = dt.max(-min_start);
                let mut dk = geo.key(p.y) - *grab_key;
                let (lo, hi) = originals.iter().fold((127, 0), |(lo, hi), (_, n)| {
                    (lo.min(i32::from(n.key)), hi.max(i32::from(n.key)))
                });
                dk = dk.clamp(-lo, 127 - hi);
                for (j, n) in originals.iter() {
                    let key = scale.snap(st.scale_root, i32::from(n.key) + dk, dk.signum());
                    let moved = Note {
                        start: n.start + dt,
                        key: key.clamp(0, KEYS - 1) as u8,
                        ..*n
                    };
                    if notes[*j] != moved {
                        notes[*j] = moved;
                        changed = true;
                    }
                }
                let new_key = scale
                    .snap(st.scale_root, i32::from(a.key) + dk, dk.signum())
                    .clamp(0, KEYS - 1) as u8;
                if new_key != *last_key {
                    *last_key = new_key;
                    let v = a.velocity;
                    start_audition(st, new_key, v, actions);
                }
            }
        }
        Some(Drag::Resize { anchor, originals }) if primary_down => {
            if let Some((_, a)) = originals.iter().find(|(j, _)| j == anchor).copied() {
                let g = st.snap.ticks();
                let end = st.snap.round(geo.tick(p.x)).max(a.start + g.min(a.length));
                let dl = end - (a.start + a.length);
                for (j, n) in originals.iter() {
                    let len = (n.length + dl).max(g.min(n.length)).max(1);
                    if notes[*j].length != len {
                        notes[*j].length = len;
                        changed = true;
                    }
                }
                st.last_length = (a.length + dl).max(1);
            }
        }
        Some(Drag::Stretch {
            originals,
            from,
            to,
        }) if primary_down => {
            let g = st.snap.ticks();
            let end = st.snap.round(geo.tick(p.x)).max(*from + g);
            let block: Vec<Note> = originals.iter().map(|(_, n)| *n).collect();
            let scaled = stretch_notes(&block, *from, *to, end);
            for ((j, _), n) in originals.iter().zip(scaled) {
                if notes[*j] != n {
                    notes[*j] = n;
                    changed = true;
                }
            }
        }
        Some(Drag::Erase) if secondary_down => {
            if let Some((i, _)) = hit_note(geo, notes, p, sorted) {
                changed |= delete_where(notes, st, |j| j == i);
            }
        }
        _ => {}
    }

    if released && !primary_down && !secondary_down {
        if let Some(Drag::Box { from, additive }) = st.drag.clone() {
            let r = Rect::from_two_pos(from, p);
            for (i, n) in notes.iter().enumerate() {
                if geo.note_rect(n).intersects(r) {
                    st.selected[i] = true;
                } else if !additive {
                    st.selected[i] = false;
                }
            }
        }
        if matches!(
            st.drag,
            Some(Drag::Move { .. } | Drag::Resize { .. } | Drag::Stretch { .. } | Drag::Erase)
        ) {
            sort_notes(notes, st);
        }
        if !matches!(
            st.drag,
            Some(
                Drag::Velocity
                    | Drag::Automation { .. }
                    | Drag::Control { .. }
                    | Drag::LaneLine { .. }
            )
        ) {
            st.drag = None;
        }
        stop_audition(st, actions);
    }
    changed
}

fn keyboard_input(
    ui: &Ui,
    st: &mut PianoRollState,
    geo: &Geo,
    resp: &Response,
    actions: &mut Vec<PianoRollAction>,
) {
    let (pos, down) = ui.input(|i| (i.pointer.interact_pos(), i.pointer.primary_down()));
    if resp.is_pointer_button_down_on() && down {
        if let Some(p) = pos {
            let key = geo.key(p.y) as u8;
            if st.audition_key != Some(key) {
                start_audition(st, key, st.last_velocity, actions);
            }
        }
    } else if st.drag.is_none() && st.audition_key.is_some() && !down {
        stop_audition(st, actions);
    }
}

fn key_commands(
    ui: &Ui,
    st: &mut PianoRollState,
    geo: &Geo,
    playhead: Option<i64>,
    notes: &mut Vec<Note>,
    owner_ids: &[u32],
    actions: &mut Vec<PianoRollAction>,
) -> bool {
    use egui::Event;
    let mut changed = false;
    let events = ui.input(|i| i.events.clone());
    let any_sel = st.selected.iter().any(|&s| s);
    for e in events {
        match e {
            Event::Copy | Event::Cut if any_sel => {
                st.clipboard = selected_block(st, notes)
                    .into_iter()
                    .map(|(n, o)| (n, owner_ids.get(usize::from(o)).copied().unwrap_or(0)))
                    .collect();
                // The notes stay in our own clipboard. Some text must be on the system
                // clipboard, or the desktop never sends a paste event.
                ui.ctx()
                    .copy_text(format!("GloomTunes Studio: {} notes", st.clipboard.len()));
                if matches!(e, Event::Cut) {
                    let sel = st.selected.clone();
                    changed |= delete_where(notes, st, |i| sel[i]);
                }
            }
            Event::Paste(_) if !st.clipboard.is_empty() => {
                // Each note goes back to its channel if that is still edited here.
                let block: Vec<(Note, u16)> = st
                    .clipboard
                    .iter()
                    .map(|&(n, id)| {
                        let o = owner_ids.iter().position(|&x| x == id).unwrap_or(0);
                        (n, o as u16)
                    })
                    .collect();
                let first = block.iter().map(|(n, _)| n.start).min().unwrap_or(0);
                let pointer = ui
                    .input(|i| i.pointer.hover_pos())
                    .filter(|p| geo.grid.contains(*p));
                let at = match (pointer, playhead) {
                    (Some(p), _) => st.snap.floor(geo.tick(p.x)).max(0),
                    (None, Some(t)) => st.snap.floor(t).max(0),
                    (None, None) => first,
                };
                paste(notes, st, &block, at - first);
                changed = true;
            }
            _ => {}
        }
    }
    let keys = crate::keymap::take_view(ui, Scope::PianoRoll);
    let pressed = |c: Command| keys.contains(&c);
    if pressed(Command::RollDelete) {
        let sel = st.selected.clone();
        changed |= delete_where(notes, st, |i| sel[i]);
    }
    if pressed(Command::RollSelectAll) {
        st.selected.iter_mut().for_each(|s| *s = true);
    }
    if pressed(Command::RollDuplicate) && any_sel {
        let block = selected_block(st, notes);
        let start = block.iter().map(|(n, _)| n.start).min().unwrap_or(0);
        let end = block
            .iter()
            .map(|(n, _)| n.start + n.length)
            .max()
            .unwrap_or(0);
        let g = st.snap.ticks().max(1);
        let span = ((end - start + g - 1) / g).max(1) * g;
        paste(notes, st, &block, span);
        changed = true;
    }
    if pressed(Command::RollQuantize) {
        actions.push(PianoRollAction::Quantize);
    }
    if pressed(Command::RollDrawTool) {
        st.tool = Tool::Draw;
    }
    if pressed(Command::RollSelectTool) {
        st.tool = Tool::Select;
    }
    if pressed(Command::RollMute) && any_sel {
        // Mutes the selection, or unmutes it when all of it is muted.
        let sel = selected_indices(st);
        let mute = !sel.iter().all(|&i| notes[i].muted);
        for i in sel {
            notes[i].muted = mute;
        }
        changed = true;
    }
    if any_sel {
        let step = st.snap.ticks();
        let scale = if st.scale_snap {
            st.scale
        } else {
            ScaleKind::Off
        };
        let edits: [(Command, Edit); 16] = [
            (Command::RollFineShorter, Edit::Length(-FINE_TICKS)),
            (Command::RollFineLonger, Edit::Length(FINE_TICKS)),
            (Command::RollFineVelocityUp, Edit::Velocity(0.01)),
            (Command::RollFineVelocityDown, Edit::Velocity(-0.01)),
            (Command::RollShorter, Edit::Length(-step)),
            (Command::RollLonger, Edit::Length(step)),
            (Command::RollOctaveUp, Edit::Transpose(12)),
            (Command::RollOctaveDown, Edit::Transpose(-12)),
            (Command::RollNudgeLeft, Edit::Move(-FINE_TICKS)),
            (Command::RollNudgeRight, Edit::Move(FINE_TICKS)),
            (Command::RollVelocityUp, Edit::Velocity(0.1)),
            (Command::RollVelocityDown, Edit::Velocity(-0.1)),
            (Command::RollUp, Edit::Degree(1)),
            (Command::RollDown, Edit::Degree(-1)),
            (Command::RollLeft, Edit::Move(-step)),
            (Command::RollRight, Edit::Move(step)),
        ];
        let mut moved = false;
        for (c, edit) in edits {
            if pressed(c) {
                moved |= edit_selection(notes, &st.selected, edit, scale, st.scale_root);
            }
        }
        if moved {
            sort_notes(notes, st);
            changed = true;
        }
    }
    changed
}

/// A keyboard edit of the selected notes.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Edit {
    /// Move by ticks.
    Move(i64),
    /// Change length by ticks (at least one tick remains).
    Length(i64),
    /// Transpose by semitones.
    Transpose(i32),
    /// Up or down one key of the scale (a semitone without one).
    Degree(i32),
    /// Change velocity.
    Velocity(f32),
}

/// Applies `edit` to the selected notes. Moves and transpositions happen only if every note
/// stays in range. Returns true if anything changed.
fn edit_selection(
    notes: &mut [Note],
    selected: &[bool],
    edit: Edit,
    scale: ScaleKind,
    root: u8,
) -> bool {
    let sel = || {
        notes
            .iter()
            .zip(selected)
            .filter(|(_, &s)| s)
            .map(|(n, _)| n)
    };
    let new_key = |n: &Note| -> Option<i32> {
        match edit {
            Edit::Transpose(dk) => Some(i32::from(n.key) + dk),
            Edit::Degree(d) => scale.step(root, i32::from(n.key), d),
            _ => Some(i32::from(n.key)),
        }
    };
    let ok = sel().all(|n| {
        let start_ok = match edit {
            Edit::Move(dt) => n.start + dt >= 0,
            _ => true,
        };
        start_ok && new_key(n).is_some_and(|k| (0..KEYS).contains(&k))
    });
    if !ok {
        return false;
    }
    let mut changed = false;
    for (n, _) in notes.iter_mut().zip(selected).filter(|(_, &s)| s) {
        let before = *n;
        match edit {
            Edit::Move(dt) => n.start += dt,
            Edit::Length(dl) => n.length = (n.length + dl).max(1),
            Edit::Transpose(_) | Edit::Degree(_) => {
                n.key = new_key(&before).unwrap_or(i32::from(n.key)) as u8;
            }
            Edit::Velocity(dv) => n.velocity = (n.velocity + dv).clamp(0.0, 1.0),
        }
        changed |= *n != before;
    }
    changed
}

/// Adds `block` (notes and their owners) shifted by `offset` ticks and selects exactly the
/// added notes.
fn paste(notes: &mut Vec<Note>, st: &mut PianoRollState, block: &[(Note, u16)], offset: i64) {
    st.selected.iter_mut().for_each(|s| *s = false);
    for &(n, o) in block {
        let n = Note {
            start: (n.start + offset).max(0),
            ..n
        };
        push_note(notes, st, n, o, true);
    }
    sort_notes(notes, st);
}

/// The selected notes and their owners.
fn selected_block(st: &PianoRollState, notes: &[Note]) -> Vec<(Note, u16)> {
    selected_indices(st)
        .into_iter()
        .map(|i| (notes[i], st.owner.get(i).copied().unwrap_or(0)))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn paint(
    ui: &Ui,
    theme: &GloomTheme,
    st: &PianoRollState,
    geo: &Geo,
    area: Rect,
    notes: &[Note],
    ghosts: &[&[Note]],
    pattern_len: i64,
    bar_ticks: i64,
    playhead: Option<i64>,
    sorted: bool,
) {
    let p = ui.painter_at(area);
    let grid = geo.grid;
    p.rect_filled(area, 0.0, theme.bg_deep);

    // Rows.
    let keys = geo.visible_keys();
    let row_white = theme.bg_panel;
    let row_black = theme.bg_deep;
    for &key in &keys {
        let y = geo.y(key);
        let r = Rect::from_min_max(pos2(grid.left(), y), pos2(grid.right(), y + geo.key_h))
            .intersect(grid);
        let mut c = if is_black(key) { row_black } else { row_white };
        if st.scale != ScaleKind::Off {
            if st.scale.contains(st.scale_root, key as u8) {
                c = theme.bg_widget;
                if (key - i32::from(st.scale_root)).rem_euclid(12) == 0 {
                    c = theme.bg_widget.lerp_to_gamma(theme.accent_dim, 0.6);
                }
            } else {
                c = row_black;
            }
        }
        p.rect_filled(r, 0.0, c);
        if key.rem_euclid(12) == 0 {
            p.line_segment(
                [
                    pos2(grid.left(), y + geo.key_h),
                    pos2(grid.right(), y + geo.key_h),
                ],
                Stroke::new(1.0, theme.stroke),
            );
        }
    }

    // Vertical lines: snap (only if at least 6 px apart), beats, bars.
    let (t0, t1) = geo.tick_range();
    let snap = st.snap.ticks();
    let line = |t: i64, c: Color32, w: f32| {
        let x = geo.x(t);
        p.line_segment(
            [pos2(x, grid.top()), pos2(x, grid.bottom())],
            Stroke::new(w, c),
        );
    };
    if snap > 1 && snap as f32 * geo.px_per_tick >= 6.0 {
        let mut t = t0.div_euclid(snap) * snap;
        while t <= t1 {
            line(t, theme.bg_widget, 1.0);
            t += snap;
        }
    }
    let beat_px = PPQ as f32 * geo.px_per_tick;
    let beat_step = if beat_px >= 6.0 { PPQ } else { bar_ticks };
    let mut t = t0.div_euclid(beat_step).max(0) * beat_step;
    while t <= t1 {
        let (c, w) = if t % bar_ticks == 0 {
            (theme.stroke.lerp_to_gamma(theme.text_dim, 0.4), 1.0)
        } else {
            (theme.stroke, 1.0)
        };
        line(t, c, w);
        t += beat_step;
    }
    // Past the pattern end.
    let end_x = geo.x(pattern_len);
    if end_x < grid.right() {
        let r = Rect::from_min_max(
            pos2(end_x.max(grid.left()), grid.top()),
            grid.right_bottom(),
        );
        p.rect_filled(r, 0.0, Color32::from_black_alpha(110));
        p.line_segment(
            [pos2(end_x, grid.top()), pos2(end_x, grid.bottom())],
            Stroke::new(1.5, theme.accent_dim),
        );
    }

    // Ghost notes.
    if st.ghosts {
        let ghost = theme.text_dim.gamma_multiply(0.35);
        for g in ghosts {
            for i in visible_range(g, t0, t1, true) {
                let r = geo.note_rect(&g[i]);
                if r.intersects(grid) {
                    p.rect_stroke(r, 1.0, Stroke::new(1.0, ghost), egui::StrokeKind::Inside);
                }
            }
        }
    }

    // Notes.
    let visible = visible_range(notes, t0, t1, sorted);
    let label_fits = geo.key_h >= 11.0;
    for &i in &visible {
        let n = &notes[i];
        let r = geo.note_rect(n);
        if !r.intersects(grid) {
            continue;
        }
        let sel = st.selected.get(i).copied().unwrap_or(false);
        let other = st.owner.get(i).is_some_and(|&o| o > 0);
        let c = group_color(theme, n.color);
        let dim = if n.color == 0 {
            theme.accent_dim
        } else {
            c.gamma_multiply(0.45)
        };
        let base = dim.lerp_to_gamma(c, 0.35 + 0.65 * n.velocity);
        let mut fill = if sel {
            c.lerp_to_gamma(Color32::WHITE, 0.35)
        } else {
            base
        };
        if n.muted {
            fill = theme.bg_widget;
        } else if other {
            fill = fill.gamma_multiply(0.55);
        }
        p.rect_filled(r, 2.0, fill);
        if n.muted || other {
            p.rect_stroke(r, 2.0, Stroke::new(1.0, c), egui::StrokeKind::Inside);
        }
        if n.slide || n.porta {
            // A wedge at the start: up-slanted for a slide, a small dot for portamento.
            let h = r.height().min(10.0);
            let at = r.left_bottom() + vec2(1.0, -1.0);
            if n.slide {
                p.add(egui::Shape::convex_polygon(
                    vec![at, at + vec2(h, 0.0), at + vec2(0.0, -h)],
                    theme.text,
                    Stroke::NONE,
                ));
            } else {
                p.circle_filled(r.left_center() + vec2(3.0, 0.0), 2.0, theme.text);
            }
        }
        if sel {
            p.rect_stroke(
                r,
                2.0,
                Stroke::new(1.0, theme.text),
                egui::StrokeKind::Inside,
            );
        }
        if label_fits && r.width() > 28.0 {
            p.text(
                r.left_center() + vec2(if n.slide || n.porta { 9.0 } else { 3.0 }, 0.0),
                egui::Align2::LEFT_CENTER,
                key_name(n.key),
                egui::FontId::proportional((geo.key_h - 3.0).min(11.0)),
                theme.bg_deep,
            );
        }
    }

    // Box selection.
    if let (Some(Drag::Box { from, .. }), Some(now)) =
        (&st.drag, ui.input(|i| i.pointer.interact_pos()))
    {
        let r = Rect::from_two_pos(*from, now).intersect(grid);
        p.rect_filled(r, 0.0, theme.accent.gamma_multiply(0.12));
        p.rect_stroke(
            r,
            0.0,
            Stroke::new(1.0, theme.accent),
            egui::StrokeKind::Inside,
        );
    }

    // Playhead.
    if let Some(t) = playhead {
        let x = geo.x(t);
        if x >= grid.left() && x <= grid.right() {
            p.line_segment(
                [pos2(x, area.top()), pos2(x, grid.bottom())],
                Stroke::new(1.5, theme.text),
            );
        }
    }

    // Ruler.
    let ruler = Rect::from_min_max(
        pos2(grid.left(), area.top()),
        pos2(grid.right(), grid.top()),
    );
    p.rect_filled(ruler, 0.0, theme.bg_panel);
    let bar_px = bar_ticks as f32 * geo.px_per_tick;
    let every = ((40.0 / bar_px).ceil() as i64).max(1);
    let mut b = t0.div_euclid(bar_ticks).max(0);
    while b * bar_ticks <= t1 {
        if b % every == 0 {
            let x = geo.x(b * bar_ticks);
            if x >= grid.left() - 1.0 {
                p.text(
                    pos2(x + 3.0, ruler.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("{}", b + 1),
                    egui::FontId::proportional(10.5),
                    theme.text_dim,
                );
                p.line_segment(
                    [pos2(x, ruler.top() + 4.0), pos2(x, ruler.bottom())],
                    Stroke::new(1.0, theme.stroke),
                );
            }
        }
        b += 1;
    }

    // Keyboard.
    let kb = Rect::from_min_max(
        pos2(area.left(), grid.top()),
        pos2(grid.left(), grid.bottom()),
    );
    let kp = ui.painter_at(kb);
    for &key in &keys {
        let y = geo.y(key);
        let r = Rect::from_min_max(pos2(kb.left(), y), pos2(kb.right() - 1.0, y + geo.key_h));
        let held = st.audition_key == Some(key as u8);
        let c = if held {
            theme.accent
        } else if is_black(key) {
            Color32::from_rgb(0x22, 0x22, 0x28)
        } else {
            Color32::from_rgb(0xb8, 0xb6, 0xbe)
        };
        kp.rect_filled(r.shrink2(vec2(0.0, 0.5)), 1.0, c);
        if (key.rem_euclid(12) == 0 || geo.rows.is_some()) && geo.key_h >= 8.0 {
            kp.text(
                r.right_center() - vec2(3.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                key_name(key as u8),
                egui::FontId::proportional((geo.key_h - 2.0).min(10.5)),
                Color32::from_rgb(0x30, 0x30, 0x36),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(start: i64, length: i64, key: u8) -> Note {
        Note::new(start, length, key, 0.8)
    }

    #[test]
    fn snap_sizes() {
        assert_eq!(Snap::Straight(4).ticks(), 960);
        assert_eq!(Snap::Straight(32).ticks(), 120);
        assert_eq!(Snap::Triplet(8).ticks(), 320);
        assert_eq!(Snap::Triplet(16).ticks(), 160);
        assert_eq!(Snap::Triplet(32).ticks(), 80);
        assert_eq!(Snap::Off.ticks(), 1);
        assert_eq!(Snap::Straight(16).floor(479), 240);
        assert_eq!(Snap::Straight(16).round(361), 480);
        assert_eq!(Snap::Straight(16).floor(-1), -240);
    }

    #[test]
    fn scales_and_names() {
        assert!(ScaleKind::Major.contains(0, 64)); // E in C major
        assert!(!ScaleKind::Major.contains(0, 61));
        assert!(ScaleKind::Minor.contains(9, 60)); // C in A minor
        assert!(!ScaleKind::Off.contains(0, 60));
        assert_eq!(key_name(60), "C5");
        assert_eq!(key_name(69), "A5");
    }

    #[test]
    fn culling_finds_long_notes_that_start_before_the_view() {
        let mut notes: Vec<Note> = (0..10_000).map(|i| n(i * 120, 60, 60)).collect();
        notes.push(n(0, 2_000_000, 40)); // a very long note starting at 0
        notes.sort_by_key(|x| (x.start, x.key));
        let vis = visible_range(&notes, 500_000, 501_000, true);
        assert!(vis.iter().any(|&i| notes[i].key == 40));
        // Only notes overlapping the window, plus nothing far away.
        assert!(vis.len() < 20, "{}", vis.len());
        assert_eq!(
            visible_range(&notes, 500_000, 501_000, false).len(),
            vis.len()
        );
    }

    #[test]
    fn keyboard_edits_refuse_to_leave_the_range() {
        let off = ScaleKind::Off;
        let mut notes = vec![n(0, 10, 127), n(240, 10, 60)];
        assert!(!edit_selection(
            &mut notes,
            &[true, false],
            Edit::Transpose(1),
            off,
            0
        ));
        assert!(!edit_selection(
            &mut notes,
            &[true, false],
            Edit::Move(-1),
            off,
            0
        ));
        assert!(edit_selection(
            &mut notes,
            &[false, true],
            Edit::Move(120),
            off,
            0
        ));
        assert!(edit_selection(
            &mut notes,
            &[false, true],
            Edit::Transpose(-12),
            off,
            0
        ));
        assert_eq!((notes[1].start, notes[1].key), (360, 48));
        // Length keeps a tick; velocity stays in 0..1.
        assert!(edit_selection(
            &mut notes,
            &[false, true],
            Edit::Length(-100),
            off,
            0
        ));
        assert_eq!(notes[1].length, 1);
        assert!(edit_selection(
            &mut notes,
            &[false, true],
            Edit::Velocity(0.5),
            off,
            0
        ));
        assert_eq!(notes[1].velocity, 1.0);
        // Up a scale degree: E to F in C major, F to G.
        let mut notes = vec![n(0, 10, 64), n(0, 10, 65)];
        assert!(edit_selection(
            &mut notes,
            &[true, true],
            Edit::Degree(1),
            ScaleKind::Major,
            0
        ));
        assert_eq!((notes[0].key, notes[1].key), (65, 67));
    }

    #[test]
    fn paste_selects_the_copies() {
        let mut st = PianoRollState::default();
        let mut notes = vec![n(0, 240, 60), n(480, 240, 62)];
        st.selected = vec![true, true];
        st.owner = vec![0, 0];
        let block = selected_block(&st, &notes);
        paste(&mut notes, &mut st, &block, 960);
        assert_eq!(notes.len(), 4);
        assert_eq!(st.selected, [false, false, true, true]);
        assert_eq!(notes[2].start, 960);
    }

    #[test]
    fn scale_snapping() {
        let major = ScaleKind::Major;
        // In the scale: unchanged.
        assert_eq!(major.snap(0, 64, 0), 64);
        // C# in C major: down to C when undirected or moving down, up to D when moving up.
        assert_eq!(major.snap(0, 61, 0), 60);
        assert_eq!(major.snap(0, 61, -1), 60);
        assert_eq!(major.snap(0, 61, 1), 62);
        // A minor pentatonic (A C D E G): B goes down to A, or up to C.
        let pent = ScaleKind::MinorPentatonic;
        assert_eq!(pent.snap(9, 71, -1), 69);
        assert_eq!(pent.snap(9, 71, 1), 72);
        // Steps walk the scale and stop at the ends of the range.
        assert_eq!(major.step(0, 64, 1), Some(65));
        assert_eq!(major.step(0, 60, -1), Some(59));
        assert_eq!(major.step(0, 127, 1), None);
        assert_eq!(ScaleKind::Off.snap(0, 61, 1), 61);
        assert_eq!(ScaleKind::Off.step(0, 61, 1), Some(62));
    }

    #[test]
    fn stretch_maths() {
        // Two notes over one bar [0, 3840) stretched to half a bar: everything halves.
        let notes = [n(0, 960, 60), n(1920, 1920, 62)];
        let s = stretch_notes(&notes, 0, 3840, 1920);
        assert_eq!((s[0].start, s[0].length), (0, 480));
        assert_eq!((s[1].start, s[1].length), (960, 960));
        // From an offset span: [960, 1920) to [960, 3840) doubles distances from 960.
        let notes = [n(960, 240, 60), n(1440, 480, 62)];
        let s = stretch_notes(&notes, 960, 1920, 3840);
        assert_eq!((s[0].start, s[0].length), (960, 720));
        assert_eq!((s[1].start, s[1].length), (2400, 1440));
        // Squeezed to almost nothing, every note keeps a tick.
        let s = stretch_notes(&notes, 960, 1920, 961);
        assert!(s.iter().all(|x| x.length >= 1 && x.start >= 960));
        // Keys and per-note values ride along.
        assert_eq!(s[1].key, 62);
    }

    #[test]
    fn note_props_map_to_zero_one() {
        let mut x = n(0, 10, 60);
        for prop in NoteProp::ALL {
            prop.set(&mut x, 0.25);
            assert!((prop.get(&x) - 0.25).abs() < 0.01, "{prop:?}");
        }
        assert_eq!(x.pan, -0.5);
        assert_eq!(x.fine, -50.0);
    }

    #[test]
    fn fold_geometry_shows_scale_rows_and_rows_with_notes() {
        let st = PianoRollState {
            scale: ScaleKind::MajorPentatonic,
            scale_root: 0,
            ..PianoRollState::default()
        };
        let notes = [n(0, 10, 61)];
        let rows = fold_rows(&st, &notes, std::iter::empty()).unwrap();
        // Five pentatonic keys per octave, plus C#5 that has a note.
        assert!(rows.contains(&61) && rows.contains(&60) && !rows.contains(&63));
        assert!(rows.windows(2).all(|w| w[0] > w[1]));
        let geo = Geo {
            grid: Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 400.0)),
            px_per_tick: 0.1,
            key_h: 10.0,
            scroll_tick: 0.0,
            scroll_rows: 0.0,
            rows: Some(rows.clone()),
        };
        for (r, &k) in rows.iter().enumerate().take(40) {
            assert_eq!(geo.key(geo.y(k) + 1.0), k);
            assert_eq!(geo.row(k), r as i32);
        }
        // No scale and no notes: nothing to fold to.
        assert!(fold_rows(&PianoRollState::default(), &[], std::iter::empty()).is_none());
    }

    #[test]
    fn geometry_round_trips() {
        let geo = Geo {
            grid: Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 400.0)),
            px_per_tick: 64.0 / 960.0,
            key_h: 14.0,
            scroll_tick: 960.0,
            scroll_rows: 43.0,
            rows: None,
        };
        assert_eq!(geo.tick(geo.x(1920) + 0.01), 1920);
        assert_eq!(geo.key(geo.y(72) + 1.0), 72);
        assert_eq!(geo.key(geo.grid.top() + 0.5), 127 - 43);
    }

    /// Drives the roll through egui with synthetic mouse input.
    struct Harness {
        ctx: egui::Context,
        st: PianoRollState,
        notes: Vec<Note>,
        /// Another channel (id 7) in the pattern.
        ghost: Vec<Note>,
        automation: Vec<Automation>,
        controls: Vec<ControlPoint>,
        theme: GloomTheme,
        playhead: Option<i64>,
    }

    impl Harness {
        fn new(notes: Vec<Note>) -> Self {
            let mut h = Self {
                ctx: egui::Context::default(),
                st: PianoRollState {
                    scroll_rows: (127 - 72) as f32,
                    ..PianoRollState::default()
                },
                notes,
                ghost: Vec::new(),
                automation: Vec::new(),
                controls: Vec::new(),
                theme: GloomTheme::default(),
                playhead: None,
            };
            h.frame(vec![]);
            h
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<PianoRollAction> {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 700.0))),
                events,
                ..Default::default()
            };
            let mut actions = Vec::new();
            let (st, notes, theme) = (&mut self.st, &mut self.notes, &self.theme);
            let (ghost, playhead) = (&mut self.ghost, self.playhead);
            let automation = &mut self.automation;
            let controls = &mut self.controls;
            let automation_names = vec!["Line".to_owned(); automation.len()];
            let mut out = self.ctx.run_ui(input, |ui| {
                actions = piano_roll(
                    ui,
                    theme,
                    st,
                    PianoRollView {
                        notes,
                        ghosts: vec![GhostChannel {
                            id: 7,
                            name: "Other",
                            notes: ghost,
                        }],
                        pattern_len: 3840,
                        bar_ticks: 3840,
                        playhead,
                        channel_name: "Test",
                        controls,
                        automation,
                        automation_names: automation_names.clone(),
                        params: Vec::new(),
                    },
                );
            });
            out.textures_delta.clear();
            actions
        }

        fn at(&self, tick: i64, key: i32) -> Pos2 {
            let g = self.st.last_geo.clone().unwrap();
            pos2(g.x(tick) + 2.0, g.y(key) + g.key_h / 2.0)
        }

        fn button(
            &mut self,
            p: Pos2,
            button: egui::PointerButton,
            pressed: bool,
        ) -> Vec<PianoRollAction> {
            self.frame(vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ])
        }

        /// A key press with the pointer at `over`.
        fn key(
            &mut self,
            over: Pos2,
            key: egui::Key,
            modifiers: egui::Modifiers,
        ) -> Vec<PianoRollAction> {
            self.frame(vec![
                egui::Event::PointerMoved(over),
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
            ])
        }

        fn drag_with(
            &mut self,
            from: Pos2,
            to: Pos2,
            modifiers: egui::Modifiers,
        ) -> Vec<PianoRollAction> {
            let press = |pressed, pos| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers,
            };
            let mut a = self.frame(vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::PointerMoved(from),
                press(true, from),
            ]);
            a.extend(self.frame(vec![egui::Event::PointerMoved(from.lerp(to, 0.5))]));
            a.extend(self.frame(vec![egui::Event::PointerMoved(to)]));
            a.extend(self.frame(vec![press(false, to)]));
            a.extend(self.frame(vec![egui::Event::ModifiersChanged(egui::Modifiers::NONE)]));
            a
        }

        fn drag(
            &mut self,
            from: Pos2,
            to: Pos2,
            button: egui::PointerButton,
        ) -> Vec<PianoRollAction> {
            let mut a = self.button(from, button, true);
            a.extend(self.frame(vec![egui::Event::PointerMoved(from.lerp(to, 0.5))]));
            a.extend(self.frame(vec![egui::Event::PointerMoved(to)]));
            a.extend(self.button(to, button, false));
            a.extend(self.frame(vec![]));
            a
        }
    }

    #[test]
    fn the_bottom_lane_draws_and_erases_pattern_automation() {
        use egui::PointerButton::{Primary, Secondary};
        let target = gt_core::ParamId::strip_pan(1);
        let mut h = Harness::new(vec![]);
        h.automation.push(Automation {
            target,
            points: vec![AutoPoint::new(0, 0.5), AutoPoint::new(3840, 0.5)],
        });
        h.st.lane = Lane::Automation(target);
        h.frame(vec![]);
        let g = h.st.last_geo.clone().unwrap();
        let top = g.grid.bottom() + 4.0;
        let at = |tick: i64, v: f32| pos2(g.x(tick), top + 2.0 + (VELOCITY_H - 4.0) * (1.0 - v));
        // A drag across a beat at full height, snapped to sixteenths.
        let a = h.drag(at(960, 1.0), at(1920, 1.0), Primary);
        assert!(a.contains(&PianoRollAction::AutomationChanged));
        let pts = &h.automation[0].points;
        assert!(pts.iter().any(|p| p.at == 960 && p.value > 0.99));
        assert!(pts.iter().any(|p| p.at == 1920 && p.value > 0.99));
        assert!(pts.iter().all(|p| p.at % 240 == 0));
        assert!(
            pts.windows(2).all(|w| w[0].at < w[1].at),
            "sorted, one per tick"
        );
        let n = pts.len();
        // A right click removes the point under it.
        let a = h.button(at(1920, 1.0), Secondary, true);
        assert!(a.is_empty());
        let a = h.button(at(1920, 1.0), Secondary, false);
        assert!(a.contains(&PianoRollAction::AutomationChanged));
        assert_eq!(h.automation[0].points.len(), n - 1);
        assert!(h.automation[0].points.iter().all(|p| p.at != 1920));
    }

    #[test]
    fn draw_move_resize_and_erase_with_the_mouse() {
        use egui::PointerButton::{Primary, Secondary};
        let mut h = Harness::new(vec![]);

        // Click on empty space draws a 1/16 note at the snapped position.
        let p = h.at(500, 64);
        let a = h.drag(p, p, Primary);
        assert!(a.contains(&PianoRollAction::Changed));
        assert!(a.contains(&PianoRollAction::AuditionOn {
            key: 64,
            velocity: gt_core::project::DEFAULT_VELOCITY
        }));
        assert!(a.contains(&PianoRollAction::AuditionOff { key: 64 }));
        assert_eq!(
            h.notes,
            [Note::new(480, 240, 64, gt_core::project::DEFAULT_VELOCITY)]
        );

        // Drag it one beat later and two semitones up.
        let a = h.drag(h.at(500, 64), h.at(1460, 66), Primary);
        assert!(a.contains(&PianoRollAction::Changed));
        assert_eq!((h.notes[0].start, h.notes[0].key), (1440, 66));
        assert!(!h.st.is_dragging());

        // Drag the right edge to make it a quarter note.
        let g = h.st.last_geo.clone().unwrap();
        let r = g.note_rect(&h.notes[0]);
        let grip = pos2(r.right() - 2.0, r.center().y);
        h.drag(grip, pos2(g.x(2400), r.center().y), Primary);
        assert_eq!(h.notes[0].length, 960);

        // Right-click deletes it.
        let p = h.at(1500, 66);
        let a = h.drag(p, p, Secondary);
        assert!(a.contains(&PianoRollAction::Changed));
        assert!(h.notes.is_empty());
    }

    #[test]
    fn cancel_ends_a_drag_and_releases_the_audition() {
        let mut h = Harness::new(vec![]);
        let p = h.at(500, 64);
        h.button(p, egui::PointerButton::Primary, true);
        assert!(h.st.is_dragging());
        assert_eq!(
            h.st.cancel(),
            Some(PianoRollAction::AuditionOff { key: 64 })
        );
        assert!(!h.st.is_dragging());
        // The notes are replaced (as undo does) while the button is still down: no panic, and
        // nothing is edited.
        h.notes.clear();
        h.frame(vec![egui::Event::PointerMoved(h.at(1500, 70))]);
        assert!(h.notes.is_empty());
        h.button(h.at(1500, 70), egui::PointerButton::Primary, false);
        // A new click draws again.
        let p = h.at(960, 60);
        h.drag(p, p, egui::PointerButton::Primary);
        assert_eq!(h.notes.len(), 1);
    }

    #[test]
    fn box_select_then_delete_key() {
        let notes = vec![n(0, 240, 60), n(960, 240, 62), n(1920, 240, 64)];
        let mut h = Harness::new(notes);
        h.st.tool = Tool::Select;
        // Box around the first two notes.
        h.drag(h.at(0, 63), h.at(1300, 59), egui::PointerButton::Primary);
        assert_eq!(h.st.selected, [true, true, false]);
        // Delete, with the pointer over the roll.
        let over = h.at(3000, 70);
        let a = h.frame(vec![
            egui::Event::PointerMoved(over),
            egui::Event::Key {
                key: egui::Key::Delete,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        assert!(a.contains(&PianoRollAction::Changed));
        assert_eq!(h.notes, [n(1920, 240, 64)]);
    }

    #[test]
    fn paste_lands_at_the_mouse_or_the_playhead() {
        let mut h = Harness::new(vec![n(240, 240, 60), n(480, 240, 64)]);
        h.st.selected = vec![true, true];
        let over = h.at(3000, 70);
        h.frame(vec![egui::Event::PointerMoved(over), egui::Event::Copy]);
        // Over the grid at beat 2 (snapped down to the sixteenth): the block starts there.
        h.frame(vec![
            egui::Event::PointerMoved(h.at(1930, 70)),
            egui::Event::Paste(String::new()),
        ]);
        let starts: Vec<i64> = h.notes.iter().map(|x| x.start).collect();
        assert_eq!(starts, [240, 480, 1920, 2160]);
        assert_eq!(h.st.selected, [false, false, true, true]);
        // Pointer over the lane, not the grid: the block lands at the playhead.
        h.playhead = Some(2900);
        let lane = h.st.last_geo.clone().unwrap().grid.left_bottom() + vec2(400.0, 30.0);
        h.frame(vec![
            egui::Event::PointerMoved(lane),
            egui::Event::Paste(String::new()),
        ]);
        assert!(h.notes.iter().any(|x| x.start == 2880 && x.key == 60));
        assert!(h.notes.iter().any(|x| x.start == 3120 && x.key == 64));
    }

    #[test]
    fn keys_nudge_resize_and_change_velocity_by_snap_or_fine_steps() {
        use egui::{Key, Modifiers};
        let mut h = Harness::new(vec![n(960, 240, 60)]);
        h.st.selected = vec![true];
        let over = h.at(3000, 70);
        h.key(over, Key::ArrowRight, Modifiers::NONE);
        assert_eq!(h.notes[0].start, 1200);
        h.key(over, Key::ArrowLeft, Modifiers::ALT);
        assert_eq!(h.notes[0].start, 1200 - FINE_TICKS);
        h.key(over, Key::ArrowRight, Modifiers::SHIFT);
        assert_eq!(h.notes[0].length, 480);
        h.key(over, Key::ArrowLeft, Modifiers::SHIFT | Modifiers::ALT);
        assert_eq!(h.notes[0].length, 480 - FINE_TICKS);
        let v = h.notes[0].velocity;
        h.key(over, Key::ArrowDown, Modifiers::ALT);
        assert!((h.notes[0].velocity - (v - 0.1)).abs() < 1e-6);
        h.key(over, Key::ArrowUp, Modifiers::SHIFT | Modifiers::ALT);
        assert!((h.notes[0].velocity - (v - 0.09)).abs() < 1e-6);
        h.key(over, Key::ArrowUp, Modifiers::SHIFT);
        assert_eq!(h.notes[0].key, 72);
        // M mutes and unmutes.
        h.key(over, Key::M, Modifiers::NONE);
        assert!(h.notes[0].muted);
        h.key(over, Key::M, Modifiers::NONE);
        assert!(!h.notes[0].muted);
    }

    #[test]
    fn scale_snap_applies_to_drawing_and_moving() {
        use egui::PointerButton::Primary;
        let mut h = Harness::new(vec![]);
        h.st.scale = ScaleKind::Major;
        h.st.scale_snap = true;
        // C#5 is not in C major: the note lands on C5.
        let p = h.at(0, 61);
        h.drag(p, p, Primary);
        assert_eq!(h.notes[0].key, 60);
        // Dragged up three rows to D#5: the next scale key up is E5.
        h.drag(h.at(10, 60), h.at(10, 63), Primary);
        assert_eq!(h.notes[0].key, 64);
    }

    #[test]
    fn stretch_drag_scales_the_selection() {
        let mut h = Harness::new(vec![n(0, 480, 60), n(960, 960, 62)]);
        h.st.selected = vec![true, true];
        h.frame(vec![]);
        let g = h.st.last_geo.clone().unwrap();
        let r = g.note_rect(&h.notes[1]);
        // Ctrl+drag the last note's end from 1920 to 3840: the selection doubles.
        h.drag_with(
            pos2(r.right() - 2.0, r.center().y),
            pos2(g.x(3840), r.center().y),
            egui::Modifiers::COMMAND,
        );
        let got: Vec<(i64, i64)> = h.notes.iter().map(|x| (x.start, x.length)).collect();
        assert_eq!(got, [(0, 960), (1920, 1920)]);
    }

    #[test]
    fn editable_ghost_channels_are_edited_with_the_channel() {
        let mut h = Harness::new(vec![n(0, 240, 60)]);
        h.ghost = vec![n(960, 240, 67)];
        h.st.editable = vec![7];
        h.frame(vec![]);
        // Both channels' notes are in the roll; a box selects them, Right moves both.
        h.st.tool = Tool::Select;
        h.drag(h.at(0, 70), h.at(1300, 55), egui::PointerButton::Primary);
        assert_eq!(h.st.selected, [true, true]);
        h.key(h.at(3000, 70), egui::Key::ArrowRight, egui::Modifiers::NONE);
        assert_eq!(h.notes, [n(240, 240, 60)]);
        assert_eq!(h.ghost, [n(1200, 240, 67)]);
        // Dragging the other channel's note keeps it in its channel.
        h.st.selected = vec![false, true];
        h.drag(h.at(1210, 67), h.at(1450, 69), egui::PointerButton::Primary);
        assert_eq!(h.ghost, [n(1440, 240, 69)]);
        assert_eq!(h.notes, [n(240, 240, 60)]);
        // Copy both, paste at bar 2 with the pointer there: each copy goes to its channel.
        h.st.selected = vec![true, true];
        h.frame(vec![egui::Event::Copy]);
        h.frame(vec![
            egui::Event::PointerMoved(h.at(1920, 50)),
            egui::Event::Paste(String::new()),
        ]);
        assert_eq!(h.notes.len(), 2);
        assert_eq!(h.ghost.len(), 2);
        assert_eq!(h.notes[1].start, 1920);
        assert_eq!(h.ghost[1].start, 1920 + 1200);
        // Not editable any more: the ghost's notes are left alone.
        h.st.editable.clear();
        h.st.selected = vec![true, true];
        h.key(h.at(3000, 70), egui::Key::Delete, egui::Modifiers::NONE);
        assert!(h.notes.is_empty());
        assert_eq!(h.ghost.len(), 2);
    }

    /// 10,000 notes, all visible at once (worst case): build and tessellate one frame.
    /// Run with `cargo test --release -p gt-ui -- --ignored --nocapture` to see timings.
    #[test]
    #[ignore = "timing; run in release"]
    fn ten_thousand_notes_frame_time() {
        let mut notes: Vec<Note> = (0..10_000)
            .map(|i| n(i * 60, 50, 36 + (i % 48) as u8))
            .collect();
        let mut ghost: Vec<Note> = (0..2_000).map(|i| n(i * 300, 200, 60)).collect();
        let theme = GloomTheme::default();
        let ctx = egui::Context::default();
        let mut st = PianoRollState {
            px_per_beat: 6.0, // zoomed out: everything on screen
            key_height: 6.0,
            ..PianoRollState::default()
        };
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
            ..Default::default()
        };
        let mut times = Vec::new();
        for _ in 0..30 {
            let t = std::time::Instant::now();
            let mut out = ctx.run_ui(input(), |ui| {
                piano_roll(
                    ui,
                    &theme,
                    &mut st,
                    PianoRollView {
                        notes: &mut notes,
                        ghosts: vec![GhostChannel {
                            id: 99,
                            name: "Ghost",
                            notes: &mut ghost,
                        }],
                        pattern_len: 640_000,
                        bar_ticks: 3840,
                        playhead: Some(1000),
                        channel_name: "Test",
                        controls: &mut Vec::new(),
                        automation: &mut Vec::new(),
                        automation_names: Vec::new(),
                        params: Vec::new(),
                    },
                );
            });
            let prims = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            out.textures_delta.clear();
            times.push(t.elapsed());
            assert!(!prims.is_empty());
        }
        times.sort();
        let median = times[times.len() / 2];
        println!(
            "10k notes: median frame {median:?}, worst {:?}",
            times.last().unwrap()
        );
        assert!(median < std::time::Duration::from_millis(16), "{median:?}");
    }

    /// Where value `v` of tick `tick` sits in the bottom lane.
    fn lane_at(h: &Harness, tick: i64, v: f32) -> Pos2 {
        let g = h.st.last_geo.clone().unwrap();
        let top = g.grid.bottom() + 4.0;
        pos2(g.x(tick), top + 2.0 + (VELOCITY_H - 4.0) * (1.0 - v))
    }

    #[test]
    fn note_curve_lane_draws_each_notes_own_curve() {
        use egui::PointerButton::{Primary, Secondary};
        use gt_core::CurveKind;
        let mut h = Harness::new(vec![n(0, 960, 60), n(960, 960, 64)]);
        h.st.lane = Lane::Curve(CurveKind::Timbre);
        // A drag over the second note only: its curve rises, the first note keeps none.
        let a = h.drag(lane_at(&h, 1000, 0.1), lane_at(&h, 1900, 0.9), Primary);
        assert!(a.contains(&PianoRollAction::Changed));
        assert!(h.notes[0].curve.is_off());
        let c = h.notes[1].curve;
        assert_eq!(c.kind, CurveKind::Timbre);
        assert!(c.points().len() >= 2, "{c:?}");
        assert!(c.value_at(0.0) < c.value_at(1.0));
        // Right-click on a point removes it.
        let before = c.points().len();
        let at = 960 + (c.points()[0].at * 960.0) as i64;
        h.drag(lane_at(&h, at, 0.5), lane_at(&h, at, 0.5), Secondary);
        assert_eq!(h.notes[1].curve.points().len(), before - 1);
    }

    #[test]
    fn line_and_curve_drags_in_the_lanes() {
        use egui::PointerButton::Primary;
        let mut h = Harness::new((0..4).map(|i| n(i * 960, 240, 60)).collect());
        // A velocity line from 0 % to 100 % across the notes ramps them up.
        h.st.lane_draw = LaneDraw::Line;
        let a = h.drag(lane_at(&h, 0, 0.0), lane_at(&h, 2880, 1.0), Primary);
        assert!(a.contains(&PianoRollAction::Changed));
        let v: Vec<f32> = h.notes.iter().map(|n| n.velocity).collect();
        assert!(v.windows(2).all(|w| w[0] < w[1]), "{v:?}");
        assert!(v[3] > 0.95 && v[0] < 0.1, "{v:?}");
        // A pan line works the same way on another note property.
        h.st.lane = Lane::Note(NoteProp::Pan);
        h.drag(lane_at(&h, 0, 1.0), lane_at(&h, 2880, 1.0), Primary);
        assert!(h.notes.iter().all(|n| n.pan > 0.95));

        // A curve in a CC lane writes points of that controller only.
        h.st.lane = Lane::Control(ControlKind::Cc(74));
        h.st.lane_draw = LaneDraw::Curve;
        h.controls.push(ControlPoint {
            tick: 0,
            kind: ControlKind::Bend,
            value: 0.5,
        });
        h.drag(lane_at(&h, 960, 0.0), lane_at(&h, 1920, 1.0), Primary);
        let cc: Vec<&ControlPoint> = h
            .controls
            .iter()
            .filter(|c| c.kind == ControlKind::Cc(74))
            .collect();
        assert!(cc.len() >= 4, "{cc:?}");
        assert!(cc
            .windows(2)
            .all(|w| w[0].tick < w[1].tick && w[0].value <= w[1].value));
        assert_eq!(
            h.controls
                .iter()
                .filter(|c| c.kind == ControlKind::Bend)
                .count(),
            1
        );

        // Freehand in the CC lane draws too.
        h.st.lane = Lane::Control(ControlKind::Cc(7));
        h.st.lane_draw = LaneDraw::Freehand;
        h.drag(lane_at(&h, 0, 0.5), lane_at(&h, 960, 0.5), Primary);
        assert!(h.controls.iter().any(|c| c.kind == ControlKind::Cc(7)));
    }

    #[test]
    fn chord_stamp_places_the_chord_on_click() {
        use egui::PointerButton::Primary;
        let mut h = Harness::new(vec![]);
        h.st.chord = Some(gt_core::note_tools::ChordKind::Minor);
        let p = h.at(0, 60);
        let a = h.drag(p, p, Primary);
        assert!(a.contains(&PianoRollAction::Changed));
        let mut keys: Vec<u8> = h.notes.iter().map(|n| n.key).collect();
        keys.sort_unstable();
        assert_eq!(keys, [60, 63, 67]);
        assert!(h.notes.iter().all(|n| n.start == 0));
        // First inversion: the root goes up an octave.
        h.notes.clear();
        h.st.chord_inversion = 1;
        let p = h.at(960, 60);
        h.drag(p, p, Primary);
        let mut keys: Vec<u8> = h.notes.iter().map(|n| n.key).collect();
        keys.sort_unstable();
        assert_eq!(keys, [63, 67, 72]);
    }

    #[test]
    fn tools_preview_live_and_cancel_or_apply() {
        let chord = vec![
            n(0, 960, 60),
            n(0, 960, 64),
            n(0, 960, 67),
            n(1920, 240, 50),
        ];
        let mut h = Harness::new(chord.clone());
        // Only the chord is selected; the low note stays as it is.
        h.st.selected = vec![true, true, true, false];
        h.st.pending_tool = Some(tools::ToolKind::Arpeggiate);
        let a = h.frame(vec![]);
        assert!(a.contains(&PianoRollAction::Changed));
        assert!(h.st.is_dragging(), "no undo step while the window is open");
        // Sixteenth arpeggio over a beat: four notes, keys going up and wrapping.
        let arp: Vec<&Note> = h.notes.iter().filter(|n| n.start < 1920).collect();
        assert_eq!(arp.len(), 4);
        assert_eq!(
            arp.iter().map(|n| n.key).collect::<Vec<_>>(),
            [60, 64, 67, 60]
        );
        assert!(h.notes.iter().any(|n| n.key == 50 && n.start == 1920));
        // A changed setting rebuilds from the original notes.
        h.st.tools.arp.pattern = gt_core::note_tools::ArpPattern::Down;
        h.frame(vec![]);
        let keys: Vec<u8> = h
            .notes
            .iter()
            .filter(|n| n.start < 1920)
            .map(|n| n.key)
            .collect();
        assert_eq!(keys, [67, 64, 60, 67]);
        // Cancel puts everything back.
        assert!(tools::finish(&mut h.st, &mut h.notes, false));
        assert_eq!(h.notes, chord);
        assert!(!h.st.is_dragging());

        // Apply keeps the preview; nothing selected means every note.
        h.st.selected = vec![false; 4];
        h.st.pending_tool = Some(tools::ToolKind::ScaleVelocity);
        h.st.tools.vel_scale = 0.5;
        h.st.tools.vel_add = 0.0;
        h.frame(vec![]);
        assert!(!tools::finish(&mut h.st, &mut h.notes, true));
        for (a, b) in h.notes.iter().zip(&chord) {
            assert!((a.velocity - b.velocity * 0.5).abs() < 1e-4);
        }
    }

    #[test]
    fn the_generator_writes_into_the_scale_and_keeps_existing_notes() {
        let mut h = Harness::new(vec![n(0, 240, 90)]);
        h.st.scale = ScaleKind::Minor;
        h.st.scale_root = 9; // A minor
        h.st.pending_tool = Some(tools::ToolKind::Generate);
        h.frame(vec![]);
        tools::finish(&mut h.st, &mut h.notes, true);
        assert!(h.notes.len() > 8);
        assert!(h.notes.iter().any(|n| n.key == 90 && n.start == 0));
        assert!(h
            .notes
            .iter()
            .all(|n| n.key == 90 || ScaleKind::Minor.contains(9, n.key)));
        assert!(h.notes.iter().all(|n| n.start < 4 * 3840));
    }
}
