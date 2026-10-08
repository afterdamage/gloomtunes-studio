//! Piano roll: a custom-painted note editor for one channel of one pattern.
//!
//! Layout: toolbar on top; below it a keyboard on the left, a bar ruler on top, the note grid,
//! and a velocity lane at the bottom.
//!
//! Mouse:
//! - Draw tool: click empty space to add a note (snapped, last used length), drag it to move.
//!   Ctrl+drag on empty space box-selects.
//! - Select tool: drag on empty space box-selects; Shift adds to the selection.
//! - Drag a note to move the selection, drag its right edge to resize it.
//! - Right-click or right-drag deletes notes.
//! - Velocity lane: drag to paint velocities (only selected notes, when there is a selection).
//! - Wheel scrolls (Shift: sideways), Ctrl+wheel zooms time, Alt+wheel zooms keys.
//!
//! Keys (while the pointer is over the roll): Delete, Ctrl+A, Ctrl+C/X/V, Ctrl+D (duplicate
//! after), arrows (move by snap / transpose; Shift+Up/Down an octave), Q quantize, P draw tool,
//! E select tool. Undo/redo is handled by the app.
//!
//! Performance: notes are kept sorted by start, so only notes that can reach the visible range
//! are visited (binary search back by the longest note length). Ghost notes are culled the
//! same way. While a drag is in progress the list is temporarily unsorted and a linear range
//! check is used instead; it is sorted again on release.

use egui::{pos2, vec2, Color32, Pos2, Rect, Response, RichText, Sense, Stroke, Ui};
use gt_core::{Note, PPQ};

use crate::GloomTheme;

const KEYBOARD_W: f32 = 46.0;
const RULER_H: f32 = 18.0;
const VELOCITY_H: f32 = 64.0;
const RESIZE_GRIP_PX: f32 = 6.0;
const MIN_PX_PER_BEAT: f32 = 6.0;
const MAX_PX_PER_BEAT: f32 = 600.0;
const MIN_KEY_H: f32 = 5.0;
const MAX_KEY_H: f32 = 40.0;
const KEYS: i32 = 128;

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
    /// Rubber-band selection from a grid position.
    Box { from: Pos2, additive: bool },
    /// Right-button erasing.
    Erase,
    /// Painting velocities.
    Velocity,
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
    clipboard: Vec<Note>,
    drag: Option<Drag>,
    audition_key: Option<u8>,
    /// What the roll is editing; selection resets when it changes.
    target: Option<(u32, u32)>,
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
            selected: Vec::new(),
            quantize_strength: 1.0,
            humanize_ticks: 20,
            humanize_velocity: 0.1,
            last_length: PPQ / 4,
            last_velocity: gt_core::project::DEFAULT_VELOCITY,
            clipboard: Vec::new(),
            drag: None,
            audition_key: None,
            target: None,
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
            self.target = Some((pattern, channel));
            self.selected.clear();
            self.drag = None;
        }
    }

    /// True while a mouse gesture is changing notes (the app commits undo steps after it).
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Abandons any gesture in progress, e.g. before the notes are replaced by undo or when the
    /// roll is hidden mid-drag. Returns the note-off for a key still being auditioned.
    pub fn cancel(&mut self) -> Option<PianoRollAction> {
        self.drag = None;
        self.audition_key
            .take()
            .map(|key| PianoRollAction::AuditionOff { key })
    }
}

/// What the roll edits and shows.
pub struct PianoRollView<'a> {
    /// The edited notes (kept sorted by start, key between frames).
    pub notes: &'a mut Vec<Note>,
    /// Other channels' notes in the same pattern, each list sorted by start.
    pub ghosts: Vec<&'a [Note]>,
    /// Pattern length in ticks (shown as the end of the loop).
    pub pattern_len: i64,
    /// Ticks per bar.
    pub bar_ticks: i64,
    /// Playhead in pattern ticks while playing.
    pub playhead: Option<i64>,
    /// Name of the edited channel.
    pub channel_name: &'a str,
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
}

