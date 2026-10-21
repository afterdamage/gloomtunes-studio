//! Dragging browser rows onto the views. A browser row sets a [`Dragged`] payload; a view
//! marks where it can land with [`drop_zone`], which highlights the zone under the pointer
//! while the item is dragged and hands the item over when it is released there. A drop the
//! views do not take is used where it falls (the app decides, see [`take_release`]).

use egui::{Rect, Stroke, Ui};

use crate::views::RowItem;
use crate::GloomTheme;

/// What is being dragged from the browser: the row's item and its name, for the label that
/// follows the pointer.
#[derive(Debug, Clone, PartialEq)]
pub struct Dragged {
    /// The row's item.
    pub item: RowItem,
    /// The row's name.
    pub name: String,
}

/// The item being dragged, if any.
pub fn dragged(ctx: &egui::Context) -> Option<Dragged> {
    egui::DragAndDrop::payload::<Dragged>(ctx).map(|d| (*d).clone())
}

/// A place the dragged item can land: when the item is one `accepts` takes, the zone is
/// outlined while the pointer is over it, and returns the item when it is released there
/// (only one zone takes it: check inner zones before the ones around them).
pub fn drop_zone(
    ui: &Ui,
    theme: &GloomTheme,
    rect: Rect,
    accepts: impl Fn(&RowItem) -> bool,
) -> Option<RowItem> {
    let d = egui::DragAndDrop::payload::<Dragged>(ui.ctx())?;
    if !accepts(&d.item) {
        return None;
    }
    let visible = rect.intersect(ui.clip_rect());
    let pos = ui.input(|i| i.pointer.hover_pos().or(i.pointer.interact_pos()))?;
    if !visible.is_positive() || !visible.contains(pos) {
        return None;
    }
    if ui.input(|i| i.pointer.any_released()) {
        let d = egui::DragAndDrop::take_payload::<Dragged>(ui.ctx())?;
        return Some(d.item.clone());
    }
    ui.painter().with_clip_rect(ui.clip_rect()).rect(
        visible.shrink(1.0),
        theme.radius,
        theme.accent.gamma_multiply(0.12),
        Stroke::new(1.5, theme.accent),
        egui::StrokeKind::Inside,
    );
    None
}

/// The item released this frame that no zone took, if any (call after every view is drawn).
pub fn take_release(ctx: &egui::Context) -> Option<RowItem> {
    if !ctx.input(|i| i.pointer.any_released()) {
        return None;
    }
    egui::DragAndDrop::take_payload::<Dragged>(ctx).map(|d| d.item.clone())
}

/// Draws the dragged item's name next to the pointer.
pub fn paint_dragged(ctx: &egui::Context, theme: &GloomTheme) {
    let Some(d) = egui::DragAndDrop::payload::<Dragged>(ctx) else {
        return;
    };
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("dragged_item"),
    ));
    let galley =
        painter.layout_no_wrap(d.name.clone(), egui::FontId::proportional(13.0), theme.text);
    let at = pos + egui::vec2(14.0, 10.0);
    let r = Rect::from_min_size(at, galley.size()).expand2(egui::vec2(6.0, 3.0));
    painter.rect(
        r,
        theme.radius,
        theme.bg_widget,
        Stroke::new(1.0, theme.accent),
        egui::StrokeKind::Outside,
    );
    painter.galley(at, galley, theme.text);
}

/// True for a sound row.
pub fn is_sample(item: &RowItem) -> bool {
    matches!(item, RowItem::Sample(_))
}

/// True for rows that make or fill a channel: sounds, synth presets, drum kits and plugins.
pub fn is_instrument(item: &RowItem) -> bool {
    matches!(
        item,
        RowItem::Sample(_)
            | RowItem::FactorySynth(_)
            | RowItem::UserSynth(_)
            | RowItem::Kit(_)
            | RowItem::Plugin(_)
    )
}

/// True for rows that go in a mixer slot: effects and plugins (the app checks a plugin's
/// kind).
pub fn is_effect(item: &RowItem) -> bool {
    matches!(item, RowItem::Effect(..) | RowItem::Plugin(_))
}
