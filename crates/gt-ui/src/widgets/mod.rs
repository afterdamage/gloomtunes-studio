//! Reusable custom-painted widgets.

mod knob;
mod meter;

pub use knob::knob;
pub use meter::{level_meter, MeterBallistics};

use egui::{RichText, Ui};

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
