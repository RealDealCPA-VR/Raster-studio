//! The one way a dialog changes a colour.
//!
//! Five dialogs hold colours a user has to be able to pick: every layer effect,
//! every gradient colour stop, the custom background in New Document and Canvas
//! Size, and any filter parameter of kind [`tools::OptionKind::Color`]. None of
//! them should grow its own picker, and none of them can hand the job to the
//! shell, because the shell would have to know how to write the value back into
//! a `ShadowEffect` or a `GradientStop`.
//!
//! So the picker is *nested*: the host dialog owns a [`ColorEdit`], a swatch
//! click opens it against a target the host names, and one frame later the
//! chosen colour comes back paired with that target. The host writes it. The
//! rule that a dialog never mutates the document still holds — a layer effect
//! colour is dialog state until [`Dialog::confirm`](super::Dialog::confirm)
//! turns the whole style into one command.
//!
//! # Why the target is generic
//!
//! Each host has a different notion of "which colour": an [`EffectKind`], a
//! [`StopRef`], a parameter key, or nothing at all. Making `ColorEdit` generic
//! over that keeps the pairing type-safe — a host cannot receive a colour and
//! apply it to the wrong field, because the target it gets back is the one it
//! put in.
//!
//! [`EffectKind`]: super::layer_style::EffectKind
//! [`StopRef`]: super::gradient_editor::StopRef

use egui::Context;

use super::action::DialogAction;
use super::chrome::DialogOutcome;
use super::color_picker::{ColorPickerDialog, ColorValue, RecentColors, ScreenSampler};

/// A colour picker opened over a host dialog, against one named target.
#[derive(Clone, Debug)]
pub struct ColorEdit<T> {
    open: Option<(T, ColorPickerDialog)>,
    /// Colours chosen through this seam before.
    ///
    /// A picker is built fresh on every swatch click, so without somewhere for
    /// the list to live between opens the "Recent" strip would be empty every
    /// single time — a feature drawn but never populated. The host owns the
    /// list; each picker is seeded from it and hands it back on close.
    recents: RecentColors,
}

impl<T> Default for ColorEdit<T> {
    fn default() -> Self {
        Self {
            open: None,
            recents: RecentColors::default(),
        }
    }
}

impl<T: Copy> ColorEdit<T> {
    /// Closed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the picker on `current`, to be written back to `target`.
    ///
    /// Opening while already open replaces the target, which is what a click on
    /// a second swatch means.
    pub fn open(&mut self, target: T, current: [f32; 4]) {
        let mut picker = ColorPickerDialog::new(ColorValue::new(current));
        picker.set_recents(self.recents.clone());
        self.open = Some((target, picker));
    }

    /// Colours chosen through this seam, most recent first.
    pub fn recents(&self) -> &[ColorValue] {
        self.recents.as_slice()
    }

    /// Close, keeping whatever the picker recorded while it was up.
    fn close_recording(&mut self) {
        if let Some((_, picker)) = self.open.take() {
            self.recents = picker.recent_colors().clone();
        }
    }

    /// Whether the picker is up.
    ///
    /// Hosts consult this before reading the keyboard: while a nested picker is
    /// open, Escape belongs to the picker and Enter must not commit the dialog
    /// underneath it.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The target being edited, if any.
    pub fn target(&self) -> Option<T> {
        self.open.as_ref().map(|(target, _)| *target)
    }

    /// The nested picker, for a test — or a host — that wants to drive it
    /// without synthesising pointer input.
    pub fn picker_mut(&mut self) -> Option<&mut ColorPickerDialog> {
        self.open.as_mut().map(|(_, picker)| picker)
    }

    /// The colour the picker currently shows.
    pub fn color(&self) -> Option<[f32; 4]> {
        self.open.as_ref().map(|(_, picker)| picker.color().rgba)
    }

    /// Close without choosing.
    pub fn cancel(&mut self) {
        self.open = None;
    }

    /// Close, taking the colour as chosen.
    ///
    /// The keyboard-free half of [`ColorEdit::show`]; the two share this so a
    /// test and a frame commit through the same code.
    pub fn commit(&mut self) -> Option<(T, [f32; 4])> {
        let (target, picker) = self.open.take()?;
        let color = picker.color();
        self.recents = picker.recent_colors().clone();
        // A repeat moves to the front rather than appearing twice, so this is a
        // no-op when the picker's own confirm path already recorded it.
        self.recents.push(color);
        Some((target, color.rgba))
    }

