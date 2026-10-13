//! Settings of the selected sampler channel: sample, waveform with start/end, pitch, loop mode
//! and the ADSR envelope; Sampler 2 (Prompt 23): quality, reverse, keys, voices, glide,
//! time-stretch, multi-sample zones, the filter with its envelope and two LFOs.

use egui::{pos2, vec2, RichText, Sense, Stroke, Ui};
use gt_core::{
    ChannelId, ChannelParam, Interp, LfoShape, LfoTarget, LoopMode, ParamId, SamplerFilterMode,
    SamplerSettings,
};

use crate::param_ui::{param_live, param_menu};
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
    /// The channel, so knobs get the automation and modulation menu.
    pub channel: Option<ChannelId>,
}

/// What the panel changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SamplerPanelOut {
    /// A sound setting changed (send the channel's settings).
    pub changed: bool,
    /// What the channel plays changed: zones or time-stretch (send its samples again).
    pub reload: bool,
    /// The user asked to load a folder as a multi-sample instrument.
    pub load_folder: bool,
}

/// Draws the panel. Returns true when a sound parameter changed.
pub fn sampler_panel(
    ui: &mut Ui,
    theme: &GloomTheme,
    name: &mut String,
    s: &mut SamplerSettings,
    view: SamplerPanelView<'_>,
) -> SamplerPanelOut {
    let mut out = SamplerPanelOut::default();
    let mut changed = false;
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    let menu = |r: egui::Response, param: ChannelParam| {
        if let Some(channel) = view.channel {
            let id = ParamId::Channel { channel, param };
            param_menu(theme, &r, id);
            param_live(theme, &r, id, |v| param.info().to_normalized(v));
        }
        r.changed()
    };
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width(170.0);
            ui.add(egui::TextEdit::singleline(name).desired_width(160.0))
                .on_hover_text("Channel name");
            let sample = s.sample.as_ref().map_or_else(
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
                    let on = s.loop_mode == mode;
                    if ui
                        .add(egui::Button::selectable(on, label))
                        .on_hover_text(tip)
                        .clicked()
                        && !on
                    {
                        s.loop_mode = mode;
                        changed = true;
                    }
                }
            });
        });

        waveform(ui, theme, s, view.waveform);

        changed |= menu(
            labeled_knob(ui, theme, "Pitch", &mut s.pitch, -24.0..=24.0, 0.0, |v| {
                format!("{v:+.2} semitones")
            }),
            ChannelParam::Pitch,
        );
        changed |= menu(
            labeled_knob(ui, theme, "Start", &mut s.start, 0.0..=1.0, 0.0, |v| {
                format!("Start {:.1} %", v * 100.0)
            }),
            ChannelParam::Start,
        );
        changed |= menu(
            labeled_knob(ui, theme, "End", &mut s.end, 0.0..=1.0, 1.0, |v| {
                format!("End {:.1} %", v * 100.0)
            }),
            ChannelParam::End,
        );
        if s.end <= s.start {
            s.end = (s.start + 0.001).min(1.0);
            s.start = s.start.min(s.end - 0.001);
        }
        ui.separator();
        let a = &mut s.adsr;
        changed |= menu(
            time_knob(ui, theme, "Attack", &mut a.attack_ms, MAX_AD_MS, 0.0),
            ChannelParam::Attack,
        );
        changed |= menu(
            time_knob(ui, theme, "Decay", &mut a.decay_ms, MAX_AD_MS, 0.0),
            ChannelParam::Decay,
        );
        changed |= menu(
            labeled_knob(ui, theme, "Sustain", &mut a.sustain, 0.0..=1.0, 1.0, |v| {
                format!("Sustain {:.0} %", v * 100.0)
            }),
            ChannelParam::Sustain,
        );
        changed |= menu(
            time_knob(ui, theme, "Release", &mut a.release_ms, MAX_R_MS, 50.0),
            ChannelParam::Release,
        );
    });
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        changed |= playback_row(ui, theme, s, &mut out);
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        changed |= filter_row(ui, theme, s, &menu);
        ui.separator();
        for (i, l) in s.lfos.iter_mut().enumerate() {
            changed |= lfo_ui(ui, theme, i, l);
            ui.separator();
        }
    });
    out.changed |= changed;
    out
}

