//! Channel rack: the pattern bar and one row per channel with mute, solo, volume, pan and the
//! step grid. Prompt 27: patterns of any length (the grid scrolls), a graph lane under the
//! selected channel (velocity, pan, release, fine pitch, mod X/Y, shift), rack filter groups,
//! pattern colours, split by channel and the step tools in the channel menu.

use egui::{pos2, vec2, Rect, RichText, Sense, Stroke, Ui};
use gt_core::rack::{self, StepLane};
use gt_core::{Channel, ChannelId, ChannelParam, ParamId, Pattern, Project};

use crate::i18n::tr;
use crate::param_ui::{param_live, param_menu};
use crate::widgets::{format_gain, format_pan, knob};
use crate::GloomTheme;

/// Velocity change per point of mouse-wheel scroll.
const WHEEL_VELOCITY: f32 = 0.002;

/// Width of a step cell.
const STEP_W: f32 = 16.0;
/// Gap between step cells, and the extra gap after each beat.
const STEP_GAP: f32 = 2.0;
const BEAT_GAP: f32 = 3.0;
/// Height of the graph lane.
const LANE_H: f32 = 44.0;

/// UI state of the rack that is not part of the document.
#[derive(Debug, Clone)]
pub struct RackState {
    /// Selected channel (index into `Project::channels`); its settings show below the rack.
    pub selected: usize,
    /// Text of the pattern name while renaming.
    renaming: Option<String>,
    /// Channel being auditioned with the mouse.
    audition: Option<usize>,
    /// Rack filter group shown (`None`: every channel).
    pub filter: Option<String>,
    /// Graph lane shown under the selected channel.
    pub lane: Option<StepLane>,
    /// Randomize: share of steps on, and the seed of the next roll.
    pub density: f32,
    /// Seed of the next randomize.
    pub seed: u64,
    /// Show the channel settings tab instead of the instrument (the panel below the rack).
    pub settings_tab: bool,
}

impl Default for RackState {
    fn default() -> Self {
        Self {
            selected: 0,
            renaming: None,
            audition: None,
            filter: None,
            lane: None,
            density: 0.5,
            seed: 1,
            settings_tab: false,
        }
    }
}

/// Left edge of step `step`'s cell, from the grid's left edge.
fn step_x(step: u16) -> f32 {
    f32::from(step) * (STEP_W + STEP_GAP) + f32::from(step / 4) * BEAT_GAP
}

/// The step under `x` (from the grid's left edge), if any of `steps`.
fn step_at(x: f32, steps: u16) -> Option<u16> {
    let beat = STEP_W * 4.0 + STEP_GAP * 4.0 + BEAT_GAP;
    let b = (x / beat).floor().max(0.0);
    let s = b * 4.0
        + ((x - b * beat) / (STEP_W + STEP_GAP))
            .floor()
            .clamp(0.0, 3.0);
    (s >= 0.0 && s < f32::from(steps)).then_some(s as u16)
}

/// The rack groups in use, sorted.
pub fn rack_groups(project: &Project) -> Vec<String> {
    let mut g: Vec<String> = project
        .channels
        .iter()
        .filter(|c| !c.group.is_empty())
        .map(|c| c.group.clone())
        .collect();
    g.sort();
    g.dedup();
    g
}

/// What changed, so the app can update the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RackAction {
    /// Notes, swing, pattern length or the current pattern changed.
    SongChanged,
    /// Channel volume, pan, mute or solo changed.
    ParamsChanged,
    /// A channel was added or removed.
    ChannelsChanged,
    /// Start auditioning a channel.
    NoteOn(usize),
    /// Stop auditioning a channel.
    NoteOff(usize),
    /// Open a channel in the piano roll.
    OpenPianoRoll(usize),
    /// Open the plugin browser to add an instrument plugin.
    AddPlugin,
    /// The current pattern's colour changed (an edit, nothing for the engine).
    PatternColor,
    /// A browser row was dropped on channel `channel` (replace or fill it), or on the rack
    /// below the channels (`None`: a new channel).
    Drop {
        /// The channel it landed on.
        channel: Option<usize>,
        /// What was dropped.
        item: crate::views::RowItem,
    },
}

