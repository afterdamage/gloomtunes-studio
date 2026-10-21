//! The bottom event lane (Prompt 22): a note property (velocity, release, pan, fine pitch,
//! mod X/Y), a controller (pitch bend, mod wheel, sustain, aftertouch, any MIDI CC) or a
//! pattern automation line for any parameter. Every lane is drawn freehand, as a straight
//! line or as a curve: a line drag rebuilds the lane from the values it had when the drag
//! began, so the line follows the pointer until the button comes up.
//!
//! A note curve lane (Prompt 34, D152) draws polyphonic automation: dragging over a note sets
//! points of its own tuning, pressure or timbre curve (of the selected notes only, when some
//! are selected); right-click removes the nearest point.

use super::*;
use crate::i18n::tr;
use gt_core::{CurveKind, NoteCurve};

/// What the bottom lane shows and edits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lane {
    /// A per-note value.
    Note(NoteProp),
    /// The channel's controller data in this pattern.
    Control(ControlKind),
    /// One of the pattern's automation lines.
    Automation(ParamId),
    /// Each note's own expression curve of this kind (polyphonic automation).
    Curve(CurveKind),
}

/// Points of a note curve closer than this (of the note's length) are moved, not added.
const CURVE_NEAR: f32 = 0.06;

/// The notes under tick `t` whose curve the lane edits: the selected ones if any are.
fn curve_notes(notes: &[Note], selected: &[bool], t: i64) -> Vec<usize> {
    let any_sel = selected.iter().any(|&s| s);
    notes
        .iter()
        .enumerate()
        .filter(|(i, n)| {
            (!any_sel || selected.get(*i).copied().unwrap_or(false))
                && n.start <= t
                && t < n.start + n.length.max(1)
        })
        .map(|(i, _)| i)
        .collect()
}

/// Position of tick `t` in note `n`, 0 to 1.
fn note_frac(n: &Note, t: i64) -> f32 {
    ((t - n.start) as f32 / n.length.max(1) as f32).clamp(0.0, 1.0)
}

impl Default for Lane {
    fn default() -> Self {
        Lane::Note(NoteProp::Velocity)
    }
}

/// How a lane drag draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneDraw {
    /// Values follow the pointer.
    Freehand,
    /// A straight line from where the drag began.
    Line,
    /// A curve from where the drag began, bent by the lane tension.
    Curve,
}

/// Lane values from before a line drag.
#[derive(Debug, Clone)]
pub(super) enum LaneSnapshot {
    /// The notes the line may touch, by index.
    Notes(Vec<(usize, Note)>),
    /// All controller points.
    Controls(Vec<ControlPoint>),
    /// The automation line.
    Auto(Vec<AutoPoint>),
}

/// Steps between controller points a line drag writes, at least.
const CONTROL_LINE_STEP: i64 = PPQ / 8;

/// Lane value (0 to 1) at height `y`.
fn lane_value(lane: Rect, y: f32) -> f32 {
    (1.0 - (y - lane.top() - 2.0) / (lane.height() - 4.0)).clamp(0.0, 1.0)
}

/// Height of lane value `v`.
fn lane_y(lane: Rect, v: f32) -> f32 {
    lane.bottom() - 2.0 - (lane.height() - 4.0) * v.clamp(0.0, 1.0)
}

/// A controller value as 0 to 1 of the lane, and back.
fn control_unit(kind: ControlKind, v: f32) -> f32 {
    (v - kind.min()) / (1.0 - kind.min())
}
fn control_value(kind: ControlKind, u: f32) -> f32 {
    kind.min() + u * (1.0 - kind.min())
}

/// The shape of a line or curve drag: fraction of the way at `x` (0 to 1).
fn line_shape(draw: LaneDraw, tension: f32, x: f32) -> f32 {
    match draw {
        LaneDraw::Curve => gt_core::playlist::Curve::Bezier(tension).shape(x),
        _ => x.clamp(0.0, 1.0),
    }
}

/// Value of a line drag from `(t0, v0)` to `(t1, v1)` at tick `t`.
fn line_value(st: &PianoRollState, (t0, v0): (i64, f32), (t1, v1): (i64, f32), t: i64) -> f32 {
    if t1 == t0 {
        return v1;
    }
    let x = (t - t0) as f32 / (t1 - t0) as f32;
    v0 + (v1 - v0) * line_shape(st.lane_draw, st.lane_tension, x)
}

