//! W18-B: artboards and layer comps between a `.psd` and the document.
//!
//! | `.psd` | this document |
//! |---|---|
//! | a group record's `artb` / `artd` / `abdd` block (rect, background, preset) | an artboard group: [`layer_model::artboard`]'s bottom background plate, painted with the background over the rect |
//! | image resource 1065 (comp names, ids, `capturedInfo`, comments, last applied) | [`layer_model::DocumentExtras::layer_comps`] with their [`layer_model::doc_extras::CompFlags`] and `last_comp` |
//! | each layer's `shmd` / `cmls` settings (visible, offset, blend, opacity, fill, layer style per comp) | each comp's [`layer_model::CompLayerState`] rows |
//! | `cmls` comp id 0 | the Layer Comps panel's Last Document State |
//!
//! Photoshop draws an artboard's background itself and has no background
//! layer, so the plate is not written as a layer record: it becomes the
//! `artb` block, and an import builds it back. The parsing and building is
//! the `psd` crate's ([`psd::artboard`], [`psd::layer_comps`]), bounded
//! there; this module only maps.

use compositor::MemoryTileSource;
use editor_core::pixels::{PixelKey, TileDelta};
use editor_core::Document;
use layer_model::artboard::{artboard_of, Artboard};
use layer_model::doc_extras::CompFlags;
use layer_model::{
    CompLayerState, Layer, LayerComp, LayerEffects, LayerId, LayerKind, RasterLayer,
};
use psd::artboard::{ArtboardBackground, PsdArtboard};
use psd::layer_comps::{self as lc, PsdCompLayerState, PsdLayerComp, PsdLayerComps};

use super::{tile_edits_for_rgba, ImportError, PsdNotes};

/// The name an imported artboard's background plate takes (the Artboard
/// tool's).
pub(crate) const PLATE_NAME: &str = "Artboard Background";

fn opts() -> psd::ReadOptions {
    psd::ReadOptions::default()
}

// ------------------------------------------------------------ artboards

fn srgb_byte(linear: f32) -> f64 {
    let v = color::transfer::linear_to_srgb(linear.clamp(0.0, 1.0));
    (f64::from(v) * 255.0).round()
}

/// The `artb` background for a model background (straight linear RGBA).
fn psd_background(bg: [f32; 4]) -> ArtboardBackground {
    if bg[3].is_nan() || bg[3] <= 0.0 {
        return ArtboardBackground::Transparent;
    }
    let rgb = [srgb_byte(bg[0]), srgb_byte(bg[1]), srgb_byte(bg[2])];
    match rgb {
        [255.0, 255.0, 255.0] => ArtboardBackground::White,
        [0.0, 0.0, 0.0] => ArtboardBackground::Black,
        c => ArtboardBackground::Other(c),
    }
}

/// The model background (straight linear RGBA) for an `artb` background.
fn model_background(bg: ArtboardBackground) -> [f32; 4] {
    match bg {
        ArtboardBackground::White => [1.0, 1.0, 1.0, 1.0],
        ArtboardBackground::Black => [0.0, 0.0, 0.0, 1.0],
        ArtboardBackground::Transparent => [0.0, 0.0, 0.0, 0.0],
        ArtboardBackground::Other(c) => {
            let lin = |v: f64| color::transfer::srgb_to_linear((v / 255.0).clamp(0.0, 1.0) as f32);
            [lin(c[0]), lin(c[1]), lin(c[2]), 1.0]
        }
    }
}

/// When `group` is an artboard: write its `artb` block onto `record` and
/// answer the plate, which is NOT written as a layer record (Photoshop draws
/// the background from the block). A translucent background is written
/// opaque — the block has no alpha — and named by [`export_document`].
pub(crate) fn export_artboard(
    document: &Document,
    group: LayerId,
    record: &mut psd::PsdLayer,
) -> Result<Option<LayerId>, ImportError> {
    let Some((plate, board)) = artboard_of(&document.layers, group) else {
        return Ok(None);
    };
    let rect = crate::artboard_export::artboard_document_rect(document, group, &board)
        .unwrap_or_else(|| raster::PixelRect::new(board.x, board.y, board.width, board.height));
    let psd_board = PsdArtboard {
        left: rect.x as f64,
        top: rect.y as f64,
        right: rect.x as f64 + f64::from(rect.width),
        bottom: rect.y as f64 + f64::from(rect.height),
        background: psd_background(board.background),
        preset_name: String::new(),
    };
    record.set_artboard(Some(&psd_board))?;
    Ok(Some(plate))
}

