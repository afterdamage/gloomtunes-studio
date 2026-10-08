//! Channel rack: the pattern bar and one row per channel with mute, solo, volume, pan and the
//! step grid.

use egui::{vec2, RichText, Sense, Stroke, Ui};
use gt_core::{Channel, ChannelParam, ParamId, Pattern, Project};

use crate::param_ui::param_menu;
use crate::widgets::{format_gain, format_pan, knob};
use crate::GloomTheme;

/// Velocity change per point of mouse-wheel scroll.
const WHEEL_VELOCITY: f32 = 0.002;

/// UI state of the rack that is not part of the document.
#[derive(Debug, Clone, Default)]
pub struct RackState {
    /// Selected channel (index into `Project::channels`); its settings show below the rack.
    pub selected: usize,
    /// Text of the pattern name while renaming.
    renaming: Option<String>,
    /// Channel being auditioned with the mouse.
    audition: Option<usize>,
}

/// What changed, so the app can update the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    let silenced: Vec<bool> = (0..project.channels.len())
        .map(|i| project.is_silenced(i))
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
    egui::ScrollArea::both()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (i, ch) in channels.iter_mut().enumerate() {
                let row = RowCtx {
                    index: i,
                    selected: state.selected == i,
                    silenced: silenced[i],
                    activity: view.activity.get(i).copied().unwrap_or(0.0),
                    steps,
                    playhead: view.playhead_step,
                };
                ui.horizontal(|ui| {
                    channel_row(
                        ui,
                        theme,
                        ch,
                        pattern,
                        state,
                        &row,
                        &mut actions,
                        &mut remove,
                    );
                });
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .button("+ Sampler")
                    .on_hover_text("Add an empty sampler channel; load a sample from the browser")
                    .clicked()
                {
                    add = Some(false);
                }
                if ui
                    .button("+ Gloom Synth")
                    .on_hover_text("Add a Gloom Synth channel")
                    .clicked()
                {
                    add = Some(true);
                }
            });
        });
    if let Some(synth) = add {
        let n = project.channels.len() + 1;
        let added = if synth {
            project.add_synth_channel(&format!("Synth {n}"), gt_core::SynthPatch::default())
        } else {
            project.add_channel(&format!("Sampler {n}"), None)
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
    actions
}

struct RowCtx {
    index: usize,
    selected: bool,
    silenced: bool,
    activity: f32,
    steps: u16,
    playhead: Option<u16>,
}

#[allow(clippy::too_many_arguments)]
fn channel_row(
    ui: &mut Ui,
    theme: &GloomTheme,
    ch: &mut Channel,
    pattern: &mut Pattern,
    state: &mut RackState,
    row: &RowCtx,
    actions: &mut Vec<RackAction>,
    remove: &mut Option<usize>,
) {
    let i = row.index;

    // Activity light.
    let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
    let lit = row.activity.clamp(0.0, 1.0);
    let colour = theme.bg_widget.lerp_to_gamma(theme.accent, lit);
    ui.painter().circle_filled(r.center(), 3.5, colour);

    let mute = ui
        .add(egui::Button::selectable(ch.mute, "M").min_size(vec2(18.0, 18.0)))
        .on_hover_text("Mute");
    if mute.clicked() {
        ch.mute = !ch.mute;
        actions.push(RackAction::ParamsChanged);
    }
    let solo = ui
        .add(egui::Button::selectable(ch.solo, "S").min_size(vec2(18.0, 18.0)))
        .on_hover_text("Solo (only soloed channels play)");
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
    if r.changed() {
        ch.volume = vol;
        actions.push(RackAction::ParamsChanged);
    }
    let mut pan = ch.pan;
    let r = knob(ui, theme, &mut pan, -1.0..=1.0, 0.0, 20.0, |p| {
        format!("Pan {}", format_pan(p))
    });
    param_menu(theme, &r, target(ChannelParam::Pan));
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
                .range(0..=gt_core::INSERTS)
                .speed(0.1)
                .custom_formatter(|v, _| {
                    if v < 0.5 {
                        "M".to_owned()
                    } else {
                        format!("{v:.0}")
                    }
                }),
        )
        .on_hover_text("Mixer insert (drag or type; M = master)")
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
        .on_hover_text("Click to select, hold to hear, right-click for options");
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
        if ui.button("Open in piano roll").clicked() {
            state.selected = i;
            actions.push(RackAction::OpenPianoRoll(i));
            ui.close();
        }
        if ui.button("Delete channel").clicked() {
            *remove = Some(i);
            ui.close();
        }
    });

    ui.add_space(6.0);
    let spacing = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = 2.0;
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
            ui.add_space(3.0);
        }
    }
    ui.spacing_mut().item_spacing.x = spacing;
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
    let (rect, resp) = ui.allocate_exact_size(vec2(16.0, 22.0), Sense::click());
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
        ui.label(dim("Pattern"));
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
                .button("New")
                .on_hover_text("New empty pattern")
                .clicked()
            {
                let id = project.new_pattern();
                project.select_pattern(id);
                actions.push(RackAction::SongChanged);
            }
            if ui
                .button("Clone")
                .on_hover_text("Copy this pattern")
                .clicked()
            {
                if let Some(id) = project.clone_pattern(current) {
                    project.select_pattern(id);
                    actions.push(RackAction::SongChanged);
                }
            }
            if ui.button("Rename").clicked() {
                state.renaming = Some(project.current_pattern().name.clone());
            }
        }

        ui.separator();
        ui.label(dim("Steps"));
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

        ui.separator();
        ui.label(dim("Swing"));
        let mut swing = project.swing;
        if knob(ui, theme, &mut swing, 0.0..=1.0, 0.0, 20.0, |s| {
            format!("Swing {:.0} % (67 % is a triplet feel)", s * 100.0)
        })
        .changed()
        {
            project.swing = swing;
            actions.push(RackAction::SongChanged);
        }
    });
}
