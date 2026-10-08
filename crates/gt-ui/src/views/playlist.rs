//! Playlist: the arrangement of pattern, audio and automation clips on tracks.
//!
//! Layout: toolbar on top; below it track headers on the left, a three-row ruler on top
//! (markers; tempo and time-signature changes; bars and the loop region) and the clip grid.
//!
//! Mouse:
//! - Draw tool: click empty space to place the current pattern, then drag to move it.
//!   Select tool (or Ctrl+drag on empty space) draws a selection box; Shift adds to it.
//!   Slice tool: click a clip to cut it at the pointer.
//! - Drag a clip to move the selection (across tracks too), drag either edge to resize,
//!   Shift+drag the body to slip the content inside the clip. Alt turns snap off.
//! - Right-click a clip (or right-drag over clips) deletes; double-click a pattern clip opens
//!   the pattern.
//! - Automation clips: drag the top strip to move; click the body to add a point, drag a point
//!   to move it, right-click a point to delete it.
//! - Ruler: click the bottom row to move the playhead, drag it to set the loop region. Click a
//!   marker to jump to it, drag to move, double-click to rename; right-click the rows for
//!   markers, tempo and time-signature changes.
//! - Drop a sound from the browser onto a track to add an audio clip.
//! - Wheel scrolls tracks (Shift: time), Ctrl+wheel zooms time, Alt+wheel track height.
//!
//! Keys (pointer over the playlist): Delete, Ctrl+A, Ctrl+D duplicate, M mute the selection,
//! P/E/C tools.

use egui::{
    pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Response, RichText, Sense, Stroke, Ui,
};
use gt_core::playlist::TRACK_COLORS;
use gt_core::{
    AutoPoint, Automation, ClipId, ClipKind, Curve, ParamId, PatternId, Peaks, Project,
    SampleSource, SigChange, StripKind, TempoMap, TempoPoint, Tick, TimeSig, TimeSigMap, TrackId,
    FX_SLOTS, MASTER, PPQ, STRIPS,
};

use crate::GloomTheme;

const HEADER_W: f32 = 168.0;
const ROW_H: f32 = 14.0;
const RULER_H: f32 = 3.0 * ROW_H;
const EDGE_PX: f32 = 6.0;
const LABEL_H: f32 = 14.0;
const POINT_R: f32 = 4.0;
const MIN_PX_PER_BEAT: f32 = 1.5;
const MAX_PX_PER_BEAT: f32 = 400.0;
const MIN_TRACK_H: f32 = 24.0;
const MAX_TRACK_H: f32 = 160.0;

/// Left-button tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistTool {
    /// Click places the current pattern.
    Draw,
    /// Drag selects.
    Select,
    /// Click cuts a clip.
    Slice,
}

/// Grid snap. Bars and beats follow the time-signature map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistSnap {
    /// Bar lines.
    Bar,
    /// Beats of the signature in effect.
    Beat,
    /// 1/8 notes.
    Eighth,
    /// 1/16 notes.
    Sixteenth,
    /// No snap.
    Off,
}

impl PlaylistSnap {
    /// Choices in menu order.
    pub const ALL: [Self; 5] = [
        Self::Bar,
        Self::Beat,
        Self::Eighth,
        Self::Sixteenth,
        Self::Off,
    ];

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Bar => "Bar",
            Self::Beat => "Beat",
            Self::Eighth => "1/8",
            Self::Sixteenth => "1/16",
            Self::Off => "Off",
        }
    }

    /// Grid line at or before `t` and the grid size there.
    fn floor_and_size(self, t: i64, sigs: &TimeSigMap) -> (i64, i64) {
        let bar = sigs.bar_of(t);
        let b0 = sigs.bar_start(bar);
        let g = match self {
            Self::Off => return (t, 1),
            Self::Bar => return (b0, sigs.bar_start(bar + 1) - b0),
            Self::Beat => sigs.sig_of_bar(bar).beat_ticks(),
            Self::Eighth => PPQ / 2,
            Self::Sixteenth => PPQ / 4,
        };
        (b0 + (t - b0).div_euclid(g) * g, g)
    }

    /// `t` rounded down to the grid.
    pub fn floor(self, t: i64, sigs: &TimeSigMap) -> i64 {
        self.floor_and_size(t, sigs).0
    }

    /// `t` rounded to the nearest grid line.
    pub fn round(self, t: i64, sigs: &TimeSigMap) -> i64 {
        let (f, g) = self.floor_and_size(t, sigs);
        if t - f >= g - (t - f) {
            f + g
        } else {
            f
        }
    }
}

/// Peaks and timing of loaded audio, provided by the app.
pub trait AudioLookup {
    /// Waveform peaks and sample rate of a loaded sound.
    fn peaks(&self, src: &SampleSource) -> Option<(&Peaks, u32)>;
}

