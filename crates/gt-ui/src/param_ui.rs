//! Right-click menu and markers shared by every control that has a [`ParamId`].
//!
//! Views call [`param_menu`] with the response of a knob or fader. It marks the control when
//! automation or a modulator drives it, and its context menu offers "Create automation clip",
//! "Add LFO", "Add envelope follower" and "MIDI learn". A choice is left in egui's memory for the app to
//! pick up with [`take_request`] once per frame, so views need no extra plumbing to report it.
//! Held and changed controls are left the same way for automation recording ([`take_touches`]),
//! and [`param_live`] draws the value automation or modulation gives a control now (D111).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{Context, Id, Response, Stroke};
use gt_core::ParamId;

use crate::GloomTheme;

/// What the user asked for on a control.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamRequest {
    /// Add an automation clip for the parameter at the playhead.
    Automate(ParamId),
    /// Add an automation line for the parameter to the current pattern (D112).
    AutomatePattern(ParamId),
    /// Attach an LFO.
    AddLfo(ParamId),
    /// Attach an envelope follower.
    AddFollower(ParamId),
    /// Attach a formula modulator (Prompt 34).
    AddFormula(ParamId),
    /// Show the parameter's modulators.
    ShowModulators(ParamId),
    /// Bind the next MIDI controller that moves.
    MidiLearn(ParamId),
    /// Remove the MIDI controller binding.
    ForgetMidi(ParamId),
}

/// Which parameters automation or modulation drive, for the markers on their controls.
#[derive(Debug, Clone, Default)]
pub struct ParamMarks {
    /// Targets of automation clips.
    pub automated: HashSet<ParamId>,
    /// Targets of enabled modulators.
    pub modulated: HashSet<ParamId>,
    /// Parameters bound to a MIDI controller, with its description ("CC 74, ch 1").
    pub midi: HashMap<ParamId, String>,
}

fn marks_id() -> Id {
    Id::new("gt_param_marks")
}

fn request_id() -> Id {
    Id::new("gt_param_request")
}

fn touches_id() -> Id {
    Id::new("gt_param_touches")
}

fn live_id() -> Id {
    Id::new("gt_param_live")
}

/// A control the user is holding or changed this frame (for automation recording, D111).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touch {
    /// The parameter.
    pub id: ParamId,
    /// The mouse holds it (a drag or a press); false for a one-off change such as a scroll.
    pub held: bool,
    /// Its value changed this frame.
    pub changed: bool,
}

/// Values automation or modulators give parameters now (plain units), from the engine.
pub type LiveValues = std::collections::HashMap<ParamId, f32>;

/// Publishes the live values (call before drawing the views); controls show them as a ghost.
pub fn set_live(ctx: &Context, values: Arc<LiveValues>) {
    ctx.data_mut(|d| d.insert_temp(live_id(), values));
}

/// Takes the controls held or changed this frame.
pub fn take_touches(ctx: &Context) -> Vec<Touch> {
    ctx.data_mut(|d| d.remove_temp::<Vec<Touch>>(touches_id()))
        .unwrap_or_default()
}

/// Draws the value automation or a modulator gives `id` now on its control: a thin outer
/// ring on a knob, a line across a fader or a slider. `travel` maps a plain value to the
/// control's 0..1 travel. The shape follows the control: tall is a fader, wide a slider (whose
/// rail is the whole rect: pass a response trimmed to it).
pub fn param_live(theme: &GloomTheme, resp: &Response, id: ParamId, travel: impl Fn(f32) -> f32) {
    let live: Option<Arc<LiveValues>> = resp.ctx.data(|d| d.get_temp(live_id()));
    let Some(v) = live.as_deref().and_then(|m| m.get(&id)).copied() else {
        return;
    };
    let t = travel(v).clamp(0.0, 1.0);
    let r = resp.rect;
    let p = resp.ctx.layer_painter(resp.layer_id);
    let stroke = Stroke::new(1.5, theme.text.gamma_multiply(0.75));
    if r.height() > 2.0 * r.width() {
        crate::widgets::fader_ghost(&p, r, t, stroke);
    } else if r.width() > 2.0 * r.height() {
        let x = r.left() + r.height() * 0.5 + t * (r.width() - r.height());
        p.line_segment([egui::pos2(x, r.top()), egui::pos2(x, r.bottom())], stroke);
    } else {
        crate::widgets::knob_ghost(&p, r, t, stroke);
    }
}

