//! The Gloom theme: every colour, size and spacing value of the UI, defined in one place.
//!
//! Near-black backgrounds in a few close steps for depth, muted desaturated greys for chrome,
//! high-contrast text, and exactly one strong accent colour. Change the look here and only here.

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Visuals};

/// All design tokens of a theme.
#[derive(Debug, Clone, PartialEq)]
pub struct GloomTheme {
    /// Window background, the darkest layer.
    pub bg_deep: Color32,
    /// Panels and frames.
    pub bg_panel: Color32,
    /// Idle widgets (buttons, combo boxes, meter tracks).
    pub bg_widget: Color32,
    /// Hovered widgets.
    pub bg_widget_hover: Color32,
    /// Hairlines and borders.
    pub stroke: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text: labels, units, hints.
    pub text_dim: Color32,
    /// The one strong accent: active states, selection, the meter fill.
    pub accent: Color32,
    /// The accent at low intensity, for selection backgrounds.
    pub accent_dim: Color32,
    /// Warnings and clipping. Used sparingly so the accent stays dominant.
    pub warn: Color32,
    /// Base body text size in points.
    pub font_size: f32,
    /// Corner radius of widgets in points.
    pub radius: u8,
}

impl Default for GloomTheme {
    fn default() -> Self {
        Self::gloom()
    }
}

impl GloomTheme {
    /// The default dark "Gloom" theme with an ember accent.
    pub fn gloom() -> Self {
        Self {
            bg_deep: Color32::from_rgb(0x0e, 0x0e, 0x11),
            bg_panel: Color32::from_rgb(0x15, 0x15, 0x19),
            bg_widget: Color32::from_rgb(0x1f, 0x1f, 0x25),
            bg_widget_hover: Color32::from_rgb(0x2a, 0x2a, 0x32),
            stroke: Color32::from_rgb(0x34, 0x34, 0x3d),
            text: Color32::from_rgb(0xe4, 0xe2, 0xe8),
            text_dim: Color32::from_rgb(0x8a, 0x88, 0x94),
            accent: Color32::from_rgb(0xe2, 0x73, 0x3a),
            accent_dim: Color32::from_rgb(0x5a, 0x30, 0x1c),
            warn: Color32::from_rgb(0xd8, 0x3b, 0x3b),
            font_size: 12.5,
            radius: 2,
        }
    }

    /// Applies the theme to an egui context. Call once at startup, and again after editing it.
    pub fn apply(&self, ctx: &egui::Context) {
        ctx.set_visuals(self.visuals());
        ctx.all_styles_mut(|style| {
            let s = &mut style.spacing;
            s.item_spacing = egui::vec2(6.0, 4.0);
            s.button_padding = egui::vec2(6.0, 2.0);
            s.interact_size = egui::vec2(36.0, 18.0);
            s.window_margin = Margin::same(6);
            s.menu_margin = Margin::same(4);
            style.text_styles = [
                (
                    TextStyle::Small,
                    FontId::new(self.font_size - 2.5, FontFamily::Proportional),
                ),
                (
                    TextStyle::Body,
                    FontId::new(self.font_size, FontFamily::Proportional),
                ),
                (
                    TextStyle::Button,
                    FontId::new(self.font_size, FontFamily::Proportional),
                ),
                (
                    TextStyle::Heading,
                    FontId::new(self.font_size + 4.0, FontFamily::Proportional),
                ),
                (
                    TextStyle::Monospace,
                    FontId::new(self.font_size, FontFamily::Monospace),
                ),
            ]
            .into();
        });
    }

    /// The egui `Visuals` for this theme.
    pub fn visuals(&self) -> Visuals {
        let mut v = Visuals::dark();
        let r = CornerRadius::same(self.radius);
        v.override_text_color = Some(self.text);
        v.panel_fill = self.bg_panel;
        v.window_fill = self.bg_panel;
        v.extreme_bg_color = self.bg_deep;
        v.faint_bg_color = self.bg_widget;
        v.window_stroke = Stroke::new(1.0, self.stroke);
        v.window_corner_radius = r;
        v.menu_corner_radius = r;
        v.selection.bg_fill = self.accent_dim;
        v.selection.stroke = Stroke::new(1.0, self.accent);
        v.hyperlink_color = self.accent;
        v.warn_fg_color = self.warn;
        v.error_fg_color = self.warn;

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = self.bg_panel;
        w.noninteractive.weak_bg_fill = self.bg_panel;
        w.noninteractive.bg_stroke = Stroke::new(1.0, self.stroke);
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.text_dim);
        w.noninteractive.corner_radius = r;
        for (state, fill) in [
            (&mut w.inactive, self.bg_widget),
            (&mut w.hovered, self.bg_widget_hover),
            (&mut w.active, self.accent_dim),
            (&mut w.open, self.bg_widget_hover),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::new(1.0, self.stroke);
            state.fg_stroke = Stroke::new(1.0, self.text);
            state.corner_radius = r;
            state.expansion = 0.0;
        }
        w.hovered.bg_stroke = Stroke::new(1.0, self.text_dim);
        w.active.bg_stroke = Stroke::new(1.0, self.accent);
        v
    }
}
