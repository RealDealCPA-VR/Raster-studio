//! W10-J: File > New's Artboard option — the new document's canvas made an
//! artboard, as Photoshop's New Document "Artboards" box does.
//!
//! The document is created as usual (its base layer filled with the chosen
//! background), then the base layer becomes the artboard's background plate
//! ([`layer_model::artboard`]): it gains the artboard rect (the whole canvas)
//! and colour, and moves into a new group, "Artboard 1", with an empty
//! "Layer 1" above it to draw on. The plate's pixels are already the
//! background over the rect, which is exactly what a plate holds. Like the
//! blank document itself, the conversion is the document's starting state,
//! not an undo step: the history is cleared and the document is clean.

use editor_core::{Command, LayerPatch};
use layer_model::{Artboard, Layer, LayerKind};

use crate::editor::Editor;

/// The artboard group's name, Photoshop's first artboard.
pub const ARTBOARD_NAME: &str = "Artboard 1";

/// Make the active (just created) document's canvas an artboard whose
/// background is `background` (straight-alpha RGBA; alpha 0 is a transparent
/// artboard). Returns the artboard group's id.
pub(crate) fn make_canvas_an_artboard(
    editor: &mut Editor,
    background: [f32; 4],
) -> Result<layer_model::LayerId, String> {
    let doc = editor.active_mut().ok_or("No document is open")?;
    let base = doc
        .document
        .active_layer()
        .ok_or("The new document has no layer")?;
    let Some(LayerKind::Raster(raster)) = doc.document.layers.get(base).map(|l| l.kind.clone())
    else {
        return Err("The new document's base layer is not a pixel layer".to_string());
    };
    let board = Artboard {
        x: 0,
        y: 0,
        width: doc.document.width(),
        height: doc.document.height(),
        background,
    };
    if !board.is_valid() {
        return Err("The artboard needs a finite background and a canvas".to_string());
    }
    let group = Layer::group(ARTBOARD_NAME);
    let layer = Layer::raster("Layer 1");
    let (group_id, layer_id) = (group.id, layer.id);
    let mut plate = raster;
    plate.artboard = Some(board);
    let commands = vec![
        Command::SetLayerKind {
            layer_id: base,
            kind: Box::new(LayerKind::Raster(plate)),
        },
        Command::SetLayerProperties {
            layer_id: base,
            patch: LayerPatch {
                name: Some("Artboard Background".to_string()),
                ..LayerPatch::default()
            },
        },
        Command::create_layer(group),
        Command::MoveLayer {
            layer_id: base,
            parent: Some(group_id),
            index: 0,
        },
        Command::create_layer(layer),
        Command::MoveLayer {
            layer_id,
            parent: Some(group_id),
            index: 0,
        },
    ];
    doc.apply(Command::Transaction {
        label: "New Artboard".into(),
        commands,
    })
    .map_err(|e| e.to_string())?;
    doc.document
        .set_active_layer(Some(layer_id))
        .map_err(|e| e.to_string())?;
    // The document's starting state, as `import::blank_document` leaves its
    // own base layer: nothing to undo, nothing to save.
    doc.history.clear();
    doc.document.mark_saved();
    Ok(group_id)
}
