//! The modulators editor: every LFO and envelope follower in the project, one row each.
//!
//! A row shows the modulator's number (`m3`, what formulas read), the target, the source and its
//! settings, the depth and an on switch. With a focus parameter (picked from a control's "Show
//! modulators"), only its modulators are listed. The ＋ menu of a row adds a modulator to its
//! depth or rate (Prompt 34, D151).

use egui::{vec2, RichText, Ui};
use gt_core::formula::Program;
use gt_core::modulation::{DEFAULT_FORMULA, SYNC_RATES};
use gt_core::{
    LfoRate, LfoShape, MidiControl, ModField, ModSourceKind, ModulatorId, ParamId, Project, STRIPS,
};

use crate::i18n::tr;
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
            if ui.small_button(tr("Show all")).clicked() {
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
            RichText::new(tr("No modulators. Right-click a knob or fader and pick Add LFO or Add envelope follower."))
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
    let mut add: Option<(ParamId, ModSourceKind)> = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("modulators")
            .num_columns(7)
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
                    let mut formula = project.formulas.get(&id).cloned();
                    let Some(m) = project.modulator_mut(id) else {
                        continue;
                    };
                    changed |= ui
                        .checkbox(&mut m.enabled, "")
                        .on_hover_text(tr("On"))
                        .changed();
                    ui.label(
                        RichText::new(format!("m{}", id.0))
                            .small()
                            .color(theme.text_dim),
                    )
                    .on_hover_text(tr("This modulator's name in formulas"));
                    ui.scope(|ui| {
                        ui.set_min_width(150.0);
                        ui.add(
                            egui::Label::new(RichText::new(target_name).small().color(theme.text))
                                .truncate(),
                        );
                    });
                    let has_rate = m.source.has_rate();
                    ui.horizontal(|ui| {
                        let was = m.source;
                        changed |= source_editor(ui, id, &mut m.source, &names);
                        if m.source == ModSourceKind::Formula {
                            if was != m.source && formula.is_none() {
                                formula = Some(DEFAULT_FORMULA.to_owned());
                            }
                            if let Some(text) = formula.as_mut() {
                                changed |= formula_editor(ui, theme, text);
                            }
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
                        .on_hover_text(tr(
                            "Depth: 100 % moves the control over its whole travel. Negative \
                             inverts the source",
                        ))
                        .changed()
                    {
                        m.amount = pct / 100.0;
                        changed = true;
                    }
                    ui.menu_button("＋", |ui| {
                        for (field, label) in [
                            (ModField::Amount, tr("Modulate depth with")),
                            (ModField::Rate, tr("Modulate rate with")),
                        ] {
                            if field == ModField::Rate && !has_rate {
                                continue;
                            }
                            ui.menu_button(label, |ui| {
                                for (kind, name) in [
                                    (ModSourceKind::default_lfo(), tr("LFO")),
                                    (ModSourceKind::default_follower(1), tr("Follower")),
                                    (ModSourceKind::Formula, tr("Formula")),
                                ] {
                                    if ui.button(name).clicked() {
                                        add = Some((ParamId::Modulator { id, field }, kind));
                                        ui.close();
                                    }
                                }
                            });
                        }
                    })
                    .response
                    .on_hover_text(tr("Add a modulator to this modulator's depth or rate"));
                    if ui.small_button(tr("Remove")).clicked() {
                        remove = Some(id);
                    }
                    match formula {
                        Some(text) if project.modulators.iter().any(|m| m.id == id) => {
                            project.formulas.insert(id, text);
                        }
                        _ => {}
                    }
                    ui.end_row();
                }
            });
    });
    if let Some(id) = remove {
        project.remove_modulator(id);
        changed = true;
    }
    if let Some((target, kind)) = add {
        changed |= project.add_modulator(target, kind).is_some();
    }
    changed
}

