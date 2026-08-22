//! Themed primitives.
//!
//! Every function reads its colors from the theme installed by
//! [`apply_theme`](crate::apply_theme), so a call site never names a color.
//! All of them restore the ambient style before returning: a primitive styles
//! itself, never the widgets that follow it.

use egui::{vec2, Align, Align2, Color32, Layout, Response, Sense, Stroke, Ui};

use crate::egui_theme::{color32, current_tokens, font_id, rounding, shadow};
use crate::theme::Tokens;
use crate::tokens::palette::ColorRole;
use crate::tokens::{Elevation, Radius, Space, TextRole, TypeRole};

/// A (text role, type rung) pair that one of the primitives below actually
/// paints.
///
/// Declared as data, and read by the primitives themselves, so the pairing a
/// gate checks is the pairing that ships: a text role held only to the 3:1
/// large-text floor must never be rendered at a rung WCAG calls small text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextPairing {
    /// Which primitive paints it; used in gate failure messages.
    pub owner: &'static str,
    pub text: TextRole,
    pub size: TypeRole,
}

impl TextPairing {
    const fn new(owner: &'static str, text: TextRole, size: TypeRole) -> Self {
        Self { owner, text, size }
    }

    /// Every pairing the primitives in this module paint on a surface.
    ///
    /// Excludes text drawn on an accent fill (see
    /// [`ColorRole::TextOnAccent`]), which is gated separately because it does
    /// not sit on a [`SurfaceRole`](crate::tokens::SurfaceRole).
    pub const ALL: &'static [TextPairing] = &[
        SECTION_HEADER_TITLE,
        INSPECTOR_LABEL,
        LIST_ROW_SELECTED,
        LIST_ROW_UNSELECTED,
        BUTTON_LABEL,
        BUTTON_LABEL_QUIET,
    ];
}

/// Title text of [`section_header`].
pub const SECTION_HEADER_TITLE: TextPairing =
    TextPairing::new("section_header", TextRole::Tertiary, TypeRole::Footnote);
/// Left-hand label of [`inspector_field`] and [`slider_row`].
pub const INSPECTOR_LABEL: TextPairing =
    TextPairing::new("inspector label", TextRole::Secondary, TypeRole::Body);
/// Text of a selected [`list_row`].
pub const LIST_ROW_SELECTED: TextPairing =
    TextPairing::new("list_row selected", TextRole::Primary, TypeRole::Body);
/// Text of an unselected [`list_row`].
pub const LIST_ROW_UNSELECTED: TextPairing =
    TextPairing::new("list_row", TextRole::Secondary, TypeRole::Body);
/// Label of a neutral button in its emphasised state.
pub const BUTTON_LABEL: TextPairing =
    TextPairing::new("button label", TextRole::Primary, TypeRole::Body);
/// Label of a ghost button at rest, and of an unselected segment.
pub const BUTTON_LABEL_QUIET: TextPairing =
    TextPairing::new("quiet button label", TextRole::Secondary, TypeRole::Body);

/// Colors a button uses across its interaction states.
struct ButtonSkin {
    fill: Color32,
    fill_hovered: Color32,
    fill_pressed: Color32,
    text: Color32,
    text_dimmed: Color32,
    stroke: Stroke,
}

fn skin_from(tokens: &Tokens, kind: ButtonKind) -> ButtonSkin {
    let p = &tokens.palette;
    let c = |role: ColorRole| color32(p.color(role));
    let t = |pairing: TextPairing| color32(p.text(pairing.text));
    match kind {
        ButtonKind::Primary => ButtonSkin {
            fill: c(ColorRole::Accent),
            fill_hovered: c(ColorRole::AccentHovered),
            fill_pressed: c(ColorRole::AccentPressed),
            text: c(ColorRole::TextOnAccent),
            text_dimmed: c(ColorRole::TextOnAccent),
            stroke: Stroke::NONE,
        },
        ButtonKind::Secondary => ButtonSkin {
            fill: c(ColorRole::ControlFill),
            fill_hovered: c(ColorRole::ControlFillHovered),
            fill_pressed: c(ColorRole::ControlFillActive),
            text: t(BUTTON_LABEL),
            text_dimmed: t(BUTTON_LABEL),
            stroke: Stroke::new(tokens.borders.hairline, c(ColorRole::ControlStroke)),
        },
        ButtonKind::Ghost => ButtonSkin {
            fill: Color32::TRANSPARENT,
            fill_hovered: c(ColorRole::ControlFillHovered),
            fill_pressed: c(ColorRole::ControlFillActive),
            text: t(BUTTON_LABEL),
            text_dimmed: t(BUTTON_LABEL_QUIET),
            stroke: Stroke::NONE,
        },
    }
}

