//! Card 046: the reusable full-source placement builder.
//!
//! Placing a decoded source onto an existing document differs from *opening*
//! one (`import::import_command`): the canvas is already there and may be
//! smaller than the source, the placement may carry an offset (including a
//! negative one), and **every decoded pixel must survive into the stored
//! tiles** — including the ones that land outside the canvas. The tile store
//! is unbounded, so off-canvas content costs storage, not correctness; the
//! compositor reads what exists and the canvas clips only what it draws.
//!
//! The result retains the source's own origin and dimensions explicitly, so
//! transparent margins cannot make the intended size unknowable later. Any
//! origin is honored exactly: placed tiles are re-sliced from the source at
//! arbitrary phase offsets, not shifted whole.
//!
//! # Allocation discipline
//!
//! Nothing here allocates a canvas-sized copy: the source's bytes arrive
//! already decoded (the bounded decode happened in the codec), the grid
//! slices them into content-addressed tiles (`MemoryTileSource::insert_bytes`
//! deduplicates by content hash), and malformed dimensions are refused
//! before any tile is built.

use crate::import::{DecodedImage, ImportError};
use editor_core::{Command, PixelTarget, TileDelta, TileEdit};
use glam::IVec2;
use layer_model::{Layer, LayerId};
use raster::{PixelRect, TileCoord, TILE_SIZE};

/// A validated, fully-tiled placement of one decoded source onto a document.
#[derive(Debug)]
pub struct Placement {
    /// The layer the placement created (the command inserts it).
    pub layer: LayerId,
    /// Card 047: the color space the SOURCE pixels were encoded in — the
    /// provenance of the conversion the placement performed.
    pub source_color_space: color::ColorSpace,
    /// Card 047: the source's raw embedded ICC profile bytes, retained
    /// verbatim even after the pixels were converted.
    pub embedded_profile: Option<Vec<u8>>,
    /// Card 047: the layer transform that realizes the fit (None for a raw
    /// placement at scale 1).
    pub fit_transform: Option<glam::Affine2>,
    /// The command to apply: create the layer, then paint every source tile
    /// at its placed position — one undoable transaction.
    pub command: Command,
    /// Where the source's (0,0) pixel lands in document space. May be
    /// negative or beyond the canvas: off-canvas content stays stored.
    pub source_origin: IVec2,
    /// The source's own full dimensions — not the placed clip, not the
    /// canvas, and not the transparent-cropped extent.
    pub source_size: (u32, u32),
}

/// Validate and tile `image`, returning the placement that puts its (0,0)
/// pixel at `origin` on whatever canvas receives the command.
///
/// Every source tile is stored, wherever it lands. The canvas never enters
/// the math: the tile store holds off-canvas tiles, and the placed layer's
/// pixels are exactly the source's.
impl Placement {
    /// Retype the placed layer, keeping its identity: the paint edits and
    /// any fit transform already reference this layer id, so a caller that
    /// needs a smart object (or any other kind) rewrites the create step in
    /// place rather than rebuilding the whole placement.
    pub fn with_layer_kind(mut self, kind: layer_model::LayerKind) -> Self {
        let target = self.layer;
        self.command = match self.command {
            Command::Transaction { label, commands } => Command::Transaction {
                label,
                commands: commands
                    .into_iter()
                    .map(|c| match c {
                        Command::CreateLayer { layer } if layer.id == target => {
                            let mut layer = *layer;
                            layer.kind = kind.clone();
                            Command::CreateLayer {
                                layer: Box::new(layer),
                            }
                        }
                        other => other,
                    })
                    .collect(),
            },
            other => other,
        };
        self
    }
}

