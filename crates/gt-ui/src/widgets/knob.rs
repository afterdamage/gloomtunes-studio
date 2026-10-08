//! Rotary knob.
//!
//! Drag up/down to change (Shift for fine steps), scroll to nudge, double-click to reset. The
//! arc runs from the default value to the current one, so a bipolar control such as pan reads
//! from its centre.

use std::f32::consts::PI;
use std::ops::RangeInclusive;

use egui::{pos2, vec2, Response, Sense, Shape, Stroke, Ui};

use crate::GloomTheme;

/// Vertical drag distance for the full range, in points.
const DRAG_RANGE_PX: f32 = 160.0;
/// Angle sweep: 270 degrees, gap at the bottom.
const START: f32 = 0.75 * PI;
const SWEEP: f32 = 1.5 * PI;

/// A knob over `range`. `format` turns the value into hover text. Returns a response whose
/// `changed()` is true when the value changed.
pub fn knob(
    ui: &mut Ui,
    theme: &GloomTheme,
    value: &mut f32,
    range: RangeInclusive<f32>,
    default: f32,
    diameter: f32,
    format: impl Fn(f32) -> String,
) -> Response {
    let (lo, hi) = (*range.start(), *range.end());
    let (rect, mut resp) =
        ui.allocate_exact_size(vec2(diameter, diameter), Sense::click_and_drag());
    let before = *value;
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let scale = if fine { 0.1 } else { 1.0 };
        *value -= resp.drag_delta().y / DRAG_RANGE_PX * (hi - lo) * scale;
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            *value += scroll / DRAG_RANGE_PX * 0.25 * (hi - lo);
        }
    }
    if resp.double_clicked() {
        *value = default;
    }
    *value = value.clamp(lo, hi);
    if *value != before {
        resp.mark_changed();
    }

    let t_of = |v: f32| ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    let angle_of = |t: f32| START + SWEEP * t;
    let c = rect.center();
    let r = diameter * 0.5 - 2.0;
    let point = |a: f32, rad: f32| pos2(c.x + rad * a.cos(), c.y + rad * a.sin());
    let arc = |t0: f32, t1: f32| -> Vec<egui::Pos2> {
        let (a0, a1) = (angle_of(t0.min(t1)), angle_of(t0.max(t1)));
        let n = (((a1 - a0) / 0.12).ceil() as usize).max(1);
        (0..=n)
            .map(|k| point(a0 + (a1 - a0) * k as f32 / n as f32, r))
            .collect()
    };
    let p = ui.painter();
    let active = resp.hovered() || resp.dragged();
    p.circle_filled(
        c,
        r - 2.0,
        if active {
            theme.bg_widget_hover
        } else {
            theme.bg_widget
        },
    );
    p.add(Shape::line(arc(0.0, 1.0), Stroke::new(2.0, theme.stroke)));
    let (t_def, t_val) = (t_of(default), t_of(*value));
    if (t_val - t_def).abs() > 1e-4 {
        p.add(Shape::line(
            arc(t_def, t_val),
            Stroke::new(2.0, theme.accent),
        ));
    }
    let a = angle_of(t_val);
    p.line_segment(
        [point(a, r * 0.25), point(a, r - 3.0)],
        Stroke::new(1.5, theme.text),
    );
    resp.on_hover_text(format(*value))
}
