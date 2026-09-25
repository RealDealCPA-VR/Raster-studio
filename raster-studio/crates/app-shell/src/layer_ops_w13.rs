//! W13-G: the last Layer-menu gaps from the confirming parity audit
//! ([`ui::menu::LayerExtraOp`]).
//!
//! * Layer Style ▸ Scale Effects… ([`scale_effects`], at the percent the
//!   dialog host's `ScaleEffects` dialog confirms, 1–1000%, with
//!   [`scale_effects_preview`] rendering its Preview) and Create Layers
//!   ([`create_layers`]).
//! * New ▸ Artboard from Layers ([`artboard_from_layers`]).
//! * Layer Mask ▸ From Transparency ([`mask_from_transparency`]).
//! * Smart Object ▸ Reset Transform ([`reset_transform`]) and Stack Mode
//!   ([`stack_mode`]).
//! * Animation ▸ Make Frames / Unmake Frames / Merge ([`make_frames`],
//!   [`merge_frames`]) over Photopea's `_a_` frame layers.
//!
//! Every document edit here is one [`Command::Transaction`], so one Ctrl+Z
//! takes it back. The menu model ([`ui::menu::LayerExtraFacts`]) greys each
//! row when it cannot apply; the refusals here repeat those reasons for a
//! caller that skips the menu.
//!
//! # Create Layers
//!
//! The compositor draws a style in three parts (`compositor::effects`): the
//! exterior passes (drop shadows, outer glow) blended into the backdrop under
//! the layer, the interior effects composited *atop* the layer's own pixels,
//! and the strokes drawn over that. Create Layers mirrors that split:
//!
//! * each exterior pass becomes a raster layer directly under the layer, in
//!   the pass's blend mode at the layer's opacity;
//! * each interior effect becomes a raster layer **clipped** to the layer,
//!   above any layers already clipped to it, in the effect's blend mode. A
//!   clipping group composites its members atop the base exactly as the
//!   interior effects are composited atop the layer, so the picture holds;
//! * each stroke becomes a raster layer above the clipping group.
//!
//! An effect's own ink (its colour and coverage, before its blend mode) is
//! recovered from the compositor itself, not re-derived: the exterior passes
//! and strokes are rendered alone (the layer at zero fill, only that effect,
//! in Normal); an interior effect is rendered in Normal atop an opaque black
//! and an opaque white copy of the layer's shape, and the two results give
//! the ink's coverage and colour exactly (`atop` in Normal is linear in the
//! colour underneath).
//!
//! What does not survive the split, as in Photoshop: a stroke whose own blend
//! mode is not Normal blends against the backdrop instead of the layer's body;
//! under a layer opacity below 100% an overlapping stroke and body are each
//! faded on their own instead of together; and under a fill below 100% an
//! interior effect in a mode other than Normal mixes against the layer's full
//! coverage (a clipped layer never sees the fill) instead of its faded one.
//! Effects near the canvas edge that read the layer's shape beyond it are
//! rebuilt from the canvas part of the shape only.

use compositor::{Canvas, CompositeOptions, MemoryTileSource, TileSource};
use editor_core::pixels::{PixelTarget, TileEdit};
use editor_core::{Command, Document, LayerPatch, Patch};
use layer_model::{
    AssetOrigin, BlendMode, ClippingMode, FillStyle, Layer, LayerEffects, LayerId, LayerKind,
};
use raster::PixelRect;
use ui::menu::{LayerExtraOp, StackMode};

use crate::doc::OpenDocument;
use crate::editor::Editor;

/// Perform one [`LayerExtraOp`] row.
pub(crate) fn perform(editor: &mut Editor, op: LayerExtraOp) -> Result<String, String> {
    match op {
        LayerExtraOp::ScaleEffects(percent) => scale_effects(editor, percent),
        LayerExtraOp::CreateLayers => create_layers(editor),
        LayerExtraOp::ArtboardFromLayers => artboard_from_layers(editor),
        LayerExtraOp::MaskFromTransparency => mask_from_transparency(editor),
        LayerExtraOp::ResetTransform => reset_transform(editor),
        LayerExtraOp::StackMode(mode) => stack_mode(editor, mode),
        LayerExtraOp::MakeFrames => make_frames(editor, true),
        LayerExtraOp::UnmakeFrames => make_frames(editor, false),
        LayerExtraOp::MergeFrames => merge_frames(editor),
    }
}

/// Apply `command` and answer whether it landed as a history step (a
/// refused transaction has already said why on the status line).
fn applied(editor: &mut Editor, command: Command) -> bool {
    let before = editor.active().map(|d| d.history_depth());
    editor.apply_command(command);
    editor.active().map(|d| d.history_depth()) != before
}

/// The selected layers, the active one included.
fn chosen(doc: &Document) -> Vec<LayerId> {
    let mut set = doc.layer_selection();
    if let Some(active) = doc.active_layer() {
        if !set.contains(&active) {
            set.push(active);
        }
    }
    set
}

fn active_layer(editor: &Editor) -> Result<(LayerId, Layer), String> {
    let doc = editor.active().ok_or("No document is open")?;
    let id = doc.document.active_layer().ok_or("Select a layer first")?;
    let layer = doc
        .document
        .layers
        .get(id)
        .ok_or("The active layer is not in the tree")?
        .clone();
    Ok((id, layer))
}

fn eight_bit(doc: &OpenDocument) -> Result<(), String> {
    if doc.document.meta.bit_depth == 8 {
        Ok(())
    } else {
        Err(ui::menu::LAYER_EXTRA_EIGHT_BIT.to_string())
    }
}

