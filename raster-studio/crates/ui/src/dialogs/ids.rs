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

/// One chip in the colour picker's recent list.
pub fn recent_color(index: usize) -> Id {
    Id::new((ROOT, "recent-color", index))
}

/// The colour picker's before and after swatches. Neither is clickable, but
/// both are allocated the same way so `swatch` has one signature.
pub fn compare_swatch(after: bool) -> Id {
    Id::new((ROOT, "compare-swatch", after))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_controls_never_share_an_id() {
        let mut ids = vec![
            custom_background("new-document"),
            custom_background("canvas-size"),
            filter_param_color("tint"),
            filter_param_color("shade"),
            recent_color(0),
            recent_color(1),
            compare_swatch(false),
            compare_swatch(true),
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
