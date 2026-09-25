//! W13-I: Quick Export — the Move tool options bar's button and File ▸
//! Export ▸ Quick Export Layer as PNG… (one [`ui::menu::MenuAction`], so the
//! bar and the menu cannot drift).
//!
//! The active layer is composited alone over transparent at canvas size,
//! through the real compositor, so its blend, opacity, mask and effects are
//! honoured the way Export Layers honours them. "Alone" hides every layer
//! that is neither the active layer, one of its enclosing groups (which must
//! stay visible for it to draw) nor, for a group, one of its children. The
//! PNG is written where the user picks, the suggested name being
//! `<document>-<layer>.png` beside the document. It runs on the interaction
//! thread: one layer, one encode.

use layer_model::LayerId;

use crate::editor::Editor;

/// Every enclosing group of `id`, innermost first.
fn ancestors(layers: &layer_model::LayerTree, id: LayerId) -> Vec<LayerId> {
    let mut out = Vec::new();
    let mut parent = layers.parent_of(id);
    while let Some(p) = parent {
        out.push(p);
        parent = layers.parent_of(p);
    }
    out
}

/// The canvas-sized straight-alpha RGBA8 of layer `id` composited alone.
pub(crate) fn layer_alone_rgba8(
    doc: &crate::doc::OpenDocument,
    id: LayerId,
) -> Result<(u32, u32, Vec<u8>), String> {
    let mut staged = doc.document.clone();
    let keep_above = ancestors(&staged.layers, id);
    for other in staged.layers.iter_depth_first() {
        let related = other == id
            || keep_above.contains(&other)
            || ancestors(&staged.layers, other).contains(&id);
        if !related {
            if let Some(layer) = staged.layers.get_mut(other) {
                layer.visible = false;
            }
        }
    }
    let (w, h) = (staged.width(), staged.height());
    let canvas = compositor::composite_region(
        &staged,
        &doc.tiles,
        raster::PixelRect::new(0, 0, w, h),
        0,
        compositor::CompositeOptions::default(),
    )
    .map_err(|e| e.to_string())?;
    Ok((w, h, canvas.to_rgba8(&staged.meta.color_space)))
}

/// Quick Export: ask where, write the active layer alone as a PNG, say so.
pub(crate) fn quick_export_layer(editor: &mut Editor) -> Result<String, String> {
    let (id, name, suggested) = {
        let doc = editor
            .active()
            .ok_or_else(|| "No document is open".to_string())?;
        let id = doc
            .document
            .active_layer()
            .ok_or_else(|| "Quick Export needs an active layer".to_string())?;
        let name = doc
            .document
            .layers
            .get(id)
            .map(|l| l.name.clone())
            .unwrap_or_default();
        let beside = doc.suggested_export_path();
        let stem = beside
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file = format!("{stem}-{}.png", crate::editor::safe_file_name(&name));
        (id, name, beside.with_file_name(file))
    };
    let Some(target) = editor.pick_save_path(&suggested) else {
        return Err("Quick Export: no destination chosen".to_string());
    };
    let is_png = target
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"));
    let target = if is_png {
        target
    } else {
        target.with_extension("png")
    };
    let (w, h, rgba) = {
        let doc = editor
            .active()
            .ok_or_else(|| "No document is open".to_string())?;
        layer_alone_rgba8(doc, id)?
    };
    raster::encode_to_path(
        &target,
        raster::ExportFormat::Png,
        w,
        h,
        raster::EncodedPixels::Rgba8(&rgba),
        &raster::EncodeOptions::default(),
    )
    .map_err(|e| format!("Quick Export: {e}"))?;
    // The bridge puts the returned line on the status bar.
    Ok(format!(
        "Quick Export: layer \"{name}\" written to {}",
        target.display()
    ))
}
