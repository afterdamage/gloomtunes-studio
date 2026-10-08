//! Vertical fader and stereo level meter for mixer strips.

use egui::{pos2, vec2, Rect, Response, Sense, Stroke, Ui};

use crate::GloomTheme;

/// Meter range in dBFS.
const FLOOR_DB: f32 = -60.0;
/// Fader travel for the full range, in points of drag.
const DRAG_PX: f32 = 140.0;

/// Fader position (0..1) to linear gain: `max · t³`, so unity sits at about 79 % of the travel
/// (with `max` = +6 dB) and the lower part of the travel spreads out the quiet range.
pub fn fader_gain(t: f32, max: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    max * t * t * t
}

/// Inverse of [`fader_gain`].
pub fn fader_position(gain: f32, max: f32) -> f32 {
    (gain.max(0.0) / max).cbrt().clamp(0.0, 1.0)
}

/// A vertical fader for a linear gain in `0..=max`. Drag (Shift for fine), scroll to nudge,
/// double-click for unity. `changed()` on the response reports edits.
pub fn fader(
    ui: &mut Ui,
    theme: &GloomTheme,
    gain: &mut f32,
    max: f32,
    size: egui::Vec2,
) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let before = *gain;
    let mut t = fader_position(*gain, max);
    if resp.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) {
            0.1
        } else {
            1.0
        };
        t -= resp.drag_delta().y / DRAG_PX * fine;
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            t += scroll / DRAG_PX * 0.25;
        }
    }
    if resp.dragged() || resp.hovered() && ui.input(|i| i.smooth_scroll_delta.y != 0.0) {
        *gain = fader_gain(t, max);
    }
    if resp.double_clicked() {
        *gain = 1.0;
    }
    if *gain != before {
        resp.mark_changed();
    }
    let t = fader_position(*gain, max);
    let p = ui.painter_at(rect.expand(2.0));
    let track = Rect::from_center_size(rect.center(), vec2(4.0, rect.height() - 10.0));
    p.rect_filled(track, 1.0, theme.bg_deep);
    // Unity mark.
    let y_of = |t: f32| track.bottom() - t * track.height();
    let unity = y_of(fader_position(1.0, max));
    p.line_segment(
        [
            pos2(rect.left() + 2.0, unity),
            pos2(rect.right() - 2.0, unity),
        ],
        Stroke::new(1.0, theme.stroke),
    );
    let y = y_of(t);
    let cap = Rect::from_center_size(pos2(rect.center().x, y), vec2(rect.width() - 4.0, 10.0));
    let active = resp.hovered() || resp.dragged();
    p.rect_filled(
        cap,
        2.0,
        if active {
            theme.bg_widget_hover
        } else {
            theme.bg_widget
        },
    );
    p.line_segment(
        [pos2(cap.left() + 2.0, y), pos2(cap.right() - 2.0, y)],
        Stroke::new(1.5, theme.accent),
    );
    resp.on_hover_text(crate::widgets::format_gain(*gain))
}

/// A vertical stereo meter: two bars filled to the peak level, with a brighter tick at the RMS
/// level. Levels in dBFS; anything at or above 0 dBFS turns the warning colour.
pub fn stereo_meter(
    ui: &mut Ui,
    theme: &GloomTheme,
    peak_db: [f32; 2],
    rms_db: [f32; 2],
    size: egui::Vec2,
) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    let w = (rect.width() - 2.0) / 2.0;
    let y_of = |db: f32| {
        let t = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
        rect.bottom() - t * rect.height()
    };
    for side in 0..2 {
        let x0 = rect.left() + side as f32 * (w + 2.0);
        let bar = Rect::from_min_max(pos2(x0, rect.top()), pos2(x0 + w, rect.bottom()));
        p.rect_filled(bar, 1.0, theme.bg_deep);
        let fill = Rect::from_min_max(pos2(x0, y_of(peak_db[side])), bar.max);
        let colour = if peak_db[side] >= 0.0 {
            theme.warn
        } else {
            theme.accent_dim
        };
        p.rect_filled(fill, 1.0, colour);
        let ry = y_of(rms_db[side]);
        if rms_db[side] > FLOOR_DB {
            let r = Rect::from_min_max(pos2(x0, ry), bar.max);
            p.rect_filled(r, 1.0, theme.accent);
        }
    }
    for db in [-12.0, 0.0] {
        let y = y_of(db);
        p.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, theme.stroke),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_law_round_trips() {
        let max = 1.995_262_3;
        assert_eq!(fader_gain(1.0, max), max);
        assert_eq!(fader_gain(0.0, max), 0.0);
        for g in [0.0, 0.1, 0.5, 1.0, 1.5] {
            assert!((fader_gain(fader_position(g, max), max) - g).abs() < 1e-5);
        }
        assert!((fader_position(1.0, max) - 0.794).abs() < 0.01);
    }
}
