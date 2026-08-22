//! Maps [`Tokens`] onto `egui`'s [`Style`](egui::Style).
//!
//! This is the only place in the crate that is allowed to know both the token
//! vocabulary and egui's field names.

use egui::style::{HandleShape, ScrollStyle, Selection, TextCursorStyle, WidgetVisuals, Widgets};
use egui::{vec2, Color32, Context, FontFamily, FontId, Id, Margin, Rounding, Shadow, Stroke};

use crate::theme::{Theme, Tokens};
use crate::tokens::palette::ColorRole;
use crate::tokens::{grid, Elevation, Motion, Palette, ShadowSpec, Space, Srgba, TypeRole};

/// Where the active theme is stashed in the egui [`Context`], so themed
/// primitives can find it without every call site threading it through.
const THEME_ID: &str = "design::active_theme";

/// Token color to egui color.
///
/// [`Srgba`] carries straight alpha and `Color32` is premultiplied, so this
/// must go through `from_rgba_unmultiplied` — passing the bytes directly would
/// wash out every translucent separator.
pub fn color32(c: Srgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

/// Uniform corner rounding.
pub fn rounding(radius_pt: f32) -> Rounding {
    Rounding::same(radius_pt)
}

/// The egui text style a given rung of the type scale is registered under.
///
/// [`TypeRole::Footnote`] and [`TypeRole::Headline`] have no built-in egui
/// counterpart and are registered as named styles.
pub fn text_style(role: TypeRole) -> egui::TextStyle {
    match role {
        TypeRole::Caption => egui::TextStyle::Small,
        TypeRole::Footnote => egui::TextStyle::Name("footnote".into()),
        TypeRole::Body => egui::TextStyle::Body,
        TypeRole::Headline => egui::TextStyle::Name("headline".into()),
        TypeRole::Title => egui::TextStyle::Heading,
    }
}

/// The [`FontId`] for a rung of the type scale.
///
/// Weight is *not* encoded: egui picks faces by family, and this crate does not
/// register any. Applications that load a weighted family should re-register
/// these ids using [`crate::tokens::FontWeight::family_suffix`].
pub fn font_id(tokens: &Tokens, role: TypeRole) -> FontId {
    FontId::new(tokens.type_scale.size_pt(role), FontFamily::Proportional)
}

/// The epaint shadow for an elevation level, tinted by the palette.
pub fn shadow(palette: &Palette, elevation: Elevation) -> Shadow {
    let spec: ShadowSpec = elevation.shadow();
    if spec.is_none() {
        return Shadow::NONE;
    }
    let base = palette.color(ColorRole::ShadowColor);
    let alpha = (f32::from(base.a) * spec.opacity).round().clamp(0.0, 255.0) as u8;
    Shadow {
        offset: vec2(0.0, spec.y_offset_pt),
        blur: spec.blur_pt,
        spread: spec.spread_pt,
        color: color32(base.with_alpha(alpha)),
    }
}

/// Build a complete [`egui::Style`] for a theme. Pure — no context needed, so
/// it can be asserted on in tests.
pub fn style_for(theme: Theme) -> egui::Style {
    let t = theme.tokens();
    let mut style = egui::Style {
        visuals: visuals_for(theme),
        spacing: spacing_for(t),
        ..Default::default()
    };

    for role in TypeRole::ALL {
        style
            .text_styles
            .insert(text_style(*role), font_id(t, *role));
    }
    // Buttons read at body size; egui's `Button` style must not drift larger.
    style
        .text_styles
        .insert(egui::TextStyle::Button, font_id(t, TypeRole::Body));
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        FontId::new(t.type_scale.size_pt(TypeRole::Body), FontFamily::Monospace),
    );

    style.animation_time = Motion::Micro.secs();
    style.interaction.tooltip_delay = 0.5;
    style.interaction.show_tooltips_only_when_still = true;
    style.interaction.selectable_labels = false;
    style.drag_value_text_style = egui::TextStyle::Body;
    style.explanation_tooltips = false;
    style
}