/// The tile edits that hold full-canvas straight RGBA8 `rgba` (`w`x`h`) in
/// layer space, skipping fully transparent tiles.
fn tile_edits(
    tiles: &mut MemoryTileSource,
    w: u32,
    h: u32,
    rgba: &[u8],
) -> Result<Vec<TileEdit>, String> {
    let grid = raster::TileGrid::from_rgba8(w, h, rgba).map_err(|e| e.to_string())?;
    let mut edits = Vec::new();
    for (coord, tile) in grid.iter() {
        let data = tile.data();
        if data.as_chunks::<4>().0.iter().all(|px| px[3] == 0) {
            continue;
        }
        edits.push(TileEdit::set(coord, tiles.insert_bytes(data.to_vec())));
    }
    Ok(edits)
}

/// Create `layer` holding `rgba` (full canvas) and seat it at `index` under
/// `parent`, as the commands of a transaction.
fn place_layer(
    commands: &mut Vec<Command>,
    tiles: &mut MemoryTileSource,
    (w, h): (u32, u32),
    layer: Layer,
    rgba: Option<&[u8]>,
    parent: Option<LayerId>,
    index: usize,
) -> Result<(), String> {
    let id = layer.id;
    commands.push(Command::create_layer(layer));
    if let Some(rgba) = rgba {
        let edits = tile_edits(tiles, w, h, rgba)?;
        if !edits.is_empty() {
            commands.push(
                Command::paint_tiles(PixelTarget::Layer(id), edits).map_err(|e| e.to_string())?,
            );
        }
    }
    commands.push(Command::MoveLayer {
        layer_id: id,
        parent,
        index,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Layer Style ▸ Scale Effects
// ---------------------------------------------------------------------------

fn scale_fill(fill: &mut FillStyle, k: f32) {
    if let FillStyle::Pattern(p) = fill {
        p.scale *= k;
        p.offset_px = [p.offset_px[0] * k, p.offset_px[1] * k];
    }
}

/// `fx` with every size and distance times `k`: shadow and satin distance
/// and size, glow size, bevel size and soften, stroke width, pattern scale
/// and offset, gradient-overlay offset. Opacities, angles, spreads and the
/// gradient-overlay scale (a fraction of the layer) are not sizes.
pub(crate) fn scaled_effects(fx: &LayerEffects, k: f32) -> LayerEffects {
    let mut out = fx.clone();
    let shadows = out
        .drop_shadow
        .iter_mut()
        .chain(out.inner_shadow.iter_mut())
        .chain(out.extras.drop_shadows.iter_mut().map(|i| &mut i.effect))
        .chain(out.extras.inner_shadows.iter_mut().map(|i| &mut i.effect));
    for s in shadows {
        s.distance_px *= k;
        s.size_px *= k;
    }
    for g in out.outer_glow.iter_mut().chain(out.inner_glow.iter_mut()) {
        g.size_px *= k;
        scale_fill(&mut g.fill, k);
    }
    if let Some(b) = &mut out.bevel_emboss {
        b.size_px *= k;
        b.soften_px *= k;
    }
    if let Some(s) = &mut out.satin {
        s.distance_px *= k;
        s.size_px *= k;
    }
    for s in out.stroke.iter_mut().chain(out.extras.strokes.iter_mut()) {
        s.size_px *= k;
        scale_fill(&mut s.fill, k);
    }
    if let Some(p) = &mut out.pattern_overlay {
        p.pattern.scale *= k;
        p.pattern.offset_px = [p.pattern.offset_px[0] * k, p.pattern.offset_px[1] * k];
    }
    for g in out
        .gradient_overlay
        .iter_mut()
        .chain(out.extras.gradient_overlays.iter_mut())
    {
        g.offset_px = [g.offset_px[0] * k, g.offset_px[1] * k];
    }
    out
}

/// Layer ▸ Layer Style ▸ Scale Effects… at `percent`%. One undo step.
pub(crate) fn scale_effects(editor: &mut Editor, percent: u16) -> Result<String, String> {
    let (id, layer) = active_layer(editor)?;
    if layer.effects.is_empty() {
        return Err(ui::menu::LAYER_EXTRA_NO_STYLE.to_string());
    }
    if layer.locked.all {
        return Err("The layer is locked".to_string());
    }
    if percent == 0 || percent == 100 {
        return Err("Scaling by that amount changes nothing".to_string());
    }
    let fx = scaled_effects(&layer.effects, f32::from(percent) / 100.0);
    let command = Command::Transaction {
        label: "Scale Effects".to_string(),
        commands: vec![Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                effects: Some(Box::new(fx)),
                ..Default::default()
            },
        }],
    };
    if !applied(editor, command) {
        return Err("Scale Effects was refused".to_string());
    }
    Ok(format!("Scaled {}'s effects to {percent}%", layer.name))
}

/// The longest side of the Scale Effects dialog's preview image.
pub(crate) const SCALE_EFFECTS_PREVIEW_SIDE: u32 = 320;

/// W13X-3: the Scale Effects dialog's Preview — the document composited
/// with the active layer's style scaled to `percent`%, nearest-downscaled so
/// its longest side is at most `max_side`. Straight RGBA8, width, height.
/// Reads the document; changes nothing.
pub(crate) fn scale_effects_preview(
    editor: &Editor,
    percent: u16,
    max_side: u32,
) -> Result<(Vec<u8>, u32, u32), String> {
    let (id, layer) = active_layer(editor)?;
    let doc = editor.active().ok_or("No document is open")?;
    let mut work = doc.document.clone();
    if let Some(l) = work.layers.get_mut(id) {
        l.effects = scaled_effects(&layer.effects, f32::from(percent) / 100.0);
    }
    let rgba = render(&work, &doc.tiles)?.to_rgba8(&work.meta.color_space);
    let (w, h) = (work.width(), work.height());
    let step = w.max(h).div_ceil(max_side.max(1)).max(1);
    if step == 1 {
        return Ok((rgba, w, h));
    }
    let (sw, sh) = (w.div_ceil(step), h.div_ceil(step));
    let mut out = Vec::with_capacity((sw * sh * 4) as usize);
    for y in 0..sh {
        for x in 0..sw {
            let i = (((y * step) * w + x * step) * 4) as usize;
            out.extend_from_slice(&rgba[i..i + 4]);
        }
    }
    Ok((out, sw, sh))
}

// ---------------------------------------------------------------------------
// Layer Style ▸ Create Layers
// ---------------------------------------------------------------------------

/// Where a piece of a style composites (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Part {
    Exterior,
    Interior,
    Stroke,
}

