//! W11-C: a `.psd`'s document-level resources on the document model, and
//! back.
//!
//! | `.psd` | this document |
//! |---|---|
//! | 1032 guides | [`editor_core::Document::guides`] |
//! | 2000–2997 saved paths, 1025 work path | path layers: shape layers with no fill and no stroke, the Paths panel's rows ([`ui::panels::paths::new_path_layer`] makes the same kind) |
//! | merged-image alpha channels named by 1006 / 1045 | [`editor_core::Document::saved_selections`] (the Channels panel's alpha channels) |
//! | 1050 slices (user and layer slices) | [`editor_core::Document::slices`], which [`crate::slices_export::restore_saved_slices`] loads into the Slice tool's store |
//!
//! The parsing and building is the `psd` crate's ([`psd::resource`]), bounded
//! there; this module only maps. A path layer goes out as a saved-path
//! resource INSTEAD of a layer record, so it does not come back doubled.

use editor_core::slices::DocumentSlice;
use editor_core::{Document, Guide, GuideAxis, Selection, SelectionMask};
use glam::{Affine2, IVec2, Vec2};
use layer_model::{BlendMode, ClippingMode, Layer, LayerKind, ShapeLayer};
use psd::resource::{self as res, AlphaChannel, PsdGuide, PsdSlice};
use psd::shape::{Knot, SubPath, VectorPath};

use super::psd_live::svg_path;
use super::{ImportError, PsdNotes};

/// A no-fill, no-stroke shape layer that carries nothing a saved path
/// cannot hold: a saved path in this model. A layer with a mask, effects,
/// clipping, a non-Normal blend, reduced opacity or fill, or hidden is a
/// layer the user styled, so it stays a layer record and keeps them.
pub(crate) fn is_path_layer(layer: &Layer) -> bool {
    matches!(&layer.kind, LayerKind::Shape(s)
        if s.fill.is_none() && s.stroke.is_none() && s.fill_paint.is_solid())
        && layer.visible
        && layer.opacity == 1.0
        && layer.fill_opacity == 1.0
        && layer.blend_mode == BlendMode::Normal
        && layer.clipping == ClippingMode::None
        && layer.mask.is_none()
        && layer.effects.is_default()
}

/// Guides, slices and alpha channels onto `document`. Returns the saved and work
/// paths, which become layers only once the layer tree is built
/// ([`push_path_layers`]).
pub(crate) fn import_resources(
    file: &psd::PsdFile,
    document: &mut Document,
    notes: &mut PsdNotes,
) -> Vec<res::SavedPath> {
    let (width, height) = (file.header.width, file.header.height);
    let guides = res::guides(&file.resources);
    if !guides.is_empty() {
        document.guides.list = guides
            .iter()
            .filter(|g| g.position.is_finite())
            .map(|g| Guide {
                axis: if g.horizontal {
                    GuideAxis::Horizontal
                } else {
                    GuideAxis::Vertical
                },
                doc: g.position as f32,
                locked: false,
            })
            .collect();
        document.guides.visible = true;
    }
    document.slices = res::slices(&file.resources, &psd::ReadOptions::default())
        .into_iter()
        .filter_map(|s| {
            let (w, h) = (s.bounds.width(), s.bounds.height());
            (w > 0 && h > 0).then(|| DocumentSlice {
                x: i64::from(s.bounds.left),
                y: i64::from(s.bounds.top),
                width: w,
                height: h,
                name: s.name,
                url: s.url,
                alt: s.alt,
            })
        })
        .collect();
    for channel in res::alpha_channels(file) {
        match SelectionMask::new(IVec2::ZERO, width, height, channel.coverage) {
            Ok(mask) => document
                .saved_selections
                .push((channel.name, Selection::Mask(mask))),
            Err(e) => notes.push(format!(
                "the alpha channel {:?} could not be kept as a saved selection: {e}",
                channel.name
            )),
        }
    }
    res::saved_paths(&file.resources, width, height)
}

