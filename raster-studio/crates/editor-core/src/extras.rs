//! W10-B: the commands behind the Layer Comps, Notes, Character Styles and
//! Paragraph Styles panels.
//!
//! Every builder reads the document and answers a finished [`Command`] — or
//! `None` when the request names something that is not there — so a panel
//! can emit it as an `Intent::Document` and the edit rides history like any
//! other: one undo step per gesture. The record itself changes through
//! [`Command::SetDocumentExtras`]; applying a comp or restyling text layers
//! adds the layer edits in the same [`Command::Transaction`].

use glam::Affine2;
use layer_model::{
    CharacterStyle, DocumentExtras, LayerComp, LayerId, LayerKind, Note, ParagraphStyle,
};

use crate::command::{Command, LayerPatch};
use crate::document::Document;

/// `doc`'s record with `edit` applied, as the command that makes it so.
pub fn edit_extras(doc: &Document, edit: impl FnOnce(&mut DocumentExtras)) -> Command {
    let mut extras = doc.extras.clone();
    edit(&mut extras);
    Command::SetDocumentExtras {
        extras: Box::new(extras),
    }
}

// ---------------------------------------------------------------------------
// Layer Comps
// ---------------------------------------------------------------------------

/// Layer Comps ▸ New: record every layer as it stands now.
pub fn new_layer_comp(doc: &Document, name: impl Into<String>) -> Command {
    let comp = LayerComp::capture(name, &doc.layers);
    let label = format!("New Layer Comp {}", comp.name);
    let set = edit_extras(doc, |x| {
        x.layer_comps.push(comp);
        x.last_comp = Some(x.layer_comps.len() - 1);
    });
    Command::Transaction {
        label,
        commands: vec![set],
    }
}

/// Layer Comps ▸ Update: re-record comp `index` from the layers as they stand.
pub fn update_layer_comp(doc: &Document, index: usize) -> Option<Command> {
    let old = doc.extras.layer_comps.get(index)?;
    let mut comp = LayerComp::capture(old.name.clone(), &doc.layers);
    comp.comment = old.comment.clone();
    Some(edit_extras(doc, |x| {
        x.layer_comps[index] = comp;
        x.last_comp = Some(index);
    }))
}

/// Layer Comps ▸ Delete.
pub fn delete_layer_comp(doc: &Document, index: usize) -> Option<Command> {
    doc.extras.layer_comps.get(index)?;
    Some(edit_extras(doc, |x| {
        x.layer_comps.remove(index);
        x.last_comp = match x.last_comp {
            Some(i) if i == index => None,
            Some(i) if i > index => Some(i - 1),
            other => other,
        };
    }))
}

/// Rename comp `index` (and set its comment).
pub fn rename_layer_comp(
    doc: &Document,
    index: usize,
    name: impl Into<String>,
    comment: impl Into<String>,
) -> Option<Command> {
    doc.extras.layer_comps.get(index)?;
    let (name, comment) = (name.into(), comment.into());
    Some(edit_extras(doc, |x| {
        x.layer_comps[index].name = name;
        x.layer_comps[index].comment = comment;
    }))
}

