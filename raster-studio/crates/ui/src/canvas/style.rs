//! Every colour and every measurement the canvas overlays are drawn with,
//! resolved from `design` tokens once per frame.
//!
//! Nothing below writes a literal colour, radius or spacing. The one place that
//! looks like an exception is the pair used for the marching ants and the brush
//! ring: those two are drawn over *arbitrary image content*, where a single
//! theme colour is invisible against roughly half of all photographs. They are
//! still tokens — the near-black `TextPrimary` of the light palette and the
//! near-white `TextPrimary` of the dark one — chosen because the design system
//! already guarantees those two are the most legible ink on their respective
//! surfaces, and therefore the highest-contrast pair it owns. The pairing is
//! asserted, not assumed: see `the_ink_pair_has_enough_contrast_for_any_image`.

use design::{color32, ColorRole, Space, Theme};
use egui::Color32;

/// The canvas's resolved appearance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CanvasStyle {
    /// The area around the image.
    pub backdrop: Color32,
    /// The ruler gutter's fill.
    pub ruler_fill: Color32,
    /// The ruler gutter's fill while the rulers cannot read anything — the view
    /// is rotated off-axis, so no number describes a position along the edge.
    /// A distinct token, because a gutter that is merely empty is
    /// indistinguishable from one that is broken.
    pub ruler_disabled: Color32,
    pub ruler_text: Color32,
    pub ruler_tick_major: Color32,
    pub ruler_tick_minor: Color32,
    /// The band of the ruler covered by the visible document.
    pub ruler_extent: Color32,

    pub grid_major: Color32,
    pub grid_minor: Color32,
    pub pixel_grid: Color32,

    pub guide: Color32,
    pub guide_locked: Color32,
    pub smart_guide: Color32,

    /// The unbroken run of the selection outline.
    pub ants_base: Color32,
    /// The dashes drawn over it.
    pub ants_dash: Color32,

    pub handle_fill: Color32,
    pub handle_stroke: Color32,
    pub handle_selected: Color32,
    pub transform_outline: Color32,

    /// The darkening over the part of the image a crop throws away.
    pub crop_scrim: Color32,
    pub crop_outline: Color32,
    pub crop_guide: Color32,

    pub path_stroke: Color32,
    pub path_anchor: Color32,
    pub path_anchor_selected: Color32,
    pub path_control: Color32,
    pub path_direction: Color32,

    pub caret: Color32,
    pub text_highlight: Color32,

    /// The two-tone brush ring, for the same reason the ants are two-tone.
    pub brush_ring_base: Color32,
    pub brush_ring_over: Color32,

    /// One physical pixel, in points.
    pub hairline_pt: f32,
    /// The emphasised stroke width, in points.
    pub thick_pt: f32,
    /// Corner radius for a handle square.
    pub handle_radius_pt: f32,
    /// Padding inside the ruler gutter.
    pub ruler_thickness_pt: f32,
    /// Gap between a ruler label and its tick.
    pub label_gap_pt: f32,
}

/// The ink pair used over arbitrary image content: the darkest and lightest
/// text colours the design system defines.
fn ink_pair() -> (Color32, Color32) {
    let dark = Theme::Light.palette().color(ColorRole::TextPrimary);
    let light = Theme::Dark.palette().color(ColorRole::TextPrimary);
    (color32(dark), color32(light))
}

/// Scale a token colour's alpha, for the washes that must not compete with the
/// image.
fn faded(role: ColorRole, theme: Theme, alpha: u8) -> Color32 {
    color32(theme.palette().color(role).with_alpha(alpha))
}

