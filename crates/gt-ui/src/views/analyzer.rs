//! The analyzer effect's display: spectrum with peak hold and slope, oscilloscope, stereo
//! correlation and a chromatic tuner. Everything here runs on the UI thread on a copy of the
//! signal read from the effect's [`Scope`]; the audio thread only writes the copy.

use egui::{pos2, vec2, Rect, RichText, Sense, Shape, Stroke, Ui};
use gt_dsp::analysis::{
    correlation, note_name, note_of, PitchDetector, Scope, Spectrum, SCOPE_FRAMES,
};

use crate::i18n::tr;
use crate::GloomTheme;

const FFT: usize = 4096;
const FLOOR_DB: f32 = -90.0;
const TOP_DB: f32 = 6.0;
/// The display falls this many dB per frame (about 60 per second).
const FALL_DB: f32 = 1.0;
/// Peak hold lets go this slowly (dB per frame) so old peaks fade instead of sticking.
const PEAK_FALL_DB: f32 = 0.05;

/// What the analyzer display keeps between frames.
#[derive(Clone, Default)]
pub struct AnalyzerState {
    spectrum: Option<Spectrum>,
    detector: Option<PitchDetector>,
    l: Vec<f32>,
    r: Vec<f32>,
    mono: Vec<f32>,
    bins: Vec<f32>,
    shown: Vec<f32>,
    peak: Vec<f32>,
    correlation: f32,
    pitch: Option<(f32, f32)>,
}

impl std::fmt::Debug for AnalyzerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AnalyzerState")
    }
}

impl AnalyzerState {
    /// Reads the latest signal and updates every display.
    fn update(&mut self, scope: &Scope) {
        let spectrum = self.spectrum.get_or_insert_with(|| Spectrum::new(FFT));
        scope.read(FFT, &mut self.l, &mut self.r);
        self.mono.clear();
        self.mono
            .extend(self.l.iter().zip(&self.r).map(|(a, b)| 0.5 * (a + b)));
        spectrum.analyze(&self.mono, &mut self.bins);
        if self.shown.len() != self.bins.len() {
            self.shown = vec![FLOOR_DB; self.bins.len()];
            self.peak = vec![FLOOR_DB; self.bins.len()];
        }
        for ((s, p), &b) in self.shown.iter_mut().zip(&mut self.peak).zip(&self.bins) {
            *s = b.max(*s - FALL_DB).max(FLOOR_DB);
            *p = s.max(*p - PEAK_FALL_DB);
        }
        self.correlation = correlation(&self.l, &self.r);
        let detector = self
            .detector
            .get_or_insert_with(|| PitchDetector::new(FFT / 2));
        self.pitch = detector
            .detect(&self.mono, scope.sample_rate(), 30.0, 4200.0)
            .filter(|&(_, clarity)| clarity > 0.8);
    }
}

