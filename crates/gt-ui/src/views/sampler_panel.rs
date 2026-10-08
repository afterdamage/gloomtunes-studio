//! Settings of the selected sampler channel: sample, waveform with start/end, pitch, loop mode
//! and the ADSR envelope.

use egui::{pos2, vec2, RichText, Sense, Stroke, Ui};
use gt_core::{Channel, LoopMode};

use crate::widgets::labeled_knob;
use crate::GloomTheme;

/// Longest attack/decay knob time, ms.
const MAX_AD_MS: f32 = 2000.0;
/// Longest release knob time, ms.
const MAX_R_MS: f32 = 4000.0;

/// What the panel shows about the channel's sample, besides the document.
#[derive(Debug, Clone, Copy, Default)]
pub struct SamplerPanelView<'a> {
    /// Min/max columns of the sample's waveform, if loaded.
    pub waveform: Option<&'a [(f32, f32)]>,
    /// Length of the sample in seconds, if loaded.
    pub seconds: Option<f64>,
    /// Loading or error message.
    pub status: Option<&'a str>,
}

/// Draws the panel. Returns true when a sound parameter changed.
pub fn sampler_panel(
    ui: &mut Ui,
    theme: &GloomTheme,
    ch: &mut Channel,
    view: SamplerPanelView<'_>,
) -> bool {
    let mut changed = false;
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width(170.0);
            ui.add(egui::TextEdit::singleline(&mut ch.name).desired_width(160.0))
                .on_hover_text("Channel name");
            let sample = ch.sampler.sample.as_ref().map_or_else(
                || "No sample: pick one in the browser".to_owned(),
                |s| s.display_name(),
            );
            ui.label(RichText::new(sample).color(theme.text));
            if let Some(s) = view.seconds {
                ui.label(dim(&format!("{s:.2} s")));
            }
            if let Some(st) = view.status {
                ui.label(RichText::new(st).color(theme.warn));
            }
            ui.horizontal(|ui| {
                for (mode, label, tip) in [
                    (
                        LoopMode::OneShot,
                        "One-shot",
                        "Play to the end point, ignore note length",
                    ),
                    (
                        LoopMode::Loop,
                        "Loop",
                        "Repeat start..end while the note is held",
                    ),
                ] {
                    let on = ch.sampler.loop_mode == mode;
                    if ui
                        .add(egui::Button::selectable(on, label))
                        .on_hover_text(tip)
                        .clicked()
                        && !on
                    {
                        ch.sampler.loop_mode = mode;
                        changed = true;
                    }
                }
            });
        });

        waveform(ui, theme, ch, view.waveform);

        let s = &mut ch.sampler;
        changed |= labeled_knob(ui, theme, "Pitch", &mut s.pitch, -24.0..=24.0, 0.0, |v| {
            format!("{v:+.2} semitones")
        })
        .changed();
        changed |= labeled_knob(ui, theme, "Start", &mut s.start, 0.0..=1.0, 0.0, |v| {
            format!("Start {:.1} %", v * 100.0)
        })
        .changed();
        changed |= labeled_knob(ui, theme, "End", &mut s.end, 0.0..=1.0, 1.0, |v| {
            format!("End {:.1} %", v * 100.0)
        })
        .changed();
        if s.end <= s.start {
            s.end = (s.start + 0.001).min(1.0);
            s.start = s.start.min(s.end - 0.001);
        }
        ui.separator();
        let a = &mut s.adsr;
        changed |= time_knob(ui, theme, "Attack", &mut a.attack_ms, MAX_AD_MS, 0.0);
        changed |= time_knob(ui, theme, "Decay", &mut a.decay_ms, MAX_AD_MS, 0.0);
        changed |= labeled_knob(ui, theme, "Sustain", &mut a.sustain, 0.0..=1.0, 1.0, |v| {
            format!("Sustain {:.0} %", v * 100.0)
        })
        .changed();
        changed |= time_knob(ui, theme, "Release", &mut a.release_ms, MAX_R_MS, 50.0);
    });
    changed
}

/// A time knob with a square-law taper: half-way is a quarter of the maximum, so short times
/// (where the ear is most sensitive) get most of the travel.
fn time_knob(
    ui: &mut Ui,
    theme: &GloomTheme,
    label: &str,
    ms: &mut f32,
    max: f32,
    default: f32,
) -> bool {
    let mut t = (*ms / max).clamp(0.0, 1.0).sqrt();
    let r = labeled_knob(
        ui,
        theme,
        label,
        &mut t,
        0.0..=1.0,
        (default / max).sqrt(),
        |t| {
            let v = t * t * max;
            if v < 1000.0 {
                format!("{label} {v:.0} ms")
            } else {
                format!("{label} {:.2} s", v / 1000.0)
            }
        },
    );
    if r.changed() {
        *ms = t * t * max;
        true
    } else {
        false
    }
}

fn waveform(ui: &mut Ui, theme: &GloomTheme, ch: &Channel, cols: Option<&[(f32, f32)]>) {
    let (rect, _) = ui.allocate_exact_size(vec2(260.0, 64.0), Sense::hover());
    let p = ui.painter_at(rect);
    let radius = f32::from(theme.radius);
    p.rect_filled(rect, radius, theme.bg_deep);
    let Some(cols) = cols.filter(|c| !c.is_empty()) else {
        return;
    };
    let s = &ch.sampler;
    let x_of = |f: f32| rect.left() + f.clamp(0.0, 1.0) * rect.width();
    let mid = rect.center().y;
    let half = rect.height() * 0.5 - 2.0;
    let w = rect.width() / cols.len() as f32;
    for (k, &(lo, hi)) in cols.iter().enumerate() {
        let x = rect.left() + (k as f32 + 0.5) * w;
        let frac = (k as f32 + 0.5) / cols.len() as f32;
        let inside = frac >= s.start && frac <= s.end;
        let colour = if inside {
            theme.accent
        } else {
            theme.text_dim.gamma_multiply(0.4)
        };
        p.line_segment(
            [
                pos2(x, mid - hi.max(0.0) * half),
                pos2(x, mid - lo.min(0.0) * half + 0.5),
            ],
            Stroke::new(w.max(1.0), colour),
        );
    }
    for f in [s.start, s.end] {
        p.line_segment(
            [pos2(x_of(f), rect.top()), pos2(x_of(f), rect.bottom())],
            Stroke::new(1.0, theme.text),
        );
    }
}