/// Read-only playback information for the rack.
#[derive(Debug, Clone, Copy, Default)]
pub struct RackView<'a> {
    /// Step under the playhead while playing.
    pub playhead_step: Option<u16>,
    /// Activity level per channel, 0 to 1 (UI-smoothed peaks).
    pub activity: &'a [f32],
}

/// Draws the rack and edits `project` in place.
pub fn channel_rack(
    ui: &mut Ui,
    theme: &GloomTheme,
    project: &mut Project,
    state: &mut RackState,
    view: RackView<'_>,
) -> Vec<RackAction> {
    let mut actions = Vec::new();
    pattern_bar(ui, theme, project, state, &mut actions);
    ui.add_space(6.0);

    let mut remove = None;
    let mut add = None;
    let steps = project.current_pattern().steps;
    let inserts = project.mixer.inserts();
    let silenced: Vec<bool> = (0..project.channels.len())
        .map(|i| project.is_silenced(i))
        .collect();
    let names: Vec<(ChannelId, String)> = project
        .channels
        .iter()
        .filter(|c| c.layer().is_none())
        .map(|c| (c.id, c.name.clone()))
        .collect();
    let (channels, patterns, current) = (
        &mut project.channels,
        &mut project.patterns,
        project.current_pattern,
    );
    let pattern = patterns
        .iter_mut()
        .find(|p| p.id == current)
        .expect("current pattern exists");
    let mut rows_rect = egui::Rect::NOTHING;
    egui::ScrollArea::both()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (i, ch) in channels.iter_mut().enumerate() {
                if state.filter.as_ref().is_some_and(|g| *g != ch.group) {
                    continue;
                }
                let row = RowCtx {
                    index: i,
                    selected: state.selected == i,
                    silenced: silenced[i],
                    activity: view.activity.get(i).copied().unwrap_or(0.0),
                    steps,
                    playhead: view.playhead_step,
                    others: &names,
                    inserts,
                };
                let line = ui.horizontal(|ui| {
                    channel_row(
                        ui,
                        theme,
                        ch,
                        pattern,
                        state,
                        &row,
                        &mut actions,
                        &mut remove,
                    )
                });
                let grid_x = line.inner;
                rows_rect = rows_rect.union(line.response.rect);
                let accepts = |it: &crate::views::RowItem| {
                    crate::dnd::is_instrument(it) || crate::dnd::is_effect(it)
                };
                if let Some(item) = crate::dnd::drop_zone(ui, theme, line.response.rect, accepts)
                {
                    state.selected = i;
                    actions.push(RackAction::Drop {
                        channel: Some(i),
                        item,
                    });
                }
                if let (true, Some(lane)) = (row.selected, state.lane) {
                    ui.horizontal(|ui| {
                        ui.add_space((grid_x - ui.cursor().left()).max(0.0));
                        if lane_graph(ui, theme, ch.id, pattern, lane, steps) {
                            actions.push(RackAction::SongChanged);
                        }
                    });
                }
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .button(tr("+ Sampler"))
                    .on_hover_text(tr("Add an empty sampler channel; load a sample from the browser"))
                    .clicked()
                {
                    add = Some(0);
                }
                if ui
                    .button(tr("+ Drum pads"))
                    .on_hover_text(tr("Add a drum pad channel: banks of 16 pads with layers, choke groups and outputs"))
                    .clicked()
                {
                    add = Some(2);
                }
                if ui
                    .button(tr("+ Drum synth"))
                    .on_hover_text(tr("Add a drum synth: kick, snare, hats, clap, toms and percussion made from oscillators and noise, every sound editable"))
                    .clicked()
                {
                    add = Some(6);
                }
                if ui
                    .button(tr("+ Gloom Synth"))
                    .on_hover_text(tr("Add a Gloom Synth channel"))
                    .clicked()
                {
                    add = Some(1);
                }
                if ui
                    .button(tr("+ FM"))
                    .on_hover_text(tr("Add a six-operator FM synth channel"))
                    .clicked()
                {
                    add = Some(3);
                }
                if ui
                    .button(tr("+ Wavetable"))
                    .on_hover_text(tr("Add a wavetable synth channel"))
                    .clicked()
                {
                    add = Some(4);
                }
                if ui
                    .button(tr("+ Bass"))
                    .on_hover_text(tr("Add a mono bass-line synth: resonant filter, accent from velocity, slide from overlapping notes"))
                    .clicked()
                {
                    add = Some(7);
                }
                if ui
                    .button(tr("+ Player"))
                    .on_hover_text(tr("Add a preset player: plays a sound pack's multi-sampled presets with eight macro knobs"))
                    .clicked()
                {
                    add = Some(8);
                }
                if ui
                    .button(tr("+ Layer"))
                    .on_hover_text(tr("Add a layer channel: one note plays several channels"))
                    .clicked()
                {
                    add = Some(5);
                }
                if ui
                    .button(tr("+ Patcher"))
                    .on_hover_text(tr("Add a modular patcher: wire oscillators, filters, envelopes, effects and plugins freely"))
                    .clicked()
                {
                    add = Some(10);
                }
                if ui
                    .button(tr("+ MIDI out"))
                    .on_hover_text(tr("Add a MIDI out channel: its notes play an external synth or drum machine"))
                    .clicked()
                {
                    add = Some(9);
                }
                if ui
                    .button(tr("+ Plugin"))
                    .on_hover_text(tr("Add a channel playing a CLAP instrument plugin"))
                    .clicked()
                {
                    actions.push(RackAction::AddPlugin);
                }
            });
        });
    // Anywhere else in the rack: a new channel.
    let over_row = ui
        .input(|i| i.pointer.hover_pos())
        .is_some_and(|p| rows_rect.contains(p));
    let new_channel = |it: &crate::views::RowItem| !over_row && crate::dnd::is_instrument(it);
    if let Some(item) = crate::dnd::drop_zone(ui, theme, ui.max_rect(), new_channel) {
        actions.push(RackAction::Drop {
            channel: None,
            item,
        });
    }
    if let Some(kind) = add {
        let n = project.channels.len() + 1;
        let added = match kind {
            1 => project.add_synth_channel(&format!("Synth {n}"), gt_core::SynthPatch::default()),
            2 => project.add_drum_channel(&format!("Drums {n}")),
            5 => project.add_layer_channel(&format!("Layer {n}")),
            9 => project.add_midi_out_channel(&format!("MIDI out {n}")),
            10 => project.add_patcher_channel(&format!("Patcher {n}")),
            6 => project.add_drum_synth_channel(&format!("Drum synth {n}")),
            8 => project.add_player_channel(&format!("Player {n}"), gt_core::Player::default()),
            3 | 4 | 7 => {
                // The engine's first factory preset.
                let engine = if kind == 7 { 3 } else { kind - 2 };
                let patch = gt_core::SynthPatch::factory()
                    .into_iter()
                    .find(|p| p.engine() == engine)
                    .unwrap_or_default();
                let name = ["Synth", "FM", "Wavetable", "Bass"][engine as usize];
                project.add_synth_channel(&format!("{name} {n}"), patch)
            }
            _ => project.add_channel(&format!("Sampler {n}"), None),
        };
        if added.is_some() {
            state.selected = project.channels.len() - 1;
            actions.push(RackAction::ChannelsChanged);
        }
    }
    if let Some(i) = remove {
        let id = project.channels[i].id;
        project.remove_channel(id);
        state.selected = state.selected.min(project.channels.len().saturating_sub(1));
        actions.push(RackAction::ChannelsChanged);
        actions.push(RackAction::SongChanged);
    }
    key_commands(ui, project, state, &mut actions);
    actions
}

