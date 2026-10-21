//! The four main views, docked in the main window or floating in windows of their own
//! (Prompt 25, D133), and the workflow windows: undo history, project info, help.

use std::sync::atomic::Ordering;

use gt_core::{AudioInput, STEP_TICKS, STRIPS};
use gt_ui::views::{
    channel_rack, help_panel, history_panel, mixer_view, playlist, project_info_panel, MixerView,
    PianoRollAction, PlaylistAction, PlaylistView, RackAction, RackView,
};
use gt_ui::{tr, GloomTheme};

use super::{plugin_view, GloomApp};
use crate::layout::MainView;

/// What the views need that is read once per frame.
pub(super) struct ViewCtx {
    pub(super) pattern_pos: Option<i64>,
    pub(super) inputs: Vec<AudioInput>,
    pub(super) sample_rate: f32,
}

/// What the views returned this frame.
#[derive(Default)]
pub(super) struct ViewOut {
    rack: Vec<RackAction>,
    roll: Vec<PianoRollAction>,
    playlist: Vec<PlaylistAction>,
    mixer_changed: bool,
}

impl GloomApp {
    /// Draws one view.
    fn draw_view(
        &mut self,
        ui: &mut egui::Ui,
        theme: &GloomTheme,
        v: MainView,
        cx: &ViewCtx,
        out: &mut ViewOut,
    ) {
        match v {
            MainView::Playlist => {
                let r = self.loop_region();
                out.playlist.extend(playlist(
                    ui,
                    theme,
                    &mut self.project,
                    &mut self.playlist,
                    PlaylistView {
                        playhead: Some(self.transport.position.0),
                        loop_region: (r.start.0, r.end.0, r.enabled),
                        audio: &self.library,
                        inputs: &cx.inputs,
                        recording: self.record.live_take(),
                        performance: gt_ui::views::PerformanceView {
                            perf: &self.show,
                            position: self.transport.position.0,
                            recording: self.perf_record,
                        },
                    },
                ));
            }
            MainView::Rack => {
                out.rack.extend(channel_rack(
                    ui,
                    theme,
                    &mut self.project,
                    &mut self.rack,
                    RackView {
                        playhead_step: cx.pattern_pos.map(|t| (t / STEP_TICKS) as u16),
                        activity: &self.activity,
                    },
                ));
            }
            MainView::PianoRoll => {
                let a = self.show_roll(ui, theme, cx.pattern_pos);
                out.roll.extend(a);
            }
            MainView::Mixer => {
                let latency = self
                    .audio
                    .engine()
                    .map_or(0, |e| e.telemetry().latency_frames.load(Ordering::Relaxed));
                let open_plugin = self.mixer_state.slot.and_then(|k| {
                    let strip = &self.project.mixer.strips
                        [self.mixer_state.selected.min(self.project.mixer.inserts())];
                    let slot = strip.slots.get(k)?.as_ref()?;
                    let p = match &slot.patch {
                        Some(patch) => patch
                            .node(self.mixer_state.patcher.selected?)?
                            .plugin
                            .as_ref()?,
                        None => slot.plugin.as_ref()?,
                    };
                    Some(plugin_view(&self.plugins, &self.project, p.instance))
                });
                let scope = self.mixer_state.slot.and_then(|k| {
                    self.scopes[self.mixer_state.selected.min(STRIPS - 1)][k].as_deref()
                });
                let tracks: Vec<(gt_core::TrackId, String, usize)> = self
                    .project
                    .playlist
                    .tracks
                    .iter()
                    .map(|t| (t.id, t.name.clone(), t.insert))
                    .collect();
                let view = MixerView {
                    tracks: &tracks,
                    strip_presets: &self.strip_presets,
                    chain_presets: &self.chain_presets,
                    preset_status: self.strip_preset_status.as_deref(),
                    scope,
                    meters: &self.strip_view,
                    fx_meters: &self.fx_meters,
                    sample_rate: cx.sample_rate,
                    latency_ms: latency as f32 * 1000.0 / cx.sample_rate,
                    plugin: open_plugin.as_ref(),
                    inputs: &cx.inputs,
                };
                if mixer_view(
                    ui,
                    theme,
                    &mut self.project.mixer,
                    &mut self.mixer_state,
                    view,
                ) {
                    out.mixer_changed = true;
                }
                if let Some(r) = self.mixer_state.request.take() {
                    self.on_mixer_request(r);
                }
                if let Some(d) = self.mixer_state.dropped.take() {
                    self.drop_on_mixer(d);
                }
                if let Some(i) = self.mixer_state.remove_request.take() {
                    if self.project.remove_insert(i) {
                        self.mixer_state.selected = i.min(self.project.mixer.inserts());
                        self.mixer_state.slot = None;
                        out.mixer_changed = true;
                    }
                }
            }
        }
    }