/// Layer Comps ▸ Apply: put back the visibility, position and appearance comp
/// `index` recorded, for every layer that still exists, as one undoable step.
///
/// Only the properties that differ are patched. A layer that is fully locked
/// keeps its state, and a position-locked layer keeps its position — the same
/// locks every other edit obeys, and a refusal of one layer must not cost the
/// user the rest of the comp.
pub fn apply_layer_comp(doc: &Document, index: usize) -> Option<Command> {
    let comp = doc.extras.layer_comps.get(index)?;
    let mut commands = Vec::new();
    for state in &comp.layers {
        let Some(layer) = doc.layers.get(state.layer) else {
            continue;
        };
        if layer.locked.all {
            continue;
        }
        let mut patch = LayerPatch::default();
        let mut any = false;
        if layer.visible != state.visible {
            patch.visible = Some(state.visible);
            any = true;
        }
        let transform_ok = state.transform.iter().all(|v| v.is_finite());
        if transform_ok
            && layer.transform != Affine2::from_cols_array(&state.transform)
            && !layer.locked.blocks_transform()
        {
            patch.transform = Some(state.transform);
            any = true;
        }
        let opacity = state.opacity.clamp(0.0, 1.0);
        if state.opacity.is_finite() && layer.opacity != opacity {
            patch.opacity = Some(opacity);
            any = true;
        }
        let fill = state.fill_opacity.clamp(0.0, 1.0);
        if state.fill_opacity.is_finite() && layer.fill_opacity != fill {
            patch.fill_opacity = Some(fill);
            any = true;
        }
        if layer.blend_mode != state.blend_mode {
            patch.blend_mode = Some(state.blend_mode);
            any = true;
        }
        if layer.effects != state.effects {
            patch.effects = Some(Box::new(state.effects.clone()));
            any = true;
        }
        if any {
            commands.push(Command::SetLayerProperties {
                layer_id: state.layer,
                patch,
            });
        }
    }
    commands.push(edit_extras(doc, |x| x.last_comp = Some(index)));
    Some(Command::Transaction {
        label: format!("Apply Layer Comp {}", comp.name),
        commands,
    })
}

/// Layer Comps ▸ Previous / Next: apply the comp one before or after the one
/// last applied, wrapping round. With none applied yet, Next starts at the
/// first comp and Previous at the last.
pub fn step_layer_comp(doc: &Document, forward: bool) -> Option<Command> {
    let count = doc.extras.layer_comps.len();
    if count == 0 {
        return None;
    }
    let index = match (doc.extras.last_comp.filter(|i| *i < count), forward) {
        (Some(i), true) => (i + 1) % count,
        (Some(i), false) => (i + count - 1) % count,
        (None, true) => 0,
        (None, false) => count - 1,
    };
    apply_layer_comp(doc, index)
}

// ---------------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------------

/// Pin a new note at document position (`x`, `y`). Answers the command and
/// the new note's id.
pub fn add_note(
    doc: &Document,
    x: f32,
    y: f32,
    author: impl Into<String>,
    text: impl Into<String>,
) -> (Command, u64) {
    let id = doc.extras.next_id();
    let note = Note {
        id,
        x: if x.is_finite() { x } else { 0.0 },
        y: if y.is_finite() { y } else { 0.0 },
        author: author.into(),
        text: text.into(),
    };
    (edit_extras(doc, |x| x.notes.push(note)), id)
}

/// Replace note `id`'s text.
pub fn edit_note(doc: &Document, id: u64, text: impl Into<String>) -> Option<Command> {
    let index = doc.extras.notes.iter().position(|n| n.id == id)?;
    let text = text.into();
    if doc.extras.notes[index].text == text {
        return None;
    }
    Some(edit_extras(doc, |x| x.notes[index].text = text))
}

/// Move note `id` to document position (`x`, `y`).
pub fn move_note(doc: &Document, id: u64, x: f32, y: f32) -> Option<Command> {
    let index = doc.extras.notes.iter().position(|n| n.id == id)?;
    if !(x.is_finite() && y.is_finite()) {
        return None;
    }
    Some(edit_extras(doc, |r| {
        r.notes[index].x = x;
        r.notes[index].y = y;
    }))
}

/// Delete note `id`.
pub fn delete_note(doc: &Document, id: u64) -> Option<Command> {
    let index = doc.extras.notes.iter().position(|n| n.id == id)?;
    Some(edit_extras(doc, |x| {
        x.notes.remove(index);
    }))
}

// ---------------------------------------------------------------------------
// Character and Paragraph Styles
// ---------------------------------------------------------------------------

fn text_of(doc: &Document, layer: LayerId) -> Option<&layer_model::TextLayer> {
    match &doc.layers.get(layer)?.kind {
        LayerKind::Text(text) => Some(text),
        _ => None,
    }
}