/// Quality, reverse, keys, voices, glide, time-stretch and zones.
fn playback_row(
    ui: &mut Ui,
    theme: &GloomTheme,
    s: &mut SamplerSettings,
    out: &mut SamplerPanelOut,
) -> bool {
    let mut changed = false;
    let dim = |t: &str| RichText::new(t).color(theme.text_dim);
    egui::ComboBox::from_id_salt("sampler_interp")
        .width(110.0)
        .selected_text(s.interp.name())
        .show_ui(ui, |ui| {
            for q in Interp::ALL {
                changed |= ui.selectable_value(&mut s.interp, q, q.name()).changed();
            }
        })
        .response
        .on_hover_text("Interpolation: sinc keeps pitched-up samples free of aliasing");
    changed |= ui
        .add(egui::Button::selectable(s.reverse, "Reverse"))
        .on_hover_text("Play the sample backwards")
        .clicked()
        .then(|| s.reverse = !s.reverse)
        .is_some();
    ui.separator();
    if s.zones.is_empty() {
        ui.label(dim("Root"));
        changed |= key_drag(
            ui,
            &mut s.root_key,
            "Key at which the sample plays at its own pitch",
        );
        ui.label(dim("Keys"));
        changed |= key_drag(ui, &mut s.key_lo, "Lowest key the sample answers");
        changed |= key_drag(ui, &mut s.key_hi, "Highest key the sample answers");
        s.key_hi = s.key_hi.max(s.key_lo);
        let mut stretch = s.stretch_beats > 0.0;
        if ui
            .checkbox(&mut stretch, "Stretch")
            .on_hover_text("Time-stretch mode: fit the sample to a number of beats at the song tempo, at its own pitch")
            .changed()
        {
            s.stretch_beats = if stretch { 4.0 } else { 0.0 };
            out.reload = true;
        }
        if stretch
            && ui
                .add(
                    egui::DragValue::new(&mut s.stretch_beats)
                        .range(0.25..=256.0)
                        .speed(0.05)
                        .suffix(" beats"),
                )
                .changed()
        {
            out.reload = true;
        }
    } else {
        ui.label(RichText::new(format!("{} zones", s.zones.len())).color(theme.text))
            .on_hover_text(zone_summary(s));
        changed |= ui
            .checkbox(&mut s.round_robin, "Round robin")
            .on_hover_text(
                "Zones answering the same key and velocity take turns instead of layering",
            )
            .changed();
        if ui
            .small_button("Clear zones")
            .on_hover_text("Back to the single sample")
            .clicked()
        {
            s.zones.clear();
            out.reload = true;
        }
    }
    if ui
        .button("Load folder…")
        .on_hover_text("Make a multi-sample instrument from a folder: roots from note names in the file names, velocity layers from pp/mf/ff or v1, v2 (you can also drop several files here)")
        .clicked()
    {
        out.load_folder = true;
    }
    ui.separator();
    ui.label(dim("Voices"));
    changed |= ui
        .add(egui::DragValue::new(&mut s.voices).range(1..=SamplerSettings::MAX_VOICES))
        .on_hover_text("Most notes sounding at once; the oldest is cut")
        .changed();
    changed |= ui.checkbox(&mut s.mono, "Mono").changed();
    if s.mono {
        changed |= ui
            .checkbox(&mut s.legato, "Legato")
            .on_hover_text("A key pressed while another is held glides without restarting")
            .changed();
    }
    changed |= ui
        .add(
            egui::DragValue::new(&mut s.glide_ms)
                .range(0.0..=2000.0)
                .speed(1.0)
                .prefix("Glide ")
                .suffix(" ms"),
        )
        .on_hover_text("Mono glide time between notes")
        .changed();
    changed
}