    /// Draw the nested picker for one frame.
    ///
    /// `Some` exactly when the user confirmed, carrying the target the host
    /// named and the colour to write. Cancelling closes the picker and yields
    /// nothing, which is the same contract every dialog in this module has.
    pub fn show(
        &mut self,
        ctx: &Context,
        id_salt: &'static str,
        sampler: Option<&dyn ScreenSampler>,
    ) -> Option<(T, [f32; 4])> {
        let target = self.target()?;
        let outcome = {
            let (_, picker) = self.open.as_mut()?;
            picker.show_nested(ctx, id_salt, sampler)
        };
        match outcome {
            DialogOutcome::Confirmed(DialogAction::SetColor(color)) => {
                self.close_recording();
                Some((target, color.rgba))
            }
            DialogOutcome::Confirmed(_) => {
                // The colour picker's only action is `SetColor`; anything else
                // would be a change to that dialog, not a case to guess at.
                self.close_recording();
                None
            }
            DialogOutcome::Cancelled => {
                // Backing out records nothing: a colour the user rejected is
                // not a colour they used.
                self.open = None;
                None
            }
            DialogOutcome::Open => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compare colours as the eight-bit values a document stores: the picker
    /// holds HSB, so a float round-trip is exact only to a code.
    fn bytes(rgba: [f32; 4]) -> [u8; 4] {
        ColorValue::new(rgba).to_bytes()
    }

    #[test]
    fn a_fresh_editor_is_closed_and_has_no_target() {
        let edit = ColorEdit::<u8>::new();
        assert!(!edit.is_open());
        assert_eq!(edit.target(), None);
        assert_eq!(edit.color(), None);
    }

    #[test]
    fn opening_carries_the_target_and_the_starting_colour() {
        let mut edit = ColorEdit::new();
        edit.open(7u8, [1.0, 0.0, 0.0, 1.0]);
        assert!(edit.is_open());
        assert_eq!(edit.target(), Some(7));
        assert_eq!(edit.color().map(bytes), Some([255, 0, 0, 255]));
    }

    #[test]
    fn committing_returns_the_edited_colour_against_its_own_target() {
        let mut edit = ColorEdit::new();
        edit.open("shadow", [0.0, 0.0, 0.0, 1.0]);
        edit.picker_mut()
            .expect("open")
            .set_color(ColorValue::new([0.0, 1.0, 0.0, 0.5]));
        let (target, rgba) = edit.commit().expect("a colour");
        assert_eq!(target, "shadow");
        assert_eq!(bytes(rgba), [0, 255, 0, 128]);
        assert!(!edit.is_open(), "committing left the picker up");
    }

    #[test]
    fn cancelling_yields_nothing_and_closes() {
        let mut edit = ColorEdit::new();
        edit.open(1u8, [1.0, 1.0, 1.0, 1.0]);
        edit.cancel();
        assert!(!edit.is_open());
        assert_eq!(edit.commit(), None);
    }

    #[test]
    fn opening_a_second_swatch_moves_the_target() {
        let mut edit = ColorEdit::new();
        edit.open(1u8, [1.0, 0.0, 0.0, 1.0]);
        edit.open(2u8, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(edit.target(), Some(2));
        assert_eq!(edit.color().map(bytes), Some([0, 0, 255, 255]));
    }

    #[test]
    fn the_recent_list_survives_the_picker_it_was_chosen_in() {
        // The defect this pins: `ColorPickerDialog::set_recents` had no caller
        // anywhere in the crate, and a picker is rebuilt on every swatch click,
        // so the "Recent" strip the dialog draws could never contain anything
        // in normal use.
        let mut edit = ColorEdit::new();
        assert!(edit.recents().is_empty());

        edit.open("shadow", [0.0, 0.0, 0.0, 1.0]);
        edit.picker_mut()
            .expect("open")
            .set_color(ColorValue::new([1.0, 0.0, 0.0, 1.0]));
        edit.commit().expect("a colour");
        assert_eq!(edit.recents().len(), 1);
        assert_eq!(bytes(edit.recents()[0].rgba), [255, 0, 0, 255]);

        // The next picker this host opens starts with it already there.
        edit.open("glow", [0.0, 0.0, 1.0, 1.0]);
        let seeded = edit.picker_mut().expect("open").recents().to_vec();
        assert_eq!(seeded.len(), 1);
        assert_eq!(bytes(seeded[0].rgba), [255, 0, 0, 255]);
    }

    #[test]
    fn a_cancelled_pick_is_not_a_recent_colour() {
        let mut edit = ColorEdit::new();
        edit.open(0u8, [0.0, 0.0, 0.0, 1.0]);
        edit.picker_mut()
            .expect("open")
            .set_color(ColorValue::new([0.0, 1.0, 0.0, 1.0]));
        edit.cancel();
        assert!(edit.recents().is_empty());
    }

    #[test]
    fn an_out_of_range_starting_colour_is_clamped_not_carried() {
        let mut edit = ColorEdit::new();
        edit.open((), [4.0, -1.0, 0.5, 9.0]);
        assert_eq!(edit.color().map(bytes), Some([255, 0, 128, 255]));
    }
}