/// The restyle edits for every layer in `layers` whose kind `restyle`
/// changes. Fully locked layers are left alone, as every kind edit leaves
/// them.
fn restyle(
    doc: &Document,
    layers: &[LayerId],
    restyle: impl Fn(&LayerKind) -> Option<LayerKind>,
) -> Vec<Command> {
    layers
        .iter()
        .filter_map(|id| {
            let layer = doc.layers.get(*id)?;
            if layer.locked.all {
                return None;
            }
            let next = restyle(&layer.kind)?;
            (next != layer.kind).then(|| Command::SetLayerKind {
                layer_id: *id,
                kind: Box::new(next),
            })
        })
        .collect()
}

/// Character Styles ▸ New: a style from `from`'s run (or the defaults, with
/// no text layer). Answers the command and the new style's id.
pub fn new_character_style(
    doc: &Document,
    name: impl Into<String>,
    from: Option<LayerId>,
) -> (Command, u64) {
    let id = doc.extras.next_id();
    let style = match from.and_then(|l| text_of(doc, l)) {
        Some(text) => CharacterStyle::from_text(id, name, text),
        None => CharacterStyle {
            id,
            name: name.into(),
            ..CharacterStyle::default()
        },
    };
    (edit_extras(doc, |x| x.character_styles.push(style)), id)
}

/// Put character style `id` on text layer `layer` and record the link; `None`
/// clears the link and leaves the run as it is.
pub fn apply_character_style(doc: &Document, layer: LayerId, id: Option<u64>) -> Option<Command> {
    text_of(doc, layer)?;
    let mut commands = Vec::new();
    if let Some(id) = id {
        let style = doc.extras.character_styles.iter().find(|s| s.id == id)?;
        commands.extend(restyle(doc, &[layer], |k| style.applied(k)));
    }
    commands.push(edit_extras(doc, |x| x.link_character(layer, id)));
    Some(Command::Transaction {
        label: "Apply Character Style".into(),
        commands,
    })
}

/// Redefine character style `style.id` and restyle every layer wearing it,
/// as one undoable step.
pub fn redefine_character_style(doc: &Document, style: CharacterStyle) -> Option<Command> {
    let index = doc
        .extras
        .character_styles
        .iter()
        .position(|s| s.id == style.id)?;
    let layers = doc.extras.layers_with_character(style.id);
    let mut commands = restyle(doc, &layers, |k| style.applied(k));
    commands.push(edit_extras(doc, |x| {
        x.character_styles[index] = style.clone()
    }));
    Some(Command::Transaction {
        label: "Redefine Character Style".into(),
        commands,
    })
}

/// Delete character style `id`. Layers that wore it keep their run and lose
/// the link.
pub fn delete_character_style(doc: &Document, id: u64) -> Option<Command> {
    let index = doc
        .extras
        .character_styles
        .iter()
        .position(|s| s.id == id)?;
    let wearers = doc.extras.layers_with_character(id);
    Some(edit_extras(doc, |x| {
        x.character_styles.remove(index);
        for layer in wearers {
            x.link_character(layer, None);
        }
    }))
}

/// Paragraph Styles ▸ New: a style from `from`'s paragraph settings (or the
/// defaults). Answers the command and the new style's id.
pub fn new_paragraph_style(
    doc: &Document,
    name: impl Into<String>,
    from: Option<LayerId>,
) -> (Command, u64) {
    let id = doc.extras.next_id();
    let style = match from.and_then(|l| text_of(doc, l)) {
        Some(text) => ParagraphStyle::from_text(id, name, text),
        None => ParagraphStyle {
            id,
            name: name.into(),
            ..ParagraphStyle::default()
        },
    };
    (edit_extras(doc, |x| x.paragraph_styles.push(style)), id)
}

/// Put paragraph style `id` on text layer `layer` and record the link; `None`
/// clears the link.
pub fn apply_paragraph_style(doc: &Document, layer: LayerId, id: Option<u64>) -> Option<Command> {
    text_of(doc, layer)?;
    let mut commands = Vec::new();
    if let Some(id) = id {
        let style = doc.extras.paragraph_styles.iter().find(|s| s.id == id)?;
        commands.extend(restyle(doc, &[layer], |k| style.applied(k)));
    }
    commands.push(edit_extras(doc, |x| x.link_paragraph(layer, id)));
    Some(Command::Transaction {
        label: "Apply Paragraph Style".into(),
        commands,
    })
}