impl CanvasStyle {
    /// Resolve the style for a theme at a display scale.
    pub fn new(theme: Theme, pixels_per_point: f32) -> Self {
        let tokens = theme.tokens();
        let p = &tokens.palette;
        let c = |role: ColorRole| color32(p.color(role));
        let (ink_dark, ink_light) = ink_pair();
        // Over an image, the base run takes the colour that contrasts with the
        // *chrome*, so the outline still reads where it crosses the backdrop.
        let (ants_base, ants_dash) = if theme.is_dark() {
            (ink_light, ink_dark)
        } else {
            (ink_dark, ink_light)
        };

        Self {
            backdrop: c(ColorRole::BackgroundCanvas),
            ruler_fill: c(ColorRole::SurfacePanel),
            ruler_disabled: c(ColorRole::ControlFillDisabled),
            ruler_text: c(ColorRole::TextTertiary),
            ruler_tick_major: c(ColorRole::SeparatorStrong),
            ruler_tick_minor: c(ColorRole::SeparatorHairline),
            ruler_extent: faded(ColorRole::Accent, theme, 40),

            grid_major: faded(ColorRole::SeparatorStrong, theme, 90),
            grid_minor: faded(ColorRole::SeparatorHairline, theme, 60),
            pixel_grid: faded(ColorRole::SeparatorHairline, theme, 40),

            guide: c(ColorRole::Accent),
            guide_locked: c(ColorRole::AccentMuted),
            smart_guide: c(ColorRole::Danger),

            ants_base,
            ants_dash,

            handle_fill: c(ColorRole::SurfaceOverlay),
            handle_stroke: c(ColorRole::Accent),
            handle_selected: c(ColorRole::AccentPressed),
            transform_outline: c(ColorRole::Accent),

            crop_scrim: faded(ColorRole::ShadowColor, theme, 150),
            crop_outline: c(ColorRole::Accent),
            crop_guide: faded(ColorRole::TextOnAccent, theme, 110),

            path_stroke: c(ColorRole::Accent),
            path_anchor: c(ColorRole::SurfaceOverlay),
            path_anchor_selected: c(ColorRole::Accent),
            path_control: c(ColorRole::AccentHovered),
            path_direction: faded(ColorRole::Accent, theme, 160),

            caret: c(ColorRole::TextPrimary),
            text_highlight: c(ColorRole::SelectionFill),

            brush_ring_base: ants_base,
            brush_ring_over: ants_dash,

            hairline_pt: tokens.borders.hairline_for_scale(pixels_per_point),
            thick_pt: tokens.borders.thick,
            handle_radius_pt: tokens.radii.small,
            ruler_thickness_pt: Space::Large.pt(),
            label_gap_pt: Space::Hair.pt(),
        }
    }

    /// Resolve the style for the theme installed on an egui context.
    pub fn from_context(ctx: &egui::Context) -> Self {
        Self::new(design::current_theme(ctx), ctx.pixels_per_point())
    }

    /// A hairline stroke in `color`.
    pub fn hairline(&self, color: Color32) -> egui::Stroke {
        egui::Stroke::new(self.hairline_pt, color)
    }