/// One effect of a style, alone: `alone` holds only it, in Normal; `mode`
/// is the blend mode it composites with.
struct Piece {
    part: Part,
    name: &'static str,
    mode: BlendMode,
    alone: LayerEffects,
}

/// The style's effects, one [`Piece`] each, in the compositor's drawing
/// order within each part (bottom-most first).
fn pieces(fx: &LayerEffects) -> Vec<Piece> {
    let normal = BlendMode::Normal;
    let mut out = Vec::new();
    for (e, contour) in fx.drop_shadows() {
        let mut alone = LayerEffects::default();
        let mut e = e.clone();
        let mode = std::mem::replace(&mut e.blend_mode, normal);
        alone.drop_shadow = Some(e);
        alone.extras.contours.drop_shadow = contour.clone();
        out.push(Piece {
            part: Part::Exterior,
            name: "Drop Shadow",
            mode,
            alone,
        });
    }
    if let Some(e) = &fx.outer_glow {
        let mut alone = LayerEffects::default();
        let mut e = e.clone();
        let mode = std::mem::replace(&mut e.blend_mode, normal);
        alone.outer_glow = Some(e);
        alone.extras.contours.outer_glow = fx.extras.contours.outer_glow.clone();
        out.push(Piece {
            part: Part::Exterior,
            name: "Outer Glow",
            mode,
            alone,
        });
    }
    if let Some(e) = &fx.pattern_overlay {
        let mut e = e.clone();
        let mode = std::mem::replace(&mut e.blend_mode, normal);
        out.push(Piece {
            part: Part::Interior,
            name: "Pattern Fill",
            mode,
            alone: LayerEffects {
                pattern_overlay: Some(e),
                ..Default::default()
            },
        });
    }
    for e in fx.gradient_overlays() {
        let mut e = e.clone();
        let mode = std::mem::replace(&mut e.blend_mode, normal);
        out.push(Piece {
            part: Part::Interior,
            name: "Gradient Fill",
            mode,
            alone: LayerEffects {
                gradient_overlay: Some(e),
                ..Default::default()
            },
        });
    }
    for e in fx.color_overlays() {
        let mut e = e.clone();
        let mode = std::mem::replace(&mut e.blend_mode, normal);
        out.push(Piece {
            part: Part::Interior,
            name: "Color Fill",
            mode,
            alone: LayerEffects {
                color_overlay: Some(e),
                ..Default::default()
            },
        });
    }
    if let Some(e) = &fx.satin {
        let mut e = e.clone();
        let mode = std::mem::replace(&mut e.blend_mode, normal);
        out.push(Piece {
            part: Part::Interior,
            name: "Satin",
            mode,
            alone: LayerEffects {
                satin: Some(e),
                ..Default::default()
            },
        });
    }
    if let Some(e) = &fx.inner_glow {
        let mut alone = LayerEffects::default();
        let mut e = e.clone();
        let mode = std::mem::replace(&mut e.blend_mode, normal);
        alone.inner_glow = Some(e);
        alone.extras.contours.inner_glow = fx.extras.contours.inner_glow.clone();
        out.push(Piece {
            part: Part::Interior,
            name: "Inner Glow",
            mode,
            alone,
        });
    }
    for (e, contour) in fx.inner_shadows() {
        let mut alone = LayerEffects::default();
        let mut e = e.clone();
        let mode = std::mem::replace(&mut e.blend_mode, normal);
        alone.inner_shadow = Some(e);
        alone.extras.contours.inner_shadow = contour.clone();
        out.push(Piece {
            part: Part::Interior,
            name: "Inner Shadow",
            mode,
            alone,
        });
    }
    if let Some(e) = &fx.bevel_emboss {
        // The bevel draws two inks, highlights then shadows; each is a layer.
        let mut hi = e.clone();
        hi.shadow_opacity = 0.0;
        let hi_mode = std::mem::replace(&mut hi.highlight_mode, normal);
        let mut sh = e.clone();
        sh.highlight_opacity = 0.0;
        let sh_mode = std::mem::replace(&mut sh.shadow_mode, normal);
        for (b, mode, name) in [
            (hi, hi_mode, "Bevel Highlights"),
            (sh, sh_mode, "Bevel Shadows"),
        ] {
            let mut alone = LayerEffects {
                bevel_emboss: Some(b),
                ..Default::default()
            };
            alone.extras.contours.bevel = fx.extras.contours.bevel.clone();
            out.push(Piece {
                part: Part::Interior,
                name,
                mode,
                alone,
            });
        }
    }
    for e in fx.strokes() {
        let mut e = e.clone();
        let mode = std::mem::replace(&mut e.blend_mode, normal);
        out.push(Piece {
            part: Part::Stroke,
            name: "Stroke",
            mode,
            alone: LayerEffects {
                stroke: Some(e),
                ..Default::default()
            },
        });
    }
    out
}

