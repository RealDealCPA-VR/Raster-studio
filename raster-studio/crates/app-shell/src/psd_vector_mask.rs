//! W9-G: a `.psd` layer's vector mask (`vmsk`/`vsms`) as a live
//! [`layer_model::VectorMask`], and back.
//!
//! The `psd` crate decodes the path records into knots in document pixels
//! ([`psd::shape::VectorPath`]); this module maps those knots onto
//! [`compositor::vector_mask::BezierSubpath`] and lets the compositor's
//! vector-mask geometry turn them into SVG path data and back (see
//! [`compositor::vector_mask::svg_from_subpaths`] and
//! [`compositor::vector_mask::subpaths_from_svg`] for how subpath operations
//! and multi-subpath masks are resolved). A layer whose path block belongs to
//! a *shape* layer (a path plus a solid fill, [`psd::shape::ShapeData`]) is
//! not a vector mask and is left to the shape-layer import.

use compositor::vector_mask::{BezierSubpath, Combine};
use layer_model::VectorMask;
use psd::shape::{Knot, SubPath, VectorPath};

/// `psd` subpath operation codes.
const OP_XOR: i16 = 0;
const OP_UNION: i16 = 1;
const OP_SUBTRACT: i16 = 2;
const OP_INTERSECT: i16 = 3;

/// The layer's vector-mask path, or `None` when it has none — or when its
/// path block is a shape layer's outline instead.
pub(crate) fn path_from_psd(source: &psd::PsdLayer, width: u32, height: u32) -> Option<VectorPath> {
    let opts = psd::ReadOptions::default();
    if psd::shape::ShapeData::of(source, width, height, &opts).is_some() {
        return None;
    }
    let block = source
        .extra
        .iter()
        .find(|b| &b.key == b"vsms")
        .or_else(|| source.extra.iter().find(|b| &b.key == b"vmsk"))?;
    VectorPath::decode(&block.data, width, height)
}

/// `true` when the layer carries a vector-mask path block at all.
pub(crate) fn has_path_block(source: &psd::PsdLayer) -> bool {
    source
        .extra
        .iter()
        .any(|b| &b.key == b"vsms" || &b.key == b"vmsk")
}

fn combine_of(op: i16) -> Combine {
    match op {
        OP_XOR => Combine::Xor,
        OP_SUBTRACT => Combine::Subtract,
        OP_INTERSECT => Combine::Intersect,
        _ => Combine::Union,
    }
}

fn op_of(c: Combine) -> i16 {
    match c {
        Combine::Xor => OP_XOR,
        Combine::Union => OP_UNION,
        Combine::Subtract => OP_SUBTRACT,
        Combine::Intersect => OP_INTERSECT,
    }
}

/// The path's covered region as SVG path data in document pixels.
pub(crate) fn svg_of(path: &VectorPath) -> String {
    let subs: Vec<BezierSubpath> = path
        .subpaths
        .iter()
        .map(|sp| BezierSubpath {
            closed: sp.closed,
            combine: combine_of(sp.operation),
            knots: sp
                .knots
                .iter()
                .map(|k| [k.before, k.anchor, k.after])
                .collect(),
        })
        .collect();
    compositor::vector_mask::svg_from_subpaths(&subs)
}

/// The imported vector mask: path, flags, and — when the mask record's
/// parameter block carried them — density and feather.
pub(crate) fn vector_mask_of(path: &VectorPath, record: Option<&psd::PsdMask>) -> VectorMask {
    let mut v = VectorMask::new(svg_of(path));
    v.inverted = path.invert;
    v.enabled = !path.disabled;
    if let Some(d) = record.and_then(|m| m.vector_density) {
        // `set_density` only refuses non-finite values; a byte never is.
        let _ = v.set_density(f32::from(d) / 255.0);
    }
    if let Some(f) = record.and_then(|m| m.vector_feather_px) {
        let _ = v.set_feather_px(f as f32);
    }
    v
}

