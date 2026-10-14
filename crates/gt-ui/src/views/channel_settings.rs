//! The channel settings tab (Prompt 27): pitch bend, swing, choke group, rack group and the
//! key settings (root, range, mono, voices) of every instrument, and the children of a layer
//! channel.

use egui::{RichText, Ui};
use gt_core::{
    Channel, ChannelId, Instrument, KeyLimits, LayerMode, LayerSettings, SamplerSettings,
};

use crate::i18n::tr;
use crate::GloomTheme;

/// What the tab changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChannelSettingsOut {
    /// A setting the engine uses changed (send the channel's settings).
    pub changed: bool,
    /// The swing changed (the song is compiled again).
    pub swing: bool,
}

/// Draws the tab for `ch`. `rack_swing` is the swing a channel follows by default; `groups`
/// are the rack groups in use, offered for quick picking.
pub fn channel_settings(
    ui: &mut Ui,
    theme: &GloomTheme,
    ch: &mut Channel,
    rack_swing: f32,
    groups: &[String],
) -> ChannelSettingsOut {
    let mut out = ChannelSettingsOut::default();
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    ui.horizontal_wrapped(|ui| {
        ui.label(dim(tr("Pitch bend")));
        out.changed |= ui
            .add(
                egui::DragValue::new(&mut ch.bend_range)
                    .range(0.0..=Channel::MAX_BEND)
                    .speed(0.1)
                    .fixed_decimals(0)
                    .prefix("±")
                    .suffix(tr(" st")),
            )
            .on_hover_text(tr(
                "How far the pitch wheel bends this instrument, in semitones \
                 (plugins set their own range)",
            ))
            .changed();
        ui.separator();
        let mut own = ch.swing.is_some();
        if ui
            .checkbox(&mut own, tr("Own swing"))
            .on_hover_text(tr("Off: the channel follows the rack's swing"))
            .changed()
        {
            ch.swing = own.then_some(rack_swing);
            out.swing = true;
        }
        let mut v = ch.swing_or(rack_swing) * 100.0;
        let r = ui.add_enabled(
            own,
            egui::DragValue::new(&mut v)
                .range(0.0..=100.0)
                .speed(0.5)
                .fixed_decimals(0)
                .suffix(" %"),
        );
        if r.changed() {
            ch.swing = Some(v / 100.0);
            out.swing = true;
        }
        ui.separator();
        ui.label(dim(tr("Choke")));
        let name = |g: u8| {
            if g == 0 {
                tr("None").to_owned()
            } else {
                g.to_string()
            }
        };
        egui::ComboBox::from_id_salt("choke_group")
            .width(50.0)
            .selected_text(name(ch.choke))
            .show_ui(ui, |ui| {
                for g in 0..=Channel::CHOKE_GROUPS {
                    out.changed |= ui.selectable_value(&mut ch.choke, g, name(g)).changed();
                }
            })
            .response
            .on_hover_text(tr(
                "Choke group: a note on this channel cuts the other channels of its group \
                 (open and closed hats)",
            ));
        ui.separator();
        ui.label(dim(tr("Group")));
        out.changed |= ui
            .add(
                egui::TextEdit::singleline(&mut ch.group)
                    .desired_width(80.0)
                    .hint_text(tr("none")),
            )
            .on_hover_text(tr(
                "Rack group: the rack's filter can show one group at a time",
            ))
            .changed();
        if !groups.is_empty() {
            ui.menu_button("▾", |ui| {
                for g in groups {
                    if ui.button(g).clicked() {
                        ch.group.clone_from(g);
                        out.changed = true;
                        ui.close();
                    }
                }
            });
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(dim(tr("Keys")));
        match &mut ch.instrument {
            Instrument::Sampler(s) => out.changed |= sampler_keys(ui, s),
            Instrument::Layer(_) => {
                ui.label(dim(tr("A layer plays its children's own key settings")));
            }
            Instrument::Player(_) => {
                ui.label(dim(tr("The pack's preset sets the keys")));
            }
            _ => out.changed |= key_limits(ui, &mut ch.keys),
        }
    });
    out
}

/// The sampler keeps its own key settings (its engine applies them per voice).
fn sampler_keys(ui: &mut Ui, s: &mut SamplerSettings) -> bool {
    let mut changed = false;
    if s.zones.is_empty() {
        changed |= root_and_range(ui, &mut s.root_key, &mut s.key_lo, &mut s.key_hi);
    } else {
        ui.label(tr("(zones set the keys)"));
    }
    ui.separator();
    changed |= ui.checkbox(&mut s.mono, tr("Mono")).changed();
    if s.mono {
        changed |= ui
            .checkbox(&mut s.legato, tr("Legato"))
            .on_hover_text(tr(
                "A key pressed while another is held glides without restarting",
            ))
            .changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut s.glide_ms)
                    .range(0.0..=2000.0)
                    .speed(1.0)
                    .prefix(tr("Glide "))
                    .suffix(tr(" ms")),
            )
            .on_hover_text(tr("Mono glide time between notes"))
            .changed();
    }
    ui.label(tr("Voices"));
    changed |= ui
        .add(egui::DragValue::new(&mut s.voices).range(1..=SamplerSettings::MAX_VOICES))
        .on_hover_text(tr("Most notes sounding at once; the oldest is cut"))
        .changed();
    changed
}