fn zone_summary(s: &SamplerSettings) -> String {
    s.zones
        .iter()
        .take(24)
        .map(|z| {
            format!(
                "{}: {}..{} (root {}), velocity {:.0}..{:.0} %",
                z.sample.display_name(),
                crate::views::key_name(z.key_lo),
                crate::views::key_name(z.key_hi),
                crate::views::key_name(z.root),
                z.vel_lo * 100.0,
                z.vel_hi * 100.0
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
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

/// Filter mode, cutoff, resonance, envelope amount and the filter envelope.
fn filter_row(
    ui: &mut Ui,
    theme: &GloomTheme,
    s: &mut SamplerSettings,
    menu: &dyn Fn(egui::Response, ChannelParam) -> bool,
) -> bool {
    let mut changed = false;
    let f = &mut s.filter;
    ui.vertical(|ui| {
        ui.label(RichText::new("Filter").color(theme.text_dim));
        egui::ComboBox::from_id_salt("sampler_filter")
            .width(80.0)
            .selected_text(f.mode.name())
            .show_ui(ui, |ui| {
                for m in SamplerFilterMode::ALL {
                    changed |= ui.selectable_value(&mut f.mode, m, m.name()).changed();
                }
            });
    });
    if f.mode == SamplerFilterMode::Off {
        return changed;
    }
    let mut oct = (f.cutoff / 20.0).log2() / 10.0;
    let r = labeled_knob(
        ui,
        theme,
        "Cutoff",
        &mut oct,
        0.0..=1.0,
        (8000.0_f32 / 20.0).log2() / 10.0,
        |t| {
            let hz = 20.0 * (t * 10.0).exp2();
            if hz >= 1000.0 {
                format!("Cutoff {:.2} kHz", hz / 1000.0)
            } else {
                format!("Cutoff {hz:.0} Hz")
            }
        },
    );
    if r.changed() {
        f.cutoff = (20.0 * (oct * 10.0).exp2()).clamp(20.0, 20_000.0);
    }
    changed |= menu(r, ChannelParam::Cutoff);
    changed |= menu(
        labeled_knob(ui, theme, "Res", &mut f.resonance, 0.0..=1.0, 0.2, |v| {
            format!("Resonance {:.0} %", v * 100.0)
        }),
        ChannelParam::Resonance,
    );
    changed |= labeled_knob(ui, theme, "Env", &mut f.env_amount, -8.0..=8.0, 0.0, |v| {
        format!("Envelope {v:+.1} octaves")
    })
    .changed();
    let e = &mut f.env;
    changed |= time_knob(ui, theme, "F.Att", &mut e.attack_ms, MAX_AD_MS, 0.0).changed();
    changed |= time_knob(ui, theme, "F.Dec", &mut e.decay_ms, MAX_AD_MS, 300.0).changed();
    changed |= labeled_knob(ui, theme, "F.Sus", &mut e.sustain, 0.0..=1.0, 0.0, |v| {
        format!("Filter sustain {:.0} %", v * 100.0)
    })
    .changed();
    changed |= time_knob(ui, theme, "F.Rel", &mut e.release_ms, MAX_R_MS, 200.0).changed();
    changed
}

fn lfo_ui(ui: &mut Ui, theme: &GloomTheme, i: usize, l: &mut gt_core::SamplerLfo) -> bool {
    let mut changed = false;
    ui.vertical(|ui| {
        ui.label(RichText::new(format!("LFO {}", i + 1)).color(theme.text_dim));
        egui::ComboBox::from_id_salt(("sampler_lfo_target", i))
            .width(70.0)
            .selected_text(l.target.name())
            .show_ui(ui, |ui| {
                for t in LfoTarget::ALL {
                    changed |= ui.selectable_value(&mut l.target, t, t.name()).changed();
                }
            });
        if l.target != LfoTarget::Off {
            egui::ComboBox::from_id_salt(("sampler_lfo_shape", i))
                .width(70.0)
                .selected_text(l.shape.name())
                .show_ui(ui, |ui| {
                    for sh in LfoShape::ALL {
                        changed |= ui.selectable_value(&mut l.shape, sh, sh.name()).changed();
                    }
                });
        }
    });
    if l.target != LfoTarget::Off {
        let mut t = (l.rate / 0.05).log2() / 10.0;
        let r = labeled_knob(
            ui,
            theme,
            "Rate",
            &mut t,
            0.0..=1.0,
            (4.0_f32 / 0.05).log2() / 10.0,
            |t| format!("Rate {:.2} Hz", 0.05 * (t * 10.0).exp2()),
        );
        if r.changed() {
            l.rate = (0.05 * (t * 10.0).exp2()).clamp(0.01, 50.0);
            changed = true;
        }
        changed |= labeled_knob(ui, theme, "Depth", &mut l.amount, 0.0..=1.0, 0.25, |v| {
            format!("Depth {:.0} %", v * 100.0)
        })
        .changed();
    }
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
) -> egui::Response {
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
    }
    r
}

fn waveform(ui: &mut Ui, theme: &GloomTheme, s: &SamplerSettings, cols: Option<&[(f32, f32)]>) {
    let (rect, _) = ui.allocate_exact_size(vec2(260.0, 64.0), Sense::hover());
    let p = ui.painter_at(rect);
    let radius = f32::from(theme.radius);
    p.rect_filled(rect, radius, theme.bg_deep);
    let Some(cols) = cols.filter(|c| !c.is_empty()) else {
        return;
    };
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