#[derive(Clone, Copy)]
enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
}

/// Shared button body. `min_size` lets the toolbar variant force a square.
fn skinned_button(
    ui: &mut Ui,
    label: &str,
    skin: &ButtonSkin,
    min_size: egui::Vec2,
    radius: f32,
) -> Response {
    ui.scope(|ui| {
        let w = &mut ui.style_mut().visuals.widgets;
        for (state, fill) in [
            (&mut w.inactive, skin.fill),
            (&mut w.hovered, skin.fill_hovered),
            (&mut w.active, skin.fill_pressed),
            (&mut w.open, skin.fill_hovered),
        ] {
            state.weak_bg_fill = fill;
            state.bg_stroke = skin.stroke;
            state.expansion = 0.0;
        }
        w.inactive.fg_stroke.color = skin.text_dimmed;
        w.hovered.fg_stroke.color = skin.text;
        w.active.fg_stroke.color = skin.text;
        w.open.fg_stroke.color = skin.text;
        ui.add(
            egui::Button::new(label)
                .min_size(min_size)
                .rounding(rounding(radius)),
        )
    })
    .inner
}

fn control_radius(tokens: &Tokens) -> f32 {
    Radius::Medium.resolve(&tokens.radii, tokens.metrics.control_height)
}

/// The filled, accent-colored button. At most one per view: it is the answer to
/// the question the view is asking.
pub fn primary_button(ui: &mut Ui, label: &str) -> Response {
    let t = current_tokens(ui);
    let skin = skin_from(t, ButtonKind::Primary);
    skinned_button(
        ui,
        label,
        &skin,
        vec2(0.0, t.metrics.control_height),
        control_radius(t),
    )
}

/// The bordered neutral button, for every other action in a row.
pub fn secondary_button(ui: &mut Ui, label: &str) -> Response {
    let t = current_tokens(ui);
    let skin = skin_from(t, ButtonKind::Secondary);
    skinned_button(
        ui,
        label,
        &skin,
        vec2(0.0, t.metrics.control_height),
        control_radius(t),
    )
}

/// Chrome-free button: no fill and no border until hovered. For dense bars and
/// destructive-adjacent actions that should not draw the eye.
pub fn ghost_button(ui: &mut Ui, label: &str) -> Response {
    let t = current_tokens(ui);
    let skin = skin_from(t, ButtonKind::Ghost);
    skinned_button(
        ui,
        label,
        &skin,
        vec2(0.0, t.metrics.control_height),
        control_radius(t),
    )
}

