//! Modal and modeless dialogs.
//!
//! All of them share one layout grammar — a clear title, the content, and a
//! right-aligned action row whose primary action comes last — so the app never
//! makes the user re-learn where the confirm button is. Escape cancels, Enter
//! confirms, and every control is reachable from the keyboard.
//!
//! A dialog's *state* is a plain struct with its own validation, separate from
//! its drawing. Confirming produces a command (or a settings value); cancelling
//! produces nothing. That split is what lets the interesting parts — aspect
//! locking, anchor arithmetic, colour conversions, gradient stop ordering — be
//! tested without a window.
//!
//! # The shape every dialog has
//!
//! * A state struct with pure mutators that keep their own invariants.
//! * An implementation of [`Dialog`]: a title, a primary action label, a
//!   `confirm()` that is `Some` exactly when the state is committable, and a
//!   `blocked_reason()` that is `Some` exactly when it is not.
//! * A `show(&mut self, ctx, ..) -> DialogOutcome<DialogAction>` that draws one
//!   frame and folds the keyboard and the action row into one answer. The five
//!   dialogs that can open a colour picker take a
//!   `sampler: Option<&dyn ScreenSampler>` as well and pass it through; the
//!   rest take only the context.
//!
//! [`resolve`] is the piece that makes the keyboard contract testable: it turns
//! (dialog, one frame's keys) into an outcome with no drawing involved, so
//! `every_dialog_confirms_to_a_valid_action_and_cancels_to_nothing` can assert
//! it across the whole set rather than one dialog at a time.
//!
//! # Picking a colour
//!
//! Layer effects, gradient stops, custom backgrounds and colour-valued filter
//! parameters all need a colour picker, and none of them can grow one of their
//! own or hand the job to the shell. [`color_edit::ColorEdit`] is the single
//! seam: a swatch click opens the picker against a target the host names, and a
//! frame later the chosen colour comes back paired with that target for the
//! host to write.
//!
//! [`controls::swatch`] is `#[must_use]` for the same reason. Every swatch is a
//! control that opens that picker; one whose `Response` is dropped is a control
//! that looks live and does nothing, and the compiler now says so. Where an
//! effect genuinely has no single colour — Bevel & Emboss, the gradient and
//! pattern overlays — no swatch is drawn at all, which
//! `the_effects_without_a_swatch_do_not_draw_one` pins.
//!
//! # Filters get their dialogs for free
//!
//! There is exactly one filter dialog, generated from a parameter schema — see
//! [`filter_dialog`]. Adding a [`FilterSpec`] to [`filter_dialog::FILTERS`] is
//! the entire cost of a new filter's UI, for **all five** parameter kinds: the
//! colour arm is covered by a test-only `FilterSpec`, because no shipping
//! filter takes a colour yet and an untested generated arm is how that arm
//! became a dead control in the first place.

pub mod action;
pub mod brush_editor;
pub mod canvas_size;
pub mod chrome;
pub mod color_edit;
pub mod color_picker;
pub mod controls;
pub mod export_as;
pub mod filter_dialog;
pub mod gradient_editor;
pub mod ids;
pub mod image_size;
pub mod layer_style;
pub mod new_document;
pub mod preferences;
pub mod sizes;
pub mod units;

