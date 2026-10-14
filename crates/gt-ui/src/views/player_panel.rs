//! The preset player's panel (Prompt 28): pick a sound pack and one of its presets, then shape
//! it with the preset's macro knobs.

use std::path::PathBuf;

use egui::{RichText, Ui};
use gt_core::{MacroTarget, Player};

use crate::i18n::tr;
use crate::widgets::labeled_knob;
use crate::GloomTheme;

/// What the panel shows besides the player.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlayerPanelView<'a> {
    /// Packs found in the packs folders (name, path).
    pub packs: &'a [(String, PathBuf)],
    /// Presets of the player's pack, when it opened.
    pub presets: Option<&'a [String]>,
    /// Why the pack did not open, if it did not.
    pub error: Option<&'a str>,
}

/// What the panel asks of the app.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerPanelOut {
    /// A macro moved (send the channel's sound again).
    pub changed: bool,
    /// Load this pack's first preset.
    pub pack: Option<PathBuf>,
    /// Load this preset of the current pack.
    pub preset: Option<String>,
    /// Pick a pack file.
    pub open: bool,
}

/// The panel.
pub fn player_panel(
    ui: &mut Ui,
    theme: &GloomTheme,
    name: &mut String,
    p: &mut Player,
    view: PlayerPanelView<'_>,
) -> PlayerPanelOut {
    let mut out = PlayerPanelOut::default();
    ui.horizontal(|ui| {
        ui.label(RichText::new(tr("Player")).strong().color(theme.accent));
        ui.add(egui::TextEdit::singleline(name).desired_width(140.0))
            .on_hover_text(tr("Channel name"));
        ui.separator();
        ui.label(RichText::new(tr("Pack")).color(theme.text_dim));
        let current = p
            .pack
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| tr("none").to_owned());
        egui::ComboBox::from_id_salt("player-pack")
            .selected_text(current)
            .width(160.0)
            .show_ui(ui, |ui| {
                for (n, path) in view.packs {
                    if ui.selectable_label(*path == p.pack, n.as_str()).clicked() {
                        out.pack = Some(path.clone());
                    }
                }
            });
        if ui
            .button(tr("Open…"))
            .on_hover_text(tr(
                "Load a sound pack (.gtpack: a zip of samples and pack.json)",
            ))
            .clicked()
        {
            out.open = true;
        }
        ui.separator();
        ui.label(RichText::new(tr("Preset")).color(theme.text_dim));
        let presets = view.presets.unwrap_or_default();
        let at = presets.iter().position(|n| *n == p.preset);
        egui::ComboBox::from_id_salt("player-preset")
            .selected_text(p.preset.as_str())
            .width(180.0)
            .show_ui(ui, |ui| {
                for n in presets {
                    if ui.selectable_label(*n == p.preset, n.as_str()).clicked() {
                        out.preset = Some(n.clone());
                    }
                }
            });
        let step = |d: isize| {
            let i = at.map_or(0, |i| (i as isize + d).rem_euclid(presets.len() as isize));
            presets.get(i as usize).cloned()
        };
        if !presets.is_empty() {
            if ui
                .small_button("◀")
                .on_hover_text(tr("Previous preset"))
                .clicked()
            {
                out.preset = step(-1);
            }
            if ui
                .small_button("▶")
                .on_hover_text(tr("Next preset"))
                .clicked()
            {
                out.preset = step(1);
            }
        }
    });
    if let Some(e) = view.error {
        ui.label(RichText::new(e).color(theme.warn));
    }
    if p.macros.is_empty() {
        ui.label(RichText::new(tr("This preset has no macros")).color(theme.text_dim));
        return out;
    }
    ui.horizontal(|ui| {
        for (i, m) in p.macros.iter_mut().enumerate() {
            ui.push_id(i, |ui| {
                let (target, min, max) = (m.target, m.min, m.max);
                let shown = move |v: f32| {
                    let probe = gt_core::Macro {
                        name: String::new(),
                        target,
                        min,
                        max,
                        value: v,
                    };
                    format_target(target, probe.target_value())
                };
                out.changed |=
                    labeled_knob(ui, theme, &m.name, &mut m.value, 0.0..=1.0, 0.5, shown)
                        .on_hover_text(tr(target_label(target)))
                        .changed();
            });
        }
    });
    out
}

fn target_label(t: MacroTarget) -> &'static str {
    match t {
        MacroTarget::Cutoff => "Filter cutoff",
        MacroTarget::Resonance => "Filter resonance",
        MacroTarget::FilterEnv => "Filter envelope amount",
        MacroTarget::Attack => "Attack",
        MacroTarget::Decay => "Decay",
        MacroTarget::Sustain => "Sustain",
        MacroTarget::Release => "Release",
        MacroTarget::Tune => "Tune",
        MacroTarget::Volume => "Volume",
        MacroTarget::LfoRate => "LFO rate",
        MacroTarget::LfoDepth => "LFO depth",
        MacroTarget::Start => "Sample start",
    }
}

fn format_target(t: MacroTarget, v: f32) -> String {
    match t {
        MacroTarget::Cutoff | MacroTarget::LfoRate if v >= 1000.0 => {
            format!("{:.1} kHz", v / 1000.0)
        }
        MacroTarget::Cutoff => format!("{v:.0} Hz"),
        MacroTarget::LfoRate => format!("{v:.2} Hz"),
        MacroTarget::Attack | MacroTarget::Decay | MacroTarget::Release => format!("{v:.0} ms"),
        MacroTarget::Tune => format!("{v:+.1} st"),
        MacroTarget::FilterEnv => format!("{v:+.1} oct"),
        MacroTarget::Resonance
        | MacroTarget::Sustain
        | MacroTarget::Volume
        | MacroTarget::LfoDepth
        | MacroTarget::Start => format!("{:.0} %", v * 100.0),
    }
}
