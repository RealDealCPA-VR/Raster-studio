//! W10-A: File ▸ Export ▸ Slice Options… — Photoshop's Slice Options dialog
//! over the slice the Slice Select tool picked.
//!
//! Three fields: the slice's name (the file File ▸ Export ▸ Slices writes it
//! as), and the link (URL) and alternate text (Alt Tag) the HTML page that
//! export writes beside the images gives it. A blank name, and a name whose
//! exported file ([`slice_file_stem`], compared ignoring case) is another
//! slice's, are refused with a reason: the name is what the exported file is
//! called and what an edited set is matched back by, and two slices that
//! export to one file would overwrite each other.
//!
//! Like Save Selection, the dialog does not implement
//! [`super::chrome::Dialog`]: its confirmation is a [`SliceOptionsSpec`], not
//! a [`super::action::DialogAction`]. The shell parks it for the
//! `SliceOptions` menu arm, which writes it into the document's slice store.

use egui::Context;

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::sizes;
use crate::strings::tr;

/// A confirmed Slice Options: slice `index` of the active document's set
/// takes these options.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SliceOptionsSpec {
    pub index: usize,
    pub name: String,
    pub url: String,
    pub alt: String,
}

/// The file name (before the exporter makes it safe) that slice `number`
/// (1-based) of a document whose file stem is `doc_stem` is exported under.
/// The Slice tool's own names (`slice_NN`) keep the `<document>_NN` pattern,
/// numbered by the slice's name, so a slice keeps its file name after another
/// is deleted; a blank name is numbered by position. A name the user gave has
/// the characters a file name cannot hold replaced by `_`; the exporter then
/// sanitises it further ([`slice_file_stem`]).
pub fn slice_file_name(doc_stem: &str, name: &str, number: usize) -> String {
    let name = name.trim();
    let own = name
        .strip_prefix("slice_")
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    match own {
        Some(digits) => format!("{doc_stem}_{digits}"),
        None if name.is_empty() => format!("{doc_stem}_{number:02}"),
        None => name
            .chars()
            .map(|c| {
                if c.is_control() || "/\\:*?\"<>|".contains(c) {
                    '_'
                } else {
                    c
                }
            })
            .collect(),
    }
}

/// The file stem File > Export > Slices really writes slice `number` as:
/// [`slice_file_name`] through [`raster::export::sanitize_file_stem`] (only
/// ASCII letters and digits and `- _ ( )` and space kept, everything else
/// `_`, cut at 64 characters). Two slices whose stems are equal ignoring
/// case would land in one file (NTFS and APFS fold case).
pub fn slice_file_stem(doc_stem: &str, name: &str, number: usize) -> String {
    raster::export::sanitize_file_stem(&slice_file_name(doc_stem, name, number))
}

/// The field ids, so a test (and the focus rule) can find them.
pub fn name_field_id() -> egui::Id {
    egui::Id::new("dialog.slice_options.name")
}

/// See [`name_field_id`].
pub fn url_field_id() -> egui::Id {
    egui::Id::new("dialog.slice_options.url")
}

/// See [`name_field_id`].
pub fn alt_field_id() -> egui::Id {
    egui::Id::new("dialog.slice_options.alt")
}

/// File ▸ Export ▸ Slice Options….
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceOptionsDialog {
    index: usize,
    name: String,
    url: String,
    alt: String,
    /// The document's file stem, which the Slice tool's names export under.
    doc_stem: String,
    /// The file stems ([`slice_file_stem`]) the other slices export as,
    /// which this one's may not equal (ignoring case).
    others: Vec<String>,
}

impl SliceOptionsDialog {
    /// Over slice `index` of a document whose file stem is `doc_stem`; the
    /// slice's options are `name`, `url` and `alt`; `others` are the file
    /// stems ([`slice_file_stem`]) the rest of the set exports as.
    pub fn new(
        index: usize,
        name: impl Into<String>,
        url: impl Into<String>,
        alt: impl Into<String>,
        doc_stem: impl Into<String>,
        others: Vec<String>,
    ) -> Self {
        Self {
            index,
            name: name.into(),
            url: url.into(),
            alt: alt.into(),
            doc_stem: doc_stem.into(),
            others,
        }
    }

    /// Which slice of the set the dialog edits (0-based).
    pub fn index(&self) -> usize {
        self.index
    }

    /// Replace the Name field, as typing into it does.
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// The fields as typed so far: name, URL, alt text.
    pub fn fields(&self) -> (&str, &str, &str) {
        (&self.name, &self.url, &self.alt)
    }