/// A copy of `doc` in which only `target` draws: every other layer hidden,
/// and the groups holding it made neutral (full opacity, no style, no mask),
/// so a render is the layer alone over transparency.
fn isolate(doc: &Document, target: LayerId) -> Document {
    let mut work = doc.clone();
    let mut chain = Vec::new();
    let mut at = work.layers.parent_of(target);
    while let Some(p) = at {
        chain.push(p);
        at = work.layers.parent_of(p);
    }
    for id in work.layers.iter_depth_first() {
        let Some(layer) = work.layers.get_mut(id) else {
            continue;
        };
        if chain.contains(&id) {
            layer.visible = true;
            layer.opacity = 1.0;
            layer.fill_opacity = 1.0;
            layer.effects = LayerEffects::default();
            layer.mask = None;
        } else if id != target {
            layer.visible = false;
        }
    }
    work
}

fn render(work: &Document, tiles: &MemoryTileSource) -> Result<Canvas, String> {
    compositor::composite_region(
        work,
        tiles,
        PixelRect::new(0, 0, work.width(), work.height()),
        0,
        CompositeOptions::default(),
    )
    .map_err(|e| e.to_string())
}

/// Render `target` alone with only `fx`, at `fill` fill opacity, full
/// opacity, in Normal.
fn render_with(
    base: &Document,
    tiles: &MemoryTileSource,
    target: LayerId,
    fx: LayerEffects,
    fill: f32,
) -> Result<Canvas, String> {
    let mut work = base.clone();
    if let Some(layer) = work.layers.get_mut(target) {
        layer.effects = fx;
        layer.fill_opacity = fill;
        layer.opacity = 1.0;
        layer.blend_mode = BlendMode::Normal;
        layer.clipping = ClippingMode::None;
    }
    render(&work, tiles)
}

