//! Gloom Synth editor: knobs for every parameter in labelled sections, the modulation matrix,
//! a filter response curve, an oscilloscope and preset handling.
//!
//! Knobs move over 0..1 and map to each parameter's own range and taper (`ParamInfo`), so
//! cutoff is logarithmic and times favour short values. Stepped parameters (waves, octaves,
//! unison voices) snap to whole values; the unrounded knob position is kept while dragging so
//! slow drags still get from one step to the next.

use std::path::PathBuf;

use egui::{pos2, vec2, Pos2, RichText, Sense, Shape, Stroke, Ui};
use gt_core::synth::{ModDest, ModSource, SynthParam as P, SynthPatch};

use crate::GloomTheme;

/// What the panel shows besides the patch.
#[derive(Debug, Clone, Copy)]
pub struct SynthPanelView<'a> {
    /// Recent output of this channel, oldest first (for the oscilloscope).
    pub scope: &'a [f32],
    /// Saved user presets: display name and file.
    pub user_presets: &'a [(String, PathBuf)],
    /// Engine sample rate (for the filter curve).
    pub sample_rate: f32,
    /// Message from the last preset operation.
    pub status: Option<&'a str>,
}

/// Things the app must do.
#[derive(Debug, Clone, PartialEq)]
pub enum SynthPanelAction {
    /// A sound parameter changed (push the patch to the engine).
    Changed,
    /// Replace the patch with this user preset file.
    LoadFile(PathBuf),
    /// Save the patch as a user preset under its name.
    Save,
}

/// Draws the editor for one synth channel.
pub fn synth_panel(
    ui: &mut Ui,
    theme: &GloomTheme,
    name: &mut String,
    patch: &mut SynthPatch,
    view: SynthPanelView<'_>,
) -> Vec<SynthPanelAction> {
    let mut actions = Vec::new();
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(name).desired_width(120.0))
            .on_hover_text("Channel name");
        ui.separator();
        ui.label(RichText::new("Preset").color(theme.text_dim));
        egui::ComboBox::from_id_salt("synth_preset")
            .width(140.0)
            .selected_text(patch.name.clone())
            .show_ui(ui, |ui| {
                ui.label(RichText::new("Factory").small().color(theme.text_dim));
                for f in SynthPatch::factory() {
                    if ui.selectable_label(false, &f.name).clicked() {
                        *patch = f;
                        changed = true;
                    }
                }
                if !view.user_presets.is_empty() {
                    ui.separator();
                    ui.label(RichText::new("Yours").small().color(theme.text_dim));
                    for (n, path) in view.user_presets {
                        if ui.selectable_label(false, n).clicked() {
                            actions.push(SynthPanelAction::LoadFile(path.clone()));
                        }
                    }
                }
            });
        ui.add(egui::TextEdit::singleline(&mut patch.name).desired_width(110.0))
            .on_hover_text("Preset name (used when saving)");
        if ui
            .button("Save preset")
            .on_hover_text("Save this sound to your preset folder")
            .clicked()
        {
            actions.push(SynthPanelAction::Save);
        }
        if let Some(s) = view.status {
            ui.label(RichText::new(s).small().color(theme.text_dim));
        }
    });
    ui.add_space(2.0);

    // The right edge sections must stay within, taken before the wrapping layout, whose own
    // max_rect can grow with its contents.
    let right = ui.max_rect().right().min(ui.clip_rect().right());
    {
        {
            ui.horizontal_wrapped(|ui| {
                for (title, osc) in [("OSC 1", 0), ("OSC 2", 1)] {
                    let ps = if osc == 0 {
                        [
                            P::Osc1Wave,
                            P::Osc1Octave,
                            P::Osc1Semi,
                            P::Osc1Fine,
                            P::Osc1Level,
                            P::Osc1Pw,
                        ]
                    } else {
                        [
                            P::Osc2Wave,
                            P::Osc2Octave,
                            P::Osc2Semi,
                            P::Osc2Fine,
                            P::Osc2Level,
                            P::Osc2Pw,
                        ]
                    };
                    section(ui, theme, right, title, |ui| {
                        changed |= knob_rows(ui, theme, patch, &[&ps[..3], &ps[3..]]);
                    });
                }
                section(ui, theme, right, "SUB · NOISE · UNISON", |ui| {
                    changed |= knob_rows(
                        ui,
                        theme,
                        patch,
                        &[
                            &[P::SubLevel, P::NoiseLevel],
                            &[P::UnisonVoices, P::UnisonDetune, P::UnisonSpread],
                        ],
                    );
                });
                section(ui, theme, right, "FILTER", |ui| {
                    ui.horizontal_top(|ui| {
                        ui.vertical(|ui| {
                            changed |= knob_rows(
                                ui,
                                theme,
                                patch,
                                &[
                                    &[P::Cutoff, P::Resonance, P::Drive],
                                    &[P::FilterEnv, P::KeyTrack],
                                ],
                            );
                        });
                        filter_curve(ui, theme, patch, view.sample_rate);
                    });
                });
                section(ui, theme, right, "ENVELOPES", |ui| {
                    for (label, row) in [
                        (
                            "AMP",
                            [P::AmpAttack, P::AmpDecay, P::AmpSustain, P::AmpRelease],
                        ),
                        (
                            "MOD",
                            [P::ModAttack, P::ModDecay, P::ModSustain, P::ModRelease],
                        ),
                    ] {
                        ui.horizontal(|ui| {
                            row_label(ui, theme, label);
                            changed |= knob_rows(ui, theme, patch, &[&row]);
                        });
                    }
                });
                section(ui, theme, right, "LFOS · VOICE", |ui| {
                    for (label, row) in [
                        ("LFO 1", &[P::Lfo1Wave, P::Lfo1Rate, P::Glide][..]),
                        (
                            "LFO 2",
                            &[P::Lfo2Wave, P::Lfo2Rate, P::VelocitySens, P::Volume][..],
                        ),
                    ] {
                        ui.horizontal(|ui| {
                            row_label(ui, theme, label);
                            changed |= knob_rows(ui, theme, patch, &[row]);
                        });
                    }
                });
                section(ui, theme, right, "MOD MATRIX", |ui| {
                    changed |= mod_matrix(ui, theme, patch);
                });
                section(ui, theme, right, "SCOPE", |ui| scope(ui, theme, view.scope));
            });
        }
    }
    if changed {
        actions.push(SynthPanelAction::Changed);
    }
    actions
}