/// Redefine paragraph style `style.id` and restyle every layer wearing it, as
/// one undoable step.
pub fn redefine_paragraph_style(doc: &Document, style: ParagraphStyle) -> Option<Command> {
    let index = doc
        .extras
        .paragraph_styles
        .iter()
        .position(|s| s.id == style.id)?;
    let layers = doc.extras.layers_with_paragraph(style.id);
    let mut commands = restyle(doc, &layers, |k| style.applied(k));
    commands.push(edit_extras(doc, |x| {
        x.paragraph_styles[index] = style.clone()
    }));
    Some(Command::Transaction {
        label: "Redefine Paragraph Style".into(),
        commands,
    })
}

/// Delete paragraph style `id`. Layers that wore it keep their settings and
/// lose the link.
pub fn delete_paragraph_style(doc: &Document, id: u64) -> Option<Command> {
    let index = doc
        .extras
        .paragraph_styles
        .iter()
        .position(|s| s.id == id)?;
    let wearers = doc.extras.layers_with_paragraph(id);
    Some(edit_extras(doc, |x| {
        x.paragraph_styles.remove(index);
        for layer in wearers {
            x.link_paragraph(layer, None);
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::History;
    use layer_model::text::Alignment;
    use layer_model::{Layer, TextLayer};

    fn text_layer(name: &str) -> Layer {
        Layer::with_kind(
            name,
            LayerKind::Text(TextLayer {
                text: name.into(),
                ..TextLayer::default()
            }),
        )
    }

    #[test]
    fn a_comp_restores_visibility_position_and_appearance_in_one_undo_step() {
        let mut doc = Document::new(64, 64, "Comps");
        let mut history = History::new();
        let a = doc.layers.push_root(Layer::raster("A")).unwrap();
        let b = doc.layers.push_root(Layer::raster("B")).unwrap();
        let step = new_layer_comp(&doc, "Both");
        history.apply(&mut doc, step).unwrap();

        // Hide A, move B, fade B.
        history
            .apply(
                &mut doc,
                Command::SetLayerProperties {
                    layer_id: a,
                    patch: LayerPatch {
                        visible: Some(false),
                        ..Default::default()
                    },
                },
            )
            .unwrap();
        history
            .apply(
                &mut doc,
                Command::SetLayerProperties {
                    layer_id: b,
                    patch: LayerPatch {
                        opacity: Some(0.25),
                        transform: Some([1.0, 0.0, 0.0, 1.0, 9.0, 4.0]),
                        ..Default::default()
                    },
                },
            )
            .unwrap();

        let apply = apply_layer_comp(&doc, 0).expect("the comp exists");
        history.apply(&mut doc, apply).unwrap();
        assert!(doc.layers.get(a).unwrap().visible, "visibility restored");
        assert_eq!(doc.layers.get(b).unwrap().opacity, 1.0);
        assert_eq!(doc.layers.get(b).unwrap().transform, Affine2::IDENTITY);
        assert_eq!(doc.extras.last_comp, Some(0));

        // One undo takes the whole comp back.
        history.undo(&mut doc).unwrap();
        assert!(!doc.layers.get(a).unwrap().visible);
        assert_eq!(doc.layers.get(b).unwrap().opacity, 0.25);
    }

    #[test]
    fn previous_and_next_wrap_round_the_comp_list() {
        let mut doc = Document::new(8, 8, "Comps");
        let mut history = History::new();
        doc.layers.push_root(Layer::raster("A")).unwrap();
        let step = new_layer_comp(&doc, "One");
        history.apply(&mut doc, step).unwrap();
        let step = new_layer_comp(&doc, "Two");
        history.apply(&mut doc, step).unwrap();
        assert_eq!(doc.extras.last_comp, Some(1));
        let step = step_layer_comp(&doc, true).unwrap();
        history.apply(&mut doc, step).unwrap();
        assert_eq!(doc.extras.last_comp, Some(0), "Next wraps to the first");
        let step = step_layer_comp(&doc, false).unwrap();
        history.apply(&mut doc, step).unwrap();
        assert_eq!(doc.extras.last_comp, Some(1), "Previous wraps to the last");
    }

    #[test]
    fn a_paragraph_style_change_updates_every_layer_that_wears_it() {
        let mut doc = Document::new(64, 64, "Styles");
        let mut history = History::new();
        let one = doc.layers.push_root(text_layer("One")).unwrap();
        let two = doc.layers.push_root(text_layer("Two")).unwrap();
        let other = doc.layers.push_root(text_layer("Other")).unwrap();
        let (new, id) = new_paragraph_style(&doc, "Body", Some(one));
        history.apply(&mut doc, new).unwrap();
        for layer in [one, two] {
            let apply = apply_paragraph_style(&doc, layer, Some(id)).unwrap();
            history.apply(&mut doc, apply).unwrap();
        }
        let mut style = doc.extras.paragraph_styles[0].clone();
        style.paragraph.alignment = Alignment::Center;
        style.paragraph.first_line_indent = 20.0;
        let step = redefine_paragraph_style(&doc, style).unwrap();
        history.apply(&mut doc, step).unwrap();
        let para = |doc: &Document, l| match &doc.layers.get(l).unwrap().kind {
            LayerKind::Text(t) => t.paragraph,
            _ => unreachable!(),
        };
        for layer in [one, two] {
            assert_eq!(para(&doc, layer).alignment, Alignment::Center);
            assert_eq!(para(&doc, layer).first_line_indent, 20.0);
        }
        assert_ne!(para(&doc, other).alignment, Alignment::Center, "not linked");
        history.undo(&mut doc).unwrap();
        assert_ne!(
            para(&doc, one).alignment,
            Alignment::Center,
            "one undo step"
        );
        assert_ne!(para(&doc, two).alignment, Alignment::Center);
    }

    #[test]
    fn a_character_style_change_restyles_its_layers() {
        let mut doc = Document::new(64, 64, "Styles");
        let mut history = History::new();
        let one = doc.layers.push_root(text_layer("One")).unwrap();
        let (new, id) = new_character_style(&doc, "Heading", Some(one));
        history.apply(&mut doc, new).unwrap();
        let step = apply_character_style(&doc, one, Some(id)).unwrap();
        history.apply(&mut doc, step).unwrap();
        let mut style = doc.extras.character_styles[0].clone();
        style.size_px = 72.0;
        let step = redefine_character_style(&doc, style).unwrap();
        history.apply(&mut doc, step).unwrap();
        match &doc.layers.get(one).unwrap().kind {
            LayerKind::Text(t) => assert_eq!(t.size_px, 72.0),
            _ => unreachable!(),
        }
        let step = delete_character_style(&doc, id).unwrap();
        history.apply(&mut doc, step).unwrap();
        assert!(doc.extras.style_links.is_empty());
    }

    #[test]
    fn notes_add_edit_and_delete_and_survive_a_json_round_trip() {
        let mut doc = Document::new(64, 64, "Notes");
        let mut history = History::new();
        let (add, id) = add_note(&doc, 10.0, 12.0, "VR", "fix the sky");
        history.apply(&mut doc, add).unwrap();
        let step = edit_note(&doc, id, "fix the sky tonight").unwrap();
        history.apply(&mut doc, step).unwrap();
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.extras.notes.len(), 1);
        assert_eq!(back.extras.notes[0].text, "fix the sky tonight");
        assert_eq!(back, doc);
        let step = delete_note(&doc, id).unwrap();
        history.apply(&mut doc, step).unwrap();
        assert!(doc.extras.notes.is_empty());
        history.undo(&mut doc).unwrap();
        assert_eq!(doc.extras.notes.len(), 1);
    }
}