/// The vector mask as `.psd` path records, its path mapped through `pose`
/// (layer space → document pixels). `None` when the path data does not
/// parse.
pub(crate) fn vector_path_of(
    v: &VectorMask,
    pose: glam::Affine2,
    not_linked: bool,
) -> Option<VectorPath> {
    let subs = compositor::vector_mask::subpaths_from_svg(&v.path_svg, pose)?;
    Some(VectorPath {
        subpaths: subs
            .into_iter()
            .map(|sp| SubPath {
                closed: sp.closed,
                operation: op_of(sp.combine),
                knots: sp
                    .knots
                    .into_iter()
                    .map(|[before, anchor, after]| Knot {
                        before,
                        anchor,
                        after,
                        linked: false,
                    })
                    .collect(),
            })
            .collect(),
        invert: v.inverted,
        not_linked,
        disabled: !v.enabled,
    })
}

/// The mask record Photoshop writes beside a `vmsk` when the layer ALSO has
/// a pixel mask: the vector's rendering (flagged "from render"), with the
/// pixel mask as the second, `real` record. A reader that follows that
/// convention takes the pixel mask from `real`; one that ignores `vmsk`
/// still sees the vector's shape. `pixel` is the pixel mask as written
/// alone; `deep` widens its samples to the 16-bit document depth (the
/// export's later widening pass covers the rendering, not `real`).
pub(crate) fn with_real_pixel_mask(
    v: &VectorMask,
    pose: glam::Affine2,
    (width, height): (u32, u32),
    pixel: psd::PsdMask,
    deep: bool,
) -> psd::PsdMask {
    let full = raster::PixelRect::new(0, 0, width, height);
    let mut coverage = compositor::vector_mask::rendering(&v.path_svg, pose, full)
        .unwrap_or_else(|| vec![0; width as usize * height as usize]);
    if v.inverted {
        coverage.iter_mut().for_each(|c| *c = 255 - *c);
    }
    let outside = if v.inverted { 255 } else { 0 };
    // Crop to the samples that differ from the default colour.
    let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0u32, 0u32);
    for y in 0..height {
        for x in 0..width {
            if coverage[(y * width + x) as usize] != outside {
                (x0, y0) = (x0.min(x), y0.min(y));
                (x1, y1) = (x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    let mut rendered = if x0 < x1 {
        let mut data = Vec::with_capacity(((x1 - x0) * (y1 - y0)) as usize);
        for y in y0..y1 {
            let row = (y * width) as usize;
            data.extend_from_slice(&coverage[row + x0 as usize..row + x1 as usize]);
        }
        let bounds = psd::Rect {
            top: y0 as i32,
            left: x0 as i32,
            bottom: y1 as i32,
            right: x1 as i32,
        };
        psd::PsdMask::new(bounds, data)
    } else {
        psd::PsdMask::new(psd::Rect::default(), Vec::new())
    };
    rendered.default_color = outside;
    rendered.from_render = true;
    let mut data = pixel.data;
    if deep {
        data = data
            .iter()
            .flat_map(|b| raster::depth::widen_sample(*b).to_be_bytes())
            .collect();
    }
    rendered.real = Some(psd::RealMask {
        bounds: pixel.bounds,
        default_color: pixel.default_color,
        relative_to_layer: pixel.relative_to_layer,
        disabled: pixel.disabled,
        invert: pixel.invert,
        data,
    });
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_triangle_survives_svg_to_records_and_back() {
        let v = VectorMask::new("M0 0 L16 0 L0 16 Z");
        let rec = vector_path_of(&v, glam::Affine2::IDENTITY, false).unwrap();
        assert_eq!(rec.subpaths.len(), 1);
        assert_eq!(rec.subpaths[0].knots.len(), 3);
        let back = vector::parse_svg(&svg_of(&rec)).unwrap();
        let b = back.bounds();
        assert_eq!((b.min.x, b.min.y, b.max.x, b.max.y), (0.0, 0.0, 16.0, 16.0));
    }

    fn triangle_psd(pixel_real: bool) -> Vec<u8> {
        let k = |x: f64, y: f64| Knot {
            before: [x, y],
            anchor: [x, y],
            after: [x, y],
            linked: false,
        };
        let path = VectorPath {
            subpaths: vec![SubPath {
                closed: true,
                operation: OP_UNION,
                knots: vec![k(0.0, 0.0), k(16.0, 0.0), k(0.0, 16.0)],
            }],
            ..Default::default()
        };
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(16, 16));
        let mut layer = psd::PsdLayer::raster("masked", psd::Rect::sized(16, 16));
        layer.set_rgba8(&[255; 16 * 16 * 4]).unwrap();
        // Photoshop's shape of it: the record holds the vector's rendering
        // (here deliberately all-hidden, so reading it as pixels would show),
        // and the vector pair rides in its parameter block.
        let mut record = psd::PsdMask::new(psd::Rect::sized(16, 16), vec![0; 256]);
        record.from_render = !pixel_real;
        record.vector_density = Some(128);
        if pixel_real {
            record.real = Some(psd::RealMask {
                bounds: psd::Rect::sized(16, 16),
                default_color: 0,
                relative_to_layer: false,
                disabled: false,
                invert: false,
                data: vec![200; 256],
            });
        }
        layer.mask = Some(record);
        layer
            .extra
            .push(psd::TaggedBlock::new(*b"vmsk", path.encode(16, 16)));
        file.layers.push(layer);
        psd::write(&file).unwrap()
    }

    fn alpha(import: &super::super::PsdImport, x: usize, y: usize) -> u8 {
        let doc = &import.imported.document;
        let rgba = compositor::composite_region(
            doc,
            &import.imported.tiles,
            raster::PixelRect::new(0, 0, 16, 16),
            0,
            compositor::CompositeOptions::default(),
        )
        .unwrap()
        .to_rgba8(&doc.meta.color_space);
        rgba[(y * 16 + x) * 4 + 3]
    }

    fn only_mask(import: &super::super::PsdImport) -> layer_model::LayerMask {
        let doc = &import.imported.document;
        let id = doc.layers.root()[0];
        doc.layers.get(id).unwrap().mask.clone().expect("a mask")
    }

    #[test]
    fn a_psd_vector_mask_imports_live_and_round_trips_as_a_path() {
        let first = super::super::document_from_psd(&triangle_psd(false), "v.psd", 10).unwrap();
        let mask = only_mask(&first);
        let v = mask
            .vector
            .clone()
            .expect("the vmsk became a live vector mask");
        assert_eq!(mask.kind, layer_model::MaskKind::Vector, "vector-only");
        assert!(
            (v.density() - 128.0 / 255.0).abs() < 1e-6,
            "{}",
            v.density()
        );
        // The record's rendering is NOT imported as pixels: outside the
        // triangle the layer shows at the density's fade, inside fully.
        assert_eq!(alpha(&first, 2, 2), 255);
        let outside = alpha(&first, 13, 13);
        assert!((126..=129).contains(&outside), "density halves: {outside}");

        // Save and reopen: still a path, same geometry, same density.
        let doc = &first.imported.document;
        let composite = compositor::composite_region(
            doc,
            &first.imported.tiles,
            raster::PixelRect::new(0, 0, 16, 16),
            0,
            compositor::CompositeOptions::default(),
        )
        .unwrap()
        .to_rgba8(&doc.meta.color_space);
        let (bytes, _) =
            super::super::psd_from_document(doc, &first.imported.tiles, &composite).unwrap();
        let file = psd::read(&bytes).unwrap();
        let block = file.layers[0]
            .extra
            .iter()
            .find(|b| &b.key == b"vmsk")
            .expect("export writes the vmsk block");
        let path = VectorPath::decode(&block.data, 16, 16).unwrap();
        assert_eq!(path.subpaths[0].knots.len(), 3);
        let again = super::super::document_from_psd(&bytes, "again.psd", 10).unwrap();
        let back = only_mask(&again).vector.expect("still a vector mask");
        assert_eq!(back.path_svg, v.path_svg, "same path after a round trip");
        assert!((back.density() - v.density()).abs() < 1e-6);
        assert_eq!(alpha(&again, 13, 13), outside);
    }

    #[test]
    fn with_a_real_record_the_pixel_mask_is_the_real_one() {
        let import = super::super::document_from_psd(&triangle_psd(true), "v.psd", 10).unwrap();
        let mask = only_mask(&import);
        assert!(mask.vector.is_some());
        assert_eq!(mask.kind, layer_model::MaskKind::Raster);
        // Inside: the real mask's 200. Outside: 200 × the vector's half fade.
        assert!((199..=201).contains(&alpha(&import, 2, 2)));
        let outside = alpha(&import, 13, 13);
        assert!((99..=101).contains(&outside), "{outside}");
        assert!(
            import.notes.is_empty(),
            "nothing dropped: {:?}",
            import.notes
        );
    }

    #[test]
    fn export_with_both_masks_writes_the_pixel_mask_as_the_real_record() {
        let first = super::super::document_from_psd(&triangle_psd(true), "v.psd", 10).unwrap();
        let doc = &first.imported.document;
        let composite = compositor::composite_region(
            doc,
            &first.imported.tiles,
            raster::PixelRect::new(0, 0, 16, 16),
            0,
            compositor::CompositeOptions::default(),
        )
        .unwrap()
        .to_rgba8(&doc.meta.color_space);
        let (bytes, _) =
            super::super::psd_from_document(doc, &first.imported.tiles, &composite).unwrap();
        let file = psd::read(&bytes).unwrap();
        let record = file.layers[0].mask.as_ref().expect("a mask record");
        // Photoshop's convention: the first record is the vector's
        // rendering, the pixel mask is the `real` one.
        assert!(record.from_render, "the first record is the rendering");
        let real = record.real.as_ref().expect("the pixel mask is `real`");
        assert!(
            !real.data.is_empty() && real.data.iter().all(|&b| b == 200),
            "the real record carries the pixel mask's coverage"
        );
        // The rendering is the triangle: inside covered, outside not.
        let w = record.bounds.width() as usize;
        let at = |x: i32, y: i32| {
            let (bx, by) = (x - record.bounds.left, y - record.bounds.top);
            if bx < 0 || by < 0 || bx >= w as i32 || by >= record.bounds.height() as i32 {
                record.default_color
            } else {
                record.data[by as usize * w + bx as usize]
            }
        };
        assert_eq!(at(2, 2), 255);
        assert_eq!(at(13, 13), 0);
        // And this build's own reader gives back both masks, same pixels.
        let again = super::super::document_from_psd(&bytes, "again.psd", 10).unwrap();
        let mask = only_mask(&again);
        assert!(mask.vector.is_some(), "still a vector mask");
        assert_eq!(mask.kind, layer_model::MaskKind::Raster, "and a pixel mask");
        assert_eq!(alpha(&again, 2, 2), alpha(&first, 2, 2));
        assert_eq!(alpha(&again, 13, 13), alpha(&first, 13, 13));
    }

    #[test]
    fn a_subtracted_subpath_cuts_a_hole() {
        let outer = SubPath {
            closed: true,
            operation: OP_UNION,
            knots: vec![],
        };
        let k = |x: f64, y: f64| Knot {
            before: [x, y],
            anchor: [x, y],
            after: [x, y],
            linked: false,
        };
        let square = |a: f64, b: f64, op: i16| SubPath {
            operation: op,
            knots: vec![k(a, a), k(b, a), k(b, b), k(a, b)],
            ..outer.clone()
        };
        let path = VectorPath {
            subpaths: vec![square(0.0, 10.0, OP_UNION), square(3.0, 7.0, OP_SUBTRACT)],
            ..Default::default()
        };
        let p = vector::parse_svg(&svg_of(&path)).unwrap();
        let mask = vector::fill(&p, &vector::FillOptions::default()).unwrap();
        assert_eq!(mask.coverage_at(glam::IVec2::new(1, 1)), 255);
        assert_eq!(mask.coverage_at(glam::IVec2::new(5, 5)), 0, "the hole");
    }
}