/// When `source` is a Photoshop artboard: give the imported group `group`
/// its background plate (bottom-most child, painted with the background over
/// the rect's part of the canvas). An unreadable block is named, and the
/// group stays a plain group.
pub(crate) fn import_artboard(
    source: &psd::PsdLayer,
    group: LayerId,
    document: &mut Document,
    tiles: &mut MemoryTileSource,
    notes: &mut PsdNotes,
) -> Result<(), ImportError> {
    let Some(read) = source.artboard(&opts()) else {
        return Ok(());
    };
    let psd_board = match read {
        Ok(b) if b.is_valid() => b,
        Ok(_) => {
            notes.push(format!(
                "the artboard \u{201c}{}\u{201d} has an empty rect and opened as a plain group",
                source.name
            ));
            return Ok(());
        }
        Err(e) => {
            notes.push(format!(
                "the artboard \u{201c}{}\u{201d} could not be read ({e}) and opened as a plain group",
                source.name
            ));
            return Ok(());
        }
    };
    const LIMIT: f64 = 1_000_000_000.0;
    let x = psd_board.left.floor().clamp(-LIMIT, LIMIT);
    let y = psd_board.top.floor().clamp(-LIMIT, LIMIT);
    let w = (psd_board.right.ceil().clamp(-LIMIT, LIMIT) - x).clamp(1.0, f64::from(u32::MAX));
    let h = (psd_board.bottom.ceil().clamp(-LIMIT, LIMIT) - y).clamp(1.0, f64::from(u32::MAX));
    let board = Artboard {
        x: x as i64,
        y: y as i64,
        width: w as u32,
        height: h as u32,
        background: model_background(psd_board.background),
    };
    let plate = Layer::with_kind(
        PLATE_NAME,
        LayerKind::Raster(RasterLayer {
            artboard: Some(board),
            ..RasterLayer::default()
        }),
    );
    // Index 0 now; the group's children are inserted above it one by one,
    // so it ends at the bottom.
    let plate = document.layers.insert_at(plate, Some(group), 0)?;
    if board.background[3] <= 0.0 {
        return Ok(());
    }
    // Painted over the rect's part of the canvas: bounded by the canvas the
    // document already holds, whatever the file claims.
    let (cw, ch) = (i64::from(document.width()), i64::from(document.height()));
    let (x0, y0) = (board.x.clamp(0, cw), board.y.clamp(0, ch));
    let x1 = (board.x + i64::from(board.width)).clamp(0, cw);
    let y1 = (board.y + i64::from(board.height)).clamp(0, ch);
    if x1 <= x0 || y1 <= y0 {
        return Ok(());
    }
    let px = match psd_board.background {
        ArtboardBackground::White => [255, 255, 255, 255],
        ArtboardBackground::Black => [0, 0, 0, 255],
        ArtboardBackground::Transparent => return Ok(()),
        ArtboardBackground::Other(c) => {
            let byte = |v: f64| v.round().clamp(0.0, 255.0) as u8;
            [byte(c[0]), byte(c[1]), byte(c[2]), 255]
        }
    };
    let (pw, ph) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let rgba = px.repeat(pw * ph);
    let rect = psd::Rect::new(x0 as i32, y0 as i32, x1 as i32, y1 as i32);
    let edits = tile_edits_for_rgba(&rgba, rect, tiles);
    if !edits.is_empty() {
        let delta = TileDelta::new(edits).map_err(editor_core::CommandError::from)?;
        document.pixels.apply(PixelKey::Layer(plate), &delta);
    }
    Ok(())
}

// ------------------------------------------------------------ layer comps

/// The id comp `index` is written under (0 is the Last Document State).
fn comp_id(index: usize) -> i32 {
    i32::try_from(index + 1).unwrap_or(i32::MAX)
}

