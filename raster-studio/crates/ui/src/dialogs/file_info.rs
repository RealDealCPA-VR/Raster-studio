//! W10-E: File ▸ File Info… — the document's XMP description.
//!
//! Photopea's File Info edits the Dublin Core fields every Adobe application
//! reads: Document Title, Author, Description, Keywords and Copyright
//! Notice. The dialog edits exactly those five
//! ([`raster::metadata::XmpFields`]) over the document's current values and
//! hands the edited set back; the application keeps it with the document and
//! Export As writes it into PNG, JPEG and TIFF files as an XMP packet
//! (`raster::metadata`). The read-only facts the old File Info window showed
//! (size, colour space, source) are listed under the fields.

use egui::Context;
use raster::metadata::XmpFields;

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::sizes;
use crate::strings::tr;

/// File ▸ File Info….
#[derive(Clone, Debug)]
pub struct FileInfoDialog {
    original: XmpFields,
    fields: XmpFields,
    keywords: String,
    /// Read-only `(label, value)` rows about the document.
    facts: Vec<(String, String)>,
}

impl FileInfoDialog {
    /// Over the document's current `fields`, with its read-only `facts`.
    pub fn new(fields: XmpFields, facts: Vec<(String, String)>) -> Self {
        Self {
            keywords: fields.keyword_line(),
            original: fields.clone(),
            fields,
            facts,
        }
    }

    /// The fields as edited so far (keywords parsed from their line).
    pub fn fields(&self) -> XmpFields {
        XmpFields {
            keywords: XmpFields::parse_keywords(&self.keywords),
            ..self.fields.clone()
        }
    }

    /// Set every field — tests.
    pub fn set_fields(&mut self, fields: XmpFields) {
        self.keywords = fields.keyword_line();
        self.fields = fields;
    }

    /// Why OK is unavailable: nothing was changed.
    pub fn blocked_reason(&self) -> Option<&'static str> {
        (self.fields() == self.original).then(|| tr("ui.file_info.unchanged"))
    }

    pub fn confirm(&self) -> Option<XmpFields> {
        self.blocked_reason().is_none().then(|| self.fields())
    }

    /// Escape and Enter, without drawing — Escape wins over Enter.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<XmpFields> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm {
            if let Some(fields) = self.confirm() {
                return DialogOutcome::Confirmed(fields);
            }
        }
        DialogOutcome::Open
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<XmpFields> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "w10e-file-info",
            tr("ui.file_info.title"),
            None,
            DialogWidth::Standard,
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

    fn text_row(ui: &mut egui::Ui, key: &str, id: &str, value: &mut String, multiline: bool) {
        design::inspector_field(ui, tr(key), |ui| {
            let edit = if multiline {
                egui::TextEdit::multiline(value)
            } else {
                egui::TextEdit::singleline(value)
            };
            ui.add(
                edit.id(egui::Id::new(id))
                    .desired_width(sizes::text_field_wide()),
            );
        });
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        caption(ui, tr("ui.file_info.subtitle"));
        design::section_header(ui, tr("ui.file_info.description.section"));
        Self::text_row(
            ui,
            "ui.file_info.doc.title",
            "w10e-fi-title",
            &mut self.fields.title,
            false,
        );
        Self::text_row(
            ui,
            "ui.file_info.author",
            "w10e-fi-author",
            &mut self.fields.author,
            false,
        );
        Self::text_row(
            ui,
            "ui.file_info.description",
            "w10e-fi-description",
            &mut self.fields.description,
            true,
        );
        Self::text_row(
            ui,
            "ui.file_info.keywords",
            "w10e-fi-keywords",
            &mut self.keywords,
            false,
        );
        caption(ui, tr("ui.file_info.keywords.note"));
        Self::text_row(
            ui,
            "ui.file_info.copyright",
            "w10e-fi-copyright",
            &mut self.fields.copyright,
            false,
        );
        if !self.facts.is_empty() {
            design::section_header(ui, tr("ui.file_info.document"));
            for (label, value) in &self.facts {
                design::inspector_field(ui, label, |ui| {
                    ui.label(value.clone());
                });
            }
        }
        action_row(ui, tr("ui.file_info.ok"), self.blocked_reason(), &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_confirm_to_the_five_fields_with_keywords_split() {
        let mut dialog = FileInfoDialog::new(XmpFields::default(), Vec::new());
        assert!(dialog.blocked_reason().is_some(), "nothing changed yet");
        dialog.set_fields(XmpFields {
            title: "Harbour".into(),
            ..XmpFields::default()
        });
        dialog.keywords = "sea; boats ,  dusk".into();
        let fields = match dialog.resolve(DialogKeys::CONFIRM) {
            DialogOutcome::Confirmed(fields) => fields,
            other => panic!("Enter did not confirm: {other:?}"),
        };
        assert_eq!(fields.title, "Harbour");
        assert_eq!(fields.keywords, vec!["sea", "boats", "dusk"]);
        assert_eq!(
            dialog.resolve(DialogKeys {
                confirm: true,
                cancel: true
            }),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = FileInfoDialog::new(
                XmpFields::default(),
                vec![("Size".into(), "8 x 8 px".into())],
            );
            assert!(dialog.show(ctx).is_open());
        });
    }
}