/// `base` (already isolated to `target`) with `target` hidden and a root
/// raster layer added holding `shape`'s coverage in one opaque `grey`,
/// styled with `fx`: the layer's shape, recoloured. Its id and the scratch
/// document.
fn shape_copy(
    base: &Document,
    tiles: &mut MemoryTileSource,
    target: LayerId,
    shape: &[u8],
    grey: u8,
    fx: LayerEffects,
) -> Result<Document, String> {
    let (w, h) = (base.width(), base.height());
    let mut work = base.clone();
    if let Some(layer) = work.layers.get_mut(target) {
        layer.visible = false;
    }
    let mut rgba = shape.to_vec();
    for px in rgba.as_chunks_mut::<4>().0 {
        px[0] = grey;
        px[1] = grey;
        px[2] = grey;
    }
    let mut layer = Layer::raster("Shape");
    layer.effects = fx;
    let id = layer.id;
    let mut history = editor_core::History::new();
    history
        .apply(&mut work, Command::create_layer(layer))
        .map_err(|e| e.to_string())?;
    let edits = tile_edits(tiles, w, h, &rgba)?;
    if !edits.is_empty() {
        history
            .apply(
                &mut work,
                Command::paint_tiles(PixelTarget::Layer(id), edits).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(work)
}

/// An interior effect's own ink, from its Normal render atop an opaque black
/// (`on_black`) and an opaque white (`on_white`) copy of the layer's shape:
/// `atop` in Normal gives `sa * ab * ink + (1 - sa) * body`, so the two
/// differ by `ab * (1 - sa)` and the black one is `sa * ab * ink`.
fn interior_ink(on_black: &Canvas, on_white: &Canvas) -> Result<Canvas, String> {
    let mut out = Canvas::transparent(on_black.rect()).map_err(|e| e.to_string())?;
    for ((o, b), wh) in out
        .pixels_mut()
        .iter_mut()
        .zip(on_black.pixels())
        .zip(on_white.pixels())
    {
        let ab = b[3];
        if ab <= 1e-6 {
            continue;
        }
        let diff = ((wh[0] - b[0]) + (wh[1] - b[1]) + (wh[2] - b[2])) / 3.0;
        let sa = (1.0 - diff / ab).clamp(0.0, 1.0);
        if sa <= 1e-6 {
            continue;
        }
        let ink = |v: f32| (v / (sa * ab)).clamp(0.0, 1.0) * sa;
        *o = [ink(b[0]), ink(b[1]), ink(b[2]), sa];
    }
    Ok(out)
}

/// Layer ▸ Layer Style ▸ Create Layers: the active layer's style split into
/// raster layers that composite the same (see the module docs). The layer
/// keeps its pixels, opacity, fill, blend mode and Blend If, and loses its
/// effects. One undo step.
pub(crate) fn create_layers(editor: &mut Editor) -> Result<String, String> {
    let (target, layer) = active_layer(editor)?;
    let fx = layer.effects.clone();
    if fx.is_empty() {
        return Err(ui::menu::LAYER_EXTRA_NO_STYLE.to_string());
    }
    if !fx.enabled {
        return Err("The layer's style is switched off".to_string());
    }
    if matches!(layer.kind, LayerKind::Group(_)) {
        return Err("A group's style cannot be split into layers in this build".to_string());
    }
    if layer.is_clipping() {
        return Err(
            "The layer is clipped to the one below: release the clipping mask first".to_string(),
        );
    }
    if layer.locked.all {
        return Err("The layer is locked".to_string());
    }
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        eight_bit(doc)?;
        let (w, h) = (doc.document.width(), doc.document.height());
        let space = doc.document.meta.color_space.clone();
        let base = isolate(&doc.document, target);
        let shape =
            render_with(&base, &doc.tiles, target, LayerEffects::default(), 1.0)?.to_rgba8(&space);
        // Each piece's ink as full-canvas straight RGBA8.
        let mut built: Vec<(Part, Layer, Vec<u8>)> = Vec::new();
        for piece in pieces(&fx) {
            let ink = match piece.part {
                Part::Exterior | Part::Stroke => {
                    render_with(&base, &doc.tiles, target, piece.alone.clone(), 0.0)?
                }
                Part::Interior => {
                    let black = shape_copy(
                        &base,
                        &mut doc.tiles,
                        target,
                        &shape,
                        0,
                        piece.alone.clone(),
                    )?;
                    let white =
                        shape_copy(&base, &mut doc.tiles, target, &shape, 255, piece.alone)?;
                    interior_ink(&render(&black, &doc.tiles)?, &render(&white, &doc.tiles)?)?
                }
            };
            let rgba = ink.to_rgba8(&space);
            let mut out = Layer::raster(format!("{}'s {}", layer.name, piece.name));
            out.blend_mode = piece.mode;
            match piece.part {
                Part::Interior => out.clipping = ClippingMode::ClipToBelow,
                Part::Exterior | Part::Stroke => out.opacity = layer.opacity,
            }
            built.push((piece.part, out, rgba));
        }
        // The final sibling order, top-most first: whatever sits above the
        // layer's clipping group, the strokes, the interior layers, the
        // group's existing clipped layers and the layer, the exterior
        // layers, then everything below.
        let parent = doc.document.layers.parent_of(target);
        let siblings: Vec<LayerId> = doc
            .document
            .layers
            .siblings_of(target)
            .ok_or("The layer is not in the tree")?
            .to_vec();
        let at = siblings
            .iter()
            .position(|id| *id == target)
            .ok_or("The layer is not in the tree")?;
        let mut top = at;
        while top > 0
            && doc
                .document
                .layers
                .get(siblings[top - 1])
                .is_some_and(Layer::is_clipping)
        {
            top -= 1;
        }
        let mut order: Vec<Option<usize>> = siblings[..top].iter().map(|_| None).collect();
        for part in [Part::Stroke, Part::Interior] {
            for (i, (p, _, _)) in built.iter().enumerate().rev() {
                if *p == part {
                    order.push(Some(i));
                }
            }
        }
        order.extend(siblings[top..=at].iter().map(|_| None));
        for (i, (p, _, _)) in built.iter().enumerate().rev() {
            if *p == Part::Exterior {
                order.push(Some(i));
            }
        }
        let mut kept = LayerEffects::default();
        kept.extras.blend_if = fx.extras.blend_if;
        let mut commands = vec![Command::SetLayerProperties {
            layer_id: target,
            patch: LayerPatch {
                effects: Some(Box::new(kept)),
                ..Default::default()
            },
        }];
        let mut slots: Vec<Option<(Layer, Vec<u8>)>> = built
            .into_iter()
            .map(|(_, layer, rgba)| Some((layer, rgba)))
            .collect();
        // Seated top-most first: every layer above a slot is already in its
        // final place when the slot is filled.
        for (index, slot) in order.iter().enumerate() {
            let Some(i) = slot else {
                continue;
            };
            let (new, rgba) = slots[*i].take().expect("each piece is placed once");
            place_layer(
                &mut commands,
                &mut doc.tiles,
                (w, h),
                new,
                Some(&rgba),
                parent,
                index,
            )?;
        }
        Command::Transaction {
            label: "Create Layers".to_string(),
            commands,
        }
    };
    let made = match &command {
        Command::Transaction { commands, .. } => commands
            .iter()
            .filter(|c| matches!(c, Command::CreateLayer { .. }))
            .count(),
        _ => 0,
    };
    if !applied(editor, command) {
        return Err("Create Layers was refused".to_string());
    }
    editor.set_layer_selection(vec![target], Some(target));
    Ok(format!(
        "Split {}'s style into {made} layer{}",
        layer.name,
        if made == 1 { "" } else { "s" }
    ))
}

// ---------------------------------------------------------------------------
// New ▸ Artboard from Layers
// ---------------------------------------------------------------------------

/// Layer ▸ New ▸ Artboard from Layers: the selected top-level layers move,
/// in their order, into a new artboard whose rect is their combined ink
/// bounds, seated where the topmost of them was. The artboard is
/// transparent, so the picture does not change. One undo step.
pub(crate) fn artboard_from_layers(editor: &mut Editor) -> Result<String, String> {
    let (command, group_id, name, count) = {
        let doc = editor.active().ok_or("No document is open")?;
        let set = chosen(&doc.document);
        let tree = &doc.document.layers;
        let root = tree.root().to_vec();
        if set.is_empty() {
            return Err("Select a layer first".to_string());
        }
        if !set.iter().all(|id| root.contains(id)) {
            return Err("Artboards sit at the top of the stack: select top-level layers".into());
        }
        if set.iter().any(|id| {
            layer_model::artboard::artboard_of(tree, *id).is_some()
                || matches!(tree.get(*id).map(|l| &l.kind),
                    Some(LayerKind::Raster(r)) if r.artboard.is_some())
        }) {
            return Err("The selection already holds an artboard".to_string());
        }
        // In stack order, top-most first.
        let members: Vec<LayerId> = root.iter().copied().filter(|id| set.contains(id)).collect();
        let bounds = members
            .iter()
            .filter_map(|id| {
                crate::tool_input::tight_document_bounds(&doc.document, &doc.tiles, *id)
            })
            .filter(|r| r.width > 0 && r.height > 0)
            .reduce(|a, b| {
                let x0 = a.x.min(b.x);
                let y0 = a.y.min(b.y);
                let x1 = a.right().max(b.right());
                let y1 = a.bottom().max(b.bottom());
                PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
            })
            .ok_or("The selected layers have no pixels to size an artboard to")?;
        let name = format!(
            "Artboard {}",
            layer_model::artboard::artboards(tree).len() + 1
        );
        let group = Layer::group(name.clone());
        let group_id = group.id;
        let plate = Layer::with_kind(
            "Artboard Background",
            LayerKind::Raster(layer_model::RasterLayer {
                artboard: Some(layer_model::Artboard {
                    x: bounds.x,
                    y: bounds.y,
                    width: bounds.width,
                    height: bounds.height,
                    background: [0.0; 4],
                }),
                ..layer_model::RasterLayer::default()
            }),
        );
        let plate_id = plate.id;
        let top = root
            .iter()
            .position(|id| *id == members[0])
            .expect("a member is at the root");
        let mut commands = vec![
            Command::create_layer(group),
            Command::MoveLayer {
                layer_id: group_id,
                parent: None,
                index: top,
            },
        ];
        for (index, id) in members.iter().enumerate() {
            commands.push(Command::MoveLayer {
                layer_id: *id,
                parent: Some(group_id),
                index,
            });
        }
        commands.push(Command::create_layer(plate));
        commands.push(Command::MoveLayer {
            layer_id: plate_id,
            parent: Some(group_id),
            index: members.len(),
        });
        (
            Command::Transaction {
                label: "Artboard from Layers".to_string(),
                commands,
            },
            group_id,
            name,
            members.len(),
        )
    };
    if !applied(editor, command) {
        return Err("Artboard from Layers was refused".to_string());
    }
    editor.set_layer_selection(vec![group_id], Some(group_id));
    Ok(format!(
        "Made {name} from {count} layer{}",
        if count == 1 { "" } else { "s" }
    ))
}

// ---------------------------------------------------------------------------
// Layer Mask ▸ From Transparency
// ---------------------------------------------------------------------------

/// Layer ▸ Layer Mask ▸ From Transparency: the layer's alpha becomes a new
/// pixel mask, and every pixel with any coverage becomes opaque in its own
/// colour — so the picture is unchanged, and the edge now lives in the mask.
/// The mask becomes the paint target, as a new mask does. One undo step.
pub(crate) fn mask_from_transparency(editor: &mut Editor) -> Result<String, String> {
    let (id, layer) = active_layer(editor)?;
    if !matches!(layer.kind, LayerKind::Raster(_)) {
        return Err("From Transparency works on a pixel layer".to_string());
    }
    if layer.locked.blocks_pixel_edit() {
        return Err("The layer's pixels are locked".to_string());
    }
    if layer.mask.is_some() {
        return Err("The layer already has a mask - delete it first".to_string());
    }
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        eight_bit(doc)?;
        let side = raster::TILE_SIZE as usize;
        let coords: Vec<_> = doc
            .document
            .layer_tiles(id)
            .map(|m| m.iter().collect())
            .unwrap_or_default();
        let mut pixels = Vec::new();
        let mut coverage = Vec::new();
        for (coord, hash) in coords {
            let Some(bytes) = TileSource::tile(&doc.tiles, hash) else {
                continue;
            };
            if bytes.len() != side * side * 4 {
                return Err("The layer's tiles are not 8-bit RGBA".to_string());
            }
            let mut rgba = bytes.to_vec();
            let mut mask = Vec::with_capacity(side * side);
            for px in rgba.as_chunks_mut::<4>().0 {
                mask.push(px[3]);
                if px[3] > 0 {
                    px[3] = 255;
                }
            }
            pixels.push(TileEdit::set(coord, doc.tiles.insert_bytes(rgba)));
            coverage.push(TileEdit::set(coord, doc.tiles.insert_bytes(mask)));
        }
        if pixels.is_empty() {
            return Err("The layer is empty: it has no transparency to read".to_string());
        }
        Command::Transaction {
            label: "Mask from Transparency".to_string(),
            commands: vec![
                Command::SetLayerProperties {
                    layer_id: id,
                    patch: LayerPatch {
                        mask: Patch::Set(layer_model::LayerMask::new(layer_model::MaskId::new())),
                        ..Default::default()
                    },
                },
                Command::paint_tiles(PixelTarget::Mask(id), coverage).map_err(|e| e.to_string())?,
                Command::paint_tiles(PixelTarget::Layer(id), pixels).map_err(|e| e.to_string())?,
            ],
        }
    };
    if !applied(editor, command) {
        return Err("From Transparency was refused".to_string());
    }
    editor.set_edit_target_kind(crate::edit_target::EditTargetKind::Mask);
    Ok(format!("{}'s transparency is now its mask", layer.name))
}

