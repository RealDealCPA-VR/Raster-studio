//! W10-I: the Layer-menu rows Photopea has and this build lacked.
//!
//! * **Layer ▸ Hide Layers / Show Layers**: every selected layer's eye,
//!   one undo step (Ctrl+, stays the active layer's show / hide toggle).
//! * **Layer ▸ Matting ▸ Remove Black / White Matte**: undo a matte the
//!   layer's partly transparent pixels were composited against — each
//!   colour is un-premultiplied against black (or white) in the encoded
//!   values, Photoshop's arithmetic, one undo step.
//! * **Layer ▸ Smart Object ▸ New Smart Object via Copy / Convert to
//!   Layers** (Export Contents and Relink to File are `Editor` methods,
//!   because they ask the file dialogs).
//! * **Layer ▸ Smart Filter ▸ Add / Edit / Disable-Enable / Delete Filter
//!   Mask**: the smart filters' shared mask, which the Layers panel's
//!   filter-mask row raises too.

use compositor::TileSource;
use editor_core::pixels::{PixelTarget, TileEdit};
use editor_core::{Command, LayerPatch};
use layer_model::{AssetId, AssetOrigin, AssetRecord, Layer, LayerId, LayerKind, MaskId};

use crate::editor::Editor;

/// The layers a Layer-menu row acts on: the multi-selection when there is
/// one, else the active layer.
fn targets(editor: &Editor) -> Result<Vec<LayerId>, String> {
    let doc = editor.active().ok_or("No document is open")?;
    let selection = doc.document.layer_selection();
    if !selection.is_empty() {
        return Ok(selection);
    }
    doc.document
        .active_layer()
        .map(|id| vec![id])
        .ok_or_else(|| "Select a layer first".to_string())
}

/// Layer ▸ Hide Layers (`visible == false`) / Show Layers (`true`): every
/// target layer whose eye differs, as one Transaction.
pub(crate) fn set_layers_visible(editor: &mut Editor, visible: bool) -> Result<String, String> {
    let layers = targets(editor)?;
    let doc = editor.active().ok_or("No document is open")?;
    let commands: Vec<Command> = layers
        .iter()
        .filter(|id| {
            doc.document
                .layers
                .get(**id)
                .is_some_and(|l| l.visible != visible)
        })
        .map(|id| Command::SetLayerProperties {
            layer_id: *id,
            patch: LayerPatch {
                visible: Some(visible),
                ..LayerPatch::default()
            },
        })
        .collect();
    let (verb, already) = if visible {
        ("Show Layers", "Every selected layer is already showing")
    } else {
        ("Hide Layers", "Every selected layer is already hidden")
    };
    if commands.is_empty() {
        return Err(already.to_string());
    }
    let count = commands.len();
    editor.apply_command(Command::Transaction {
        label: verb.to_string(),
        commands,
    });
    Ok(format!(
        "{} {count} layer{}",
        if visible { "Showed" } else { "Hid" },
        if count == 1 { "" } else { "s" }
    ))
}

/// One premultiplied linear pixel with the matte it was composited against
/// taken back out: `matte` is `0.0` (black) or `1.0` (white). Photoshop's
/// arithmetic, in the encoded values a matte was mixed in: a pixel stored
/// as `s = t·a + m·(1 − a)` was the colour `t = (s − m·(1 − a)) / a`. An
/// opaque or empty pixel is returned unchanged.
pub(crate) fn remove_matte(px: [f32; 4], matte: f32) -> [f32; 4] {
    let a = px[3];
    if a <= 0.0 || a >= 1.0 {
        return px;
    }
    let channel = |p: f32| {
        let stored = color::linear_to_srgb((p / a).clamp(0.0, 1.0));
        let true_colour = ((stored - matte * (1.0 - a)) / a).clamp(0.0, 1.0);
        color::srgb_to_linear(true_colour) * a
    };
    [channel(px[0]), channel(px[1]), channel(px[2]), a]
}

/// Layer ▸ Matting ▸ Remove Black / White Matte over the active pixel layer
/// (inside the selection, when there is one), one undo step.
pub(crate) fn matting(editor: &mut Editor, op: ui::menu::MattingOp) -> Result<String, String> {
    let matte = match op {
        ui::menu::MattingOp::RemoveBlackMatte => 0.0,
        ui::menu::MattingOp::RemoveWhiteMatte => 1.0,
    };
    let label = op.label();
    let mut edge = false;
    super::edit_active_pixels(editor, label, |buffer, _| {
        for px in buffer.pixels_mut() {
            if px[3] > 0.0 && px[3] < 1.0 {
                edge = true;
                *px = remove_matte(*px, matte);
            }
        }
        if edge {
            Ok(())
        } else {
            Err(format!(
                "{label}: the layer has no partly transparent pixel for a matte to be in"
            ))
        }
    })?;
    Ok(format!("{label} applied"))
}

/// The active layer, when it is a smart object: its id, the layer, and
/// the smart object's own record.
fn active_smart_object(editor: &Editor) -> Result<(LayerId, Layer), String> {
    let doc = editor.active().ok_or("No document is open")?;
    let id = doc
        .document
        .active_layer()
        .ok_or("Select a smart object layer first")?;
    let layer = doc
        .document
        .layers
        .get(id)
        .ok_or("Select a smart object layer first")?;
    match &layer.kind {
        LayerKind::SmartObject(_) => Ok((id, layer.clone())),
        other => Err(format!(
            "The active layer is a {}, not a smart object",
            editor_core::layer_class_name(other)
        )),
    }
}

/// Layer ▸ Smart Object ▸ New Smart Object via Copy: a copy of the active
/// smart object over its OWN asset — a fresh [`AssetId`] carrying a copy of
/// the source record — so replacing, relinking or editing one never
/// reaches the other (Duplicate Layer shares the source). Lands directly
/// above the original, selected, as one undo step; the pixels are the
/// original's content hashes, so nothing is re-stored.
pub(crate) fn new_via_copy(editor: &mut Editor) -> Result<String, String> {
    let (source, original) = active_smart_object(editor)?;
    let LayerKind::SmartObject(so) = &original.kind else {
        unreachable!("active_smart_object returns smart objects only");
    };
    let doc = editor.active_mut().ok_or("No document is open")?;
    let record = doc
        .document
        .assets()
        .iter()
        .find(|r| r.id == so.asset)
        .cloned()
        .ok_or("The smart object's asset is missing from the document")?;
    let asset = AssetId::new();
    let mut copy = original.clone();
    copy.id = LayerId::new();
    copy.name = format!("{} copy", original.name);
    let mut object = so.clone();
    object.asset = asset;
    copy.kind = LayerKind::SmartObject(object);
    let old_mask = copy.mask.as_mut().map(|m| {
        let old = m.id;
        m.id = MaskId::new();
        old
    });
    let filter_mask = rekey_filter_mask(&mut copy, &doc.document);
    let new_id = copy.id;
    let mut commands = vec![Command::create_layer(copy)];
    if let Some(edits) = filter_mask.filter(|e| !e.is_empty()) {
        commands.push(
            Command::paint_tiles(PixelTarget::FilterMask(new_id), edits)
                .map_err(|e| e.to_string())?,
        );
    }
    let edits: Vec<TileEdit> = doc
        .document
        .layer_tiles(source)
        .map(|m| m.iter().map(|(c, h)| TileEdit::set(c, h)).collect())
        .unwrap_or_default();
    if !edits.is_empty() {
        commands.push(
            Command::paint_tiles(PixelTarget::Layer(new_id), edits).map_err(|e| e.to_string())?,
        );
    }
    if let Some(old_mask) = old_mask {
        let edits: Vec<TileEdit> = doc
            .document
            .pixels
            .tiles(editor_core::PixelKey::Mask(old_mask))
            .map(|m| m.iter().map(|(c, h)| TileEdit::set(c, h)).collect())
            .unwrap_or_default();
        if !edits.is_empty() {
            commands.push(
                Command::paint_tiles(PixelTarget::Mask(new_id), edits)
                    .map_err(|e| e.to_string())?,
            );
        }
    }
    let index = doc
        .document
        .layers
        .index_in_parent(source)
        .ok_or("The layer is not in the tree")?;
    commands.push(Command::MoveLayer {
        layer_id: new_id,
        parent: doc.document.layers.parent_of(source),
        index,
    });
    // The asset row rides outside the command stream, as every smart-object
    // creation's does (append-only for the session, see
    // `Editor::convert_to_smart_object`).
    doc.document.set_asset_origin(AssetRecord {
        id: asset,
        origin: record.origin,
        source_size: record.source_size,
    });
    editor.apply_command(Command::Transaction {
        label: "New Smart Object via Copy".to_string(),
        commands,
    });
    editor.set_active_layer(new_id);
    Ok(format!("{} copy is its own smart object", original.name))
}

