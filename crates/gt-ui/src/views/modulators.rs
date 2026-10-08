//! The modulators editor: every LFO and envelope follower in the project, one row each.
//!
//! A row shows the target, the source and its settings, the depth and an on switch. With a
//! focus parameter (picked from a control's "Show modulators"), only its modulators are listed.

use egui::{vec2, RichText, Ui};
use gt_core::modulation::SYNC_RATES;
use gt_core::{LfoRate, LfoShape, ModSourceKind, ModulatorId, ParamId, Project, STRIPS};

use crate::GloomTheme;

/// What the editor shows.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ModulatorsState {
    /// Only list the modulators of this parameter.
    pub focus: Option<ParamId>,
}

/// Draws the editor. Returns true when a modulator changed (rebuild the engine's plan).
pub fn modulators_panel(
    ui: &mut Ui,
    theme: &GloomTheme,
    project: &mut Project,
    st: &mut ModulatorsState,
) -> bool {
    let mut changed = false;
    if let Some(f) = st.focus {
        ui.horizontal(|ui| {
            ui.label(RichText::new(f.name(project)).color(theme.text));
            if ui.small_button("Show all").clicked() {
                st.focus = None;
            }
        });
    }
    let ids: Vec<ModulatorId> = project
        .modulators
        .iter()
        .filter(|m| st.focus.is_none_or(|f| m.target == f))
        .map(|m| m.id)
        .collect();
    if ids.is_empty() {
        ui.label(
            RichText::new("No modulators. Right-click a knob or fader and pick Add LFO or Add envelope follower.")
                .color(theme.text_dim),
        );
        return false;
    }
    let names: Vec<String> = (0..STRIPS)
        .map(|i| {
            let s = &project.mixer.strips[i];
            format!("{} {}", gt_core::StripKind::of(i).short(), s.name)
        })
        .collect();
    let mut remove = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("modulators")
            .num_columns(5)
            .spacing(vec2(8.0, 4.0))
            .striped(true)
            .show(ui, |ui| {
                for id in ids {
                    let target_name = project
                        .modulators
                        .iter()
                        .find(|m| m.id == id)
                        .map(|m| m.target.name(project))
                        .unwrap_or_default();
                    let Some(m) = project.modulator_mut(id) else {
                        continue;
                    };
                    changed |= ui
                        .checkbox(&mut m.enabled, "")
                        .on_hover_text("On")
                        .changed();
                    ui.scope(|ui| {
                        ui.set_min_width(150.0);
                        ui.add(
                            egui::Label::new(RichText::new(target_name).small().color(theme.text))
                                .truncate(),
                        );
                    });
                    ui.horizontal(|ui| {
                        changed |= source_editor(ui, id, &mut m.source, &names);
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
                        .on_hover_text(
                            "Depth: 100 % moves the control over its whole travel. Negative \
                             inverts the source",
                        )
                        .changed()
                    {
                        m.amount = pct / 100.0;
                        changed = true;
                    }
                    if ui.small_button("Remove").clicked() {
                        remove = Some(id);
                    }
                    ui.end_row();
                }
            });
    });
    if let Some(id) = remove {
        project.remove_modulator(id);
        changed = true;
    }
    changed
}

/// Source kind and its settings. Returns true if anything changed.
fn source_editor(ui: &mut Ui, id: ModulatorId, src: &mut ModSourceKind, strips: &[String]) -> bool {
    let before = *src;
    egui::ComboBox::from_id_salt(("mod_kind", id.0))
        .width(64.0)
        .selected_text(src.name())
        .show_ui(ui, |ui| {
            let is_lfo = matches!(src, ModSourceKind::Lfo { .. });
            if ui.selectable_label(is_lfo, "LFO").clicked() && !is_lfo {
                *src = ModSourceKind::default_lfo();
            }
            if ui.selectable_label(!is_lfo, "Follower").clicked() && is_lfo {
                *src = ModSourceKind::default_follower(1);
            }
        });
    match src {
        ModSourceKind::Lfo { shape, rate, phase } => {
            egui::ComboBox::from_id_salt(("mod_shape", id.0))
                .width(72.0)
                .selected_text(shape.name())
                .show_ui(ui, |ui| {
                    for s in LfoShape::ALL {
                        ui.selectable_value(shape, s, s.name());
                    }
                });
            let mut synced = matches!(rate, LfoRate::Sync(_));
            if ui
                .checkbox(&mut synced, "Sync")
                .on_hover_text("Lock the LFO to the song position")
                .changed()
            {
                *rate = if synced {
                    LfoRate::Sync(5)
                } else {
                    LfoRate::Hz(2.0)
                };
            }
            match rate {
                LfoRate::Sync(i) => {
                    egui::ComboBox::from_id_salt(("mod_sync", id.0))
                        .width(60.0)
                        .selected_text(SYNC_RATES.get(*i).map_or("?", |r| r.0))
                        .show_ui(ui, |ui| {
                            for (k, (name, _)) in SYNC_RATES.iter().enumerate() {
                                ui.selectable_value(i, k, *name);
                            }
                        });
                }
                LfoRate::Hz(hz) => {
                    let (lo, hi) = LfoRate::HZ_RANGE;
                    ui.add(
                        egui::DragValue::new(hz)
                            .range(lo..=hi)
                            .speed(0.02)
                            .suffix(" Hz")
                            .max_decimals(2),
                    );
                }
            }
            let mut deg = *phase * 360.0;
            if ui
                .add(
                    egui::DragValue::new(&mut deg)
                        .range(0.0..=359.0)
                        .speed(1.0)
                        .suffix("°")
                        .fixed_decimals(0),
                )
                .on_hover_text("Start phase")
                .changed()
            {
                *phase = deg / 360.0;
            }
        }
        ModSourceKind::Follower {
            strip,
            attack_ms,
            release_ms,
            gain,
        } => {
            egui::ComboBox::from_id_salt(("mod_strip", id.0))
                .width(96.0)
                .selected_text(strips.get(*strip).map_or("?", String::as_str))
                .show_ui(ui, |ui| {
                    for (k, name) in strips.iter().enumerate() {
                        ui.selectable_value(strip, k, name);
                    }
                })
                .response
                .on_hover_text("Strip whose output level is followed");
            ui.add(
                egui::DragValue::new(attack_ms)
                    .range(0.1..=1000.0)
                    .speed(0.5)
                    .prefix("A ")
                    .suffix(" ms")
                    .max_decimals(1),
            )
            .on_hover_text("Attack");
            ui.add(
                egui::DragValue::new(release_ms)
                    .range(1.0..=5000.0)
                    .speed(2.0)
                    .prefix("R ")
                    .suffix(" ms")
                    .fixed_decimals(0),
            )
            .on_hover_text("Release");
            ui.add(
                egui::DragValue::new(gain)
                    .range(0.1..=100.0)
                    .speed(0.05)
                    .prefix("×")
                    .max_decimals(2),
            )
            .on_hover_text("Input gain: a peak of 1/gain drives the follower fully");
        }
    }
    *src != before
}
