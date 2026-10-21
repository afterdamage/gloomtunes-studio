//! Mixer view: the strips (master and up to 256 inserts) side by side, and for the selected
//! strip its routing, sends and effect slots, with an editor for the selected effect.

use egui::{pos2, vec2, Color32, RichText, Sense, Shape, Stroke, Ui};
use gt_core::effects::{EQ_BANDS, IMPULSE_RESPONSES, SLOT_MIX, STUTTER_PATTERN, STUTTER_STEPS};
use gt_core::{
    EffectKind, EffectSlot, Mixer, MixerStrip, PanLaw, ParamId, PluginOwner, StripKind, StripParam,
    FX_SLOTS, MASTER, MAX_SENDS,
};
use gt_dsp::analysis::Scope;
use gt_dsp::fx::{eq_band_response, BandType};

use crate::i18n::tr;
use crate::param_ui::{param_live, param_menu};
use crate::views::analyzer::{analyzer_panel, AnalyzerState};
use crate::views::plugins::{plugin_controls, PluginAction, PluginPanelView};
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
    /// The user chose "Plugin…" for this (strip, slot): the app opens the plugin browser.
    pub plugin_request: Option<(usize, usize)>,
    /// What the user asked of the open slot's plugin this frame.
    pub plugin_actions: Vec<PluginAction>,
    /// The user asked to choose an audio file for this (strip, slot): the app opens a picker.
    pub file_request: Option<(usize, usize)>,
    /// The open analyzer's display state.
    pub analyzer: AnalyzerState,
    /// The user asked to delete this insert (the app renumbers the project, see
    /// [`gt_core::Project::remove_insert`]).
    pub remove_request: Option<usize>,
    /// Something only the app can do (presets, track links).
    pub request: Option<MixerRequest>,
    /// The open patcher slot's editor.
    pub patcher: crate::views::PatcherState,
    /// The user chose "Plugin…" in this (strip, slot)'s patch: the app adds the plugin there.
    pub patch_plugin_request: Option<(usize, usize)>,
    /// A browser effect or plugin dropped on a strip (`slot`: on that effect slot, else the
    /// first free one); the app adds it.
    pub dropped: Option<MixerDrop>,
}

/// A browser row dropped on the mixer.
#[derive(Debug, Clone, PartialEq)]
pub struct MixerDrop {
    /// Strip index.
    pub strip: usize,
    /// Effect slot, when dropped on one.
    pub slot: Option<usize>,
    /// What was dropped.
    pub item: crate::views::RowItem,
}

/// What the mixer asks of the app.
#[derive(Debug, Clone, PartialEq)]
pub enum MixerRequest {
    /// Link playlist track `.1` to strip `.0` ([`gt_core::Project::link_track`]).
    LinkTrack(usize, gt_core::TrackId),
    /// Save strip `.0` as a strip preset (`chain`: only its effects).
    Save {
        /// Strip index.
        strip: usize,
        /// Only the effect chain.
        chain: bool,
    },
    /// Load the preset file into strip `strip` (`chain`: only its effects).
    Load {
        /// Strip index.
        strip: usize,
        /// Only the effect chain.
        chain: bool,
        /// The file.
        path: std::path::PathBuf,
    },
}

/// Icons a strip can show (an index is stored, see [`gt_core::STRIP_ICONS`]).
pub const STRIP_ICON_GLYPHS: [&str; gt_core::STRIP_ICONS as usize] = [
    "🎵", "🎸", "🎹", "🎤", "🎧", "🎻", "🎺", "🔊", "⭐", "🔥", "💧", "🌙", "⚡", "❄", "🌿", "💀",
];

