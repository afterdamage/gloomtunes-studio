//! Mixer view: the strips (master, 64 inserts, 4 sends) side by side, and for the selected
//! strip its routing, send levels and effect slots, with an editor for the selected effect.

use egui::{pos2, vec2, Color32, RichText, Sense, Shape, Stroke, Ui};
use gt_core::effects::EQ_BANDS;
use gt_core::mixer::FIRST_SEND;
use gt_core::{
    EffectKind, EffectSlot, Mixer, MixerStrip, StripKind, FX_SLOTS, MASTER, SENDS, STRIPS,
};
use gt_dsp::fx::{eq_band_response, BandType};

use crate::widgets::{fader, format_gain, format_pan, knob, param_knob, stereo_meter};
use crate::GloomTheme;

const STRIP_W: f32 = 58.0;
const METER_FLOOR_DB: f32 = -60.0;

/// UI state of the mixer that is not part of the document.
#[derive(Debug, Clone)]
pub struct MixerState {
    /// Selected strip index.
    pub selected: usize,
    /// Effect slot open in the editor.
    pub slot: Option<usize>,
    /// EQ band shown in the EQ editor.
    pub eq_band: usize,
}

impl Default for MixerState {
    fn default() -> Self {
        Self {
            selected: MASTER,
            slot: None,
            eq_band: 2,
        }
    }
}

/// Displayed levels of one strip (after the UI's meter ballistics), in dBFS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StripMeter {
    /// Peak, left and right.
    pub peak_db: [f32; 2],
    /// RMS, left and right.
    pub rms_db: [f32; 2],
}

impl Default for StripMeter {
    fn default() -> Self {
        Self {
            peak_db: [METER_FLOOR_DB; 2],
            rms_db: [METER_FLOOR_DB; 2],
        }
    }
}

/// Read-only information for the mixer view.
#[derive(Debug, Clone, Copy)]
pub struct MixerView<'a> {
    /// Meter levels by strip index.
    pub meters: &'a [StripMeter],
    /// Effect meters (gain reduction in dB) by strip and slot.
    pub fx_meters: &'a [[f32; FX_SLOTS]],
    /// Engine sample rate, for the EQ curve.
    pub sample_rate: f32,
    /// Total effect latency to the output, in milliseconds.
    pub latency_ms: f32,
}

/// Strips in display order: master first, then inserts, then sends.
fn display_order() -> impl Iterator<Item = usize> {
    std::iter::once(MASTER).chain(1..STRIPS)
}

/// Short routing label of a strip: "M", "3", "S2".
fn short(i: usize) -> String {
    StripKind::of(i).short()
}

/// Draws the mixer and edits it in place. Returns true if anything changed.
pub fn mixer_view(
    ui: &mut Ui,
    theme: &GloomTheme,
    mixer: &mut Mixer,
    state: &mut MixerState,
    view: MixerView<'_>,
) -> bool {
    let mut changed = false;
    state.selected = state.selected.min(STRIPS - 1);
    let detail_w = 372.0;
    let avail = ui.available_size();
    ui.horizontal_top(|ui| {
        let strips_w = (avail.x - detail_w - 8.0).max(STRIP_W * 3.0);
        ui.allocate_ui(vec2(strips_w, avail.y), |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("mixer_strips")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for i in display_order() {
                            let meter = view.meters.get(i).copied().unwrap_or_default();
                            let (c, clicked) = strip(
                                ui,
                                theme,
                                i,
                                &mut mixer.strips[i],
                                state.selected == i,
                                meter,
                            );
                            changed |= c;
                            if clicked && state.selected != i {
                                state.selected = i;
                                state.slot = mixer.strips[i].slots.iter().position(Option::is_some);
                            }
                            if i == MASTER || i == FIRST_SEND - 1 {
                                ui.add_space(6.0);
                            }
                        }
                    });
                });
        });
        ui.separator();
        let down = egui::Layout::top_down(egui::Align::Min);
        ui.allocate_ui_with_layout(vec2(detail_w, avail.y), down, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("mixer_detail")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    changed |= detail(ui, theme, mixer, state, view);
                });
        });
    });
    if changed {
        mixer.sanitize();
    }
    changed
}