/// The [`egui::Visuals`] half of the style.
pub fn visuals_for(theme: Theme) -> egui::Visuals {
    let t = theme.tokens();
    let p = &t.palette;
    let c = |role: ColorRole| color32(p.color(role));
    let hairline = t.borders.hairline;

    let mut v = if theme.is_dark() {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    v.dark_mode = theme.is_dark();
    v.override_text_color = None;
    v.widgets = widgets_for(theme);
    v.selection = Selection {
        bg_fill: c(ColorRole::SelectionFill),
        stroke: Stroke::new(hairline, c(ColorRole::SelectionStroke)),
    };

    v.hyperlink_color = c(ColorRole::TextLink);
    v.faint_bg_color = c(ColorRole::SurfaceElevated);
    v.extreme_bg_color = c(ColorRole::SurfaceSunken);
    v.code_bg_color = c(ColorRole::SurfaceSunken);
    v.warn_fg_color = c(ColorRole::Warning);
    v.error_fg_color = c(ColorRole::Danger);

    v.window_rounding = rounding(t.radii.large);
    v.window_shadow = shadow(p, Elevation::Modal);
    v.window_fill = c(ColorRole::SurfaceOverlay);
    v.window_stroke = Stroke::new(hairline, c(ColorRole::SeparatorHairline));
    v.window_highlight_topmost = false;

    v.menu_rounding = rounding(t.radii.medium);
    v.panel_fill = c(ColorRole::SurfacePanel);
    v.popup_shadow = shadow(p, Elevation::Overlay);

    v.resize_corner_size = Space::Medium.pt();
    v.text_cursor = TextCursorStyle {
        stroke: Stroke::new(1.5_f32, c(ColorRole::Accent)),
        ..Default::default()
    };
    v.clip_rect_margin = 3.0;
    v.button_frame = true;
    // Apple-style chrome: no boxes around headers, no ladder lines in trees,
    // no zebra striping. Hierarchy comes from spacing and weight.
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = false;
    v.striped = false;
    v.slider_trailing_fill = true;
    v.handle_shape = HandleShape::Circle;
    v.interact_cursor = None;
    v.image_loading_spinners = false;
    v
}

/// Per-interaction-state widget colors.
pub fn widgets_for(theme: Theme) -> Widgets {
    let t = theme.tokens();
    let p = &t.palette;
    let c = |role: ColorRole| color32(p.color(role));
    let r = rounding(t.radii.medium);
    let hairline = t.borders.hairline;

    // Nothing grows on hover: `expansion` stays 0 everywhere so a hovered
    // control never nudges its neighbours.
    Widgets {
        noninteractive: WidgetVisuals {
            bg_fill: c(ColorRole::SurfacePanel),
            weak_bg_fill: c(ColorRole::SurfacePanel),
            bg_stroke: Stroke::new(hairline, c(ColorRole::SeparatorHairline)),
            rounding: r,
            fg_stroke: Stroke::new(1.0_f32, c(ColorRole::TextPrimary)),
            expansion: 0.0,
        },
        inactive: WidgetVisuals {
            bg_fill: c(ColorRole::ControlFill),
            weak_bg_fill: c(ColorRole::ControlFill),
            bg_stroke: Stroke::new(hairline, c(ColorRole::ControlStroke)),
            rounding: r,
            fg_stroke: Stroke::new(1.0_f32, c(ColorRole::TextPrimary)),
            expansion: 0.0,
        },
        hovered: WidgetVisuals {
            bg_fill: c(ColorRole::ControlFillHovered),
            weak_bg_fill: c(ColorRole::ControlFillHovered),
            bg_stroke: Stroke::new(hairline, c(ColorRole::ControlStrokeStrong)),
            rounding: r,
            fg_stroke: Stroke::new(1.0_f32, c(ColorRole::TextPrimary)),
            expansion: 0.0,
        },
        active: WidgetVisuals {
            bg_fill: c(ColorRole::ControlFillActive),
            weak_bg_fill: c(ColorRole::ControlFillActive),
            bg_stroke: Stroke::new(hairline, c(ColorRole::SelectionStroke)),
            rounding: r,
            fg_stroke: Stroke::new(1.0_f32, c(ColorRole::TextPrimary)),
            expansion: 0.0,
        },
        open: WidgetVisuals {
            bg_fill: c(ColorRole::ControlFillHovered),
            weak_bg_fill: c(ColorRole::ControlFillHovered),
            bg_stroke: Stroke::new(hairline, c(ColorRole::ControlStrokeStrong)),
            rounding: r,
            fg_stroke: Stroke::new(1.0_f32, c(ColorRole::TextPrimary)),
            expansion: 0.0,
        },
    }
}

/// The [`egui::style::Spacing`] half of the style.
///
/// Every scalar set here lands on the [`UNIT_PT`](crate::tokens::UNIT_PT) grid.
/// The two seams inside a
/// single control — `button_padding.y` and `menu_spacing` — use the one
/// sanctioned half-unit, [`Space::Hair`]. The only member that is *not*
/// grid-derived is `scroll`, whose sub-metrics are egui's own
/// [`ScrollStyle::thin`] preset; this function only overrides its `floating`
/// flag. [`grid_spacing_fields`] and [`hair_spacing_fields`] enumerate the two
/// sets and are what the `spacing_lands_on_the_grid` gate iterates.
pub fn spacing_for(t: &Tokens) -> egui::style::Spacing {
    let m = &t.metrics;
    let mut s = egui::style::Spacing {
        item_spacing: vec2(Space::Small.pt(), Space::XSmall.pt()),
        window_margin: Margin::same(m.panel_padding),
        menu_margin: Margin::same(Space::XSmall.pt()),
        button_padding: vec2(Space::Medium.pt(), Space::Hair.pt()),
        indent: Space::Large.pt(),
        interact_size: vec2(Space::XXLarge.pt(), m.control_height),
        slider_width: grid(30.0),
        slider_rail_height: Space::XSmall.pt(),
        combo_width: grid(30.0),
        text_edit_width: grid(45.0),
        icon_width: grid(4.0),
        icon_width_inner: Space::Small.pt(),
        icon_spacing: Space::XSmall.pt(),
        default_area_size: vec2(grid(150.0), grid(100.0)),
        tooltip_width: grid(80.0),
        menu_width: grid(50.0),
        menu_spacing: Space::Hair.pt(),
        indent_ends_with_horizontal_line: false,
        combo_height: grid(80.0),
        scroll: ScrollStyle::thin(),
    };
    s.scroll.floating = true;
    s
}

/// Every scalar of [`spacing_for`]'s result that must be a whole multiple of
/// [`UNIT_PT`](crate::tokens::UNIT_PT), paired with the field path it came from.
///
/// Vec2 and Margin members are expanded into their components, so a regression
/// in either axis is caught. `scroll` is excluded by design — see
/// [`spacing_for`].
pub fn grid_spacing_fields(s: &egui::style::Spacing) -> Vec<(&'static str, f32)> {
    vec![
        ("item_spacing.x", s.item_spacing.x),
        ("item_spacing.y", s.item_spacing.y),
        ("window_margin.left", s.window_margin.left),
        ("window_margin.right", s.window_margin.right),
        ("window_margin.top", s.window_margin.top),
        ("window_margin.bottom", s.window_margin.bottom),
        ("menu_margin.left", s.menu_margin.left),
        ("menu_margin.right", s.menu_margin.right),
        ("menu_margin.top", s.menu_margin.top),
        ("menu_margin.bottom", s.menu_margin.bottom),
        ("button_padding.x", s.button_padding.x),
        ("indent", s.indent),
        ("interact_size.x", s.interact_size.x),
        ("interact_size.y", s.interact_size.y),
        ("slider_width", s.slider_width),
        ("slider_rail_height", s.slider_rail_height),
        ("combo_width", s.combo_width),
        ("text_edit_width", s.text_edit_width),
        ("icon_width", s.icon_width),
        ("icon_width_inner", s.icon_width_inner),
        ("icon_spacing", s.icon_spacing),
        ("default_area_size.x", s.default_area_size.x),
        ("default_area_size.y", s.default_area_size.y),
        ("tooltip_width", s.tooltip_width),
        ("menu_width", s.menu_width),
        ("combo_height", s.combo_height),
    ]
}

/// The scalars of [`spacing_for`]'s result that are allowed the sanctioned
/// half-unit [`Space::Hair`], and are held to exactly that value.
pub fn hair_spacing_fields(s: &egui::style::Spacing) -> Vec<(&'static str, f32)> {
    vec![
        ("button_padding.y", s.button_padding.y),
        ("menu_spacing", s.menu_spacing),
    ]
}

/// Install `theme` on `ctx` and remember it, so [`current_theme`] and the
/// themed primitives can read it back.
///
/// Safe to call every frame; it is idempotent.
pub fn apply_theme(ctx: &Context, theme: Theme) {
    ctx.set_style(style_for(theme));
    ctx.data_mut(|d| d.insert_temp(Id::new(THEME_ID), theme));
}

/// The theme last passed to [`apply_theme`] on this context.
///
/// Falls back to [`Theme::default`] when the app never called `apply_theme`, so
/// primitives still draw something coherent.
pub fn current_theme(ctx: &Context) -> Theme {
    ctx.data(|d| d.get_temp::<Theme>(Id::new(THEME_ID)))
        .unwrap_or_default()
}

/// The token bundle for the theme installed on this ui's context.
pub fn current_tokens(ui: &egui::Ui) -> &'static Tokens {
    current_theme(ui.ctx()).tokens()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::UNIT_PT;

    /// `Color32` stores *premultiplied* bytes. Handing it straight-alpha bytes
    /// via `from_rgba_premultiplied` would leave every translucent token at
    /// full brightness — the separators, the selection fill, the accent wash,
    /// the focus ring and every shadow. These assertions are on the stored
    /// bytes, not on a round trip, because a fully saturated channel round
    /// trips identically under *both* conversions and proves nothing.
    #[test]
    fn translucent_tokens_are_premultiplied_not_copied() {
        // The real dark hairline: white at alpha 31.
        let hairline = color32(Srgba::hexa(0xFFFFFF1F));
        assert_eq!(hairline.a(), 31, "alpha must survive untouched");
        assert_ne!(
            hairline.to_array(),
            [255, 255, 255, 31],
            "hairline kept its straight bytes: alpha was never premultiplied"
        );
        // Premultiplying by 31/255 must pull the channel well below 255.
        assert!(
            hairline.r() < 128,
            "hairline red is {} — far too bright for alpha 31",
            hairline.r()
        );

        // A non-saturating, three-way-distinct color: every channel has to
        // scale, and none of them may be left at its straight value.
        let tinted = color32(Srgba::rgba(200, 100, 50, 64));
        assert_eq!(tinted.a(), 64);
        for (channel, straight) in [(tinted.r(), 200), (tinted.g(), 100), (tinted.b(), 50)] {
            assert!(
                channel < straight,
                "channel {channel} was not scaled down from {straight}"
            );
        }
        assert!(
            tinted.r() > tinted.g() && tinted.g() > tinted.b(),
            "hue drifted"
        );

        // Opaque colors are copied verbatim — premultiplying by 1.0 is a no-op.
        assert_eq!(
            color32(Srgba::hex(0x2A6FD4)).to_array(),
            [0x2A, 0x6F, 0xD4, 0xFF]
        );
        assert_eq!(color32(Srgba::TRANSPARENT), Color32::TRANSPARENT);
    }

    #[test]
    fn the_whole_visuals_mapping_comes_from_the_tokens() {
        for theme in Theme::ALL {
            let t = theme.tokens();
            let p = &t.palette;
            let v = visuals_for(*theme);
            let c = |role: ColorRole| color32(p.color(role));

            assert_eq!(
                v.window_shadow,
                shadow(p, Elevation::Modal),
                "{theme:?} window_shadow"
            );
            assert_ne!(v.window_shadow, Shadow::NONE, "{theme:?} window_shadow");
            assert_eq!(
                v.popup_shadow,
                shadow(p, Elevation::Overlay),
                "{theme:?} popup_shadow"
            );
            assert_ne!(v.popup_shadow, Shadow::NONE, "{theme:?} popup_shadow");

            assert_eq!(
                v.selection.bg_fill,
                c(ColorRole::SelectionFill),
                "{theme:?} selection.bg_fill"
            );
            assert_eq!(
                v.selection.stroke,
                Stroke::new(t.borders.hairline, c(ColorRole::SelectionStroke)),
                "{theme:?} selection.stroke"
            );

            assert_eq!(
                v.window_rounding,
                rounding(t.radii.large),
                "{theme:?} window_rounding"
            );
            assert_eq!(
                v.menu_rounding,
                rounding(t.radii.medium),
                "{theme:?} menu_rounding"
            );
            assert_eq!(
                v.window_fill,
                c(ColorRole::SurfaceOverlay),
                "{theme:?} window_fill"
            );
            assert_eq!(
                v.window_stroke,
                Stroke::new(t.borders.hairline, c(ColorRole::SeparatorHairline)),
                "{theme:?} window_stroke"
            );

            assert_eq!(
                v.extreme_bg_color,
                c(ColorRole::SurfaceSunken),
                "{theme:?} extreme_bg"
            );
            assert_eq!(
                v.code_bg_color,
                c(ColorRole::SurfaceSunken),
                "{theme:?} code_bg"
            );
            assert_eq!(
                v.faint_bg_color,
                c(ColorRole::SurfaceElevated),
                "{theme:?} faint_bg"
            );
            assert_eq!(
                v.hyperlink_color,
                c(ColorRole::TextLink),
                "{theme:?} hyperlink"
            );
            assert_eq!(v.warn_fg_color, c(ColorRole::Warning), "{theme:?} warn_fg");
            assert_eq!(v.error_fg_color, c(ColorRole::Danger), "{theme:?} error_fg");
            assert_eq!(
                v.text_cursor.stroke.color,
                c(ColorRole::Accent),
                "{theme:?} text_cursor"
            );
            assert_eq!(v.dark_mode, theme.is_dark(), "{theme:?} dark_mode");
        }
    }

    #[test]
    fn the_installed_style_carries_the_whole_visuals_mapping() {
        // `apply_theme` must install exactly what `visuals_for` computes; a
        // field dropped on the way to the context is invisible otherwise.
        for theme in Theme::ALL {
            let ctx = Context::default();
            apply_theme(&ctx, *theme);
            let installed = ctx.style();
            let expected = visuals_for(*theme);
            assert_eq!(
                installed.visuals.window_shadow, expected.window_shadow,
                "{theme:?}"
            );
            assert_eq!(
                installed.visuals.popup_shadow, expected.popup_shadow,
                "{theme:?}"
            );
            assert_eq!(installed.visuals.selection, expected.selection, "{theme:?}");
            assert_eq!(
                installed.visuals.window_rounding, expected.window_rounding,
                "{theme:?}"
            );
        }
    }

    #[test]
    fn panel_fill_comes_from_the_palette_in_both_themes() {
        for theme in Theme::ALL {
            let expected = color32(theme.palette().color(ColorRole::SurfacePanel));
            assert_eq!(visuals_for(*theme).panel_fill, expected, "{theme:?}");
        }
    }

    #[test]
    fn every_interaction_state_has_a_distinct_fill() {
        for theme in Theme::ALL {
            let w = widgets_for(*theme);
            assert_ne!(w.inactive.bg_fill, w.hovered.bg_fill, "{theme:?}");
            assert_ne!(w.hovered.bg_fill, w.active.bg_fill, "{theme:?}");
            for state in [&w.inactive, &w.hovered, &w.active, &w.open] {
                // egui requires bg_fill to never be fully transparent.
                assert!(state.bg_fill.a() > 0, "{theme:?}");
                assert_eq!(state.expansion, 0.0, "{theme:?}");
            }
        }
    }

    #[test]
    fn text_styles_cover_the_whole_scale_and_stay_ordered() {
        for theme in Theme::ALL {
            let style = style_for(*theme);
            let mut previous = 0.0;
            for role in TypeRole::ALL {
                let id = style
                    .text_styles
                    .get(&text_style(*role))
                    .unwrap_or_else(|| panic!("{role:?} not registered"));
                assert!(id.size > previous, "{role:?} at {}", id.size);
                previous = id.size;
            }
        }
    }

    #[test]
    fn shadows_scale_the_palette_alpha_and_never_point_upward() {
        let p = Theme::Dark.palette();
        assert_eq!(shadow(p, Elevation::Flat), Shadow::NONE);
        let raised = shadow(p, Elevation::Raised);
        let modal = shadow(p, Elevation::Modal);
        assert!(modal.blur > raised.blur);
        assert!(modal.color.a() > raised.color.a());
        assert!(raised.offset.y > 0.0);
    }

    #[test]
    fn spacing_lands_on_the_grid() {
        for theme in Theme::ALL {
            let t = theme.tokens();
            let s = spacing_for(t);

            let grid_fields = grid_spacing_fields(&s);
            // Guards the enumeration itself: dropping a row from
            // `grid_spacing_fields` would otherwise silently shrink the gate.
            assert_eq!(
                grid_fields.len(),
                26,
                "{theme:?}: the grid field enumeration changed size"
            );
            for (name, value) in grid_fields {
                assert!(value > 0.0, "{theme:?}: {name} = {value} is not positive");
                assert_eq!(
                    value % UNIT_PT,
                    0.0,
                    "{theme:?}: {name} = {value} is off the {UNIT_PT}pt grid"
                );
            }

            let hair_fields = hair_spacing_fields(&s);
            assert_eq!(hair_fields.len(), 2);
            for (name, value) in hair_fields {
                assert_eq!(
                    value,
                    Space::Hair.pt(),
                    "{theme:?}: {name} = {value} is neither a whole unit nor the \
                     sanctioned half-unit"
                );
            }

            // The two token-sourced values the rest of the app aligns to.
            assert_eq!(s.interact_size.y, t.metrics.control_height, "{theme:?}");
            assert_eq!(s.window_margin.left, t.metrics.panel_padding, "{theme:?}");
        }
    }

    #[test]
    fn theme_round_trips_through_the_context() {
        let ctx = Context::default();
        assert_eq!(current_theme(&ctx), Theme::default());
        apply_theme(&ctx, Theme::Light);
        assert_eq!(current_theme(&ctx), Theme::Light);
        apply_theme(&ctx, Theme::Dark);
        assert_eq!(current_theme(&ctx), Theme::Dark);
    }

    #[test]
    fn applying_the_theme_installs_the_style() {
        let ctx = Context::default();
        apply_theme(&ctx, Theme::Light);
        let installed = ctx.style();
        assert_eq!(
            installed.visuals.panel_fill,
            color32(Theme::Light.palette().color(ColorRole::SurfacePanel))
        );
        assert!(!installed.visuals.dark_mode);
    }
}
