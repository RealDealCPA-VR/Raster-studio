//! View ▸ New Guide… — one guide, by orientation and document position.
//!
//! Guides are a *document* feature ([`editor_core::Guides`], persisted and
//! undoable through [`Command::SetGuides`]), so the dialog is seeded with the
//! set the document holds and confirms to that set plus one guide, as a
//! [`DialogAction::Command`]. One undo step removes the guide again.

use editor_core::{Command, Guide, GuideAxis, Guides};
use egui::Context;

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, modal, resolve, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::controls::{combo, numeric};
use super::ids;
use crate::strings::tr;

/// The orientation choice, as the combo lists it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

impl Orientation {
    pub const ALL: [Orientation; 2] = [Orientation::Horizontal, Orientation::Vertical];

    pub const fn label(self) -> &'static str {
        match self {
            Orientation::Horizontal => "Horizontal",
            Orientation::Vertical => "Vertical",
        }
    }

    pub const fn axis(self) -> GuideAxis {
        match self {
            Orientation::Horizontal => GuideAxis::Horizontal,
            Orientation::Vertical => GuideAxis::Vertical,
        }
    }
}

/// View ▸ New Guide….
#[derive(Debug, Clone, PartialEq)]
pub struct NewGuideDialog {
    current: Guides,
    canvas: (u32, u32),
    orientation: Orientation,
    position: f64,
}

impl NewGuideDialog {
    /// Over the document's current guide set and its canvas size (which
    /// bounds the position field).
    pub fn new(current: Guides, canvas: (u32, u32)) -> Self {
        Self {
            current,
            canvas,
            orientation: Orientation::Horizontal,
            position: 0.0,
        }
    }

    pub fn orientation(&self) -> Orientation {
        self.orientation
    }

    pub fn set_orientation(&mut self, orientation: Orientation) {
        self.orientation = orientation;
    }

    /// The document coordinate the guide will sit at, in pixels.
    pub fn position(&self) -> f64 {
        self.position
    }

    pub fn set_position(&mut self, position: f64) {
        self.position = position;
    }

    /// The guide a confirmation adds.
    pub fn guide(&self) -> Guide {
        Guide {
            axis: self.orientation.axis(),
            doc: self.position as f32,
            locked: false,
        }
    }

    /// The whole set a confirmation writes: the current guides plus the new
    /// one.
    pub fn guides_after(&self) -> Guides {
        let mut next = self.current.clone();
        next.list.push(self.guide());
        next
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DialogAction> {
        let keys = DialogKeys::read(ctx);
        let mut outcome = resolve(self, keys);
        let drawn = modal(
            ctx,
            "new-guide",
            self.title(),
            None,
            DialogWidth::Narrow,
            |ui| self.body(ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => DialogOutcome::Open,
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        caption(ui, tr("ui.new_guide.subtitle"));
        design::inspector_field(ui, "Orientation", |ui| {
            combo(
                ui,
                ids::new_guide_orientation(),
                &mut self.orientation,
                &Orientation::ALL,
                |o| o.label().to_string(),
                |_| None,
            );
        });
        let extent = f64::from(match self.orientation {
            Orientation::Horizontal => self.canvas.1,
            Orientation::Vertical => self.canvas.0,
        });
        design::inspector_field(ui, "Position", |ui| {
            numeric(
                ui,
                &mut self.position,
                0.0..=extent.max(1.0),
                0,
                tr("ui.refine_mask.px.suffix"),
            );
        });
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &[],
        )
    }
}

impl Dialog for NewGuideDialog {
    fn title(&self) -> &'static str {
        tr("ui.new_guide.title")
    }

    fn confirm_label(&self) -> &'static str {
        "Place"
    }

    fn confirm(&self) -> Option<DialogAction> {
        self.position.is_finite().then(|| {
            DialogAction::Command(Box::new(Command::SetGuides {
                guides: self.guides_after(),
            }))
        })
    }

    fn blocked_reason(&self) -> Option<String> {
        (!self.position.is_finite()).then(|| tr("ui.new_guide.position.must.be.finite").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> NewGuideDialog {
        NewGuideDialog::new(
            Guides {
                list: vec![Guide {
                    axis: GuideAxis::Vertical,
                    doc: 5.0,
                    locked: true,
                }],
                visible: true,
                locked: false,
            },
            (64, 32),
        )
    }

    #[test]
    fn confirming_appends_one_guide_to_the_set_it_was_seeded_with() {
        let mut dialog = seeded();
        dialog.set_orientation(Orientation::Vertical);
        dialog.set_position(20.0);
        match dialog.confirm() {
            Some(DialogAction::Command(command)) => match *command {
                Command::SetGuides { guides } => {
                    assert_eq!(guides.list.len(), 2, "the existing guide is kept");
                    assert_eq!(guides.list[1].axis, GuideAxis::Vertical);
                    assert_eq!(guides.list[1].doc, 20.0);
                    assert!(!guides.list[1].locked);
                    assert!(guides.list[0].locked, "the seeded guide is untouched");
                }
                other => panic!("confirmed to {other:?}"),
            },
            other => panic!("confirmed to {other:?}"),
        }
    }

    #[test]
    fn a_non_finite_position_is_blocked_with_a_reason() {
        let mut dialog = seeded();
        dialog.set_position(f64::NAN);
        assert_eq!(dialog.confirm(), None);
        assert_eq!(
            dialog.blocked_reason().as_deref(),
            Some("The position must be a finite number")
        );
    }

    #[test]
    fn resolve_confirms_on_enter_and_cancels_on_escape() {
        let dialog = seeded();
        assert!(matches!(
            resolve(&dialog, DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(DialogAction::Command(_))
        ));
        assert_eq!(
            resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = seeded();
            assert!(dialog.show(ctx).is_open());
        });
    }
}