/// One strip. Returns (changed, clicked).
fn strip(
    ui: &mut Ui,
    theme: &GloomTheme,
    i: usize,
    s: &mut MixerStrip,
    selected: bool,
    meter: StripMeter,
) -> (bool, bool) {
    let mut changed = false;
    let fill = if selected {
        theme.accent_dim
    } else {
        theme.bg_panel
    };
    let h = ui.available_height().clamp(200.0, 460.0);
    let resp = egui::Frame::new()
        .fill(fill)
        .corner_radius(theme.radius)
        .inner_margin(egui::Margin::symmetric(2, 3))
        .show(ui, |ui| {
            ui.set_width(STRIP_W - 4.0);
            ui.set_height(h - 6.0);
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(RichText::new(short(i)).small().color(theme.text_dim));
                ui.add(
                    egui::Label::new(RichText::new(&s.name).small().color(theme.text)).truncate(),
                )
                .on_hover_text(&s.name);
                let fx_count = s.slots.iter().flatten().count();
                let fx_text = if fx_count > 0 {
                    format!("fx {fx_count}")
                } else {
                    String::new()
                };
                ui.label(RichText::new(fx_text).small().color(theme.accent));
                let meter_h = (h - 150.0).max(60.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.add_space(2.0);
                    stereo_meter(ui, theme, meter.peak_db, meter.rms_db, vec2(12.0, meter_h));
                    let mut v = s.volume;
                    if fader(
                        ui,
                        theme,
                        &mut v,
                        MixerStrip::MAX_VOLUME,
                        vec2(30.0, meter_h),
                    )
                    .changed()
                    {
                        s.volume = v;
                        changed = true;
                    }
                });
                let mut pan = s.pan;
                if knob(ui, theme, &mut pan, -1.0..=1.0, 0.0, 22.0, |p| {
                    format!("Pan {}", format_pan(p))
                })
                .changed()
                {
                    s.pan = pan;
                    changed = true;
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 1.0;
                    ui.spacing_mut().button_padding = vec2(2.0, 0.0);
                    let small = |t: &str| RichText::new(t).small();
                    for (on, label, tip) in [
                        (&mut s.mute, "M", "Mute"),
                        (
                            &mut s.solo,
                            "S",
                            "Solo: hear this strip and what feeds it or follows it",
                        ),
                        (&mut s.phase_invert, "Ø", "Invert polarity"),
                    ] {
                        if ui
                            .add(
                                egui::Button::selectable(*on, small(label))
                                    .min_size(vec2(16.0, 15.0)),
                            )
                            .on_hover_text(tip)
                            .clicked()
                        {
                            *on = !*on;
                            changed = true;
                        }
                    }
                });
                let to = if i == MASTER {
                    "out".to_owned()
                } else {
                    format!("to {}", short(s.output))
                };
                ui.label(RichText::new(to).small().color(theme.text_dim));
            });
        })
        .response;
    // Pressing anywhere on the strip (fader and knob included) selects it.
    let clicked = ui.rect_contains_pointer(resp.rect) && ui.input(|i| i.pointer.any_pressed());
    (changed, clicked)
}

fn strip_label(m: &Mixer, i: usize) -> String {
    match StripKind::of(i) {
        StripKind::Master => "Master".to_owned(),
        k => {
            let def = k.default_name();
            let name = &m.strips[i].name;
            if *name == def {
                def
            } else {
                format!("{} · {name}", k.short())
            }
        }
    }
}

