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

/// Smallest and largest body text size the theme editor allows, in points.
pub const FONT_SIZE_RANGE: (f32, f32) = (10.0, 16.0);
/// Largest corner radius the theme editor allows.
pub const MAX_RADIUS: u8 = 6;

/// `a` blended towards `b` by `t` (0..1), per channel.
pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// `#rrggbb` for a colour.
pub fn color_to_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// Parses `#rrggbb` (the `#` is optional).
pub fn color_from_hex(text: &str) -> Option<Color32> {
    let h = text.trim().trim_start_matches('#');
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?))
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

    /// Accent colours offered by the theme editor and the first-run wizard.
    pub const ACCENTS: [(&'static str, Color32); 6] = [
        ("Ember", Color32::from_rgb(0xe2, 0x73, 0x3a)),
        ("Blood", Color32::from_rgb(0xc8, 0x32, 0x3c)),
        ("Toxic", Color32::from_rgb(0x8c, 0xc8, 0x3c)),
        ("Ice", Color32::from_rgb(0x4a, 0xa8, 0xd8)),
        ("Violet", Color32::from_rgb(0x9a, 0x6a, 0xe0)),
        ("Bone", Color32::from_rgb(0xd8, 0xcf, 0xb8)),
    ];

    /// Sets the accent and derives its dim variant (selection backgrounds) from it, about a
    /// third of the way from the deepest background.
    pub fn set_accent(&mut self, accent: Color32) {
        self.accent = accent;
        self.accent_dim = lerp_color(self.bg_deep, accent, 0.35);
    }

    /// Every colour token with its name, for the theme editor and the settings file.
    pub fn colors_mut(&mut self) -> [(&'static str, &mut Color32); 10] {
        [
            ("bg_deep", &mut self.bg_deep),
            ("bg_panel", &mut self.bg_panel),
            ("bg_widget", &mut self.bg_widget),
            ("bg_widget_hover", &mut self.bg_widget_hover),
            ("stroke", &mut self.stroke),
            ("text", &mut self.text),
            ("text_dim", &mut self.text_dim),
            ("accent", &mut self.accent),
            ("accent_dim", &mut self.accent_dim),
            ("warn", &mut self.warn),
        ]
    }

    /// Keeps sizes in the range the layout was designed for.
    pub fn clamp_sizes(&mut self) {
        self.font_size = self.font_size.clamp(FONT_SIZE_RANGE.0, FONT_SIZE_RANGE.1);
        self.radius = self.radius.min(MAX_RADIUS);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let mut t = GloomTheme::gloom();
        for (_, c) in t.colors_mut() {
            assert_eq!(color_from_hex(&color_to_hex(*c)), Some(*c));
        }
        assert_eq!(
            color_from_hex("e2733a"),
            Some(Color32::from_rgb(0xe2, 0x73, 0x3a))
        );
        assert_eq!(color_from_hex("#e2733"), None);
        assert_eq!(color_from_hex("#zz0000"), None);
    }

    #[test]
    fn the_default_accent_dim_matches_the_derived_one() {
        // Keeps `set_accent` consistent with the hand-picked Gloom palette.
        let g = GloomTheme::gloom();
        let mut t = g.clone();
        t.set_accent(g.accent);
        let d = |a: u8, b: u8| a.abs_diff(b);
        assert!(d(t.accent_dim.r(), g.accent_dim.r()) <= 8);
        assert!(d(t.accent_dim.g(), g.accent_dim.g()) <= 8);
        assert!(d(t.accent_dim.b(), g.accent_dim.b()) <= 8);
    }

    #[test]
    fn sizes_are_clamped() {
        let mut t = GloomTheme::gloom();
        t.font_size = 40.0;
        t.radius = 99;
        t.clamp_sizes();
        assert_eq!(t.font_size, FONT_SIZE_RANGE.1);
        assert_eq!(t.radius, MAX_RADIUS);
    }
}
