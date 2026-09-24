//! W10-A: Edit ▸ Define Custom Shape.
//!
//! The outline comes from the active shape layer (its path mapped through the
//! layer's transform, so a rotated shape is defined as it is seen) or, when
//! the active layer is not a shape, from the Paths panel's current path (the
//! selected path, else the Work Path — the one the menu context parked).
//!
//! It is normalised to the unit square the Custom Shape tool fits into the
//! box the user drags ([`vector::custom::normalised`]), stored as cubic knots
//! ([`vector::custom::knots_of`]) in the shape presets beside the `.csh`
//! imports — so it survives a restart — and registered with the Custom Shape
//! tool's Shape list ([`tools::registry::register_custom_shapes`]), where it
//! is listed after the built-in library and the imports.

use asset_store::resources::{KnotResource, ShapeResource, SubpathResource};

use crate::editor::Editor;

/// The name a defined shape is given: the shape layer's own name, else
/// "Custom Shape", with " 2", " 3", ... appended until no listed shape (built
/// in, imported or defined earlier) has it.
fn fresh_name(base: &str) -> String {
    let base = base.trim();
    let base = if base.is_empty() {
        "Custom Shape"
    } else {
        base
    };
    let taken = |n: &str| tools::registry::custom_shape_choices().contains(&n);
    if !taken(base) {
        return base.to_owned();
    }
    (2..)
        .map(|i| format!("{base} {i}"))
        .find(|n| !taken(n))
        .expect("an unbounded range finds a free name")
}

/// The outline to define and the name to start from: the active shape
/// layer's, in document space, else the current path's.
fn source_outline(editor: &Editor) -> Result<(vector::Path, String), String> {
    let doc = editor.active().ok_or("No document is open")?;
    let from_layer = doc
        .document
        .active_layer()
        .and_then(|id| doc.document.layers.get(id))
        .and_then(|layer| match &layer.kind {
            layer_model::LayerKind::Shape(shape) => Some((
                compositor::vector_mask::svg_transformed(&shape.path_svg, layer.transform),
                layer.name.clone(),
            )),
            _ => None,
        });
    let (svg, name) = match from_layer {
        Some((svg, name)) => (svg.ok_or("The shape layer's outline cannot be read")?, name),
        None => (
            super::current_vector_path().ok_or(
                "Define Custom Shape needs a shape layer or a path: select a shape layer, \
                 or draw a path with the Pen",
            )?,
            String::new(),
        ),
    };
    let path = vector::parse_svg(&svg).map_err(|e| format!("The outline cannot be read: {e}"))?;
    Ok((path, name))
}

/// Edit ▸ Define Custom Shape: see the module docs. The status line names the
/// shape and where to pick it.
pub(crate) fn define_custom_shape(editor: &mut Editor) -> Result<String, String> {
    let (path, base) = source_outline(editor)?;
    let b = path.bounds();
    if path.is_empty()
        || !path.is_finite()
        || !(b.width() > 0.0 && b.height() > 0.0)
        || path.subpaths().iter().all(|s| s.segments.is_empty())
    {
        return Err("The outline encloses nothing, so there is no shape to define".into());
    }
    let unit = vector::custom::normalised(&path);
    let name = fresh_name(&base);
    let resource = ShapeResource {
        name: name.clone(),
        id: String::new(),
        subpaths: vector::custom::knots_of(&unit)
            .into_iter()
            .map(|(closed, knots)| SubpathResource {
                closed,
                knots: knots
                    .into_iter()
                    .map(|[before, anchor, after]| KnotResource {
                        before: [before.x, before.y],
                        anchor: [anchor.x, anchor.y],
                        after: [after.x, after.y],
                    })
                    .collect(),
            })
            .collect(),
    };
    // The stored form is what the next session registers, so the picker gets
    // exactly that form now, not the path it was made from.
    let stored = vector::custom::path_from_knots(&vector::custom::knots_of(&unit));
    let listed = tools::registry::register_custom_shapes([(name.as_str(), stored)]);
    if !listed.contains(&name.as_str()) {
        return Err(format!(
            "“{name}” could not be added to the Custom Shape list"
        ));
    }
    editor.presets_mut().define_shape(resource);
    let file = editor.paths().presets_file();
    if let Err(e) = editor.presets().save(&file) {
        tracing::warn!("could not write the presets: {e}");
    }
    Ok(format!(
        "Defined custom shape “{name}”; pick it from the Custom Shape tool's Shape list"
    ))
}
