//! W10-B: the Notes panel.
//!
//! A note is text pinned at a document position — a reviewer's "check this
//! edge" — kept in the document ([`layer_model::DocumentExtras::notes`]) so it
//! is saved and reopened with the file. A note is annotation, not content:
//! it is not a layer, so no compositor and no exporter ever reads it.
//!
//! The panel lists the notes oldest first. Each row carries the note's text
//! in a field (Enter or leaving the field commits the edit as one undo
//! step), a target button that centres the view on the note's pin, and a
//! delete button. The footer's New Note pins a note at the centre of the
//! view; the Note tool (`tools::note::NoteTool`) pins one where a click
//! lands on the canvas, and the application's canvas overlay draws a
//! numbered pin at every note (numbered in this list's order).
//!
//! # Strings
//!
//! Through the strings catalogue ([`crate::strings::tr`]): each constant
//! below is a catalogue key.

use design::{current_tokens, Space, TextRole};
use editor_core::extras;
use editor_core::{Command, Document};
use egui::{Align, Layout, Ui, Vec2};

use crate::intent::Intent;
use crate::strings::tr;
use crate::view::{
    empty_state, hairline, hint, icon_action_id, text, text_field_sized, ActionState,
};
use crate::Workspace;

const NO_DOCUMENT: &str = "ui.notes.no_document";
const NO_NOTES: &str = "ui.notes.none";
const NEW: &str = "ui.notes.new";
const SHOW: &str = "ui.notes.show";
const DELETE: &str = "ui.notes.delete";
const AUTHOR: &str = "ui.w16.notes.author";
/// The text a fresh note starts with.
pub const NEW_NOTE_TEXT: &str = "Note";

/// Stable ids for a headless test.
pub mod ids {
    pub fn new() -> egui::Id {
        egui::Id::new("raster-notes-new")
    }
    /// The text field of the note with document id `note`.
    pub fn text(note: u64) -> egui::Id {
        egui::Id::new(("raster-notes-text", note))
    }
    /// W16-E: the author field of the note with document id `note`.
    pub fn author(note: u64) -> egui::Id {
        crate::panels::panel_menus_w16::ids::note_author(note)
    }
    pub fn show(note: u64) -> egui::Id {
        egui::Id::new(("raster-notes-show", note))
    }
    pub fn delete(note: u64) -> egui::Id {
        egui::Id::new(("raster-notes-delete", note))
    }
}

/// Draw the panel.
pub(crate) fn notes_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    if doc.width() == 0 || doc.height() == 0 {
        empty_state(ui, tr(NO_DOCUMENT));
        return;
    }
    let t = current_tokens(ui);
    let notes = &doc.extras.notes;
    if notes.is_empty() {
        ui.label(hint(ui, tr(NO_NOTES)));
    }
    for (index, note) in notes.iter().enumerate() {
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), t.metrics.list_row_height),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.add_space(Space::XSmall.pt());
                ui.label(text(
                    ui,
                    format!("{}", index + 1),
                    TextRole::Secondary,
                    design::TypeRole::Caption,
                ));
                let buttons = (t.metrics.min_hit_target + Space::XSmall.pt()) * 2.0;
                // W16-E: Photopea's Author field, beside the note's text and
                // committed like it (Enter or leaving the field; one undo
                // step): a third of the room, the text the rest.
                let room = (ui.available_width() - buttons).max(t.metrics.min_hit_target * 2.0);
                let author_width = (room / 3.0).max(t.metrics.min_hit_target);
                let author = text_field_sized(ui, ids::author(note.id), &note.author, author_width);
                if let Some(name) = author.committed.clone() {
                    if let Some(command) = set_author(doc, note.id, name) {
                        w.emit(Intent::Document(command));
                    }
                }
                author.response.on_hover_text(tr(AUTHOR));
                let width = (ui.available_width() - buttons).max(t.metrics.min_hit_target);
                let field = text_field_sized(ui, ids::text(note.id), &note.text, width);
                if let Some(committed) = field.committed {
                    if let Some(command) = extras::edit_note(doc, note.id, committed) {
                        w.emit(Intent::Document(command));
                    }
                }
                if icon_action_id(
                    ui,
                    "target",
                    tr(SHOW),
                    ActionState::Idle,
                    Some(ids::show(note.id)),
                )
                .clicked()
                {
                    // Moved here and then emitted, like the Navigator: the
                    // absorb moves the canvas camera, the application's
                    // second absorb is a no-op (an absolute set).
                    let centre = Intent::SetViewCenter((note.x, note.y));
                    w.absorb(&centre);
                    w.emit(centre);
                }
                if icon_action_id(
                    ui,
                    "trash",
                    tr(DELETE),
                    ActionState::Idle,
                    Some(ids::delete(note.id)),
                )
                .clicked()
                {
                    if let Some(command) = extras::delete_note(doc, note.id) {
                        w.emit(Intent::Document(command));
                    }
                }
            },
        );
    }

    ui.add_space(Space::XSmall.pt());
    hairline(ui);
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), t.metrics.control_height),
        Layout::right_to_left(Align::Center),
        |ui| {
            if icon_action_id(ui, "plus", tr(NEW), ActionState::Idle, Some(ids::new())).clicked() {
                let (x, y) = pin_point(w, doc);
                let (command, _) = extras::add_note(doc, x, y, String::new(), NEW_NOTE_TEXT);
                w.emit(Intent::Document(command));
            }
        },
    );
}

/// W16-E: the Notes panel's Author field: set note `id`'s author, as one
/// undo step. `None` when there is no such note or nothing changes.
pub fn set_author(doc: &Document, id: u64, author: impl Into<String>) -> Option<Command> {
    let index = doc.extras.notes.iter().position(|n| n.id == id)?;
    let author = author.into().trim().to_string();
    if doc.extras.notes[index].author == author {
        return None;
    }
    Some(extras::edit_extras(doc, |x| x.notes[index].author = author))
}

/// Where New Note pins: the centre of the view, clamped into the canvas so a
/// view scrolled off the page still pins on it.
pub fn pin_point(w: &Workspace, doc: &Document) -> (f32, f32) {
    let (x, y) = w.view_center;
    let clamp = |v: f32, max: u32| {
        if v.is_finite() {
            v.clamp(0.0, max as f32)
        } else {
            max as f32 * 0.5
        }
    };
    (clamp(x, doc.width()), clamp(y, doc.height()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W10-B: every string the panel shows is a catalogue key that resolves.
    #[test]
    fn every_string_resolves_through_the_catalogue() {
        for key in [NO_DOCUMENT, NO_NOTES, NEW, SHOW, DELETE, AUTHOR] {
            assert!(!tr(key).is_empty(), "{key} is not in the catalogue");
        }
    }

    #[test]
    fn a_new_note_pins_inside_the_canvas() {
        let doc = Document::new(100, 50, "t");
        let mut w = Workspace::new();
        w.view_center = (500.0, -20.0);
        assert_eq!(pin_point(&w, &doc), (100.0, 0.0));
        w.view_center = (f32::NAN, 10.0);
        assert_eq!(pin_point(&w, &doc), (50.0, 10.0));
    }
}