/// Each saved (and the work) path as a path layer on top of the tree, the
/// first path top-most — the order [`export_resources`] numbers them in, so a
/// round trip keeps the order.
pub(crate) fn push_path_layers(
    document: &mut Document,
    paths: Vec<res::SavedPath>,
    notes: &mut PsdNotes,
) -> Result<(), ImportError> {
    for saved in paths.into_iter().rev() {
        let Some(svg) = svg_path::from_knots(&saved.path.subpaths) else {
            notes.push(format!(
                "the path {:?} has no geometry this build can draw and was not kept",
                saved.name
            ));
            continue;
        };
        let shape = ShapeLayer {
            path_svg: svg,
            fill: None,
            stroke: None,
            ..ShapeLayer::default()
        };
        document
            .layers
            .push_root(Layer::with_kind(saved.name, LayerKind::Shape(shape)))?;
    }
    Ok(())
}

/// Guides, slices, path layers (as saved paths) and saved selections (as
/// named alpha channels) onto `file`, whose merged image must already be set.
pub(crate) fn export_resources(
    document: &Document,
    file: &mut psd::PsdFile,
    notes: &mut PsdNotes,
) -> Result<(), ImportError> {
    let (width, height) = (document.width(), document.height());
    let guides = &document.guides.list;
    if !guides.is_empty() {
        let list: Vec<PsdGuide> = guides
            .iter()
            .map(|g| PsdGuide {
                horizontal: g.axis == GuideAxis::Horizontal,
                position: f64::from(g.doc),
            })
            .collect();
        file.resources.push(res::guides_resource(&list));
        if document.guides.locked || guides.iter().any(|g| g.locked) {
            notes.push("guide locks have no .psd equivalent and were not written");
        }
    }

    if !document.slices.is_empty() {
        let clamp = |v: i64| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        let list: Vec<PsdSlice> = document
            .slices
            .iter()
            .zip(1u32..)
            .map(|(s, id)| PsdSlice {
                id,
                name: s.name.clone(),
                bounds: psd::Rect::new(
                    clamp(s.x),
                    clamp(s.y),
                    clamp(s.x + i64::from(s.width)),
                    clamp(s.y + i64::from(s.height)),
                ),
                origin: 2,
                url: s.url.clone(),
                alt: s.alt.clone(),
            })
            .collect();
        file.resources.push(res::slices_resource(
            &list,
            &document.meta.title,
            width,
            height,
        ));
    }

    let mut id = res::ID_SAVED_PATH_FIRST;
    for layer_id in document.layers.iter_depth_first() {
        let Some(layer) = document.layers.get(layer_id) else {
            continue;
        };
        let LayerKind::Shape(shape) = &layer.kind else {
            continue;
        };
        if !is_path_layer(layer) {
            continue;
        }
        if id > res::ID_SAVED_PATH_LAST {
            notes.push(format!(
                "the path {:?} is past the {} saved paths a .psd holds and was not written",
                layer.name,
                res::ID_SAVED_PATH_LAST - res::ID_SAVED_PATH_FIRST + 1
            ));
            continue;
        }
        let path = vector_path(&shape.path_svg, layer.transform);
        file.resources.push(res::saved_path_resource(
            id,
            &layer.name,
            &path,
            width,
            height,
        ));
        id += 1;
    }

    // A .psd holds 56 channels: the saved selections past that room are
    // named, not written, and the save goes ahead with the rest.
    let room = res::alpha_channel_room(file);
    let (fits, left) = document
        .saved_selections
        .split_at(room.min(document.saved_selections.len()));
    if !left.is_empty() {
        let names: Vec<String> = left.iter().map(|(n, _)| format!("{n:?}")).collect();
        notes.push(format!(
            "{} saved selections are past the {room} alpha channels this .psd can hold and were not written: {}",
            left.len(),
            names.join(", ")
        ));
    }
    let channels: Vec<AlphaChannel> = fits
        .iter()
        .map(|(name, sel)| AlphaChannel {
            name: name.clone(),
            coverage: coverage(sel, width, height),
        })
        .collect();
    res::set_alpha_channels(file, &channels)?;
    Ok(())
}