fn row_label(ui: &mut Ui, theme: &GloomTheme, text: &str) {
    ui.add_sized(
        vec2(30.0, 46.0),
        egui::Label::new(RichText::new(text).small().color(theme.text_dim)),
    );
}

/// A titled box. Inside a wrapping layout it starts a new row when its width (remembered from
/// the previous frame) no longer fits, since a frame only knows its size after drawing.
fn section(ui: &mut Ui, theme: &GloomTheme, right: f32, title: &str, body: impl FnOnce(&mut Ui)) {
    let id = ui.id().with(("synth_section_width", title));
    let width: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(160.0);
    let x = ui.cursor().min.x;
    let at_row_start = x <= ui.max_rect().min.x + 1.0;
    if !at_row_start && x + width > right {
        ui.end_row();
    }
    let r = egui::Frame::new()
        .fill(theme.bg_deep)
        .corner_radius(theme.radius)
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(title).small().strong().color(theme.accent));
                body(ui);
            });
        });
    let w = r.response.rect.width();
    ui.data_mut(|d| d.insert_temp(id, w));
}

/// Rows of parameter knobs. Returns true if any value changed.
fn knob_rows(ui: &mut Ui, theme: &GloomTheme, patch: &mut SynthPatch, rows: &[&[P]]) -> bool {
    let mut changed = false;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
        for row in rows {
            ui.horizontal(|ui| {
                for &p in *row {
                    changed |= param_knob(ui, theme, patch, p);
                }
            });
        }
    });
    changed
}

fn param_knob(ui: &mut Ui, theme: &GloomTheme, patch: &mut SynthPatch, p: P) -> bool {
    let mut v = patch.get(p);
    let changed = crate::widgets::param_knob(ui, theme, ("synth", p as usize), p.info(), &mut v);
    if changed {
        patch.set(p, v);
    }
    changed
}

fn mod_matrix(ui: &mut Ui, theme: &GloomTheme, patch: &mut SynthPatch) -> bool {
    let mut changed = false;
    egui::Grid::new("synth_mods")
        .num_columns(4)
        .spacing(vec2(4.0, 1.0))
        .show(ui, |ui| {
            for (i, m) in patch.mods.iter_mut().enumerate() {
                ui.label(
                    RichText::new(format!("{}", i + 1))
                        .small()
                        .color(theme.text_dim),
                );
                egui::ComboBox::from_id_salt(("mod_src", i))
                    .width(70.0)
                    .selected_text(RichText::new(m.source.label()).small())
                    .show_ui(ui, |ui| {
                        for &s in ModSource::ALL {
                            changed |= ui.selectable_value(&mut m.source, s, s.label()).changed();
                        }
                    });
                egui::ComboBox::from_id_salt(("mod_dst", i))
                    .width(84.0)
                    .selected_text(RichText::new(m.dest.label()).small())
                    .show_ui(ui, |ui| {
                        for &d in ModDest::ALL {
                            changed |= ui.selectable_value(&mut m.dest, d, d.label()).changed();
                        }
                    });
                let mut pct = m.amount * 100.0;
                if ui
                    .add(
                        egui::DragValue::new(&mut pct)
                            .range(-100.0..=100.0)
                            .speed(0.5)
                            .suffix(" %")
                            .fixed_decimals(0),
                    )
                    .on_hover_text("Amount. 100 % is the full range of the destination")
                    .changed()
                {
                    m.amount = pct / 100.0;
                    changed = true;
                }
                ui.end_row();
            }
        });
    changed
}

