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
//! the entire cost of a new filter's UI, for **all five** parameter kinds.
//!
//! A [`FilterSpec`] is keyed by [`crate::menu::FilterId`], so the catalogue
//! and the Filter menu are one list checked in both directions: every menu
//! entry has a dialog, and no dialog exists for something the menu cannot
//! reach.

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

    /// The type a line implements [`Dialog`] for, if it is such a line.
    ///
    /// Path-tolerant: `impl chrome::Dialog for X` counts, because a dialog
    /// written that way is still a dialog the registry has to carry. Matching
    /// only the bare `impl Dialog for ` let exactly that spelling walk past
    /// this gate, which the mutation check found.
    fn implemented_dialog(line: &str) -> Option<String> {
        let line = line.trim_start();
        if !line.starts_with("impl ") {
            return None;
        }
        let at = line.find("Dialog for ")?;
        // `impl Debug for FilterDialog` must not match: the trait name has to
        // end at `Dialog`, not merely end with it.
        let boundary = line[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        if !boundary {
            return None;
        }
        let name = line[at + "Dialog for ".len()..]
            .trim_end_matches('{')
            .trim()
            .to_string();
        (!name.is_empty()).then_some(name)
    }

    /// Every `impl Dialog for` in the module's shipping source.
    ///
    /// Read from the files rather than from a list, because a list is the
    /// thing that gets forgotten. `tests/dialogs_style_gate.rs` walks the same
    /// directory the same way, and for the same reason.
    fn dialog_impls_in_source() -> Vec<String> {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let entries =
                std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("dialogs");
        let mut files = Vec::new();
        walk(&root, &mut files);
        assert!(
            files.len() >= 10,
            "the dialogs module lost its source files: found {}",
            files.len()
        );
        let mut found = Vec::new();
        for path in &files {
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            // Only what ships: a test may well define a `Dialog` of its own,
            // and one that is not in the registry is not a gap.
            let shipping = match text.find("#[cfg(test)]") {
                Some(at) => &text[..at],
                None => &text[..],
            };
            for line in shipping.lines() {
                if let Some(name) = implemented_dialog(line) {
                    found.push(name);
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn the_registry_scan_reads_a_dialog_impl_however_it_is_spelled() {
        // A gate whose rule is never exercised is a gate that can quietly stop
        // matching. These four lines are the ones that decide whether an
        // unregistered dialog is found.
        assert_eq!(
            implemented_dialog("impl Dialog for NewDocumentDialog {"),
            Some("NewDocumentDialog".to_string())
        );
        assert_eq!(
            implemented_dialog("    impl super::chrome::Dialog for Scratch {"),
            Some("Scratch".to_string())
        );
        assert_eq!(
            implemented_dialog("impl std::fmt::Debug for FilterDialog {"),
            None,
            "a Debug impl on a type whose name ends in Dialog is not a Dialog impl"
        );
        assert_eq!(implemented_dialog("/// impl Dialog for Doc"), None);
    }

    #[test]
    fn the_registry_covers_every_dialog_in_the_module() {
        // The contract tests above only cover what `all_dialogs()` hands them,
        // so a dialog that is written but never registered is a dialog whose
        // confirm/cancel guarantee nothing checks. The expected count is
        // therefore read out of the source: every `impl Dialog for` in
        // `src/dialogs`, with `FilterDialog`'s single impl standing in for the
        // whole generated set. A literal here — this assertion used to say
        // `9 + FILTERS.len()` — could only catch an entry being *deleted* from
        // the registry, which is the direction that already fails to compile.
        let impls = dialog_impls_in_source();
        let expected = impls.len() - 1 + filter_dialog::FILTERS.len();
        assert_eq!(
            all_dialogs().len(),
            expected,
            "`impl Dialog for` exists for {impls:?}, but all_dialogs() has {} entries \
             (expected {expected}: one per impl, with FilterDialog standing in for all \
             {} generated filter dialogs)",
            all_dialogs().len(),
            filter_dialog::FILTERS.len()
        );
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