fn captured(flags: CompFlags) -> u32 {
    let mut bits = 0;
    if flags.visibility {
        bits |= lc::CAPTURED_VISIBILITY;
    }
    if flags.position {
        bits |= lc::CAPTURED_POSITION;
    }
    if flags.appearance {
        bits |= lc::CAPTURED_APPEARANCE;
    }
    bits
}

fn flags(bits: u32) -> CompFlags {
    CompFlags {
        visibility: bits & lc::CAPTURED_VISIBILITY != 0,
        position: bits & lc::CAPTURED_POSITION != 0,
        appearance: bits & lc::CAPTURED_APPEARANCE != 0,
    }
}

fn effects_descriptor(effects: &LayerEffects) -> Option<psd::Descriptor> {
    if effects.is_default() {
        return None;
    }
    let (data, _, _) =
        psd::effects::export_effects_in(effects, psd::effects::EffectsContext::default())?;
    psd::Effects {
        key: *b"lfx2",
        data,
    }
    .descriptor(&opts())
}

fn effects_of(descriptor: &psd::Descriptor) -> LayerEffects {
    let mut sink = psd::bytes::Sink::new();
    sink.u32(0);
    sink.u32(16);
    if descriptor.write(&mut sink).is_err() {
        return LayerEffects::default();
    }
    let block = psd::Effects {
        key: *b"lfx2",
        data: sink.into_inner(),
    };
    psd::effects::import_effects(&block, &opts()).map_or_else(LayerEffects::default, |i| i.effects)
}

/// One comp row as a `cmls` entry for `layer`: the `Ofst` is how far the
/// comp's translation is from the layer's (Photoshop's and Photopea's
/// `Ofst` is the move from the saved position, not the position).
fn psd_state(id: i32, layer: &Layer, s: &CompLayerState) -> PsdCompLayerState {
    let t = glam::Affine2::from_cols_array(&s.transform);
    let (dx, dy) = (
        t.translation.x - layer.transform.translation.x,
        t.translation.y - layer.transform.translation.y,
    );
    let shift = |v: f32| {
        if v.is_finite() {
            v.round().clamp(-1.0e9, 1.0e9) as i32
        } else {
            0
        }
    };
    PsdCompLayerState {
        comp_id: id,
        visible: Some(s.visible),
        offset: Some((shift(dx), shift(dy))),
        blend_mode: Some(s.blend_mode),
        opacity: Some(f64::from(s.opacity)),
        fill_opacity: Some(f64::from(s.fill_opacity)),
        // Always written: an entry inherits what it omits from the entries
        // before it, so "no layer style" is an empty `Lefx`, not an absent
        // one.
        effects: Some(
            effects_descriptor(&s.effects).unwrap_or_else(|| psd::Descriptor::new("null")),
        ),
    }
}

/// Write layer `id`'s row of every comp (and of the Last Document State)
/// onto its finished record as `cmls` settings. Nothing when the document
/// has no comps.
pub(crate) fn export_comp_states(
    document: &Document,
    id: LayerId,
    record: &mut psd::PsdLayer,
) -> Result<(), ImportError> {
    let x = &document.extras;
    if x.layer_comps.is_empty() && x.last_document_state.is_none() {
        return Ok(());
    }
    let Some(layer) = document.layers.get(id) else {
        return Ok(());
    };
    let mut states = Vec::new();
    if let Some(s) = x.last_document_state.as_ref().and_then(|c| c.state_of(id)) {
        states.push(psd_state(lc::LAST_DOCUMENT_STATE_ID, layer, s));
    }
    for (index, comp) in x.layer_comps.iter().enumerate() {
        if let Some(s) = comp.state_of(id) {
            states.push(psd_state(comp_id(index), layer, s));
        }
    }
    lc::set_layer_states(record, &states)?;
    Ok(())
}

