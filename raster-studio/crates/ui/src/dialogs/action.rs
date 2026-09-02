//! What a confirmed dialog hands back.
//!
//! One union type, so the shell drains dialogs the same way it drains panels
//! and so the confirm/cancel contract can be asserted over every dialog in one
//! loop. Cancelling produces `DialogOutcome::Cancelled`, which carries no
//! `DialogAction` at all — that is the contract, expressed in the type.
//!
//! Document edits ride as [`DialogAction::Command`] and go through
//! [`editor_core::History`] like every other edit. The rest are application
//! intents that are not edits to a document: creating one, exporting one,
//! changing a setting. Keeping them in the same enum is what stops a dialog
//! from reaching around the command system to "just do it".

use editor_core::Command;
use layer_model::Gradient;
use tools::BrushSettings;

use super::canvas_size::CanvasSizeSpec;
use super::color_picker::ColorValue;
use super::export_as::ExportJob;
use super::filter_dialog::FilterInvocation;
use super::image_size::ImageSizeSpec;
use super::new_document::NewDocumentSpec;
use super::preferences::UiPreferences;

/// The outcome of confirming a dialog.
///
/// Every variant is boxed whose payload is larger than a pointer or two: a
/// `Vec` of export presets and a whole preferences tree must not set the size
/// of the variant a colour swatch uses.
#[derive(Clone, PartialEq, Debug)]
pub enum DialogAction {
    /// A document edit, applied through [`editor_core::History`].
    Command(Box<Command>),
    /// Create a document.
    NewDocument(Box<NewDocumentSpec>),
    /// Resample the document.
    ResizeImage(ImageSizeSpec),
    /// Re-frame the document without resampling.
    ResizeCanvas(CanvasSizeSpec),
    /// Rotate the canvas and every layer by this many degrees, positive
    /// clockwise. Right angles take the exact path the fixed rotations use;
    /// anything else resamples.
    RotateCanvas(f64),
    /// Write one or more files.
    Export(Box<ExportJob>),
    /// Set the active colour.
    SetColor(ColorValue),
    /// Save a named brush preset and make it the active brush.
    ///
    /// The name rides along because the Brush Editor requires one before it
    /// will let the user save — a field that gates the action and is then
    /// thrown away is a control that demands input it discards.
    SetBrush {
        name: String,
        settings: Box<BrushSettings>,
    },
    /// Replace the active gradient.
    SetGradient(Box<Gradient>),
    /// Replace the application preferences.
    SetPreferences(Box<UiPreferences>),
    /// Run a filter with the given parameters.
    RunFilter(Box<FilterInvocation>),
    /// Fill the active selection.
    Fill(Box<super::fill_stroke::FillSpec>),
    /// Stroke the active selection's border.
    Stroke(Box<super::fill_stroke::StrokeSpec>),
}

impl DialogAction {
    /// Whether this action is one the application can actually carry out.
    ///
    /// The dialogs already refuse to produce an invalid action, so this is the
    /// assertion that keeps that true rather than a runtime guard: it is what
    /// `every_dialog_confirms_to_a_valid_action_and_cancels_to_nothing`
    /// checks.
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Command(_) => true,
            Self::NewDocument(spec) => spec.is_valid(),
            Self::ResizeImage(spec) => spec.is_valid(),
            Self::ResizeCanvas(spec) => spec.is_valid(),
            Self::RotateCanvas(degrees) => degrees.is_finite(),
            Self::Export(job) => job.is_valid(),
            Self::SetColor(color) => color.rgba.iter().all(|c| (0.0..=1.0).contains(c)),
            Self::SetBrush { name, settings } => {
                !name.trim().is_empty() && settings.validated().is_ok()
            }
            Self::SetGradient(gradient) => {
                gradient.stops.len() >= super::gradient_editor::MIN_STOPS
            }
            Self::SetPreferences(prefs) => prefs.is_sane() && prefs.keymap.conflicts().is_empty(),
            Self::RunFilter(invocation) => invocation.is_valid(),
            Self::Fill(spec) => spec.is_valid(),
            Self::Stroke(spec) => spec.is_valid(),
        }
    }

    /// A short description, for the status bar and the history label.
    pub fn label(&self) -> String {
        match self {
            Self::Command(_) => "Edit".to_string(),
            Self::NewDocument(spec) => format!("New {} x {}", spec.width, spec.height),
            Self::ResizeImage(spec) => format!("Image Size {} x {}", spec.width, spec.height),
            Self::ResizeCanvas(spec) => format!("Canvas Size {} x {}", spec.width, spec.height),
            Self::RotateCanvas(degrees) => format!("Rotate Canvas {degrees}°"),
            Self::Export(job) => format!("Export {} file(s)", job.entries.len()),
            Self::SetColor(color) => format!("Color #{}", color.to_hex(false)),
            Self::SetBrush { name, .. } => format!("Brush \"{name}\""),
            Self::SetGradient(_) => "Gradient".to_string(),
            Self::SetPreferences(_) => "Preferences".to_string(),
            Self::Fill(_) => "Fill".to_string(),
            Self::Stroke(_) => "Stroke".to_string(),
            Self::RunFilter(invocation) => invocation.filter.name().to_string(),
        }
    }

    /// The document edit this action carries, if it is one.
    pub fn as_command(&self) -> Option<&Command> {
        match self {
            Self::Command(command) => Some(command),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_has_a_label() {
        for action in super::super::tests_support::one_of_each_action() {
            assert!(!action.label().is_empty(), "{action:?} has no label");
        }
    }

    #[test]
    fn only_a_command_action_carries_a_command() {
        for action in super::super::tests_support::one_of_each_action() {
            let is_command = matches!(action, DialogAction::Command(_));
            assert_eq!(action.as_command().is_some(), is_command, "{action:?}");
        }
    }

    #[test]
    fn a_colour_outside_the_unit_range_is_not_a_valid_action() {
        // `ColorValue::new` clamps, so this has to be built by hand — which is
        // exactly what a future caller deserialising one would do.
        let action = DialogAction::SetColor(ColorValue {
            rgba: [2.0, 0.0, 0.0, 1.0],
        });
        assert!(!action.is_valid());
    }

    #[test]
    fn a_zero_sized_new_document_is_not_a_valid_action() {
        let mut dialog = super::super::NewDocumentDialog::default();
        dialog.set_pixel_width(1_000_000.0);
        let action = DialogAction::NewDocument(Box::new(dialog.spec()));
        assert!(!action.is_valid());
    }
}