/// The rack's own keys (Prompt 25; remappable in the shortcut editor).
fn key_commands(
    ui: &Ui,
    project: &mut Project,
    state: &mut RackState,
    actions: &mut Vec<RackAction>,
) {
    use crate::keymap::{Command, Scope};
    let keys = crate::keymap::take_view(ui, Scope::Rack);
    let n = project.channels.len();
    for c in keys {
        match c {
            Command::RackUp => state.selected = state.selected.saturating_sub(1),
            Command::RackDown if n > 0 => state.selected = (state.selected + 1).min(n - 1),
            Command::RackMute | Command::RackSolo => {
                if let Some(ch) = project.channels.get_mut(state.selected) {
                    if c == Command::RackMute {
                        ch.mute = !ch.mute;
                    } else {
                        ch.solo = !ch.solo;
                    }
                    actions.push(RackAction::ParamsChanged);
                }
            }
            Command::RackNextPattern | Command::RackPrevPattern => {
                let ids: Vec<_> = project.patterns.iter().map(|p| p.id).collect();
                let at = ids
                    .iter()
                    .position(|&id| id == project.current_pattern)
                    .unwrap_or(0);
                let to = if c == Command::RackNextPattern {
                    (at + 1).min(ids.len() - 1)
                } else {
                    at.saturating_sub(1)
                };
                if to != at {
                    project.select_pattern(ids[to]);
                    actions.push(RackAction::SongChanged);
                }
            }
            Command::RackOpenRoll if state.selected < n => {
                actions.push(RackAction::OpenPianoRoll(state.selected));
            }
            _ => {}
        }
    }
}