#[derive(Debug, Clone)]
enum Drag {
    Move {
        grab_tick: i64,
        grab_row: i32,
        anchor: ClipId,
        originals: Vec<(ClipId, i64, usize)>,
    },
    Resize {
        left: bool,
        anchor: ClipId,
        originals: Vec<(ClipId, i64, i64, i64)>,
    },
    Slip {
        grab_tick: i64,
        originals: Vec<(ClipId, i64)>,
    },
    Box {
        from: Pos2,
        additive: bool,
    },
    Erase,
    Point {
        clip: ClipId,
        index: usize,
    },
    /// Ctrl+drag on an automation segment bends it (a Bézier tension).
    Tension {
        clip: ClipId,
        index: usize,
        grab_y: f32,
        orig: f32,
    },
    Loop {
        from: i64,
    },
    Marker {
        index: usize,
        grab: i64,
        orig: i64,
    },
    Tempo {
        index: usize,
        grab: i64,
        orig: i64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Popup {
    Marker(usize),
    Tempo(usize),
    /// The curve of the automation segment starting at point `index`.
    Segment {
        clip: ClipId,
        index: usize,
    },
    Sig(usize),
}

/// View and editing state of the playlist that is not part of the document.
#[derive(Debug, Clone)]
pub struct PlaylistState {
    /// Horizontal zoom: points per quarter note.
    pub px_per_beat: f32,
    /// Track height in points.
    pub track_h: f32,
    /// Tick at the left edge of the grid.
    pub scroll_tick: f64,
    /// Points scrolled from the first track.
    pub scroll_y: f32,
    /// Grid snap.
    pub snap: PlaylistSnap,
    /// Left-button tool.
    pub tool: PlaylistTool,
    /// Selected clips.
    pub selected: Vec<ClipId>,
    /// Track that new automation clips and dropped audio prefer.
    pub selected_track: Option<TrackId>,
    drag: Option<Drag>,
    /// Loop region being dragged (preview).
    loop_preview: Option<(i64, i64)>,
    popup: Option<(Popup, Pos2, u32)>,
    menu_tick: i64,
    #[cfg(test)]
    last_geo: Option<Geo>,
}

impl Default for PlaylistState {
    fn default() -> Self {
        Self {
            px_per_beat: 20.0,
            track_h: 44.0,
            scroll_tick: 0.0,
            scroll_y: 0.0,
            snap: PlaylistSnap::Bar,
            tool: PlaylistTool::Draw,
            selected: Vec::new(),
            selected_track: None,
            drag: None,
            loop_preview: None,
            popup: None,
            menu_tick: 0,
            #[cfg(test)]
            last_geo: None,
        }
    }
}

impl PlaylistState {
    /// True while a mouse gesture is changing the document (the app commits undo steps after
    /// it).
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Abandons a gesture in progress (before undo replaces the document).
    pub fn cancel(&mut self) {
        self.drag = None;
        self.loop_preview = None;
        self.popup = None;
    }
}

/// What the playlist shows besides the document.
pub struct PlaylistView<'a> {
    /// Playhead while playing (or the held position), in ticks.
    pub playhead: Option<i64>,
    /// Loop region and whether it is on.
    pub loop_region: (i64, i64, bool),
    /// Loaded audio, for waveforms and clip lengths.
    pub audio: &'a dyn AudioLookup,
}

/// Things the app must act on.
#[derive(Debug, Clone, PartialEq)]
pub enum PlaylistAction {
    /// Clips, tracks or markers changed.
    Changed,
    /// The tempo or time-signature map changed (the document too).
    TimingChanged,
    /// Move the playhead.
    Locate(i64),
    /// Set and enable the loop region.
    SetLoop {
        /// First tick.
        start: i64,
        /// First tick after.
        end: i64,
    },
    /// A sound was placed that is not loaded yet.
    Load(SampleSource),
    /// Open a pattern for editing.
    EditPattern(PatternId),
}

#[derive(Debug, Clone, Copy)]
struct Geo {
    grid: Rect,
    px_per_tick: f32,
    scroll_tick: f64,
    track_h: f32,
    scroll_y: f32,
}

impl Geo {
    fn x(&self, tick: i64) -> f32 {
        self.xf(tick as f64)
    }
    fn xf(&self, tick: f64) -> f32 {
        self.grid.left() + ((tick - self.scroll_tick) as f32) * self.px_per_tick
    }
    fn tick(&self, x: f32) -> i64 {
        self.tick_f(x).floor() as i64
    }
    fn tick_f(&self, x: f32) -> f64 {
        self.scroll_tick + f64::from((x - self.grid.left()) / self.px_per_tick)
    }
    fn row_top(&self, row: usize) -> f32 {
        self.grid.top() + row as f32 * self.track_h - self.scroll_y
    }
    fn row(&self, y: f32) -> i32 {
        ((y - self.grid.top() + self.scroll_y) / self.track_h).floor() as i32
    }
    fn clip_rect(&self, row: usize, start: i64, end: i64) -> Rect {
        let top = self.row_top(row);
        Rect::from_min_max(
            pos2(self.x(start), top + 1.0),
            pos2(
                self.x(end).max(self.x(start) + 3.0),
                top + self.track_h - 1.0,
            ),
        )
    }
}

fn color(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

/// Draws the playlist and edits `project` in place.
pub fn playlist(
    ui: &mut Ui,
    theme: &GloomTheme,
    project: &mut Project,
    st: &mut PlaylistState,
    view: PlaylistView<'_>,
) -> Vec<PlaylistAction> {
    let mut actions = Vec::new();
    // Selection of clips that no longer exist (undo) is dropped.
    st.selected
        .retain(|id| project.playlist.clips.iter().any(|c| c.id == *id));
    toolbar(ui, theme, project, st, &view, &mut actions);
    ui.add_space(4.0);

    let area = ui.available_rect_before_wrap();
    let area = Rect::from_min_size(area.min, vec2(area.width(), area.height().max(160.0)));
    ui.allocate_rect(area, Sense::hover());
    let grid = Rect::from_min_max(pos2(area.left() + HEADER_W, area.top() + RULER_H), area.max);
    let headers = Rect::from_min_max(
        pos2(area.left(), grid.top()),
        pos2(grid.left(), area.bottom()),
    );
    let ruler = Rect::from_min_max(
        pos2(grid.left(), area.top()),
        pos2(area.right(), grid.top()),
    );

    let hovered = ui.rect_contains_pointer(area);
    if hovered {
        handle_wheel(ui, st, grid);
    }
    let tracks = project.playlist.tracks.len();
    let max_y = (tracks as f32 * st.track_h - grid.height() + st.track_h).max(0.0);
    st.scroll_y = st.scroll_y.clamp(0.0, max_y);
    st.scroll_tick = st.scroll_tick.max(-(PPQ as f64));
    let geo = Geo {
        grid,
        px_per_tick: st.px_per_beat / PPQ as f32,
        scroll_tick: st.scroll_tick,
        track_h: st.track_h,
        scroll_y: st.scroll_y,
    };

    #[cfg(test)]
    {
        st.last_geo = Some(geo);
    }
    let grid_resp = ui.interact(grid, ui.id().with("pl_grid"), Sense::click_and_drag());
    let ruler_resp = ui.interact(ruler, ui.id().with("pl_ruler"), Sense::click_and_drag());

    let mut changed = false;
    changed |= grid_input(ui, st, &geo, &grid_resp, project, &mut actions);
    if let Some(src) = drop_audio(ui, &grid_resp, st, &geo, project, view.audio) {
        changed = true;
        if view.audio.peaks(&src).is_none() {
            actions.push(PlaylistAction::Load(src));
        }
    }
    let timing = ruler_input(ui, st, &geo, ruler, &ruler_resp, project, &mut actions);
    changed |= timing;
    if hovered && !ui.ctx().text_edit_focused() {
        changed |= key_commands(ui, st, project);
    }

    paint(ui, theme, st, &geo, area, ruler, project, &view);
    changed |= track_headers(ui, theme, st, headers, &geo, project);
    let (popup_timing, popup_changed) = show_popup(ui, theme, st, project);
    let timing = timing | popup_timing;
    changed |= popup_changed;
    if changed {
        actions.push(PlaylistAction::Changed);
    }
    if timing {
        actions.push(PlaylistAction::TimingChanged);
    }
    actions
}

fn toolbar(
    ui: &mut Ui,
    theme: &GloomTheme,
    project: &mut Project,
    st: &mut PlaylistState,
    view: &PlaylistView<'_>,
    actions: &mut Vec<PlaylistAction>,
) {
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    ui.horizontal(|ui| {
        for (tool, label, tip) in [
            (
                PlaylistTool::Draw,
                "Draw",
                "Click to place the current pattern (P)",
            ),
            (PlaylistTool::Select, "Select", "Drag to select clips (E)"),
            (PlaylistTool::Slice, "Slice", "Click a clip to cut it (C)"),
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
        egui::ComboBox::from_id_salt("pl_snap")
            .width(52.0)
            .selected_text(st.snap.label())
            .show_ui(ui, |ui| {
                for s in PlaylistSnap::ALL {
                    ui.selectable_value(&mut st.snap, s, s.label());
                }
            });
        ui.separator();
        ui.label(dim("Pattern"));
        let current = project.current_pattern().name.clone();
        egui::ComboBox::from_id_salt("pl_pattern")
            .width(110.0)
            .selected_text(current)
            .show_ui(ui, |ui| {
                for i in 0..project.patterns.len() {
                    let (id, name) = (project.patterns[i].id, project.patterns[i].name.clone());
                    if ui
                        .selectable_label(project.current_pattern == id, name)
                        .clicked()
                    {
                        project.select_pattern(id);
                    }
                }
            })
            .response
            .on_hover_text("The pattern the Draw tool places");
        ui.separator();
        if ui.button("+ Track").clicked() {
            project.playlist.add_track();
            actions.push(PlaylistAction::Changed);
        }
        let start = st.snap.floor(
            view.playhead.unwrap_or(st.scroll_tick as i64).max(0),
            &project.signatures,
        );
        ui.menu_button("+ Automation", |ui| {
            if let Some(target) = automation_menu(ui, project) {
                add_automation(project, st, target, start);
                actions.push(PlaylistAction::Changed);
                ui.close();
            }
        })
        .response
        .on_hover_text("Add an automation clip at the playhead");
        ui.separator();
        let clips = project.playlist.clips.len();
        ui.label(dim(&format!(
            "{} tracks, {} clips, {} selected",
            project.playlist.tracks.len(),
            clips,
            st.selected.len()
        )));
    });
}

/// Every parameter, by owner: channels (with their synth knobs) and the mixer strips worth
/// automating (the master, named inserts or inserts with effects, and the sends).
fn automation_menu(ui: &mut Ui, project: &Project) -> Option<ParamId> {
    let mut picked = None;
    let mut list = |ui: &mut Ui, ids: Vec<ParamId>| {
        for id in ids {
            if ui.button(id.label()).clicked() {
                picked = Some(id);
            }
        }
    };
    ui.menu_button("Channels", |ui| {
        for ch in &project.channels {
            ui.menu_button(&ch.name, |ui| {
                let all = ParamId::of_channel(ch);
                let (synth, rest): (Vec<ParamId>, Vec<ParamId>) = all
                    .into_iter()
                    .partition(|id| !matches!(id, ParamId::Channel { .. }));
                list(ui, rest);
                if !synth.is_empty() {
                    ui.menu_button("Gloom Synth", |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(420.0)
                            .show(ui, |ui| list(ui, synth));
                    });
                }
            });
        }
    });
    ui.menu_button("Mixer", |ui| {
        let mixer = &project.mixer;
        for i in 0..STRIPS {
            let s = &mixer.strips[i];
            let kind = StripKind::of(i);
            let used = i == MASTER
                || matches!(kind, StripKind::Send(_))
                || s.name != kind.default_name()
                || s.slots.iter().any(Option::is_some);
            if !used {
                continue;
            }
            ui.menu_button(format!("{} {}", kind.short(), s.name), |ui| {
                let (fx, strip): (Vec<ParamId>, Vec<ParamId>) = ParamId::of_strip(project, i)
                    .into_iter()
                    .partition(|id| matches!(id, ParamId::Effect { .. }));
                list(ui, strip);
                for k in 0..FX_SLOTS {
                    let Some(slot) = &s.slots[k] else {
                        continue;
                    };
                    ui.menu_button(format!("{} {}", k + 1, slot.kind.name()), |ui| {
                        let mine = fx
                            .iter()
                            .copied()
                            .filter(|id| matches!(id, ParamId::Effect { slot, .. } if *slot == k))
                            .collect();
                        egui::ScrollArea::vertical()
                            .max_height(420.0)
                            .show(ui, |ui| list(ui, mine));
                    });
                }
            });
        }
    });
    picked
}

/// Adds a flat four-bar automation clip at `start` on the selected track if it is free there,
/// else on the first free track (a new one if none is).
/// Returns the new clip.
pub fn add_automation(
    project: &mut Project,
    st: &mut PlaylistState,
    target: ParamId,
    start: i64,
) -> ClipId {
    let len = project
        .signatures
        .bar_start(project.signatures.bar_of(start) + 4)
        - start;
    let value = target.normalized(project);
    let name = target.name(project);
    let pl = &mut project.playlist;
    let free = |pl: &gt_core::Playlist, t: TrackId| {
        !pl.clips
            .iter()
            .any(|c| c.track == t && c.start < start + len && c.end() > start)
    };
    let track = st
        .selected_track
        .filter(|&t| pl.track(t).is_some() && free(pl, t))
        .or_else(|| pl.tracks.iter().map(|t| t.id).find(|&t| free(pl, t)))
        .unwrap_or_else(|| pl.add_track());
    if let Some(t) = pl.tracks.iter_mut().find(|t| t.id == track) {
        if t.name.starts_with("Track ") && !pl.clips.iter().any(|c| c.track == track) {
            t.name = name;
        }
    }
    let id = pl.add_clip(
        track,
        start,
        len,
        ClipKind::Automation(Automation {
            target,
            points: vec![AutoPoint::new(0, value), AutoPoint::new(len, value)],
        }),
    );
    st.selected = vec![id];
    id
}

fn handle_wheel(ui: &Ui, st: &mut PlaylistState, grid: Rect) {
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
        let ppt = st.px_per_beat / PPQ as f32;
        let at = st.scroll_tick + f64::from((p.x - grid.left()) / ppt);
        st.px_per_beat = (st.px_per_beat * zoom).clamp(MIN_PX_PER_BEAT, MAX_PX_PER_BEAT);
        let ppt = st.px_per_beat / PPQ as f32;
        st.scroll_tick = at - f64::from((p.x - grid.left()) / ppt);
    } else if alt && scroll.y != 0.0 {
        let row = (p.y - grid.top() + st.scroll_y) / st.track_h;
        st.track_h = (st.track_h * (1.0 + scroll.y * 0.004)).clamp(MIN_TRACK_H, MAX_TRACK_H);
        st.scroll_y = row * st.track_h - (p.y - grid.top());
    } else {
        st.scroll_y -= scroll.y;
        st.scroll_tick -= f64::from(scroll.x / (st.px_per_beat / PPQ as f32));
    }
}

/// Where the pointer is on a clip.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Zone {
    Left,
    Right,
    Body,
    /// Inside an automation clip below its label strip.
    Curve,
    /// On an automation point.
    Point(usize),
}

fn track_row(project: &Project, id: TrackId) -> Option<usize> {
    project.playlist.track_index(id)
}