impl Default for MixerState {
    fn default() -> Self {
        Self {
            selected: MASTER,
            slot: None,
            eq_band: 2,
            plugin_request: None,
            plugin_actions: Vec::new(),
            file_request: None,
            analyzer: AnalyzerState::default(),
            remove_request: None,
            request: None,
            patcher: crate::views::PatcherState::default(),
            patch_plugin_request: None,
            dropped: None,
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
    /// Status of the plugin in the open slot of the selected strip, if it holds one.
    pub plugin: Option<&'a PluginPanelView>,
    /// Device inputs an insert can take.
    pub inputs: &'a [gt_core::AudioInput],
    /// The signal tap of the open slot's effect, when it has one (the analyzer).
    pub scope: Option<&'a Scope>,
    /// Playlist tracks: id, name and the strip each plays into.
    pub tracks: &'a [(gt_core::TrackId, String, usize)],
    /// Saved strip presets (name, file).
    pub strip_presets: &'a [(String, std::path::PathBuf)],
    /// Saved effect-chain presets (name, file).
    pub chain_presets: &'a [(String, std::path::PathBuf)],
    /// The last preset save or load, to show.
    pub preset_status: Option<&'a str>,
}

/// Short routing label of a strip: "M", "3".
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
    state.selected = state.selected.min(mixer.strips.len() - 1);
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
                        for i in 0..mixer.strips.len() {
                            let meter = view.meters.get(i).copied().unwrap_or_default();
                            let track = view
                                .tracks
                                .iter()
                                .find(|t| t.2 == i && i != MASTER)
                                .map(|t| t.1.as_str());
                            let (c, clicked, rect) = strip(
                                ui,
                                theme,
                                i,
                                &mut mixer.strips[i],
                                state.selected == i,
                                meter,
                                track,
                            );
                            changed |= c;
                            if let Some(item) =
                                crate::dnd::drop_zone(ui, theme, rect, crate::dnd::is_effect)
                            {
                                state.selected = i;
                                state.dropped = Some(MixerDrop {
                                    strip: i,
                                    slot: None,
                                    item,
                                });
                            }
                            if clicked && state.selected != i {
                                state.selected = i;
                                state.slot = mixer.strips[i].slots.iter().position(Option::is_some);
                            }
                            if i == MASTER {
                                ui.add_space(6.0);
                            }
                        }
                        let full = mixer.inserts() >= gt_core::MAX_INSERTS;
                        if ui
                            .add_enabled(!full, egui::Button::new("+").min_size(vec2(24.0, 40.0)))
                            .on_hover_text(tr("Add an insert"))
                            .clicked()
                        {
                            if let Some(i) = mixer.add_insert() {
                                state.selected = i;
                                state.slot = None;
                                changed = true;
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

/// One strip. Returns (changed, clicked, its rectangle).
fn strip(
    ui: &mut Ui,
    theme: &GloomTheme,
    i: usize,
    s: &mut MixerStrip,
    selected: bool,
    meter: StripMeter,
    track: Option<&str>,
) -> (bool, bool, egui::Rect) {
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
                if let Some(c) = s.color {
                    // A bar in the frame's top margin, so coloured strips stay aligned.
                    let top = ui.max_rect();
                    let r = egui::Rect::from_min_size(
                        pos2(top.left(), top.top() - 3.0),
                        vec2(top.width(), 3.0),
                    );
                    ui.painter()
                        .rect_filled(r, 1.0, Color32::from_rgb(c[0], c[1], c[2]));
                }
                let icon = s
                    .icon
                    .and_then(|k| STRIP_ICON_GLYPHS.get(usize::from(k)))
                    .copied()
                    .unwrap_or("");
                ui.label(
                    RichText::new(format!("{icon} {}", short(i)))
                        .small()
                        .color(theme.text_dim),
                );
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
                // The linked playlist track shares the line with the effect count.
                let line = match track {
                    Some(t) if fx_count > 0 => format!("{fx_text} · {t}"),
                    Some(t) => t.to_owned(),
                    None => fx_text,
                };
                let r = ui.add(
                    egui::Label::new(RichText::new(line).small().color(theme.accent)).truncate(),
                );
                if let Some(t) = track {
                    r.on_hover_text(format!("{}: {t}", tr("Linked playlist track")));
                }
                let meter_h = (h - 150.0).max(60.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.add_space(2.0);
                    stereo_meter(ui, theme, meter.peak_db, meter.rms_db, vec2(12.0, meter_h));
                    let mut v = s.volume;
                    let r = fader(
                        ui,
                        theme,
                        &mut v,
                        MixerStrip::MAX_VOLUME,
                        vec2(30.0, meter_h),
                    );
                    param_menu(theme, &r, ParamId::strip_volume(i));
                    param_live(theme, &r, ParamId::strip_volume(i), |v| {
                        crate::widgets::fader_position(v, MixerStrip::MAX_VOLUME)
                    });
                    if r.changed() {
                        s.volume = v;
                        changed = true;
                    }
                });
                let mut pan = s.pan;
                let r = knob(ui, theme, &mut pan, -1.0..=1.0, 0.0, 22.0, |p| {
                    format!("Pan {}", format_pan(p))
                });
                param_menu(theme, &r, ParamId::strip_pan(i));
                param_live(theme, &r, ParamId::strip_pan(i), |v| (v + 1.0) * 0.5);
                if r.changed() {
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
    (changed, clicked, resp.rect)
}

/// Colour, icon, the linked playlist track and presets of strip `i`.
fn looks(
    ui: &mut Ui,
    theme: &GloomTheme,
    mixer: &mut Mixer,
    state: &mut MixerState,
    view: &MixerView<'_>,
    i: usize,
) -> bool {
    let mut changed = false;
    let s = &mut mixer.strips[i];
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.label(RichText::new(tr("Colour")).small().color(theme.text_dim));
        if ui
            .add(egui::Button::selectable(
                s.color.is_none(),
                RichText::new("–").small(),
            ))
            .on_hover_text(tr("No colour"))
            .clicked()
        {
            s.color = None;
            changed = true;
        }
        for c in gt_core::playlist::TRACK_COLORS {
            let (r, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
            ui.painter()
                .rect_filled(r.shrink(1.0), 2.0, Color32::from_rgb(c[0], c[1], c[2]));
            if s.color == Some(c) {
                ui.painter().rect_stroke(
                    r,
                    2.0,
                    Stroke::new(1.5, theme.text),
                    egui::StrokeKind::Inside,
                );
            }
            if resp.clicked() {
                s.color = Some(c);
                changed = true;
            }
        }
        ui.add_space(6.0);
        let shown = s
            .icon
            .and_then(|k| STRIP_ICON_GLYPHS.get(usize::from(k)))
            .copied()
            .unwrap_or("–");
        let mut icon = s.icon;
        egui::ComboBox::from_id_salt(("mixer_icon", i))
            .width(44.0)
            .selected_text(shown)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut icon, None, tr("None"));
                for (k, g) in STRIP_ICON_GLYPHS.iter().enumerate() {
                    ui.selectable_value(&mut icon, Some(k as u8), *g);
                }
            })
            .response
            .on_hover_text(tr("Strip icon"));
        if icon != s.icon {
            s.icon = icon;
            changed = true;
        }
    });
    if i != MASTER && !view.tracks.is_empty() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Track")).small().color(theme.text_dim))
                .on_hover_text(tr(
                    "Link a playlist track: its audio plays into this strip, and the strip \
                     takes its name and colour",
                ));
            let linked: Vec<&str> = view
                .tracks
                .iter()
                .filter(|t| t.2 == i)
                .map(|t| t.1.as_str())
                .collect();
            let shown = if linked.is_empty() {
                tr("Link a track…").to_owned()
            } else {
                linked.join(", ")
            };
            egui::ComboBox::from_id_salt(("mixer_track", i))
                .width(180.0)
                .selected_text(RichText::new(shown).small())
                .show_ui(ui, |ui| {
                    for (id, name, _) in view.tracks {
                        if ui.selectable_label(false, name).clicked() {
                            state.request = Some(MixerRequest::LinkTrack(i, *id));
                        }
                    }
                });
        });
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new(tr("Presets")).small().color(theme.text_dim));
        for (chain, label, list) in [
            (false, tr("Strip"), view.strip_presets),
            (true, tr("Effect chain"), view.chain_presets),
        ] {
            ui.menu_button(RichText::new(label).small(), |ui| {
                let tip = if chain {
                    tr("Save the ten effect slots as a preset")
                } else {
                    tr("Save the strip's name, fader, pan, colour, icon and effects (not its routing)")
                };
                if ui.button(tr("Save…")).on_hover_text(tip).clicked() {
                    state.request = Some(MixerRequest::Save { strip: i, chain });
                    ui.close();
                }
                if !list.is_empty() {
                    ui.separator();
                }
                for (name, path) in list {
                    if ui.button(name).clicked() {
                        state.request = Some(MixerRequest::Load {
                            strip: i,
                            chain,
                            path: path.clone(),
                        });
                        ui.close();
                    }
                }
            });
        }
        if let Some(st) = view.preset_status {
            ui.label(RichText::new(st).small().color(theme.text_dim));
        }
    });
    changed
}

