//! The drum pad instrument's panel (Prompt 23): a 4×4 grid of the shown bank's pads (click to
//! play and select), bank tabs, and the selected pad's layers, tuning, gain, pan, envelope,
//! choke group and output.

use egui::{vec2, Color32, RichText, Sense, Stroke, Ui};
use gt_core::pads::{CHOKE_GROUPS, MAX_BANKS, PADS_PER_BANK};
use gt_core::{DrumKit, Pad, INSERTS};

use crate::i18n::tr;
use crate::widgets::labeled_knob;
use crate::GloomTheme;

/// Which bank and pad the panel shows.
#[derive(Debug, Clone, Copy, Default)]
pub struct DrumPadState {
    /// Shown bank.
    pub bank: usize,
    /// Selected pad in it.
    pub pad: usize,
}

/// What the panel asks of the app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrumPadOut {
    /// A pad setting changed (send the kit again).
    pub changed: bool,
    /// Play this key now (a pad was clicked).
    pub audition: Option<u8>,
    /// Pick a sample for the selected pad (true: add it as another velocity layer).
    pub load: Option<bool>,
    /// Pick a folder to fill the pads from the selected one on.
    pub load_folder: bool,
}

/// Draws the panel.
pub fn drum_pads(
    ui: &mut Ui,
    theme: &GloomTheme,
    name: &mut String,
    kit: &mut DrumKit,
    st: &mut DrumPadState,
) -> DrumPadOut {
    let mut out = DrumPadOut::default();
    st.bank = st.bank.min(kit.banks.len().saturating_sub(1));
    st.pad = st.pad.min(PADS_PER_BANK - 1);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.add(egui::TextEdit::singleline(name).desired_width(150.0))
                .on_hover_text(tr("Channel name"));
            ui.horizontal(|ui| {
                for b in 0..kit.banks.len() {
                    let label = format!("Bank {}", (b'A' + b as u8) as char);
                    if ui.add(egui::Button::selectable(st.bank == b, label)).clicked() {
                        st.bank = b;
                    }
                }
                if kit.banks.len() < MAX_BANKS
                    && ui.small_button("+").on_hover_text(tr("Add a bank of 16 pads")).clicked()
                {
                    kit.banks.push(vec![Pad::default(); PADS_PER_BANK]);
                    st.bank = kit.banks.len() - 1;
                    out.changed = true;
                }
            });
            if ui
                .button(tr("Fill from folder…"))
                .on_hover_text(tr("Put a folder's samples on the pads, from the selected pad on (you can also drop files here)"))
                .clicked()
            {
                out.load_folder = true;
            }
            ui.label(
                RichText::new(tr("Steps in the channel rack play pad 1 of bank A; the piano roll plays every pad."))
                    .color(theme.text_dim)
                    .small(),
            );
        });
        pad_grid(ui, theme, kit, st, &mut out);
        ui.separator();
        pad_editor(ui, theme, &mut kit.banks[st.bank][st.pad], &mut out);
    });
    out
}

fn pad_grid(
    ui: &mut Ui,
    theme: &GloomTheme,
    kit: &DrumKit,
    st: &mut DrumPadState,
    out: &mut DrumPadOut,
) {
    let size = vec2(58.0, 34.0);
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
        // Pad 1 at the bottom left, like a hardware pad grid.
        for row in (0..4).rev() {
            ui.horizontal(|ui| {
                for col in 0..4 {
                    let p = row * 4 + col;
                    let pad = &kit.banks[st.bank][p];
                    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                    let selected = st.pad == p;
                    let fill = if pad.layers.is_empty() {
                        theme.bg_deep
                    } else {
                        theme
                            .accent
                            .gamma_multiply(if selected { 0.55 } else { 0.25 })
                    };
                    let painter = ui.painter_at(rect);
                    painter.rect_filled(rect, f32::from(theme.radius), fill);
                    let stroke = if selected {
                        Stroke::new(1.5, theme.accent)
                    } else {
                        Stroke::new(1.0, theme.text_dim.gamma_multiply(0.3))
                    };
                    painter.rect_stroke(
                        rect,
                        f32::from(theme.radius),
                        stroke,
                        egui::StrokeKind::Inside,
                    );
                    let mut label = pad.label();
                    if label.chars().count() > 9 {
                        label = label.chars().take(8).collect::<String>() + "…";
                    }
                    painter.text(
                        rect.left_top() + vec2(4.0, 3.0),
                        egui::Align2::LEFT_TOP,
                        format!("{}", p + 1),
                        egui::FontId::proportional(9.0),
                        theme.text_dim,
                    );
                    painter.text(
                        rect.center() + vec2(0.0, 5.0),
                        egui::Align2::CENTER_CENTER,
                        label,
                        egui::FontId::proportional(10.0),
                        if pad.layers.is_empty() {
                            theme.text_dim
                        } else {
                            theme.text
                        },
                    );
                    if pad.choke > 0 {
                        painter.text(
                            rect.right_top() + vec2(-4.0, 3.0),
                            egui::Align2::RIGHT_TOP,
                            format!("C{}", pad.choke),
                            egui::FontId::proportional(9.0),
                            Color32::from_rgb(220, 150, 80),
                        );
                    }
                    if resp.clicked() {
                        st.pad = p;
                        out.audition = Some(DrumKit::key(st.bank, p));
                    }
                }
            });
        }
    });
}