struct RowCtx<'a> {
    index: usize,
    selected: bool,
    silenced: bool,
    activity: f32,
    steps: u16,
    playhead: Option<u16>,
    /// Channels that can take a copy of the step row (not layers).
    others: &'a [(ChannelId, String)],
    /// Inserts in the mixer.
    inserts: usize,
}

#[allow(clippy::too_many_arguments)]
fn channel_row(
    ui: &mut Ui,
    theme: &GloomTheme,
    ch: &mut Channel,
    pattern: &mut Pattern,
    state: &mut RackState,
    row: &RowCtx<'_>,
    actions: &mut Vec<RackAction>,
    remove: &mut Option<usize>,
) -> f32 {
    let i = row.index;

    // Activity light.
    let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
    let lit = row.activity.clamp(0.0, 1.0);
    let colour = theme.bg_widget.lerp_to_gamma(theme.accent, lit);
    ui.painter().circle_filled(r.center(), 3.5, colour);

    let mute = ui
        .add(egui::Button::selectable(ch.mute, "M").min_size(vec2(18.0, 18.0)))
        .on_hover_text(tr("Mute"));
    if mute.clicked() {
        ch.mute = !ch.mute;
        actions.push(RackAction::ParamsChanged);
    }
    let solo = ui
        .add(egui::Button::selectable(ch.solo, "S").min_size(vec2(18.0, 18.0)))
        .on_hover_text(tr("Solo (only soloed channels play)"));
    if solo.clicked() {
        ch.solo = !ch.solo;
        actions.push(RackAction::ParamsChanged);
    }

    let mut vol = ch.volume;
    let r = knob(
        ui,
        theme,
        &mut vol,
        0.0..=Channel::MAX_VOLUME,
        Channel::DEFAULT_VOLUME,
        20.0,
        |v| format!("Volume {}", format_gain(v)),
    );
    let target = |param| ParamId::Channel {
        channel: ch.id,
        param,
    };
    param_menu(theme, &r, target(ChannelParam::Volume));
    param_live(theme, &r, target(ChannelParam::Volume), |v| {
        v / Channel::MAX_VOLUME
    });
    if r.changed() {
        ch.volume = vol;
        actions.push(RackAction::ParamsChanged);
    }
    let mut pan = ch.pan;
    let r = knob(ui, theme, &mut pan, -1.0..=1.0, 0.0, 20.0, |p| {
        format!("Pan {}", format_pan(p))
    });
    param_menu(theme, &r, target(ChannelParam::Pan));
    param_live(theme, &r, target(ChannelParam::Pan), |v| (v + 1.0) * 0.5);
    if r.changed() {
        ch.pan = pan;
        actions.push(RackAction::ParamsChanged);
    }
    // Mixer insert the channel plays into ("M" for the master).
    let mut insert = ch.insert;
    if ui
        .add_sized(
            vec2(24.0, 18.0),
            egui::DragValue::new(&mut insert)
                .range(0..=row.inserts)
                .speed(0.1)
                .custom_formatter(|v, _| {
                    if v < 0.5 {
                        "M".to_owned()
                    } else {
                        format!("{v:.0}")
                    }
                }),
        )
        .on_hover_text(tr("Mixer insert (drag or type; M = master)"))
        .changed()
    {
        ch.insert = insert;
        actions.push(RackAction::ParamsChanged);
    }

    // Name: click selects, hold to audition, right-click for the menu.
    let text_colour = if row.silenced {
        theme.text_dim
    } else {
        theme.text
    };
    let name = ui
        .add(
            egui::Button::selectable(row.selected, RichText::new(&ch.name).color(text_colour))
                .min_size(vec2(96.0, 20.0))
                .truncate(),
        )
        .on_hover_text(tr("Click to select, hold to hear, right-click for options"));
    if name.clicked() || name.drag_started() {
        state.selected = i;
    }
    let down = name.is_pointer_button_down_on();
    if down && state.audition.is_none() {
        state.audition = Some(i);
        actions.push(RackAction::NoteOn(i));
    } else if !down && state.audition == Some(i) {
        state.audition = None;
        actions.push(RackAction::NoteOff(i));
    }
    name.context_menu(|ui| {
        if ui.button(tr("Open in piano roll")).clicked() {
            state.selected = i;
            actions.push(RackAction::OpenPianoRoll(i));
            ui.close();
        }
        ui.menu_button(tr("Graph lane"), |ui| {
            if ui
                .selectable_label(state.lane.is_none(), tr("Hidden"))
                .clicked()
            {
                state.lane = None;
                ui.close();
            }
            for l in StepLane::ALL {
                if ui
                    .selectable_label(state.lane == Some(l), tr(l.label()))
                    .clicked()
                {
                    state.selected = i;
                    state.lane = Some(l);
                    ui.close();
                }
            }
        });
        if step_tools(ui, ch.id, pattern, state, row.others) {
            actions.push(RackAction::SongChanged);
        }
        ui.separator();
        if ui.button(tr("Delete channel")).clicked() {
            *remove = Some(i);
            ui.close();
        }
    });

    ui.add_space(6.0);
    let grid_x = ui.cursor().left();
    let spacing = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = STEP_GAP;
    for step in 0..row.steps {
        step_button(
            ui,
            theme,
            ch,
            pattern,
            step,
            row.playhead == Some(step),
            actions,
        );
        if step % 4 == 3 {
            ui.add_space(BEAT_GAP);
        }
    }
    ui.spacing_mut().item_spacing.x = spacing;
    grid_x
}