/// The sends of insert `i`: level, pre-fader switch and removal per send, and a menu adding a
/// send to any strip it can feed without a loop.
fn sends(ui: &mut Ui, theme: &GloomTheme, mixer: &mut Mixer, i: usize) -> bool {
    let mut changed = false;
    ui.add_space(2.0);
    ui.label(RichText::new(tr("Sends")).small().color(theme.text_dim));
    let mut remove = None;
    for k in 0..mixer.strips[i].sends.len() {
        let x = mixer.strips[i].sends[k];
        let name = strip_label(mixer, x.to);
        ui.horizontal(|ui| {
            ui.push_id(("send", i, x.to), |ui| {
                let mut v = x.level;
                let r = knob(ui, theme, &mut v, 0.0..=1.0, 0.0, 22.0, |g| {
                    format!("{}: {}", tr("Send level"), format_gain(g))
                });
                let send = ParamId::Strip {
                    strip: i,
                    param: StripParam::Send(x.to),
                };
                param_menu(theme, &r, send);
                param_live(theme, &r, send, |v| v);
                if r.changed() {
                    mixer.strips[i].sends[k].level = v;
                    changed = true;
                }
                ui.add(
                    egui::Label::new(RichText::new(format!("{} {name}", tr("to"))).small().color(theme.text))
                        .truncate(),
                )
                .on_hover_text(format_gain(x.level));
                if ui
                    .add(egui::Button::selectable(x.pre, RichText::new(tr("Pre")).small()))
                    .on_hover_text(tr(
                        "Pre-fader: send what leaves the effects, before this strip's fader and pan",
                    ))
                    .clicked()
                {
                    mixer.strips[i].sends[k].pre = !x.pre;
                    changed = true;
                }
                if ui
                    .small_button("×")
                    .on_hover_text(tr("Remove this send"))
                    .clicked()
                {
                    remove = Some(k);
                }
            });
        });
    }
    if let Some(k) = remove {
        mixer.strips[i].sends.remove(k);
        changed = true;
    }
    if mixer.strips[i].sends.len() < MAX_SENDS {
        let targets: Vec<(usize, String)> = (0..mixer.strips.len())
            .filter(|&t| mixer.strips[i].sends.iter().all(|x| x.to != t) && mixer.can_send(i, t))
            .map(|t| (t, strip_label(mixer, t)))
            .collect();
        let mut pick = None;
        egui::ComboBox::from_id_salt(("mixer_add_send", i))
            .width(180.0)
            .selected_text(RichText::new(tr("Add send…")).small())
            .show_ui(ui, |ui| {
                for (t, label) in &targets {
                    if ui.selectable_label(false, label).clicked() {
                        pick = Some(*t);
                    }
                }
            });
        if let Some(t) = pick {
            changed |= mixer.add_send(i, t, 0.5, false);
        }
    }
    changed
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
            .on_hover_text(tr("Strip name"))
            .changed();
        if matches!(kind, StripKind::Insert(_))
            && mixer.inserts() > 1
            && ui
                .small_button("🗑")
                .on_hover_text(tr("Delete this insert (the ones after it move down)"))
                .clicked()
        {
            state.remove_request = Some(i);
        }
        if view.latency_ms > 0.0 && i == MASTER {
            ui.label(
                RichText::new(format!("PDC {:.1} ms", view.latency_ms))
                    .small()
                    .color(theme.text_dim),
            )
            .on_hover_text(tr("Effect latency the mixer compensates; everything reaches the output this much later"));
        }
    });

    egui::Grid::new("mixer_routing")
        .num_columns(2)
        .spacing(vec2(8.0, 4.0))
        .show(ui, |ui| {
            if i != MASTER {
                ui.label(RichText::new(tr("Output")).small().color(theme.text_dim));
                let current = mixer.strips[i].output;
                let options: Vec<(usize, String)> = (0..mixer.strips.len())
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
            ui.label(RichText::new(tr("Sidechain")).small().color(theme.text_dim))
                .on_hover_text(tr("Key signal for compressors on this strip (switch on Sidechain in the compressor)"));
            let current = mixer.strips[i].sidechain;
            let options: Vec<(usize, String)> = (1..mixer.strips.len())
                .filter(|&t| Some(t) == current || mixer.can_sidechain(i, t))
                .map(|t| (t, strip_label(mixer, t)))
                .collect();
            let mut sc = current;
            egui::ComboBox::from_id_salt(("mixer_sc", i))
                .width(180.0)
                .selected_text(current.map_or("None".to_owned(), |t| strip_label(mixer, t)))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut sc, None, tr("None"));
                    for (t, label) in &options {
                        ui.selectable_value(&mut sc, Some(*t), label);
                    }
                });
            if sc != current {
                mixer.strips[i].sidechain = sc;
                changed = true;
            }
            ui.end_row();
            if i == MASTER {
                ui.label(RichText::new(tr("Pan law")).small().color(theme.text_dim))
                    .on_hover_text(tr(
                        "Level of each side with a strip's pan knob centred: 0 dB keeps both at \
                         unity, -3 dB keeps the power constant as you pan, -6 dB is linear",
                    ));
                let mut law = mixer.pan_law;
                egui::ComboBox::from_id_salt("mixer_pan_law")
                    .width(180.0)
                    .selected_text(law.label())
                    .show_ui(ui, |ui| {
                        for l in PanLaw::ALL {
                            ui.selectable_value(&mut law, l, l.label());
                        }
                    });
                if law != mixer.pan_law {
                    mixer.pan_law = law;
                    changed = true;
                }
                ui.end_row();
            }
            if matches!(kind, StripKind::Insert(_)) {
                ui.label(RichText::new(tr("Input")).small().color(theme.text_dim))
                    .on_hover_text(
                        tr("A device input heard live through this insert and its effects (live \
                         effects). Record it by arming a playlist track that plays into this \
                         insert with the same input."),
                    );
                changed |= crate::views::input_menu(
                    ui,
                    ("mixer_in", i),
                    180.0,
                    &mut mixer.strips[i].input,
                    view.inputs,
                );
                ui.end_row();
            }
        });

    changed |= looks(ui, theme, mixer, state, &view, i);
    if matches!(kind, StripKind::Insert(_)) {
        changed |= sends(ui, theme, mixer, i);
    }

    ui.add_space(4.0);
    ui.label(
        RichText::new(tr("EFFECTS"))
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
                    .on_hover_text(tr("Edit this effect"))
                    .clicked()
                {
                    state.slot = if open { None } else { Some(k) };
                }
                let current = slot.as_ref().map(|s| s.kind);
                let mut pick = current;
                let shown = slot.as_ref().map_or("—", |s| {
                    s.plugin.as_ref().map_or(s.kind.name(), |p| p.name.as_str())
                });
                let combo = egui::ComboBox::from_id_salt(("mixer_fx", i, k))
                    .width(130.0)
                    .height(320.0)
                    .selected_text(RichText::new(shown).small())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut pick, None, tr("Empty"));
                        for kind in EffectKind::ALL {
                            ui.selectable_value(&mut pick, Some(kind), kind.name());
                        }
                        ui.selectable_value(&mut pick, Some(EffectKind::Patcher), tr("Patcher"))
                            .on_hover_text(tr("A modular patch of modules, effects and plugins"));
                        ui.separator();
                        if ui
                            .selectable_label(false, tr("Plugin…"))
                            .on_hover_text(tr("Choose a CLAP effect plugin"))
                            .clicked()
                        {
                            state.plugin_request = Some((i, k));
                        }
                    });
                if let Some(item) =
                    crate::dnd::drop_zone(ui, theme, combo.response.rect, crate::dnd::is_effect)
                {
                    state.dropped = Some(MixerDrop {
                        strip: i,
                        slot: Some(k),
                        item,
                    });
                }
                if pick != current {
                    *slot = pick.map(EffectSlot::new);
                    state.slot = pick.map(|_| k);
                    changed = true;
                }
                if let Some(s) = slot.as_mut() {
                    let text = RichText::new(if s.enabled { "On" } else { "Off" }).small();
                    if ui
                        .add(egui::Button::selectable(s.enabled, text).min_size(vec2(28.0, 16.0)))
                        .on_hover_text(tr("Bypass"))
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
            let title = slot
                .plugin
                .as_ref()
                .map_or(slot.kind.name(), |p| p.name.as_str())
                .to_owned();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{title} · slot {}", k + 1))
                        .strong()
                        .color(theme.accent),
                );
                let presets = slot.kind.presets();
                if !presets.is_empty() {
                    egui::ComboBox::from_id_salt(("fx_preset", i, k))
                        .width(110.0)
                        .selected_text(RichText::new(tr("Presets")).small())
                        .show_ui(ui, |ui| {
                            for preset in presets {
                                if ui.selectable_label(false, preset.name).clicked() {
                                    slot.apply_preset(preset);
                                    changed = true;
                                }
                            }
                        });
                }
            });
            // Every slot's own wet/dry mix, lined up with the effect's latency in the engine.
            changed |= param_knob(
                ui,
                theme,
                (i, k, usize::MAX),
                &SLOT_MIX,
                &mut slot.mix,
                None,
            );
            changed |= if let Some(p) = slot.plugin.as_deref_mut() {
                let owner = PluginOwner::Effect { strip: i, slot: k };
                let missing = PluginPanelView::default();
                let pv = view.plugin.unwrap_or(&missing);
                plugin_controls(ui, theme, owner, p, pv, &mut state.plugin_actions)
            } else if slot.kind == EffectKind::Plugin {
                false
            } else if let Some(patch) = slot.patch.as_deref_mut() {
                let missing = PluginPanelView::default();
                let pv = view.plugin.unwrap_or(&missing);
                let mut acts = Vec::new();
                let c = crate::views::patcher_view(
                    ui,
                    theme,
                    crate::views::PatchPlace::Effect { strip: i, slot: k },
                    patch,
                    &mut state.patcher,
                    pv,
                    &mut acts,
                );
                for a in acts {
                    match a {
                        crate::views::PatcherAction::AddPlugin => {
                            state.patch_plugin_request = Some((i, k));
                        }
                        crate::views::PatcherAction::Plugin(pa) => state.plugin_actions.push(pa),
                    }
                }
                c
            } else if slot.kind == EffectKind::Eq {
                eq_editor(
                    ui,
                    theme,
                    (i, k),
                    slot,
                    &mut state.eq_band,
                    view.sample_rate,
                )
            } else if slot.kind == EffectKind::Analyzer {
                if let Some(scope) = view.scope {
                    let slope = slot.params[0];
                    let hold = slot.params[1] >= 0.5;
                    analyzer_panel(ui, theme, scope, slope, hold, &mut state.analyzer);
                }
                knobs(ui, theme, (i, k), slot, 0..slot.kind.params().len())
            } else if slot.kind == EffectKind::Stutter {
                let steps = step_editor(ui, theme, slot);
                steps
                    | knobs(
                        ui,
                        theme,
                        (i, k),
                        slot,
                        STUTTER_PATTERN..slot.kind.params().len(),
                    )
            } else {
                let mut c = knobs(ui, theme, (i, k), slot, 0..slot.kind.params().len());
                if slot.kind.uses_file() {
                    c |= file_row(ui, theme, slot, || state.file_request = Some((i, k)));
                }
                c
            };
        }
    }
    changed
}