fn pad_editor(ui: &mut Ui, theme: &GloomTheme, pad: &mut Pad, out: &mut DrumPadOut) {
    let dim = |t: &str| RichText::new(t).color(theme.text_dim);
    ui.vertical(|ui| {
        ui.set_width(200.0);
        let hint = pad.label();
        out.changed |= ui
            .add(
                egui::TextEdit::singleline(&mut pad.name)
                    .hint_text(hint)
                    .desired_width(180.0),
            )
            .on_hover_text(tr("Pad name"))
            .changed();
        let mut remove = None;
        for (i, l) in pad.layers.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(l.sample.display_name()).color(theme.text))
                    .on_hover_text(format!(
                        "Velocity {:.0} to {:.0} %",
                        l.vel_lo * 100.0,
                        l.vel_hi * 100.0
                    ));
                if ui
                    .small_button("✕")
                    .on_hover_text(tr("Remove this layer"))
                    .clicked()
                {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            let rest: Vec<_> = pad
                .layers
                .iter()
                .enumerate()
                .filter(|(k, _)| *k != i)
                .map(|(_, l)| l.sample.clone())
                .collect();
            pad.set_layers(rest);
            out.changed = true;
        }
        if pad.layers.is_empty() {
            ui.label(dim(tr("Empty: load a sample")));
        }
        ui.horizontal(|ui| {
            if ui
                .button(tr("Load…"))
                .on_hover_text(tr("Replace the pad's sample"))
                .clicked()
            {
                out.load = Some(false);
            }
            if !pad.layers.is_empty()
                && pad.layers.len() < 8
                && ui
                    .button(tr("+ Layer…"))
                    .on_hover_text(tr(
                        "Add a velocity layer (the range splits evenly, softest first)",
                    ))
                    .clicked()
            {
                out.load = Some(true);
            }
        });
        ui.horizontal(|ui| {
            ui.label(dim(tr("Choke")));
            egui::ComboBox::from_id_salt("pad_choke")
                .width(60.0)
                .selected_text(if pad.choke == 0 {
                    "None".to_owned()
                } else {
                    format!("Group {}", pad.choke)
                })
                .show_ui(ui, |ui| {
                    out.changed |= ui.selectable_value(&mut pad.choke, 0, tr("None")).changed();
                    for g in 1..=CHOKE_GROUPS {
                        out.changed |= ui
                            .selectable_value(&mut pad.choke, g, format!("Group {g}"))
                            .changed();
                    }
                })
                .response
                .on_hover_text(tr(
                    "A pad cuts the sounding pads of its group (open and closed hats)",
                ));
        });
        ui.horizontal(|ui| {
            ui.label(dim(tr("Output")));
            egui::ComboBox::from_id_salt("pad_output")
                .width(80.0)
                .selected_text(
                    pad.output
                        .map_or("Channel".to_owned(), |o| format!("Insert {o}")),
                )
                .show_ui(ui, |ui| {
                    out.changed |= ui
                        .selectable_value(&mut pad.output, None, tr("Channel"))
                        .changed();
                    for o in 1..=INSERTS {
                        out.changed |= ui
                            .selectable_value(&mut pad.output, Some(o), format!("Insert {o}"))
                            .changed();
                    }
                })
                .response
                .on_hover_text(tr(
                    "Send this pad to its own mixer insert (at the channel's volume)",
                ));
        });
    });
    out.changed |= labeled_knob(ui, theme, "Tune", &mut pad.tune, -24.0..=24.0, 0.0, |v| {
        format!("Tune {v:+.2} semitones")
    })
    .changed();
    out.changed |= labeled_knob(ui, theme, "Gain", &mut pad.gain, 0.0..=2.0, 1.0, |v| {
        format!("Gain {:+.1} dB", 20.0 * v.max(1e-6).log10())
    })
    .changed();
    out.changed |= labeled_knob(ui, theme, "Pan", &mut pad.pan, -1.0..=1.0, 0.0, |v| {
        format!("Pan {:+.0} %", v * 100.0)
    })
    .changed();
    let a = &mut pad.adsr;
    out.changed |= labeled_knob(
        ui,
        theme,
        "Attack",
        &mut a.attack_ms,
        0.0..=500.0,
        0.0,
        |v| format!("Attack {v:.0} ms"),
    )
    .changed();
    out.changed |= labeled_knob(
        ui,
        theme,
        "Decay",
        &mut a.decay_ms,
        0.0..=2000.0,
        0.0,
        |v| {
            if v == 0.0 {
                "Decay off".to_owned()
            } else {
                format!("Decay {v:.0} ms")
            }
        },
    )
    .changed();
    out.changed |= labeled_knob(ui, theme, "Sustain", &mut a.sustain, 0.0..=1.0, 1.0, |v| {
        format!("Sustain {:.0} %", v * 100.0)
    })
    .changed();
    out.changed |= labeled_knob(
        ui,
        theme,
        "Release",
        &mut a.release_ms,
        0.0..=2000.0,
        30.0,
        |v| format!("Release {v:.0} ms"),
    )
    .changed();
}