/// Card 050: convert a decoded source into the document's working color
/// space through the existing profile handling. Refuses an unsupported
/// profile on EITHER side instead of silently falling back; the returned
/// image's `color_space` names the working space while the CALLER keeps the
/// original for provenance.
pub(crate) fn working_space_pixels(
    image: &DecodedImage,
    working: &color::ColorSpace,
) -> Result<DecodedImage, ImportError> {
    if !image.color_space.is_transform_supported() {
        return Err(ImportError::UnsupportedColorProfile {
            name: image.color_space.name(),
        });
    }
    if !working.is_transform_supported() {
        return Err(ImportError::UnsupportedColorProfile {
            name: working.name(),
        });
    }
    if image.width == 0 || image.height == 0 {
        return Err(ImportError::EmptyImage {
            width: image.width,
            height: image.height,
        });
    }
    let expected = (image.width as usize)
        .saturating_mul(image.height as usize)
        .saturating_mul(4);
    if image.rgba8.len() != expected {
        return Err(ImportError::PixelCount {
            expected,
            found: image.rgba8.len(),
        });
    }
    if image.color_space == *working {
        return Ok(DecodedImage {
            color_space: working.clone(),
            ..image.clone()
        });
    }
    let mut converted = image.rgba8.clone();
    for px in converted.chunks_exact_mut(4) {
        let rgb = [
            px[0] as f32 / 255.0,
            px[1] as f32 / 255.0,
            px[2] as f32 / 255.0,
        ];
        // Both spaces were gated transformable above - the infallible entry
        // points cannot fall back here.
        let linear = color::to_linear(&image.color_space, rgb);
        let out = color::from_linear(working, linear);
        px[0] = (out[0] * 255.0).round().clamp(0.0, 255.0) as u8;
        px[1] = (out[1] * 255.0).round().clamp(0.0, 255.0) as u8;
        px[2] = (out[2] * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    Ok(DecodedImage {
        width: image.width,
        height: image.height,
        rgba8: converted,
        color_space: working.clone(),
        icc_profile: image.icc_profile.clone(),
    })
}

/// Card 050: slice a decoded source into its ink tiles PLACED at `origin` -
/// the SAME re-slicing the builder uses, shared so the linked refresh and
/// the placement cannot drift. The per-pixel mapping is
/// `source = placed_tile_pixel - origin`, so an arbitrary origin (negative,
/// non-tile-multiple) places pixels exactly. Fully transparent tiles are
/// skipped (absent tile = transparent to the compositor).
pub(crate) fn slice_source_tiles(image: &DecodedImage, origin: IVec2) -> Vec<(TileCoord, Vec<u8>)> {
    let (w, h) = (image.width as i32, image.height as i32);
    let px_origin = IVec2::new(
        origin.x.div_euclid(TILE_SIZE as i32),
        origin.y.div_euclid(TILE_SIZE as i32),
    );
    let px_last = IVec2::new(
        (origin.x + image.width as i32 - 1).div_euclid(TILE_SIZE as i32),
        (origin.y + image.height as i32 - 1).div_euclid(TILE_SIZE as i32),
    );
    let mut out = Vec::new();
    for ty in px_origin.y..=px_last.y {
        for tx in px_origin.x..=px_last.x {
            let mut bytes = vec![0u8; (TILE_SIZE * TILE_SIZE * 4) as usize];
            let mut any = false;
            for ly in 0..TILE_SIZE as i32 {
                let sy = ty * TILE_SIZE as i32 + ly - origin.y;
                if sy < 0 || sy >= h {
                    continue;
                }
                for lx in 0..TILE_SIZE as i32 {
                    let sx = tx * TILE_SIZE as i32 + lx - origin.x;
                    if sx < 0 || sx >= w {
                        continue;
                    }
                    let src = (((sy as i64) * w as i64 + sx as i64) * 4) as usize;
                    let dst = ((ly * TILE_SIZE as i32 + lx) * 4) as usize;
                    let rgba = &image.rgba8[src..src + 4];
                    if rgba[3] != 0 {
                        any = true;
                    }
                    bytes[dst..dst + 4].copy_from_slice(rgba);
                }
            }
            if any {
                out.push((TileCoord::new(tx, ty, 0), bytes));
            }
        }
    }
    out
}

pub fn place_source(
    image: &DecodedImage,
    name: &str,
    origin: IVec2,
    tiles: &mut compositor::MemoryTileSource,
) -> Result<Placement, ImportError> {
    // Malformed dimensions are refused before any allocation beyond the
    // caller's own decode.
    if image.width == 0 || image.height == 0 {
        return Err(ImportError::EmptyImage {
            width: image.width,
            height: image.height,
        });
    }
    let expected = (image.width as usize)
        .saturating_mul(image.height as usize)
        .saturating_mul(4);
    if image.rgba8.len() != expected {
        return Err(ImportError::PixelCount {
            expected,
            found: image.rgba8.len(),
        });
    }

    let layer = Layer::raster(name);
    let layer_id = layer.id;

    let mut edits = Vec::new();
    for (placed, bytes) in slice_source_tiles(image, origin) {
        let hash = tiles.insert_bytes(bytes);
        edits.push(TileEdit::set(placed, hash));
    }

    let paint = Command::PaintTiles {
        target: PixelTarget::Layer(layer_id),
        delta: TileDelta::new(edits).map_err(editor_core::CommandError::from)?,
    };
    let command = Command::Transaction {
        label: format!("Place {name}"),
        commands: vec![Command::create_layer(layer), paint],
    };
    Ok(Placement {
        layer: layer_id,
        source_color_space: image.color_space.clone(),
        embedded_profile: image.icc_profile.clone(),
        fit_transform: None,
        command,
        source_origin: origin,
        source_size: (image.width, image.height),
    })
}

pub fn place_source_fit(
    image: &DecodedImage,
    name: &str,
    canvas: PixelRect,
    working: &color::ColorSpace,
    allow_upscale: bool,
    tiles: &mut compositor::MemoryTileSource,
) -> Result<Placement, ImportError> {
    // Both profiles must be transformable BEFORE any pixel work: an
    // unsupported profile is the caller's problem to surface, not ours to
    // paper over — on EITHER side of the transform.
    if !image.color_space.is_transform_supported() {
        return Err(ImportError::UnsupportedColorProfile {
            name: image.color_space.name(),
        });
    }
    if !working.is_transform_supported() {
        return Err(ImportError::UnsupportedColorProfile {
            name: working.name(),
        });
    }
    if canvas.width == 0 || canvas.height == 0 {
        return Err(ImportError::EmptyImage {
            width: canvas.width,
            height: canvas.height,
        });
    }
    if image.width == 0 || image.height == 0 {
        return Err(ImportError::EmptyImage {
            width: image.width,
            height: image.height,
        });
    }

    // Aspect-preserving fit, centered. Smaller assets are NOT enlarged
    // unless the caller says so: scale = min(fit, 1) by default.
    let fit =
        (canvas.width as f32 / image.width as f32).min(canvas.height as f32 / image.height as f32);
    let scale = if allow_upscale { fit } else { fit.min(1.0) };
    let placed_w = image.width as f32 * scale;
    let placed_h = image.height as f32 * scale;
    let offset_x = (canvas.width as f32 - placed_w) / 2.0;
    let offset_y = (canvas.height as f32 - placed_h) / 2.0;

    // Convert the decoded pixels into the working space, in place of a
    // copy. Alpha is color-agnostic and rides unchanged.
    let mut converted = image.rgba8.clone();
    if image.color_space != *working {
        for px in converted.chunks_exact_mut(4) {
            let rgb = [
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
            ];
            let linear = color::try_to_linear(&image.color_space, rgb).map_err(|_| {
                ImportError::UnsupportedColorProfile {
                    name: image.color_space.name(),
                }
            })?;
            let out = color::from_linear(working, linear);
            px[0] = (out[0] * 255.0).round().clamp(0.0, 255.0) as u8;
            px[1] = (out[1] * 255.0).round().clamp(0.0, 255.0) as u8;
            px[2] = (out[2] * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    let converted_image = DecodedImage {
        width: image.width,
        height: image.height,
        rgba8: converted,
        // The pixels are NOW in the working space — the provenance fields
        // below keep the source's own space and profile.
        color_space: working.clone(),
        icc_profile: image.icc_profile.clone(),
    };

    // The tiles stay full-source at the origin; the fit is the layer
    // transform applied after they land.
    let mut placement = place_source(&converted_image, name, IVec2::ZERO, tiles)?;
    // The provenance is the SOURCE's, not the converted pixels'.
    placement.source_color_space = image.color_space.clone();
    placement.embedded_profile = image.icc_profile.clone();
    let fit_transform = glam::Affine2::from_scale_angle_translation(
        glam::Vec2::splat(scale),
        0.0,
        glam::Vec2::new(offset_x, offset_y),
    );
    // A source that already fills the canvas at scale 1 needs no transform
    // command — the placement stays as clean as a raw place.
    let identity = (scale - 1.0).abs() < 1e-6 && offset_x.abs() < 1e-3 && offset_y.abs() < 1e-3;
    if !identity {
        let transform = Command::TransformLayer {
            layer_id: placement.layer,
            matrix: fit_transform.to_cols_array(),
        };
        placement.command = match placement.command {
            Command::Transaction { label, commands } => Command::Transaction {
                label,
                commands: {
                    let mut c = commands;
                    c.push(transform);
                    c
                },
            },
            other => other,
        };
        placement.fit_transform = Some(fit_transform);
    }
    Ok(placement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::PixelKey;

    /// A 600×500 source with opaque ink only at the far right and bottom —
    /// the transparent padding must not hide where the content is.
    fn asymmetric_source() -> DecodedImage {
        let (w, h) = (600u32, 500u32);
        let mut rgba8 = vec![0u8; (w * h * 4) as usize];
        for y in 480..500u32 {
            for x in 560..600u32 {
                let i = ((y * w + x) * 4) as usize;
                rgba8[i..i + 4].copy_from_slice(&[200, 30, 30, 255]);
            }
        }
        DecodedImage {
            width: w,
            height: h,
            rgba8,
            color_space: Default::default(),
            icc_profile: None,
        }
    }

    #[test]
    fn a_large_asymmetric_source_keeps_its_far_content_in_stored_tiles() {
        // Card 046's done-check: the source is 600×500, the canvas is 64×64 —
        // the far-right and bottom tiles still hold the ink.
        let mut tiles = compositor::MemoryTileSource::new();
        let placement =
            place_source(&asymmetric_source(), "Wide", IVec2::new(0, 0), &mut tiles).unwrap();
        assert_eq!(placement.source_size, (600, 500));
        assert_eq!(placement.source_origin, IVec2::new(0, 0));

        // Apply the command to a tiny document and read the stored tiles.
        let mut doc = editor_core::Document::new(64, 64, "tiny");
        let mut history = editor_core::History::new();
        history.apply(&mut doc, placement.command.clone()).unwrap();
        let map = doc.pixels.tiles(PixelKey::Layer(placement.layer)).unwrap();
        // The ink's tiles: source tile (2,1) covers 512..768 × 256..512 —
        // placed at (0,0) those coords are unchanged and hold the corner ink.
        let far = TileCoord::new(2, 1, 0);
        let hash = map.get(far).unwrap_or_else(|| {
            panic!(
                "the far-right/bottom tile is stored: {:?}",
                map.iter().map(|(c, _)| c).collect::<Vec<_>>()
            )
        });
        use compositor::TileSource;
        let bytes = tiles.tile(hash).unwrap();
        // The ink inside that tile sits at source (560..600, 480..500) —
        // tile-local (48..88, 224..244) clamped to the 256 tile.
        let px = |x: u32, y: u32| {
            let i = ((y * TILE_SIZE + x) * 4) as usize;
            [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
        };
        assert_eq!(px(50, 230)[3], 255, "the far ink survived");
        assert_eq!(px(0, 0)[3], 0, "the transparent padding stayed transparent");
    }

    #[test]
    fn an_arbitrary_origin_places_pixels_exactly_through_re_sliced_tiles() {
        // The critical case an earlier draft got wrong: a non-tile-multiple
        // origin. The ink lands at doc (260..300, 230..250) — tile (1,0),
        // tile-local (4..44, 230..250) — and the re-slice must put it there
        // exactly, not shifted by the phase offset.
        let mut tiles = compositor::MemoryTileSource::new();
        let placement = place_source(
            &asymmetric_source(),
            "Shifted",
            IVec2::new(-300, -250),
            &mut tiles,
        )
        .unwrap();
        assert_eq!(placement.source_origin, IVec2::new(-300, -250));
        assert_eq!(placement.source_size, (600, 500));
        let mut doc = editor_core::Document::new(64, 64, "tiny");
        let mut history = editor_core::History::new();
        history.apply(&mut doc, placement.command).unwrap();
        let map = doc.pixels.tiles(PixelKey::Layer(placement.layer)).unwrap();
        let hash = map
            .get(TileCoord::new(1, 0, 0))
            .unwrap_or_else(|| panic!("the ink's placed tile is stored"));
        use compositor::TileSource;
        let bytes = tiles.tile(hash).unwrap();
        let px = |x: u32, y: u32| {
            let i = ((y * TILE_SIZE + x) * 4) as usize;
            [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
        };
        assert_eq!(px(10, 240)[3], 255, "the ink sits exactly where it belongs");
        assert_eq!(px(2, 240)[3], 0, "the pre-ink phase stays transparent");
        assert_eq!(px(46, 240)[3], 0, "the post-ink phase stays transparent");
    }

    #[test]
    fn malformed_sources_are_refused_before_any_allocation() {
        let mut tiles = compositor::MemoryTileSource::new();
        let mut bad = asymmetric_source();
        bad.width = 0;
        assert!(matches!(
            place_source(&bad, "Zero", IVec2::ZERO, &mut tiles),
            Err(ImportError::EmptyImage { width: 0, .. })
        ));
        let mut short = asymmetric_source();
        short.rgba8.truncate(short.rgba8.len() - 4);
        assert!(matches!(
            place_source(&short, "Short", IVec2::ZERO, &mut tiles),
            Err(ImportError::PixelCount { .. })
        ));
    }
}

#[cfg(test)]
mod fit_tests {
    use super::*;
    use editor_core::{History, PixelKey};

    /// A 400x300 source with four labeled (distinct-colored) 40px corners.
    fn corner_source() -> DecodedImage {
        let (w, h) = (400u32, 300u32);
        let mut rgba8 = vec![0u8; (w * h * 4) as usize];
        let corners = [
            (0u32, 0u32, [220u8, 30, 30, 255]),
            (360, 0, [30, 220, 30, 255]),
            (0, 260, [30, 30, 220, 255]),
            (360, 260, [220, 220, 30, 255]),
        ];
        for (x0, y0, color) in corners {
            for y in y0..y0 + 40 {
                for x in x0..x0 + 40 {
                    let i = ((y * w + x) * 4) as usize;
                    rgba8[i..i + 4].copy_from_slice(&color);
                }
            }
        }
        DecodedImage {
            width: w,
            height: h,
            rgba8,
            color_space: color::ColorSpace::Srgb,
            icc_profile: None,
        }
    }

    #[test]
    fn the_four_labeled_corners_survive_the_fit_and_the_detail_stays_full_resolution() {
        // Card 047's done-check: fit a 400x300 source into 100x100 - the
        // four corners are visible in the composite at their scaled spots,
        // the fit is a layer transform (scale 0.25, centered), and the
        // STORED tiles keep the full-resolution detail (scaling back up
        // later exposes it - nothing was resampled).
        let mut tiles = compositor::MemoryTileSource::new();
        let placement = place_source_fit(
            &corner_source(),
            "Corners",
            PixelRect::new(0, 0, 100, 100),
            &color::ColorSpace::Srgb,
            false,
            &mut tiles,
        )
        .unwrap();
        let fit = placement.fit_transform.unwrap();
        assert!(
            (fit.matrix2.x_axis.x - 0.25).abs() < 1e-4,
            "scale 0.25: {fit:?}"
        );
        // 100x75 placed, centered vertically: offset (0, 12.5).
        assert!((fit.translation.y - 12.5).abs() < 1e-3, "centered: {fit:?}");

        let mut doc = editor_core::Document::new(100, 100, "tiny");
        let mut history = History::new();
        history.apply(&mut doc, placement.command).unwrap();
        let mut canvas_buf = [0u8; (100 * 100 * 4) as usize];
        // Composite through the tile store via the compositor facade.
        {
            let composite = compositor::composite_rect(
                &doc,
                &tiles,
                PixelRect::new(0, 0, 100, 100),
                0,
                compositor::CompositeOptions::default(),
            )
            .unwrap();
            for (i, px) in composite.pixels().iter().enumerate() {
                for (dst, src) in canvas_buf[i * 4..i * 4 + 4].iter_mut().zip(px.iter()) {
                    *dst = (src.clamp(0.0, 1.0) * 255.0) as u8;
                }
            }
        }
        let at = |x: usize, y: usize| {
            let i = (y * 100 + x) * 4;
            [
                canvas_buf[i],
                canvas_buf[i + 1],
                canvas_buf[i + 2],
                canvas_buf[i + 3],
            ]
        };
        // Top-left corner ink (220,30,30) scaled into the top-left band.
        let c = at(5, 17);
        assert!(c[0] > 120 && c[1] < 90, "the red corner shows: {c:?}");
        // Top-right (30,220,30) at the mirrored spot.
        let c = at(95, 17);
        assert!(c[1] > 120 && c[0] < 90, "the green corner shows: {c:?}");
        // Bottom-left (30,30,220) at the bottom-left spot (the placed
        // content spans y 12.5..87.5).
        let c = at(5, 85);
        assert!(c[2] > 120 && c[0] < 90, "the blue corner shows: {c:?}");
        // Bottom-right (220,220,30) at the bottom-right spot.
        let c = at(95, 85);
        assert!(
            c[0] > 120 && c[1] > 120 && c[2] < 90,
            "the yellow corner shows: {c:?}"
        );

        // The detail is full-resolution: the stored tile set covers the
        // whole source (400x300 needs tiles (0..2, 0..2)) and the layer
        // transform is the ONLY thing that scales.
        let map = doc.pixels.tiles(PixelKey::Layer(placement.layer)).unwrap();
        assert!(
            map.get(TileCoord::new(1, 1, 0)).is_some(),
            "the source's own full-resolution tiles are stored"
        );
    }

    #[test]
    fn a_smaller_asset_is_not_enlarged_by_default() {
        let mut tiny = corner_source();
        tiny.width = 50;
        tiny.height = 40;
        tiny.rgba8 = vec![0u8; (50 * 40 * 4) as usize];
        let mut tiles = compositor::MemoryTileSource::new();
        let placement = place_source_fit(
            &tiny,
            "Small",
            PixelRect::new(0, 0, 200, 200),
            &color::ColorSpace::Srgb,
            false,
            &mut tiles,
        )
        .unwrap();
        let fit = placement.fit_transform.unwrap();
        assert!(
            (fit.matrix2.x_axis.x - 1.0).abs() < 1e-6,
            "no enlargement: {fit:?}"
        );
        // Centered: offset (75, 80).
        assert!((fit.translation.x - 75.0).abs() < 1e-3, "centered: {fit:?}");
    }

    #[test]
    fn a_tagged_source_converts_into_the_working_space() {
        // Display P3 source into a Linear sRGB working space: the stored
        // pixel must equal the reference conversion through the color
        // crate's own transforms.
        let mut image = corner_source();
        // A saturated red patch in P3.
        image.color_space = color::ColorSpace::DisplayP3;
        for y in 0..40u32 {
            for x in 0..40u32 {
                let i = ((y * image.width + x) * 4) as usize;
                image.rgba8[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let mut tiles = compositor::MemoryTileSource::new();
        let placement = place_source_fit(
            &image,
            "Tagged",
            PixelRect::new(0, 0, 100, 100),
            &color::ColorSpace::LinearSrgb,
            false,
            &mut tiles,
        )
        .unwrap();
        assert_eq!(
            placement.source_color_space,
            color::ColorSpace::DisplayP3,
            "the source space rides on the result"
        );
        let mut doc = editor_core::Document::new(100, 100, "tiny");
        let mut history = History::new();
        history.apply(&mut doc, placement.command).unwrap();
        let map = doc.pixels.tiles(PixelKey::Layer(placement.layer)).unwrap();
        let hash = map.get(TileCoord::new(0, 0, 0)).expect("the corner tile");
        use compositor::TileSource;
        let bytes = tiles.tile(hash).unwrap();
        // Reference: the agreed conversion P3 -> linear, encoded as u8 (the
        // working space is Linear sRGB, whose encode is the identity).
        let linear = color::to_linear(&color::ColorSpace::DisplayP3, [1.0, 0.0, 0.0]);
        let got = [bytes[0], bytes[1], bytes[2]];
        for (i, e) in linear.iter().enumerate() {
            let e8 = (e * 255.0).round().clamp(0.0, 255.0) as u8;
            assert!(
                (got[i] as i32 - e8 as i32).abs() <= 1,
                "the stored pixel matches the reference conversion: {got:?} vs {e8}"
            );
        }
    }

    #[test]
    fn an_unsupported_profile_is_an_error_not_silent_srgb() {
        let mut image = corner_source();
        image.color_space = color::ColorSpace::IccProfile {
            asset_hash: "garbage".to_string(),
            profile: vec![0u8; 32],
        };
        let mut tiles = compositor::MemoryTileSource::new();
        let err = place_source_fit(
            &image,
            "Bad",
            PixelRect::new(0, 0, 100, 100),
            &color::ColorSpace::Srgb,
            false,
            &mut tiles,
        )
        .unwrap_err();
        assert!(
            matches!(err, ImportError::UnsupportedColorProfile { .. }),
            "an unsupported profile is refused, not silently sRGB: {err:?}"
        );
    }
}