/// Key settings applied to the notes of any other instrument.
fn key_limits(ui: &mut Ui, k: &mut KeyLimits) -> bool {
    let mut changed = root_and_range(ui, &mut k.root, &mut k.lo, &mut k.hi);
    ui.separator();
    changed |= ui
        .checkbox(&mut k.mono, tr("Mono"))
        .on_hover_text(tr("One note at a time: a new note ends the one before"))
        .changed();
    ui.label(tr("Voices"));
    changed |= ui
        .add(
            egui::DragValue::new(&mut k.voices)
                .range(0..=KeyLimits::MAX_VOICES)
                .custom_formatter(|v, _| {
                    if v < 1.0 {
                        tr("any").to_owned()
                    } else {
                        format!("{v:.0}")
                    }
                }),
        )
        .on_hover_text(tr("Most notes held at once; the oldest ends"))
        .changed();
    k.sanitize();
    changed
}

fn root_and_range(ui: &mut Ui, root: &mut u8, lo: &mut u8, hi: &mut u8) -> bool {
    ui.label(tr("Root"));
    let mut changed = key_drag(ui, root, tr("Key that plays at the instrument's own pitch"));
    ui.label(tr("Range"));
    changed |= key_drag(ui, lo, tr("Lowest key that plays"));
    changed |= key_drag(ui, hi, tr("Highest key that plays"));
    *hi = (*hi).max(*lo);
    changed
}

fn key_drag(ui: &mut Ui, key: &mut u8, tip: &str) -> bool {
    ui.add(
        egui::DragValue::new(key)
            .range(0..=127)
            .custom_formatter(|v, _| crate::views::key_name(v as u8))
            .speed(0.3),
    )
    .on_hover_text(tip)
    .changed()
}

/// The instrument tab of a layer channel: which channels it plays and how. `channels` are the
/// rack's other channels that can be children (id, name).
pub fn layer_panel(
    ui: &mut Ui,
    theme: &GloomTheme,
    name: &mut String,
    l: &mut LayerSettings,
    channels: &[(ChannelId, String)],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(tr("Layer")).strong().color(theme.accent));
        ui.add(egui::TextEdit::singleline(name).desired_width(140.0))
            .on_hover_text(tr("Channel name"));
        ui.separator();
        ui.label(RichText::new(tr("Mode")).color(theme.text_dim));
        for m in LayerMode::ALL {
            changed |= ui.selectable_value(&mut l.mode, m, tr(m.label())).changed();
        }
        ui.separator();
        changed |= ui
            .checkbox(&mut l.split, tr("Velocity split"))
            .on_hover_text(tr(
                "Each child answers its share of the velocity range, in order (soft to hard)",
            ))
            .changed();
    });
    ui.label(
        RichText::new(tr(
            "Plays (one note on this channel plays each ticked channel):",
        ))
        .color(theme.text_dim),
    );
    ui.horizontal_wrapped(|ui| {
        for (id, n) in channels {
            let mut on = l.children.contains(id);
            let full = l.children.len() >= LayerSettings::MAX_CHILDREN;
            if ui
                .add_enabled(on || !full, egui::Checkbox::new(&mut on, n.as_str()))
                .changed()
            {
                if on {
                    l.children.push(*id);
                } else {
                    l.children.retain(|c| c != id);
                }
                changed = true;
            }
        }
        if channels.is_empty() {
            ui.label(
                RichText::new(tr("Add instrument channels to layer them")).color(theme.text_dim),
            );
        }
    });
    changed
}