/// The text of a formula, with its error when it does not compile. Returns true on a change.
fn formula_editor(ui: &mut Ui, theme: &GloomTheme, text: &mut String) -> bool {
    let r = ui
        .add(
            egui::TextEdit::singleline(text)
                .desired_width(200.0)
                .font(egui::TextStyle::Monospace),
        )
        .on_hover_text(tr(
            "Formula, -1 to 1: t (seconds), beat, bpm, wheel, touch, pi, m1 m2 … (other \
             modulators); + - * / % ^ < >; sin cos (radians), tri saw sqr (cycles), abs floor \
             frac sqrt exp log sign hash, min max pow step, clamp lerp if",
        ));
    if let Err(e) = Program::compile(text) {
        ui.label(RichText::new("⚠").color(theme.warn))
            .on_hover_text(e.to_string());
    }
    r.changed()
}

/// Source kind and its settings. Returns true if anything changed.
fn source_editor(ui: &mut Ui, id: ModulatorId, src: &mut ModSourceKind, strips: &[String]) -> bool {
    let before = *src;
    egui::ComboBox::from_id_salt(("mod_kind", id.0))
        .width(64.0)
        .selected_text(src.name())
        .show_ui(ui, |ui| {
            let is_lfo = matches!(src, ModSourceKind::Lfo { .. });
            let is_fol = matches!(src, ModSourceKind::Follower { .. });
            let is_midi = matches!(src, ModSourceKind::Midi { .. });
            let is_formula = *src == ModSourceKind::Formula;
            if ui.selectable_label(is_lfo, tr("LFO")).clicked() && !is_lfo {
                *src = ModSourceKind::default_lfo();
            }
            if ui.selectable_label(is_fol, tr("Follower")).clicked() && !is_fol {
                *src = ModSourceKind::default_follower(1);
            }
            if ui
                .selectable_label(is_midi, tr("MIDI"))
                .on_hover_text(tr("Mod wheel or aftertouch of the target's channel"))
                .clicked()
                && !is_midi
            {
                *src = ModSourceKind::Midi {
                    control: MidiControl::ModWheel,
                };
            }
            if ui
                .selectable_label(is_formula, tr("Formula"))
                .on_hover_text(tr("An expression of time, the song and other modulators"))
                .clicked()
                && !is_formula
            {
                *src = ModSourceKind::Formula;
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
                .checkbox(&mut synced, tr("Sync"))
                .on_hover_text(tr("Lock the LFO to the song position"))
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
                            .suffix(tr(" Hz"))
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
                .on_hover_text(tr("Start phase"))
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
                .on_hover_text(tr("Strip whose output level is followed"));
            ui.add(
                egui::DragValue::new(attack_ms)
                    .range(0.1..=1000.0)
                    .speed(0.5)
                    .prefix("A ")
                    .suffix(tr(" ms"))
                    .max_decimals(1),
            )
            .on_hover_text(tr("Attack"));
            ui.add(
                egui::DragValue::new(release_ms)
                    .range(1.0..=5000.0)
                    .speed(2.0)
                    .prefix("R ")
                    .suffix(tr(" ms"))
                    .fixed_decimals(0),
            )
            .on_hover_text(tr("Release"));
            ui.add(
                egui::DragValue::new(gain)
                    .range(0.1..=100.0)
                    .speed(0.05)
                    .prefix("×")
                    .max_decimals(2),
            )
            .on_hover_text(tr("Input gain: a peak of 1/gain drives the follower fully"));
        }
        ModSourceKind::Midi { control } => {
            egui::ComboBox::from_id_salt(("mod_midi", id.0))
                .width(96.0)
                .selected_text(control.name())
                .show_ui(ui, |ui| {
                    for c in MidiControl::ALL {
                        ui.selectable_value(control, c, c.name());
                    }
                })
                .response
                .on_hover_text(tr(
                    "From the target's own channel; mixer and effect targets follow the \
                     selected channel",
                ));
        }
        ModSourceKind::Formula => {}
    }
    *src != before
}