/// The lane's short name, drawn at its left.
fn short_name(lane: Lane) -> String {
    match lane {
        Lane::Note(p) => match p {
            NoteProp::Velocity => "Vel",
            NoteProp::Release => "Rel",
            NoteProp::Pan => "Pan",
            NoteProp::Fine => "Fine",
            NoteProp::ModX => "X",
            NoteProp::ModY => "Y",
        }
        .to_owned(),
        Lane::Control(k) => match k {
            ControlKind::Bend => "Bend".to_owned(),
            ControlKind::Sustain => "Sus".to_owned(),
            ControlKind::ModWheel => "Mod".to_owned(),
            ControlKind::Pressure => "AT".to_owned(),
            ControlKind::Cc(n) => format!("CC{n}"),
        },
        Lane::Automation(_) => "Auto".to_owned(),
        Lane::Curve(k) => match k {
            CurveKind::Tuning => "Tune",
            CurveKind::Pressure => "Press",
            CurveKind::Timbre | CurveKind::Off => "Timb",
        }
        .to_owned(),
    }
}

/// The lane chooser, drawing mode and lane actions in the toolbar.
pub(super) fn lane_selector(
    ui: &mut Ui,
    theme: &GloomTheme,
    st: &mut PianoRollState,
    view: &mut PianoRollView<'_>,
    actions: &mut Vec<PianoRollAction>,
) {
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    let shown_auto = match st.lane {
        Lane::Automation(t) => view.automation.iter().position(|a| a.target == t),
        _ => None,
    };
    let label = match st.lane {
        Lane::Note(p) => p.name().to_owned(),
        Lane::Control(k) => k.label(),
        Lane::Automation(_) => shown_auto
            .and_then(|i| view.automation_names.get(i))
            .cloned()
            .unwrap_or_default(),
        Lane::Curve(k) => format!("{} {}", tr("Note"), tr(k.name()).to_lowercase()),
    };
    super::fit(ui, 130.0);
    egui::ComboBox::from_id_salt("roll_lane")
        .width(130.0)
        .selected_text(label)
        .show_ui(ui, |ui| {
            ui.label(dim(tr("Notes")));
            for prop in NoteProp::ALL {
                ui.selectable_value(&mut st.lane, Lane::Note(prop), prop.name());
            }
            ui.separator();
            ui.label(dim(tr("Note curves")));
            for kind in CurveKind::ALL {
                ui.selectable_value(&mut st.lane, Lane::Curve(kind), tr(kind.name()));
            }
            ui.separator();
            ui.label(dim(tr("Controllers")));
            let mut kinds: Vec<ControlKind> = ControlKind::ALL.to_vec();
            for c in view.controls.iter() {
                if !kinds.contains(&c.kind) {
                    kinds.push(c.kind);
                }
            }
            if let Lane::Control(k) = st.lane {
                if !kinds.contains(&k) {
                    kinds.push(k);
                }
            }
            for k in kinds {
                ui.selectable_value(&mut st.lane, Lane::Control(k), k.label());
            }
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut st.cc_pick)
                        .range(0..=gt_core::midi::MAX_LEARN_CC)
                        .prefix(tr("CC ")),
                );
                if ui.button(tr("Show")).clicked() {
                    st.lane = Lane::Control(ControlKind::of_cc(st.cc_pick));
                }
            });
            if !view.automation.is_empty() {
                ui.separator();
                ui.label(dim(tr("Automation")));
            }
            for (a, name) in view.automation.iter().zip(&view.automation_names) {
                ui.selectable_value(&mut st.lane, Lane::Automation(a.target), name.as_str());
            }
            let free: Vec<&(ParamId, String)> = view
                .params
                .iter()
                .filter(|(p, _)| !view.automation.iter().any(|a| a.target == *p))
                .collect();
            if !free.is_empty() {
                ui.menu_button(tr("Automate a parameter"), |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for (p, name) in free {
                                if ui.button(name.as_str()).clicked() {
                                    actions.push(PianoRollAction::AutomateParam(*p));
                                    ui.close();
                                }
                            }
                        });
                });
            }
        })
        .response
        .on_hover_text(tr(
            "Bottom lane: a note property, a controller or a pattern automation line \
             (left-drag draws, right-click removes a point)",
        ));
    for (mode, name, tip) in [
        (LaneDraw::Freehand, "Free", "Draw lane values freehand"),
        (LaneDraw::Line, "Line", "Drag a straight line in the lane"),
        (LaneDraw::Curve, "Curve", "Drag a curve in the lane"),
    ] {
        if ui
            .add(egui::Button::selectable(st.lane_draw == mode, name))
            .on_hover_text(tip)
            .clicked()
        {
            st.lane_draw = mode;
        }
    }
    if st.lane_draw == LaneDraw::Curve {
        ui.add(
            egui::DragValue::new(&mut st.lane_tension)
                .range(-1.0..=1.0)
                .speed(0.01)
                .prefix(tr("Bend ")),
        )
        .on_hover_text(tr("Curve tension: positive rises fast then levels off"));
    }
    let clearable = match st.lane {
        Lane::Control(k) => view.controls.iter().any(|c| c.kind == k).then_some(k),
        _ => None,
    };
    if let Some(i) = shown_auto.filter(|_| matches!(st.lane, Lane::Automation(_))) {
        if ui
            .small_button("✕")
            .on_hover_text(tr("Delete this pattern automation"))
            .clicked()
        {
            view.automation.remove(i);
            st.lane = Lane::default();
            actions.push(PianoRollAction::AutomationChanged);
        }
    } else if let Some(k) = clearable {
        if ui
            .small_button("✕")
            .on_hover_text(tr("Clear this controller in the pattern"))
            .clicked()
        {
            actions.push(PianoRollAction::ClearControls(Some(k)));
        }
    }
}