/// The step tools of the channel menu (Prompt 27). Returns true when the steps changed.
fn step_tools(
    ui: &mut Ui,
    id: ChannelId,
    pattern: &mut Pattern,
    state: &mut RackState,
    others: &[(ChannelId, String)],
) -> bool {
    let mut changed = false;
    let row = pattern.step_row(id);
    let mut new_row = None;
    ui.menu_button(tr("Step tools"), |ui| {
        ui.menu_button(tr("Fill every"), |ui| {
            for n in [1, 2, 3, 4, 8, 16] {
                if ui.button(format!("{n}")).clicked() {
                    new_row = Some(rack::fill_every(&row, n, 0));
                    ui.close();
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(tr("Rotate"));
            if ui.button("◀").on_hover_text(tr("One step earlier, wrapping around")).clicked() {
                new_row = Some(rack::rotate(&row, -1));
            }
            if ui.button("▶").on_hover_text(tr("One step later, wrapping around")).clicked() {
                new_row = Some(rack::rotate(&row, 1));
            }
        });
        ui.horizontal(|ui| {
            ui.label(tr("Shift"));
            if ui.button("◀").on_hover_text(tr("One step earlier; the first step drops off")).clicked() {
                new_row = Some(rack::shift(&row, -1));
            }
            if ui.button("▶").on_hover_text(tr("One step later; the last step drops off")).clicked() {
                new_row = Some(rack::shift(&row, 1));
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(tr("Density"));
            ui.add(
                egui::DragValue::new(&mut state.density)
                    .range(0.0..=1.0)
                    .speed(0.01)
                    .custom_formatter(|v, _| format!("{:.0} %", v * 100.0))
                    .custom_parser(|s| s.trim_end_matches([' ', '%']).parse::<f64>().ok().map(|v| v / 100.0)),
            );
            ui.label(tr("Seed"));
            ui.add(egui::DragValue::new(&mut state.seed).speed(0.2));
        });
        if ui
            .button(tr("Randomize"))
            .on_hover_text(tr("Random steps at this density; the same seed gives the same row (the seed then moves on)"))
            .clicked()
        {
            new_row = Some(rack::randomize(row.len(), state.density, state.seed));
            state.seed = state.seed.wrapping_add(1);
        }
        ui.separator();
        ui.menu_button(tr("Copy steps to"), |ui| {
            for (other, name) in others.iter().filter(|(o, _)| *o != id) {
                if ui.button(name.as_str()).clicked() {
                    pattern.set_step_row(*other, &row);
                    changed = true;
                    ui.close();
                }
            }
        });
        if ui.button(tr("Clear steps")).clicked() {
            new_row = Some(vec![None; row.len()]);
            ui.close();
        }
    });
    if let Some(r) = new_row {
        pattern.set_step_row(id, &r);
        changed = true;
    }
    changed
}

/// The graph lane of channel `id`: one bar per step note, drag to draw, right-click resets.
/// Returns true when a value changed.
fn lane_graph(
    ui: &mut Ui,
    theme: &GloomTheme,
    id: ChannelId,
    pattern: &mut Pattern,
    lane: StepLane,
    steps: u16,
) -> bool {
    let width = step_x(steps.max(1) - 1) + STEP_W;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, LANE_H), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, theme.bg_deep);
    let (lo, hi) = lane.range();
    let y_of = |v: f32| rect.bottom() - (v - lo) / (hi - lo) * rect.height();
    let base = if lane.bipolar() {
        y_of(0.0)
    } else {
        rect.bottom()
    };
    p.hline(
        rect.x_range(),
        y_of(lane.neutral()),
        Stroke::new(1.0, theme.bg_widget_hover),
    );
    let clip = ui.clip_rect();
    for s in 0..steps {
        let x = rect.left() + step_x(s);
        if x > clip.right() || x + STEP_W < clip.left() {
            continue;
        }
        let Some(n) = pattern.step_note(id, s) else {
            continue;
        };
        let v = lane.get(n, rack::step_tick(s));
        let (a, b) = (y_of(v).min(base), y_of(v).max(base));
        let bar = Rect::from_min_max(pos2(x + 2.0, a), pos2(x + STEP_W - 2.0, b.max(a + 1.0)));
        p.rect_filled(bar, 1.0, theme.accent);
    }
    let mut changed = false;
    if let Some(pos) = resp.interact_pointer_pos() {
        if let Some(s) = step_at(pos.x - rect.left(), steps) {
            let v = if resp.secondary_clicked() {
                lane.neutral()
            } else {
                lo + (rect.bottom() - pos.y) / rect.height() * (hi - lo)
            };
            if (resp.dragged() || resp.clicked() || resp.secondary_clicked())
                && pattern.step_note(id, s).is_some()
            {
                pattern.set_step_lane(id, s, lane, v);
                changed = true;
            }
        }
    }
    if let Some(s) = resp
        .hover_pos()
        .and_then(|q| step_at(q.x - rect.left(), steps))
    {
        if let Some(n) = pattern.step_note(id, s) {
            let v = lane.get(n, rack::step_tick(s));
            resp.on_hover_text(format!(
                "{} {}: {}",
                tr(lane.label()),
                s + 1,
                lane_text(lane, v)
            ));
        }
    }
    changed
}

fn lane_text(lane: StepLane, v: f32) -> String {
    match lane {
        StepLane::Pan => crate::widgets::format_pan(v),
        StepLane::Fine => format!("{v:+.0} cents"),
        StepLane::Shift => format!("{v:+.0} ticks"),
        _ => format!("{:.0}", v * 127.0),
    }
}

fn step_button(
    ui: &mut Ui,
    theme: &GloomTheme,
    ch: &Channel,
    pattern: &mut Pattern,
    step: u16,
    playing: bool,
    actions: &mut Vec<RackAction>,
) {
    let (rect, resp) = ui.allocate_exact_size(vec2(STEP_W, 22.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let note = pattern.step_note(ch.id, step).copied();
    // Alternate beat groups so the bar structure is readable.
    let base = if (step / 4) % 2 == 0 {
        theme.bg_widget
    } else {
        theme.bg_panel.lerp_to_gamma(theme.bg_widget, 0.5)
    };
    let bg = if resp.hovered() {
        theme.bg_widget_hover
    } else {
        base
    };
    let p = ui.painter();
    let radius = f32::from(theme.radius);
    p.rect_filled(rect, radius, bg);
    if let Some(n) = note {
        // Velocity shown as fill height over a dim full-height block.
        p.rect_filled(rect.shrink(1.0), radius, theme.accent_dim);
        let h = (rect.height() - 2.0) * n.velocity.clamp(0.05, 1.0);
        let mut bar = rect.shrink(1.0);
        bar.set_top(bar.bottom() - h);
        p.rect_filled(bar, radius, theme.accent);
    } else {
        // Notes the piano roll placed in this step (other keys or off-grid): a small mark, so
        // a melodic channel's row does not look empty.
        let t0 = i64::from(step) * gt_core::STEP_TICKS;
        let notes = pattern.channel_notes(ch.id);
        let lo = notes.partition_point(|n| n.start < t0);
        if notes
            .get(lo)
            .is_some_and(|n| n.start < t0 + gt_core::STEP_TICKS)
        {
            let mark = egui::Rect::from_center_size(rect.center(), vec2(6.0, 6.0));
            p.rect_filled(mark, 1.0, theme.accent_dim.lerp_to_gamma(theme.accent, 0.4));
        }
    }
    if playing {
        p.rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, theme.text),
            egui::StrokeKind::Inside,
        );
    }

    if resp.clicked() {
        pattern.toggle_step(ch.id, step);
        actions.push(RackAction::SongChanged);
    }
    if let Some(n) = note {
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                pattern.set_step_velocity(ch.id, step, n.velocity + scroll * WHEEL_VELOCITY);
                actions.push(RackAction::SongChanged);
            }
        }
        resp.on_hover_text(format!(
            "Step {}: velocity {} (scroll to change)",
            step + 1,
            (n.velocity * 127.0).round()
        ));
    }
}