/// A path layer's knots in document pixels.
fn vector_path(path_svg: &str, transform: Affine2) -> VectorPath {
    let at = |p: [f64; 2]| {
        let q = transform.transform_point2(Vec2::new(p[0] as f32, p[1] as f32));
        [f64::from(q.x), f64::from(q.y)]
    };
    VectorPath {
        subpaths: svg_path::knots(path_svg)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, knots)| !knots.is_empty())
            .map(|(closed, knots)| SubPath {
                closed,
                operation: 1,
                knots: knots
                    .into_iter()
                    .map(|k| Knot {
                        before: at(k.before),
                        anchor: at(k.anchor),
                        after: at(k.after),
                        linked: k.linked,
                    })
                    .collect(),
            })
            .collect(),
        ..VectorPath::default()
    }
}

/// A saved selection as one canvas-sized 8-bit plane.
fn coverage(sel: &Selection, width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    match sel {
        Selection::None => vec![255; w * h],
        Selection::Rect { min, max } => {
            let mut out = vec![0; w * h];
            let (x0, x1) = (min.x.clamp(0, width as i32), max.x.clamp(0, width as i32));
            let (y0, y1) = (min.y.clamp(0, height as i32), max.y.clamp(0, height as i32));
            for y in y0..y1 {
                let row = y as usize * w;
                out[row + x0 as usize..row + x1.max(x0) as usize].fill(255);
            }
            out
        }
        Selection::Mask(mask) => {
            let mut out = Vec::with_capacity(w * h);
            for y in 0..height as i32 {
                for x in 0..width as i32 {
                    out.push(mask.coverage_at(IVec2::new(x, y)));
                }
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{DocumentId, OpenDocument};
    use psd::resource::PsdSlice;

    const W: u32 = 64;
    const H: u32 = 48;

    fn triangle() -> VectorPath {
        let k = |x: f64, y: f64| Knot {
            before: [x, y],
            anchor: [x, y],
            after: [x, y],
            linked: false,
        };
        VectorPath {
            subpaths: vec![SubPath {
                closed: true,
                operation: 1,
                knots: vec![k(4.0, 4.0), k(60.0, 8.0), k(20.0, 40.0)],
            }],
            ..VectorPath::default()
        }
    }

    /// A left-half alpha channel with a soft column at x = 31.
    fn alpha() -> Vec<u8> {
        (0..W * H)
            .map(|i| match i % W {
                x if x < 31 => 255,
                31 => 128,
                _ => 0,
            })
            .collect()
    }

    /// A `.psd` another application wrote: a masked layer, three guides, a
    /// saved path, an alpha channel and two slices.
    fn source_psd() -> Vec<u8> {
        let rect = psd::Rect::sized(W, H);
        let red = [200u8, 40, 40, 255].repeat((W * H) as usize);
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(W, H));
        let mut layer = psd::PsdLayer::raster("Masked", rect);
        layer.set_rgba8(&red).unwrap();
        let ramp: Vec<u8> = (0..W * H).map(|i| (i % W * 4) as u8).collect();
        layer.mask = Some(psd::PsdMask::new(rect, ramp));
        file.layers = vec![layer];
        file.merged = Some(psd::MergedImage::from_rgba8(W, H, &red).unwrap());
        file.resources.push(res::guides_resource(&[
            PsdGuide {
                horizontal: false,
                position: 16.0,
            },
            PsdGuide {
                horizontal: true,
                position: 24.5,
            },
            PsdGuide {
                horizontal: false,
                position: 40.0,
            },
        ]));
        file.resources
            .push(res::saved_path_resource(2000, "Outline", &triangle(), W, H));
        let slice = |id, name: &str, bounds| PsdSlice {
            id,
            name: name.into(),
            bounds,
            origin: 2,
            url: String::new(),
            alt: String::new(),
        };
        file.resources.push(res::slices_resource(
            &[
                slice(1, "top", psd::Rect::new(0, 0, 64, 24)),
                slice(2, "bottom", psd::Rect::new(0, 24, 64, 48)),
            ],
            "doc",
            W,
            H,
        ));
        res::set_alpha_channels(
            &mut file,
            &[AlphaChannel {
                name: "Alpha 1".into(),
                coverage: alpha(),
            }],
        )
        .unwrap();
        psd::write(&file).unwrap()
    }

    fn expected_guides() -> Vec<Guide> {
        let g = |axis, doc| Guide {
            axis,
            doc,
            locked: false,
        };
        vec![
            g(GuideAxis::Vertical, 16.0),
            g(GuideAxis::Horizontal, 24.5),
            g(GuideAxis::Vertical, 40.0),
        ]
    }

    fn expected_slices() -> Vec<DocumentSlice> {
        let s = |y, name: &str| DocumentSlice {
            x: 0,
            y,
            width: 64,
            height: 24,
            name: name.into(),
            url: String::new(),
            alt: String::new(),
        };
        vec![s(0, "top"), s(24, "bottom")]
    }

    /// Everything the source carried that this model maps, checked on an
    /// open document: guides, slices, the path as a path layer, the alpha
    /// channel as a saved selection.
    fn assert_mapped(open: &OpenDocument) {
        let doc = &open.document;
        assert_eq!(doc.guides.list, expected_guides());
        assert!(doc.guides.visible, "imported guides are shown");
        assert_eq!(doc.slices, expected_slices());

        let paths: Vec<&Layer> = doc
            .layers
            .iter_depth_first()
            .into_iter()
            .filter_map(|id| doc.layers.get(id))
            .filter(|l| is_path_layer(l))
            .collect();
        assert_eq!(paths.len(), 1, "exactly one path layer, never doubled");
        assert_eq!(paths[0].name, "Outline");
        let LayerKind::Shape(shape) = &paths[0].kind else {
            unreachable!("is_path_layer matched a shape")
        };
        let knots = svg_path::knots(&shape.path_svg).unwrap();
        assert_eq!(knots.len(), 1);
        let anchors: Vec<[f64; 2]> = knots[0].1.iter().map(|k| k.anchor).collect();
        assert_eq!(anchors.len(), 3, "{anchors:?}");
        for (got, want) in anchors.iter().zip([[4.0, 4.0], [60.0, 8.0], [20.0, 40.0]]) {
            assert!(
                (got[0] - want[0]).abs() < 1e-3 && (got[1] - want[1]).abs() < 1e-3,
                "{anchors:?}"
            );
        }
        // Plus the one raster layer: nothing else came in.
        assert_eq!(doc.layers.iter_depth_first().len(), 2);

        assert_eq!(doc.saved_selections.len(), 1);
        let (name, sel) = &doc.saved_selections[0];
        assert_eq!(name, "Alpha 1");
        assert_eq!(coverage(sel, W, H), alpha());
    }

    fn masked_layer(doc: &Document) -> layer_model::LayerId {
        doc.layers
            .iter_depth_first()
            .into_iter()
            .find(|id| doc.layers.get(*id).is_some_and(|l| l.name == "Masked"))
            .expect("the masked layer came in")
    }

    /// W11-C: a document with 3 guides, a saved path, an alpha channel and 2
    /// slices round-trips through `.psd` by the application's own open and
    /// Save As PSD route, with nothing named as left behind.
    #[test]
    fn guides_slices_a_saved_path_and_an_alpha_channel_round_trip_through_psd() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("source.psd");
        let mut open =
            OpenDocument::open_psd_bytes(DocumentId(1), &first, &source_psd(), 10).unwrap();
        assert_mapped(&open);
        assert!(
            open.psd_notes().summary().is_none(),
            "{:?}",
            open.psd_notes()
        );

        let saved = dir.path().join("saved.psd");
        let notes = open.export_psd_to(&saved).unwrap();
        assert!(notes.summary().is_none(), "{notes:?}");

        // The written file carries the resources an independent reader
        // looks for.
        let file = psd::read(&std::fs::read(&saved).unwrap()).unwrap();
        assert_eq!(res::guides(&file.resources).len(), 3);
        assert_eq!(res::saved_paths(&file.resources, W, H).len(), 1);
        assert_eq!(res::alpha_channels(&file).len(), 1);
        let written: Vec<(String, psd::Rect)> =
            res::slices(&file.resources, &psd::ReadOptions::default())
                .into_iter()
                .map(|s| (s.name, s.bounds))
                .collect();
        assert_eq!(
            written,
            vec![
                ("top".to_string(), psd::Rect::new(0, 0, 64, 24)),
                ("bottom".to_string(), psd::Rect::new(0, 24, 64, 48)),
            ]
        );
        assert_eq!(
            file.all_layers().len(),
            1,
            "the path is a resource, not a layer"
        );

        let again = OpenDocument::open_psd(DocumentId(2), &saved, 10).unwrap();
        assert_mapped(&again);
    }

    /// W11-C: the slices a `.psd` carried reach the Slice tool's store by the
    /// editor's own open route (File > Open, then any slice action restores
    /// them), and a slice edited there is what Save As PSD writes.
    #[test]
    fn psd_slices_reach_the_slice_store_and_edits_are_written_back() {
        use crate::dialogs::ScriptedDialogs;
        use crate::editor::Editor;
        use crate::prefs::{AppPaths, Preferences};
        use crate::recent::RecentFiles;
        use crate::slices_export::{remember_committed, restore_saved_slices};
        use raster::PixelRect;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.psd");
        std::fs::write(&path, source_psd()).unwrap();
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        let id = ed.open_path(&path).unwrap();
        // W11-I: the open route itself restored them; nothing is left to do.
        assert_eq!(restore_saved_slices(&mut ed), 0);
        assert_eq!(
            ed.slices.get(id),
            &[PixelRect::new(0, 0, 64, 24), PixelRect::new(0, 24, 64, 24)]
        );
        assert_eq!(ed.slices.options(id)[0].name, "top");
        assert_eq!(ed.slices.options(id)[1].name, "bottom");

        // A set committed the way the Slice tool commits one replaces it.
        remember_committed(
            &mut ed,
            &[tools::Slice {
                rect: PixelRect::new(8, 8, 16, 16),
                name: "badge".into(),
            }],
        );
        let saved = dir.path().join("saved.psd");
        ed.active_mut().unwrap().export_psd_to(&saved).unwrap();
        let file = psd::read(&std::fs::read(&saved).unwrap()).unwrap();
        let written = res::slices(&file.resources, &psd::ReadOptions::default());
        assert_eq!(written.len(), 1, "{written:?}");
        assert_eq!(written[0].bounds, psd::Rect::new(8, 8, 24, 24));
    }

    /// W11-C round 2: a no-fill, no-stroke shape layer the user styled (here
    /// half opacity and a Multiply blend) is not taken for a saved path: it
    /// goes out as a layer record that keeps those properties.
    #[test]
    fn a_styled_empty_shape_layer_stays_a_layer_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut open = OpenDocument::open_psd_bytes(
            DocumentId(1),
            &dir.path().join("source.psd"),
            &source_psd(),
            10,
        )
        .unwrap();
        let path_id = open
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .find(|id| {
                open.document
                    .layers
                    .get(*id)
                    .is_some_and(|l| l.name == "Outline")
            })
            .unwrap();
        {
            let layer = open.document.layers.get_mut(path_id).unwrap();
            layer.opacity = 0.5;
            layer.blend_mode = layer_model::BlendMode::Multiply;
            assert!(!is_path_layer(layer));
        }
        let saved = dir.path().join("styled.psd");
        open.export_psd_to(&saved).unwrap();
        let file = psd::read(&std::fs::read(&saved).unwrap()).unwrap();
        assert!(res::saved_paths(&file.resources, W, H).is_empty());
        let record = file
            .all_layers()
            .into_iter()
            .find(|l| l.name == "Outline")
            .expect("the styled shape layer is a layer record");
        assert_eq!(record.opacity, 128);
    }

    /// W11-C round 3: a document with more saved selections than a .psd has
    /// channels for still saves: the first ones that fit are written as alpha
    /// channels and the rest are named in the notes.
    #[test]
    fn saved_selections_past_the_channel_ceiling_are_named_not_a_failed_save() {
        let dir = tempfile::tempdir().unwrap();
        let mut open = OpenDocument::open_psd_bytes(
            DocumentId(1),
            &dir.path().join("source.psd"),
            &source_psd(),
            10,
        )
        .unwrap();
        assert_eq!(open.document.saved_selections.len(), 1);
        for i in 2..=60 {
            open.document.saved_selections.push((
                format!("Sel {i}"),
                Selection::Rect {
                    min: IVec2::new(0, 0),
                    max: IVec2::new(i % 32 + 1, 8),
                },
            ));
        }
        let saved = dir.path().join("many.psd");
        let notes = open.export_psd_to(&saved).expect("the save goes ahead");
        let file = psd::read(&std::fs::read(&saved).unwrap()).unwrap();
        let written = res::alpha_channels(&file);
        assert_eq!(file.header.channels, 56);
        assert_eq!(written.len(), 52);
        assert_eq!(written[0].name, "Alpha 1");
        assert_eq!(written[51].name, "Sel 52");
        assert_eq!(
            written[51].coverage,
            coverage(&open.document.saved_selections[51].1, W, H)
        );
        let note = notes
            .notes()
            .iter()
            .find(|n| n.contains("saved selections are past"))
            .unwrap_or_else(|| panic!("{notes:?}"));
        assert!(note.starts_with("8 saved selections"), "{note}");
        for i in 53..=60 {
            assert!(
                note.contains(&format!("{:?}", format!("Sel {i}"))),
                "{note}"
            );
        }
        assert!(!note.contains("Sel 52"), "{note}");
    }

    /// W11-C: a pixel mask's density 50% and feather 4 px are written to the
    /// mask record's parameter block and come back, with no note.
    #[test]
    fn mask_density_and_feather_round_trip_through_psd() {
        let dir = tempfile::tempdir().unwrap();
        let mut open = OpenDocument::open_psd_bytes(
            DocumentId(1),
            &dir.path().join("source.psd"),
            &source_psd(),
            10,
        )
        .unwrap();
        let id = masked_layer(&open.document);
        {
            let mask = open
                .document
                .layers
                .get_mut(id)
                .unwrap()
                .mask
                .as_mut()
                .expect("the mask came in");
            mask.set_density(0.5).unwrap();
            mask.set_feather_px(4.0).unwrap();
        }
        let saved = dir.path().join("mask.psd");
        let notes = open.export_psd_to(&saved).unwrap();
        assert!(
            !notes
                .notes()
                .iter()
                .any(|n| n.contains("density or feather")),
            "{notes:?}"
        );

        let file = psd::read(&std::fs::read(&saved).unwrap()).unwrap();
        let record = file
            .all_layers()
            .into_iter()
            .find(|l| l.name == "Masked")
            .and_then(|l| l.mask.clone())
            .expect("the mask record was written");
        assert_eq!(record.density, 128, "50% as the format's byte");
        assert_eq!(record.feather_px, 4.0);

        let again = OpenDocument::open_psd(DocumentId(2), &saved, 10).unwrap();
        let layer = again
            .document
            .layers
            .get(masked_layer(&again.document))
            .unwrap();
        let mask = layer.mask.as_ref().unwrap();
        assert!(
            (mask.density() - 128.0 / 255.0).abs() < 1e-6,
            "{}",
            mask.density()
        );
        assert!((mask.feather_px() - 4.0).abs() < 1e-6);
    }
}
