//! W10-B: the Character Styles and Paragraph Styles panels.
//!
//! A style is a named set of text settings kept in the document
//! ([`layer_model::DocumentExtras::character_styles`] /
//! [`layer_model::DocumentExtras::paragraph_styles`]): a character style is
//! the run's family, size and base style (weight, slant, colour, tracking,
//! …); a paragraph style is the paragraph block (alignment, leading, indents,
//! spacing).
//! The document also records which text layer wears which style
//! ([`layer_model::StyleLink`]), which is what makes a style *live*:
//!
//! * clicking a style's row selects it and puts it on the active text layer
//!   (one undo step: the restyle and the link together);
//! * **Redefine** takes the selected style's settings from the active text
//!   layer and restyles *every* layer that wears it, in one undo step — so
//!   changing a paragraph style changes every paragraph that uses it;
//! * **New** records the active text layer's settings as a new style (the
//!   defaults with no text layer active), **Clear** unlinks the active layer
//!   (its text keeps its look), and **Delete** removes the selected style,
//!   unlinking its wearers.
//!
//! Both panels are one body, [`styles_body`], over [`StyleKind`].
//!
//! # Strings
//!
//! Through the strings catalogue ([`crate::strings::tr`]): each constant
//! below is a catalogue key.

use design::{current_tokens, Space, TextRole};
use editor_core::extras;
use editor_core::Document;
use egui::{Align, Layout, Ui, Vec2};
use layer_model::{CharacterStyle, LayerId, LayerKind, ParagraphStyle, TextLayer};

use crate::intent::Intent;
use crate::strings::tr;
use crate::view::{
    body, empty_state, hairline, hint, icon_action_id, list_row_layout, text, ActionState,
};
use crate::Workspace;

const NO_DOCUMENT: &str = "ui.text_styles.no_document";
const IN_USE: &str = "ui.text_styles.in_use";

/// Which of the two panels.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum StyleKind {
    Character,
    Paragraph,
}

impl StyleKind {
    fn key(self) -> &'static str {
        match self {
            StyleKind::Character => "character",
            StyleKind::Paragraph => "paragraph",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            StyleKind::Character => tr("ui.text_styles.noun.character"),
            StyleKind::Paragraph => tr("ui.text_styles.noun.paragraph"),
        }
    }

    fn empty(self) -> &'static str {
        match self {
            StyleKind::Character => tr("ui.text_styles.empty.character"),
            StyleKind::Paragraph => tr("ui.text_styles.empty.paragraph"),
        }
    }
}

/// Stable ids for a headless test.
pub mod ids {
    use super::StyleKind;

    pub fn new(kind: StyleKind) -> egui::Id {
        egui::Id::new(("raster-text-style-new", kind.key()))
    }
    pub fn redefine(kind: StyleKind) -> egui::Id {
        egui::Id::new(("raster-text-style-redefine", kind.key()))
    }
    pub fn clear(kind: StyleKind) -> egui::Id {
        egui::Id::new(("raster-text-style-clear", kind.key()))
    }
    pub fn delete(kind: StyleKind) -> egui::Id {
        egui::Id::new(("raster-text-style-delete", kind.key()))
    }
    /// The row of the style with document id `style`.
    pub fn row(kind: StyleKind, style: u64) -> egui::Id {
        egui::Id::new(("raster-text-style-row", kind.key(), style))
    }
}

/// One listed style, as the body needs it.
struct Row {
    id: u64,
    name: String,
}

fn rows(doc: &Document, kind: StyleKind) -> Vec<Row> {
    match kind {
        StyleKind::Character => doc
            .extras
            .character_styles
            .iter()
            .map(|s| Row {
                id: s.id,
                name: s.name.clone(),
            })
            .collect(),
        StyleKind::Paragraph => doc
            .extras
            .paragraph_styles
            .iter()
            .map(|s| Row {
                id: s.id,
                name: s.name.clone(),
            })
            .collect(),
    }
}