/// The convolution reverb's own response: the file in use and a button to choose one.
fn file_row(
    ui: &mut Ui,
    theme: &GloomTheme,
    slot: &mut EffectSlot,
    mut pick: impl FnMut(),
) -> bool {
    let user = (IMPULSE_RESPONSES.len() - 1) as f32;
    let mut changed = false;
    ui.horizontal(|ui| {
        if ui
            .small_button(tr("Load response…"))
            .on_hover_text(tr(
                "Use your own impulse response (a WAV, FLAC, MP3 or OGG file)",
            ))
            .clicked()
        {
            pick();
        }
        match &slot.file {
            Some(f) => {
                let name = f
                    .file_name()
                    .map_or_else(|| f.display().to_string(), |n| n.to_string_lossy().into());
                let using = slot.get("ir") == Some(user);
                let text = RichText::new(name).small().color(if using {
                    theme.text
                } else {
                    theme.text_dim
                });
                ui.label(text).on_hover_text(f.display().to_string());
                if !using && ui.small_button(tr("Use")).clicked() {
                    *slot = slot.clone().with("ir", user);
                    changed = true;
                }
            }
            None => {
                ui.label(
                    RichText::new(tr("built-in responses only"))
                        .small()
                        .color(theme.text_dim),
                );
            }
        }
    });
    changed
}