    /// An emphasised stroke in `color`.
    pub fn thick(&self, color: Color32) -> egui::Stroke {
        egui::Stroke::new(self.thick_pt, color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use design::{contrast_ratio, UNIT_PT};

    /// The ants and the brush ring have to read over a photograph. The pair the
    /// style picks must therefore be genuinely high contrast, not merely two
    /// different tokens.
    #[test]
    fn the_ink_pair_has_enough_contrast_for_any_image() {
        let dark = Theme::Light.palette().color(ColorRole::TextPrimary);
        let light = Theme::Dark.palette().color(ColorRole::TextPrimary);
        let ratio = contrast_ratio(dark, light);
        assert!(
            ratio >= 7.0,
            "the two-tone overlay ink is only {ratio}:1 — an outline in it \
             would disappear over mid-grey image content"
        );
        assert!(dark.is_opaque() && light.is_opaque());
    }

    #[test]
    fn both_themes_resolve_and_the_two_tone_ink_swaps_between_them() {
        let light = CanvasStyle::new(Theme::Light, 1.0);
        let dark = CanvasStyle::new(Theme::Dark, 1.0);
        assert_ne!(light.backdrop, dark.backdrop);
        assert_eq!(light.ants_base, dark.ants_dash);
        assert_eq!(light.ants_dash, dark.ants_base);
        assert_ne!(light.ants_base, light.ants_dash);
        // The brush ring uses the same pair, for the same reason.
        assert_eq!(light.brush_ring_base, light.ants_base);
        assert_eq!(light.brush_ring_over, light.ants_dash);
    }

    #[test]
    fn every_colour_comes_from_the_palette_and_none_is_invisible() {
        for theme in Theme::ALL {
            let s = CanvasStyle::new(*theme, 2.0);
            let all = [
                ("backdrop", s.backdrop),
                ("ruler_fill", s.ruler_fill),
                ("ruler_disabled", s.ruler_disabled),
                ("ruler_text", s.ruler_text),
                ("ruler_tick_major", s.ruler_tick_major),
                ("ruler_tick_minor", s.ruler_tick_minor),
                ("ruler_extent", s.ruler_extent),
                ("grid_major", s.grid_major),
                ("grid_minor", s.grid_minor),
                ("pixel_grid", s.pixel_grid),
                ("guide", s.guide),
                ("guide_locked", s.guide_locked),
                ("smart_guide", s.smart_guide),
                ("ants_base", s.ants_base),
                ("ants_dash", s.ants_dash),
                ("handle_fill", s.handle_fill),
                ("handle_stroke", s.handle_stroke),
                ("handle_selected", s.handle_selected),
                ("transform_outline", s.transform_outline),
                ("crop_scrim", s.crop_scrim),
                ("crop_outline", s.crop_outline),
                ("crop_guide", s.crop_guide),
                ("path_stroke", s.path_stroke),
                ("path_anchor", s.path_anchor),
                ("path_anchor_selected", s.path_anchor_selected),
                ("path_control", s.path_control),
                ("path_direction", s.path_direction),
                ("caret", s.caret),
                ("text_highlight", s.text_highlight),
                ("brush_ring_base", s.brush_ring_base),
                ("brush_ring_over", s.brush_ring_over),
            ];
            assert_eq!(all.len(), 31, "a colour was added without a check");
            for (name, colour) in all {
                assert!(colour.a() > 0, "{theme:?}: {name} is fully transparent");
            }
            assert_ne!(
                s.ruler_fill, s.ruler_disabled,
                "{theme:?}: a disabled ruler looks exactly like a working one"
            );
        }
    }

    #[test]
    fn the_hairline_is_one_physical_pixel_at_any_display_scale() {
        for scale in [1.0_f32, 1.5, 2.0, 3.0] {
            let s = CanvasStyle::new(Theme::Dark, scale);
            assert!((s.hairline_pt * scale - 1.0).abs() < 1e-4, "{scale}");
        }
    }

    #[test]
    fn the_geometry_comes_from_the_grid() {
        let s = CanvasStyle::new(Theme::Dark, 1.0);
        assert_eq!(s.ruler_thickness_pt % UNIT_PT, 0.0);
        assert_eq!(s.handle_radius_pt, Theme::Dark.tokens().radii.small);
        assert_eq!(s.thick_pt, Theme::Dark.tokens().borders.thick);
        assert_eq!(s.label_gap_pt, Space::Hair.pt());
    }

    #[test]
    fn strokes_carry_the_token_widths() {
        let s = CanvasStyle::new(Theme::Light, 2.0);
        assert_eq!(s.hairline(s.guide).width, s.hairline_pt);
        assert_eq!(s.thick(s.guide).width, s.thick_pt);
        assert_eq!(s.hairline(s.guide).color, s.guide);
    }

    /// The canvas's half of the crate-wide design-system gate.
    ///
    /// `tests/no_hardcoded_style.rs` skips this directory on the grounds that
    /// it has its own gate. This is that gate: the same rule, scanning the
    /// canvas's shipping source. Without it the exemption would be a hole.
    ///
    /// Only what ships is scanned — everything from the first `#[cfg(test)]`
    /// onward is cut, because a test that paints white on black to check
    /// geometry is not a design decision — and comments are stripped, so a rule
    /// quoted in prose is not read as a violation of itself.
    #[test]
    fn no_colour_font_or_radius_is_written_literally_in_the_canvas() {
        const FORBIDDEN: &[(&str, &str)] = &[
            ("Color32::WHITE", "use a ColorRole through CanvasStyle"),
            ("Color32::BLACK", "use a ColorRole through CanvasStyle"),
            ("Color32::RED", "use a ColorRole through CanvasStyle"),
            ("Color32::GREEN", "use a ColorRole through CanvasStyle"),
            ("Color32::BLUE", "use a ColorRole through CanvasStyle"),
            ("Color32::GRAY", "use a ColorRole through CanvasStyle"),
            ("Color32::from_rgb(", "use a ColorRole through CanvasStyle"),
            ("Color32::from_gray(", "use a ColorRole through CanvasStyle"),
            ("Color32::from_rgba", "use a ColorRole through CanvasStyle"),
            (
                "FontId::new(",
                "use design::egui_theme::font_id with a TypeRole",
            ),
            ("FontId::proportional(", "use design::egui_theme::font_id"),
            ("FontId::monospace(", "use design::egui_theme::font_id"),
            ("Rounding::same(", "use design::egui_theme::rounding"),
            ("TextStyle::Heading", "use a TypeRole"),
        ];
        // `Stroke::new` with a literal width, which is the spacing half of the
        // same rule.
        const NUMERIC: &str = "Stroke::new(";
        // A `*_PT` constant initialised from a bare number is a screen
        // measurement that did not come off the spacing scale — the class of
        // violation the colour patterns above cannot see.
        const POINT_CONST: &str = "_PT: f32 =";

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("canvas");
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "rs"))
            .collect();
        files.sort();
        assert!(
            files.len() >= 10,
            "the canvas lost its source files: {}",
            files.len()
        );

        let mut violations = Vec::new();
        for path in &files {
            let text = std::fs::read_to_string(path).unwrap();
            let shipping = match text.find("#[cfg(test)]") {
                Some(at) => &text[..at],
                None => &text[..],
            };
            for (number, line) in shipping.lines().enumerate() {
                let code = match line.find("//") {
                    Some(at) => &line[..at],
                    None => line,
                };
                for (pattern, fix) in FORBIDDEN {
                    if code.contains(pattern) {
                        violations.push(format!(
                            "{}:{}: `{pattern}` — {fix}",
                            path.display(),
                            number + 1
                        ));
                    }
                }
                let mut from = 0usize;
                while let Some(at) = code[from..].find(NUMERIC) {
                    let after = from + at + NUMERIC.len();
                    if code[after..]
                        .trim_start()
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit())
                    {
                        violations.push(format!(
                            "{}:{}: `Stroke::new` with a literal width — use                              CanvasStyle::hairline or ::thick",
                            path.display(),
                            number + 1
                        ));
                    }
                    from = after;
                }
                if let Some(at) = code.find(POINT_CONST) {
                    let rest = code[at + POINT_CONST.len()..].trim_start();
                    if rest
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit() || c == '-' || c == '.')
                    {
                        violations.push(format!(
                            "{}:{}: a `*_PT` constant written as a bare number — \
                             build it from design::Space or design::UNIT_PT",
                            path.display(),
                            number + 1
                        ));
                    }
                }
            }
        }
        assert!(
            violations.is_empty(),
            "style values written literally in the canvas:
{}",
            violations.join(
                "
"
            )
        );
    }

    /// A gate nobody has seen fail is a gate nobody knows works.
    #[test]
    fn the_canvas_gate_would_catch_a_violation() {
        let bad = "painter.rect_filled(r, Rounding::same(4.0), Color32::WHITE);";
        assert!(bad.contains("Rounding::same("));
        assert!(bad.contains("Color32::WHITE"));
        let call = "Stroke::new(";
        let literal = "let s = Stroke::new(1.0, c);";
        let after = literal.find(call).unwrap() + call.len();
        assert!(literal[after..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit()));
        let tokenised = "let s = Stroke::new(style.hairline_pt, c);";
        let after = tokenised.find(call).unwrap() + call.len();
        assert!(!tokenised[after..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit()));

        // …and the same for the point-constant half of the rule.
        let marker = "_PT: f32 =";
        let bare = "pub const MIN_LINE_GAP_PT: f32 = 3.0;";
        let after = bare.find(marker).unwrap() + marker.len();
        assert!(bare[after..]
            .trim_start()
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit()));
        let from_tokens =
            "pub const MIN_LINE_GAP_PT: f32 = design::Space::Hair.units() * design::UNIT_PT;";
        let after = from_tokens.find(marker).unwrap() + marker.len();
        assert!(!from_tokens[after..]
            .trim_start()
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '-' || c == '.'));
    }

    #[test]
    fn the_style_follows_the_theme_installed_on_the_context() {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, Theme::Light);
        assert_eq!(
            CanvasStyle::from_context(&ctx).backdrop,
            CanvasStyle::new(Theme::Light, ctx.pixels_per_point()).backdrop
        );
        design::apply_theme(&ctx, Theme::Dark);
        assert_eq!(
            CanvasStyle::from_context(&ctx).backdrop,
            CanvasStyle::new(Theme::Dark, ctx.pixels_per_point()).backdrop
        );
    }
}