/// The active layer when it is a text layer.
fn active_text(doc: &Document) -> Option<(LayerId, &TextLayer)> {
    let id = doc.active_layer()?;
    match &doc.layers.get(id)?.kind {
        LayerKind::Text(text) => Some((id, text)),
        _ => None,
    }
}

/// The style `layer` wears, of this kind.
fn worn(doc: &Document, kind: StyleKind, layer: Option<LayerId>) -> Option<u64> {
    let link = doc.extras.link(layer?)?;
    match kind {
        StyleKind::Character => link.character,
        StyleKind::Paragraph => link.paragraph,
    }
}

/// `<noun> <n>` for the lowest free `n`.
pub fn next_style_name(doc: &Document, kind: StyleKind) -> String {
    let taken = rows(doc, kind);
    let mut n = taken.len() + 1;
    loop {
        let name = format!("{} {n}", kind.noun());
        if !taken.iter().any(|r| r.name == name) {
            return name;
        }
        n += 1;
    }
}

fn selected_key(kind: StyleKind) -> egui::Id {
    egui::Id::new(("raster-text-style-selected", kind.key()))
}

/// The row the panel has selected, as the last frame left it.
pub fn selected(ctx: &egui::Context, kind: StyleKind) -> Option<u64> {
    ctx.data(|d| d.get_temp(selected_key(kind)))
}

fn set_selected(ctx: &egui::Context, kind: StyleKind, style: Option<u64>) {
    ctx.data_mut(|d| match style {
        Some(s) => d.insert_temp(selected_key(kind), s),
        None => d.remove::<u64>(selected_key(kind)),
    });
}

/// The command that redefines style `id` from `text`'s current settings and
/// restyles every layer wearing it.
pub fn redefine_from(
    doc: &Document,
    kind: StyleKind,
    id: u64,
    text: &TextLayer,
) -> Option<editor_core::Command> {
    match kind {
        StyleKind::Character => {
            let old = doc.extras.character_styles.iter().find(|s| s.id == id)?;
            extras::redefine_character_style(
                doc,
                CharacterStyle::from_text(id, old.name.clone(), text),
            )
        }
        StyleKind::Paragraph => {
            let old = doc.extras.paragraph_styles.iter().find(|s| s.id == id)?;
            extras::redefine_paragraph_style(
                doc,
                ParagraphStyle::from_text(id, old.name.clone(), text),
            )
        }
    }
}