/// Edits the lane with the mouse. Returns (notes or controllers changed, automation changed).
#[allow(clippy::too_many_arguments)]
pub(super) fn lane_input(
    ui: &Ui,
    st: &mut PianoRollState,
    geo: &Geo,
    lane: Rect,
    resp: &Response,
    notes: &mut [Note],
    controls: &mut Vec<ControlPoint>,
    automation: &mut [Automation],
    pattern_len: i64,
) -> (bool, bool) {
    let (pos, pressed, down) = ui.input(|i| {
        (
            i.pointer.interact_pos(),
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
        )
    });
    let Some(p) = pos else {
        return (false, false);
    };
    let auto = match st.lane {
        Lane::Automation(t) => automation.iter().position(|a| a.target == t),
        _ => None,
    };
    if matches!(st.lane, Lane::Automation(_)) && auto.is_none() {
        st.lane = Lane::default();
    }

    // Right-click removes the nearest point of a controller or automation lane.
    if resp.secondary_clicked() {
        let near = |ticks: &mut dyn Iterator<Item = (usize, i64)>| {
            ticks
                .map(|(i, t)| (i, (geo.x(t) - p.x).abs()))
                .filter(|&(_, d)| d <= 8.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        };
        match st.lane {
            Lane::Control(k) => {
                let mut it = controls
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.kind == k)
                    .map(|(i, c)| (i, c.tick));
                if let Some(i) = near(&mut it) {
                    controls.remove(i);
                    return (true, false);
                }
            }
            Lane::Automation(_) => {
                if let Some(points) = auto.map(|i| &mut automation[i].points) {
                    let mut it = points.iter().enumerate().map(|(i, q)| (i, q.at));
                    if let Some(i) = near(&mut it).filter(|_| points.len() > 1) {
                        points.remove(i);
                        return (false, true);
                    }
                }
            }
            Lane::Curve(_) => {
                let t = geo.tick(p.x);
                let mut changed = false;
                for i in curve_notes(notes, &st.selected, t) {
                    let n = &mut notes[i];
                    let near = 8.0 / (geo.x(n.start + n.length.max(1)) - geo.x(n.start)).max(1.0);
                    changed |= n.curve.remove_near(note_frac(n, t), near);
                }
                return (changed, false);
            }
            Lane::Note(_) => {}
        }
        return (false, false);
    }

    let snapped = st.snap.round(geo.tick(p.x)).clamp(0, pattern_len.max(0));
    let value = lane_value(lane, p.y);
    if resp.hovered() && pressed && st.drag.is_none() {
        st.drag = Some(match (st.lane_draw, st.lane) {
            (LaneDraw::Freehand, Lane::Note(_)) | (_, Lane::Curve(_)) => Drag::Velocity,
            (LaneDraw::Freehand, Lane::Automation(_)) => Drag::Automation { last: snapped },
            (LaneDraw::Freehand, Lane::Control(_)) => Drag::Control { last: snapped },
            (_, lane_kind) => {
                let any_sel = st.selected.iter().any(|&s| s);
                let snapshot = match lane_kind {
                    Lane::Note(_) => LaneSnapshot::Notes(
                        notes
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| !any_sel || st.selected[*i])
                            .map(|(i, n)| (i, *n))
                            .collect(),
                    ),
                    Lane::Control(_) => LaneSnapshot::Controls(controls.clone()),
                    Lane::Automation(_) | Lane::Curve(_) => LaneSnapshot::Auto(
                        auto.map(|i| automation[i].points.clone())
                            .unwrap_or_default(),
                    ),
                };
                let t = match lane_kind {
                    Lane::Note(_) => geo.tick(p.x),
                    _ => snapped,
                };
                Drag::LaneLine {
                    from: (t, value),
                    snapshot,
                }
            }
        });
    }
    if !down {
        if matches!(
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
        return (false, false);
    }

    match st.drag.clone() {
        Some(Drag::Velocity) if matches!(st.lane, Lane::Curve(_)) => {
            let Lane::Curve(kind) = st.lane else {
                return (false, false);
            };
            let t = geo.tick(p.x);
            let mut changed = false;
            for i in curve_notes(notes, &st.selected, t) {
                let n = &mut notes[i];
                let before = n.curve;
                if n.curve.kind != kind {
                    n.curve = NoteCurve::new(kind, &[]);
                }
                n.curve.set_point(note_frac(n, t), value, CURVE_NEAR);
                changed |= n.curve != before;
            }
            (changed, false)
        }
        Some(Drag::Velocity) => {
            let Lane::Note(prop) = st.lane else {
                return (false, false);
            };
            let v = if prop == NoteProp::Velocity {
                value.max(0.01)
            } else {
                value
            };
            let any_sel = st.selected.iter().any(|&s| s);
            let (t0, t1) = (geo.tick(p.x - 3.0), geo.tick(p.x + 3.0));
            let mut changed = false;
            for (n, &sel) in notes.iter_mut().zip(&st.selected) {
                if (any_sel && !sel) || n.start < t0 || n.start > t1 {
                    continue;
                }
                let before = *n;
                prop.set(n, v);
                changed |= *n != before;
            }
            if changed && prop == NoteProp::Velocity {
                st.last_velocity = v;
            }
            (changed, false)
        }
        Some(Drag::Control { last }) => {
            let Lane::Control(kind) = st.lane else {
                return (false, false);
            };
            let before = controls.clone();
            let (lo, hi) = (last.min(snapped), last.max(snapped));
            controls.retain(|c| {
                c.kind != kind || c.tick < lo || c.tick > hi || c.tick == last && c.tick != snapped
            });
            let i = controls.partition_point(|c| c.tick <= snapped);
            controls.insert(
                i,
                ControlPoint {
                    tick: snapped,
                    kind,
                    value: control_value(kind, value),
                },
            );
            st.drag = Some(Drag::Control { last: snapped });
            (*controls != before, false)
        }
        Some(Drag::Automation { last }) => {
            let Some(points) = auto.map(|i| &mut automation[i].points) else {
                return (false, false);
            };
            let (lo, hi) = (last.min(snapped), last.max(snapped));
            let before = points.clone();
            points.retain(|q| q.at < lo || q.at > hi || q.at == last && q.at != snapped);
            let i = points.partition_point(|q| q.at < snapped);
            points.insert(i, AutoPoint::new(snapped, value));
            st.drag = Some(Drag::Automation { last: snapped });
            (false, *points != before)
        }
        Some(Drag::LaneLine { from, snapshot }) => match snapshot {
            LaneSnapshot::Notes(orig) => {
                let Lane::Note(prop) = st.lane else {
                    return (false, false);
                };
                let to = (geo.tick(p.x), value);
                let (lo, hi) = (from.0.min(to.0), from.0.max(to.0));
                let mut changed = false;
                for (i, n) in orig {
                    let Some(cur) = notes.get_mut(i) else {
                        continue;
                    };
                    let mut want = n;
                    if (lo..=hi).contains(&n.start) {
                        let v = line_value(st, from, to, n.start);
                        let v = if prop == NoteProp::Velocity {
                            v.max(0.01)
                        } else {
                            v
                        };
                        prop.set(&mut want, v);
                    }
                    if *cur != want {
                        *cur = want;
                        changed = true;
                    }
                }
                (changed, false)
            }
            LaneSnapshot::Controls(orig) => {
                let Lane::Control(kind) = st.lane else {
                    return (false, false);
                };
                let to = (snapped, value);
                let (lo, hi) = (from.0.min(to.0), from.0.max(to.0));
                let mut next: Vec<ControlPoint> = orig
                    .into_iter()
                    .filter(|c| c.kind != kind || c.tick < lo || c.tick > hi)
                    .collect();
                let step = st.snap.ticks().max(CONTROL_LINE_STEP);
                let mut t = lo;
                loop {
                    next.push(ControlPoint {
                        tick: t,
                        kind,
                        value: control_value(kind, line_value(st, from, to, t)),
                    });
                    if t >= hi {
                        break;
                    }
                    t = (t + step).min(hi);
                }
                next.sort_by_key(|c| c.tick);
                let changed = *controls != next;
                *controls = next;
                (changed, false)
            }
            LaneSnapshot::Auto(orig) => {
                let Some(points) = auto.map(|i| &mut automation[i].points) else {
                    return (false, false);
                };
                let to = (snapped, value);
                let (lo, hi) = (from.0.min(to.0), from.0.max(to.0));
                let mut next: Vec<AutoPoint> = orig
                    .into_iter()
                    .filter(|q| q.at < lo || q.at > hi)
                    .collect();
                let curve = match st.lane_draw {
                    LaneDraw::Curve if from.0 <= to.0 => {
                        gt_core::playlist::Curve::Bezier(st.lane_tension)
                    }
                    LaneDraw::Curve => gt_core::playlist::Curve::Bezier(-st.lane_tension),
                    _ => gt_core::playlist::Curve::Linear,
                };
                let first = AutoPoint {
                    curve,
                    ..AutoPoint::new(lo, line_value(st, from, to, lo))
                };
                next.push(first);
                if hi > lo {
                    next.push(AutoPoint::new(hi, line_value(st, from, to, hi)));
                }
                next.sort_by_key(|q| q.at);
                let changed = *points != next;
                *points = next;
                (false, changed)
            }
        },
        _ => (false, false),
    }
}

