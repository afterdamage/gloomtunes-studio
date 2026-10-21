//! The patcher editor (Prompt 34): modules as boxes on a canvas, wires drawn between their
//! ports, and the selected module's controls and wires below.
//!
//! Drag from an output port to an input port to wire them; drop a modulation output on a
//! module's body to pick one of its parameters. Drag a module by its title, the background to
//! pan; right-click a module to remove it.

use egui::{vec2, Color32, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use gt_core::patcher::{ModuleKind, NodeId, Patch, PortKind, WireEnd, MAX_NODES};
use gt_core::{EffectKind, PluginOwner, SynthPatch};

use crate::views::{plugin_controls, PluginAction, PluginPanelView};
use crate::widgets::param_knob;
use crate::{tr, GloomTheme};

const NODE_W: f32 = 132.0;
const HEADER_H: f32 = 20.0;
const PORT_H: f32 = 16.0;
const PORT_R: f32 = 4.5;
const CANVAS_H: f32 = 300.0;

/// What the editor asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum PatcherAction {
    /// Choose a plugin to add as a module.
    AddPlugin,
    /// A plugin module's panel asked for something.
    Plugin(PluginAction),
}

/// Editor state kept between frames.
#[derive(Debug, Clone, Default)]
pub struct PatcherState {
    /// The selected module.
    pub selected: Option<NodeId>,
    /// Canvas scroll.
    pan: Vec2,
    /// A wire being dragged from this output.
    dragging: Option<(NodeId, u8)>,
    /// A modulation output dropped on a module: pick the parameter.
    picking: Option<(NodeId, u8, NodeId, Pos2)>,
    /// Where the canvas origin was last drawn.
    origin: Pos2,
}

/// Where a plugin module's panel sits, for its owner.
#[derive(Debug, Clone, Copy)]
pub enum PatchPlace {
    /// A patcher channel.
    Channel(gt_core::ChannelId),
    /// A patcher effect slot.
    Effect {
        /// Strip.
        strip: usize,
        /// Slot.
        slot: usize,
    },
}

fn port_color(theme: &GloomTheme, k: PortKind) -> Color32 {
    match k {
        PortKind::Audio => theme.accent,
        PortKind::Mod => Color32::from_rgb(0xd0, 0xa0, 0x40),
        PortKind::Midi => Color32::from_rgb(0x60, 0xb0, 0xd0),
    }
}

fn node_height(n: &gt_core::patcher::Node) -> f32 {
    HEADER_H + PORT_H * n.inputs().len().max(n.outputs().len()).max(1) as f32 + 6.0
}

fn in_port(origin: Pos2, n: &gt_core::patcher::Node, i: usize) -> Pos2 {
    origin + vec2(n.pos[0], n.pos[1] + HEADER_H + PORT_H * (i as f32 + 0.5))
}

fn out_port(origin: Pos2, n: &gt_core::patcher::Node, i: usize) -> Pos2 {
    origin
        + vec2(
            n.pos[0] + NODE_W,
            n.pos[1] + HEADER_H + PORT_H * (i as f32 + 0.5),
        )
}

fn wire_shape(a: Pos2, b: Pos2, stroke: Stroke) -> egui::Shape {
    let dx = ((b.x - a.x).abs() * 0.5).max(30.0);
    egui::epaint::CubicBezierShape::from_points_stroke(
        [a, a + vec2(dx, 0.0), b - vec2(dx, 0.0), b],
        false,
        Color32::TRANSPARENT,
        stroke,
    )
    .into()
}