fn pattern_bar(
    ui: &mut Ui,
    theme: &GloomTheme,
    project: &mut Project,
    state: &mut RackState,
    actions: &mut Vec<RackAction>,
) {
    let dim = |s: &str| RichText::new(s).color(theme.text_dim);
    ui.horizontal(|ui| {
        ui.label(dim(tr("Pattern")));
        let current = project.current_pattern;
        if let Some(text) = &mut state.renaming {
            let r = ui.add(egui::TextEdit::singleline(text).desired_width(140.0));
            r.request_focus();
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if escape {
                state.renaming = None;
            } else if enter || r.lost_focus() {
                project.rename_pattern(current, text);
                state.renaming = None;
            }
        } else {
            let mut selected = current;
            egui::ComboBox::from_id_salt("pattern_select")
                .width(140.0)
                .selected_text(project.current_pattern().name.clone())
                .show_ui(ui, |ui| {
                    for p in &project.patterns {
                        ui.selectable_value(&mut selected, p.id, &p.name);
                    }
                });
            if selected != current {
                project.select_pattern(selected);
                actions.push(RackAction::SongChanged);
            }
            if ui
                .button(tr("New"))
                .on_hover_text(tr("New empty pattern"))
                .clicked()
            {
                let id = project.new_pattern();
                project.select_pattern(id);
                actions.push(RackAction::SongChanged);
            }
            if ui
                .button(tr("Clone"))
                .on_hover_text(tr("Copy this pattern"))
                .clicked()
            {
                if let Some(id) = project.clone_pattern(current) {
                    project.select_pattern(id);
                    actions.push(RackAction::SongChanged);
                }
            }
            if ui.button(tr("Rename")).clicked() {
                state.renaming = Some(project.current_pattern().name.clone());
            }
            if pattern_color(ui, theme, project.current_pattern_mut()) {
                actions.push(RackAction::PatternColor);
            }
            if ui
                .button(tr("Split"))
                .on_hover_text(tr(
                    "Split by channel: one pattern per channel, and the playlist clips of this \
                     pattern become one clip per channel on the tracks below",
                ))
                .clicked()
            {
                if let Some(&first) = project.split_pattern_by_channel(current).first() {
                    project.select_pattern(first);
                    actions.push(RackAction::SongChanged);
                }
            }
        }

        ui.separator();
        ui.label(dim(tr("Steps")));
        for n in Pattern::STEP_COUNTS {
            let on = project.current_pattern().steps == n;
            if ui
                .add(egui::Button::selectable(on, n.to_string()))
                .clicked()
                && !on
            {
                project.current_pattern_mut().steps = n;
                actions.push(RackAction::SongChanged);
            }
        }
        let mut steps = project.current_pattern().steps;
        if ui
            .add(egui::DragValue::new(&mut steps).range(1..=Pattern::MAX_STEPS).speed(0.2))
            .on_hover_text(tr("Any length, 1 to 512 steps (the grid scrolls)"))
            .changed()
        {
            project.current_pattern_mut().steps = steps;
            actions.push(RackAction::SongChanged);
        }

        ui.separator();
        ui.label(dim(tr("Swing")));
        let mut swing = project.swing;
        if knob(ui, theme, &mut swing, 0.0..=1.0, 0.0, 20.0, |s| {
            format!("Swing {:.0} % (67 % is a triplet feel)", s * 100.0)
        })
        .changed()
        {
            project.swing = swing;
            actions.push(RackAction::SongChanged);
        }

        ui.separator();
        let groups = rack_groups(project);
        if !groups.is_empty() || state.filter.is_some() {
            ui.label(dim(tr("Show")));
            let label = state.filter.clone().unwrap_or_else(|| tr("All channels").to_owned());
            egui::ComboBox::from_id_salt("rack_filter")
                .width(100.0)
                .selected_text(label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut state.filter, None, tr("All channels"));
                    for g in groups {
                        let on = state.filter.as_deref() == Some(&g);
                        if ui.selectable_label(on, &g).clicked() {
                            state.filter = Some(g);
                        }
                    }
                })
                .response
                .on_hover_text(tr("Rack filter: show one group of channels (set a channel's group in its settings tab)"));
        }
    });
}