// ---------------------------------------------------------------------------
// Smart Object ▸ Reset Transform / Stack Mode
// ---------------------------------------------------------------------------

/// A smart object: its id, the layer, and its source's origin and recorded
/// size.
type SmartFacts = (LayerId, Layer, Option<AssetOrigin>, Option<(u32, u32)>);

/// The active smart object's [`SmartFacts`].
fn active_smart(editor: &Editor) -> Result<SmartFacts, String> {
    let (id, layer) = active_layer(editor)?;
    let LayerKind::SmartObject(so) = &layer.kind else {
        return Err("The active layer is not a smart object".to_string());
    };
    let doc = editor.active().ok_or("No document is open")?;
    let record = doc.document.assets().iter().find(|r| r.id == so.asset);
    let origin = record.map(|r| r.origin.clone());
    let size = record.and_then(|r| r.source_size);
    Ok((id, layer, origin, size))
}

/// Layer ▸ Smart Object ▸ Reset Transform: the object's scale, rotation and
/// skew undone — its source at 100%, upright — keeping the object's centre
/// where it is (rounded to whole pixels). One undo step.
pub(crate) fn reset_transform(editor: &mut Editor) -> Result<String, String> {
    let (id, layer, _, size) = active_smart(editor)?;
    if layer.locked.blocks_transform() {
        return Err("The layer's position is locked".to_string());
    }
    let (sw, sh) = size.ok_or("The smart object does not record its source's size")?;
    let half = glam::Vec2::new(sw as f32 / 2.0, sh as f32 / 2.0);
    let centre = layer.transform.transform_point2(half);
    let reset = glam::Affine2::from_translation((centre - half).round());
    if reset.abs_diff_eq(layer.transform, 1e-4) {
        return Err("The smart object is already at its source's size and angle".to_string());
    }
    let command = Command::Transaction {
        label: "Reset Transform".to_string(),
        commands: vec![Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                transform: Some(reset.to_cols_array()),
                ..Default::default()
            },
        }],
    };
    if !applied(editor, command) {
        return Err("Reset Transform was refused".to_string());
    }
    Ok(format!("{} is back at {sw}x{sh}, upright", layer.name))
}