/// The editor. `plugin_view` describes the selected plugin module's state. Returns true when
/// the patch changed.
pub fn patcher_view(
    ui: &mut Ui,
    theme: &GloomTheme,
    place: PatchPlace,
    patch: &mut Patch,
    state: &mut PatcherState,
    plugin_view: &PluginPanelView,
    actions: &mut Vec<PatcherAction>,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(tr("PATCH"))
                .small()
                .strong()
                .color(theme.accent),
        );
        let full = patch.nodes.len() >= MAX_NODES;
        let at = [40.0 - state.pan.x, 40.0 - state.pan.y];
        ui.add_enabled_ui(!full, |ui| {
            ui.menu_button(tr("＋ Module"), |ui| {
                for kind in ModuleKind::BUILT_IN {
                    if ui.button(tr(kind.name())).clicked() {
                        state.selected = patch.add(kind, at);
                        changed = true;
                        ui.close();
                    }
                }
                ui.menu_button(tr("Effect"), |ui| {
                    for kind in EffectKind::ALL {
                        if ui.button(kind.name()).clicked() {
                            state.selected = patch.add(ModuleKind::Effect(kind), at);
                            changed = true;
                            ui.close();
                        }
                    }
                });
                if ui
                    .button(tr("Plugin…"))
                    .on_hover_text(tr("Add an instrument or effect plugin as a module"))
                    .clicked()
                {
                    actions.push(PatcherAction::AddPlugin);
                    ui.close();
                }
            });
        });
        ui.label(
            RichText::new(tr(
                "Drag from an output to an input; drop a modulation output on a module to move \
                 one of its parameters",
            ))
            .small()
            .color(theme.text_dim),
        );
    });

    let width = ui.available_width().max(200.0);
    let (rect, bg) = ui.allocate_exact_size(vec2(width, CANVAS_H), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, f32::from(theme.radius), theme.bg_deep);
    if bg.dragged() && state.dragging.is_none() {
        state.pan += bg.drag_delta();
    }
    if bg.clicked() {
        state.selected = None;
        state.picking = None;
    }
    let origin = rect.min + state.pan;
    state.origin = origin;
    let pointer = ui.input(|i| i.pointer.interact_pos());

    // Wires under the modules.
    for w in &patch.wires {
        let (Some(a), Some(b)) = (patch.node(w.from), patch.node(w.to)) else {
            continue;
        };
        let kind = patch.output_kind(w.from, w.port).unwrap_or(PortKind::Audio);
        let from = out_port(origin, a, usize::from(w.port));
        let to = match w.end {
            WireEnd::Input(i) => in_port(origin, b, usize::from(i)),
            // Parameter wires end at the module's title.
            WireEnd::Param(_) => origin + vec2(b.pos[0], b.pos[1] + HEADER_H * 0.5),
        };
        let selected = state.selected.is_some_and(|s| s == w.from || s == w.to);
        let mut c = port_color(theme, kind);
        if matches!(w.end, WireEnd::Param(_)) {
            c = c.gamma_multiply(0.6);
        }
        let width = if selected { 2.5 } else { 1.5 };
        p.add(wire_shape(from, to, Stroke::new(width, c)));
    }

    // Modules.
    let mut remove = None;
    let mut drop_on: Option<NodeId> = None;
    let mut drop_port: Option<(NodeId, u8)> = None;
    let released = ui.input(|i| i.pointer.any_released());
    for n in &mut patch.nodes {
        let box_rect = Rect::from_min_size(
            origin + vec2(n.pos[0], n.pos[1]),
            vec2(NODE_W, node_height(n)),
        );
        if !box_rect.intersects(rect) {
            continue;
        }
        let selected = state.selected == Some(n.id);
        p.rect_filled(box_rect, 3.0, theme.bg_panel);
        p.rect_stroke(
            box_rect,
            3.0,
            Stroke::new(
                if selected { 1.5 } else { 1.0 },
                if selected { theme.accent } else { theme.stroke },
            ),
            egui::StrokeKind::Inside,
        );
        let header = Rect::from_min_size(box_rect.min, vec2(NODE_W, HEADER_H));
        p.text(
            header.left_center() + vec2(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            n.name(),
            egui::FontId::proportional(11.0),
            if selected { theme.accent } else { theme.text },
        );
        let id = ui.id().with(("patch_node", n.id));
        let hr = ui.interact(header, id, Sense::click_and_drag());
        if hr.dragged() {
            let d = hr.drag_delta();
            n.pos[0] += d.x;
            n.pos[1] += d.y;
            changed = true;
        }
        if hr.clicked() || hr.drag_started() {
            state.selected = Some(n.id);
        }
        let body = ui.interact(box_rect, id.with("body"), Sense::click());
        if body.clicked() {
            state.selected = Some(n.id);
        }
        if !n.kind.is_fixed() {
            body.context_menu(|ui| {
                if ui.button(tr("Remove module")).clicked() {
                    remove = Some(n.id);
                    ui.close();
                }
            });
            hr.context_menu(|ui| {
                if ui.button(tr("Remove module")).clicked() {
                    remove = Some(n.id);
                    ui.close();
                }
            });
        }
        for (i, (name, kind)) in n.inputs().iter().enumerate() {
            let at = in_port(origin, n, i);
            p.circle_filled(at, PORT_R, port_color(theme, *kind));
            p.text(
                at + vec2(8.0, 0.0),
                egui::Align2::LEFT_CENTER,
                tr(name),
                egui::FontId::proportional(10.0),
                theme.text_dim,
            );
            let r = Rect::from_center_size(at, vec2(14.0, 14.0));
            if released && pointer.is_some_and(|q| r.contains(q)) {
                drop_port = Some((n.id, i as u8));
            }
        }
        for (i, (name, kind)) in n.outputs().iter().enumerate() {
            let at = out_port(origin, n, i);
            p.circle_filled(at, PORT_R, port_color(theme, *kind));
            p.text(
                at - vec2(8.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                tr(name),
                egui::FontId::proportional(10.0),
                theme.text_dim,
            );
            let r = Rect::from_center_size(at, vec2(14.0, 14.0));
            let resp = ui.interact(r, id.with(("out", i)), Sense::drag());
            if resp.drag_started() {
                state.dragging = Some((n.id, i as u8));
            }
        }
        if released && pointer.is_some_and(|q| box_rect.contains(q)) {
            drop_on = Some(n.id);
        }
    }

    // A wire being dragged.
    if let Some((from, port)) = state.dragging {
        if let (Some(n), Some(q)) = (patch.node(from), pointer) {
            let kind = patch.output_kind(from, port).unwrap_or(PortKind::Audio);
            let a = out_port(origin, n, usize::from(port));
            p.add(wire_shape(a, q, Stroke::new(1.5, port_color(theme, kind))));
        }
        if released {
            state.dragging = None;
            match (drop_port, drop_on) {
                (Some((to, input)), _) => {
                    changed |= patch
                        .connect(from, port, to, WireEnd::Input(input), 1.0)
                        .is_ok();
                }
                (None, Some(to))
                    if to != from
                        && patch.output_kind(from, port) == Some(PortKind::Mod)
                        && patch.node(to).is_some_and(|n| n.param_count() > 0) =>
                {
                    state.picking = Some((from, port, to, pointer.unwrap_or(rect.center())));
                }
                _ => {}
            }
        }
    }

    // Choosing the parameter a modulation wire moves.
    if let Some((from, port, to, at)) = state.picking {
        let mut close = false;
        egui::Area::new(ui.id().with("patch_pick"))
            .fixed_pos(at)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(RichText::new(tr("Modulate")).small().color(theme.text_dim));
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .show(ui, |ui| {
                            let names: Vec<String> = patch.node(to).map_or_else(Vec::new, |n| {
                                (0..n.param_count())
                                    .map(|i| n.param_name(i).to_owned())
                                    .collect()
                            });
                            for (i, name) in names.into_iter().enumerate() {
                                if ui.button(name).clicked() {
                                    let end = WireEnd::Param(i as u16);
                                    changed |= patch.connect(from, port, to, end, 0.5).is_ok();
                                    close = true;
                                }
                            }
                        });
                    if ui.small_button(tr("Cancel")).clicked() {
                        close = true;
                    }
                });
            });
        if close {
            state.picking = None;
        }
    }

    if let Some(id) = remove {
        changed |= patch.remove(id);
        if state.selected == Some(id) {
            state.selected = None;
        }
    }

    // The selected module's controls and wires.
    if let Some(id) = state.selected {
        changed |= selected_panel(ui, theme, place, patch, id, plugin_view, actions);
    }
    changed
}

