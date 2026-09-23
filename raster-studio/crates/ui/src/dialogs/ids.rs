//! Stable widget ids for the dialog controls a test has to be able to click.
//!
//! A swatch that opens the colour picker is exactly the kind of control that
//! can be drawn, look live, and be wired to nothing — the failure mode the
//! whole module is trying to avoid. `egui` hands out an automatic id per
//! widget, which is enough to draw with but not enough to *find* one from a
//! test, so every such control is allocated with an id from this file and the
//! click tests drive the real rectangle by looking it up with
//! [`egui::Context::read_response`].
//!
//! Ids here are namespaced under one root string so they cannot collide with a
//! panel's.

use egui::Id;

use super::adjustment_dialog::ColorTarget;
use super::gradient_editor::{StopKey, StopKind, StopRef};
use super::layer_style::EffectKind;

const ROOT: &str = "raster-studio-dialog";

/// The colour swatch for one layer effect's colour.
pub fn effect_color(kind: EffectKind) -> Id {
    Id::new((ROOT, "effect-color", kind))
}

/// The ramp preview for one layer effect's gradient, which opens the gradient
/// editor on it.
pub fn effect_gradient(kind: EffectKind) -> Id {
    Id::new((ROOT, "effect-gradient", kind))
}

/// The Layer Style dialog's Blending Options page: the blend-mode combo.
pub fn blending_mode() -> Id {
    Id::new((ROOT, "blending-mode"))
}

/// The Layer Style dialog's Blending Options page: the opacity field.
pub fn blending_opacity() -> Id {
    Id::new((ROOT, "blending-opacity"))
}

/// The Layer Style dialog's Blending Options page: the fill-opacity field.
pub fn blending_fill() -> Id {
    Id::new((ROOT, "blending-fill"))
}

/// The Fill dialog's contents combo.
pub fn fill_contents() -> Id {
    Id::new((ROOT, "fill-contents"))
}

/// The Fill dialog's colour swatch, which opens the picker.
pub fn fill_color() -> Id {
    Id::new((ROOT, "fill-color"))
}

/// The Fill dialog's pattern combo.
pub fn fill_pattern() -> Id {
    Id::new((ROOT, "fill-pattern"))
}

/// The Fill dialog's blend-mode combo.
pub fn fill_blend() -> Id {
    Id::new((ROOT, "fill-blend"))
}

/// The Fill dialog's opacity field.
pub fn fill_opacity() -> Id {
    Id::new((ROOT, "fill-opacity"))
}

/// The Stroke dialog's width field.
pub fn stroke_width() -> Id {
    Id::new((ROOT, "stroke-width"))
}

/// The Stroke dialog's location combo.
pub fn stroke_location() -> Id {
    Id::new((ROOT, "stroke-location"))
}

/// The Stroke dialog's blend-mode combo.
pub fn stroke_blend() -> Id {
    Id::new((ROOT, "stroke-blend"))
}

/// The Stroke dialog's opacity field.
pub fn stroke_opacity() -> Id {
    Id::new((ROOT, "stroke-opacity"))
}

/// One draggable stop handle on the gradient ramp.
///
/// Keyed by the stop's [`StopKey`] and never by its index. Dragging a stop past
/// its neighbour re-sorts the ramp; an index-keyed id would then hand the
/// in-flight drag to whichever stop moved into that slot, and the neighbour
/// would follow the pointer too. That was a real defect, and
/// `dragging_a_stop_past_its_neighbour_leaves_the_neighbour_alone` is the test
/// that would have caught it.
pub fn gradient_stop_handle(kind: StopKind, key: StopKey) -> Id {
    Id::new((ROOT, "gradient-stop-handle", kind, key))
}

/// The colour swatch in the gradient editor's stop inspector.
pub fn gradient_stop_color(stop: StopRef) -> Id {
    Id::new((ROOT, "gradient-stop-color", stop.kind, stop.index))
}

/// The swatch beside a "Custom" background/fill menu entry. `scope` names the
/// dialog, because New Document and Canvas Size both have one.
pub fn custom_background(scope: &'static str) -> Id {
    Id::new((ROOT, "custom-background", scope))
}

/// The swatch for a generated filter form's colour parameter.
pub fn filter_param_color(key: &'static str) -> Id {
    Id::new((ROOT, "filter-param-color", key))
}

/// A colour swatch in the Adjustments dialog: Photo Filter's colour, or one
/// Gradient Map stop.
pub fn adjustment_color(target: ColorTarget) -> Id {
    Id::new((ROOT, "adjustment-color", target))
}