/// W10-I: give `copy` — a clone of a layer about to be created under a new
/// id — its own smart-filter mask identity, returning the old mask's tiles
/// to paint under the new one (`PixelTarget::FilterMask(<new id>)`), so the
/// copy's filter mask is its own: painting one never reaches the other.
/// `None` when the layer carries no filter mask.
pub(crate) fn rekey_filter_mask(
    copy: &mut Layer,
    doc: &editor_core::Document,
) -> Option<Vec<TileEdit>> {
    let LayerKind::SmartObject(so) = &mut copy.kind else {
        return None;
    };
    let mask = so.filter_mask.as_mut()?;
    let old = mask.id;
    mask.id = MaskId::new();
    Some(
        doc.pixels
            .tiles(editor_core::PixelKey::Mask(old))
            .map(|m| m.iter().map(|(c, h)| TileEdit::set(c, h)).collect())
            .unwrap_or_default(),
    )
}

/// Layer ▸ Smart Filter ▸ Add Filter Mask: the active smart object's
/// filters get a white mask (every filter shows everywhere) covering every
/// tile of the object's source, and painting is aimed at it — one undo step.
fn add_filter_mask(editor: &mut Editor) -> Result<String, String> {
    let (id, layer) = active_smart_object(editor)?;
    let LayerKind::SmartObject(so) = &layer.kind else {
        unreachable!("active_smart_object returns smart objects only");
    };
    if so.filters.is_empty() {
        return Err(ui::menu::NO_SMART_FILTERS.to_string());
    }
    if so.filter_mask.is_some() {
        return Err(ui::menu::FILTER_MASK_EXISTS.to_string());
    }
    let doc = editor.active_mut().ok_or("No document is open")?;
    let mut next = so.clone();
    next.filter_mask = Some(layer_model::LayerMask::new(MaskId::new()));
    let white = doc
        .tiles
        .insert_bytes(vec![255u8; editor_core::MASK_TILE_BYTES]);
    let edits: Vec<TileEdit> = doc
        .document
        .layer_tiles(id)
        .map(|m| m.iter().map(|(c, _)| TileEdit::set(c, white)).collect())
        .unwrap_or_default();
    let mut commands = vec![Command::SetLayerKind {
        layer_id: id,
        kind: Box::new(LayerKind::SmartObject(next)),
    }];
    if !edits.is_empty() {
        commands.push(
            Command::paint_tiles(PixelTarget::FilterMask(id), edits).map_err(|e| e.to_string())?,
        );
    }
    editor.apply_command(Command::Transaction {
        label: "Add Filter Mask".to_string(),
        commands,
    });
    editor.set_edit_target_kind(crate::edit_target::EditTargetKind::FilterMask);
    Ok(format!("{}: filter mask added", layer.name))
}

/// Replace the active smart object's filter mask with `f`'s answer, one
/// undo step labelled `label`.
fn edit_filter_mask(
    editor: &mut Editor,
    label: &str,
    f: impl FnOnce(layer_model::LayerMask) -> Option<layer_model::LayerMask>,
) -> Result<(LayerId, Option<layer_model::LayerMask>), String> {
    let (id, layer) = active_smart_object(editor)?;
    let LayerKind::SmartObject(so) = &layer.kind else {
        unreachable!("active_smart_object returns smart objects only");
    };
    let mask = so
        .filter_mask
        .clone()
        .ok_or_else(|| ui::menu::NO_FILTER_MASK.to_string())?;
    let mut next = so.clone();
    next.filter_mask = f(mask);
    let result = next.filter_mask.clone();
    editor.apply_command(Command::Transaction {
        label: label.to_string(),
        commands: vec![Command::SetLayerKind {
            layer_id: id,
            kind: Box::new(LayerKind::SmartObject(next)),
        }],
    });
    Ok((id, result))
}

/// Layer ▸ Smart Filter (and the Layers panel's filter-mask row).
pub(crate) fn smart_filter(
    editor: &mut Editor,
    op: ui::menu::SmartFilterOp,
) -> Result<String, String> {
    use ui::menu::SmartFilterOp as Op;
    match op {
        Op::AddMask => add_filter_mask(editor),
        Op::EditMask => {
            let (_, layer) = active_smart_object(editor)?;
            if crate::edit_target::filter_mask_of(&layer).is_none() {
                return Err(ui::menu::NO_FILTER_MASK.to_string());
            }
            editor.set_edit_target_kind(crate::edit_target::EditTargetKind::FilterMask);
            Ok(format!("Painting {}'s filter mask", layer.name))
        }
        Op::ToggleMask => {
            let (_, mask) = edit_filter_mask(editor, "Disable / Enable Filter Mask", |mut m| {
                m.enabled = !m.enabled;
                Some(m)
            })?;
            Ok(if mask.is_some_and(|m| m.enabled) {
                "Filter mask enabled".to_string()
            } else {
                "Filter mask disabled: the smart filters show everywhere".to_string()
            })
        }
        Op::DeleteMask => {
            edit_filter_mask(editor, "Delete Filter Mask", |_| None)?;
            editor.set_edit_target_kind(crate::edit_target::EditTargetKind::Content);
            Ok("Filter mask deleted".to_string())
        }
    }
}