fn hit_clip(geo: &Geo, project: &Project, p: Pos2) -> Option<(ClipId, Zone)> {
    let pl = &project.playlist;
    pl.clips.iter().rev().find_map(|c| {
        let row = track_row(project, c.track)?;
        let r = geo.clip_rect(row, c.start, c.end());
        if !r.contains(p) {
            return None;
        }
        if let ClipKind::Automation(a) = &c.kind {
            if let Some(i) = point_positions(geo, r, c.start, c.offset, c.length, a)
                .into_iter()
                .find(|&(_, q)| q.distance(p) <= POINT_R + 2.0)
                .map(|(i, _)| i)
            {
                return Some((c.id, Zone::Point(i)));
            }
        }
        let grip = EDGE_PX.min(r.width() * 0.25);
        let zone = if p.x <= r.left() + grip {
            Zone::Left
        } else if p.x >= r.right() - grip {
            Zone::Right
        } else if matches!(c.kind, ClipKind::Automation(_)) && p.y > r.top() + LABEL_H {
            Zone::Curve
        } else {
            Zone::Body
        };
        Some((c.id, zone))
    })
}

/// Screen positions of an automation clip's points inside the clip window.
fn point_positions(
    geo: &Geo,
    r: Rect,
    start: i64,
    offset: i64,
    length: i64,
    a: &Automation,
) -> Vec<(usize, Pos2)> {
    let body = curve_rect(r);
    a.points
        .iter()
        .enumerate()
        .filter(|(_, p)| p.at >= offset && p.at <= offset + length)
        .map(|(i, p)| {
            (
                i,
                pos2(
                    geo.x(start + p.at - offset),
                    body.bottom() - p.value * body.height(),
                ),
            )
        })
        .collect()
}

fn curve_rect(r: Rect) -> Rect {
    Rect::from_min_max(
        pos2(r.left(), r.top() + LABEL_H),
        pos2(r.right(), r.bottom() - 2.0),
    )
}

/// The automation segment of clip `c` under screen x: the index of its first point, if x is
/// between two points.
fn segment_at(geo: &Geo, c: &gt_core::Clip, x: f32) -> Option<usize> {
    let ClipKind::Automation(a) = &c.kind else {
        return None;
    };
    let src = geo.tick_f(x) - c.start as f64 + c.offset as f64;
    let k = a.points.partition_point(|p| (p.at as f64) <= src);
    (k > 0 && k < a.points.len()).then(|| k - 1)
}

fn snap_of(ui: &Ui, st: &PlaylistState) -> PlaylistSnap {
    if ui.input(|i| i.modifiers.alt) {
        PlaylistSnap::Off
    } else {
        st.snap
    }
}

/// Where the gesture that just started began: egui reports a drag only once the pointer has
/// moved past its threshold, so hit-testing must use the press position, not the current one.
fn gesture_origin(ui: &Ui, resp: &Response) -> Option<Pos2> {
    ui.input(|i| i.pointer.press_origin())
        .or_else(|| resp.interact_pointer_pos())
}

fn grid_input(
    ui: &Ui,
    st: &mut PlaylistState,
    geo: &Geo,
    resp: &Response,
    project: &mut Project,
    actions: &mut Vec<PlaylistAction>,
) -> bool {
    let mut changed = false;
    let snap = snap_of(ui, st);
    let (shift, ctrl) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
    let pointer = resp.interact_pointer_pos().or_else(|| resp.hover_pos());

    if resp.double_clicked() {
        if let Some(p) = pointer {
            if let Some((id, Zone::Body | Zone::Left | Zone::Right)) = hit_clip(geo, project, p) {
                if let Some(ClipKind::Pattern(pid)) = project.playlist.clip(id).map(|c| &c.kind) {
                    actions.push(PlaylistAction::EditPattern(*pid));
                }
            }
        }
    }

    if resp.drag_started_by(egui::PointerButton::Primary)
        || resp.clicked_by(egui::PointerButton::Primary) && st.drag.is_none()
    {
        if let Some(p) = gesture_origin(ui, resp) {
            let tick = geo.tick(p.x);
            let row = geo.row(p.y);
            if let Some(t) = usize::try_from(row)
                .ok()
                .and_then(|r| project.playlist.tracks.get(r))
            {
                st.selected_track = Some(t.id);
            }
            let hit = hit_clip(geo, project, p);
            match (st.tool, hit) {
                (PlaylistTool::Slice, Some((id, _))) => {
                    let at = snap.round(tick, &project.signatures);
                    if let Some(new) = project.playlist.split_clip(id, at) {
                        st.selected = vec![new];
                        changed = true;
                    }
                }
                (_, Some((id, Zone::Point(i)))) => {
                    st.drag = Some(Drag::Point { clip: id, index: i });
                }
                (_, Some((id, Zone::Curve)))
                    if ctrl
                        && project
                            .playlist
                            .clip(id)
                            .and_then(|c| segment_at(geo, c, p.x))
                            .is_some() =>
                {
                    let c = project.playlist.clip(id).expect("checked");
                    let index = segment_at(geo, c, p.x).expect("checked");
                    let orig = match &c.kind {
                        ClipKind::Automation(a) => match a.points[index].curve {
                            Curve::Bezier(t) => t,
                            _ => 0.0,
                        },
                        _ => 0.0,
                    };
                    st.drag = Some(Drag::Tension {
                        clip: id,
                        index,
                        grab_y: p.y,
                        orig,
                    });
                }
                (_, Some((id, Zone::Curve))) => {
                    // Add a point under the pointer and drag it.
                    if let Some(c) = project.playlist.clip_mut(id) {
                        let row_r = Rect::from_min_max(
                            pos2(0.0, geo.row_top(geo.row(p.y).max(0) as usize) + 1.0),
                            pos2(
                                1.0,
                                geo.row_top(geo.row(p.y).max(0) as usize) + geo.track_h - 1.0,
                            ),
                        );
                        let body = curve_rect(row_r);
                        let at = snap.round(tick, &project.signatures) - c.start + c.offset;
                        let at = at.clamp(c.offset, c.offset + c.length);
                        let value = ((body.bottom() - p.y) / body.height()).clamp(0.0, 1.0);
                        if let ClipKind::Automation(a) = &mut c.kind {
                            let i = a.insert_point(AutoPoint::new(at, value));
                            st.drag = Some(Drag::Point { clip: id, index: i });
                            changed = true;
                        }
                    }
                }
                (_, Some((id, zone))) => {
                    if !st.selected.contains(&id) {
                        if !shift {
                            st.selected.clear();
                        }
                        st.selected.push(id);
                    }
                    st.drag = Some(start_clip_drag(project, st, id, zone, tick, row, shift));
                }
                (PlaylistTool::Select, None) => {
                    st.drag = Some(Drag::Box {
                        from: p,
                        additive: shift,
                    });
                }
                (PlaylistTool::Draw, None) if ctrl => {
                    st.drag = Some(Drag::Box {
                        from: p,
                        additive: shift,
                    });
                }
                (PlaylistTool::Draw, None) => {
                    if let Some(track) = usize::try_from(row)
                        .ok()
                        .and_then(|r| project.playlist.tracks.get(r))
                        .map(|t| t.id)
                    {
                        let pid = project.current_pattern;
                        let len = project.current_pattern().length_ticks();
                        let start = snap.floor(tick, &project.signatures);
                        let id =
                            project
                                .playlist
                                .add_clip(track, start, len, ClipKind::Pattern(pid));
                        st.selected = vec![id];
                        st.drag = Some(start_clip_drag(
                            project,
                            st,
                            id,
                            Zone::Body,
                            tick,
                            row,
                            false,
                        ));
                        changed = true;
                    } else {
                        st.selected.clear();
                    }
                }
                (PlaylistTool::Slice, None) => st.selected.clear(),
            }
            if resp.clicked_by(egui::PointerButton::Primary)
                && !matches!(st.drag, Some(Drag::Point { .. }))
            {
                // A click without movement ends the gesture at once.
                st.drag = None;
            }
        }
    }

    if resp.dragged_by(egui::PointerButton::Primary) {
        if let Some(p) = pointer {
            changed |= drag_update(st, geo, project, p, snap);
        }
    }

    // Right button: delete clips, or a point of an automation clip.
    if resp.drag_started_by(egui::PointerButton::Secondary)
        || resp.clicked_by(egui::PointerButton::Secondary)
    {
        st.drag = Some(Drag::Erase);
        if let Some(p) = gesture_origin(ui, resp) {
            let hit = hit_clip(geo, project, p);
            if let Some((id, Zone::Curve)) = hit {
                // Right-click on a curve: pick the segment's shape instead of erasing.
                if let Some(index) = project
                    .playlist
                    .clip(id)
                    .and_then(|c| segment_at(geo, c, p.x))
                {
                    st.drag = None;
                    st.popup = Some((Popup::Segment { clip: id, index }, p, 0));
                }
            }
            if let Some((id, Zone::Point(i))) = hit {
                if let Some(ClipKind::Automation(a)) =
                    project.playlist.clip_mut(id).map(|c| &mut c.kind)
                {
                    if a.points.len() > 1 {
                        a.points.remove(i);
                        changed = true;
                    }
                }
                st.drag = None;
            }
        }
    }
    if matches!(st.drag, Some(Drag::Erase)) {
        if let Some(p) = pointer {
            if let Some((id, zone)) = hit_clip(geo, project, p) {
                if !matches!(zone, Zone::Point(_)) {
                    project.playlist.remove_clips(&[id]);
                    st.selected.retain(|x| *x != id);
                    changed = true;
                }
            }
        }
    }

    let released = ui.input(|i| i.pointer.any_released()) || !ui.input(|i| i.pointer.any_down());
    if released {
        if let Some(Drag::Box { from, additive }) = st.drag {
            if let Some(p) = pointer {
                let r = Rect::from_two_pos(from, p);
                if !additive {
                    st.selected.clear();
                }
                for c in &project.playlist.clips {
                    if let Some(row) = track_row(project, c.track) {
                        if geo.clip_rect(row, c.start, c.end()).intersects(r)
                            && !st.selected.contains(&c.id)
                        {
                            st.selected.push(c.id);
                        }
                    }
                }
            }
        }
        if !matches!(
            st.drag,
            Some(Drag::Loop { .. } | Drag::Marker { .. } | Drag::Tempo { .. })
        ) {
            st.drag = None;
        }
    }
    changed
}