/// One statistic over the `n` samples of one channel, each in `0..=1`.
pub(crate) fn stack_statistic(mode: StackMode, values: &mut [f32]) -> f32 {
    let n = values.len();
    if n == 0 {
        return 0.0;
    }
    let nf = n as f32;
    let mean = values.iter().sum::<f32>() / nf;
    let moment = |k: i32| values.iter().map(|v| (v - mean).powi(k)).sum::<f32>() / nf;
    let variance = moment(2);
    let sd = variance.sqrt();
    let out = match mode {
        StackMode::Mean => mean,
        StackMode::Minimum => values.iter().copied().fold(f32::MAX, f32::min),
        StackMode::Maximum => values.iter().copied().fold(f32::MIN, f32::max),
        StackMode::Range => {
            values.iter().copied().fold(f32::MIN, f32::max)
                - values.iter().copied().fold(f32::MAX, f32::min)
        }
        StackMode::Median => {
            values.sort_by(f32::total_cmp);
            if n % 2 == 1 {
                values[n / 2]
            } else {
                (values[n / 2 - 1] + values[n / 2]) / 2.0
            }
        }
        StackMode::Summation => values.iter().sum(),
        StackMode::Variance => variance,
        StackMode::StandardDeviation => sd,
        StackMode::Skewness if sd > 1e-6 => moment(3) / (sd * sd * sd),
        StackMode::Kurtosis if sd > 1e-6 => moment(4) / (variance * variance),
        StackMode::Skewness | StackMode::Kurtosis => 0.0,
        StackMode::Entropy => {
            let mut bins = [0usize; 256];
            for v in values.iter() {
                bins[(v.clamp(0.0, 1.0) * 255.0).round() as usize] += 1;
            }
            let h: f32 = bins
                .iter()
                .filter(|c| **c > 0)
                .map(|c| {
                    let p = *c as f32 / nf;
                    -p * p.log2()
                })
                .sum();
            if n > 1 {
                h / nf.log2()
            } else {
                0.0
            }
        }
    };
    out.clamp(0.0, 1.0)
}

/// The stack statistic of `layers` (each straight RGBA8, all one size):
/// `mode` per colour channel across the layers covering the pixel, and the
/// union of their coverage (the largest alpha) as the alpha.
pub(crate) fn stack_pixels(mode: StackMode, layers: &[Vec<u8>]) -> Vec<u8> {
    let len = layers.first().map_or(0, Vec::len);
    let mut out = vec![0u8; len];
    let mut values = Vec::with_capacity(layers.len());
    for i in (0..len).step_by(4) {
        let alpha = layers.iter().map(|l| l[i + 3]).max().unwrap_or(0);
        if alpha == 0 {
            continue;
        }
        for c in 0..3 {
            values.clear();
            values.extend(
                layers
                    .iter()
                    .filter(|l| l[i + 3] > 0)
                    .map(|l| f32::from(l[i + c]) / 255.0),
            );
            out[i + c] = (stack_statistic(mode, &mut values) * 255.0).round() as u8;
        }
        out[i + 3] = alpha;
    }
    out
}

/// Layer ▸ Smart Object ▸ Stack Mode ▸ `mode`: the statistic across the
/// object's layers (its layered PSD source's visible top-level layers, each
/// composited alone) lands as a new raster layer directly above the object,
/// posed as the object is; the object is kept, hidden, beneath it. The
/// statistic is baked, not live: changing the object's contents does not
/// update it. One undo step.
pub(crate) fn stack_mode(editor: &mut Editor, mode: StackMode) -> Result<String, String> {
    let (id, layer, origin, _) = active_smart(editor)?;
    let bytes = match origin {
        Some(AssetOrigin::Embedded { bytes, .. }) => bytes,
        Some(AssetOrigin::Linked { path }) => {
            std::fs::read(&path).map_err(|e| format!("The linked source cannot be read: {e}"))?
        }
        None => return Err("The smart object's source is missing".to_string()),
    };
    if !bytes.starts_with(b"8BPS") {
        return Err(
            "The smart object's source is a single image, not a stack of layers".to_string(),
        );
    }
    let inner = crate::import::document_from_psd(&bytes, "Contents", 1)
        .map_err(|e| format!("The smart object's source cannot be read: {e}"))?
        .imported;
    let (iw, ih) = (inner.document.width(), inner.document.height());
    let roots: Vec<LayerId> = inner
        .document
        .layers
        .root()
        .iter()
        .copied()
        .filter(|r| inner.document.layers.get(*r).is_some_and(|l| l.visible))
        .collect();
    if roots.len() < 2 {
        return Err("Stack Mode needs two or more visible layers in the smart object".to_string());
    }
    let space = inner.document.meta.color_space.clone();
    let mut stack = Vec::with_capacity(roots.len());
    for shown in &roots {
        let mut work = inner.document.clone();
        for r in &roots {
            if let Some(l) = work.layers.get_mut(*r) {
                l.visible = r == shown;
            }
        }
        stack.push(render(&work, &inner.tiles)?.to_rgba8(&space));
    }
    let rgba = stack_pixels(mode, &stack);
    let (command, new_id) = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        eight_bit(doc)?;
        let mut result = Layer::raster(format!("{} ({})", layer.name, mode.label()));
        result.transform = layer.transform;
        let new_id = result.id;
        let parent = doc.document.layers.parent_of(id);
        let index = doc
            .document
            .layers
            .index_in_parent(id)
            .ok_or("The layer is not in the tree")?;
        let mut commands = Vec::new();
        place_layer(
            &mut commands,
            &mut doc.tiles,
            (iw, ih),
            result,
            Some(&rgba),
            parent,
            index,
        )?;
        commands.push(Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                visible: Some(false),
                ..Default::default()
            },
        });
        (
            Command::Transaction {
                label: format!("Stack Mode {}", mode.label()),
                commands,
            },
            new_id,
        )
    };
    if !applied(editor, command) {
        return Err("Stack Mode was refused".to_string());
    }
    editor.set_layer_selection(vec![new_id], Some(new_id));
    Ok(format!(
        "{} of {} layers, above the hidden smart object",
        mode.label(),
        roots.len()
    ))
}