/// Publishes this frame's markers (call before drawing the views).
pub fn set_marks(ctx: &Context, marks: Arc<ParamMarks>) {
    ctx.data_mut(|d| d.insert_temp(marks_id(), marks));
}

/// Makes a request as a control's menu would, for controls without [`param_menu`] (the tempo).
pub fn request(ctx: &Context, req: ParamRequest) {
    ctx.data_mut(|d| d.insert_temp(request_id(), req));
}

/// Takes the request a control's menu made this frame, if any.
pub fn take_request(ctx: &Context) -> Option<ParamRequest> {
    ctx.data_mut(|d| {
        let req = d.get_temp::<ParamRequest>(request_id());
        d.remove::<ParamRequest>(request_id());
        req
    })
}

/// Marks `resp`'s control if it is automated (a ring) or modulated (a dot) and gives it the
/// parameter menu.
pub fn param_menu(theme: &GloomTheme, resp: &Response, id: ParamId) {
    // A press with the main button (a right click opens the menu instead).
    let held = resp.dragged()
        || resp.is_pointer_button_down_on() && resp.ctx.input(|i| i.pointer.primary_down());
    if held || resp.changed() {
        let touch = Touch {
            id,
            held,
            changed: resp.changed(),
        };
        resp.ctx.data_mut(|d| {
            d.get_temp_mut_or_default::<Vec<Touch>>(touches_id())
                .push(touch)
        });
    }
    let marks: Option<Arc<ParamMarks>> = resp.ctx.data(|d| d.get_temp(marks_id()));
    let (automated, modulated) = marks.as_deref().map_or((false, false), |m| {
        (m.automated.contains(&id), m.modulated.contains(&id))
    });
    if automated || modulated {
        let c = resp.rect.right_top() + egui::vec2(-3.0, 3.0);
        let p = resp.ctx.layer_painter(resp.layer_id);
        if modulated {
            p.circle_filled(c, 2.5, theme.accent);
        }
        if automated {
            p.circle_stroke(c, 3.5, Stroke::new(1.0, theme.text));
        }
    }
    resp.context_menu(|ui| {
        ui.label(egui::RichText::new(id.label()).color(theme.text_dim));
        let mut items = vec![
            ("Create automation clip", ParamRequest::Automate(id)),
            ("Automate in pattern", ParamRequest::AutomatePattern(id)),
            ("Add LFO", ParamRequest::AddLfo(id)),
            ("Add envelope follower", ParamRequest::AddFollower(id)),
            ("Add formula", ParamRequest::AddFormula(id)),
        ];
        if modulated {
            items.push(("Show modulators", ParamRequest::ShowModulators(id)));
        }
        let bound = marks.as_deref().and_then(|m| m.midi.get(&id));
        let forget = bound.map(|b| format!("Forget MIDI ({b})"));
        items.push(("MIDI learn", ParamRequest::MidiLearn(id)));
        if let Some(f) = &forget {
            items.push((f.as_str(), ParamRequest::ForgetMidi(id)));
        }
        for (text, req) in items {
            if ui.button(text).clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(request_id(), req));
                ui.close();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_taken_once() {
        let ctx = Context::default();
        assert_eq!(take_request(&ctx), None);
        let req = ParamRequest::AddLfo(gt_core::MASTER_VOLUME);
        ctx.data_mut(|d| d.insert_temp(request_id(), req));
        assert_eq!(take_request(&ctx), Some(req));
        assert_eq!(take_request(&ctx), None);
    }

    #[test]
    fn touches_are_taken_once() {
        let ctx = Context::default();
        assert!(take_touches(&ctx).is_empty());
        let t = Touch {
            id: gt_core::MASTER_VOLUME,
            held: true,
            changed: false,
        };
        ctx.data_mut(|d| {
            d.get_temp_mut_or_default::<Vec<Touch>>(touches_id())
                .push(t)
        });
        assert_eq!(take_touches(&ctx), vec![t]);
        assert!(take_touches(&ctx).is_empty());
    }
}
