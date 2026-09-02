//! The quality gates that make the design system checkable rather than
//! decorative. These run against the tokens alone — no window, no GPU.

use std::collections::BTreeSet;

use design::tokens::palette::{ColorRole, MISSING_ROLE_COLOR};
use design::tokens::{contrast_ratio_over, Space, SurfaceRole, TextRole, TypeRole};
use design::{Palette, TextPairing, Theme};

/// Every (text, surface) pair a theme is held to, with its WCAG floor.
///
/// [`TextRole::Disabled`] is skipped: WCAG 2.1 SC 1.4.3 exempts inactive
/// controls, and holding disabled text to 4.5:1 would make it read as enabled.
fn checked_pairs() -> Vec<(TextRole, SurfaceRole, f32)> {
    let mut pairs = Vec::new();
    for text in TextRole::ALL {
        let Some(size) = text.checked_size() else {
            continue;
        };
        for surface in SurfaceRole::ALL {
            pairs.push((*text, *surface, size.min_contrast_aa()));
        }
    }
    pairs
}

#[test]
fn text_meets_wcag_aa_on_every_surface_in_both_themes() {
    let mut failures = Vec::new();
    for theme in Theme::ALL {
        let palette = theme.palette();
        for (text, surface, floor) in checked_pairs() {
            let ratio = palette.text_contrast(text, surface);
            if ratio < floor {
                failures.push(format!(
                    "{:?}: {text:?} on {surface:?} = {ratio:.2}:1, needs {floor:.1}:1",
                    theme
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "WCAG AA failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn text_on_accent_meets_wcag_aa_in_both_themes() {
    // Accent-filled buttons are the one place text does not sit on a surface
    // role, so they need their own check.
    for theme in Theme::ALL {
        let p = theme.palette();
        for fill in [
            ColorRole::Accent,
            ColorRole::AccentHovered,
            ColorRole::AccentPressed,
        ] {
            let ratio = contrast_ratio_over(p.color(ColorRole::TextOnAccent), p.color(fill));
            assert!(
                ratio >= 4.5,
                "{theme:?}: TextOnAccent on {fill:?} = {ratio:.2}:1, needs 4.5:1"
            );
        }
    }
}

#[test]
fn the_selected_toolbar_glyph_is_legible_over_its_accent_wash() {
    // `toolbar_icon_button(selected)` paints the glyph on `AccentSubtle`
    // composited over `SurfacePanel`. The glyph is text, so 4.5:1 applies.
    // Painting the accent itself there measures 2.66:1 in dark — this gate is
    // the reason the glyph stays `TextPrimary`.
    for theme in Theme::ALL {
        let p = theme.palette();
        let wash = p
            .color(ColorRole::AccentSubtle)
            .composite_over(p.surface(SurfaceRole::Panel));
        let ratio = contrast_ratio_over(p.color(ColorRole::TextPrimary), wash);
        assert!(
            ratio >= 4.5,
            "{theme:?}: selected toolbar glyph on the accent wash = {ratio:.2}:1, needs 4.5:1"
        );

        // And the state must still read as "selected": the accent border that
        // carries that meaning is a graphical object, held to SC 1.4.11's 3:1.
        let border = contrast_ratio_over(p.color(ColorRole::Accent), p.surface(SurfaceRole::Panel));
        assert!(
            border >= 3.0,
            "{theme:?}: selected toolbar border on Panel = {border:.2}:1, needs 3.0:1"
        );
    }
}

#[test]
fn no_primitive_renders_a_small_text_role_at_a_large_text_discount() {
    // Guards the shape of the contrast gate itself. A text role checked at the
    // 3:1 large-text floor may only be painted at a rung WCAG actually calls
    // large (>= 18pt, or >= 14pt bold). `section_header` renders Tertiary at
    // 11pt, so Tertiary must be held to 4.5:1.
    let scale = Theme::Dark.tokens().type_scale;
    for pairing in TextPairing::ALL {
        let Some(checked) = pairing.text.checked_size() else {
            continue;
        };
        let required = scale.style(pairing.size).wcag_size();
        assert!(
            checked.min_contrast_aa() >= required.min_contrast_aa(),
            "{}: {:?} is checked at {:.1}:1 but is rendered at {:?} ({}pt), which needs {:.1}:1",
            pairing.owner,
            pairing.text,
            checked.min_contrast_aa(),
            pairing.size,
            scale.size_pt(pairing.size),
            required.min_contrast_aa()
        );
    }
}

#[test]
fn semantic_colors_are_legible_as_foreground_on_a_panel() {
    // Success/Warning/Danger are used as icon and label colors, which WCAG
    // treats as non-text/large content: the 3:1 floor applies.
    for theme in Theme::ALL {
        let p = theme.palette();
        let panel = p.surface(SurfaceRole::Panel);
        for role in [ColorRole::Success, ColorRole::Warning, ColorRole::Danger] {
            let ratio = contrast_ratio_over(p.color(role), panel);
            assert!(
                ratio >= 3.0,
                "{theme:?}: {role:?} on Panel = {ratio:.2}:1, needs 3.0:1"
            );
        }
    }
}

#[test]
fn separators_are_visible_on_the_surfaces_they_divide() {
    // A hairline that resolves to less than 1.2:1 is invisible; a divider that
    // cannot be seen is not a divider.
    for theme in Theme::ALL {
        let p = theme.palette();
        for surface in [
            SurfaceRole::Panel,
            SurfaceRole::Elevated,
            SurfaceRole::Overlay,
        ] {
            let ratio =
                contrast_ratio_over(p.color(ColorRole::SeparatorHairline), p.surface(surface));
            assert!(
                ratio >= 1.2,
                "{theme:?}: hairline on {surface:?} = {ratio:.3}:1"
            );
        }
    }
}

#[test]
fn light_and_dark_define_exactly_the_same_roles() {
    let light = Palette::light();
    let dark = Palette::dark();

    assert_eq!(light.missing_roles(), Vec::<ColorRole>::new());
    assert_eq!(dark.missing_roles(), Vec::<ColorRole>::new());

    let expected: BTreeSet<ColorRole> = ColorRole::ALL.iter().copied().collect();
    assert_eq!(light.defined_roles(), expected, "light palette role set");
    assert_eq!(dark.defined_roles(), expected, "dark palette role set");
    assert_eq!(light.defined_roles(), dark.defined_roles());
}

#[test]
fn no_role_falls_back_to_the_missing_color() {
    for theme in Theme::ALL {
        let p = theme.palette();
        for role in ColorRole::ALL {
            assert_ne!(
                p.color(*role),
                MISSING_ROLE_COLOR,
                "{theme:?} left {role:?} undefined"
            );
        }
    }
}

#[test]
fn surfaces_are_ordered_from_recessed_to_elevated() {
    // Light gets brighter as it rises, dark gets lighter as it rises: in both
    // themes luminance must climb monotonically through the stack, or panels
    // stop reading as layers.
    let order = [
        SurfaceRole::Sunken,
        SurfaceRole::Canvas,
        SurfaceRole::Panel,
        SurfaceRole::Elevated,
    ];
    for theme in Theme::ALL {
        let p = theme.palette();
        for pair in order.windows(2) {
            let lo = p.surface(pair[0]).relative_luminance();
            let hi = p.surface(pair[1]).relative_luminance();
            assert!(
                hi > lo,
                "{theme:?}: {:?} ({lo:.4}) is not below {:?} ({hi:.4})",
                pair[0],
                pair[1]
            );
        }
    }
}

#[test]
fn the_spacing_scale_is_strictly_increasing() {
    for pair in Space::ALL.windows(2) {
        assert!(
            pair[1].pt() > pair[0].pt(),
            "{:?} ({}) !< {:?} ({})",
            pair[0],
            pair[0].pt(),
            pair[1],
            pair[1].pt()
        );
    }
}

#[test]
fn the_type_scale_is_strictly_increasing() {
    let scale = Theme::Dark.tokens().type_scale;
    for pair in TypeRole::ALL.windows(2) {
        assert!(
            scale.size_pt(pair[1]) > scale.size_pt(pair[0]),
            "{:?} ({}) !< {:?} ({})",
            pair[0],
            scale.size_pt(pair[0]),
            pair[1],
            scale.size_pt(pair[1])
        );
    }
}

#[test]
fn the_contrast_gate_actually_catches_a_bad_palette() {
    // Guards the gate itself: a palette with grey-on-grey body text must fail
    // the same check the real palettes pass.
    let mut pairs: Vec<(ColorRole, design::Srgba)> = design::tokens::palette::LIGHT_ROLES.to_vec();
    pairs.push((ColorRole::TextPrimary, design::Srgba::hex(0xC9C9CE)));
    let bad = Palette::from_pairs(false, &pairs);

    assert!(bad.missing_roles().is_empty());
    let ratio = bad.text_contrast(TextRole::Primary, SurfaceRole::Elevated);
    assert!(ratio < 4.5, "sabotaged palette still scored {ratio:.2}:1");
}

#[test]
fn the_density_tokens_cannot_drift_back() {
    // Photopea's compact density, pinned: a row is 20px, a control 20px, the
    // options bar 28px, panel padding 4px, and body type 12pt. The whole P1
    // wave's fit depends on these; a regression here is visible everywhere.
    for theme in Theme::ALL {
        let m = theme.tokens().metrics;
        assert_eq!(m.control_height, 20.0, "{theme:?}");
        assert_eq!(m.list_row_height, 20.0, "{theme:?}");
        assert_eq!(m.toolbar_button, 24.0, "{theme:?}");
        assert_eq!(m.toolbar_height, 28.0, "{theme:?}");
        assert_eq!(m.panel_padding, 4.0, "{theme:?}");
        let body = theme
            .tokens()
            .type_scale
            .size_pt(design::tokens::typography::TypeRole::Body);
        assert_eq!(body, 12.0, "{theme:?}");
    }
}

#[test]
fn a_900px_tall_layers_panel_fits_at_least_twelve_rows() {
    // The density pass's user-visible claim: on a 1440x900 window the Layers
    // panel has room for twelve rows after the chrome above and below it.
    // The chrome's heights are the tokens themselves, so this is arithmetic
    // the tokens cannot quietly break.
    let m = Theme::Dark.tokens().metrics;
    let rows = m.list_row_height;
    // 900px window: menu bar + options bar + document tabs + status bar.
    let chrome = m.toolbar_height * 3.0 + rows;
    // One section header and the panel's own padding.
    let overhead = m.panel_padding * 2.0 + rows;
    let available = 900.0 - chrome - overhead;
    assert!(
        available / rows >= 12.0,
        "only {} rows of {} fit in the 900px panel (available {available})",
        (available / rows) as u32,
        rows
    );
}