/// The pattern colour swatch with its palette. Returns true when it changed.
fn pattern_color(ui: &mut Ui, theme: &GloomTheme, pattern: &mut Pattern) -> bool {
    let swatch = |c: u8| {
        rack::PATTERN_COLORS
            .get(usize::from(c).wrapping_sub(1))
            .map_or(theme.bg_widget_hover, |&[r, g, b]| {
                egui::Color32::from_rgb(r, g, b)
            })
    };
    let mut changed = false;
    let (rect, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
    ui.painter().rect_filled(rect, 3.0, swatch(pattern.color));
    let resp = resp.on_hover_text(tr("Pattern colour (in the playlist)"));
    egui::Popup::menu(&resp).show(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.set_max_width(8.0 * 20.0);
            for c in 0..Pattern::COLORS {
                let (r, x) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                ui.painter().rect_filled(r, 3.0, swatch(c));
                if c == pattern.color {
                    ui.painter().rect_stroke(
                        r,
                        3.0,
                        Stroke::new(1.5, theme.text),
                        egui::StrokeKind::Outside,
                    );
                }
                let x = if c == 0 {
                    x.on_hover_text(tr("Track colour"))
                } else {
                    x
                };
                if x.clicked() {
                    pattern.color = c;
                    changed = true;
                }
            }
        });
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect};

    #[test]
    fn step_cells_and_pointer_positions_agree() {
        for s in [0_u16, 3, 4, 7, 255, 511] {
            let x = step_x(s);
            assert_eq!(step_at(x + 1.0, 512), Some(s));
            assert_eq!(step_at(x + STEP_W - 1.0, 512), Some(s));
        }
        assert_eq!(step_at(step_x(16), 16), None);
    }

    #[test]
    fn a_long_pattern_with_lanes_layers_and_a_filter_draws() {
        let ctx = egui::Context::default();
        let theme = GloomTheme::default();
        let mut p = Project::demo();
        p.current_pattern_mut().steps = Pattern::MAX_STEPS;
        let layer = p.add_layer_channel("Layer").unwrap();
        p.channels[0].group = "Drums".into();
        let c0 = p.channels[0].id;
        p.current_pattern_mut()
            .set_step_row(c0, &rack::randomize(512, 0.5, 7));
        let mut st = RackState::default();
        for (lane, filter) in [
            (Some(StepLane::Velocity), None),
            (Some(StepLane::Shift), Some("Drums".to_owned())),
            (None, Some("none".to_owned())),
        ] {
            st.lane = lane;
            st.filter = filter;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 600.0))),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                channel_rack(ui, &theme, &mut p, &mut st, RackView::default());
                let groups = rack_groups(&p);
                assert_eq!(groups, ["Drums"]);
                let i = p.channel_index(layer).unwrap();
                let others: Vec<_> = p.channels[..i]
                    .iter()
                    .map(|c| (c.id, c.name.clone()))
                    .collect();
                let ch = &mut p.channels[i];
                crate::views::channel_settings(ui, &theme, ch, 0.3, &groups);
                if let gt_core::Instrument::Layer(l) = &mut ch.instrument {
                    crate::views::layer_panel(ui, &theme, &mut ch.name, l, &others);
                }
            });
            out.textures_delta.clear();
        }
    }
}
