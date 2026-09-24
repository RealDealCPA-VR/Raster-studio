//! Layer ▸ Duplicate Layer… — the name the copy is given, and (W9-I) the
//! open document it lands in: Photopea's Destination ▸ Document.
//!
//! Photoshop's row asks before it copies (the field opens at "<name> copy"),
//! which is what its ellipsis promises; this dialog is that question and
//! nothing else. The copy itself is the application's — the layer tree and
//! the tiles live there — so the shell parks the confirmed name for the
//! `DuplicateLayer` menu arm the way it parks Trim's options, and that arm
//! makes the copy as one undoable step.
//!
//! No [`super::chrome::Dialog`] impl: that trait confirms to a
//! [`super::action::DialogAction`], and a layer name is not one of those.
//! `show` still folds Escape, Enter and the action row into one
//! [`DialogOutcome`], so the keyboard contract is every other dialog's.

use egui::Context;
use layer_model::LayerId;

use super::chrome::{action_row, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth};
use super::{ids, sizes};
use crate::strings::tr;

/// One open document a copy can land in, as the Destination combo lists it.
///
/// `key` is the application's document identity, opaque here: the `ui` crate
/// knows no document handle, and an index would re-target another document
/// if the tabs moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateDestination {
    pub key: u64,
    pub title: String,
}

/// What a confirmed Duplicate Layer dialog hands over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateLayerSpec {
    /// The copy's name, trimmed and never blank.
    pub name: String,
    /// The document the copy lands in, by [`DuplicateDestination::key`];
    /// `None` is the document the layer is in.
    pub destination: Option<u64>,
}

/// W9-I: the drag payload a Layers-panel row carries while it is dragged, so
/// a document tab can take it: dropping it on another document's tab copies
/// the layer there (Photopea's drag-to-tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerRowDrag {
    pub layer: LayerId,
}

/// Layer ▸ Duplicate Layer….
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateLayerDialog {
    source: LayerId,
    name: String,
    /// The open documents, the source's own included; empty when the host
    /// named none, and then the copy stays where it is.
    destinations: Vec<DuplicateDestination>,
    /// The source document's key.
    here: Option<u64>,
    /// The chosen entry of `destinations`.
    destination: usize,
}

impl DuplicateLayerDialog {
    /// Over `source`, whose name is `current`; the field opens at
    /// "`current` copy", Photoshop's suggestion.
    pub fn new(source: LayerId, current: &str) -> Self {
        Self {
            source,
            name: Self::suggested_name(current),
            destinations: Vec::new(),
            here: None,
            destination: 0,
        }
    }

    /// The Destination combo's documents, opening on `here`, the document
    /// the layer is in.
    pub fn with_destinations(mut self, here: u64, destinations: Vec<DuplicateDestination>) -> Self {
        self.destination = destinations.iter().position(|d| d.key == here).unwrap_or(0);
        self.here = Some(here);
        self.destinations = destinations;
        self
    }

    /// The documents the combo lists.
    pub fn destinations(&self) -> &[DuplicateDestination] {
        &self.destinations
    }

    /// Choose the destination by key; `false` (and no change) for a key the
    /// combo does not list.
    pub fn set_destination(&mut self, key: u64) -> bool {
        match self.destinations.iter().position(|d| d.key == key) {
            Some(index) => {
                self.destination = index;
                true
            }
            None => false,
        }
    }

    /// The chosen document's key when it is another document; `None` for the
    /// layer's own.
    pub fn destination(&self) -> Option<u64> {
        let key = self.destinations.get(self.destination)?.key;
        (Some(key) != self.here).then_some(key)
    }

    /// The name the dialog opens with for a layer called `current`.
    pub fn suggested_name(current: &str) -> String {
        format!("{current} copy")
    }

    /// The layer being copied.
    pub fn source(&self) -> LayerId {
        self.source
    }

