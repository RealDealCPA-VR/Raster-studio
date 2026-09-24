//! W10-E: Image ▸ Vectorize Bitmap… — the active pixel layer traced into
//! shape layers, one per colour, as ONE undo step.
//!
//! The tracing is [`vector::trace`], whose module header documents the
//! algorithm (median-cut posterize, a potrace-style colour stack so the
//! shapes tile the image with no hairline gaps, crack-following contours,
//! Schneider least-squares Bézier fitting with corner detection). Here the
//! layer's pixels are read, traced, and each traced colour becomes a shape
//! layer filled with that colour — bottom colour first, above everything, in
//! the source layer's own pixel space (its transform is copied onto every
//! shape) — and the source layer is hidden when the dialog asked, all in one
//! `Command::Transaction`.

use editor_core::{Command, LayerPatch};
use layer_model::{Layer, LayerId, LayerKind, ShapeFillRule, ShapeLayer};

use crate::editor::Editor;

/// The layer Vectorize Bitmap traces: the active one, which must own pixels.
pub fn source_layer(editor: &Editor) -> Result<LayerId, String> {
    let doc = editor.active().ok_or("No document is open")?;
    let id = doc.document.active_layer().ok_or("Select a layer first")?;
    let layer = doc
        .document
        .layers
        .get(id)
        .ok_or("The active layer is not in the document")?;
    match &layer.kind {
        LayerKind::Raster(_) | LayerKind::Generator(_) => Ok(id),
        other => Err(format!(
            "Vectorize Bitmap traces a pixel layer; the active layer is a {}",
            editor_core::layer_class_name(other)
        )),
    }
}

/// Trace the active pixel layer with `spec` and add the shape layers.
pub fn vectorize(editor: &mut Editor, spec: &ui::dialogs::VectorizeSpec) -> Result<String, String> {
    if !spec.is_valid() {
        return Err("Vectorize Bitmap: the settings are out of range".to_string());
    }
    let source = source_layer(editor)?;
    let (command, count) = {
        let doc = editor.active().ok_or("No document is open")?;
        let (w, h) = (doc.document.width(), doc.document.height());
        let rgba = crate::menu_bridge::pixels::read_layer(doc, source);
        let options = vector::trace::TraceOptions {
            colors: spec.colors as usize,
            tolerance: spec.tolerance,
            corner_length: spec.corner,
        };
        let traced = vector::trace::trace(&rgba, w, h, &options)
            .map_err(|e| format!("Vectorize Bitmap: {e}"))?;
        if traced.is_empty() {
            return Err("Vectorize Bitmap: the layer has no opaque pixels to trace".to_string());
        }
        let source_layer = doc
            .document
            .layers
            .get(source)
            .ok_or("The active layer is not in the document")?;
        let base = source_layer.name.clone();
        let transform = source_layer.transform;
        let mut commands = Vec::with_capacity(traced.len() + 1);
        for (i, t) in traced.iter().enumerate() {
            let [r, g, b, a] = t.color;
            let shape = ShapeLayer {
                path_svg: vector::to_svg(&t.path),
                fill: Some([
                    f32::from(r) / 255.0,
                    f32::from(g) / 255.0,
                    f32::from(b) / 255.0,
                    f32::from(a) / 255.0,
                ]),
                fill_rule: ShapeFillRule::NonZero,
                ..ShapeLayer::default()
            };
            let mut layer = Layer::with_kind(
                format!("{base} #{:02X}{:02X}{:02X} ({})", r, g, b, i + 1),
                LayerKind::Shape(shape),
            );
            layer.transform = transform;
            commands.push(Command::create_layer(layer));
        }
        if spec.hide_source {
            commands.push(Command::SetLayerProperties {
                layer_id: source,
                patch: LayerPatch {
                    visible: Some(false),
                    ..LayerPatch::default()
                },
            });
        }
        (
            Command::Transaction {
                label: "Vectorize Bitmap".to_string(),
                commands,
            },
            traced.len(),
        )
    };
    let before = editor.active().map_or(0, |d| d.history_depth());
    editor.apply_command(command);
    let after = editor.active().map_or(0, |d| d.history_depth());
    if after == before {
        return Err("Vectorize Bitmap: the document refused the new layers".to_string());
    }
    let message = format!("Vectorize Bitmap: traced {count} colour(s) into shape layers");
    editor.set_status(message.clone());
    Ok(message)
}