/// Paints the lane.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_lane(
    ui: &Ui,
    theme: &GloomTheme,
    st: &PianoRollState,
    geo: &Geo,
    area: Rect,
    lane: Rect,
    notes: &[Note],
    controls: &[ControlPoint],
    automation: &[Automation],
    sorted: bool,
) {
    let lp = ui.painter_at(lane);
    lp.rect_filled(lane, 0.0, theme.bg_panel);
    let label_colour = if matches!(st.lane, Lane::Note(NoteProp::Velocity)) {
        theme.text_dim
    } else {
        theme.accent
    };
    ui.painter_at(area).text(
        pos2(area.left() + 4.0, lane.center().y),
        egui::Align2::LEFT_CENTER,
        short_name(st.lane),
        egui::FontId::proportional(10.5),
        label_colour,
    );
    let (t0, t1) = geo.tick_range();
    match st.lane {
        Lane::Note(prop) => {
            if prop.neutral() > 0.0 {
                let y = lane_y(lane, prop.neutral());
                lp.line_segment(
                    [pos2(lane.left(), y), pos2(lane.right(), y)],
                    Stroke::new(1.0, theme.stroke),
                );
            }
            let any_sel = st.selected.iter().any(|&s| s);
            for i in visible_range(notes, t0, t1, sorted) {
                let n = &notes[i];
                let x = geo.x(n.start);
                if x < lane.left() || x > lane.right() {
                    continue;
                }
                let sel = st.selected.get(i).copied().unwrap_or(false);
                let c = if sel || !any_sel {
                    group_color(theme, n.color)
                } else {
                    theme.accent_dim
                };
                let (from, to) = (lane_y(lane, prop.neutral()), lane_y(lane, prop.get(n)));
                lp.line_segment(
                    [pos2(x + 1.0, from), pos2(x + 1.0, to)],
                    Stroke::new(2.0, c),
                );
                lp.circle_filled(pos2(x + 1.0, to), 2.5, c);
            }
            // Recorded controllers, faint, over the stems.
            for kind in ControlKind::ALL {
                paint_controls(&lp, geo, lane, controls, kind, theme.text_dim, false);
            }
        }
        Lane::Control(kind) => {
            if kind == ControlKind::Bend {
                let y = lane_y(lane, 0.5);
                lp.line_segment(
                    [pos2(lane.left(), y), pos2(lane.right(), y)],
                    Stroke::new(1.0, theme.stroke),
                );
            }
            paint_controls(&lp, geo, lane, controls, kind, theme.accent, true);
        }
        Lane::Curve(kind) => {
            let y = lane_y(lane, kind.neutral());
            lp.line_segment(
                [pos2(lane.left(), y), pos2(lane.right(), y)],
                Stroke::new(1.0, theme.stroke),
            );
            let any_sel = st.selected.iter().any(|&s| s);
            for i in visible_range(notes, t0, t1, sorted) {
                let n = &notes[i];
                let (x0, x1) = (geo.x(n.start), geo.x(n.start + n.length.max(1)));
                if x1 < lane.left() || x0 > lane.right() {
                    continue;
                }
                let sel = st.selected.get(i).copied().unwrap_or(false);
                let c = if sel || !any_sel {
                    group_color(theme, n.color)
                } else {
                    theme.accent_dim
                };
                if n.curve.kind != kind || n.curve.is_off() {
                    lp.line_segment(
                        [pos2(x0, y), pos2(x1, y)],
                        Stroke::new(1.0, c.gamma_multiply(0.4)),
                    );
                    continue;
                }
                let steps = ((x1 - x0) / 3.0).clamp(2.0, 200.0) as usize;
                let line: Vec<Pos2> = (0..=steps)
                    .map(|k| {
                        let f = k as f32 / steps as f32;
                        pos2(x0 + (x1 - x0) * f, lane_y(lane, n.curve.value_at(f)))
                    })
                    .collect();
                lp.add(egui::Shape::line(line, Stroke::new(1.5, c)));
                for q in n.curve.points() {
                    lp.circle_filled(pos2(x0 + (x1 - x0) * q.at, lane_y(lane, q.value)), 2.5, c);
                }
            }
        }
        Lane::Automation(t) => {
            let Some(a) = automation.iter().find(|a| a.target == t) else {
                return;
            };
            let points = &a.points;
            // Sampled so curves show their shape; a few points per pixel column at most.
            let (t0, t1) = (geo.tick(lane.left()), geo.tick(lane.right()) + 1);
            let step = ((t1 - t0) as f32 / lane.width().max(1.0)).max(1.0) as i64 * 2;
            let line: Vec<Pos2> = (t0..=t1)
                .step_by(step.max(1) as usize)
                .chain([t1])
                .map(|t| {
                    pos2(
                        geo.x(t),
                        lane_y(lane, gt_core::playlist::value_at(points, t as f64)),
                    )
                })
                .collect();
            lp.add(egui::Shape::line(line, Stroke::new(1.5, theme.accent)));
            for p in points {
                lp.circle_filled(pos2(geo.x(p.at), lane_y(lane, p.value)), 3.0, theme.accent);
            }
        }
    }
}