fn start_clip_drag(
    project: &Project,
    st: &PlaylistState,
    anchor: ClipId,
    zone: Zone,
    tick: i64,
    row: i32,
    shift: bool,
) -> Drag {
    let sel: Vec<&gt_core::Clip> = project
        .playlist
        .clips
        .iter()
        .filter(|c| st.selected.contains(&c.id))
        .collect();
    match zone {
        Zone::Left | Zone::Right => Drag::Resize {
            left: zone == Zone::Left,
            anchor,
            originals: sel
                .iter()
                .map(|c| (c.id, c.start, c.length, c.offset))
                .collect(),
        },
        _ if shift => Drag::Slip {
            grab_tick: tick,
            originals: sel.iter().map(|c| (c.id, c.offset)).collect(),
        },
        _ => Drag::Move {
            grab_tick: tick,
            grab_row: row,
            anchor,
            originals: sel
                .iter()
                .filter_map(|c| Some((c.id, c.start, track_row(project, c.track)?)))
                .collect(),
        },
    }
}

fn drag_update(
    st: &mut PlaylistState,
    geo: &Geo,
    project: &mut Project,
    p: Pos2,
    snap: PlaylistSnap,
) -> bool {
    let tick = geo.tick(p.x);
    let sigs = project.signatures.clone();
    match st.drag.clone() {
        Some(Drag::Move {
            grab_tick,
            grab_row,
            anchor,
            originals,
        }) => {
            let Some(&(_, a_start, _)) = originals.iter().find(|o| o.0 == anchor) else {
                return false;
            };
            // Snap the anchor's new start; everything moves by the same amount.
            let want = a_start + (tick - grab_tick);
            let delta = snap.round(want, &sigs) - a_start;
            let tracks = project.playlist.tracks.len() as i32;
            let min_row = originals.iter().map(|o| o.2 as i32).min().unwrap_or(0);
            let max_row = originals.iter().map(|o| o.2 as i32).max().unwrap_or(0);
            let drow = (geo.row(p.y) - grab_row).clamp(-min_row, tracks - 1 - max_row);
            let mut changed = false;
            for (id, start, row) in originals {
                let track = project.playlist.tracks[(row as i32 + drow) as usize].id;
                if let Some(c) = project.playlist.clip_mut(id) {
                    let ns = start + delta;
                    if c.start != ns || c.track != track {
                        c.start = ns;
                        c.track = track;
                        changed = true;
                    }
                }
            }
            changed
        }
        Some(Drag::Resize {
            left,
            anchor,
            originals,
        }) => {
            let Some(&(_, a_start, a_len, _)) = originals.iter().find(|o| o.0 == anchor) else {
                return false;
            };
            let edge = snap.round(tick, &sigs);
            let mut changed = false;
            if left {
                let delta = edge - a_start;
                for (id, start, len, offset) in originals {
                    // The content stays put: the offset moves with the edge, never below 0, and
                    // the clip keeps at least one tick.
                    let d = delta.max(-offset).min(len - 1);
                    if let Some(c) = project.playlist.clip_mut(id) {
                        c.start = start + d;
                        c.length = len - d;
                        c.offset = offset + d;
                        changed = true;
                    }
                }
            } else {
                let delta = edge - (a_start + a_len);
                for (id, _, len, _) in originals {
                    if let Some(c) = project.playlist.clip_mut(id) {
                        c.length = (len + delta).max(1);
                        changed = true;
                    }
                }
            }
            changed
        }
        Some(Drag::Slip {
            grab_tick,
            originals,
        }) => {
            let raw = tick - grab_tick;
            // Snap the content movement to whole grid steps from where it started.
            let delta = if snap == PlaylistSnap::Off {
                raw
            } else {
                snap.round(grab_tick + raw, &sigs) - snap.round(grab_tick, &sigs)
            };
            for (id, offset) in originals {
                if let Some(c) = project.playlist.clip_mut(id) {
                    c.offset = (offset - delta).max(0);
                }
            }
            true
        }
        Some(Drag::Tension {
            clip,
            index,
            grab_y,
            orig,
        }) => {
            // 80 px of travel bends a straight segment fully; up is "rise fast".
            let t = (orig + (grab_y - p.y) / 80.0).clamp(-1.0, 1.0);
            let t = if t.abs() < 0.03 { 0.0 } else { t };
            if let Some(ClipKind::Automation(a)) =
                project.playlist.clip_mut(clip).map(|c| &mut c.kind)
            {
                if let Some(pt) = a.points.get_mut(index) {
                    if pt.curve != Curve::Bezier(t) {
                        pt.curve = Curve::Bezier(t);
                        return true;
                    }
                }
            }
            false
        }
        Some(Drag::Point { clip, index }) => {
            let Some(track) = project.playlist.clip(clip).map(|c| c.track) else {
                return false;
            };
            let Some(row) = project.playlist.track_index(track) else {
                return false;
            };
            let Some(c) = project.playlist.clip_mut(clip) else {
                return false;
            };
            let r = geo.clip_rect(row, c.start, c.end());
            let body = curve_rect(r);
            let at = snap.round(tick, &sigs) - c.start + c.offset;
            let (lo_lim, hi_lim) = (c.offset, c.offset + c.length);
            if let ClipKind::Automation(a) = &mut c.kind {
                // Points keep their order: a point stops at its neighbours.
                let lo = if index > 0 {
                    a.points[index - 1].at
                } else {
                    lo_lim
                };
                let hi = a.points.get(index + 1).map_or(hi_lim, |q| q.at);
                if let Some(pt) = a.points.get_mut(index) {
                    pt.at = at.clamp(lo.min(hi), hi.max(lo));
                    pt.value = ((body.bottom() - p.y) / body.height()).clamp(0.0, 1.0);
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// Adds an audio clip where a sound from the browser is dropped. Returns the sound.
fn drop_audio(
    ui: &Ui,
    resp: &Response,
    st: &mut PlaylistState,
    geo: &Geo,
    project: &mut Project,
    audio: &dyn AudioLookup,
) -> Option<SampleSource> {
    let src = resp.dnd_release_payload::<SampleSource>()?;
    let p = ui.input(|i| i.pointer.interact_pos())?;
    let row = usize::try_from(geo.row(p.y)).ok()?;
    let track = project.playlist.tracks.get(row)?.id;
    let start = snap_of(ui, st)
        .floor(geo.tick(p.x), &project.signatures)
        .max(0);
    // Length: the sound's duration at the tempo where it starts, else one bar.
    let tempo = &project.tempo;
    let length = audio
        .peaks(&src)
        .map(|(pk, rate)| {
            let secs = pk.frames() as f64 / f64::from(rate.max(1));
            let end = tempo.seconds_to_tick(tempo.tick_to_seconds(start as f64) + secs);
            (end.ceil() as i64 - start).max(1)
        })
        .unwrap_or(4 * PPQ);
    let id = project.playlist.add_clip(
        track,
        start,
        length,
        ClipKind::Audio {
            source: (*src).clone(),
            gain: 1.0,
        },
    );
    st.selected = vec![id];
    st.selected_track = Some(track);
    Some((*src).clone())
}

fn key_commands(ui: &Ui, st: &mut PlaylistState, project: &mut Project) -> bool {
    use egui::{Key, Modifiers};
    let pressed = |k: Key, m: Modifiers| ui.input_mut(|i| i.consume_key(m, k));
    let mut changed = false;
    if !st.selected.is_empty()
        && (pressed(Key::Delete, Modifiers::NONE) || pressed(Key::Backspace, Modifiers::NONE))
    {
        project.playlist.remove_clips(&st.selected);
        st.selected.clear();
        changed = true;
    }
    if pressed(Key::A, Modifiers::COMMAND) {
        st.selected = project.playlist.clips.iter().map(|c| c.id).collect();
    }
    if pressed(Key::D, Modifiers::COMMAND) && !st.selected.is_empty() {
        let grid = match st.snap {
            PlaylistSnap::Off => 0,
            s => {
                let first = st
                    .selected
                    .iter()
                    .filter_map(|&id| project.playlist.clip(id))
                    .map(|c| c.start)
                    .min()
                    .unwrap_or(0);
                s.floor_and_size(first, &project.signatures).1
            }
        };
        st.selected = project.playlist.duplicate_clips(&st.selected, grid);
        changed = true;
    }
    if pressed(Key::M, Modifiers::NONE) && !st.selected.is_empty() {
        let mute = project
            .playlist
            .clips
            .iter()
            .filter(|c| st.selected.contains(&c.id))
            .any(|c| !c.muted);
        for c in &mut project.playlist.clips {
            if st.selected.contains(&c.id) {
                c.muted = mute;
            }
        }
        changed = true;
    }
    if pressed(Key::P, Modifiers::NONE) {
        st.tool = PlaylistTool::Draw;
    }
    if pressed(Key::E, Modifiers::NONE) {
        st.tool = PlaylistTool::Select;
    }
    if pressed(Key::C, Modifiers::NONE) {
        st.tool = PlaylistTool::Slice;
    }
    changed
}

/// Flags in the ruler's top two rows: markers, tempo and signature changes.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Flag {
    Marker(usize),
    Tempo(usize),
    Sig(usize),
}

fn flag_rects(geo: &Geo, ruler: Rect, project: &Project, ui: &Ui) -> Vec<(Flag, Rect, String)> {
    let font = FontId::proportional(10.5);
    let width = |s: &str| {
        ui.fonts_mut(|f| {
            f.layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
    };
    let mut out = Vec::new();
    for (i, m) in project.playlist.markers.iter().enumerate() {
        let x = geo.x(m.at);
        let r = Rect::from_min_size(pos2(x, ruler.top()), vec2(width(&m.name) + 8.0, ROW_H));
        out.push((Flag::Marker(i), r, m.name.clone()));
    }
    let row2 = ruler.top() + ROW_H;
    for (i, p) in project.tempo.points().enumerate() {
        let text = format!("{:.1} BPM", p.bpm);
        let r = Rect::from_min_size(pos2(geo.x(p.at.0), row2), vec2(width(&text) + 8.0, ROW_H));
        out.push((Flag::Tempo(i), r, text));
    }
    for (i, c) in project.signatures.changes().enumerate() {
        let text = c.sig.to_string();
        let mut x = geo.x(project.signatures.bar_start(c.bar));
        // Shares the tempo row: a signature that would cover a tempo flag starting at or just
        // before its bar line moves right past it.
        for (f, r, _) in &out {
            if matches!(f, Flag::Tempo(_)) && r.left() <= x + 1.0 && r.right() > x {
                x = r.right() + 2.0;
            }
        }
        let r = Rect::from_min_size(pos2(x, row2), vec2(width(&text) + 8.0, ROW_H));
        out.push((Flag::Sig(i), r, text));
    }
    out
}

/// Ruler input. Returns true if the tempo or signature map changed.
fn ruler_input(
    ui: &Ui,
    st: &mut PlaylistState,
    geo: &Geo,
    ruler: Rect,
    resp: &Response,
    project: &mut Project,
    actions: &mut Vec<PlaylistAction>,
) -> bool {
    let mut timing = false;
    let snap = snap_of(ui, st);
    let pointer = resp.interact_pointer_pos().or_else(|| resp.hover_pos());
    let row_of = |p: Pos2| ((p.y - ruler.top()) / ROW_H).floor() as i32;
    let flags = flag_rects(geo, ruler, project, ui);
    let flag_at = |p: Pos2| {
        flags
            .iter()
            .rev()
            .find(|(_, r, _)| r.contains(p))
            .map(|(f, _, _)| *f)
    };

    if resp.double_clicked() {
        if let Some(p) = pointer {
            match flag_at(p) {
                Some(Flag::Marker(i)) => st.popup = Some((Popup::Marker(i), p, 0)),
                Some(Flag::Tempo(i)) => st.popup = Some((Popup::Tempo(i), p, 0)),
                Some(Flag::Sig(i)) => st.popup = Some((Popup::Sig(i), p, 0)),
                None => {}
            }
        }
    }
    if resp.drag_started_by(egui::PointerButton::Primary)
        || resp.clicked_by(egui::PointerButton::Primary)
    {
        if let Some(p) = gesture_origin(ui, resp) {
            let tick = geo.tick(p.x);
            match (row_of(p), flag_at(p)) {
                (_, Some(Flag::Marker(i))) => {
                    let at = project.playlist.markers[i].at;
                    actions.push(PlaylistAction::Locate(at));
                    st.drag = Some(Drag::Marker {
                        index: i,
                        grab: tick,
                        orig: at,
                    });
                }
                (_, Some(Flag::Tempo(i))) if i > 0 => {
                    let at = project.tempo.points().nth(i).map_or(0, |p| p.at.0);
                    st.drag = Some(Drag::Tempo {
                        index: i,
                        grab: tick,
                        orig: at,
                    });
                }
                (2, _) => {
                    let t = snap.round(tick, &project.signatures).max(0);
                    if resp.clicked_by(egui::PointerButton::Primary) {
                        actions.push(PlaylistAction::Locate(t));
                    } else {
                        st.drag = Some(Drag::Loop { from: t });
                    }
                }
                _ => {}
            }
        }
    }
    if resp.dragged_by(egui::PointerButton::Primary) {
        if let Some(p) = pointer {
            let tick = geo.tick(p.x);
            match st.drag {
                Some(Drag::Loop { from }) => {
                    let to = snap.round(tick, &project.signatures).max(0);
                    st.loop_preview = Some((from.min(to), from.max(to)));
                }
                Some(Drag::Marker { index, grab, orig }) => {
                    let at = snap.round(orig + tick - grab, &project.signatures).max(0);
                    if let Some(m) = project.playlist.markers.get_mut(index) {
                        m.at = at;
                    }
                }
                Some(Drag::Tempo { index, grab, orig }) => {
                    let at = snap.round(orig + tick - grab, &project.signatures).max(1);
                    let mut pts: Vec<TempoPoint> = project.tempo.points().collect();
                    let lo = pts[index - 1].at.0 + 1;
                    let hi = pts.get(index + 1).map_or(i64::MAX, |q| q.at.0 - 1);
                    let at = at.clamp(lo, hi.max(lo));
                    if pts[index].at.0 != at {
                        pts[index].at = Tick(at);
                        project.tempo = TempoMap::from_points_lossy(&pts);
                        timing = true;
                    }
                }
                _ => {}
            }
        }
    }
    let released = !ui.input(|i| i.pointer.any_down());
    if released {
        match st.drag {
            Some(Drag::Loop { .. }) => {
                if let Some((a, b)) = st.loop_preview.take() {
                    if b > a {
                        actions.push(PlaylistAction::SetLoop { start: a, end: b });
                    }
                }
                st.drag = None;
            }
            Some(Drag::Marker { .. }) => {
                project.playlist.sort_markers();
                actions.push(PlaylistAction::Changed);
                st.drag = None;
            }
            Some(Drag::Tempo { .. }) => st.drag = None,
            _ => {}
        }
    }

    if resp.secondary_clicked() {
        if let Some(p) = pointer {
            st.menu_tick = snap.round(geo.tick(p.x), &project.signatures).max(0);
        }
    }
    let menu_tick = st.menu_tick;
    let menu_flag = resp
        .hover_pos()
        .or(ui.ctx().input(|i| i.pointer.press_origin()))
        .and_then(flag_at);
    resp.context_menu(|ui| {
        let sigs = &project.signatures;
        match menu_flag {
            Some(Flag::Marker(i)) => {
                if ui.button("Rename marker").clicked() {
                    st.popup = Some((Popup::Marker(i), pos2(geo.x(menu_tick), ruler.bottom()), 0));
                    ui.close();
                }
                if ui.button("Delete marker").clicked() {
                    if i < project.playlist.markers.len() {
                        project.playlist.markers.remove(i);
                        actions.push(PlaylistAction::Changed);
                    }
                    ui.close();
                }
                return;
            }
            Some(Flag::Tempo(i)) => {
                if ui.button("Edit tempo").clicked() {
                    st.popup = Some((Popup::Tempo(i), pos2(geo.x(menu_tick), ruler.bottom()), 0));
                    ui.close();
                }
                if i > 0 && ui.button("Delete tempo change").clicked() {
                    let mut pts: Vec<TempoPoint> = project.tempo.points().collect();
                    pts.remove(i);
                    project.tempo = TempoMap::from_points_lossy(&pts);
                    timing = true;
                    ui.close();
                }
                return;
            }
            Some(Flag::Sig(i)) => {
                if ui.button("Edit time signature").clicked() {
                    st.popup = Some((Popup::Sig(i), pos2(geo.x(menu_tick), ruler.bottom()), 0));
                    ui.close();
                }
                if i > 0 && ui.button("Delete time signature change").clicked() {
                    let mut ch: Vec<SigChange> = project.signatures.changes().collect();
                    ch.remove(i);
                    project.signatures = TimeSigMap::new(&ch);
                    timing = true;
                    ui.close();
                }
                return;
            }
            None => {}
        }
        let bar = sigs.bar_of(menu_tick);
        if ui.button("Add marker here").clicked() {
            let n = project.playlist.markers.len() + 1;
            let i = project
                .playlist
                .add_marker(menu_tick, &format!("Marker {n}"));
            st.popup = Some((Popup::Marker(i), pos2(geo.x(menu_tick), ruler.bottom()), 0));
            actions.push(PlaylistAction::Changed);
            ui.close();
        }
        if ui.button("Add tempo change here").clicked() {
            let mut pts: Vec<TempoPoint> = project.tempo.points().collect();
            let bpm = project.tempo.bpm_at(Tick(menu_tick));
            pts.push(TempoPoint {
                at: Tick(menu_tick),
                bpm,
            });
            project.tempo = TempoMap::from_points_lossy(&pts);
            let i = project
                .tempo
                .points()
                .position(|p| p.at.0 == menu_tick)
                .unwrap_or(0);
            st.popup = Some((Popup::Tempo(i), pos2(geo.x(menu_tick), ruler.bottom()), 0));
            timing = true;
            ui.close();
        }
        if ui
            .button(format!("Add time signature at bar {}", bar + 1))
            .clicked()
        {
            let mut ch: Vec<SigChange> = project.signatures.changes().collect();
            let sig = project.signatures.sig_of_bar(bar);
            ch.push(SigChange {
                bar,
                sig: if sig == TimeSig::default() {
                    TimeSig::new(3, 4)
                } else {
                    TimeSig::default()
                },
            });
            project.signatures = TimeSigMap::new(&ch);
            let i = project
                .signatures
                .changes()
                .position(|c| c.bar == bar)
                .unwrap_or(0);
            st.popup = Some((
                Popup::Sig(i),
                pos2(geo.x(project.signatures.bar_start(bar)), ruler.bottom()),
                0,
            ));
            timing = true;
            ui.close();
        }
    });
    timing
}

/// Small editor for a marker name, a tempo, a time signature or an automation segment's
/// curve. Returns whether timing changed and whether the playlist changed.
fn show_popup(
    ui: &Ui,
    theme: &GloomTheme,
    st: &mut PlaylistState,
    project: &mut Project,
) -> (bool, bool) {
    let Some((popup, pos, age)) = st.popup else {
        return (false, false);
    };
    let mut timing = false;
    let mut changed = false;
    let mut close = false;
    let area = egui::Area::new(ui.id().with("pl_popup"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| match popup {
                    Popup::Marker(i) => {
                        let Some(m) = project.playlist.markers.get_mut(i) else {
                            close = true;
                            return;
                        };
                        ui.label(RichText::new("Marker").color(theme.text_dim));
                        let r =
                            ui.add(egui::TextEdit::singleline(&mut m.name).desired_width(120.0));
                        if age == 0 {
                            r.request_focus();
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            close = true;
                        }
                    }
                    Popup::Tempo(i) => {
                        let mut pts: Vec<TempoPoint> = project.tempo.points().collect();
                        let Some(p) = pts.get_mut(i) else {
                            close = true;
                            return;
                        };
                        ui.label(RichText::new("BPM").color(theme.text_dim));
                        let mut bpm = p.bpm;
                        if ui
                            .add(
                                egui::DragValue::new(&mut bpm)
                                    .range(gt_core::time::MIN_BPM..=gt_core::time::MAX_BPM)
                                    .speed(0.1)
                                    .fixed_decimals(2),
                            )
                            .changed()
                        {
                            p.bpm = bpm;
                            project.tempo = TempoMap::from_points_lossy(&pts);
                            timing = true;
                        }
                    }
                    Popup::Segment { clip, index } => {
                        let Some(ClipKind::Automation(a)) =
                            project.playlist.clip_mut(clip).map(|c| &mut c.kind)
                        else {
                            close = true;
                            return;
                        };
                        let Some(pt) = a.points.get_mut(index) else {
                            close = true;
                            return;
                        };
                        ui.label(RichText::new("Curve").color(theme.text_dim));
                        for (k, name) in Curve::NAMES.iter().enumerate() {
                            if ui.selectable_label(pt.curve.index() == k, *name).clicked()
                                && pt.curve.index() != k
                            {
                                pt.curve = Curve::from_index(k);
                                changed = true;
                            }
                        }
                        if let Curve::Bezier(t) = &mut pt.curve {
                            if ui
                                .add(
                                    egui::DragValue::new(t)
                                        .range(-1.0..=1.0)
                                        .speed(0.01)
                                        .fixed_decimals(2),
                                )
                                .on_hover_text("Tension: positive rises fast, negative slowly")
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    }
                    Popup::Sig(i) => {
                        let mut ch: Vec<SigChange> = project.signatures.changes().collect();
                        let Some(c) = ch.get_mut(i) else {
                            close = true;
                            return;
                        };
                        ui.label(RichText::new(format!("Bar {}", c.bar + 1)).color(theme.text_dim));
                        let mut num = i32::from(c.sig.num);
                        let mut den = c.sig.den;
                        ui.add(egui::DragValue::new(&mut num).range(1..=32).speed(0.05));
                        ui.label("/");
                        egui::ComboBox::from_id_salt("pl_sig_den")
                            .width(36.0)
                            .selected_text(den.to_string())
                            .show_ui(ui, |ui| {
                                for d in [2u8, 4, 8, 16] {
                                    ui.selectable_value(&mut den, d, d.to_string());
                                }
                            });
                        let sig = TimeSig::new(num as u8, den);
                        if sig != c.sig {
                            c.sig = sig;
                            project.signatures = TimeSigMap::new(&ch);
                            timing = true;
                        }
                    }
                });
                if ui.button("Done").clicked() {
                    close = true;
                }
            });
        });
    let outside = ui.input(|i| {
        i.pointer.any_pressed()
            && i.pointer
                .interact_pos()
                .is_some_and(|p| !area.response.rect.contains(p))
    });
    let combo_open = egui::Popup::is_any_open(ui.ctx());
    if close || ui.input(|i| i.key_pressed(egui::Key::Escape)) || outside && age > 1 && !combo_open
    {
        st.popup = None;
    } else {
        st.popup = Some((popup, pos, age.saturating_add(1)));
    }
    (timing, changed)
}

#[allow(clippy::too_many_arguments)]
fn paint(
    ui: &Ui,
    theme: &GloomTheme,
    st: &PlaylistState,
    geo: &Geo,
    area: Rect,
    ruler: Rect,
    project: &Project,
    view: &PlaylistView<'_>,
) {
    let grid = geo.grid;
    let p = ui.painter_at(area);
    p.rect_filled(area, 0.0, theme.bg_deep);
    let sigs = &project.signatures;
    let (t0, t1) = (geo.tick(grid.left()) - 1, geo.tick(grid.right()) + 1);

    // Track rows.
    let pl = &project.playlist;
    let gp = ui.painter_at(grid);
    for (row, t) in pl.tracks.iter().enumerate() {
        let top = geo.row_top(row);
        if top > grid.bottom() || top + geo.track_h < grid.top() {
            continue;
        }
        let r = Rect::from_min_max(
            pos2(grid.left(), top),
            pos2(grid.right(), top + geo.track_h),
        );
        let base = if row % 2 == 0 {
            theme.bg_panel
        } else {
            theme.bg_deep.lerp_to_gamma(theme.bg_panel, 0.5)
        };
        gp.rect_filled(r, 0.0, base);
        if st.selected_track == Some(t.id) {
            gp.rect_filled(r, 0.0, color(t.color).gamma_multiply(0.06));
        }
        gp.line_segment(
            [
                pos2(grid.left(), r.bottom()),
                pos2(grid.right(), r.bottom()),
            ],
            Stroke::new(1.0, theme.bg_deep),
        );
    }

    // Loop region.
    let (ls, le, lon) = st
        .loop_preview
        .map_or(view.loop_region, |(a, b)| (a, b, true));
    if lon && le > ls {
        let r = Rect::from_min_max(pos2(geo.x(ls), grid.top()), pos2(geo.x(le), grid.bottom()));
        gp.rect_filled(r, 0.0, theme.accent.gamma_multiply(0.04));
    }

    // Vertical lines: beats when at least 8 px apart, bars always (thinned when tight).
    let beat_px = geo.px_per_tick * PPQ as f32;
    let bar_lo = sigs.bar_of(t0.max(-PPQ * 4));
    let bar_hi = sigs.bar_of(t1);
    let bar_px = (sigs.bar_start(bar_lo + 1) - sigs.bar_start(bar_lo)) as f32 * geo.px_per_tick;
    let bar_step = if bar_px >= 24.0 {
        1
    } else {
        (24.0 / bar_px.max(0.1)).ceil() as i64
    };
    if beat_px >= 8.0 {
        sigs.for_each_beat(t0.max(0), t1, |t, down| {
            if !down {
                let x = geo.x(t);
                gp.line_segment(
                    [pos2(x, grid.top()), pos2(x, grid.bottom())],
                    Stroke::new(1.0, theme.bg_widget.gamma_multiply(0.6)),
                );
            }
        });
    }
    for bar in bar_lo.max(0)..=bar_hi {
        if bar % bar_step != 0 {
            continue;
        }
        let x = geo.x(sigs.bar_start(bar));
        gp.line_segment(
            [pos2(x, grid.top()), pos2(x, grid.bottom())],
            Stroke::new(1.0, theme.stroke),
        );
    }

    // Clips.
    for c in &pl.clips {
        let Some(row) = track_row(project, c.track) else {
            continue;
        };
        if c.end() < t0 || c.start > t1 {
            continue;
        }
        let r = geo.clip_rect(row, c.start, c.end());
        if r.bottom() < grid.top() || r.top() > grid.bottom() {
            continue;
        }
        let track = &pl.tracks[row];
        let selected = st.selected.contains(&c.id);
        let audible = !c.muted && pl.is_track_audible(c.track);
        let base = color(track.color);
        let fill = if audible {
            base.gamma_multiply(0.45)
        } else {
            theme.bg_widget
        };
        let cp = gp.with_clip_rect(r.intersect(grid));
        cp.rect_filled(r, 2.0, fill);
        let label = Rect::from_min_max(r.min, pos2(r.right(), r.top() + LABEL_H));
        cp.rect_filled(
            label,
            2.0,
            if audible {
                base.gamma_multiply(0.85)
            } else {
                theme.bg_widget_hover
            },
        );
        let ink = if audible { theme.text } else { theme.text_dim };
        let body = Rect::from_min_max(pos2(r.left(), label.bottom()), r.max);
        let name = match &c.kind {
            ClipKind::Pattern(id) => {
                let pat = project.pattern(*id);
                if let Some(pat) = pat {
                    paint_pattern(&cp, geo, c, pat, body, ink);
                }
                pat.map_or_else(|| "(missing)".to_owned(), |p| p.name.clone())
            }
            ClipKind::Audio { source, .. } => {
                if let Some((peaks, rate)) = view.audio.peaks(source) {
                    paint_wave(&cp, geo, c, &project.tempo, peaks, rate, body, ink);
                }
                source.display_name()
            }
            ClipKind::Automation(a) => {
                paint_curve(&cp, geo, c, a, r, ink, theme.accent);
                a.target.name(project)
            }
        };
        cp.text(
            pos2(r.left() + 4.0, r.top() + 1.0),
            Align2::LEFT_TOP,
            name,
            FontId::proportional(10.5),
            if audible {
                Color32::WHITE
            } else {
                theme.text_dim
            },
        );
        let stroke = if selected {
            Stroke::new(1.5, theme.accent)
        } else {
            Stroke::new(1.0, base.gamma_multiply(0.9))
        };
        cp.rect_stroke(r, 2.0, stroke, egui::StrokeKind::Inside);
    }

    // Selection box.
    if let (Some(Drag::Box { from, .. }), Some(pos)) =
        (&st.drag, ui.input(|i| i.pointer.hover_pos()))
    {
        let r = Rect::from_two_pos(*from, pos);
        gp.rect_filled(r, 0.0, theme.accent.gamma_multiply(0.08));
        gp.rect_stroke(
            r,
            0.0,
            Stroke::new(1.0, theme.accent),
            egui::StrokeKind::Inside,
        );
    }

    // Drop target highlight.
    if egui::DragAndDrop::has_payload_of_type::<SampleSource>(ui.ctx()) {
        if let Some(pos) = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|q| grid.contains(*q))
        {
            if let Ok(row) = usize::try_from(geo.row(pos.y)) {
                if row < pl.tracks.len() {
                    let top = geo.row_top(row);
                    let x = geo.x(st.snap.floor(geo.tick(pos.x), sigs).max(0));
                    gp.rect_stroke(
                        Rect::from_min_max(
                            pos2(x, top + 1.0),
                            pos2(x + 60.0, top + geo.track_h - 1.0),
                        ),
                        2.0,
                        Stroke::new(1.0, theme.accent),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
    }

    // Ruler.
    let rp = ui.painter_at(ruler);
    rp.rect_filled(ruler, 0.0, theme.bg_panel);
    let bars_row = Rect::from_min_max(pos2(ruler.left(), ruler.top() + 2.0 * ROW_H), ruler.max);
    if le > ls {
        // A thin band along the bottom keeps the bar numbers readable.
        let r = Rect::from_min_max(
            pos2(geo.x(ls), bars_row.bottom() - 4.0),
            pos2(geo.x(le), bars_row.bottom()),
        );
        rp.rect_filled(
            r,
            0.0,
            if lon {
                theme.accent_dim
            } else {
                theme.bg_widget
            },
        );
    }
    for bar in bar_lo.max(0)..=bar_hi {
        if bar % bar_step != 0 {
            continue;
        }
        let x = geo.x(sigs.bar_start(bar));
        rp.line_segment(
            [pos2(x, bars_row.top()), pos2(x, bars_row.bottom())],
            Stroke::new(1.0, theme.stroke),
        );
        rp.text(
            pos2(x + 3.0, bars_row.center().y),
            Align2::LEFT_CENTER,
            (bar + 1).to_string(),
            FontId::proportional(10.5),
            theme.text_dim,
        );
    }
    for (flag, r, text) in flag_rects(geo, ruler, project, ui) {
        let (fill, ink) = match flag {
            Flag::Marker(_) => (theme.bg_widget_hover, theme.text),
            Flag::Tempo(_) => (theme.bg_widget, theme.accent),
            Flag::Sig(_) => (theme.bg_widget, theme.text),
        };
        rp.rect_filled(r, 1.0, fill);
        rp.line_segment(
            [pos2(r.left(), r.top()), pos2(r.left(), r.bottom())],
            Stroke::new(1.5, ink),
        );
        rp.text(
            pos2(r.left() + 4.0, r.center().y),
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(10.5),
            ink,
        );
    }
    // Marker lines through the grid.
    for m in &pl.markers {
        let x = geo.x(m.at);
        gp.line_segment(
            [pos2(x, grid.top()), pos2(x, grid.bottom())],
            Stroke::new(1.0, theme.text_dim.gamma_multiply(0.3)),
        );
    }
    p.line_segment(
        [
            pos2(area.left(), grid.top()),
            pos2(area.right(), grid.top()),
        ],
        Stroke::new(1.0, theme.stroke),
    );

    // Playhead.
    if let Some(t) = view.playhead {
        let x = geo.x(t);
        if x >= grid.left() && x <= grid.right() {
            p.line_segment(
                [pos2(x, ruler.top()), pos2(x, grid.bottom())],
                Stroke::new(1.5, theme.accent),
            );
        }
    }
}

/// Mini piano roll of the pattern repetitions inside a clip.
fn paint_pattern(
    p: &egui::Painter,
    geo: &Geo,
    c: &gt_core::Clip,
    pat: &gt_core::Pattern,
    body: Rect,
    ink: Color32,
) {
    let len = pat.length_ticks();
    if len <= 0 || body.height() < 6.0 {
        return;
    }
    let keys = pat.notes.values().flatten().map(|n| n.key);
    let (lo, hi) = keys.fold((u8::MAX, 0u8), |(a, b), k| (a.min(k), b.max(k)));
    if lo > hi {
        return;
    }
    let span = f32::from(hi - lo + 1).max(6.0);
    let row_h = (body.height() - 4.0) / span;
    let vis = p.clip_rect();
    let (src_lo, src_hi) = (c.offset, c.offset + c.length);
    let first = src_lo.div_euclid(len);
    let last = (src_hi - 1).div_euclid(len);
    for k in first..=last {
        let base = k * len;
        // Skip repetitions off screen.
        let x0 = geo.x(c.start + base - src_lo);
        let x1 = geo.x(c.start + base + len - src_lo);
        if x1 < vis.left() || x0 > vis.right() {
            continue;
        }
        for n in pat.notes.values().flatten() {
            let s = (base + n.start).max(src_lo);
            let e = (base + n.start + n.length.max(1)).min(src_hi);
            if e <= s {
                continue;
            }
            let y = body.bottom() - 2.0 - f32::from(n.key - lo + 1) * row_h;
            let r = Rect::from_min_max(
                pos2(geo.x(c.start + s - src_lo), y),
                pos2(
                    geo.x(c.start + e - src_lo)
                        .max(geo.x(c.start + s - src_lo) + 1.0),
                    y + row_h.max(1.0),
                ),
            );
            p.rect_filled(r, 0.0, ink.gamma_multiply(0.85));
        }
    }
}

/// Waveform of an audio clip from the peak cache, one column per pixel.
#[allow(clippy::too_many_arguments)]
fn paint_wave(
    p: &egui::Painter,
    geo: &Geo,
    c: &gt_core::Clip,
    tempo: &TempoMap,
    peaks: &Peaks,
    rate: u32,
    body: Rect,
    ink: Color32,
) {
    let vis = p.clip_rect().intersect(body);
    if vis.width() <= 0.0 {
        return;
    }
    let origin = tempo.tick_to_seconds((c.start - c.offset) as f64);
    let rate = f64::from(rate.max(1));
    let mid = body.center().y;
    let half = body.height() * 0.5 - 1.0;
    let frame_at = |x: f32| (tempo.tick_to_seconds(geo.tick_f(x)) - origin) * rate;
    let mut x = vis.left().floor();
    let mut f0 = frame_at(x);
    while x < vis.right() {
        let f1 = frame_at(x + 1.0);
        if let Some((mn, mx)) = peaks.range(f0, f1.max(f0 + 1.0)) {
            p.line_segment(
                [
                    pos2(x + 0.5, mid - mx.clamp(-1.0, 1.0) * half),
                    pos2(x + 0.5, mid - mn.clamp(-1.0, 1.0) * half + 1.0),
                ],
                Stroke::new(1.0, ink.gamma_multiply(0.8)),
            );
        }
        f0 = f1;
        x += 1.0;
    }
}

fn paint_curve(
    p: &egui::Painter,
    geo: &Geo,
    c: &gt_core::Clip,
    a: &Automation,
    r: Rect,
    ink: Color32,
    accent: Color32,
) {
    let body = curve_rect(r);
    let y = |v: f32| body.bottom() - v * body.height();
    let vis = p.clip_rect().intersect(body);
    if vis.width() <= 0.0 {
        return;
    }
    // Sample the line every 3 px; the points themselves are exact.
    let mut pts = Vec::new();
    let mut x = vis.left();
    while x <= vis.right() + 3.0 {
        let src = geo.tick_f(x) - c.start as f64 + c.offset as f64;
        pts.push(pos2(x, y(a.value_at(src))));
        x += 3.0;
    }
    p.add(egui::Shape::line(pts, Stroke::new(1.5, ink)));
    for (_, q) in point_positions(geo, r, c.start, c.offset, c.length, a) {
        p.circle_filled(q, POINT_R - 1.0, accent);
    }
}

/// Track headers: colour, name, mute, solo and the strip audio clips play into. Returns true
/// if a track changed.
fn track_headers(
    ui: &mut Ui,
    theme: &GloomTheme,
    st: &mut PlaylistState,
    headers: Rect,
    geo: &Geo,
    project: &mut Project,
) -> bool {
    let mut changed = false;
    let mut hui = ui.new_child(egui::UiBuilder::new().max_rect(headers));
    hui.set_clip_rect(headers.intersect(ui.clip_rect()));
    hui.painter().rect_filled(headers, 0.0, theme.bg_panel);
    let mut remove = None;
    let mut insert_at = None;
    let n = project.playlist.tracks.len();
    for row in 0..n {
        let top = geo.row_top(row);
        if top > headers.bottom() || top + geo.track_h < headers.top() {
            continue;
        }
        let r = Rect::from_min_max(
            pos2(headers.left(), top),
            pos2(headers.right() - 2.0, top + geo.track_h),
        );
        let resp = hui.interact(r, hui.id().with(("pl_head", row)), Sense::click());
        if resp.clicked() {
            st.selected_track = Some(project.playlist.tracks[row].id);
        }
        let t = &mut project.playlist.tracks[row];
        let sel = st.selected_track == Some(t.id);
        hui.painter().rect_filled(
            r.shrink2(vec2(0.0, 0.5)),
            0.0,
            if sel {
                theme.bg_widget_hover
            } else {
                theme.bg_widget
            },
        );
        let swatch = Rect::from_min_size(r.min + vec2(2.0, 2.0), vec2(5.0, r.height() - 4.0));
        hui.painter().rect_filled(swatch, 1.0, color(t.color));
        let line1 = Rect::from_min_size(r.min + vec2(10.0, 3.0), vec2(r.width() - 56.0, 16.0));
        if hui
            .put(
                line1,
                egui::TextEdit::singleline(&mut t.name)
                    .frame(egui::Frame::NONE)
                    .font(egui::TextStyle::Small),
            )
            .changed()
        {
            changed = true;
        }
        let btn = |hui: &mut Ui, x: f32, on: bool, label: &str, tip: &str, col: Color32| {
            let b = Rect::from_min_size(pos2(r.right() - x, r.top() + 3.0), vec2(18.0, 16.0));
            let text = RichText::new(label)
                .small()
                .color(if on { col } else { theme.text_dim });
            hui.put(b, egui::Button::selectable(on, text))
                .on_hover_text(tip)
                .clicked()
        };
        if btn(&mut hui, 42.0, t.mute, "M", "Mute track", theme.warn) {
            t.mute = !t.mute;
            changed = true;
        }
        if btn(&mut hui, 22.0, t.solo, "S", "Solo track", theme.accent) {
            t.solo = !t.solo;
            changed = true;
        }
        if r.height() >= 40.0 {
            let line2 =
                Rect::from_min_size(pos2(r.left() + 10.0, r.top() + 21.0), vec2(90.0, 16.0));
            let mut ins = t.insert as i64;
            let resp = hui
                .put(
                    line2,
                    egui::DragValue::new(&mut ins)
                        .range(0..=gt_core::INSERTS as i64)
                        .speed(0.1)
                        .custom_formatter(|v, _| {
                            if v < 0.5 {
                                "audio to M".to_owned()
                            } else {
                                format!("audio to {v:.0}")
                            }
                        }),
                )
                .on_hover_text("Mixer strip this track's audio clips play into (M = master)");
            if resp.changed() {
                t.insert = ins.clamp(0, gt_core::INSERTS as i64) as usize;
                changed = true;
            }
        }
        let id = t.id;
        let mut col = t.color;
        resp.context_menu(|ui| {
            ui.horizontal(|ui| {
                for c in TRACK_COLORS {
                    let (rect, r) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
                    ui.painter().rect_filled(rect, 2.0, color(c));
                    if r.clicked() {
                        col = c;
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Colour");
                egui::color_picker::color_edit_button_srgb(ui, &mut col);
            });
            ui.separator();
            if ui.button("Insert track above").clicked() {
                insert_at = Some(row);
                ui.close();
            }
            if n > 1 && ui.button("Delete track and its clips").clicked() {
                remove = Some(id);
                ui.close();
            }
        });
        if col != project.playlist.tracks[row].color {
            project.playlist.tracks[row].color = col;
            changed = true;
        }
    }
    if let Some(id) = remove {
        project.playlist.remove_track(id);
        changed = true;
    }
    if let Some(row) = insert_at {
        let id = project.playlist.add_track();
        if let Some(i) = project.playlist.track_index(id) {
            let t = project.playlist.tracks.remove(i);
            project.playlist.tracks.insert(row, t);
        }
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_follows_signature_changes() {
        // 4/4, then 3/4 from bar 2 (index 1).
        let sigs = TimeSigMap::new(&[SigChange {
            bar: 1,
            sig: TimeSig::new(3, 4),
        }]);
        let s = PlaylistSnap::Bar;
        assert_eq!(s.floor(3839, &sigs), 0);
        assert_eq!(s.floor(3840 + 2879, &sigs), 3840);
        assert_eq!(s.round(3840 + 1500, &sigs), 3840 + 2880);
        assert_eq!(s.round(3840 + 1400, &sigs), 3840);
        assert_eq!(PlaylistSnap::Beat.floor(3840 + 1000, &sigs), 3840 + 960);
        assert_eq!(PlaylistSnap::Sixteenth.round(130, &sigs), 240);
        assert_eq!(PlaylistSnap::Off.round(131, &sigs), 131);
        assert_eq!(s.floor(-10, &sigs), -3840);
    }

    struct NoAudio;
    impl AudioLookup for NoAudio {
        fn peaks(&self, _: &SampleSource) -> Option<(&Peaks, u32)> {
            None
        }
    }

    /// Runs the playlist for one frame with the given events and returns its actions.
    fn frame(
        ctx: &egui::Context,
        project: &mut Project,
        st: &mut PlaylistState,
        events: Vec<egui::Event>,
    ) -> Vec<PlaylistAction> {
        frame_mods(ctx, project, st, events, egui::Modifiers::NONE)
    }

    /// [`frame`] with modifier keys held.
    fn frame_mods(
        ctx: &egui::Context,
        project: &mut Project,
        st: &mut PlaylistState,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
    ) -> Vec<PlaylistAction> {
        let theme = GloomTheme::default();
        let mut actions = Vec::new();
        let mut events = events;
        events.insert(0, egui::Event::ModifiersChanged(modifiers));
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 600.0))),
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            actions = playlist(
                ui,
                &theme,
                project,
                st,
                PlaylistView {
                    playhead: None,
                    loop_region: (0, 0, false),
                    audio: &NoAudio,
                },
            );
        });
        out.textures_delta.clear();
        actions
    }

    fn click(at: Pos2) -> Vec<Vec<egui::Event>> {
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        vec![
            vec![egui::Event::PointerMoved(at)],
            vec![press(true)],
            vec![press(false)],
            vec![],
        ]
    }

    fn drag(from: Pos2, to: Pos2) -> Vec<Vec<egui::Event>> {
        let press = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut out = vec![
            vec![egui::Event::PointerMoved(from)],
            vec![press(from, true)],
        ];
        for i in 1..=8 {
            out.push(vec![egui::Event::PointerMoved(
                from + (to - from) * (i as f32 / 8.0),
            )]);
        }
        out.push(vec![press(to, false)]);
        out.push(vec![]);
        out
    }

    #[test]
    fn dragging_a_clip_moves_it_across_tracks_and_bars() {
        let ctx = egui::Context::default();
        let mut p = Project::demo();
        let mut st = PlaylistState {
            tool: PlaylistTool::Select,
            ..Default::default()
        };
        frame(&ctx, &mut p, &mut st, vec![]);
        let g = st.last_geo.unwrap();
        // The Break clip: track 3, bars 7-8.
        let id = p
            .playlist
            .clips
            .iter()
            .find(|c| c.start == 6 * 3840 && c.track == p.playlist.tracks[2].id)
            .unwrap()
            .id;
        let from = pos2(g.x(6 * 3840 + 1920), g.row_top(2) + g.track_h * 0.6);
        let to = pos2(g.x(8 * 3840 + 1920), g.row_top(5) + g.track_h * 0.6);
        for ev in drag(from, to) {
            frame(&ctx, &mut p, &mut st, ev);
        }
        let c = p.playlist.clip(id).unwrap();
        assert_eq!((c.start, c.length), (8 * 3840, 2 * 3840));
        assert_eq!(c.track, p.playlist.tracks[5].id);
        assert!(!st.is_dragging());
    }

    /// The demo's first Pattern 1 clip (track 1, bars 3-6) and its row's vertical centre.
    fn beat_clip(p: &Project, g: &Geo) -> (ClipId, f32) {
        let c = p
            .playlist
            .clips
            .iter()
            .find(|c| c.start == 2 * 3840 && c.track == p.playlist.tracks[0].id)
            .unwrap();
        (c.id, g.row_top(0) + g.track_h * 0.6)
    }

    #[test]
    fn dragging_a_right_edge_resizes_and_slice_splits() {
        let ctx = egui::Context::default();
        let mut p = Project::demo();
        let mut st = PlaylistState {
            tool: PlaylistTool::Select,
            ..Default::default()
        };
        frame(&ctx, &mut p, &mut st, vec![]);
        let g = st.last_geo.unwrap();
        let (id, y) = beat_clip(&p, &g);
        // Bars 3-6 end at tick 6*3840; pull the edge back one bar.
        let edge = pos2(g.x(6 * 3840) - 2.0, y);
        for ev in drag(edge, edge - vec2(g.x(3840) - g.x(0), 0.0)) {
            frame(&ctx, &mut p, &mut st, ev);
        }
        let c = p.playlist.clip(id).unwrap();
        assert_eq!((c.start, c.length), (2 * 3840, 3 * 3840));

        st.tool = PlaylistTool::Slice;
        let n = p.playlist.clips.len();
        for ev in click(pos2(g.x(3 * 3840 + 100), y)) {
            frame(&ctx, &mut p, &mut st, ev);
        }
        assert_eq!(p.playlist.clips.len(), n + 1);
        assert_eq!(p.playlist.clip(id).unwrap().length, 3840);
        let new = p.playlist.clip(st.selected[0]).unwrap();
        assert_eq!(
            (new.start, new.length, new.offset),
            (3 * 3840, 2 * 3840, 3840)
        );
    }

    #[test]
    fn dragging_the_bars_row_sets_the_loop() {
        let ctx = egui::Context::default();
        let mut p = Project::demo();
        let mut st = PlaylistState::default();
        frame(&ctx, &mut p, &mut st, vec![]);
        let g = st.last_geo.unwrap();
        let y = g.grid.top() - ROW_H / 2.0;
        let mut actions = Vec::new();
        for ev in drag(pos2(g.x(2 * 3840 + 100), y), pos2(g.x(4 * 3840 - 100), y)) {
            actions.extend(frame(&ctx, &mut p, &mut st, ev));
        }
        assert!(
            actions.contains(&PlaylistAction::SetLoop {
                start: 2 * 3840,
                end: 4 * 3840
            }),
            "{actions:?}"
        );
    }

    #[test]
    fn clicking_empty_space_places_the_current_pattern() {
        let ctx = egui::Context::default();
        let mut p = Project::demo();
        let mut st = PlaylistState::default();
        let before = p.playlist.clips.len();
        // Lay out one frame, then click on track 6 (empty) in the middle of bar 3.
        frame(&ctx, &mut p, &mut st, vec![]);
        let g = st.last_geo.unwrap();
        let at = pos2(g.x(2 * 3840 + 1920), g.row_top(5) + g.track_h / 2.0);
        let mut actions = Vec::new();
        for ev in click(at) {
            actions.extend(frame(&ctx, &mut p, &mut st, ev));
        }
        assert_eq!(p.playlist.clips.len(), before + 1);
        let c = p.playlist.clips.last().unwrap();
        assert_eq!(c.kind, ClipKind::Pattern(p.current_pattern));
        assert_eq!(c.start % 3840, 0, "snapped to a bar");
        assert!(actions.contains(&PlaylistAction::Changed));
        assert_eq!(st.selected, vec![c.id]);
        assert!(!st.is_dragging());
    }

    /// The demo's "Bass filter" automation clip and the screen point in the middle of its first
    /// segment (on the curve's band, between the first two points).
    fn bass_filter_segment(p: &Project, g: &Geo) -> (ClipId, Pos2) {
        let c = p
            .playlist
            .clips
            .iter()
            .find(|c| {
                matches!(c.kind, ClipKind::Automation(_)) && c.track == p.playlist.tracks[4].id
            })
            .unwrap();
        let ClipKind::Automation(a) = &c.kind else {
            unreachable!()
        };
        let mid = c.start - c.offset + (a.points[0].at + a.points[1].at) / 2;
        (c.id, pos2(g.x(mid), g.row_top(4) + g.track_h * 0.75))
    }

    fn first_curve(p: &Project, id: ClipId) -> Curve {
        match &p.playlist.clip(id).unwrap().kind {
            ClipKind::Automation(a) => a.points[0].curve,
            _ => unreachable!(),
        }
    }

    #[test]
    fn ctrl_dragging_a_segment_bends_it() {
        let ctx = egui::Context::default();
        let mut p = Project::demo();
        let mut st = PlaylistState {
            tool: PlaylistTool::Select,
            ..Default::default()
        };
        frame(&ctx, &mut p, &mut st, vec![]);
        let g = st.last_geo.unwrap();
        let (id, at) = bass_filter_segment(&p, &g);
        let points = |p: &Project| match &p.playlist.clip(id).unwrap().kind {
            ClipKind::Automation(a) => a.points.len(),
            _ => 0,
        };
        let n = points(&p);
        // 40 px up is half of the full bend.
        for ev in drag(at, at - vec2(0.0, 40.0)) {
            frame_mods(&ctx, &mut p, &mut st, ev, egui::Modifiers::COMMAND);
        }
        match first_curve(&p, id) {
            Curve::Bezier(t) => assert!((t - 0.5).abs() < 0.02, "{t}"),
            c => panic!("{c:?}"),
        }
        assert_eq!(points(&p), n, "bending adds no point");
        assert!(!st.is_dragging());
    }

    #[test]
    fn right_clicking_a_segment_opens_its_curve_menu() {
        let ctx = egui::Context::default();
        let mut p = Project::demo();
        let mut st = PlaylistState::default();
        frame(&ctx, &mut p, &mut st, vec![]);
        let g = st.last_geo.unwrap();
        let (id, at) = bass_filter_segment(&p, &g);
        let n = p.playlist.clips.len();
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        for ev in [
            vec![egui::Event::PointerMoved(at)],
            vec![press(true)],
            vec![press(false)],
            vec![],
        ] {
            frame(&ctx, &mut p, &mut st, ev);
        }
        assert!(
            matches!(st.popup, Some((Popup::Segment { clip, index: 0 }, _, _)) if clip == id),
            "{:?}",
            st.popup.as_ref().map(|p| p.1)
        );
        assert_eq!(p.playlist.clips.len(), n, "the clip is not erased");
    }
}