/// Draws the analyzer for a slot with spectrum `slope` (dB per octave, tilted around 1 kHz)
/// and peak `hold`.
pub fn analyzer_panel(
    ui: &mut Ui,
    theme: &GloomTheme,
    scope: &Scope,
    slope: f32,
    hold: bool,
    state: &mut AnalyzerState,
) {
    state.update(scope);
    ui.ctx().request_repaint();
    let sr = scope.sample_rate().max(8000.0);
    let width = ui.available_width().min(352.0);

    // Spectrum.
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 130.0), Sense::click());
    if resp.clicked() {
        state.peak.fill(FLOOR_DB);
    }
    let p = ui.painter_at(rect);
    p.rect_filled(rect, theme.radius, theme.bg_deep);
    let (f_lo, f_hi) = (20.0_f32, 20_000.0_f32.min(sr * 0.5));
    let x_of = |f: f32| rect.left() + (f / f_lo).ln() / (f_hi / f_lo).ln() * rect.width();
    let y_of = |db: f32| {
        let t = ((db - FLOOR_DB) / (TOP_DB - FLOOR_DB)).clamp(0.0, 1.0);
        rect.bottom() - t * rect.height()
    };
    for f in [100.0, 1000.0, 10_000.0] {
        let x = x_of(f);
        p.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.0, theme.bg_widget),
        );
    }
    for db in [-72.0, -48.0, -24.0, 0.0] {
        let y = y_of(db);
        p.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, theme.bg_widget),
        );
    }
    let bin_hz = sr / FFT as f32;
    let curve = |values: &[f32]| -> Vec<egui::Pos2> {
        // One point per pixel column: the loudest bin under it.
        let n = rect.width() as usize;
        (0..=n)
            .filter_map(|c| {
                let x0 = rect.left() + c as f32;
                let f0 = f_lo * (f_hi / f_lo).powf(c as f32 / n as f32);
                let f1 = f_lo * (f_hi / f_lo).powf((c + 1) as f32 / n as f32);
                let (b0, b1) = ((f0 / bin_hz) as usize, (f1 / bin_hz).ceil() as usize);
                let db = values
                    .get(b0.max(1)..=b1.min(values.len().saturating_sub(1)))?
                    .iter()
                    .fold(FLOOR_DB, |m, &v| m.max(v));
                let tilt = slope * (f0.max(1.0) / 1000.0).log2();
                Some(pos2(x0, y_of(db + tilt)))
            })
            .collect()
    };
    if hold {
        p.add(Shape::line(
            curve(&state.peak),
            Stroke::new(1.0, theme.text_dim),
        ));
    }
    p.add(Shape::line(
        curve(&state.shown),
        Stroke::new(1.5, theme.accent),
    ));
    resp.on_hover_text(format!(
        "Spectrum, {slope:.1} dB/oct slope around 1 kHz. Click to clear the peak hold."
    ));

    ui.horizontal(|ui| {
        // Oscilloscope: the last 20 ms, started on a rising zero crossing so it stands still.
        let (rect, _) = ui.allocate_exact_size(vec2(width * 0.5 - 4.0, 80.0), Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, theme.radius, theme.bg_deep);
        p.line_segment(
            [
                pos2(rect.left(), rect.center().y),
                pos2(rect.right(), rect.center().y),
            ],
            Stroke::new(1.0, theme.bg_widget),
        );
        let span = ((0.02 * sr) as usize).min(SCOPE_FRAMES / 4);
        let search = state.mono.len().saturating_sub(span);
        let start = (search.saturating_sub(span)..search)
            .rev()
            .find(|&i| i > 0 && state.mono[i - 1] < 0.0 && state.mono[i] >= 0.0)
            .unwrap_or(search);
        for (ch, colour) in [(&state.l, theme.accent), (&state.r, theme.text_dim)] {
            let pts: Vec<egui::Pos2> = ch
                .iter()
                .skip(start)
                .take(span)
                .enumerate()
                .map(|(i, &v)| {
                    pos2(
                        rect.left() + i as f32 / span as f32 * rect.width(),
                        rect.center().y - v.clamp(-1.0, 1.0) * rect.height() * 0.5,
                    )
                })
                .collect();
            p.add(Shape::line(pts, Stroke::new(1.0, colour)));
        }

        ui.vertical(|ui| {
            // Correlation: -1 (out of phase) to +1 (mono).
            ui.label(
                RichText::new(tr("Correlation"))
                    .small()
                    .color(theme.text_dim),
            );
            let (rect, _) = ui.allocate_exact_size(vec2(width * 0.5 - 8.0, 12.0), Sense::hover());
            let p = ui.painter_at(rect);
            p.rect_filled(rect, theme.radius, theme.bg_deep);
            let x = rect.center().x + state.correlation * rect.width() * 0.5;
            let colour = if state.correlation < 0.0 {
                theme.warn
            } else {
                theme.accent
            };
            p.rect_filled(
                Rect::from_x_y_ranges(
                    rect.center().x.min(x)..=rect.center().x.max(x),
                    rect.y_range(),
                ),
                theme.radius,
                colour,
            );
            ui.label(
                RichText::new(format!("{:+.2}", state.correlation))
                    .small()
                    .color(theme.text_dim),
            );
            // Tuner.
            ui.label(RichText::new(tr("Tuner")).small().color(theme.text_dim));
            match state.pitch {
                Some((hz, _)) => {
                    let (note, cents) = note_of(hz);
                    let colour = if cents.abs() < 5.0 {
                        theme.accent
                    } else {
                        theme.text
                    };
                    ui.label(
                        RichText::new(format!("{} {cents:+.0}¢", note_name(note)))
                            .strong()
                            .color(colour),
                    )
                    .on_hover_text(format!("{hz:.2} Hz"));
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(width * 0.5 - 8.0, 8.0), Sense::hover());
                    let p = ui.painter_at(rect);
                    p.rect_filled(rect, theme.radius, theme.bg_deep);
                    let x = rect.center().x + cents / 50.0 * rect.width() * 0.5;
                    p.line_segment(
                        [
                            pos2(rect.center().x, rect.top()),
                            pos2(rect.center().x, rect.bottom()),
                        ],
                        Stroke::new(1.0, theme.stroke),
                    );
                    p.line_segment(
                        [pos2(x, rect.top()), pos2(x, rect.bottom())],
                        Stroke::new(2.0, colour),
                    );
                }
                None => {
                    ui.label(RichText::new("—").color(theme.text_dim));
                }
            }
        });
    });
}