/// W10-I: the filter mask's coverage over the object's source extent (its
/// layer space, level 0) as an opaque grayscale thumbnail at most
/// `max_edge` pixels on a side — black hidden, white revealed, after the
/// mask's invert — or `None` for a layer without a filter mask. Nearest
/// sampling: a thumbnail, not a resample.
pub(crate) fn filter_mask_thumbnail(
    open: &crate::doc::OpenDocument,
    id: LayerId,
    max_edge: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    let layer = open.document.layers.get(id)?;
    let mask = crate::edit_target::filter_mask_of(layer)?;
    let tile = raster::TILE_SIZE as i64;
    let level0 = |map: Option<&editor_core::TileMap>| -> Vec<raster::TileCoord> {
        map.map(|m| m.iter().map(|(c, _)| c).filter(|c| c.level == 0).collect())
            .unwrap_or_default()
    };
    let mask_map = open
        .document
        .pixels
        .tiles(editor_core::PixelKey::Mask(mask.id));
    let mut coords = level0(open.document.layer_tiles(id));
    if coords.is_empty() {
        coords = level0(mask_map);
    }
    let (x0, y0, x1, y1) = coords.iter().fold(
        (i64::MAX, i64::MAX, i64::MIN, i64::MIN),
        |(a, b, c, d), t| {
            let (x, y) = (i64::from(t.x) * tile, i64::from(t.y) * tile);
            (a.min(x), b.min(y), c.max(x + tile), d.max(y + tile))
        },
    );
    if x0 >= x1 || y0 >= y1 {
        return Some((1, 1, vec![0, 0, 0, 255]));
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let edge = i64::from(max_edge.max(1));
    let (tw, th) = if w.max(h) <= edge {
        (w, h)
    } else if w >= h {
        (edge, (h * edge / w).max(1))
    } else {
        ((w * edge / h).max(1), edge)
    };
    let tiles: std::collections::HashMap<(i32, i32), &[u8]> = mask_map
        .map(|m| {
            m.iter()
                .filter(|(c, _)| c.level == 0)
                .filter_map(|(c, hash)| {
                    let bytes = TileSource::tile(&open.tiles, hash)?;
                    (bytes.len() >= editor_core::MASK_TILE_BYTES).then_some(((c.x, c.y), bytes))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut rgba = Vec::with_capacity((tw * th * 4) as usize);
    for ty in 0..th {
        for tx in 0..tw {
            let x = x0 + (tx * w + w / 2) / tw.max(1);
            let y = y0 + (ty * h + h / 2) / th.max(1);
            let key = (x.div_euclid(tile) as i32, y.div_euclid(tile) as i32);
            let stored = tiles.get(&key).map_or(0u8, |bytes| {
                let (lx, ly) = (x.rem_euclid(tile), y.rem_euclid(tile));
                bytes[(ly * tile + lx) as usize]
            });
            let v = if mask.inverted { 255 - stored } else { stored };
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    Some((tw as u32, th as u32, rgba))
}

/// W10-I: keep `slot`'s filter-mask thumbnail for layer `id` current: built
/// when the layer has a filter mask whose tiles or flags moved since the
/// stored one (the fingerprint), dropped when it has none.
pub(crate) fn refresh_filter_mask_thumb(
    ctx: &egui::Context,
    slot: &mut std::collections::HashMap<LayerId, (u64, egui::TextureHandle)>,
    open: &crate::doc::OpenDocument,
    id: LayerId,
    max_edge: u32,
) {
    use std::hash::{Hash, Hasher};
    let Some(mask) = open
        .document
        .layers
        .get(id)
        .and_then(crate::edit_target::filter_mask_of)
    else {
        slot.remove(&id);
        return;
    };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    mask.id.hash(&mut h);
    mask.inverted.hash(&mut h);
    max_edge.hash(&mut h);
    for key in [
        editor_core::PixelKey::Mask(mask.id),
        editor_core::PixelKey::Layer(id),
    ] {
        if let Some(map) = open.document.pixels.tiles(key) {
            for (c, hash) in map.iter() {
                (c.x, c.y, c.level, hash.0).hash(&mut h);
            }
        }
        0xffu8.hash(&mut h);
    }
    let fingerprint = h.finish();
    if slot.get(&id).is_some_and(|(fp, _)| *fp == fingerprint) {
        return;
    }
    let Some((w, h, rgba)) = filter_mask_thumbnail(open, id, max_edge) else {
        return;
    };
    let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
    match slot.get_mut(&id) {
        Some((fp, tex)) => {
            tex.set(img, egui::TextureOptions::NEAREST);
            *fp = fingerprint;
        }
        None => {
            let tex = ctx.load_texture(
                format!("filter-mask-thumb-{id}"),
                img,
                egui::TextureOptions::NEAREST,
            );
            slot.insert(id, (fingerprint, tex));
        }
    }
}

/// What Convert to Layers unpacks: layers (top-most first) of a document
/// and the tile store their pixels live in.
struct Unpacked {
    document: editor_core::Document,
    tiles: compositor::MemoryTileSource,
}

/// The embedded document a PSD source carries, when it is one this build
/// reads, and the target is an 8-bit document (a PSD's layers import at 8
/// bits).
fn embedded_psd(origin: &AssetOrigin, sixteen_bit: bool) -> Option<Unpacked> {
    if sixteen_bit {
        return None;
    }
    let bytes = match origin {
        AssetOrigin::Embedded { bytes, .. } if bytes.starts_with(b"8BPS") => bytes.clone(),
        AssetOrigin::Linked { path } => {
            let bytes = std::fs::read(path).ok()?;
            bytes.starts_with(b"8BPS").then_some(bytes)?
        }
        _ => return None,
    };
    let import = crate::import::document_from_psd(&bytes, "Contents", 1).ok()?;
    Some(Unpacked {
        document: import.imported.document,
        tiles: import.imported.tiles,
    })
}

/// Layer ▸ Smart Object ▸ Convert to Layers: the smart object is replaced,
/// in its place in the stack, by a group of ordinary layers holding its
/// contents — the embedded document's own layers when the source is a
/// layered PSD (each keeping its pixels, mask, blend and style, posed by
/// the object's transform), otherwise one pixel layer carrying the object's
/// source pixels at the object's transform. The group takes the object's
/// name, visibility, opacity, blend mode, style, clipping and mask. Smart
/// filters are not carried (they were a render of the object, not layers),
/// and the status says so. One undo step.
pub(crate) fn convert_to_layers(editor: &mut Editor) -> Result<String, String> {
    let (source, original) = active_smart_object(editor)?;
    let LayerKind::SmartObject(so) = &original.kind else {
        unreachable!("active_smart_object returns smart objects only");
    };
    let doc = editor.active_mut().ok_or("No document is open")?;
    let origin = doc.document.asset_origin(so.asset).cloned();
    let unpacked = origin
        .as_ref()
        .and_then(|o| embedded_psd(o, doc.is_sixteen_bit()));
    let pose = original.transform;

    let mut group = Layer::group(original.name.clone());
    group.visible = original.visible;
    group.opacity = original.opacity;
    group.fill_opacity = original.fill_opacity;
    group.blend_mode = original.blend_mode;
    group.clipping = original.clipping;
    group.effects = original.effects.clone();
    let old_group_mask = original.mask.as_ref().map(|m| m.id);
    group.mask = original.mask.clone().map(|mut m| {
        m.id = MaskId::new();
        m
    });
    let group_id = group.id;
    let mut commands = vec![Command::create_layer(group)];
    let mut tile_paints: Vec<(PixelTarget, Vec<TileEdit>)> = Vec::new();
    if let Some(old) = old_group_mask {
        let edits: Vec<TileEdit> = doc
            .document
            .pixels
            .tiles(editor_core::PixelKey::Mask(old))
            .map(|m| m.iter().map(|(c, h)| TileEdit::set(c, h)).collect())
            .unwrap_or_default();
        tile_paints.push((PixelTarget::Mask(group_id), edits));
    }

    let mut unpacked_count = 0usize;
    match unpacked {
        Some(Unpacked { document, tiles }) => {
            // Fresh ids for every copied layer and mask; groups arrive empty
            // and are filled by moves, as every multi-layer insert here is.
            let all = document.layers.iter_depth_first();
            let fresh: std::collections::HashMap<LayerId, LayerId> =
                all.iter().map(|id| (*id, LayerId::new())).collect();
            let mut moves = Vec::new();
            for old in &all {
                let Some(layer) = document.layers.get(*old) else {
                    continue;
                };
                let new_id = fresh[old];
                let mut copy = layer.clone();
                copy.id = new_id;
                if let LayerKind::Group(g) = &mut copy.kind {
                    for (index, child) in g.children.iter().enumerate() {
                        if let Some(child) = fresh.get(child) {
                            moves.push(Command::MoveLayer {
                                layer_id: *child,
                                parent: Some(new_id),
                                index,
                            });
                        }
                    }
                    g.children.clear();
                } else {
                    copy.transform = pose * copy.transform;
                }
                if let LayerKind::SmartObject(nested) = &copy.kind {
                    if let Some(record) = document.assets().iter().find(|r| r.id == nested.asset) {
                        if doc.document.asset_origin(record.id).is_none() {
                            doc.document.set_asset_origin(record.clone());
                        }
                    }
                }
                let old_mask = copy.mask.as_mut().map(|m| {
                    let old = m.id;
                    m.id = MaskId::new();
                    old
                });
                let mut restore = |map: Option<&editor_core::TileMap>| -> Vec<TileEdit> {
                    map.map(|m| {
                        m.iter()
                            .filter_map(|(coord, hash)| {
                                let bytes = TileSource::tile(&tiles, hash)?.to_vec();
                                Some(TileEdit::set(coord, doc.tiles.insert_bytes(bytes)))
                            })
                            .collect()
                    })
                    .unwrap_or_default()
                };
                tile_paints.push((
                    PixelTarget::Layer(new_id),
                    restore(document.layer_tiles(*old)),
                ));
                if let Some(old_mask) = old_mask {
                    tile_paints.push((
                        PixelTarget::Mask(new_id),
                        restore(document.pixels.tiles(editor_core::PixelKey::Mask(old_mask))),
                    ));
                }
                commands.push(Command::create_layer(copy));
                unpacked_count += 1;
            }
            commands.extend(moves);
            for (index, root) in document.layers.root().iter().enumerate() {
                commands.push(Command::MoveLayer {
                    layer_id: fresh[root],
                    parent: Some(group_id),
                    index,
                });
            }
        }
        None => {
            let name = match &origin {
                Some(AssetOrigin::Embedded { name, .. }) if !name.is_empty() => name.clone(),
                Some(AssetOrigin::Linked { path }) => path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| original.name.clone()),
                _ => original.name.clone(),
            };
            let mut layer = Layer::raster(name);
            layer.transform = pose;
            let id = layer.id;
            commands.push(Command::create_layer(layer));
            let edits: Vec<TileEdit> = doc
                .document
                .layer_tiles(source)
                .map(|m| m.iter().map(|(c, h)| TileEdit::set(c, h)).collect())
                .unwrap_or_default();
            tile_paints.push((PixelTarget::Layer(id), edits));
            commands.push(Command::MoveLayer {
                layer_id: id,
                parent: Some(group_id),
                index: 0,
            });
            unpacked_count = 1;
        }
    }
    for (target, edits) in tile_paints {
        if !edits.is_empty() {
            commands.push(Command::paint_tiles(target, edits).map_err(|e| e.to_string())?);
        }
    }
    let index = doc
        .document
        .layers
        .index_in_parent(source)
        .ok_or("The layer is not in the tree")?;
    commands.push(Command::MoveLayer {
        layer_id: group_id,
        parent: doc.document.layers.parent_of(source),
        index,
    });
    commands.push(Command::DeleteLayer { layer_id: source });
    let dropped = so.filters.len();
    editor.apply_command(Command::Transaction {
        label: "Convert to Layers".to_string(),
        commands,
    });
    editor.set_active_layer(group_id);
    let mut status = format!(
        "Converted {} to a group of {unpacked_count} layer{}",
        original.name,
        if unpacked_count == 1 { "" } else { "s" }
    );
    if dropped > 0 {
        status.push_str(&format!(
            "; its {dropped} smart filter{} did not carry over",
            if dropped == 1 { "" } else { "s" }
        ));
    }
    Ok(status)
}

/// Layer ▸ Smart Object ▸ the W10-I rows.
pub(crate) fn smart_object(
    editor: &mut Editor,
    op: ui::menu::SmartObjectOp,
) -> Result<String, String> {
    match op {
        ui::menu::SmartObjectOp::NewViaCopy => new_via_copy(editor),
        ui::menu::SmartObjectOp::ConvertToLayers => convert_to_layers(editor),
        ui::menu::SmartObjectOp::ExportContents => editor.export_smart_object_contents(),
        ui::menu::SmartObjectOp::RelinkToFile => editor.relink_smart_object(),
    }
}

#[cfg(test)]
mod tests {
    //! Every row is driven the way the menu bar drives it: the shell's own
    //! menu context resolves it, `Chrome::menu_click` (the handler the bar
    //! calls on a click) routes it, and the pick it hands back goes through
    //! `menu_bridge::perform`.

    use std::path::{Path, PathBuf};

    use editor_core::Command;
    use layer_model::{AssetOrigin, LayerKind};
    use ui::menu::{MattingOp, MenuAction, SmartObjectOp};

    use super::super::{context, menus, perform, pixels, resolve_intent};
    use crate::chrome::{Chrome, ChromeOutput};
    use crate::dialogs::ScriptedDialogs;
    use crate::editor::Editor;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;

    fn editor_with(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(dialogs),
        );
        ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        ed
    }

    /// A `w`x`h` PNG whose colour depends on `seed`.
    fn png(dir: &Path, name: &str, w: u32, h: u32, seed: u8) -> PathBuf {
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            px.copy_from_slice(&[seed, (i % 251) as u8, seed.wrapping_mul(3), 255]);
        }
        let bytes = raster::encode(raster::ExportFormat::Png, w, h, &rgba).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// The row is on the menu bar, enabled, and a click on it performs.
    fn click(ed: &mut Editor, action: MenuAction) -> Result<String, String> {
        assert!(
            menus(ed)
                .iter()
                .flat_map(|m| m.actions())
                .any(|a| a == action),
            "{action:?} is not on the menu bar"
        );
        let mut chrome = Chrome::new();
        let ctx = context(ed, chrome.workspace());
        let intent = resolve_intent(action, &ctx, ed)
            .unwrap_or_else(|reason| panic!("{action:?} is greyed: {reason}"));
        let mut out = ChromeOutput::default();
        chrome.menu_click(intent, ed, &mut out);
        let picks = std::mem::take(&mut out.menu);
        assert_eq!(picks, vec![action], "the click routes to perform");
        perform(action, ed)
    }

    fn greyed(ed: &mut Editor, action: MenuAction) -> String {
        let chrome = Chrome::new();
        let ctx = context(ed, chrome.workspace());
        resolve_intent(action, &ctx, ed).expect_err("greyed")
    }

    fn composite(ed: &mut Editor) -> Vec<u8> {
        let doc = ed.active_mut().unwrap();
        let rect = doc.canvas_rect();
        doc.composite(rect).unwrap()
    }

    fn undo(ed: &mut Editor) {
        assert!(ed.active_mut().unwrap().undo().unwrap());
    }

    fn depth(ed: &Editor) -> usize {
        ed.active().unwrap().history.undo_depth()
    }

    /// An opened 40x30 image plus a second raster layer on top, active.
    fn two_layers(
        dir: &Path,
        dialogs: ScriptedDialogs,
    ) -> (Editor, layer_model::LayerId, layer_model::LayerId) {
        let mut ed = editor_with(dir, dialogs);
        ed.open_path(&png(dir, "base.png", 40, 30, 90)).unwrap();
        let base = ed.active().unwrap().document.active_layer().unwrap();
        let top = layer_model::Layer::raster("Top");
        let top_id = top.id;
        ed.apply_command(Command::create_layer(top));
        ed.set_active_layer(top_id);
        (ed, base, top_id)
    }

    fn layer(ed: &Editor, id: layer_model::LayerId) -> &layer_model::Layer {
        ed.active().unwrap().document.layers.get(id).unwrap()
    }

    #[test]
    fn hide_layers_hides_every_selected_layer_in_one_step_and_ctrl_comma_stays_the_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, base, top) = two_layers(dir.path(), ScriptedDialogs::new());
        // Show is greyed while the one layer is showing, and says why.
        assert_eq!(
            greyed(&mut ed, MenuAction::ShowLayers),
            "The layer is already showing"
        );
        ed.active_mut()
            .unwrap()
            .document
            .set_layer_selection(vec![base, top])
            .unwrap();
        let before = depth(&ed);
        click(&mut ed, MenuAction::HideLayers).unwrap();
        assert!(!layer(&ed, base).visible && !layer(&ed, top).visible);
        assert_eq!(depth(&ed), before + 1, "one undo step for both");
        click(&mut ed, MenuAction::ShowLayers).unwrap();
        assert!(layer(&ed, base).visible && layer(&ed, top).visible);
        undo(&mut ed);
        assert!(!layer(&ed, base).visible && !layer(&ed, top).visible);
        undo(&mut ed);
        assert!(layer(&ed, base).visible && layer(&ed, top).visible);

        // Photopea's Ctrl+, is already this build's show / hide toggle of
        // the active layer (an application chord), so the row paints no
        // second claim on it.
        assert_eq!(MenuAction::HideLayers.shortcut(), None);
        let chord =
            crate::keymap::chord_of_shortcut(ui::Shortcut::ctrl_key(ui::Key::Comma)).unwrap();
        assert_eq!(
            crate::keymap::Keymap::default().resolve_any(&chord),
            Some(crate::keymap::Resolved::App(
                crate::action::Action::ToggleLayerVisibility
            ))
        );
    }

    #[test]
    fn link_layers_links_the_selection_and_a_second_click_unlinks_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, base, top) = two_layers(dir.path(), ScriptedDialogs::new());
        assert_eq!(
            greyed(&mut ed, MenuAction::LinkLayers),
            "Select two or more layers to link"
        );
        ed.active_mut()
            .unwrap()
            .document
            .set_layer_selection(vec![base, top])
            .unwrap();
        click(&mut ed, MenuAction::LinkLayers).unwrap();
        assert!(layer(&ed, base).linked && layer(&ed, top).linked);
        click(&mut ed, MenuAction::LinkLayers).unwrap();
        assert!(!layer(&ed, base).linked && !layer(&ed, top).linked);
        undo(&mut ed);
        assert!(layer(&ed, base).linked && layer(&ed, top).linked);
    }

    /// A pixel whose true colour `t` was mixed with a matte at alpha `a` is
    /// stored as `t·a + m·(1 − a)`; Remove Black / White Matte gives back `t`
    /// (to 8-bit rounding), leaves opaque and empty pixels alone, and is one
    /// undo step.
    #[test]
    fn remove_black_and_white_matte_take_the_matte_back_out() {
        let t = [200u8, 120, 40];
        let a = 128u8;
        let af = f32::from(a) / 255.0;
        for (op, m) in [
            (MattingOp::RemoveBlackMatte, 0.0),
            (MattingOp::RemoveWhiteMatte, 255.0),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (mut ed, _, top) = two_layers(dir.path(), ScriptedDialogs::new());
            let (w, h) = (40usize, 30usize);
            let mut rgba = vec![0u8; w * h * 4];
            let stored: Vec<u8> = t
                .iter()
                .map(|c| (f32::from(*c) * af + m * (1.0 - af)).round() as u8)
                .collect();
            // (0,0): the matted edge pixel; (1,0): opaque; (2,0): empty.
            rgba[0..4].copy_from_slice(&[stored[0], stored[1], stored[2], a]);
            rgba[4..8].copy_from_slice(&[10, 20, 30, 255]);
            let paint = pixels::write_layer(ed.active_mut().unwrap(), top, &rgba, "Edge").unwrap();
            ed.apply_command(paint);
            let before = depth(&ed);
            click(&mut ed, MenuAction::Matting(op)).unwrap();
            assert_eq!(depth(&ed), before + 1);
            let after = pixels::read_layer(ed.active().unwrap(), top);
            for c in 0..3 {
                let got = i32::from(after[c]);
                assert!(
                    (got - i32::from(t[c])).abs() <= 3,
                    "{op:?}: channel {c} is {got}, the true colour is {} (stored {})",
                    t[c],
                    stored[c]
                );
            }
            assert_eq!(after[3], a, "the alpha is kept");
            assert_eq!(
                &after[4..8],
                &[10, 20, 30, 255],
                "an opaque pixel is untouched"
            );
            assert_eq!(after[11], 0, "an empty pixel stays empty");
            undo(&mut ed);
            assert_eq!(pixels::read_layer(ed.active().unwrap(), top), rgba);
        }
        // A layer with no edge pixel refuses, saying why.
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, base, _) = two_layers(dir.path(), ScriptedDialogs::new());
        ed.set_active_layer(base);
        let reason = click(&mut ed, MenuAction::Matting(MattingOp::RemoveBlackMatte)).unwrap_err();
        assert!(reason.contains("partly transparent"), "{reason}");
    }

    /// Place `file` embedded (or linked) as the active smart object.
    fn placed(
        dir: &Path,
        dialogs: ScriptedDialogs,
        linked: bool,
    ) -> (Editor, layer_model::LayerId) {
        let (mut ed, _, _) = two_layers(dir, dialogs);
        let action = if linked {
            MenuAction::PlaceLinked
        } else {
            MenuAction::PlaceEmbedded
        };
        perform(action, &mut ed).unwrap();
        let id = ed.active().unwrap().document.active_layer().unwrap();
        assert!(matches!(layer(&ed, id).kind, LayerKind::SmartObject(_)));
        (ed, id)
    }

    fn asset_of(ed: &Editor, id: layer_model::LayerId) -> (layer_model::AssetId, AssetOrigin) {
        let LayerKind::SmartObject(so) = &layer(ed, id).kind else {
            panic!("not a smart object");
        };
        let origin = ed
            .active()
            .unwrap()
            .document
            .asset_origin(so.asset)
            .cloned()
            .unwrap();
        (so.asset, origin)
    }

    /// New Smart Object via Copy makes an object with its OWN source:
    /// replacing the copy's contents leaves the original's source and
    /// pixels exactly as they were (Duplicate Layer would share them).
    #[test]
    fn new_smart_object_via_copy_does_not_share_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let first = png(dir.path(), "first.png", 16, 12, 30);
        let second = png(dir.path(), "second.png", 16, 12, 220);
        let (mut ed, original) = placed(
            dir.path(),
            ScriptedDialogs::new()
                .placing(&first)
                .replacing_with(&second),
            false,
        );
        let before = depth(&ed);
        click(&mut ed, MenuAction::SmartObject(SmartObjectOp::NewViaCopy)).unwrap();
        assert_eq!(depth(&ed), before + 1);
        let copy = ed.active().unwrap().document.active_layer().unwrap();
        assert_ne!(copy, original);
        assert_eq!(
            layer(&ed, copy).name,
            format!("{} copy", layer(&ed, original).name)
        );
        let (a0, o0) = asset_of(&ed, original);
        let (a1, o1) = asset_of(&ed, copy);
        assert_ne!(a0, a1, "the copy has its own asset");
        assert_eq!(o0, o1, "holding the same source bytes");
        // Directly above the original.
        let doc = &ed.active().unwrap().document;
        assert_eq!(
            doc.layers.index_in_parent(copy).unwrap() + 1,
            doc.layers.index_in_parent(original).unwrap()
        );
        let original_tiles = doc.layer_tiles(original).cloned();
        assert_eq!(
            doc.layer_tiles(copy).cloned(),
            original_tiles,
            "same pixels"
        );

        // Replace the copy's contents: the original does not follow.
        perform(MenuAction::ReplaceContents, &mut ed).unwrap();
        assert_eq!(
            asset_of(&ed, original).1,
            o0,
            "the original's source is untouched"
        );
        assert_eq!(
            ed.active().unwrap().document.layer_tiles(original).cloned(),
            original_tiles,
            "the original's pixels are untouched"
        );
        assert_ne!(
            ed.active().unwrap().document.layer_tiles(copy).cloned(),
            original_tiles,
            "the copy took the new source"
        );
    }

    /// Export Contents writes the embedded source bytes, byte for byte, to
    /// the picked file, suggesting the extension the bytes name; a
    /// cancelled picker writes nothing and says so.
    #[test]
    fn export_contents_writes_the_embedded_source_to_the_picked_file() {
        let dir = tempfile::tempdir().unwrap();
        let first = png(dir.path(), "first.png", 16, 12, 30);
        let target = dir.path().join("out.png");
        let (mut ed, id) = placed(
            dir.path(),
            ScriptedDialogs::new().placing(&first).saving_to(&target),
            false,
        );
        let AssetOrigin::Embedded { bytes, .. } = asset_of(&ed, id).1 else {
            panic!("embedded");
        };
        click(
            &mut ed,
            MenuAction::SmartObject(SmartObjectOp::ExportContents),
        )
        .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        assert_eq!(crate::editor::source_extension(&bytes), "png");
        let reason = click(
            &mut ed,
            MenuAction::SmartObject(SmartObjectOp::ExportContents),
        )
        .unwrap_err();
        assert!(reason.contains("cancelled"), "{reason}");
    }

    /// Relink to File points a linked object at another file (it stays
    /// linked, the pixels are re-read, one undo step); over an embedded
    /// object the row is greyed on the menu bar, with the reason, and the
    /// action itself still refuses.
    #[test]
    fn relink_to_file_repoints_a_linked_object_and_refuses_an_embedded_one() {
        let dir = tempfile::tempdir().unwrap();
        let first = png(dir.path(), "first.png", 16, 12, 30);
        let second = png(dir.path(), "second.png", 16, 12, 220);
        let (mut ed, id) = placed(
            dir.path(),
            ScriptedDialogs::new()
                .placing(&first)
                .replacing_with(&second),
            true,
        );
        assert_eq!(
            asset_of(&ed, id).1,
            AssetOrigin::Linked {
                path: first.clone()
            }
        );
        let tiles = ed.active().unwrap().document.layer_tiles(id).cloned();
        let before = depth(&ed);
        click(
            &mut ed,
            MenuAction::SmartObject(SmartObjectOp::RelinkToFile),
        )
        .unwrap();
        assert_eq!(
            asset_of(&ed, id).1,
            AssetOrigin::Linked {
                path: second.clone()
            }
        );
        assert_ne!(
            ed.active().unwrap().document.layer_tiles(id).cloned(),
            tiles
        );
        assert_eq!(depth(&ed), before + 1);
        undo(&mut ed);
        assert_eq!(asset_of(&ed, id).1, AssetOrigin::Linked { path: first });

        let dir = tempfile::tempdir().unwrap();
        let first = png(dir.path(), "first.png", 16, 12, 30);
        let (mut ed, _) = placed(dir.path(), ScriptedDialogs::new().placing(&first), false);
        assert_eq!(
            greyed(
                &mut ed,
                MenuAction::SmartObject(SmartObjectOp::RelinkToFile)
            ),
            ui::menu::RELINK_EMBEDDED
        );
        // The other Smart Object rows stay live over the embedded object.
        for op in [
            SmartObjectOp::NewViaCopy,
            SmartObjectOp::ExportContents,
            SmartObjectOp::ConvertToLayers,
        ] {
            let chrome = Chrome::new();
            let ctx = context(&mut ed, chrome.workspace());
            assert!(
                resolve_intent(MenuAction::SmartObject(op), &ctx, &ed).is_ok(),
                "{op:?} is greyed"
            );
        }
        let reason = perform(
            MenuAction::SmartObject(SmartObjectOp::RelinkToFile),
            &mut ed,
        )
        .unwrap_err();
        assert_eq!(reason, ui::menu::RELINK_EMBEDDED);
    }

    /// Convert to Layers replaces the object, in place, with a group holding
    /// one pixel layer of its source at its transform: the composite does
    /// not change, and one undo brings the object back.
    #[test]
    fn convert_to_layers_unpacks_a_placed_image_into_a_group_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let first = png(dir.path(), "first.png", 16, 12, 30);
        let (mut ed, id) = placed(dir.path(), ScriptedDialogs::new().placing(&first), false);
        let index = ed
            .active()
            .unwrap()
            .document
            .layers
            .index_in_parent(id)
            .unwrap();
        let before = composite(&mut ed);
        let steps = depth(&ed);
        click(
            &mut ed,
            MenuAction::SmartObject(SmartObjectOp::ConvertToLayers),
        )
        .unwrap();
        assert_eq!(depth(&ed), steps + 1);
        let doc = &ed.active().unwrap().document;
        assert!(!doc.layers.contains(id), "the object is gone");
        let group = doc.active_layer().unwrap();
        let g = doc.layers.get(group).unwrap();
        assert!(g.is_group());
        assert_eq!(
            doc.layers.index_in_parent(group),
            Some(index),
            "in the object's place"
        );
        assert_eq!(g.children().len(), 1);
        let child = doc.layers.get(g.children()[0]).unwrap();
        assert!(matches!(child.kind, LayerKind::Raster(_)));
        assert_eq!(composite(&mut ed), before, "the picture is unchanged");
        undo(&mut ed);
        assert!(ed.active().unwrap().document.layers.contains(id));
        assert_eq!(composite(&mut ed), before);
    }

    // ------------------------------------------------ the chrome, frame by frame

    fn frame_input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            ..Default::default()
        }
    }

    /// One chrome frame with `events`, its output applied in the order
    /// `Shell::apply_chrome` applies it: the layer selection, the edit
    /// target, commands through `apply_command`, menu picks through
    /// `perform`, kind edits through `apply_kind_edit` (the gesture fold).
    fn chrome_frame(
        ctx: &egui::Context,
        chrome: &mut Chrome,
        ed: &mut Editor,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut out = ChromeOutput::default();
        let full = ctx.run(frame_input(events), |ctx| {
            out = chrome.ui(ctx, ed);
        });
        if let Some((layers, active)) = out.select_layers {
            ed.set_layer_selection(layers, active);
        } else if let Some(id) = out.select_layer {
            ed.set_active_layer(id);
        }
        if let Some(kind) = out.edit_target {
            ed.set_edit_target_kind(kind);
        }
        for command in out.commands {
            ed.apply_command(command);
        }
        for action in out.menu {
            if let Err(reason) = perform(action, ed) {
                panic!("{action:?} refused: {reason}");
            }
        }
        for edit in out.layer_kind {
            ed.apply_kind_edit(edit);
        }
        full
    }

    /// A press and release on the chrome control `id`, applied.
    fn click_control(ctx: &egui::Context, chrome: &mut Chrome, ed: &mut Editor, id: egui::Id) {
        let at = ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("{id:?} was not drawn"))
            .rect
            .center();
        chrome_frame(
            ctx,
            chrome,
            ed,
            vec![
                egui::Event::PointerMoved(at),
                press(at, true),
                press(at, false),
            ],
        );
        for _ in 0..2 {
            chrome_frame(ctx, chrome, ed, Vec::new());
        }
    }

    /// The Layers panel's id for one part of a smart object's filter-mask
    /// row — the tuple `ui::view::docks::smart_filter_mask_id` hashes.
    fn filter_mask_part(layer: layer_model::LayerId, part: &str) -> egui::Id {
        egui::Id::new(("raster-smart-filter-mask", layer, part))
    }

    /// Every textured mesh of a frame whose vertices all lie in `rect`.
    fn images_in(full: &egui::FullOutput, rect: egui::Rect) -> Vec<egui::TextureId> {
        fn walk(shape: &egui::Shape, rect: egui::Rect, out: &mut Vec<egui::TextureId>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, rect, out)),
                egui::Shape::Mesh(m)
                    if m.texture_id != egui::TextureId::default()
                        && !m.vertices.is_empty()
                        && m.vertices.iter().all(|v| rect.expand(1.0).contains(v.pos)) =>
                {
                    out.push(m.texture_id)
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &full.shapes {
            walk(&clipped.shape, rect, &mut out);
        }
        out
    }

    fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// The Layers panel's id for one part of a smart filter's row — the same
    /// tuple `ui::view::docks::smart_filter_part_id` hashes.
    fn filter_part(layer: layer_model::LayerId, index: usize, part: &str) -> egui::Id {
        egui::Id::new(("raster-smart-filter", layer, index, part))
    }

    /// A placed canvas-sized (40x30) smart object carrying one Gaussian
    /// Blur smart filter.
    fn filtered_object(dir: &Path) -> (Editor, layer_model::LayerId) {
        let first = png(dir, "first.png", 40, 30, 30);
        let (mut ed, id) = placed(dir, ScriptedDialogs::new().placing(&first), false);
        let LayerKind::SmartObject(mut so) = layer(&ed, id).kind.clone() else {
            panic!("not a smart object");
        };
        let mut params = std::collections::BTreeMap::new();
        params.insert("radius".to_string(), layer_model::SmartParam::Float(2.0));
        so.filters = vec![layer_model::SmartFilter::new("GaussianBlur", params)];
        ed.apply_command(Command::SetLayerKind {
            layer_id: id,
            kind: Box::new(LayerKind::SmartObject(so)),
        });
        (ed, id)
    }

    fn filter_opacity(ed: &Editor, id: layer_model::LayerId) -> f32 {
        match &layer(ed, id).kind {
            LayerKind::SmartObject(so) => so.filters[0].opacity,
            other => panic!("not a smart object: {other:?}"),
        }
    }

    /// Round 2, defect 2: one drag of a smart filter's opacity slider, over
    /// many frames, through the real chrome and the shell's own output
    /// route, is ONE undo step — and one undo puts the opacity back.
    #[test]
    fn a_smart_filter_opacity_drag_is_one_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, id) = filtered_object(dir.path());
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        for _ in 0..6 {
            chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
        }
        // Open the filter's Blending Options row.
        let blend = ctx
            .read_response(filter_part(id, 0, "blend"))
            .expect("the Layers panel drew the filter's blending-options button")
            .rect
            .center();
        chrome_frame(
            &ctx,
            &mut chrome,
            &mut ed,
            vec![
                egui::Event::PointerMoved(blend),
                press(blend, true),
                press(blend, false),
            ],
        );
        for _ in 0..2 {
            chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
        }
        let slider = ctx
            .read_response(filter_part(id, 0, "opacity"))
            .expect("the opacity slider was drawn")
            .rect;
        let before = depth(&ed);
        assert_eq!(filter_opacity(&ed, id), 1.0);

        // Press near the left end, then sweep right over eight frames.
        let y = slider.center().y;
        let start = egui::pos2(slider.left() + 2.0, y);
        chrome_frame(
            &ctx,
            &mut chrome,
            &mut ed,
            vec![egui::Event::PointerMoved(start), press(start, true)],
        );
        let mut seen = vec![filter_opacity(&ed, id)];
        for step in 1..=8 {
            let at = egui::pos2(slider.left() + 2.0 + step as f32 * 6.0, y);
            chrome_frame(
                &ctx,
                &mut chrome,
                &mut ed,
                vec![egui::Event::PointerMoved(at)],
            );
            seen.push(filter_opacity(&ed, id));
        }
        let end = egui::pos2(slider.left() + 50.0, y);
        chrome_frame(&ctx, &mut chrome, &mut ed, vec![press(end, false)]);
        let settled = filter_opacity(&ed, id);
        seen.dedup();
        assert!(
            seen.len() >= 4,
            "the drag moved the opacity over several frames: {seen:?}"
        );
        assert!(settled < 1.0, "{settled}");
        assert_eq!(
            depth(&ed),
            before + 1,
            "one drag, one undo step (frames seen: {seen:?})"
        );
        undo(&mut ed);
        assert_eq!(filter_opacity(&ed, id), 1.0, "one undo restores it");
    }

    /// Round 2, defect 1: the smart filters' shared mask, end to end through
    /// the real routes. The Layers panel's filter-mask row offers Add Filter
    /// Mask; the click (chrome frame, then `perform`) gives the object a
    /// white mask, one undo step, aims painting at it and draws its
    /// thumbnail well framed as the target. A brush stroke through the real
    /// `ToolPointer` paints black into the MASK (the object's source tiles do
    /// not move), and the composite then shows the bare source there and the
    /// filtered result elsewhere; the thumbnail goes black there too. The
    /// row's eye switches the mask off (the filter shows everywhere again),
    /// its trash deletes it, and undo brings each back.
    #[test]
    fn the_filter_mask_row_adds_a_paintable_mask_that_limits_the_smart_filters() {
        use ui::canvas::{PointerInput, PointerPhase};
        super::super::install_smart_filter_runner();
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, id) = filtered_object(dir.path());
        let so = match &layer(&ed, id).kind {
            LayerKind::SmartObject(so) => so.clone(),
            _ => unreachable!(),
        };
        let stack = so.filters.clone();
        let bare = {
            let mut off = so.clone();
            off.filters.clear();
            ed.apply_command(Command::SetLayerKind {
                layer_id: id,
                kind: Box::new(LayerKind::SmartObject(off)),
            });
            let px = composite(&mut ed);
            undo(&mut ed);
            px
        };
        let filtered = composite(&mut ed);
        let at =
            |px: &[u8], x: usize, y: usize| px[(y * 40 + x) * 4..(y * 40 + x) * 4 + 4].to_vec();
        assert_ne!(
            at(&bare, 5, 15),
            at(&filtered, 5, 15),
            "the filter changes (5, 15)"
        );
        assert_ne!(at(&bare, 35, 15), at(&filtered, 35, 15), "and (35, 15)");

        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        for _ in 0..6 {
            chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
        }
        assert!(
            ctx.read_response(filter_mask_part(id, "well")).is_none(),
            "no well before there is a mask"
        );
        let steps = depth(&ed);
        click_control(&ctx, &mut chrome, &mut ed, filter_mask_part(id, "add"));
        assert_eq!(depth(&ed), steps + 1, "Add Filter Mask is one undo step");
        let mask = match &layer(&ed, id).kind {
            LayerKind::SmartObject(so) => so
                .filter_mask
                .clone()
                .expect("the object has a filter mask"),
            _ => unreachable!(),
        };
        assert_eq!(
            match &layer(&ed, id).kind {
                LayerKind::SmartObject(so) => so.filters.clone(),
                _ => unreachable!(),
            },
            stack,
            "the stack is untouched"
        );
        assert_eq!(
            ed.edit_target_filter_mask(),
            Some(mask.id),
            "painting aims at it"
        );
        assert_eq!(
            composite(&mut ed),
            filtered,
            "a white mask shows the filter everywhere"
        );
        // The well: drawn, framed as the target, showing the thumbnail the
        // application built from the mask's coverage.
        let full = chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
        let well = ctx
            .read_response(filter_mask_part(id, "well"))
            .expect("the filter-mask well is drawn")
            .rect;
        assert_eq!(chrome.workspace().filter_mask_target, Some(id));
        let thumb = chrome
            .workspace()
            .filter_mask_thumbs
            .get(&id)
            .map(|(_, t)| t.id())
            .expect("the application built the filter mask's thumbnail");
        assert!(
            images_in(&full, well).contains(&thumb),
            "the well paints the thumbnail"
        );

        // A black brush stroke down the left of the canvas, through the
        // real tool route.
        let source_tiles = ed.active().unwrap().document.layer_tiles(id).cloned();
        ed.set_tool(tools::ToolId::Brush);
        ed.set_foreground([0.0, 0.0, 0.0, 1.0]);
        let mut brush = *ed.brush();
        brush.size = 16.0;
        brush.hardness = 1.0;
        ed.set_brush(brush);
        let viewport = glam::Vec2::new(400.0, 300.0);
        {
            let doc = ed.active_mut().unwrap();
            doc.set_viewport(viewport);
            doc.camera.zoom = 1.0;
            doc.camera.center = glam::Vec2::new(20.0, 15.0);
        }
        let screen = |x: f32, y: f32| viewport * 0.5 + glam::Vec2::new(x - 20.0, y - 15.0);
        let mut pointer = crate::tool_input::ToolPointer::new();
        let points = [(5.0, -4.0), (5.0, 10.0), (5.0, 20.0), (5.0, 34.0)];
        for (i, (x, y)) in points.iter().enumerate() {
            let phase = if i == 0 {
                PointerPhase::Down
            } else {
                PointerPhase::Move
            };
            pointer.handle(&mut ed, PointerInput::at(phase, screen(*x, *y)), false, &[]);
        }
        pointer.handle(
            &mut ed,
            PointerInput::at(PointerPhase::Up, screen(5.0, 34.0)),
            false,
            &[],
        );
        assert_eq!(
            ed.active().unwrap().document.layer_tiles(id).cloned(),
            source_tiles,
            "the stroke painted the mask, not the object's source"
        );
        let painted = composite(&mut ed);
        assert_eq!(
            at(&painted, 5, 15),
            at(&bare, 5, 15),
            "under the stroke: the bare source"
        );
        assert_eq!(
            at(&painted, 35, 15),
            at(&filtered, 35, 15),
            "elsewhere: the filter"
        );
        let (tw, _, rgba) = super::filter_mask_thumbnail(ed.active().unwrap(), id, 256).unwrap();
        let thumb_px = |x: usize, y: usize| rgba[(y * tw as usize + x) * 4];
        assert_eq!(
            thumb_px(5, 15),
            0,
            "the thumbnail is black under the stroke"
        );
        assert_eq!(thumb_px(35, 15), 255, "and white elsewhere");
        // ...and the well's uploaded thumbnail is rebuilt from the new
        // coverage on the next chrome frame.
        let fingerprint = |chrome: &Chrome| chrome.workspace().filter_mask_thumbs[&id].0;
        let before_frame = fingerprint(&chrome);
        chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
        assert_ne!(
            fingerprint(&chrome),
            before_frame,
            "the stroke reached the well's thumbnail"
        );

        // The row's eye: off, the filter shows everywhere again; undo, back.
        for _ in 0..2 {
            chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
        }
        click_control(&ctx, &mut chrome, &mut ed, filter_mask_part(id, "enable"));
        assert_eq!(
            composite(&mut ed),
            filtered,
            "a disabled mask masks nothing"
        );
        undo(&mut ed);
        assert_eq!(composite(&mut ed), painted);

        // The trash: the mask goes, painting returns to the content.
        for _ in 0..2 {
            chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
        }
        click_control(&ctx, &mut chrome, &mut ed, filter_mask_part(id, "delete"));
        assert!(matches!(
            &layer(&ed, id).kind,
            LayerKind::SmartObject(so) if so.filter_mask.is_none()
        ));
        assert_eq!(ed.edit_target_filter_mask(), None);
        assert_eq!(composite(&mut ed), filtered);
        undo(&mut ed);
        assert_eq!(
            composite(&mut ed),
            painted,
            "undo brings the painted mask back"
        );
    }

    /// Duplicate Layer and New Smart Object via Copy give the copy its OWN
    /// filter mask: same coverage, a different mask id, so painting one
    /// never reaches the other.
    #[test]
    fn a_copied_smart_object_gets_its_own_filter_mask() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, id) = filtered_object(dir.path());
        click(
            &mut ed,
            MenuAction::SmartFilter(ui::menu::SmartFilterOp::AddMask),
        )
        .unwrap();
        let mask_of = |ed: &Editor, id: layer_model::LayerId| match &layer(ed, id).kind {
            LayerKind::SmartObject(so) => so.filter_mask.clone().unwrap(),
            _ => unreachable!(),
        };
        let original = mask_of(&ed, id);
        let tiles = |ed: &Editor, m: &layer_model::LayerMask| {
            ed.active()
                .unwrap()
                .document
                .pixels
                .tiles(editor_core::PixelKey::Mask(m.id))
                .cloned()
        };
        assert!(tiles(&ed, &original).is_some_and(|t| t.iter().count() > 0));
        for action in [
            MenuAction::SmartObject(SmartObjectOp::NewViaCopy),
            MenuAction::DuplicateLayer,
        ] {
            ed.set_active_layer(id);
            if action == MenuAction::DuplicateLayer {
                // The row asks for a name in a dialog; its confirm lands here.
                crate::layer_ops::duplicate_layer(&mut ed, None).unwrap();
            } else {
                click(&mut ed, action).unwrap();
            }
            let copy = ed.active().unwrap().document.active_layer().unwrap();
            assert_ne!(copy, id, "{action:?} made a copy");
            let mask = mask_of(&ed, copy);
            assert_ne!(mask.id, original.id, "{action:?}: the copy's own mask");
            assert_eq!(
                tiles(&ed, &mask),
                tiles(&ed, &original),
                "{action:?}: with the same coverage"
            );
        }
    }

    /// Image ▸ Image Size resamples a smart object's filter mask with the
    /// document, as it does a layer mask: halving the canvas halves the
    /// mask's white area.
    #[test]
    fn image_size_resamples_the_filter_mask_too() {
        use compositor::TileSource as _;
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, id) = filtered_object(dir.path());
        click(
            &mut ed,
            MenuAction::SmartFilter(ui::menu::SmartFilterOp::AddMask),
        )
        .unwrap();
        let byte = |ed: &Editor, x: usize, y: usize| -> u8 {
            let doc = ed.active().unwrap();
            let LayerKind::SmartObject(so) = &layer(ed, id).kind else {
                unreachable!()
            };
            let map = doc
                .document
                .pixels
                .tiles(editor_core::PixelKey::Mask(
                    so.filter_mask.as_ref().unwrap().id,
                ))
                .unwrap();
            let hash = map.get(raster::TileCoord::new(0, 0, 0)).unwrap();
            doc.tiles.tile(hash).unwrap()[y * raster::TILE_SIZE as usize + x]
        };
        assert_eq!(
            byte(&ed, 25, 25),
            255,
            "the new mask is white over the object"
        );
        let command = ed
            .active_mut()
            .unwrap()
            .resample_command(&ui::dialogs::ImageSizeSpec {
                width: 20,
                height: 15,
                resolution_ppi: 72.0,
                resample: Some(raster::ResampleFilter::Triangle),
            })
            .unwrap();
        ed.apply_command(command);
        assert_eq!(
            ed.active().unwrap().document.width(),
            20,
            "Image Size landed"
        );
        assert_eq!(
            byte(&ed, 5, 5),
            255,
            "inside the halved canvas: still white"
        );
        assert_eq!(byte(&ed, 25, 25), 0, "outside it: resampled away");
    }

    /// Layer ▸ Smart Filter rows are greyed with their reasons: Add needs a
    /// smart filter and no mask yet; the others need a mask.
    #[test]
    fn the_smart_filter_rows_grey_out_with_their_reasons() {
        use ui::menu::SmartFilterOp as Op;
        let dir = tempfile::tempdir().unwrap();
        let first = png(dir.path(), "first.png", 16, 12, 30);
        let (mut ed, _) = placed(dir.path(), ScriptedDialogs::new().placing(&first), false);
        assert_eq!(
            greyed(&mut ed, MenuAction::SmartFilter(Op::AddMask)),
            ui::menu::NO_SMART_FILTERS
        );
        for op in [Op::EditMask, Op::ToggleMask, Op::DeleteMask] {
            assert_eq!(
                greyed(&mut ed, MenuAction::SmartFilter(op)),
                ui::menu::NO_FILTER_MASK
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, _) = filtered_object(dir.path());
        click(&mut ed, MenuAction::SmartFilter(Op::AddMask)).unwrap();
        assert_eq!(
            greyed(&mut ed, MenuAction::SmartFilter(Op::AddMask)),
            ui::menu::FILTER_MASK_EXISTS
        );
        ed.set_edit_target_kind(crate::edit_target::EditTargetKind::Content);
        click(&mut ed, MenuAction::SmartFilter(Op::EditMask)).unwrap();
        assert!(ed.edit_target_filter_mask().is_some());
    }

    /// A smart object whose embedded file is a layered PSD (what PSD import
    /// makes of a `SoLd` layer; Place refuses a PSD) unpacks into the PSD's
    /// own layers, each posed by the object's transform: an object moved
    /// by (5, 3) unpacks into layers moved by (5, 3), so the picture stays
    /// where the object put it.
    #[test]
    fn convert_to_layers_unpacks_an_embedded_psds_layers() {
        let dir = tempfile::tempdir().unwrap();
        // A two-layer document written as a PSD.
        let (mut src, _, top) = two_layers(dir.path(), ScriptedDialogs::new());
        let rgba = vec![200u8; 40 * 30 * 4];
        let paint = pixels::write_layer(src.active_mut().unwrap(), top, &rgba, "Top").unwrap();
        src.apply_command(paint);
        let flat = composite(&mut src);
        let doc = src.active().unwrap();
        let (bytes, _) =
            crate::import::psd_from_document(&doc.document, &doc.tiles, &flat).unwrap();
        assert!(bytes.starts_with(b"8BPS"));

        // A placed object whose asset row then carries those PSD bytes.
        let dir2 = tempfile::tempdir().unwrap();
        let first = png(dir2.path(), "first.png", 40, 30, 30);
        let (mut ed, id) = placed(dir2.path(), ScriptedDialogs::new().placing(&first), false);
        let asset = asset_of(&ed, id).0;
        ed.active_mut()
            .unwrap()
            .document
            .set_asset_origin(layer_model::AssetRecord {
                id: asset,
                origin: AssetOrigin::Embedded {
                    name: "layered".to_string(),
                    bytes,
                },
                source_size: Some((40, 30)),
            });
        // Move the object by (5, 3): its pose is what the layers inherit.
        ed.apply_command(Command::TransformLayer {
            layer_id: id,
            matrix: tools::edit::translation_matrix(glam::Vec2::new(5.0, 3.0)),
        });
        let pose = layer(&ed, id).transform;
        assert_ne!(pose, glam::Affine2::IDENTITY, "the object was moved");
        click(
            &mut ed,
            MenuAction::SmartObject(SmartObjectOp::ConvertToLayers),
        )
        .unwrap();
        let doc = &ed.active().unwrap().document;
        assert!(!doc.layers.contains(id));
        let group = doc.layers.get(doc.active_layer().unwrap()).unwrap();
        assert!(group.is_group());
        assert_eq!(group.children().len(), 2, "both of the PSD's layers");
        let names: Vec<String> = group
            .children()
            .iter()
            .map(|c| doc.layers.get(*c).unwrap().name.clone())
            .collect();
        assert!(names.contains(&"Top".to_string()), "{names:?}");
        // The PSD's layers sit at identity in the PSD, so each unpacked layer
        // carries exactly the object's pose.
        for child in group.children() {
            let child = doc.layers.get(*child).unwrap();
            assert_eq!(child.transform, pose, "{} is posed", child.name);
        }
        // The composite: the PSD's own picture (`flat`) now starts at (5, 3);
        // left of it the document's own base image shows through.
        let px = composite(&mut ed);
        let at = |x: usize, y: usize| px[(y * 40 + x) * 4..(y * 40 + x) * 4 + 3].to_vec();
        let psd_at = |x: usize, y: usize| flat[(y * 40 + x) * 4..(y * 40 + x) * 4 + 3].to_vec();
        for (x, y) in [(5, 3), (20, 20), (39, 29)] {
            assert_eq!(
                at(x, y),
                psd_at(x - 5, y - 3),
                "({x}, {y}) is the PSD moved by (5, 3)"
            );
        }
        assert_eq!(
            at(2, 1),
            vec![90, 42, 14],
            "outside them: the base image, not the PSD's Top"
        );
    }
}
