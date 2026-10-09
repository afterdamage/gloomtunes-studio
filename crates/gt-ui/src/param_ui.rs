//! Right-click menu and markers shared by every control that has a [`ParamId`].
//!
//! Views call [`param_menu`] with the response of a knob or fader. It marks the control when
//! automation or a modulator drives it, and its context menu offers "Create automation clip",
//! "Add LFO", "Add envelope follower" and "MIDI learn". A choice is left in egui's memory for the app to
//! pick up with [`take_request`] once per frame, so views need no extra plumbing to report it.

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
    /// Attach an LFO.
    AddLfo(ParamId),
    /// Attach an envelope follower.
    AddFollower(ParamId),
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

/// Publishes this frame's markers (call before drawing the views).
pub fn set_marks(ctx: &Context, marks: Arc<ParamMarks>) {
    ctx.data_mut(|d| d.insert_temp(marks_id(), marks));
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
            ("Add LFO", ParamRequest::AddLfo(id)),
            ("Add envelope follower", ParamRequest::AddFollower(id)),
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
}