/// Routing, sends and effect slots of the selected strip.
fn detail(
    ui: &mut Ui,
    theme: &GloomTheme,
    mixer: &mut Mixer,
    state: &mut MixerState,
    view: MixerView<'_>,
) -> bool {
    let mut changed = false;
    let i = state.selected;
    let kind = StripKind::of(i);
    ui.horizontal(|ui| {
        ui.label(RichText::new(kind.default_name()).strong().color(theme.accent));
        let name = &mut mixer.strips[i].name;
        changed |= ui
            .add(egui::TextEdit::singleline(name).desired_width(150.0))
            .on_hover_text("Strip name")
            .changed();
        if view.latency_ms > 0.0 && i == MASTER {
            ui.label(
                RichText::new(format!("PDC {:.1} ms", view.latency_ms))
                    .small()
                    .color(theme.text_dim),
            )
            .on_hover_text("Effect latency the mixer compensates; everything reaches the output this much later");
        }
    });

    egui::Grid::new("mixer_routing")
        .num_columns(2)
        .spacing(vec2(8.0, 4.0))
        .show(ui, |ui| {
            if i != MASTER {
                ui.label(RichText::new("Output").small().color(theme.text_dim));
                let current = mixer.strips[i].output;
                let options: Vec<(usize, String)> = (0..STRIPS)
                    .filter(|&t| t == current || mixer.can_route(i, t))
                    .map(|t| (t, strip_label(mixer, t)))
                    .collect();
                let mut out = current;
                egui::ComboBox::from_id_salt(("mixer_out", i))
                    .width(180.0)
                    .selected_text(strip_label(mixer, current))
                    .show_ui(ui, |ui| {
                        for (t, label) in &options {
                            ui.selectable_value(&mut out, *t, label);
                        }
                    });
                if out != current {
                    mixer.strips[i].output = out;
                    changed = true;
                }
                ui.end_row();
            }
            ui.label(RichText::new("Sidechain").small().color(theme.text_dim))
                .on_hover_text("Key signal for compressors on this strip (switch on Sidechain in the compressor)");
            let current = mixer.strips[i].sidechain;
            let options: Vec<(usize, String)> = (1..STRIPS)
                .filter(|&t| Some(t) == current || mixer.can_sidechain(i, t))
                .map(|t| (t, strip_label(mixer, t)))
                .collect();
            let mut sc = current;
            egui::ComboBox::from_id_salt(("mixer_sc", i))
                .width(180.0)
                .selected_text(current.map_or("None".to_owned(), |t| strip_label(mixer, t)))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut sc, None, "None");
                    for (t, label) in &options {
                        ui.selectable_value(&mut sc, Some(*t), label);
                    }
                });
            if sc != current {
                mixer.strips[i].sidechain = sc;
                changed = true;
            }
            ui.end_row();
        });

    if matches!(kind, StripKind::Insert(_)) {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Sends").small().color(theme.text_dim));
            for k in 0..SENDS {
                let name = mixer.strips[FIRST_SEND + k].name.clone();
                let mut v = mixer.strips[i].sends[k];
                let r = ui
                    .allocate_ui_with_layout(
                        vec2(50.0, 50.0),
                        egui::Layout::top_down(egui::Align::Center),
                        |ui| {
                            let r = knob(ui, theme, &mut v, 0.0..=1.0, 0.0, 26.0, |g| {
                                format!("Send to {name}: {}", format_gain(g))
                            });
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&name).small().color(theme.text_dim),
                                )
                                .truncate(),
                            );
                            r
                        },
                    )
                    .inner;
                if r.changed() {
                    mixer.strips[i].sends[k] = v;
                    changed = true;
                }
            }
        });
    }

    ui.add_space(4.0);
    ui.label(
        RichText::new("EFFECTS")
            .small()
            .strong()
            .color(theme.accent),
    );
    egui::Grid::new(("mixer_slots", i))
        .num_columns(4)
        .spacing(vec2(4.0, 2.0))
        .show(ui, |ui| {
            for k in 0..FX_SLOTS {
                let open = state.slot == Some(k);
                let slot = &mut mixer.strips[i].slots[k];
                let label = RichText::new(format!("{}", k + 1)).small();
                if ui
                    .add(egui::Button::selectable(open, label).min_size(vec2(18.0, 16.0)))
                    .on_hover_text("Edit this effect")
                    .clicked()
                {
                    state.slot = if open { None } else { Some(k) };
                }
                let current = slot.as_ref().map(|s| s.kind);
                let mut pick = current;
                egui::ComboBox::from_id_salt(("mixer_fx", i, k))
                    .width(130.0)
                    .selected_text(RichText::new(current.map_or("—", |k| k.name())).small())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut pick, None, "Empty");
                        for kind in EffectKind::ALL {
                            ui.selectable_value(&mut pick, Some(kind), kind.name());
                        }
                    });
                if pick != current {
                    *slot = pick.map(EffectSlot::new);
                    state.slot = pick.map(|_| k);
                    changed = true;
                }
                if let Some(s) = slot.as_mut() {
                    let text = RichText::new(if s.enabled { "On" } else { "Off" }).small();
                    if ui
                        .add(egui::Button::selectable(s.enabled, text).min_size(vec2(28.0, 16.0)))
                        .on_hover_text("Bypass")
                        .clicked()
                    {
                        s.enabled = !s.enabled;
                        changed = true;
                    }
                    let gr = view.fx_meters.get(i).map_or(0.0, |m| m[k]);
                    let gr_text = if matches!(s.kind, EffectKind::Compressor | EffectKind::Limiter)
                        && s.enabled
                    {
                        format!("GR {gr:.1} dB")
                    } else {
                        String::new()
                    };
                    ui.label(RichText::new(gr_text).small().color(theme.text_dim));
                } else {
                    ui.label("");
                    ui.label("");
                }
                ui.end_row();
            }
        });

    if let Some(k) = state.slot {
        if let Some(slot) = mixer.strips[i].slots[k].as_mut() {
            ui.add_space(4.0);
            ui.separator();
            ui.label(
                RichText::new(format!("{} · slot {}", slot.kind.name(), k + 1))
                    .strong()
                    .color(theme.accent),
            );
            changed |= if slot.kind == EffectKind::Eq {
                eq_editor(
                    ui,
                    theme,
                    (i, k),
                    slot,
                    &mut state.eq_band,
                    view.sample_rate,
                )
            } else {
                knobs(ui, theme, (i, k), slot, 0..slot.kind.params().len())
            };
        }
    }
    changed
}

