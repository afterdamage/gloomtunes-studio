//! The panel of a MIDI out channel (Prompt 33): which port and MIDI channel its notes go to,
//! the program it selects and how early it sends.

use egui::{Grid, RichText, Ui};

use crate::i18n::tr;
use crate::GloomTheme;
use gt_core::MidiOutSettings;

/// Draws the panel. `ports` are the output ports found; a saved port that is missing stays
/// selectable. True when anything changed.
pub fn midi_out_panel(
    ui: &mut Ui,
    theme: &GloomTheme,
    name: &mut String,
    m: &mut MidiOutSettings,
    ports: &[String],
) -> bool {
    let mut changed = false;
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    ui.horizontal(|ui| {
        ui.label(RichText::new(tr("MIDI out")).strong().color(theme.accent));
        changed |= ui
            .add(egui::TextEdit::singleline(name).desired_width(140.0))
            .on_hover_text(tr("Channel name"))
            .changed();
    });
    Grid::new("midi_out_panel")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(dim(tr("Port")));
            let none = tr("Not connected");
            let shown = if m.port.is_empty() {
                none
            } else {
                m.port.as_str()
            };
            egui::ComboBox::from_id_salt("midi_out_port")
                .width(220.0)
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    changed |= ui
                        .selectable_value(&mut m.port, String::new(), none)
                        .changed();
                    for p in ports {
                        changed |= ui.selectable_value(&mut m.port, p.clone(), p).changed();
                    }
                });
            if !m.port.is_empty() && !ports.contains(&m.port) {
                ui.label(RichText::new(tr("not found")).color(theme.warn));
            }
            ui.end_row();
            ui.label(dim(tr("MIDI channel")));
            let mut ch = m.channel + 1;
            if ui
                .add(egui::DragValue::new(&mut ch).range(1..=16))
                .changed()
            {
                m.channel = ch - 1;
                changed = true;
            }
            ui.end_row();
            ui.label(dim(tr("Program")));
            changed |= optional(ui, &mut m.program, 1, 128);
            ui.end_row();
            ui.label(dim(tr("Bank")));
            changed |= optional(ui, &mut m.bank, 1, 16_384);
            ui.end_row();
            ui.label(dim(tr("Device latency")));
            changed |= ui
                .add(
                    egui::DragValue::new(&mut m.latency_ms)
                        .range(0.0..=MidiOutSettings::MAX_LATENCY_MS)
                        .speed(0.2)
                        .suffix(tr(" ms")),
                )
                .on_hover_text(tr(
                    "How late the device sounds after a note reaches it: notes go out this \
                     much earlier, so they line up with the audio",
                ))
                .changed();
            ui.end_row();
        });
    ui.label(dim(tr(
        "Notes, recorded controllers and live playing go to the device; record its audio \
         back through an audio input.",
    )));
    changed
}

/// A number shown from `base` (1 for programs and banks), or off. True when changed.
fn optional<T>(ui: &mut Ui, v: &mut Option<T>, base: u32, max: u32) -> bool
where
    T: Copy + TryFrom<u32> + Into<u32>,
{
    let mut changed = false;
    ui.horizontal(|ui| {
        let mut on = v.is_some();
        if ui.checkbox(&mut on, "").changed() {
            *v = if on { T::try_from(0).ok() } else { None };
            changed = true;
        }
        if let Some(x) = v.as_mut() {
            let mut shown = (*x).into() + base;
            if ui
                .add(egui::DragValue::new(&mut shown).range(base..=max))
                .changed()
            {
                if let Ok(n) = T::try_from(shown - base) {
                    *x = n;
                    changed = true;
                }
            }
        }
    });
    changed
}