/// Resource 1065 for the document's comps, and a layer id on every record
/// so the `cmls` settings name their layer (nothing without comps); and the
/// notes for what the artboards and comps could not carry.
pub(crate) fn export_document(
    document: &Document,
    file: &mut psd::PsdFile,
    notes: &mut PsdNotes,
) -> Result<(), ImportError> {
    for (group, board) in layer_model::artboard::artboards(&document.layers) {
        let a = board.background[3];
        if a > 0.0 && a < 1.0 {
            let name = document.layers.get(group).map_or("", |l| l.name.as_str());
            notes.push(format!(
                "the artboard \u{201c}{name}\u{201d} has a translucent background; a .psd artboard's is opaque, so it was written at full opacity"
            ));
        }
    }
    let x = &document.extras;
    if x.layer_comps.is_empty() && x.last_document_state.is_none() {
        return Ok(());
    }
    let comps = PsdLayerComps {
        comps: x
            .layer_comps
            .iter()
            .enumerate()
            .map(|(i, c)| PsdLayerComp {
                id: comp_id(i),
                name: c.name.clone(),
                captured: captured(c.flags),
                comment: (!c.comment.is_empty()).then(|| c.comment.clone()),
            })
            .collect(),
        last_applied: x
            .last_comp
            .filter(|i| *i < x.layer_comps.len())
            .map(comp_id),
    };
    file.resources.push(lc::resource(&comps)?);
    lc::number_layers(&mut file.layers, &opts())?;
    let reshaped = x
        .layer_comps
        .iter()
        .flat_map(|c| &c.layers)
        .filter(|s| {
            let t = glam::Affine2::from_cols_array(&s.transform);
            let m = t.matrix2;
            document
                .layers
                .get(s.layer)
                .is_some_and(|l| l.transform.matrix2 != m)
        })
        .count();
    if reshaped > 0 {
        notes.push(format!(
            "{reshaped} layer comp row(s) record a scale or rotation; a .psd comp keeps only the position, so they were written as the offset"
        ));
    }
    Ok(())
}

/// Every comp row the imported layers' `cmls` settings recorded, gathered
/// layer by layer while the tree is built ([`ImportedComps::collect`]) and
/// turned into the document's comps at the end ([`ImportedComps::finish`]).
#[derive(Default)]
pub(crate) struct ImportedComps {
    /// (comp id, row), in the order the layers were read.
    rows: Vec<(i32, CompLayerState)>,
    unreadable: Vec<String>,
}

impl ImportedComps {
    /// The rows `source` recorded, for the layer it became (`id`).
    pub(crate) fn collect(&mut self, source: &psd::PsdLayer, id: LayerId, document: &Document) {
        let states = match lc::layer_states(source, &opts()) {
            Ok(s) => s,
            Err(_) => {
                self.unreadable.push(source.name.clone());
                return;
            }
        };
        let Some(layer) = document.layers.get(id) else {
            return;
        };
        for s in states {
            // `Ofst` is the move from the saved position.
            let transform = match s.offset {
                Some((dx, dy)) => {
                    glam::Affine2::from_translation(glam::Vec2::new(dx as f32, dy as f32))
                        * layer.transform
                }
                None => layer.transform,
            };
            let row = CompLayerState {
                layer: id,
                visible: s.visible.unwrap_or(layer.visible),
                transform: transform.to_cols_array(),
                opacity: s.opacity.map_or(layer.opacity, |v| v as f32),
                fill_opacity: s.fill_opacity.map_or(layer.fill_opacity, |v| v as f32),
                blend_mode: s.blend_mode.unwrap_or(layer.blend_mode),
                effects: s
                    .effects
                    .as_ref()
                    .map_or_else(LayerEffects::default, effects_of),
            };
            self.rows.push((s.comp_id, row));
        }
    }