/// A square icon button for the toolbar. `selected` gives it the accent wash
/// used for the active tool.
///
/// The selected state carries the accent in the *wash and the border*, never in
/// the glyph: accent-on-accent-wash measures below 3:1 in the dark palette, and
/// the active-tool indicator is the state that most needs to be legible. The
/// glyph stays [`ColorRole::TextPrimary`] — see the
/// `the_selected_toolbar_glyph_is_legible_over_its_accent_wash` gate in
/// `tests/token_gates.rs`.
pub fn toolbar_icon_button(ui: &mut Ui, glyph: &str, tooltip: &str, selected: bool) -> Response {
    let t = current_tokens(ui);
    let p = &t.palette;
    let mut skin = skin_from(t, ButtonKind::Ghost);
    if selected {
        skin.fill = color32(p.color(ColorRole::AccentSubtle));
        skin.fill_hovered = skin.fill;
        skin.text = color32(p.text(BUTTON_LABEL.text));
        skin.text_dimmed = skin.text;
        skin.stroke = Stroke::new(t.borders.hairline, color32(p.color(ColorRole::Accent)));
    }
    let side = t.metrics.toolbar_button;
    let radius = Radius::Medium.resolve(&t.radii, side);
    // A `Button` is `max(min_size.x, glyph_width + 2 * button_padding.x)` wide,
    // so the ambient horizontal padding could silently un-square the toolbar.
    // Drop it here: `min_size` alone decides the box.
    let response = ui
        .scope(|ui| {
            ui.spacing_mut().button_padding.x = 0.0;
            skinned_button(ui, glyph, &skin, vec2(side, side), radius)
        })
        .inner;
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

/// A segmented control: mutually exclusive options in one capsule.
///
/// `selected` is clamped into `options` and returns `true` whenever it changed,
/// including when an out-of-range index had to be corrected. A no-op on an
/// empty `options`.
pub fn segmented_control(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash,
    selected: &mut usize,
    options: &[&str],
) -> bool {
    if options.is_empty() {
        return false;
    }
    let mut changed = false;
    if *selected >= options.len() {
        *selected = options.len() - 1;
        changed = true;
    }

    let t = current_tokens(ui);
    let p = &t.palette;
    let c = |role: ColorRole| color32(p.color(role));
    let height = t.metrics.control_height;
    let pad = 2.0;
    let outer = Radius::Medium.resolve(&t.radii, height);
    let inner = (outer - pad).max(0.0);
    let segment_shadow = shadow(p, Elevation::Raised);

    ui.push_id(id_salt, |ui| {
        egui::Frame::none()
            .fill(c(ColorRole::SurfaceSunken))
            .rounding(rounding(outer))
            .inner_margin(egui::Margin::same(pad))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = pad;
                // `min_size` on a Button is a floor, not a height: egui still
                // raises the button to `spacing.interact_size.y`, which the
                // ambient style sets to the full `control_height`. Left alone,
                // each segment would be `control_height` tall and the 2pt frame
                // margin on each side would push the whole control 2 * pad
                // above every other control in the row. Lower the floor for the
                // segments so `segment + 2 * pad == control_height`.
                ui.spacing_mut().interact_size.y = height - 2.0 * pad;
                ui.horizontal(|ui| {
                    for (index, option) in options.iter().enumerate() {
                        let is_selected = index == *selected;
                        let mut skin = ButtonSkin {
                            fill: Color32::TRANSPARENT,
                            fill_hovered: c(ColorRole::ControlFillHovered),
                            fill_pressed: c(ColorRole::ControlFillActive),
                            text: color32(p.text(BUTTON_LABEL.text)),
                            text_dimmed: color32(p.text(BUTTON_LABEL_QUIET.text)),
                            stroke: Stroke::NONE,
                        };
                        if is_selected {
                            skin.fill = c(ColorRole::SurfaceElevated);
                            skin.fill_hovered = skin.fill;
                            skin.fill_pressed = skin.fill;
                            skin.text_dimmed = skin.text;
                        }
                        // Reserve the slot *before* the button so the shadow
                        // lands behind it; the lifted segment is the only
                        // shadow in the control and is what makes the choice
                        // legible.
                        let shadow_slot = ui.painter().add(egui::Shape::Noop);
                        let response =
                            skinned_button(ui, option, &skin, vec2(0.0, height - 2.0 * pad), inner);
                        if is_selected && ui.is_rect_visible(response.rect) {
                            ui.painter().set(
                                shadow_slot,
                                segment_shadow.as_shape(response.rect, rounding(inner)),
                            );
                        }
                        if response.clicked() && !is_selected {
                            *selected = index;
                            changed = true;
                        }
                    }
                });
            });
    });
    changed
}

/// A label + slider + numeric field on one line — the inspector workhorse.
///
/// The returned [`Response`] is the union of the slider and the field, so
/// `changed()` is true when either edited the value.
pub fn slider_row(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> Response {
    let t = current_tokens(ui);
    let p = &t.palette;
    let label_color = color32(p.text(INSPECTOR_LABEL.text));
    let label_width = t.metrics.inspector_label_width;
    let field_width = t.metrics.numeric_field_width;
    let height = t.metrics.control_height;

    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            vec2(label_width, height),
            Layout::right_to_left(Align::Center),
            |ui| {
                ui.label(egui::RichText::new(label).color(label_color));
            },
        );
        let remaining = (ui.available_width() - field_width - Space::Small.pt()).max(48.0);
        let slider = ui.add_sized(
            vec2(remaining, height),
            egui::Slider::new(value, range.clone()).show_value(false),
        );
        let field = ui.add_sized(
            vec2(field_width, height),
            egui::DragValue::new(value).range(range).max_decimals(2),
        );
        slider | field
    })
    .inner
}