/// Geometry of the editing area for one frame.
#[derive(Debug, Clone, Copy)]
struct Geo {
    grid: Rect,
    px_per_tick: f32,
    key_h: f32,
    scroll_tick: f64,
    scroll_rows: f32,
}

impl Geo {
    fn x(&self, tick: i64) -> f32 {
        self.grid.left() + ((tick as f64 - self.scroll_tick) as f32) * self.px_per_tick
    }
    fn tick(&self, x: f32) -> i64 {
        (self.scroll_tick + f64::from((x - self.grid.left()) / self.px_per_tick)).floor() as i64
    }
    fn y(&self, key: i32) -> f32 {
        self.grid.top() + ((KEYS - 1 - key) as f32 - self.scroll_rows) * self.key_h
    }
    fn key(&self, y: f32) -> i32 {
        let row = ((y - self.grid.top()) / self.key_h + self.scroll_rows).floor() as i32;
        (KEYS - 1 - row).clamp(0, KEYS - 1)
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

/// Draws the piano roll and edits `view.notes` in place.
pub fn piano_roll(
    ui: &mut Ui,
    theme: &GloomTheme,
    st: &mut PianoRollState,
    view: PianoRollView<'_>,
) -> Vec<PianoRollAction> {
    let mut actions = Vec::new();
    st.selected.resize(view.notes.len(), false);
    toolbar(ui, theme, st, &view, &mut actions);
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

    // Zoom and scroll first, so this frame draws with the new view.
    let hovered = ui.rect_contains_pointer(area);
    if hovered {
        handle_wheel(ui, st, grid);
    }
    clamp_scroll(st, grid);
    let geo = Geo {
        grid,
        px_per_tick: st.px_per_beat / PPQ as f32,
        key_h: st.key_height,
        scroll_tick: st.scroll_tick,
        scroll_rows: st.scroll_rows,
    };

    #[cfg(test)]
    {
        st.last_geo = Some(geo);
    }
    let grid_resp = ui.interact(grid, ui.id().with("roll_grid"), Sense::click_and_drag());
    let lane_resp = ui.interact(lane, ui.id().with("roll_lane"), Sense::click_and_drag());
    let key_resp = ui.interact(keyboard, ui.id().with("roll_keys"), Sense::click_and_drag());

    let notes = view.notes;
    let mut changed = false;
    changed |= grid_input(ui, st, &geo, &grid_resp, notes, &mut actions);
    changed |= lane_input(ui, st, &geo, lane, &lane_resp, notes);
    keyboard_input(ui, st, &geo, &key_resp, &mut actions);
    if hovered && !ui.ctx().text_edit_focused() {
        changed |= key_commands(ui, st, notes, &mut actions);
    }
    if changed {
        actions.push(PianoRollAction::Changed);
    }

    let sorted = st.drag.is_none();
    paint(
        ui,
        theme,
        st,
        &geo,
        area,
        lane,
        notes,
        &view.ghosts,
        view.pattern_len,
        view.bar_ticks,
        view.playhead,
        sorted,
    );
    actions
}

fn toolbar(
    ui: &mut Ui,
    theme: &GloomTheme,
    st: &mut PianoRollState,
    view: &PianoRollView<'_>,
    actions: &mut Vec<PianoRollAction>,
) {
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    ui.horizontal(|ui| {
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
        ui.label(dim("Snap"));
        egui::ComboBox::from_id_salt("roll_snap")
            .width(56.0)
            .selected_text(st.snap.label())
            .show_ui(ui, |ui| {
                for s in Snap::ALL {
                    ui.selectable_value(&mut st.snap, s, s.label());
                }
            });
        ui.separator();
        ui.label(dim("Scale"));
        egui::ComboBox::from_id_salt("roll_scale_root")
            .width(40.0)
            .selected_text(key_name(st.scale_root).trim_end_matches('0').to_owned())
            .show_ui(ui, |ui| {
                for r in 0..12u8 {
                    let name = key_name(r).trim_end_matches('0').to_owned();
                    ui.selectable_value(&mut st.scale_root, r, name);
                }
            });
        egui::ComboBox::from_id_salt("roll_scale")
            .width(120.0)
            .selected_text(st.scale.name())
            .show_ui(ui, |ui| {
                for s in ScaleKind::ALL {
                    ui.selectable_value(&mut st.scale, s, s.name());
                }
            });
        if ui
            .add(egui::Button::selectable(st.ghosts, "Ghosts"))
            .on_hover_text("Show other channels' notes in this pattern")
            .clicked()
        {
            st.ghosts = !st.ghosts;
        }
        ui.separator();
        if ui
            .button("Quantize")
            .on_hover_text("Snap note starts of the selection (or all notes) to the grid (Q)")
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
            .on_hover_text("Quantize strength")
            .changed()
        {
            st.quantize_strength = q / 100.0;
        }
        if ui
            .button("Humanize")
            .on_hover_text("Randomly shift timing and velocity of the selection (or all notes)")
            .clicked()
        {
            actions.push(PianoRollAction::Humanize);
        }
        ui.add(
            egui::DragValue::new(&mut st.humanize_ticks)
                .range(0..=240)
                .suffix(" ticks")
                .speed(0.5),
        )
        .on_hover_text("Humanize timing range (±, 960 ticks per beat)");
        let mut hv = st.humanize_velocity * 100.0;
        if ui
            .add(
                egui::DragValue::new(&mut hv)
                    .range(0.0..=100.0)
                    .suffix(" % vel")
                    .speed(0.5),
            )
            .on_hover_text("Humanize velocity range (±)")
            .changed()
        {
            st.humanize_velocity = hv / 100.0;
        }
        let n_sel = st.selected.iter().filter(|&&s| s).count();
        ui.label(dim(&format!(
            "{} notes, {} selected",
            view.notes.len(),
            n_sel
        )));
    });
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

fn clamp_scroll(st: &mut PianoRollState, grid: Rect) {
    let visible_rows = grid.height() / st.key_height;
    st.scroll_rows = st
        .scroll_rows
        .clamp(0.0, (KEYS as f32 - visible_rows).max(0.0));
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

fn sort_notes(notes: &mut Vec<Note>, selected: &mut Vec<bool>) {
    selected.resize(notes.len(), false);
    let mut pairs: Vec<(Note, bool)> = notes.drain(..).zip(selected.drain(..)).collect();
    pairs.sort_by_key(|(n, _)| (n.start, n.key));
    for (n, s) in pairs {
        notes.push(n);
        selected.push(s);
    }
}

fn delete_where(
    notes: &mut Vec<Note>,
    selected: &mut Vec<bool>,
    kill: impl Fn(usize) -> bool,
) -> bool {
    let before = notes.len();
    let mut keep_n = Vec::with_capacity(before);
    let mut keep_s = Vec::with_capacity(before);
    for (i, (n, s)) in notes.drain(..).zip(selected.drain(..)).enumerate() {
        if !kill(i) {
            keep_n.push(n);
            keep_s.push(s);
        }
    }
    *notes = keep_n;
    *selected = keep_s;
    notes.len() != before
}

fn grid_input(
    ui: &Ui,
    st: &mut PianoRollState,
    geo: &Geo,
    resp: &Response,
    notes: &mut Vec<Note>,
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
    let sorted = st.drag.is_none();

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
                if grip {
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
                let key = geo.key(p.y) as u8;
                let note = Note {
                    start: st.snap.floor(geo.tick(p.x)).max(0),
                    length: st.last_length.max(1),
                    key,
                    velocity: st.last_velocity,
                };
                st.selected.iter_mut().for_each(|s| *s = false);
                notes.push(note);
                st.selected.push(true);
                let i = notes.len() - 1;
                changed = true;
                start_audition(st, key, note.velocity, actions);
                st.drag = Some(Drag::Move {
                    anchor: i,
                    grab_tick: geo.tick(p.x),
                    grab_key: i32::from(key),
                    originals: vec![(i, note)],
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
        Some(Drag::Move { originals, .. } | Drag::Resize { originals, .. }) => {
            originals.iter().any(|(j, _)| *j >= notes.len())
        }
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
                    let moved = Note {
                        start: n.start + dt,
                        key: (i32::from(n.key) + dk) as u8,
                        ..*n
                    };
                    if notes[*j] != moved {
                        notes[*j] = moved;
                        changed = true;
                    }
                }
                let new_key = (i32::from(a.key) + dk) as u8;
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
        Some(Drag::Erase) if secondary_down => {
            if let Some((i, _)) = hit_note(geo, notes, p, sorted) {
                changed |= delete_where(notes, &mut st.selected, |j| j == i);
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
            Some(Drag::Move { .. } | Drag::Resize { .. } | Drag::Erase)
        ) {
            sort_notes(notes, &mut st.selected);
        }
        if !matches!(st.drag, Some(Drag::Velocity)) {
            st.drag = None;
        }
        stop_audition(st, actions);
    }
    changed
}

fn lane_input(
    ui: &Ui,
    st: &mut PianoRollState,
    geo: &Geo,
    lane: Rect,
    resp: &Response,
    notes: &mut [Note],
) -> bool {
    let (pos, pressed, down, released) = ui.input(|i| {
        (
            i.pointer.interact_pos(),
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.any_released(),
        )
    });
    if resp.hovered() && pressed && st.drag.is_none() {
        st.drag = Some(Drag::Velocity);
    }
    if !matches!(st.drag, Some(Drag::Velocity)) {
        return false;
    }
    if released && !down {
        st.drag = None;
        return false;
    }
    let Some(p) = pos else {
        return false;
    };
    let v = (1.0 - (p.y - lane.top()) / lane.height()).clamp(0.01, 1.0);
    let any_sel = st.selected.iter().any(|&s| s);
    let (t0, t1) = (geo.tick(p.x - 3.0), geo.tick(p.x + 3.0));
    let mut changed = false;
    let lo = notes.partition_point(|n| n.start < t0);
    let hi = notes.partition_point(|n| n.start <= t1);
    let hi = hi.max(lo);
    for (n, &sel) in notes[lo..hi].iter_mut().zip(&st.selected[lo..hi]) {
        if any_sel && !sel {
            continue;
        }
        if n.velocity != v {
            n.velocity = v;
            changed = true;
        }
    }
    if changed {
        st.last_velocity = v;
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
    notes: &mut Vec<Note>,
    actions: &mut Vec<PianoRollAction>,
) -> bool {
    use egui::{Event, Key, Modifiers};
    let mut changed = false;
    let events = ui.input(|i| i.events.clone());
    let any_sel = st.selected.iter().any(|&s| s);
    for e in events {
        match e {
            Event::Copy | Event::Cut if any_sel => {
                st.clipboard = selected_indices(st).into_iter().map(|i| notes[i]).collect();
                // The notes stay in our own clipboard. Some text must be on the system
                // clipboard, or the desktop never sends a paste event.
                ui.ctx()
                    .copy_text(format!("GloomTunes Studio: {} notes", st.clipboard.len()));
                if matches!(e, Event::Cut) {
                    let sel = st.selected.clone();
                    changed |= delete_where(notes, &mut st.selected, |i| sel[i]);
                }
            }
            Event::Paste(_) if !st.clipboard.is_empty() => {
                paste(notes, &mut st.selected, &st.clipboard, 0);
                changed = true;
            }
            _ => {}
        }
    }
    let pressed = |k: Key, m: Modifiers| ui.input_mut(|i| i.consume_key(m, k));
    let shift = Modifiers::SHIFT;
    let cmd = Modifiers::COMMAND;
    if pressed(Key::Delete, Modifiers::NONE) || pressed(Key::Backspace, Modifiers::NONE) {
        let sel = st.selected.clone();
        changed |= delete_where(notes, &mut st.selected, |i| sel[i]);
    }
    if pressed(Key::A, cmd) {
        st.selected.iter_mut().for_each(|s| *s = true);
    }
    if pressed(Key::D, cmd) && any_sel {
        let block: Vec<Note> = selected_indices(st).into_iter().map(|i| notes[i]).collect();
        let start = block.iter().map(|n| n.start).min().unwrap_or(0);
        let end = block.iter().map(|n| n.start + n.length).max().unwrap_or(0);
        let g = st.snap.ticks().max(1);
        let span = ((end - start + g - 1) / g).max(1) * g;
        paste(notes, &mut st.selected, &block, span);
        changed = true;
    }
    if pressed(Key::Q, Modifiers::NONE) {
        actions.push(PianoRollAction::Quantize);
    }
    if pressed(Key::P, Modifiers::NONE) {
        st.tool = Tool::Draw;
    }
    if pressed(Key::E, Modifiers::NONE) {
        st.tool = Tool::Select;
    }
    if any_sel {
        let step = st.snap.ticks();
        // Shift variants first: egui matches shortcuts ignoring extra Shift.
        let moves: [(Key, Modifiers, i64, i32); 6] = [
            (Key::ArrowUp, shift, 0, 12),
            (Key::ArrowDown, shift, 0, -12),
            (Key::ArrowUp, Modifiers::NONE, 0, 1),
            (Key::ArrowDown, Modifiers::NONE, 0, -1),
            (Key::ArrowLeft, Modifiers::NONE, -step, 0),
            (Key::ArrowRight, Modifiers::NONE, step, 0),
        ];
        for (k, m, dt, dk) in moves {
            if pressed(k, m) {
                changed |= nudge(notes, &st.selected, dt, dk);
            }
        }
        if changed {
            sort_notes(notes, &mut st.selected);
        }
    }
    changed
}

/// Moves selected notes by `dt` ticks and `dk` semitones, if all of them stay in range.
fn nudge(notes: &mut [Note], selected: &[bool], dt: i64, dk: i32) -> bool {
    let ok = notes
        .iter()
        .zip(selected)
        .filter(|(_, &s)| s)
        .all(|(n, _)| n.start + dt >= 0 && (0..=127).contains(&(i32::from(n.key) + dk)));
    if !ok {
        return false;
    }
    for (n, _) in notes.iter_mut().zip(selected).filter(|(_, &s)| s) {
        n.start += dt;
        n.key = (i32::from(n.key) + dk) as u8;
    }
    true
}

/// Adds `block` shifted by `offset` ticks and selects exactly the added notes.
fn paste(notes: &mut Vec<Note>, selected: &mut Vec<bool>, block: &[Note], offset: i64) {
    selected.iter_mut().for_each(|s| *s = false);
    for n in block {
        notes.push(Note {
            start: n.start + offset,
            ..*n
        });
        selected.push(true);
    }
    sort_notes(notes, selected);
}

#[allow(clippy::too_many_arguments)]
fn paint(
    ui: &Ui,
    theme: &GloomTheme,
    st: &PianoRollState,
    geo: &Geo,
    area: Rect,
    lane: Rect,
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
    let top_key = geo.key(grid.top());
    let bottom_key = geo.key(grid.bottom() - 0.5);
    let row_white = theme.bg_panel;
    let row_black = theme.bg_deep;
    for key in bottom_key..=top_key {
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
        let base = theme
            .accent_dim
            .lerp_to_gamma(theme.accent, 0.35 + 0.65 * n.velocity);
        let fill = if sel {
            theme.accent.lerp_to_gamma(Color32::WHITE, 0.35)
        } else {
            base
        };
        p.rect_filled(r, 2.0, fill);
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
                r.left_center() + vec2(3.0, 0.0),
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
    for key in bottom_key..=top_key {
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
        if key.rem_euclid(12) == 0 && geo.key_h >= 8.0 {
            kp.text(
                r.right_center() - vec2(3.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                key_name(key as u8),
                egui::FontId::proportional((geo.key_h - 2.0).min(10.5)),
                Color32::from_rgb(0x30, 0x30, 0x36),
            );
        }
    }

    // Velocity lane.
    let lp = ui.painter_at(lane);
    lp.rect_filled(lane, 0.0, theme.bg_panel);
    p.text(
        pos2(area.left() + 4.0, lane.center().y),
        egui::Align2::LEFT_CENTER,
        "Vel",
        egui::FontId::proportional(10.5),
        theme.text_dim,
    );
    let any_sel = st.selected.iter().any(|&s| s);
    for &i in &visible {
        let n = &notes[i];
        let x = geo.x(n.start);
        if x < lane.left() || x > lane.right() {
            continue;
        }
        let h = (lane.height() - 4.0) * n.velocity;
        let sel = st.selected.get(i).copied().unwrap_or(false);
        let c = if sel || !any_sel {
            theme.accent
        } else {
            theme.accent_dim
        };
        lp.line_segment(
            [
                pos2(x + 1.0, lane.bottom()),
                pos2(x + 1.0, lane.bottom() - h),
            ],
            Stroke::new(2.0, c),
        );
        lp.circle_filled(pos2(x + 1.0, lane.bottom() - h), 2.5, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(start: i64, length: i64, key: u8) -> Note {
        Note {
            start,
            length,
            key,
            velocity: 0.8,
        }
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
    fn nudge_refuses_to_leave_the_range() {
        let mut notes = vec![n(0, 10, 127), n(240, 10, 60)];
        assert!(!nudge(&mut notes, &[true, false], 0, 1));
        assert!(!nudge(&mut notes, &[true, false], -1, 0));
        assert!(nudge(&mut notes, &[false, true], 120, -12));
        assert_eq!((notes[1].start, notes[1].key), (360, 48));
    }

    #[test]
    fn paste_selects_the_copies() {
        let mut notes = vec![n(0, 240, 60), n(480, 240, 62)];
        let mut sel = vec![true, true];
        let block = notes.clone();
        paste(&mut notes, &mut sel, &block, 960);
        assert_eq!(notes.len(), 4);
        assert_eq!(sel, [false, false, true, true]);
        assert_eq!(notes[2].start, 960);
    }

    #[test]
    fn geometry_round_trips() {
        let geo = Geo {
            grid: Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 400.0)),
            px_per_tick: 64.0 / 960.0,
            key_h: 14.0,
            scroll_tick: 960.0,
            scroll_rows: 43.0,
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
        theme: GloomTheme,
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
                theme: GloomTheme::default(),
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
            let mut out = self.ctx.run_ui(input, |ui| {
                actions = piano_roll(
                    ui,
                    theme,
                    st,
                    PianoRollView {
                        notes,
                        ghosts: vec![],
                        pattern_len: 3840,
                        bar_ticks: 3840,
                        playhead: None,
                        channel_name: "Test",
                    },
                );
            });
            out.textures_delta.clear();
            actions
        }

        fn at(&self, tick: i64, key: i32) -> Pos2 {
            let g = self.st.last_geo.unwrap();
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
            [Note {
                start: 480,
                length: 240,
                key: 64,
                velocity: gt_core::project::DEFAULT_VELOCITY
            }]
        );

        // Drag it one beat later and two semitones up.
        let a = h.drag(h.at(500, 64), h.at(1460, 66), Primary);
        assert!(a.contains(&PianoRollAction::Changed));
        assert_eq!((h.notes[0].start, h.notes[0].key), (1440, 66));
        assert!(!h.st.is_dragging());

        // Drag the right edge to make it a quarter note.
        let g = h.st.last_geo.unwrap();
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

    /// 10,000 notes, all visible at once (worst case): build and tessellate one frame.
    /// Run with `cargo test --release -p gt-ui -- --ignored --nocapture` to see timings.
    #[test]
    #[ignore = "timing; run in release"]
    fn ten_thousand_notes_frame_time() {
        let mut notes: Vec<Note> = (0..10_000)
            .map(|i| n(i * 60, 50, 36 + (i % 48) as u8))
            .collect();
        let ghost: Vec<Note> = (0..2_000).map(|i| n(i * 300, 200, 60)).collect();
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
                        ghosts: vec![&ghost],
                        pattern_len: 640_000,
                        bar_ticks: 3840,
                        playhead: Some(1000),
                        channel_name: "Test",
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
}
