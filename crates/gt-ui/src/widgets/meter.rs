//! Horizontal peak meter.

use egui::{Rect, Sense, Stroke, Ui, Vec2};

use crate::GloomTheme;

/// Range shown by the meter, in dBFS.
const FLOOR_DB: f32 = -60.0;

/// Peak meter ballistics, applied on the UI thread: the audio thread only reports raw peaks.
///
/// Rises instantly to a new peak and falls at a fixed rate in dB per second, which is how
/// hardware peak meters behave and makes short transients readable.
#[derive(Debug, Clone)]
pub struct MeterBallistics {
    level_db: f32,
    fall_db_per_s: f32,
}

impl Default for MeterBallistics {
    fn default() -> Self {
        Self {
            level_db: FLOOR_DB,
            fall_db_per_s: 24.0,
        }
    }
}

impl MeterBallistics {
    /// Feeds the raw peak (linear) observed since the last frame; `dt` is the frame time.
    pub fn update(&mut self, raw_peak: f32, dt: f32) {
        let peak_db = if raw_peak > 0.0 {
            (20.0 * raw_peak.log10()).max(FLOOR_DB)
        } else {
            FLOOR_DB
        };
        let fallen = (self.level_db - self.fall_db_per_s * dt).max(FLOOR_DB);
        self.level_db = peak_db.max(fallen);
    }

    /// The displayed level in dBFS.
    pub fn level_db(&self) -> f32 {
        self.level_db
    }

    /// True while the meter is still above the floor and needs repainting.
    pub fn is_moving(&self) -> bool {
        self.level_db > FLOOR_DB
    }
}

/// Draws a horizontal meter for `level_db` (dBFS) with -12 and 0 dBFS tick marks.
pub fn level_meter(ui: &mut Ui, theme: &GloomTheme, level_db: f32, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, theme.radius as f32, theme.bg_deep);

    let x_of = |db: f32| {
        let t = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
        rect.left() + t * rect.width()
    };
    let fill = Rect::from_min_max(rect.min, egui::pos2(x_of(level_db), rect.bottom()));
    let colour = if level_db >= 0.0 {
        theme.warn
    } else {
        theme.accent
    };
    painter.rect_filled(fill, theme.radius as f32, colour);

    for db in [-12.0, 0.0] {
        let x = x_of(db);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            Stroke::new(1.0, theme.text_dim),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rises_instantly_and_falls_slowly() {
        let mut m = MeterBallistics::default();
        m.update(0.251_188_64, 1.0 / 60.0);
        assert!((m.level_db() + 12.0).abs() < 0.01);
        m.update(0.0, 0.5);
        assert!((m.level_db() + 24.0).abs() < 0.01);
        m.update(0.0, 10.0);
        assert!(!m.is_moving());
    }
}