/// The stutter's 16 steps: click cycles off, on, repeat and half; right-click turns a step off.
fn step_editor(ui: &mut Ui, theme: &GloomTheme, slot: &mut EffectSlot) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for step in 0..STUTTER_PATTERN {
            let v = slot.params[step].round() as usize;
            let (fill, label) = match v {
                0 => (theme.bg_deep, ""),
                2 => (theme.accent_dim, "R"),
                3 => (theme.accent_dim, "½"),
                _ => (theme.accent, ""),
            };
            let (rect, resp) = ui.allocate_exact_size(vec2(19.0, 24.0), Sense::click());
            let p = ui.painter_at(rect);
            // A gap every four steps marks the beats.
            let r = rect.shrink2(vec2(if step % 4 == 3 { 2.0 } else { 0.5 }, 0.0));
            p.rect_filled(r, theme.radius, fill);
            p.rect_stroke(
                r,
                theme.radius,
                Stroke::new(1.0, theme.stroke),
                egui::StrokeKind::Inside,
            );
            p.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(11.0),
                theme.text,
            );
            let resp = resp.on_hover_text(format!(
                "Step {}: {}. Click to change, right-click for off.",
                step + 1,
                STUTTER_STEPS[v.min(STUTTER_STEPS.len() - 1)]
            ));
            if resp.clicked() {
                slot.params[step] = ((v + 1) % STUTTER_STEPS.len()) as f32;
                changed = true;
            } else if resp.secondary_clicked() {
                slot.params[step] = 0.0;
                changed = true;
            }
        }
    });
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
            let target = ParamId::Effect {
                strip: id.0,
                slot: id.1,
                kind: slot.kind,
                index: p,
            };
            changed |= param_knob(
                ui,
                theme,
                (id.0, id.1, p),
                &table[p],
                &mut slot.params[p],
                Some(target),
            );
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