    /// The name as typed so far.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    pub fn title(&self) -> &'static str {
        tr("ui.duplicate_layer.title")
    }

    fn confirm_label(&self) -> &'static str {
        "Duplicate"
    }

    /// Why the primary action is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<String> {
        self.name
            .trim()
            .is_empty()
            .then(|| tr("ui.duplicate_layer.name.empty").to_string())
    }

    /// What a confirmation hands over: the trimmed name and the destination;
    /// `None` when the name is blank.
    pub fn confirm(&self) -> Option<DuplicateLayerSpec> {
        let name = self.name.trim();
        (!name.is_empty()).then(|| DuplicateLayerSpec {
            name: name.to_string(),
            destination: self.destination(),
        })
    }

    /// Escape and Enter, without drawing — Escape wins over Enter, like
    /// [`super::chrome::resolve`].
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<DuplicateLayerSpec> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm {
            if let Some(spec) = self.confirm() {
                return DialogOutcome::Confirmed(spec);
            }
        }
        DialogOutcome::Open
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DuplicateLayerSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "duplicate-layer",
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
        design::inspector_field(ui, "Name", |ui| {
            let field = egui::TextEdit::singleline(&mut self.name)
                .id(ids::duplicate_layer_name())
                .desired_width(sizes::text_field_name());
            let response = ui.add(field);
            // The field is the dialog's whole point, so it takes focus on
            // the frame it appears, while nothing else holds it.
            if ui.memory(|m| m.focused()).is_none() {
                response.request_focus();
            }
        });
        // W9-I: Destination ▸ Document, over every open document. With only
        // the layer's own there is nothing to choose, and no row.
        if self.destinations.len() > 1 {
            let indices: Vec<usize> = (0..self.destinations.len()).collect();
            let titles: Vec<String> = self.destinations.iter().map(|d| d.title.clone()).collect();
            let mut chosen = self.destination;
            design::inspector_field(ui, "Destination", |ui| {
                super::controls::combo(
                    ui,
                    "duplicate-layer-destination",
                    &mut chosen,
                    &indices,
                    |i| titles[i].clone(),
                    |_| None,
                );
            });
            self.destination = chosen;
        }
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &[],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_opens_at_the_name_copy_and_confirms_the_trimmed_name() {
        let id = LayerId::new();
        let mut dialog = DuplicateLayerDialog::new(id, "Sky");
        assert_eq!(dialog.source(), id);
        assert_eq!(dialog.name(), "Sky copy");
        assert_eq!(
            dialog.confirm().map(|s| s.name),
            Some("Sky copy".to_string())
        );
        dialog.set_name("  Clouds  ");
        assert_eq!(
            dialog.confirm(),
            Some(DuplicateLayerSpec {
                name: "Clouds".to_string(),
                destination: None
            })
        );
        assert_eq!(dialog.blocked_reason(), None);
    }

    #[test]
    fn a_blank_name_is_blocked_with_a_reason() {
        let mut dialog = DuplicateLayerDialog::new(LayerId::new(), "Sky");
        dialog.set_name("   ");
        assert_eq!(dialog.confirm(), None);
        assert_eq!(
            dialog.blocked_reason().as_deref(),
            Some("The name cannot be empty")
        );
        assert_eq!(dialog.resolve(DialogKeys::CONFIRM), DialogOutcome::Open);
    }

    #[test]
    fn resolve_confirms_on_enter_and_cancels_on_escape() {
        let dialog = DuplicateLayerDialog::new(LayerId::new(), "Sky");
        assert_eq!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(DuplicateLayerSpec {
                name: "Sky copy".to_string(),
                destination: None
            })
        );
        assert_eq!(dialog.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
    }

    fn two_documents() -> Vec<DuplicateDestination> {
        vec![
            DuplicateDestination {
                key: 7,
                title: "Alphadoc.png".to_string(),
            },
            DuplicateDestination {
                key: 9,
                title: "Betadoc.png".to_string(),
            },
        ]
    }

    #[test]
    fn the_destination_opens_on_this_document_and_confirms_another_by_key() {
        let mut dialog =
            DuplicateLayerDialog::new(LayerId::new(), "Sky").with_destinations(7, two_documents());
        assert_eq!(dialog.destinations().len(), 2);
        assert_eq!(
            dialog.destination(),
            None,
            "it opens on the layer's own document"
        );
        assert!(!dialog.set_destination(42), "an unlisted key is refused");
        assert!(dialog.set_destination(9));
        assert_eq!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(DuplicateLayerSpec {
                name: "Sky copy".to_string(),
                destination: Some(9)
            })
        );
        assert!(dialog.set_destination(7));
        assert_eq!(dialog.confirm().and_then(|s| s.destination), None);
    }

    /// Every text a frame painted, nested shapes included.
    fn painted_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
                egui::Shape::Vec(inner) => inner.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        shapes.iter().for_each(|c| walk(&c.shape, &mut out));
        out
    }

    #[test]
    fn the_destination_combo_is_drawn_only_when_another_document_is_open() {
        let draw = |dialog: DuplicateLayerDialog| {
            let ctx = Context::default();
            design::apply_theme(&ctx, design::Theme::Dark);
            let mut dialog = Some(dialog);
            let mut shown = Vec::new();
            // A modal settles over a few frames; read the last one.
            for _ in 0..3 {
                let out = ctx.run(egui::RawInput::default(), |ctx| {
                    let _ = dialog.as_mut().unwrap().show(ctx);
                });
                shown = painted_text(&out.shapes);
            }
            dialog.take();
            shown
        };
        let mut two =
            DuplicateLayerDialog::new(LayerId::new(), "Sky").with_destinations(7, two_documents());
        assert!(two.set_destination(9));
        let text = draw(two);
        assert!(
            text.iter().any(|t| t == "Betadoc.png"),
            "the combo shows the chosen document: {text:?}"
        );
        assert!(text.iter().any(|t| t == "Destination"), "{text:?}");
        let one = DuplicateLayerDialog::new(LayerId::new(), "Sky")
            .with_destinations(7, two_documents().into_iter().take(1).collect());
        let text = draw(one);
        assert!(
            !text.iter().any(|t| t == "Destination"),
            "one document, nothing to choose: {text:?}"
        );
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = DuplicateLayerDialog::new(LayerId::new(), "Sky");
            assert!(dialog.show(ctx).is_open());
        });
    }
}