/// One controller as a held line from its points (with dots when `points`).
fn paint_controls(
    lp: &egui::Painter,
    geo: &Geo,
    lane: Rect,
    controls: &[ControlPoint],
    kind: ControlKind,
    c: Color32,
    points: bool,
) {
    let y = |v: f32| lane_y(lane, control_unit(kind, v));
    let stroke = Stroke::new(if points { 1.5 } else { 1.0 }, c);
    let mut last: Option<Pos2> = None;
    for pt in controls.iter().filter(|c| c.kind == kind) {
        let x = geo.x(pt.tick);
        let at = pos2(x, y(pt.value));
        let from = last.unwrap_or(pos2(geo.x(0), y(kind.min().max(0.0))));
        lp.line_segment([from, pos2(x, from.y)], stroke);
        lp.line_segment([pos2(x, from.y), at], stroke);
        if points {
            lp.circle_filled(at, 3.0, c);
        }
        last = Some(at);
    }
    if let Some(from) = last {
        lp.line_segment([from, pos2(lane.right(), from.y)], stroke);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_and_curve_values() {
        let mut st = PianoRollState {
            lane_draw: LaneDraw::Line,
            ..PianoRollState::default()
        };
        assert_eq!(line_value(&st, (0, 0.0), (100, 1.0), 25), 0.25);
        assert_eq!(line_value(&st, (100, 1.0), (0, 0.0), 25), 0.25);
        st.lane_draw = LaneDraw::Curve;
        st.lane_tension = 0.8;
        // A positive bend rises fast: above the straight line half way.
        assert!(line_value(&st, (0, 0.0), (100, 1.0), 50) > 0.6);
        assert_eq!(control_value(ControlKind::Bend, 0.5), 0.0);
        assert_eq!(control_unit(ControlKind::Bend, 1.0), 1.0);
    }
}