    /// Resource 1065 and the gathered rows onto `document.extras`: each
    /// comp's rows in the tree's depth-first order, as a captured comp's
    /// are.
    pub(crate) fn finish(self, file: &psd::PsdFile, document: &mut Document, notes: &mut PsdNotes) {
        if !self.unreadable.is_empty() {
            notes.push(format!(
                "the layer comp settings of {} layer(s) could not be read: {}",
                self.unreadable.len(),
                self.unreadable.join(", ")
            ));
        }
        let comps = match lc::layer_comps(&file.resources, &opts()) {
            None => return,
            Some(Ok(c)) => c,
            Some(Err(e)) => {
                notes.push(format!(
                    "the document's layer comps could not be read ({e})"
                ));
                return;
            }
        };
        let order: std::collections::HashMap<LayerId, usize> = document
            .layers
            .iter_depth_first()
            .into_iter()
            .enumerate()
            .map(|(i, id)| (id, i))
            .collect();
        let rank = |id: LayerId| order.get(&id).copied().unwrap_or(usize::MAX);
        let rows_for = |comp: i32| {
            let mut rows: Vec<CompLayerState> = self
                .rows
                .iter()
                .filter(|(c, _)| *c == comp)
                .map(|(_, r)| r.clone())
                .collect();
            rows.sort_by_key(|r| rank(r.layer));
            rows
        };
        let x = &mut document.extras;
        x.layer_comps = comps
            .comps
            .iter()
            .map(|c| LayerComp {
                name: c.name.clone(),
                comment: c.comment.clone().unwrap_or_default(),
                layers: rows_for(c.id),
                flags: flags(c.captured),
            })
            .collect();
        x.last_comp = comps
            .last_applied
            .and_then(|id| comps.comps.iter().position(|c| c.id == id));
        let last = rows_for(lc::LAST_DOCUMENT_STATE_ID);
        if !last.is_empty() {
            x.last_document_state = Some(LayerComp {
                name: "Last Document State".into(),
                layers: last,
                ..LayerComp::default()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_model::artboard::artboards;
    use layer_model::BlendMode;

    const W: u32 = 300;
    const H: u32 = 120;

    fn paint(doc: &mut Document, tiles: &mut MemoryTileSource, id: LayerId, rect: psd::Rect) {
        let rgba = [200u8, 30, 30, 255].repeat((rect.width() * rect.height()) as usize);
        let edits = tile_edits_for_rgba(&rgba, rect, tiles);
        let delta = TileDelta::new(edits).unwrap();
        doc.pixels.apply(PixelKey::Layer(id), &delta);
    }

    /// An artboard group with its plate (painted, as the Artboard tool
    /// does) and one painted content layer.
    fn artboard(
        doc: &mut Document,
        tiles: &mut MemoryTileSource,
        name: &str,
        board: Artboard,
        content: (&str, psd::Rect),
    ) {
        let group = doc.layers.push_root(Layer::group(name)).unwrap();
        let plate = doc
            .layers
            .push_root(Layer::with_kind(
                PLATE_NAME,
                LayerKind::Raster(RasterLayer {
                    artboard: Some(board),
                    ..RasterLayer::default()
                }),
            ))
            .unwrap();
        doc.layers.move_layer(plate, Some(group), 0).unwrap();
        let (x, y) = (board.x as i32, board.y as i32);
        paint(
            doc,
            tiles,
            plate,
            psd::Rect::new(x, y, x + board.width as i32, y + board.height as i32),
        );
        let kid = doc.layers.push_root(Layer::raster(content.0)).unwrap();
        doc.layers.move_layer(kid, Some(group), 0).unwrap();
        paint(doc, tiles, kid, content.1);
    }

    fn find(doc: &Document, name: &str) -> LayerId {
        doc.layers
            .iter_depth_first()
            .into_iter()
            .find(|id| doc.layers.get(*id).is_some_and(|l| l.name == name))
            .unwrap_or_else(|| panic!("no layer {name}"))
    }

    fn orange() -> [f32; 4] {
        model_background(ArtboardBackground::Other([255.0, 128.0, 0.0]))
    }

    /// Two artboards and three layer comps (plus the Last Document State),
    /// as the editor keeps them.
    fn source() -> (Document, MemoryTileSource) {
        let mut doc = Document::new(W, H, "boards");
        let mut tiles = MemoryTileSource::new();
        artboard(
            &mut doc,
            &mut tiles,
            "Artboard 1",
            Artboard {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
                background: [1.0, 1.0, 1.0, 1.0],
            },
            ("Red", psd::Rect::new(20, 20, 30, 30)),
        );
        artboard(
            &mut doc,
            &mut tiles,
            "Artboard 2",
            Artboard {
                x: 150,
                y: 0,
                width: 120,
                height: 100,
                background: orange(),
            },
            ("Blue", psd::Rect::new(160, 10, 170, 20)),
        );
        let free = doc.layers.push_root(Layer::raster("Free")).unwrap();
        paint(
            &mut doc,
            &mut tiles,
            free,
            psd::Rect::new(280, 100, 285, 105),
        );

        // Comp 1: everything as it stands.
        let all = LayerComp::capture("All", &doc.layers);
        // Comp 2: Red hidden, Blue half-opaque Multiply with a quarter fill;
        // visibility only, with a comment.
        let (red, blue) = (find(&doc, "Red"), find(&doc, "Blue"));
        doc.layers.get_mut(red).unwrap().visible = false;
        {
            let b = doc.layers.get_mut(blue).unwrap();
            b.opacity = 0.5;
            b.fill_opacity = 0.25;
            b.blend_mode = BlendMode::Multiply;
            b.effects.drop_shadow = Some(layer_model::effects::ShadowEffect::default());
        }
        let mut half = LayerComp::capture("Half", &doc.layers);
        half.comment = "for the client".into();
        half.flags = CompFlags {
            visibility: true,
            position: false,
            appearance: false,
        };
        // Comp 3: Free moved by (7, -3); position and appearance only.
        doc.layers.get_mut(free).unwrap().transform =
            glam::Affine2::from_translation(glam::Vec2::new(7.0, -3.0));
        let mut moved = LayerComp::capture("Moved", &doc.layers);
        moved.flags.visibility = false;
        let last = LayerComp::capture("Last Document State", &doc.layers);
        // Back to the document state the file is saved in.
        {
            doc.layers.get_mut(red).unwrap().visible = true;
            let b = doc.layers.get_mut(blue).unwrap();
            b.opacity = 1.0;
            b.fill_opacity = 1.0;
            b.blend_mode = BlendMode::Normal;
            b.effects = LayerEffects::default();
            doc.layers.get_mut(free).unwrap().transform = glam::Affine2::IDENTITY;
        }
        doc.extras.layer_comps = vec![all, half, moved];
        doc.extras.last_comp = Some(1);
        doc.extras.last_document_state = Some(last);
        (doc, tiles)
    }

    fn saved(doc: &Document, tiles: &MemoryTileSource) -> Vec<u8> {
        let composite = vec![0u8; (W * H * 4) as usize];
        super::super::psd_from_document(doc, tiles, &composite)
            .unwrap()
            .0
    }

    type Row = (String, bool, [f32; 6], f32, f32, BlendMode);

    /// The rows of `comp` by layer name, plates left out (a `.psd` artboard
    /// has no background layer to record).
    fn rows(doc: &Document, comp: &LayerComp) -> Vec<Row> {
        comp.layers
            .iter()
            .filter_map(|s| {
                let l = doc.layers.get(s.layer)?;
                (l.name != PLATE_NAME).then(|| {
                    (
                        l.name.clone(),
                        s.visible,
                        s.transform,
                        s.opacity,
                        s.fill_opacity,
                        s.blend_mode,
                    )
                })
            })
            .collect()
    }

    fn boards(doc: &Document) -> Vec<(String, Artboard)> {
        artboards(&doc.layers)
            .into_iter()
            .map(|(g, a)| (doc.layers.get(g).unwrap().name.clone(), a))
            .collect()
    }

    #[test]
    fn two_artboards_and_three_comps_round_trip_through_psd() {
        let (doc, tiles) = source();
        let bytes = saved(&doc, &tiles);

        // On disk: each artboard is a group record with an `artb` block and
        // no background layer; the comps are resource 1065.
        let file = psd::read(&bytes).unwrap();
        let opts = psd::ReadOptions::default();
        let groups: Vec<&psd::PsdLayer> = file.layers.iter().filter(|l| l.is_group()).collect();
        assert_eq!(groups.len(), 2);
        for g in &groups {
            let board = g.artboard(&opts).expect("an artb block").unwrap();
            assert!(board.is_valid());
            assert!(
                g.children().iter().all(|c| c.name != PLATE_NAME),
                "no plate record"
            );
        }
        let second = groups.iter().find(|g| g.name == "Artboard 2").unwrap();
        let b2 = second.artboard(&opts).unwrap().unwrap();
        assert_eq!(
            (b2.left, b2.top, b2.right, b2.bottom),
            (150.0, 0.0, 270.0, 100.0)
        );
        assert_eq!(
            b2.background,
            ArtboardBackground::Other([255.0, 128.0, 0.0])
        );
        let written = lc::layer_comps(&file.resources, &opts).unwrap().unwrap();
        assert_eq!(written.comps.len(), 3);
        // Photoshop's `Ofst` is the move from the saved position: Free is
        // moved by (7, -3) in comp 3 and the Last Document State (captured
        // after the move), every other row is unmoved.
        let free_record = file.layers.iter().find(|l| l.name == "Free").unwrap();
        let free_states = lc::layer_states(free_record, &opts).unwrap();
        assert_eq!(free_states.len(), 4, "three comps and the last state");
        let moved = [written.comps[2].id, lc::LAST_DOCUMENT_STATE_ID];
        for st in &free_states {
            let want = if moved.contains(&st.comp_id) {
                (7, -3)
            } else {
                (0, 0)
            };
            assert_eq!(st.offset, Some(want), "comp id {}", st.comp_id);
        }
        let red_record = groups
            .iter()
            .flat_map(|g| g.children())
            .find(|l| l.name == "Red")
            .unwrap();
        for st in lc::layer_states(red_record, &opts).unwrap() {
            assert_eq!(st.offset, Some((0, 0)), "Red never moves");
        }
        assert_eq!(written.last_applied, Some(written.comps[1].id));

        // Reopened: the same artboards, plate at the bottom and painted.
        let back = super::super::document_from_psd(&bytes, "back", 10).unwrap();
        let reopened = &back.imported.document;
        assert_eq!(boards(reopened), boards(&doc));
        for (group, _) in artboards(&reopened.layers) {
            let (plate, _) = artboard_of(&reopened.layers, group).unwrap();
            let LayerKind::Group(g) = &reopened.layers.get(group).unwrap().kind else {
                panic!("an artboard is a group");
            };
            assert_eq!(
                g.children.last(),
                Some(&plate),
                "the plate is the bottom child"
            );
            assert!(
                reopened.pixels.tiles(PixelKey::Layer(plate)).is_some(),
                "the plate is painted"
            );
        }
        assert!(
            !back.notes.notes().iter().any(|n| n.contains("left behind")),
            "1065 is mapped, not dropped: {:?}",
            back.notes.notes()
        );

        // The same comps: names, comments, flags, the last applied, and each
        // layer's recorded state.
        let (a, b) = (&doc.extras, &reopened.extras);
        assert_eq!(b.layer_comps.len(), 3);
        for (want, got) in a.layer_comps.iter().zip(&b.layer_comps) {
            assert_eq!(got.name, want.name);
            assert_eq!(got.comment, want.comment);
            assert_eq!(got.flags, want.flags);
            assert_eq!(rows(reopened, got), rows(&doc, want), "comp {}", want.name);
            assert_eq!(rows(reopened, got).len(), 5, "five non-plate layers");
        }
        assert_eq!(b.last_comp, Some(1));
        let (want, got) = (
            a.last_document_state.as_ref().unwrap(),
            b.last_document_state.as_ref().unwrap(),
        );
        assert_eq!(rows(reopened, got), rows(&doc, want));

        // Blue's layer style came back where it was recorded (comps 2 and
        // 3), and comp 1, recorded before it, still has none: a `cmls`
        // entry inherits what it omits, so "no style" must be written.
        let blue = find(reopened, "Blue");
        let styled = |c: &LayerComp| c.state_of(blue).unwrap().effects.drop_shadow.is_some();
        assert!(styled(&b.layer_comps[1]) && styled(&b.layer_comps[2]));
        assert!(!styled(&b.layer_comps[0]), "comp 1 has no drop shadow");

        // Comp 3 moves Free on the reopened document.
        let free = find(reopened, "Free");
        let state = b.layer_comps[2].state_of(free).unwrap();
        assert_eq!(state.transform[4..], [7.0, -3.0]);
    }
}