    pub fn title(&self) -> &'static str {
        tr("ui.slice_options.title")
    }

    /// Why OK is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<String> {
        let name = self.name.trim();
        // The file this name exports as, against the other slices' files.
        let stem = slice_file_stem(&self.doc_stem, name, self.index + 1);
        if name.is_empty() {
            Some(tr("ui.slice_options.name.empty").to_string())
        } else if self.others.iter().any(|o| o.eq_ignore_ascii_case(&stem)) {
            Some(tr("ui.slice_options.name.taken").to_string())
        } else {
            None
        }
    }

    /// The spec a confirmation hands over: the trimmed fields.
    pub fn confirm(&self) -> Option<SliceOptionsSpec> {
        self.blocked_reason().is_none().then(|| SliceOptionsSpec {
            index: self.index,
            name: self.name.trim().to_string(),
            url: self.url.trim().to_string(),
            alt: self.alt.trim().to_string(),
        })
    }

    /// Escape and Enter, without drawing.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<SliceOptionsSpec> {
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
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<SliceOptionsSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "slice-options",
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
        design::inspector_field(ui, tr("ui.slice_options.name"), |ui| {
            let field = egui::TextEdit::singleline(&mut self.name)
                .id(name_field_id())
                .desired_width(sizes::text_field_name());
            let response = ui.add(field);
            // The name is what the dialog is mostly for, so it takes focus on
            // the frame it appears, while nothing else holds it.
            if ui.memory(|m| m.focused()).is_none() {
                response.request_focus();
            }
        });
        design::inspector_field(ui, tr("ui.slice_options.url"), |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.url)
                    .id(url_field_id())
                    .desired_width(sizes::text_field_wide()),
            );
        });
        design::inspector_field(ui, tr("ui.slice_options.alt"), |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.alt)
                    .id(alt_field_id())
                    .desired_width(sizes::text_field_wide()),
            );
        });
        caption(ui, tr("ui.slice_options.caption"));
        action_row(
            ui,
            tr("ui.slice_options.confirm"),
            self.blocked_reason().as_deref(),
            &[],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> SliceOptionsDialog {
        SliceOptionsDialog::new(1, "slice_02", "", "", "poster", vec!["poster_01".into()])
    }

    #[test]
    fn confirming_hands_over_the_trimmed_fields_for_the_slice() {
        let mut d = dialog();
        d.name = "  hero ".into();
        d.url = " https://example.com ".into();
        d.alt = "The hero".into();
        assert_eq!(
            d.confirm(),
            Some(SliceOptionsSpec {
                index: 1,
                name: "hero".into(),
                url: "https://example.com".into(),
                alt: "The hero".into(),
            })
        );
    }

    #[test]
    fn a_blank_name_or_another_slices_name_is_blocked_with_a_reason() {
        let mut d = dialog();
        d.name = "   ".into();
        assert_eq!(d.confirm(), None);
        assert_eq!(d.blocked_reason().as_deref(), Some("A slice needs a name"));
        d.name = "slice_01".into();
        assert_eq!(d.confirm(), None);
        assert_eq!(
            d.blocked_reason().as_deref(),
            Some("Another slice already has this name")
        );
        // Its own name is not "taken".
        d.name = "slice_02".into();
        assert!(d.confirm().is_some());
    }

    /// Wave-10 review: a name is refused when the file it exports as is
    /// another slice's, not only when the names are equal: the exporter
    /// turns `?` and `!` into `_`, keeps the Slice tool's `<document>_NN`,
    /// cuts at 64 characters, and the disk folds case.
    #[test]
    fn a_name_that_exports_to_another_slices_file_is_blocked() {
        let others = vec![
            slice_file_stem("poster", "hero?", 1),
            slice_file_stem("poster", "slice_03", 3),
            slice_file_stem("poster", &"x".repeat(70), 4),
        ];
        assert_eq!(others, ["hero_", "poster_03", &"x".repeat(64)]);
        let taken = Some("Another slice already has this name");
        for name in ["hero!", "HERO_", "poster_03", "Poster_03", &"x".repeat(65)] {
            let mut d = SliceOptionsDialog::new(1, name, "", "", "poster", others.clone());
            assert_eq!(d.blocked_reason().as_deref(), taken, "{name}");
            assert_eq!(d.confirm(), None, "{name}");
            d.name = "hero".into();
            assert!(d.confirm().is_some(), "hero exports as hero, which is free");
        }
    }

    #[test]
    fn resolve_confirms_on_enter_and_cancels_on_escape() {
        let d = dialog();
        assert!(matches!(
            d.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(_)
        ));
        assert_eq!(d.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut d = dialog();
            assert!(d.show(ctx).is_open());
        });
    }
}