/// Filter magnitude response on log frequency (20 Hz to 20 kHz) and dB (-48 to +24) axes.
fn filter_curve(ui: &mut Ui, theme: &GloomTheme, patch: &SynthPatch, sample_rate: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(150.0, 100.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, f32::from(theme.radius), theme.bg_panel);
    let (f_lo, f_hi) = (20.0_f32, 20_000.0_f32);
    let (db_lo, db_hi) = (-48.0_f32, 24.0_f32);
    let x_of = |f: f32| rect.left() + rect.width() * (f / f_lo).ln() / (f_hi / f_lo).ln();
    let y_of = |db: f32| {
        rect.bottom() - rect.height() * (db.clamp(db_lo, db_hi) - db_lo) / (db_hi - db_lo)
    };
    for f in [100.0, 1000.0, 10_000.0] {
        let x = x_of(f);
        p.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.0, theme.bg_widget),
        );
    }
    let y0 = y_of(0.0);
    p.line_segment(
        [pos2(rect.left(), y0), pos2(rect.right(), y0)],
        Stroke::new(1.0, theme.stroke),
    );
    let sr = sample_rate.max(8000.0);
    let cutoff = patch.get(P::Cutoff);
    let res = patch.get(P::Resonance);
    let n = 120;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let f = f_lo * (f_hi / f_lo).powf(i as f32 / n as f32);
            let mag = if f < sr * 0.5 {
                gt_dsp::ladder_response(cutoff, res, sr, f)
            } else {
                0.0
            };
            pos2(x_of(f), y_of(20.0 * mag.max(1e-6).log10()))
        })
        .collect();
    p.add(Shape::line(pts, Stroke::new(1.5, theme.accent)));
    let xc = x_of(cutoff.clamp(f_lo, f_hi));
    p.line_segment(
        [pos2(xc, rect.top()), pos2(xc, rect.bottom())],
        Stroke::new(1.0, theme.text_dim.gamma_multiply(0.5)),
    );
    p.text(
        rect.left_top() + vec2(3.0, 2.0),
        egui::Align2::LEFT_TOP,
        P::Cutoff.info().format(cutoff),
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );
}

/// Index where the shown window starts: the last rising zero crossing that leaves `window`
/// samples after it, so a periodic wave stands still. Falls back to the newest window.
pub fn scope_trigger(samples: &[f32], window: usize) -> usize {
    let n = samples.len();
    if n <= window {
        return 0;
    }
    let last = n - window;
    (1..=last)
        .rev()
        .find(|&i| samples[i - 1] <= 0.0 && samples[i] > 0.0)
        .unwrap_or(last)
}

fn scope(ui: &mut Ui, theme: &GloomTheme, samples: &[f32]) {
    let (rect, _) = ui.allocate_exact_size(vec2(170.0, 100.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, f32::from(theme.radius), theme.bg_panel);
    let mid = rect.center().y;
    p.line_segment(
        [pos2(rect.left(), mid), pos2(rect.right(), mid)],
        Stroke::new(1.0, theme.bg_widget),
    );
    const WINDOW: usize = 1024;
    let start = scope_trigger(samples, WINDOW);
    let shown = &samples[start..(start + WINDOW).min(samples.len())];
    if shown.is_empty() {
        return;
    }
    // Scale so the loudest sample fills most of the height, but never magnify noise.
    let peak = shown.iter().fold(0.0_f32, |m, s| m.max(s.abs())).max(0.25);
    let half = rect.height() * 0.45;
    let pts: Vec<Pos2> = shown
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            pos2(
                rect.left() + rect.width() * i as f32 / (WINDOW - 1) as f32,
                mid - (s / peak).clamp(-1.1, 1.1) * half,
            )
        })
        .collect();
    p.add(Shape::line(pts, Stroke::new(1.2, theme.accent)));
    if peak <= 0.25 && shown.iter().all(|s| s.abs() < 1e-4) {
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "play a note",
            egui::FontId::proportional(10.0),
            theme.text_dim.gamma_multiply(0.6),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_triggers_on_a_rising_zero_crossing() {
        let s: Vec<f32> = (0..4096).map(|i| (i as f32 * 0.05 + 1.0).sin()).collect();
        let i = scope_trigger(&s, 1024);
        assert!(i <= 4096 - 1024);
        assert!(s[i - 1] <= 0.0 && s[i] > 0.0);
        assert_eq!(scope_trigger(&[0.0; 100], 1024), 0);
        assert_eq!(scope_trigger(&[0.0; 2000], 1024), 2000 - 1024);
    }
}