/// A labelled inspector row whose right-hand side is whatever `add_contents`
/// draws. Labels share [`Metrics::inspector_label_width`], so every field in a
/// panel lines up on one column.
///
/// [`Metrics::inspector_label_width`]: crate::tokens::Metrics::inspector_label_width
pub fn inspector_field<R>(
    ui: &mut Ui,
    label: &str,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    let t = current_tokens(ui);
    let label_color = color32(t.palette.text(INSPECTOR_LABEL.text));
    let label_width = t.metrics.inspector_label_width;
    let height = t.metrics.control_height;

    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            vec2(label_width, height),
            Layout::right_to_left(Align::Center),
            |ui| {
                ui.label(egui::RichText::new(label).color(label_color));
            },
        );
        add_contents(ui)
    })
}

/// A panel section header: quiet tertiary text over a hairline rule.
///
/// Sections are separated by weight and space, not by boxes — the header adds
/// [`Space::Medium`] above itself and [`Space::XSmall`] below the rule.
pub fn section_header(ui: &mut Ui, title: &str) -> Response {
    let t = current_tokens(ui);
    let p = &t.palette;
    ui.add_space(Space::Medium.pt());
    let response = ui.label(
        egui::RichText::new(title)
            .color(color32(p.text(SECTION_HEADER_TITLE.text)))
            .font(font_id(t, SECTION_HEADER_TITLE.size)),
    );
    ui.add_space(Space::Hair.pt());
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), t.borders.hairline),
        Sense::hover(),
    );
    if ui.is_rect_visible(rect) {
        ui.painter().hline(
            rect.x_range(),
            rect.center().y,
            Stroke::new(
                t.borders.hairline,
                color32(p.color(ColorRole::SeparatorHairline)),
            ),
        );
    }
    ui.add_space(Space::XSmall.pt());
    response
}