// ---------------------------------------------------------------------------
// Animation ▸ Make Frames / Unmake Frames / Merge
// ---------------------------------------------------------------------------

/// Animation ▸ Make Frames (`make`) / Unmake Frames: the selected top-level
/// layers take (or lose) Photopea's `_a_<name>,<delay>` frame name, which is
/// what File ▸ Export As writes an animation from. One undo step.
pub(crate) fn make_frames(editor: &mut Editor, make: bool) -> Result<String, String> {
    let command = {
        let doc = editor.active().ok_or("No document is open")?;
        let root = doc.document.layers.root().to_vec();
        let mut commands = Vec::new();
        for id in root.iter().rev() {
            if !chosen(&doc.document).contains(id) {
                continue;
            }
            let Some(layer) = doc.document.layers.get(*id) else {
                continue;
            };
            let parsed = raster::animation::parse_frame_layer_name(&layer.name);
            let name = match (make, parsed) {
                (true, None) => raster::animation::frame_layer_name(
                    &layer.name,
                    raster::animation::DEFAULT_FRAME_DELAY_MS,
                ),
                (false, Some((label, _))) => label.to_string(),
                _ => continue,
            };
            commands.push(Command::SetLayerProperties {
                layer_id: *id,
                patch: LayerPatch {
                    name: Some(name),
                    ..Default::default()
                },
            });
        }
        if commands.is_empty() {
            return Err(if make {
                "Select top-level layers that are not frames yet".to_string()
            } else {
                "No selected layer is a frame".to_string()
            });
        }
        Command::Transaction {
            label: if make { "Make Frames" } else { "Unmake Frames" }.to_string(),
            commands,
        }
    };
    let count = match &command {
        Command::Transaction { commands, .. } => commands.len(),
        _ => 1,
    };
    if !applied(editor, command) {
        return Err("The frame change was refused".to_string());
    }
    Ok(format!(
        "{} {count} layer{}",
        if make {
            "Made frames of"
        } else {
            "Unmade the frames of"
        },
        if count == 1 { "" } else { "s" }
    ))
}

/// Animation ▸ Merge: every frame flattened — that frame shown, the other
/// frames hidden, every non-frame layer as the document has it — into one
/// raster frame layer of the same name and visibility; the new frames
/// replace every top-level layer. An exported animation is unchanged, and
/// each frame is now self-contained. One undo step.
pub(crate) fn merge_frames(editor: &mut Editor) -> Result<String, String> {
    let (command, first) = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        eight_bit(doc)?;
        let frames = crate::import::animation_frame_layers(&doc.document);
        if frames.is_empty() {
            return Err("The document has no frames: use Make Frames first".to_string());
        }
        let tree = &doc.document.layers;
        if tree
            .iter_depth_first()
            .iter()
            .any(|id| tree.get(*id).is_some_and(|l| l.locked.all))
        {
            return Err("A locked layer cannot be merged away: unlock it first".to_string());
        }
        let pictures = crate::import::composite_animation_frames(&doc.document, &doc.tiles)
            .map_err(|e| e.to_string())?;
        let (w, h) = (doc.document.width(), doc.document.height());
        let old_roots = doc.document.layers.root().to_vec();
        let mut commands = Vec::new();
        let mut first = None;
        // Created in play order: each lands on top, so frame 1 ends at the
        // bottom, as the frames were.
        for ((old, _), picture) in frames.iter().zip(&pictures) {
            let source = doc
                .document
                .layers
                .get(*old)
                .ok_or("A frame is not in the tree")?;
            let mut frame = Layer::raster(source.name.clone());
            frame.visible = source.visible;
            let id = frame.id;
            first.get_or_insert(id);
            commands.push(Command::create_layer(frame));
            let edits = tile_edits(&mut doc.tiles, w, h, &picture.rgba8)?;
            if !edits.is_empty() {
                commands.push(
                    Command::paint_tiles(PixelTarget::Layer(id), edits)
                        .map_err(|e| e.to_string())?,
                );
            }
        }
        for old in old_roots {
            commands.push(Command::DeleteLayer { layer_id: old });
        }
        (
            Command::Transaction {
                label: "Merge Frames".to_string(),
                commands,
            },
            first,
        )
    };
    let count = match &command {
        Command::Transaction { commands, .. } => commands
            .iter()
            .filter(|c| matches!(c, Command::CreateLayer { .. }))
            .count(),
        _ => 0,
    };
    if !applied(editor, command) {
        return Err("Merge was refused".to_string());
    }
    if let Some(id) = first {
        editor.set_layer_selection(vec![id], Some(id));
    }
    Ok(format!(
        "Merged {count} frame{}",
        if count == 1 { "" } else { "s" }
    ))
}

#[cfg(test)]
#[path = "layer_ops_w13_tests.rs"]
mod tests;
