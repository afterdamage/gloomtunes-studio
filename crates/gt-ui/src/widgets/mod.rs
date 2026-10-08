//! Reusable custom-painted widgets.

mod fader;
mod knob;
mod meter;

pub use fader::{fader, fader_gain, fader_position, stereo_meter};
pub use knob::knob;
pub use meter::{level_meter, MeterBallistics};

use egui::{RichText, Ui};
use gt_core::ParamInfo;

use crate::GloomTheme;

/// A knob with a small caption underneath.
#[allow(clippy::too_many_arguments)]
pub fn labeled_knob(
    ui: &mut Ui,
    theme: &GloomTheme,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    default: f32,
    format: impl Fn(f32) -> String,
) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(46.0, 52.0),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            let r = knob(ui, theme, value, range, default, 30.0, format);
            ui.label(RichText::new(label).small().color(theme.text_dim));
            r
        },
    )
    .inner
}

/// A labelled knob for a described parameter, with the knob's travel following the
/// parameter's taper. `id_salt` must be unique among the knobs drawn in the same `Ui`.
/// Returns true if the value changed.
pub fn param_knob(
    ui: &mut Ui,
    theme: &GloomTheme,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    info: &ParamInfo,
    value: &mut f32,
) -> bool {
    // Stepped parameters round their value, so while dragging keep the unrounded knob position
    // or small drags would never move it.
    let id = ui.id().with(("param_knob", id_salt));
    let stored: Option<f32> = ui.data(|d| d.get_temp(id));
    let mut t = stored.unwrap_or_else(|| info.to_normalized(*value));
    let before = *value;
    let resp = labeled_knob(
        ui,
        theme,
        info.name,
        &mut t,
        0.0..=1.0,
        info.to_normalized(info.default),
        |t| format!("{}: {}", info.name, info.format(info.from_normalized(t))),
    );
    if resp.dragged() {
        ui.data_mut(|d| d.insert_temp(id, t));
    } else if stored.is_some() {
        ui.data_mut(|d| d.remove::<f32>(id));
    }
    if resp.changed() {
        *value = info.from_normalized(t);
    }
    *value != before
}

/// Formats a linear gain as dB for hover text.
pub fn format_gain(g: f32) -> String {
    if g <= 1e-5 {
        "-inf dB".to_owned()
    } else {
        format!("{:+.1} dB", 20.0 * g.log10())
    }
}

/// Formats a pan value as "C", "L 40" or "R 100".
pub fn format_pan(p: f32) -> String {
    let v = (p * 100.0).round() as i32;
    match v {
        0 => "C".to_owned(),
        v if v < 0 => format!("L {}", -v),
        v => format!("R {v}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(format_gain(1.0), "+0.0 dB");
        assert_eq!(format_gain(0.0), "-inf dB");
        assert_eq!(format_pan(0.0), "C");
        assert_eq!(format_pan(-0.4), "L 40");
        assert_eq!(format_pan(1.0), "R 100");
    }
}