/// A full-width list row with a selection state, for layers and assets.
///
/// The row is exactly [`Metrics::list_row_height`] tall so lists stay on the
/// grid, and it senses clicks across its whole width, not just the text.
///
/// [`Metrics::list_row_height`]: crate::tokens::Metrics::list_row_height
pub fn list_row(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let t = current_tokens(ui);
    let p = &t.palette;
    let height = t.metrics.list_row_height;
    let width = ui.available_width().max(t.metrics.min_hit_target);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());

    if ui.is_rect_visible(rect) {
        let fill = if selected {
            color32(p.color(ColorRole::SelectionFill))
        } else if response.hovered() {
            color32(p.color(ColorRole::ControlFillHovered))
        } else {
            Color32::TRANSPARENT
        };
        let radius = Radius::Medium.resolve(&t.radii, height);
        if fill != Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, rounding(radius), fill);
        }
        let pairing = if selected {
            LIST_ROW_SELECTED
        } else {
            LIST_ROW_UNSELECTED
        };
        ui.painter().text(
            egui::pos2(rect.left() + Space::Small.pt(), rect.center().y),
            Align2::LEFT_CENTER,
            label,
            font_id(t, pairing.size),
            color32(p.text(pairing.text)),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egui_theme::apply_theme;
    use crate::theme::Theme;
    use egui::epaint::ClippedShape;
    use egui::{Context, Pos2, Rect, Shape};

    /// Run one headless egui frame and return whatever `f` produced plus every
    /// untessellated shape the frame emitted.
    fn frame<R>(theme: Theme, f: impl FnOnce(&mut Ui) -> R) -> (R, Vec<ClippedShape>) {
        let ctx = Context::default();
        apply_theme(&ctx, theme);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
            ..Default::default()
        };
        let mut f = Some(f);
        let mut out = None;
        let output = ctx.run(input, |ctx| {
            if let Some(f) = f.take() {
                egui::CentralPanel::default().show(ctx, |ui| {
                    out = Some(f(ui));
                });
            }
        });
        (out.expect("frame body never ran"), output.shapes)
    }

    /// Every fill color painted by a rectangle in `shapes`, recursing into
    /// `Shape::Vec` so nesting does not hide a fill.
    fn rect_fills(shapes: &[ClippedShape]) -> Vec<Color32> {
        fn walk(shape: &Shape, out: &mut Vec<Color32>) {
            match shape {
                Shape::Rect(r) => out.push(r.fill),
                Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    /// Every color a glyph run in `shapes` resolves to.
    ///
    /// egui resolves glyph color in three places: an explicit override on the
    /// shape wins, then the color baked into the layout job's sections, and
    /// only a [`Color32::PLACEHOLDER`] section falls back to the color the
    /// painter was handed. All three paths are followed here, or a `RichText`
    /// color would be invisible to the assertion.
    fn text_colors(shapes: &[ClippedShape]) -> Vec<Color32> {
        fn walk(shape: &Shape, out: &mut Vec<Color32>) {
            match shape {
                Shape::Text(t) => {
                    if let Some(override_color) = t.override_text_color {
                        out.push(override_color);
                        return;
                    }
                    if t.galley.job.sections.is_empty() {
                        out.push(t.fallback_color);
                    }
                    for section in &t.galley.job.sections {
                        if section.format.color == Color32::PLACEHOLDER {
                            out.push(t.fallback_color);
                        } else {
                            out.push(section.format.color);
                        }
                    }
                }
                Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    /// Every line-segment stroke in `shapes`, recursing into `Shape::Vec`.
    fn line_strokes(shapes: &[ClippedShape]) -> Vec<egui::epaint::PathStroke> {
        fn walk(shape: &Shape, out: &mut Vec<egui::epaint::PathStroke>) {
            match shape {
                Shape::LineSegment { stroke, .. } => out.push(stroke.clone()),
                Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn every_primitive_paints_in_both_themes() {
        for theme in Theme::ALL {
            let ((), shapes) = frame(*theme, |ui| {
                primary_button(ui, "Export");
                secondary_button(ui, "Cancel");
                ghost_button(ui, "Reset");
                toolbar_icon_button(ui, "B", "Brush", true);
                toolbar_icon_button(ui, "E", "", false);
                section_header(ui, "Adjustments");
                let mut seg = 0usize;
                segmented_control(ui, "seg", &mut seg, &["Fit", "Fill", "1:1"]);
                let mut opacity = 50.0f32;
                slider_row(ui, "Opacity", &mut opacity, 0.0..=100.0);
                inspector_field(ui, "Blend", |ui| ui.label("Normal"));
                list_row(ui, "Background", true);
                list_row(ui, "Layer 1", false);
            });
            assert!(!shapes.is_empty(), "{theme:?} painted nothing");
        }
    }

    #[test]
    fn the_primary_button_skin_is_accent_on_accent_with_no_border() {
        for theme in Theme::ALL {
            let p = theme.palette();
            let skin = skin_from(theme.tokens(), ButtonKind::Primary);
            assert_eq!(skin.fill, color32(p.color(ColorRole::Accent)), "{theme:?}");
            assert_eq!(
                skin.fill_hovered,
                color32(p.color(ColorRole::AccentHovered)),
                "{theme:?}"
            );
            assert_eq!(
                skin.fill_pressed,
                color32(p.color(ColorRole::AccentPressed)),
                "{theme:?}"
            );
            assert_eq!(
                skin.text,
                color32(p.color(ColorRole::TextOnAccent)),
                "{theme:?}"
            );
            assert_eq!(skin.text_dimmed, skin.text, "{theme:?}");
            assert_eq!(skin.stroke, Stroke::NONE, "{theme:?}");
        }
    }

    #[test]
    fn the_secondary_button_skin_is_a_control_fill_behind_a_hairline_border() {
        for theme in Theme::ALL {
            let t = theme.tokens();
            let p = &t.palette;
            let skin = skin_from(t, ButtonKind::Secondary);
            assert_eq!(
                skin.fill,
                color32(p.color(ColorRole::ControlFill)),
                "{theme:?}"
            );
            assert_eq!(
                skin.fill_hovered,
                color32(p.color(ColorRole::ControlFillHovered)),
                "{theme:?}"
            );
            assert_eq!(
                skin.fill_pressed,
                color32(p.color(ColorRole::ControlFillActive)),
                "{theme:?}"
            );
            assert_eq!(skin.text, color32(p.text(TextRole::Primary)), "{theme:?}");
            assert_eq!(
                skin.stroke,
                Stroke::new(
                    t.borders.hairline,
                    color32(p.color(ColorRole::ControlStroke))
                ),
                "{theme:?}"
            );
        }
    }

    #[test]
    fn the_ghost_button_skin_has_no_fill_and_no_border_at_rest() {
        for theme in Theme::ALL {
            let p = theme.palette();
            let skin = skin_from(theme.tokens(), ButtonKind::Ghost);
            assert_eq!(skin.fill, Color32::TRANSPARENT, "{theme:?}");
            assert_eq!(skin.stroke, Stroke::NONE, "{theme:?}");
            assert_eq!(
                skin.fill_hovered,
                color32(p.color(ColorRole::ControlFillHovered)),
                "{theme:?}"
            );
            // Quiet at rest, full strength once the pointer commits.
            assert_eq!(
                skin.text_dimmed,
                color32(p.text(TextRole::Secondary)),
                "{theme:?}"
            );
            assert_eq!(skin.text, color32(p.text(TextRole::Primary)), "{theme:?}");
            assert_ne!(skin.text_dimmed, skin.text, "{theme:?}");
        }
    }

    #[test]
    fn a_selected_list_row_paints_the_selection_fill_and_an_idle_one_paints_nothing() {
        for theme in Theme::ALL {
            let expected = color32(theme.palette().color(ColorRole::SelectionFill));
            let ((), selected) = frame(*theme, |ui| {
                list_row(ui, "Background", true);
            });
            assert!(
                rect_fills(&selected).contains(&expected),
                "{theme:?}: selected row never painted {expected:?}, saw {:?}",
                rect_fills(&selected)
            );

            let ((), idle) = frame(*theme, |ui| {
                list_row(ui, "Background", false);
            });
            assert!(
                !rect_fills(&idle).contains(&expected),
                "{theme:?}: unselected row painted the selection fill"
            );
        }
    }

    #[test]
    fn a_selected_toolbar_button_washes_the_background_but_never_the_glyph() {
        for theme in Theme::ALL {
            let p = theme.palette();
            let wash = color32(p.color(ColorRole::AccentSubtle));
            let accent = color32(p.color(ColorRole::Accent));
            let glyph = color32(p.color(ColorRole::TextPrimary));

            let ((), shapes) = frame(*theme, |ui| {
                toolbar_icon_button(ui, "B", "Brush", true);
            });
            assert!(
                rect_fills(&shapes).contains(&wash),
                "{theme:?}: selected toolbar button never painted the accent wash"
            );
            let painted = text_colors(&shapes);
            assert!(
                painted.contains(&glyph),
                "{theme:?}: glyph was not TextPrimary, saw {painted:?}"
            );
            // Accent-on-accent-wash measures 2.66:1 in dark: the accent must
            // stay in the wash and the border, never in the glyph.
            assert!(
                !painted.contains(&accent),
                "{theme:?}: glyph was painted in the accent over its own wash"
            );
        }
    }

    #[test]
    fn list_row_text_switches_role_with_the_selection() {
        for theme in Theme::ALL {
            let p = theme.palette();
            let ((), selected) = frame(*theme, |ui| {
                list_row(ui, "Background", true);
            });
            assert!(
                text_colors(&selected).contains(&color32(p.text(LIST_ROW_SELECTED.text))),
                "{theme:?}: selected row text is not {:?}",
                LIST_ROW_SELECTED.text
            );

            let ((), idle) = frame(*theme, |ui| {
                list_row(ui, "Background", false);
            });
            assert!(
                text_colors(&idle).contains(&color32(p.text(LIST_ROW_UNSELECTED.text))),
                "{theme:?}: unselected row text is not {:?}",
                LIST_ROW_UNSELECTED.text
            );
            assert_ne!(
                color32(p.text(LIST_ROW_SELECTED.text)),
                color32(p.text(LIST_ROW_UNSELECTED.text)),
                "{theme:?}: the two states are indistinguishable"
            );
        }
    }

    #[test]
    fn the_section_header_title_is_quiet_footnote_text() {
        for theme in Theme::ALL {
            let t = theme.tokens();
            let expected = color32(t.palette.text(SECTION_HEADER_TITLE.text));
            let ((), shapes) = frame(*theme, |ui| {
                section_header(ui, "Adjustments");
            });
            let painted = text_colors(&shapes);
            assert!(
                painted.contains(&expected),
                "{theme:?}: header text is not {:?}, saw {painted:?}",
                SECTION_HEADER_TITLE.text
            );
            assert_eq!(
                font_id(t, SECTION_HEADER_TITLE.size).size,
                t.type_scale.size_pt(TypeRole::Footnote)
            );
        }
    }

    #[test]
    fn an_unselected_toolbar_button_paints_no_accent_wash() {
        for theme in Theme::ALL {
            let wash = color32(theme.palette().color(ColorRole::AccentSubtle));
            let ((), shapes) = frame(*theme, |ui| {
                toolbar_icon_button(ui, "E", "", false);
            });
            assert!(
                !rect_fills(&shapes).contains(&wash),
                "{theme:?}: idle toolbar button painted the selected wash"
            );
        }
    }

    #[test]
    fn the_section_header_rule_uses_the_hairline_separator_token() {
        for theme in Theme::ALL {
            let t = theme.tokens();
            let expected = Stroke::new(
                t.borders.hairline,
                color32(t.palette.color(ColorRole::SeparatorHairline)),
            );
            let ((), shapes) = frame(*theme, |ui| {
                section_header(ui, "Adjustments");
            });
            let rules = line_strokes(&shapes);
            let wanted = egui::epaint::PathStroke::from(expected);
            assert!(
                rules.contains(&wanted),
                "{theme:?}: rule stroke {expected:?} not among {rules:?}"
            );
        }
    }

    #[test]
    fn buttons_restore_the_ambient_style() {
        let expected = color32(Theme::Dark.palette().color(ColorRole::ControlFill));
        let (after, _) = frame(Theme::Dark, |ui| {
            primary_button(ui, "Export");
            ui.visuals().widgets.inactive.weak_bg_fill
        });
        assert_eq!(after, expected);
    }

    #[test]
    fn list_rows_are_exactly_one_grid_row_tall() {
        let ((selected, unselected), _) = frame(Theme::Light, |ui| {
            (
                list_row(ui, "Background", true).rect,
                list_row(ui, "Layer 1", false).rect,
            )
        });
        let h = Theme::Light.tokens().metrics.list_row_height;
        assert_eq!(selected.height(), h);
        assert_eq!(unselected.height(), h);
        assert!(selected.width() > 100.0, "row did not fill the panel");
    }

    #[test]
    fn toolbar_buttons_are_square_and_clear_the_min_hit_target() {
        let (rect, _) = frame(Theme::Dark, |ui| {
            toolbar_icon_button(ui, "B", "Brush", false).rect
        });
        let m = Theme::Dark.tokens().metrics;
        assert_eq!(rect.width(), m.toolbar_button);
        assert_eq!(rect.height(), m.toolbar_button);
        assert!(rect.width() >= m.min_hit_target);
    }

    /// The square must come from [`Metrics::toolbar_button`] alone. Without the
    /// padding clamp in `toolbar_icon_button` the box is
    /// `glyph_width + 2 * button_padding.x` wide as soon as that exceeds the
    /// metric, so changing `spacing_for`'s `button_padding` — or picking a wider
    /// glyph — would silently turn every toolbar button into a rectangle.
    ///
    /// [`Metrics::toolbar_button`]: crate::tokens::Metrics::toolbar_button
    #[test]
    fn a_toolbar_button_stays_square_under_a_wide_padding_and_a_wide_glyph() {
        let side = Theme::Dark.tokens().metrics.toolbar_button;
        let (rects, _) = frame(Theme::Dark, |ui| {
            // Far wider than any value `spacing_for` would ever set.
            ui.spacing_mut().button_padding.x = 40.0;
            [
                toolbar_icon_button(ui, "B", "Brush", false).rect,
                toolbar_icon_button(ui, "W", "Wand", true).rect,
            ]
        });
        for rect in rects {
            assert_eq!(rect.width(), side, "toolbar button is not {side}pt wide");
            assert_eq!(rect.height(), side, "toolbar button is not {side}pt tall");
        }
    }

    #[test]
    fn segmented_control_corrects_an_out_of_range_index() {
        let options = ["A", "B", "C"];
        // The boundary: `len()` is one past the last valid index and must be
        // corrected. `> len()` instead of `>= len()` leaves it uncorrected and
        // no segment ever renders as selected.
        let ((changed, selected), _) = frame(Theme::Dark, |ui| {
            let mut selected = options.len();
            let changed = segmented_control(ui, "seg", &mut selected, &options);
            (changed, selected)
        });
        assert!(changed, "index == len() was left uncorrected");
        assert_eq!(selected, options.len() - 1);

        let ((changed, selected), _) = frame(Theme::Dark, |ui| {
            let mut selected = 99usize;
            let changed = segmented_control(ui, "seg", &mut selected, &options);
            (changed, selected)
        });
        assert!(changed);
        assert_eq!(selected, 2);
    }

    #[test]
    fn the_selected_segment_is_lifted_onto_the_elevated_surface() {
        for theme in Theme::ALL {
            let lift = color32(theme.palette().color(ColorRole::SurfaceElevated));
            let sunken = color32(theme.palette().color(ColorRole::SurfaceSunken));
            let ((), shapes) = frame(*theme, |ui| {
                let mut selected = 1usize;
                segmented_control(ui, "seg", &mut selected, &["A", "B", "C"]);
            });
            let fills = rect_fills(&shapes);
            assert!(
                fills.contains(&sunken),
                "{theme:?}: segmented control lost its sunken track"
            );
            assert!(
                fills.contains(&lift),
                "{theme:?}: selected segment was never lifted, saw {fills:?}"
            );
        }
    }

    /// Every control that shares a toolbar row must occupy exactly
    /// [`Metrics::control_height`], or the row will not align.
    ///
    /// The segmented control is the one that can drift: its segments are
    /// `Button`s inside a padded `Frame`, and a `Button`'s height is
    /// `max(min_size.y, spacing.interact_size.y)` — so passing
    /// `height - 2 * pad` as a *minimum* does not stop egui from restoring the
    /// full `control_height` and pushing the frame 2 * pad taller than its
    /// neighbours.
    ///
    /// [`Metrics::control_height`]: crate::tokens::Metrics::control_height
    #[test]
    fn every_control_in_a_row_is_exactly_one_control_height_tall() {
        for theme in Theme::ALL {
            let expected = theme.tokens().metrics.control_height;
            // `ui.scope` reports the child's `min_rect`, i.e. everything the
            // primitive actually claimed — frame margins included.
            let (heights, _) = frame(*theme, |ui| {
                let primary = ui.scope(|ui| primary_button(ui, "Export")).response.rect;
                let secondary = ui.scope(|ui| secondary_button(ui, "Cancel")).response.rect;
                let ghost = ui.scope(|ui| ghost_button(ui, "Reset")).response.rect;
                let segmented = ui
                    .scope(|ui| {
                        let mut selected = 1usize;
                        segmented_control(ui, "seg", &mut selected, &["Fit", "Fill", "1:1"]);
                    })
                    .response
                    .rect;
                let slider = ui
                    .scope(|ui| {
                        let mut value = 50.0f32;
                        slider_row(ui, "Opacity", &mut value, 0.0..=100.0);
                    })
                    .response
                    .rect;
                [
                    ("primary_button", primary.height()),
                    ("secondary_button", secondary.height()),
                    ("ghost_button", ghost.height()),
                    ("segmented_control", segmented.height()),
                    ("slider_row", slider.height()),
                ]
            });
            for (name, height) in heights {
                assert_eq!(
                    height, expected,
                    "{theme:?}: {name} is {height}pt, not {expected}pt"
                );
            }
        }
    }

    #[test]
    fn segmented_control_is_stable_without_input() {
        let ((changed, selected), _) = frame(Theme::Dark, |ui| {
            let mut selected = 1usize;
            let changed = segmented_control(ui, "seg", &mut selected, &["A", "B", "C"]);
            (changed, selected)
        });
        assert!(!changed);
        assert_eq!(selected, 1);
    }

    #[test]
    fn segmented_control_with_no_options_is_a_no_op() {
        let ((changed, selected), _) = frame(Theme::Dark, |ui| {
            let mut selected = 7usize;
            let changed = segmented_control(ui, "seg", &mut selected, &[]);
            (changed, selected)
        });
        assert!(!changed);
        assert_eq!(selected, 7);
    }

    #[test]
    fn slider_row_leaves_an_untouched_value_alone() {
        let (value, _) = frame(Theme::Light, |ui| {
            let mut value = 42.5f32;
            slider_row(ui, "Opacity", &mut value, 0.0..=100.0);
            value
        });
        assert_eq!(value, 42.5);
    }

    #[test]
    fn inspector_field_returns_its_contents() {
        let (answer, _) = frame(Theme::Light, |ui| {
            inspector_field(ui, "Blend", |ui| {
                ui.label("Normal");
                7u32
            })
            .inner
        });
        assert_eq!(answer, 7);
    }
}