pub use action::DialogAction;
pub use brush_editor::BrushEditorDialog;
pub use canvas_size::{Anchor, CanvasSizeDialog, CanvasSizeSpec, Change, EdgeChange, Side};
pub use chrome::{
    action_row, action_row_with_extras, resolve, Dialog, DialogButton, DialogKeys, DialogOutcome,
    ModalStyle,
};
pub use color_edit::ColorEdit;
pub use color_picker::{ColorPickerDialog, ColorValue, Eyedropper, RecentColors, ScreenSampler};
pub use export_as::{ExportAsDialog, ExportEntry, ExportJob, PreviewSource};
pub use filter_dialog::{
    filter_by_id, FilterDialog, FilterGroup, FilterInvocation, FilterParams, FilterSpec, ParamValue,
};
pub use gradient_editor::{GradientEditorDialog, StopKind, StopRef};
pub use image_size::{ImageSizeDialog, ImageSizeSpec};
pub use layer_style::{shadow_offset, EffectKind, LayerStyleDialog};
pub use new_document::{
    BackgroundContents, ColorMode, DocumentPreset, NewDocumentDialog, NewDocumentSpec, PresetGroup,
};
pub use preferences::{
    Keymap, KeymapError, PreferencesDialog, PrefsSection, Shortcut, ThemeChoice, UiPreferences,
};
pub use units::{format_bytes, ResolutionUnit, Unit};

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    /// One instance of every dialog in the module, in a state that is valid to
    /// confirm.
    ///
    /// The registry the cross-dialog contract tests iterate. A new dialog that
    /// is not added here is a new dialog whose confirm/cancel behaviour nothing
    /// checks — which is why the count is asserted.
    pub fn all_dialogs() -> Vec<Box<dyn Dialog>> {
        let mut dialogs: Vec<Box<dyn Dialog>> = vec![
            Box::new(NewDocumentDialog::default()),
            Box::new(ImageSizeDialog::new(1920, 1080, 72.0)),
            Box::new(CanvasSizeDialog::new(1920, 1080, 72.0)),
            Box::new(ExportAsDialog::new(
                800,
                600,
                "Sketch",
                PreviewSource::placeholder(32, 32),
            )),
            Box::new(ColorPickerDialog::new(ColorValue::WHITE)),
            Box::new(GradientEditorDialog::default()),
            Box::new(BrushEditorDialog::default()),
            Box::new(LayerStyleDialog::new(
                layer_model::LayerId::new(),
                "Headline",
                layer_model::LayerEffects::default(),
            )),
            Box::new(PreferencesDialog::default()),
        ];
        for filter in filter_dialog::FILTERS {
            dialogs.push(Box::new(FilterDialog::with_placeholder(filter)));
        }
        dialogs
    }

    /// One [`DialogAction`] of every variant.
    pub fn one_of_each_action() -> Vec<DialogAction> {
        let export = ExportAsDialog::new(800, 600, "Sketch", PreviewSource::placeholder(16, 16));
        vec![
            DialogAction::Command(Box::new(editor_core::Command::SetLayerProperties {
                layer_id: layer_model::LayerId::new(),
                patch: editor_core::LayerPatch::default(),
            })),
            DialogAction::NewDocument(Box::new(NewDocumentDialog::default().spec())),
            DialogAction::ResizeImage(ImageSizeDialog::new(64, 64, 72.0).spec()),
            DialogAction::ResizeCanvas(CanvasSizeDialog::new(64, 64, 72.0).spec()),
            DialogAction::Export(Box::new(export.job())),
            DialogAction::SetColor(ColorValue::WHITE),
            DialogAction::SetBrush {
                name: "Custom Brush".to_string(),
                settings: Box::default(),
            },
            DialogAction::SetGradient(Box::default()),
            DialogAction::SetPreferences(Box::default()),
            DialogAction::RunFilter(Box::new(
                FilterDialog::with_placeholder(&filter_dialog::FILTERS[0]).invocation(),
            )),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::all_dialogs;
    use super::*;

    #[test]
    fn every_dialog_confirms_to_a_valid_action_and_cancels_to_nothing() {
        for dialog in all_dialogs() {
            let confirmed = resolve(dialog.as_ref(), DialogKeys::CONFIRM);
            match confirmed {
                DialogOutcome::Confirmed(action) => assert!(
                    action.is_valid(),
                    "{} confirmed to an invalid action: {action:?}",
                    dialog.title()
                ),
                other => panic!("{} did not confirm: {other:?}", dialog.title()),
            }
            assert_eq!(
                resolve(dialog.as_ref(), DialogKeys::CANCEL),
                DialogOutcome::Cancelled,
                "{} did not cancel cleanly",
                dialog.title()
            );
        }
    }

    #[test]
    fn no_dialog_decides_anything_without_a_keystroke() {
        for dialog in all_dialogs() {
            assert!(
                resolve(dialog.as_ref(), DialogKeys::NONE).is_open(),
                "{} closed itself",
                dialog.title()
            );
        }
    }

    #[test]
    fn escape_beats_enter_in_every_dialog() {
        let both = DialogKeys {
            confirm: true,
            cancel: true,
        };
        for dialog in all_dialogs() {
            assert_eq!(
                resolve(dialog.as_ref(), both),
                DialogOutcome::Cancelled,
                "{} committed while backing out",
                dialog.title()
            );
        }
    }

    #[test]
    fn a_blocked_primary_action_always_has_a_reason() {
        for dialog in all_dialogs() {
            assert_eq!(
                dialog.confirm().is_none(),
                dialog.blocked_reason().is_some(),
                "{}: confirm() and blocked_reason() disagree",
                dialog.title()
            );
        }
    }

    #[test]
    fn every_dialog_has_a_title_and_a_specific_primary_label() {
        for dialog in all_dialogs() {
            assert!(!dialog.title().is_empty());
            assert!(!dialog.confirm_label().is_empty());
            assert_ne!(
                dialog.confirm_label(),
                "OK",
                "{} should say what its button does",
                dialog.title()
            );
        }
    }

    #[test]
    fn the_registry_covers_every_dialog_in_the_module() {
        // Nine hand-written dialogs plus one generated dialog per filter. If a
        // dialog is added without being registered, the contract tests above
        // would silently stop covering it — so the count is pinned.
        assert_eq!(all_dialogs().len(), 9 + filter_dialog::FILTERS.len());
    }

    #[test]
    fn confirming_twice_produces_the_same_action() {
        // `Dialog::confirm` is documented as pure. A dialog that mutated on
        // confirm would make the Enter key and the button disagree.
        for dialog in all_dialogs() {
            let first = dialog.confirm();
            let second = dialog.confirm();
            assert_eq!(first, second, "{} is not pure", dialog.title());
        }
    }
}