/// Knobs for the parameters in `range`, wrapping to fit.
fn knobs(
    ui: &mut Ui,
    theme: &GloomTheme,
    id: (usize, usize),
    slot: &mut EffectSlot,
    range: std::ops::Range<usize>,
) -> bool {
    let table = slot.kind.params();
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
        for p in range {
            changed |= param_knob(ui, theme, (id.0, id.1, p), &table[p], &mut slot.params[p]);
        }
    });
    changed
}

/// The EQ: response curve with draggable band handles, band buttons, and the selected band's
/// knobs plus the output gain.
fn eq_editor(
    ui: &mut Ui,
    theme: &GloomTheme,
    id: (usize, usize),
    slot: &mut EffectSlot,
    band: &mut usize,
    sr: f32,
) -> bool {
    let mut changed = false;
    *band = (*band).min(EQ_BANDS - 1);
    let sr = sr.max(8000.0);
    let band_of = |params: &[f32], b: usize| {
        (
            BandType::from_value(params[b * 4]),
            params[b * 4 + 1],
            params[b * 4 + 2],
            params[b * 4 + 3],
        )
    };
    let output = slot.params[EQ_BANDS * 4];

    let size = vec2(ui.available_width().min(352.0), 120.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, theme.radius, theme.bg_deep);
    let (f_lo, f_hi) = (20.0_f32, 20_000.0_f32);
    let db_range = 24.0;
    let x_of = |f: f32| rect.left() + (f / f_lo).ln() / (f_hi / f_lo).ln() * rect.width();
    let f_of =
        |x: f32| f_lo * (f_hi / f_lo).powf(((x - rect.left()) / rect.width()).clamp(0.0, 1.0));
    let y_of = |db: f32| rect.center().y - (db / db_range).clamp(-1.0, 1.0) * rect.height() * 0.5;
    let db_of = |y: f32| {
        ((rect.center().y - y) / (rect.height() * 0.5) * db_range).clamp(-db_range, db_range)
    };
    for f in [100.0, 1000.0, 10_000.0] {
        let x = x_of(f);
        p.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.0, theme.bg_widget),
        );
    }
    for db in [-12.0, 0.0, 12.0] {
        let y = y_of(db);
        let c = if db == 0.0 {
            theme.stroke
        } else {
            theme.bg_widget
        };
        p.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, c),
        );
    }
    let n = rect.width() as usize;
    let pts: Vec<egui::Pos2> = (0..=n)
        .map(|k| {
            let x = rect.left() + k as f32;
            let f = f_of(x);
            let db: f32 = (0..EQ_BANDS)
                .map(|b| {
                    let (t, bf, g, q) = band_of(&slot.params, b);
                    eq_band_response(t, bf, g, q, sr, f)
                })
                .sum::<f32>()
                + output;
            pos2(x, y_of(db))
        })
        .collect();
    p.add(Shape::line(pts, Stroke::new(1.5, theme.accent)));
    // Band handles.
    let handle = |b: usize, params: &[f32]| {
        let (t, f, g, _) = band_of(params, b);
        pos2(x_of(f), y_of(if t.uses_gain() { g } else { 0.0 }))
    };
    for b in 0..EQ_BANDS {
        let (t, ..) = band_of(&slot.params, b);
        let c = handle(b, &slot.params);
        let colour = if b == *band {
            theme.accent
        } else if t == BandType::Off {
            theme.stroke
        } else {
            theme.text_dim
        };
        p.circle_filled(c, if b == *band { 5.0 } else { 4.0 }, colour);
        p.text(
            c + vec2(0.0, -9.0),
            egui::Align2::CENTER_BOTTOM,
            format!("{}", b + 1),
            egui::FontId::proportional(9.0),
            Color32::from_gray(150),
        );
    }
    if let Some(pos) = resp.interact_pointer_pos() {
        if resp.drag_started() || resp.clicked() {
            // Pick the nearest handle.
            if let Some(b) = (0..EQ_BANDS).min_by(|&a, &b| {
                handle(a, &slot.params)
                    .distance(pos)
                    .total_cmp(&handle(b, &slot.params).distance(pos))
            }) {
                if handle(b, &slot.params).distance(pos) < 14.0 {
                    *band = b;
                }
            }
        }
        if resp.dragged() {
            let b = *band;
            let table = slot.kind.params();
            let (t, ..) = band_of(&slot.params, b);
            slot.params[b * 4 + 1] = table[b * 4 + 1].clamp(f_of(pos.x));
            if t.uses_gain() {
                slot.params[b * 4 + 2] = table[b * 4 + 2].clamp(db_of(pos.y));
            }
            if t == BandType::Off {
                slot.params[b * 4] = BandType::Bell as i32 as f32;
                slot.params[b * 4 + 2] = table[b * 4 + 2].clamp(db_of(pos.y));
            }
            changed = true;
        }
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for b in 0..EQ_BANDS {
            let off = BandType::from_value(slot.params[b * 4]) == BandType::Off;
            let text = RichText::new(format!("{}", b + 1)).small().color(if off {
                theme.text_dim
            } else {
                theme.text
            });
            if ui
                .add(egui::Button::selectable(*band == b, text).min_size(vec2(22.0, 16.0)))
                .clicked()
            {
                *band = b;
            }
        }
    });
    ui.horizontal(|ui| {
        changed |= knobs(ui, theme, id, slot, *band * 4..*band * 4 + 4);
        changed |= knobs(ui, theme, id, slot, EQ_BANDS * 4..EQ_BANDS * 4 + 1);
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_order_lists_every_strip_once_master_first() {
        let v: Vec<usize> = display_order().collect();
        assert_eq!(v.len(), STRIPS);
        assert_eq!(v[0], MASTER);
        let mut sorted = v.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), STRIPS);
    }
}