    /// The main window's view tabs and its docked view.
    pub(super) fn docked_views(
        &mut self,
        ui: &mut egui::Ui,
        theme: &GloomTheme,
        cx: &ViewCtx,
        out: &mut ViewOut,
    ) {
        let shown = self.layout.docked_view();
        ui.horizontal(|ui| {
            for (v, tip) in [
                (MainView::Playlist, tr("Arrangement of clips (F5)")),
                (MainView::Rack, tr("Step sequencer (F6)")),
                (
                    MainView::PianoRoll,
                    tr("Notes of the selected channel (F7)"),
                ),
                (MainView::Mixer, tr("Inserts, sends and effects (F9)")),
            ] {
                let floating = self.layout.is_floating(v);
                let label = if floating {
                    format!("{} {}", v.label(), tr("(window)"))
                } else {
                    v.label().to_owned()
                };
                let tip = if floating {
                    tr("In its own window: click to bring it to the front")
                } else {
                    tip
                };
                if ui
                    .add(egui::Button::selectable(shown == Some(v), label))
                    .on_hover_text(tip)
                    .clicked()
                {
                    self.show_view(v);
                }
            }
            if let Some(v) = shown {
                if ui
                    .small_button(tr("Float"))
                    .on_hover_text(tr(
                        "Open this view in its own window (move it to another monitor; the \
                         layout is kept for next time)",
                    ))
                    .clicked()
                {
                    let size = ui.ctx().content_rect().size() * 0.7;
                    let at = ui
                        .ctx()
                        .input(|i| i.viewport().outer_rect)
                        .map(|r| egui::Rect::from_min_size(r.min + egui::vec2(60.0, 60.0), size));
                    self.layout.float(v, at);
                }
            }
            ui.separator();
            let n = self.project.modulators.len();
            let label = if n > 0 {
                format!("{} ({n})", tr("Modulators"))
            } else {
                tr("Modulators").to_owned()
            };
            if ui
                .add(egui::Button::selectable(self.show_modulators, label))
                .on_hover_text(tr(
                    "LFOs and envelope followers. Right-click any knob or fader to add one",
                ))
                .clicked()
            {
                self.show_modulators = !self.show_modulators;
            }
        });
        ui.separator();
        self.view_rects.clear();
        match shown {
            Some(v) => {
                self.view_rects.push((v, ui.available_rect_before_wrap()));
                self.draw_view(ui, theme, v, cx, out);
            }
            None => {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        egui::RichText::new(tr(
                            "Every view is in its own window. Close one to bring it back here.",
                        ))
                        .color(theme.text_dim),
                    );
                });
            }
        }
    }

    /// The floating views, each in a native window of its own (an egui window where the
    /// system cannot open more windows).
    pub(super) fn floating_views(
        &mut self,
        ctx: &egui::Context,
        theme: &GloomTheme,
        cx: &ViewCtx,
        out: &mut ViewOut,
    ) {
        let focus = self.focus_view.take();
        let views: Vec<_> = self
            .layout
            .floating
            .iter()
            .map(|f| (f.view, f.opened_at))
            .collect();
        for (v, at) in views {
            let mut builder = egui::ViewportBuilder::default()
                .with_title(format!("{} - GloomTunes Studio", v.label()))
                .with_min_inner_size([360.0, 240.0]);
            builder = match at {
                Some(r) => builder.with_position(r.min).with_inner_size(r.size()),
                None => builder.with_inner_size([960.0, 600.0]),
            };
            if focus == Some(v) {
                ctx.send_viewport_cmd_to(v.viewport_id(), egui::ViewportCommand::Focus);
            }
            let close = ctx.show_viewport_immediate(v.viewport_id(), builder, |ui, class| {
                if class == egui::ViewportClass::Immediate {
                    // Global shortcuts work in every window.
                    self.run_shortcuts(ui.ctx());
                    let info = ui.ctx().input(|i| i.viewport().clone());
                    if let (Some(o), Some(i)) = (info.outer_rect, info.inner_rect) {
                        let rect = egui::Rect::from_min_size(o.min, i.size());
                        if let Some(f) = self.layout.floating.iter_mut().find(|f| f.view == v) {
                            f.rect = Some(rect);
                        }
                    }
                }
                egui::CentralPanel::default()
                    .frame(egui::Frame::central_panel(ui.style()).inner_margin(10))
                    .show(ui, |ui| {
                        if class != egui::ViewportClass::Immediate
                            && ui.small_button(tr("Dock")).clicked()
                        {
                            return true;
                        }
                        self.draw_view(ui, theme, v, cx, out);
                        false
                    })
                    .inner
                    || class == egui::ViewportClass::Immediate
                        && ui.ctx().input(|i| i.viewport().close_requested())
            });
            if close {
                self.layout.dock(v);
            }
        }
    }

    /// Acts on what the views returned.
    pub(super) fn on_views(&mut self, out: ViewOut) {
        for a in out.rack {
            self.on_rack(a);
        }
        for a in out.roll {
            self.on_roll(a);
        }
        for a in out.playlist {
            self.on_playlist(a);
        }
        if out.mixer_changed {
            self.pending_edit = Some("Mixer");
        }
    }

    /// The undo history, project info and help windows.
    pub(super) fn workflow_windows(&mut self, ctx: &egui::Context, theme: &GloomTheme) {
        let center = ctx.content_rect().center();
        if self.show_history {
            let mut open = true;
            let mut jump = None;
            egui::Window::new(tr("Undo history"))
                .open(&mut open)
                .default_width(260.0)
                .default_pos(center + egui::vec2(120.0, -200.0))
                .show(ctx, |ui| {
                    let (labels, applied) = self.history.steps();
                    jump = history_panel(ui, theme, &labels, applied);
                });
            self.show_history = open;
            if let Some(to) = jump {
                self.jump_history(to);
            }
        }
        if self.show_info {
            let mut open = true;
            let mut changed = false;
            egui::Window::new(tr("Project info"))
                .open(&mut open)
                .default_width(420.0)
                .default_pos(center - egui::vec2(210.0, 200.0))
                .show(ctx, |ui| {
                    changed = project_info_panel(ui, theme, &mut self.project.info);
                });
            self.show_info = open;
            if changed {
                self.pending_edit = Some("Project info");
            }
        }
        if self.show_help {
            let mut open = true;
            egui::Window::new(tr("Help"))
                .open(&mut open)
                .default_size([560.0, 480.0])
                .default_pos(center - egui::vec2(280.0, 240.0))
                .show(ctx, |ui| {
                    help_panel(ui, theme, &mut self.help, &self.keymap);
                });
            self.show_help = open;
        }
    }

    /// Undoes or redoes until `applied` steps are applied (the history panel's click).
    fn jump_history(&mut self, applied: usize) {
        if let Some(label) = self.pending_edit.take() {
            if self.history.commit(&self.project, label) {
                self.edits += 1;
            }
        }
        self.cancel_roll_gesture();
        self.playlist.cancel();
        let moved = self.history.jump_to(&mut self.project, applied);
        if moved > 0 {
            self.after_undo();
        }
    }
}