/// Draw one of the two panels.
pub(crate) fn styles_body(w: &mut Workspace, ui: &mut Ui, doc: &Document, kind: StyleKind) {
    if doc.width() == 0 || doc.height() == 0 {
        empty_state(ui, tr(NO_DOCUMENT));
        return;
    }
    let listed = rows(doc, kind);
    let active = active_text(doc);
    let active_id = active.map(|(id, _)| id);
    let wearing = worn(doc, kind, active_id);
    // A selection that names a deleted style falls back to the style the
    // active layer wears.
    let mut chosen = selected(ui.ctx(), kind)
        .filter(|s| listed.iter().any(|r| r.id == *s))
        .or(wearing);

    if listed.is_empty() {
        ui.label(hint(ui, kind.empty()));
    }
    for row in &listed {
        let response = list_row_layout(ui, ids::row(kind, row.id), chosen == Some(row.id), |ui| {
            ui.add_space(Space::XSmall.pt());
            ui.label(body(ui, row.name.clone()));
            if wearing == Some(row.id) {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(Space::XSmall.pt());
                    ui.label(text(
                        ui,
                        tr(IN_USE),
                        TextRole::Secondary,
                        design::TypeRole::Caption,
                    ));
                });
            }
        })
        .response;
        if response.clicked() {
            chosen = Some(row.id);
            if let Some((layer, _)) = active {
                let command = match kind {
                    StyleKind::Character => extras::apply_character_style(doc, layer, Some(row.id)),
                    StyleKind::Paragraph => extras::apply_paragraph_style(doc, layer, Some(row.id)),
                };
                if let Some(command) = command {
                    w.emit(Intent::Document(command));
                }
            }
        }
    }

    ui.add_space(Space::XSmall.pt());
    hairline(ui);
    let t = current_tokens(ui);
    let with_text = |on: bool| {
        if on {
            ActionState::Idle
        } else {
            ActionState::Disabled
        }
    };
    let noun = kind.noun();
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), t.metrics.control_height),
        Layout::right_to_left(Align::Center),
        |ui| {
            if icon_action_id(
                ui,
                "trash",
                &tr("ui.text_styles.delete").replace("{noun}", noun),
                with_text(chosen.is_some()),
                Some(ids::delete(kind)),
            )
            .clicked()
            {
                let command = chosen.and_then(|id| match kind {
                    StyleKind::Character => extras::delete_character_style(doc, id),
                    StyleKind::Paragraph => extras::delete_paragraph_style(doc, id),
                });
                if let Some(command) = command {
                    w.emit(Intent::Document(command));
                    chosen = None;
                }
            }
            if icon_action_id(
                ui,
                "plus",
                &tr("ui.text_styles.new").replace("{noun}", noun),
                ActionState::Idle,
                Some(ids::new(kind)),
            )
            .clicked()
            {
                let name = next_style_name(doc, kind);
                let (command, id) = match kind {
                    StyleKind::Character => extras::new_character_style(doc, name, active_id),
                    StyleKind::Paragraph => extras::new_paragraph_style(doc, name, active_id),
                };
                w.emit(Intent::Document(command));
                chosen = Some(id);
            }
            if icon_action_id(
                ui,
                "check",
                &tr("ui.text_styles.redefine").replace("{noun}", noun),
                with_text(chosen.is_some() && active.is_some()),
                Some(ids::redefine(kind)),
            )
            .clicked()
            {
                let command = chosen
                    .zip(active)
                    .and_then(|(id, (_, text))| redefine_from(doc, kind, id, text));
                if let Some(command) = command {
                    w.emit(Intent::Document(command));
                }
            }
            if icon_action_id(
                ui,
                "close",
                &tr("ui.text_styles.clear").replace("{noun}", noun),
                with_text(wearing.is_some()),
                Some(ids::clear(kind)),
            )
            .clicked()
            {
                let command = active_id.and_then(|layer| match kind {
                    StyleKind::Character => extras::apply_character_style(doc, layer, None),
                    StyleKind::Paragraph => extras::apply_paragraph_style(doc, layer, None),
                });
                if let Some(command) = command {
                    w.emit(Intent::Document(command));
                }
            }
        },
    );
    set_selected(ui.ctx(), kind, chosen);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W10-B: every string the panel shows is a catalogue key that resolves.
    #[test]
    fn every_string_resolves_through_the_catalogue() {
        for key in [
            NO_DOCUMENT,
            IN_USE,
            "ui.text_styles.delete",
            "ui.text_styles.new",
            "ui.text_styles.redefine",
            "ui.text_styles.clear",
        ] {
            assert!(!tr(key).is_empty(), "{key} is not in the catalogue");
        }
    }

    #[test]
    fn style_names_count_up_per_kind() {
        let mut doc = Document::new(8, 8, "t");
        assert_eq!(
            next_style_name(&doc, StyleKind::Paragraph),
            "Paragraph Style 1"
        );
        let (command, _) = extras::new_paragraph_style(&doc, "Paragraph Style 1", None);
        command.apply(&mut doc).unwrap();
        assert_eq!(
            next_style_name(&doc, StyleKind::Paragraph),
            "Paragraph Style 2"
        );
        assert_eq!(
            next_style_name(&doc, StyleKind::Character),
            "Character Style 1"
        );
    }
}