fn selected_panel(
    ui: &mut Ui,
    theme: &GloomTheme,
    place: PatchPlace,
    patch: &mut Patch,
    id: NodeId,
    plugin_view: &PluginPanelView,
    actions: &mut Vec<PatcherAction>,
) -> bool {
    let mut changed = false;
    let names: Vec<(NodeId, String)> = patch
        .nodes
        .iter()
        .map(|n| (n.id, n.name().to_owned()))
        .collect();
    let Some(n) = patch.node_mut(id) else {
        return false;
    };
    ui.label(
        RichText::new(n.name().to_owned())
            .strong()
            .color(theme.accent),
    );
    if let Some(plugin) = n.plugin.as_deref_mut() {
        let owner = match place {
            PatchPlace::Channel(c) => PluginOwner::Node {
                channel: Some(c),
                strip: 0,
                slot: 0,
                node: id,
            },
            PatchPlace::Effect { strip, slot } => PluginOwner::Node {
                channel: None,
                strip,
                slot,
                node: id,
            },
        };
        let mut pa = Vec::new();
        changed |= plugin_controls(ui, theme, owner, plugin, plugin_view, &mut pa);
        actions.extend(pa.into_iter().map(PatcherAction::Plugin));
    } else if let Some(synth) = n.synth.as_deref_mut() {
        egui::ComboBox::from_id_salt(("patch_synth", id))
            .selected_text(RichText::new(synth.name.clone()).small())
            .show_ui(ui, |ui| {
                for preset in SynthPatch::factory() {
                    if ui
                        .selectable_label(preset.name == synth.name, &preset.name)
                        .clicked()
                    {
                        *synth = preset;
                        changed = true;
                    }
                }
            });
    } else {
        let infos = n.kind.params();
        ui.horizontal_wrapped(|ui| {
            for (i, (info, v)) in infos.iter().zip(n.params.iter_mut()).enumerate() {
                changed |= param_knob(ui, theme, ("patch", id, i), info, v, None);
            }
        });
    }
    // Wires into it: amount and removal.
    let mut drop = None;
    let incoming: Vec<usize> = (0..patch.wires.len())
        .filter(|&i| patch.wires[i].to == id)
        .collect();
    if !incoming.is_empty() {
        ui.label(RichText::new(tr("Wires in")).small().color(theme.text_dim));
    }
    for i in incoming {
        let w = patch.wires[i];
        let from = names
            .iter()
            .find(|(n, _)| *n == w.from)
            .map_or("?", |(_, s)| s.as_str());
        let out = patch
            .node(w.from)
            .and_then(|n| n.outputs().get(usize::from(w.port)))
            .map_or("?", |(s, _)| s);
        let to = patch.node(id).map_or_else(String::new, |n| match w.end {
            WireEnd::Input(k) => n
                .inputs()
                .get(usize::from(k))
                .map_or("?".to_owned(), |(s, _)| tr(s).to_owned()),
            WireEnd::Param(k) => n.param_name(usize::from(k)).to_owned(),
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{from} · {} → {to}", tr(out))).small());
            let mut a = w.amount;
            let range = if matches!(w.end, WireEnd::Param(_)) {
                -1.0..=1.0
            } else {
                0.0..=4.0
            };
            if ui
                .add(egui::DragValue::new(&mut a).range(range).speed(0.01))
                .on_hover_text(tr("Amount"))
                .changed()
            {
                patch.wires[i].amount = a;
                changed = true;
            }
            if ui
                .small_button("✕")
                .on_hover_text(tr("Remove wire"))
                .clicked()
            {
                drop = Some(i);
            }
        });
    }
    if let Some(i) = drop {
        patch.wires.remove(i);
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn frame(ctx: &egui::Context, patch: &mut Patch, st: &mut PatcherState, ev: Vec<egui::Event>) {
        let theme = GloomTheme::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 700.0))),
            events: ev,
            ..Default::default()
        };
        let mut actions = Vec::new();
        let mut out = ctx.run_ui(input, |ui| {
            patcher_view(
                ui,
                &theme,
                PatchPlace::Channel(gt_core::ChannelId(1)),
                patch,
                st,
                &PluginPanelView::default(),
                &mut actions,
            );
        });
        out.textures_delta.clear();
    }

    fn drag(from: Pos2, to: Pos2) -> Vec<Vec<egui::Event>> {
        let press = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut out = vec![
            vec![egui::Event::PointerMoved(from)],
            vec![press(from, true)],
        ];
        for i in 1..=8 {
            out.push(vec![egui::Event::PointerMoved(
                from + (to - from) * (i as f32 / 8.0),
            )]);
        }
        out.push(vec![press(to, false)]);
        out.push(vec![]);
        out
    }

    #[test]
    fn ports_sit_on_the_box_edges() {
        let p = Patch::instrument();
        let n = &p.nodes[3];
        let o = pos2(10.0, 10.0);
        assert_eq!(in_port(o, n, 0).x, 10.0 + n.pos[0]);
        assert_eq!(out_port(o, n, 0).x, 10.0 + n.pos[0] + NODE_W);
        assert!(node_height(n) > HEADER_H);
    }

    #[test]
    fn dragging_between_ports_wires_them_and_a_mod_drop_asks_for_a_parameter() {
        let ctx = egui::Context::default();
        let mut patch = Patch::instrument();
        let noise = patch.add(ModuleKind::Noise, [200.0, 200.0]).unwrap();
        let lfo = patch.add(ModuleKind::Lfo, [20.0, 200.0]).unwrap();
        let mut st = PatcherState::default();
        frame(&ctx, &mut patch, &mut st, vec![]);
        let o = st.origin;
        let out = patch.nodes[2].id;
        let from = out_port(o, patch.node(noise).unwrap(), 0);
        let to = in_port(o, patch.node(out).unwrap(), 0);
        let before = patch.wires.len();
        for ev in drag(from, to) {
            frame(&ctx, &mut patch, &mut st, ev);
        }
        assert_eq!(patch.wires.len(), before + 1);
        let w = patch.wires.last().unwrap();
        assert_eq!((w.from, w.to, w.end), (noise, out, WireEnd::Input(0)));
        // The LFO dropped on the filter's body: pick one of its parameters.
        let filter = patch.nodes[5].id;
        let f = patch.node(filter).unwrap();
        let body = o + vec2(f.pos[0] + NODE_W * 0.5, f.pos[1] + HEADER_H + 4.0);
        let from = out_port(o, patch.node(lfo).unwrap(), 0);
        for ev in drag(from, body) {
            frame(&ctx, &mut patch, &mut st, ev);
        }
        assert_eq!(st.picking.map(|p| (p.0, p.2)), Some((lfo, filter)));
        assert_eq!(patch.wires.len(), before + 1, "nothing wired yet");
    }
}