/// The Adjustments dialog's preview image (or its off-state well).
pub fn adjustment_preview() -> Id {
    Id::new((ROOT, "adjustment-preview"))
}

/// The Levels dialog's histogram well.
pub fn adjustment_histogram() -> Id {
    Id::new((ROOT, "adjustment-histogram"))
}

/// The Trim dialog's basis combo (transparent / top-left / bottom-right).
pub fn trim_basis() -> Id {
    Id::new((ROOT, "trim-basis"))
}

/// The New Guide dialog's orientation combo.
pub fn new_guide_orientation() -> Id {
    Id::new((ROOT, "new-guide-orientation"))
}

/// The Rename Layer dialog's name field.
pub fn rename_layer_name() -> Id {
    Id::new((ROOT, "rename-layer-name"))
}

/// The Duplicate Layer dialog's name field.
pub fn duplicate_layer_name() -> Id {
    Id::new((ROOT, "duplicate-layer-name"))
}

/// One chip in the colour picker's recent list.
pub fn recent_color(index: usize) -> Id {
    Id::new((ROOT, "recent-color", index))
}

/// The colour picker's before and after swatches. Neither is clickable, but
/// both are allocated the same way so `swatch` has one signature.
pub fn compare_swatch(after: bool) -> Id {
    Id::new((ROOT, "compare-swatch", after))
}

/// Select ▸ Color Range…: the preview image, which samples on click.
pub fn color_range_preview() -> Id {
    Id::new((ROOT, "color-range-preview"))
}

/// Select ▸ Color Range…: the Fuzziness slider.
pub fn color_range_fuzziness() -> Id {
    Id::new((ROOT, "color-range-fuzziness"))
}

/// Select ▸ Color Range…: the sampled-colour readout.
pub fn color_range_color() -> Id {
    Id::new((ROOT, "color-range-color"))
}

/// Select ▸ Modify ▸ …: the amount field.
pub fn selection_modify_amount() -> Id {
    Id::new((ROOT, "selection-modify-amount"))
}

/// Select ▸ Save Selection…: the name field.
pub fn save_selection_name() -> Id {
    Id::new((ROOT, "save-selection-name"))
}

/// Select ▸ Load Selection…: the saved-selection combo.
pub fn load_selection_source() -> Id {
    Id::new((ROOT, "load-selection-source"))
}

/// Select ▸ Load Selection…: the operation combo.
pub fn load_selection_operation() -> Id {
    Id::new((ROOT, "load-selection-operation"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_controls_never_share_an_id() {
        let mut ids = vec![
            color_range_preview(),
            color_range_fuzziness(),
            color_range_color(),
            selection_modify_amount(),
            save_selection_name(),
            load_selection_source(),
            load_selection_operation(),
            custom_background("new-document"),
            custom_background("canvas-size"),
            filter_param_color("tint"),
            filter_param_color("shade"),
            recent_color(0),
            recent_color(1),
            compare_swatch(false),
            compare_swatch(true),
            adjustment_color(ColorTarget::PhotoFilter),
            adjustment_color(ColorTarget::GradientStop(0)),
            adjustment_color(ColorTarget::GradientStop(1)),
            adjustment_preview(),
            adjustment_histogram(),
            trim_basis(),
            new_guide_orientation(),
            rename_layer_name(),
            duplicate_layer_name(),
            blending_mode(),
            blending_opacity(),
            blending_fill(),
            gradient_stop_color(StopRef {
                kind: StopKind::Color,
                index: 0,
            }),
            gradient_stop_color(StopRef {
                kind: StopKind::Opacity,
                index: 0,
            }),
            gradient_stop_color(StopRef {
                kind: StopKind::Color,
                index: 1,
            }),
        ];
        ids.extend(EffectKind::ALL.map(effect_color));
        ids.extend(EffectKind::ALL.map(effect_gradient));
        // Real keys, from a real ramp: a handle id has to differ per stop and
        // per ramp, which is the whole reason the key exists.
        let dialog = crate::dialogs::GradientEditorDialog::default();
        for kind in StopKind::ALL {
            for index in 0..dialog.stops(*kind).len() {
                let key = dialog.stop_key(*kind, index).expect("a key per stop");
                ids.push(gradient_stop_handle(*kind, key));
            }
        }
        let unique: std::collections::HashSet<Id> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "two dialog controls share an id");
    }
}
