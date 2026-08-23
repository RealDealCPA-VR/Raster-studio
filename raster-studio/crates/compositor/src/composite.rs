//! The traversal: turning a layer tree into pixels.
//!
//! # Order
//!
//! Siblings are visited **bottom-up** (a [`layer_model::LayerTree`] sibling list
//! is top-most first, so the walk runs it in reverse) and groups recurse
//! depth-first. Each layer is blended into the accumulator that already holds
//! everything beneath it, which is what makes "the backdrop" a real thing that
//! an adjustment layer or a `Multiply` can read.
//!
//! # Why groups are not flattened
//!
//! A [`GroupBlending::Isolated`] group composites its children into its *own*
//! transparent buffer and only then blends that buffer down with the group's
//! opacity and blend mode. Applying the group's opacity to each child instead
//! gives a different picture whenever two children overlap — the overlap gets
//! the opacity applied twice. `group_opacity_is_not_per_child_opacity` is the
//! executable version of that sentence.
//!
//! [`GroupBlending::PassThrough`] is the other mode: the children blend
//! directly against whatever is under the group, so an adjustment layer inside
//! reaches the document beneath. It is honoured only when the group's blend
//! mode is `Normal`, its transform is the identity, and nothing is clipped to
//! it. A pass-through group is by definition *not* a separate buffer, and each
//! of those three things needs one to act on, so such a group composites as
//! isolated instead — which is also what [`GroupBlending::Isolated`]'s own docs
//! require of a non-`Normal` group.
//!
//! # Clipping groups
//!
//! A run of [`ClippingMode::ClipToBelow`] layers plus the non-clipping layer
//! beneath them is composited as one unit:
//!
//! 1. the base layer's own content is rendered (its pixels × its mask), giving
//!    a buffer whose alpha is the base's **shape**;
//! 2. each clipped layer is composited **atop** that buffer — Porter-Duff
//!    `atop`, so a clipped layer can recolour the base but never extend it;
//! 3. the finished buffer is blended down with the **base's** blend mode,
//!    opacity and clip, which is what "the base layer's blending options apply
//!    to the whole clipping group" means.
//!
//! Three consequences fall out and each is tested: a hidden base hides the
//! whole group, a clipping group's alpha is exactly the base's alpha, and a
//! clipped adjustment layer adjusts only the base rather than the document
//! beneath it. A clipping flag with no layer beneath it to clip to has no
//! effect, which is exactly what [`layer_model::LayerTree::clipping_group`]
//! reports for the same arrangement.
//!
//! # Adjustment layers
//!
//! An adjustment layer never contributes pixels. It rewrites the accumulator
//! that is already there — everything beneath it in its own scope, nothing
//! above it — weighted by its opacity, its fill opacity and its mask. Alpha is
//! left alone: an adjustment re-colours a backdrop, it does not reshape it.
//!
//! Because it has no pixels of its own, an adjustment layer's transform can
//! only move one thing: a **linked** mask, which travels with the layer exactly
//! as a raster layer's content does. `Ctx::adjustment_coverage` is that path,
//! and it resamples the mask through the same bilinear resampler
//! `render_source` uses, so the two agree. An unlinked mask stays in document
//! space, transform or not.
//!
//! # Region independence
//!
//! Everything here is either pointwise or reads a bounded neighbourhood of
//! **stored** data (a mask's feather, a transformed layer's pre-image). Nothing
//! reads a neighbouring pixel of an intermediate result. That is what makes
//! compositing a region equal to the same sub-rect of compositing everything,
//! and it is why the tile-parallel path can hand each tile to a different
//! thread without a seam.
//!
//! # Sampling a transformed layer without unbounded allocation
//!
//! A minified layer is the awkward case: the pre-image of a 256×256 tile under
//! a 1:50 scale is 12800×12800, and a naive implementation allocates that as
//! `f32` — per tile, on every worker of the rayon pool — or refuses the frame
//! outright. Neither is acceptable for an action as ordinary as dragging a
//! placed image small, so three bounds apply, in this order, and none of them
//! changes a pixel:
//!
//! 1. **What exists.** The pre-image is intersected with `Ctx::content_bounds`
//!    — the extent of the layer's *stored* tiles, or for a group the union of
//!    its children's extents mapped forward through their transforms. Outside
//!    that extent a layer is transparent by definition, so sampling it is
//!    provably the same as not sampling it.
//! 2. **Split the destination.** A pre-image's area is the destination's area
//!    divided by the transform's determinant, so halving the destination rect
//!    halves the pre-image. When the bounded pre-image is still larger than
//!    [`MAX_PREIMAGE_PIXELS`] the destination is split and the halves are
//!    composited separately — exact, by the region independence above, and the
//!    same total work.
//! 3. **A single destination pixel.** Splitting stops at 1×1, where the
//!    bilinear resampler reads exactly the 2×2 samples around the point that
//!    pixel maps back to — the centre of its pre-image. A window of
//!    [`TAP_WINDOW`] pixels there holds every sample that can be read, so
//!    clamping to it is exact as well.
//!
//! The one thing genuinely given up is a *feather* so large that its sampled
//! rect could not be allocated: `Ctx::mask_sample` scales the blur radii down
//! to what fits rather than failing the frame. It takes a feather of thousands
//! of pixels to reach, and the clamp is applied to the cache key too, so the
//! answer stays region-independent and cacheable.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use glam::{Affine2, Vec2};
use rayon::prelude::*;

use color::{to_linear, unpremultiply, ColorSpace};
use editor_core::{Document, PixelKey};
use layer_model::{
    dissolve_keeps_source, BlendMode, ClippingMode, GroupBlending, GroupLayer, Layer, LayerId,
    LayerKind, LayerMask, MaskId, MaskKind,
};
use raster::mipmap::{level_count, level_dimensions};
use raster::{PixelFormat, PixelRect, Tile, TileCoord, TileGrid, TILE_SIZE};

use crate::adjust::PreparedAdjustment;
use crate::blending::{blend_atop, blend_over, dissolve_noise, BlendContext, BlendSpace};
use crate::canvas::Canvas;
use crate::error::CompositeError;
use crate::source::TileSource;

/// Largest pre-image buffer the compositor allocates for one transformed
/// layer: 2^22 pixels, 64 MiB of RGBA `f32`.
///
/// Well under [`crate::MAX_CANVAS_PIXELS`] on purpose. A pre-image is an
/// *intermediate*, one per layer per tile and one per rayon worker at a time,
/// so its ceiling is about how much memory a frame may hold at once rather than
/// about what a caller may ask for. Passing it splits the destination rather
/// than failing (see the module docs), so the number trades peak memory against
/// the number of sub-composites, never against correctness.
pub const MAX_PREIMAGE_PIXELS: u64 = 1 << 22;

/// Side of the source window kept for a single destination pixel whose
/// pre-image is too large to allocate.
///
/// Bilinear sampling of one pixel reads the four samples around the point it
/// maps to, which lies within a pixel of the centre of its pre-image rect, so
/// eight pixels of window is several times the reach actually needed.
pub const TAP_WINDOW: u32 = 8;

/// Knobs that change what a composite *means*, as opposed to how fast it is
/// produced.
///
/// They take part in the tile cache key, so changing one invalidates cached
/// tiles rather than mixing two policies into one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CompositeOptions {
    /// Which encoding the blend functions see. See [`BlendSpace`].
    pub blend_space: BlendSpace,
    /// Seed for `Dissolve`'s per-pixel draw. Stable across frames by default so
    /// a dissolving layer does not shimmer while the user pans.
    pub dissolve_seed: u64,
}

/// Composite the whole document over `region` at `level`, tile by tile and in
/// parallel.
///
/// This is the entry point the renderer wants: ask for the visible region and
/// get exactly it. The work is quantized to tiles internally, so the answer for
/// any given pixel does not depend on which region asked for it.
pub fn composite_region<S: TileSource + ?Sized>(
    doc: &Document,
    source: &S,
    region: PixelRect,
    level: u8,
    opts: CompositeOptions,
) -> Result<Canvas, CompositeError> {
    let ctx = Ctx::new(doc, source, level, opts)?;
    let mut out = Canvas::transparent(region)?;
    let coords = ctx.tiles_covering(region);
    let tiles = coords
        .par_iter()
        .map(|c| ctx.composite_root(tile_rect(*c)))
        .collect::<Result<Vec<_>, _>>()?;
    for t in &tiles {
        out.blit_from(t);
    }
    Ok(out)
}

/// Composite one tile of the document. `coord` carries its own mip level.
pub fn composite_tile<S: TileSource + ?Sized>(
    doc: &Document,
    source: &S,
    coord: TileCoord,
    opts: CompositeOptions,
) -> Result<Canvas, CompositeError> {
    let ctx = Ctx::new(doc, source, coord.level, opts)?;
    ctx.composite_root(tile_rect(coord))
}

/// Composite `rect` in one pass, without tiling and without touching any cache.
///
/// Equal to [`composite_region`] for the same rect — the tiled path exists for
/// parallelism and caching, not for a different answer, and
/// `tiling_does_not_change_the_answer` pins that. Useful when you want the
/// pixels once and do not want a tile grid's worth of intermediate buffers.
pub fn composite_rect<S: TileSource + ?Sized>(
    doc: &Document,
    source: &S,
    rect: PixelRect,
    level: u8,
    opts: CompositeOptions,
) -> Result<Canvas, CompositeError> {
    Ctx::new(doc, source, level, opts)?.composite_root(rect)
}

/// Composite one layer and its descendants over `rect`, as if nothing else were
/// in the document.
///
/// The subtree root is drawn over transparency with its own blend mode and
/// opacity, and its clipping flag is inert — there is nothing beneath it to
/// clip to, the same answer [`layer_model::LayerTree::clipping_group`] gives
/// for a clipper with no base.
pub fn composite_subtree<S: TileSource + ?Sized>(
    doc: &Document,
    source: &S,
    root: LayerId,
    rect: PixelRect,
    level: u8,
    opts: CompositeOptions,
) -> Result<Canvas, CompositeError> {
    if !doc.layers.contains(root) {
        return Err(CompositeError::LayerNotFound(root));
    }
    let ctx = Ctx::new(doc, source, level, opts)?;
    let mut c = Canvas::transparent(rect)?;
    ctx.composite_ids(&[root], rect, &mut c)?;
    ctx.clip_to_document(&mut c);
    Ok(c)
}

/// The image-space rect one tile covers.
pub fn tile_rect(coord: TileCoord) -> PixelRect {
    let (x, y) = coord.pixel_origin();
    PixelRect::new(x, y, TILE_SIZE, TILE_SIZE)
}

/// Everything the traversal needs, resolved once per composite call.
pub(crate) struct Ctx<'a, S: TileSource + ?Sized> {
    doc: &'a Document,
    source: &'a S,
    level: u8,
    opts: CompositeOptions,
    /// Document dimensions **at `level`**. Zero for a zero-area document.
    width: u32,
    height: u32,
    space: ColorSpace,
    /// 8-bit decode table, present only for colour spaces whose decode acts on
    /// each channel independently. Built from [`to_linear`] itself, so it
    /// cannot disagree with the non-table path.
    decode: Option<[f32; 256]>,
}

impl<'a, S: TileSource + ?Sized> Ctx<'a, S> {
    pub(crate) fn new(
        doc: &'a Document,
        source: &'a S,
        level: u8,
        opts: CompositeOptions,
    ) -> Result<Self, CompositeError> {
        let (w0, h0) = (doc.width(), doc.height());
        if level >= level_count(w0, h0) {
            return Err(CompositeError::NoSuchLevel {
                level,
                width: w0,
                height: h0,
            });
        }
        // `level_dimensions` floors at 1 in each axis; a document with no area
        // has no pixels at any level, so say so rather than inventing a row.
        let (width, height) = if w0 == 0 || h0 == 0 {
            (0, 0)
        } else {
            level_dimensions(w0, h0, level)
        };
        let space = doc.meta.color_space.clone();
        let decode = match space {
            // Per-channel transfer functions: a 256-entry table is exact.
            ColorSpace::Srgb | ColorSpace::LinearSrgb => {
                let mut lut = [0.0f32; 256];
                for (i, slot) in lut.iter_mut().enumerate() {
                    *slot = to_linear(&space, [i as f32 / 255.0; 3])[0];
                }
                Some(lut)
            }
            // Display P3 mixes channels through a matrix, so no per-channel
            // table can stand in for it.
            _ => None,
        };
        Ok(Self {
            doc,
            source,
            level,
            opts,
            width,
            height,
            space,
            decode,
        })
    }

    /// Tile coordinates covering `region`, clamped to the document's tile grid
    /// at this level. Reuses [`TileGrid`]'s extent maths so the compositor and
    /// the tile store agree on which tiles exist.
    pub(crate) fn tiles_covering(&self, region: PixelRect) -> Vec<TileCoord> {
        let grid = TileGrid::new_at_level(self.width, self.height, PixelFormat::Rgba8, self.level);
        grid.visible_tiles(region).collect()
    }

    /// Composite the document root over `rect`.
    pub(crate) fn composite_root(&self, rect: PixelRect) -> Result<Canvas, CompositeError> {
        let mut c = Canvas::transparent(rect)?;
        self.composite_ids(self.doc.layers.root(), rect, &mut c)?;
        self.clip_to_document(&mut c);
        Ok(c)
    }

    /// Zero every pixel outside the document's canvas at this level.
    ///
    /// Layers keep pixels past the canvas edge — that is what makes a transform
    /// non-destructive — and an edge tile's padding can hold whatever a
    /// full-tile fill wrote there. Neither is part of the image.
    fn clip_to_document(&self, c: &mut Canvas) {
        let rect = c.rect();
        let (w, h) = (self.width as i64, self.height as i64);
        if rect.x >= 0 && rect.y >= 0 && rect.right() <= w && rect.bottom() <= h {
            return;
        }
        let stride = rect.width as i64;
        for (i, px) in c.pixels_mut().iter_mut().enumerate() {
            let x = rect.x + (i as i64) % stride;
            let y = rect.y + (i as i64) / stride;
            if x < 0 || y < 0 || x >= w || y >= h {
                *px = [0.0; 4];
            }
        }
    }

    /// Composite an ordered sibling list (top-most first) onto `backdrop`.
    fn composite_ids(
        &self,
        ids: &[LayerId],
        rect: PixelRect,
        backdrop: &mut Canvas,
    ) -> Result<(), CompositeError> {
        let mut i = ids.len();
        while i > 0 {
            i -= 1;
            let Some(layer) = self.doc.layers.get(ids[i]) else {
                continue;
            };
            if layer.clipping == ClippingMode::ClipToBelow {
                // Reached bottom-up, so nothing non-clipping lies beneath it in
                // this list: the flag has no base and no effect.
                self.draw_plain(layer, rect, backdrop)?;
                continue;
            }
            // Collect the contiguous run of clippers stacked on this base,
            // bottom-most clipper first, consuming them from the walk.
            let mut clipped = Vec::new();
            while i > 0
                && self
                    .doc
                    .layers
                    .get(ids[i - 1])
                    .is_some_and(|l| l.clipping == ClippingMode::ClipToBelow)
            {
                i -= 1;
                clipped.push(ids[i]);
            }
            if clipped.is_empty() {
                self.draw_plain(layer, rect, backdrop)?;
            } else {
                self.draw_clipping_group(layer, &clipped, rect, backdrop)?;
            }
        }
        Ok(())
    }

    /// Draw one unclipped layer onto `backdrop`.
    fn draw_plain(
        &self,
        layer: &Layer,
        rect: PixelRect,
        backdrop: &mut Canvas,
    ) -> Result<(), CompositeError> {
        if layer.is_noop() {
            return Ok(());
        }
        match &layer.kind {
            LayerKind::Adjustment(adj) => {
                let prepared = PreparedAdjustment::new(&adj.kind);
                if !prepared.is_identity() {
                    let cov = self.adjustment_coverage(layer, rect)?;
                    self.apply_adjustment(&prepared, layer, backdrop, cov.as_deref());
                }
            }
            LayerKind::Group(g) if self.is_pass_through(layer, g) => {
                let mut buf = backdrop.clone();
                self.composite_ids(&g.children, rect, &mut buf)?;
                let cov = self.mask_coverage(layer, rect)?;
                let w = layer.effective_opacity() * layer.effective_fill_opacity();
                let src = buf.pixels().to_vec();
                for (i, d) in backdrop.pixels_mut().iter_mut().enumerate() {
                    let k = w * cov.as_ref().map_or(1.0, |c| c[i]);
                    if k <= 0.0 {
                        continue;
                    }
                    *d = lerp4(*d, src[i], k);
                }
            }
            LayerKind::Group(_) | LayerKind::Raster(_) | LayerKind::Generator(_) => {
                let src = self.render_source(layer, rect)?;
                self.blend_source(&src, layer, backdrop);
            }
            // No rasterizer for these yet; they contribute nothing. See the
            // crate docs' "Not yet" list.
            LayerKind::Text(_) | LayerKind::Shape(_) | LayerKind::SmartObject(_) => {}
        }
        Ok(())
    }

    /// Draw a base layer together with the run of layers clipped to it.
    ///
    /// `clipped` is bottom-most clipper first.
    fn draw_clipping_group(
        &self,
        base: &Layer,
        clipped: &[LayerId],
        rect: PixelRect,
        backdrop: &mut Canvas,
    ) -> Result<(), CompositeError> {
        // A hidden base hides everything clipped to it — there is no shape for
        // the group to live in.
        if base.is_noop() {
            return Ok(());
        }
        // The group's buffer starts as the base's own content, so its alpha is
        // the base's shape and stays that way through every `atop`.
        let mut buf = self.render_source(base, rect)?;
        for &id in clipped {
            let Some(layer) = self.doc.layers.get(id) else {
                continue;
            };
            if layer.is_noop() {
                continue;
            }
            match &layer.kind {
                LayerKind::Adjustment(adj) => {
                    let prepared = PreparedAdjustment::new(&adj.kind);
                    if !prepared.is_identity() {
                        let cov = self.adjustment_coverage(layer, rect)?;
                        // `buf`'s alpha is the base's shape, and the adjustment
                        // skips zero-alpha pixels, so the clip is automatic.
                        self.apply_adjustment(&prepared, layer, &mut buf, cov.as_deref());
                    }
                }
                _ => {
                    let src = self.render_source(layer, rect)?;
                    self.blend_atop_buffer(&src, layer, &mut buf);
                }
            }
        }
        self.blend_source(&buf, base, backdrop);
        Ok(())
    }

    fn is_pass_through(&self, layer: &Layer, group: &GroupLayer) -> bool {
        group.blending == GroupBlending::PassThrough
            && layer.blend_mode == BlendMode::Normal
            && is_identity(&self.level_transform(layer))
    }

    /// The layer's own contribution over `rect`: premultiplied linear pixels
    /// whose alpha is the layer's **shape** (content alpha × mask), before its
    /// opacity and blend mode are applied.
    fn render_source(&self, layer: &Layer, rect: PixelRect) -> Result<Canvas, CompositeError> {
        let t = self.level_transform(layer);
        let has_mask = self.active_mask(layer).is_some();
        let mask_linked = self.active_mask(layer).is_some_and(|m| m.linked);

        if is_identity(&t) {
            let mut c = self.render_content(layer, rect)?;
            if let Some(cov) = self.mask_coverage(layer, rect)? {
                multiply_alpha(&mut c, &cov);
            }
            return Ok(c);
        }

        let bounds = self.content_bounds(layer);
        let mut out = self.render_via_transform(&t, rect, bounds, &|src_rect| {
            let mut c = self.render_content(layer, src_rect)?;
            if mask_linked {
                if let Some(cov) = self.mask_coverage(layer, src_rect)? {
                    multiply_alpha(&mut c, &cov);
                }
            }
            Ok(c)
        })?;
        if has_mask && !mask_linked {
            // An unlinked mask stays put in document space while the content
            // moves under it.
            if let Some(cov) = self.mask_coverage(layer, rect)? {
                multiply_alpha(&mut out, &cov);
            }
        }
        Ok(out)
    }

    /// Render something authored in layer space into document space through
    /// `t`, allocating no more than [`MAX_PREIMAGE_PIXELS`] at a time.
    ///
    /// `render` produces the layer-space content over whatever rect it is
    /// handed; `bounds` is the extent outside which that content is known to be
    /// empty, or `None` when it cannot be bounded. See the module docs for why
    /// each of the three bounds applied here is exact rather than an
    /// approximation.
    fn render_via_transform<F>(
        &self,
        t: &Affine2,
        rect: PixelRect,
        bounds: Option<PixelRect>,
        render: &F,
    ) -> Result<Canvas, CompositeError>
    where
        F: Fn(PixelRect) -> Result<Canvas, CompositeError>,
    {
        let pre = preimage_rect(t, rect);
        let src_rect = clip_to(pre, bounds);
        if src_rect.is_empty() {
            // Nothing of this layer reaches `rect`: a degenerate transform, or
            // a pre-image that misses every tile the layer actually stores.
            return Canvas::transparent(rect);
        }
        if rect_area(src_rect) <= MAX_PREIMAGE_PIXELS {
            let src = render(src_rect)?;
            return resample(&src, t, rect);
        }
        if let Some((a, b)) = split_rect(rect) {
            let mut out = Canvas::transparent(rect)?;
            out.blit_from(&self.render_via_transform(t, a, bounds, render)?);
            out.blit_from(&self.render_via_transform(t, b, bounds, render)?);
            return Ok(out);
        }
        // One destination pixel, and even its pre-image is too big to hold:
        // keep the window the bilinear taps can actually reach. The window is
        // taken from the unclipped pre-image so that clipping cannot move it
        // off the point being sampled.
        let src_rect = clip_to(centre_window(pre, TAP_WINDOW), bounds);
        if src_rect.is_empty() {
            return Canvas::transparent(rect);
        }
        let src = render(src_rect)?;
        resample(&src, t, rect)
    }

    /// The extent, in the layer's own pre-transform pixel space at this level,
    /// outside which the layer renders nothing. `None` when it cannot be
    /// bounded.
    ///
    /// This is what keeps a minified layer from asking for a pre-image the size
    /// of its inverse scale: outside its stored tiles a raster layer is
    /// transparent, so intersecting with this is not an approximation.
    ///
    /// A group's extent is the union of its children's, each mapped forward
    /// through the child's own transform. Layers that contribute no pixels —
    /// adjustments, and the kinds with no rasterizer yet — bound to nothing:
    /// an adjustment rewrites pixels that are already there, and pixels are
    /// only there inside some sibling's extent.
    fn content_bounds(&self, layer: &Layer) -> Option<PixelRect> {
        match &layer.kind {
            LayerKind::Raster(_) | LayerKind::Generator(_) => {
                Some(self.tile_map_bounds(PixelKey::Layer(layer.id)))
            }
            LayerKind::Group(g) => {
                let mut acc = EMPTY_RECT;
                for &cid in &g.children {
                    let Some(child) = self.doc.layers.get(cid) else {
                        continue;
                    };
                    let b = self.content_bounds(child)?;
                    if b.is_empty() {
                        continue;
                    }
                    let ct = self.level_transform(child);
                    let b = if is_identity(&ct) {
                        b
                    } else {
                        image_rect(&ct, b)?
                    };
                    acc = union_rects(acc, b);
                }
                Some(acc)
            }
            LayerKind::Adjustment(_)
            | LayerKind::Text(_)
            | LayerKind::Shape(_)
            | LayerKind::SmartObject(_) => Some(EMPTY_RECT),
        }
    }

    /// Bounding rect of every tile stored under `key` at this mip level.
    fn tile_map_bounds(&self, key: PixelKey) -> PixelRect {
        let Some(map) = self.doc.pixels.tiles(key) else {
            return EMPTY_RECT;
        };
        let t = TILE_SIZE as i64;
        let (mut x0, mut y0) = (i64::MAX, i64::MAX);
        let (mut x1, mut y1) = (i64::MIN, i64::MIN);
        let mut any = false;
        for (coord, _) in map.iter() {
            if coord.level != self.level {
                continue;
            }
            any = true;
            let (ox, oy) = coord.pixel_origin();
            x0 = x0.min(ox);
            y0 = y0.min(oy);
            x1 = x1.max(ox + t);
            y1 = y1.max(oy + t);
        }
        if !any {
            return EMPTY_RECT;
        }
        rect_from_bounds(x0, y0, x1, y1).unwrap_or(EMPTY_RECT)
    }

    /// The mask a composite will actually read, if any.
    ///
    /// [`MaskKind::Vector`] is a path rasterized on demand and this crate has
    /// no rasterizer for one, so a vector mask that nobody has rasterized into
    /// tiles resolves to `None` — the layer renders unmasked rather than
    /// disappearing behind coverage that is zero only because it was never
    /// computed. Tiles stored under the mask's id are honoured whatever the
    /// kind says, so a rasterizer can start filling them in without this
    /// changing.
    fn active_mask<'l>(&self, layer: &'l Layer) -> Option<&'l LayerMask> {
        let mask = layer.effective_mask()?;
        if mask.kind == MaskKind::Vector
            && self
                .doc
                .pixels
                .tiles(PixelKey::Mask(mask.id))
                .is_none_or(|m| m.is_empty())
        {
            return None;
        }
        Some(mask)
    }

    /// The layer's pixels before any mask or transform.
    fn render_content(&self, layer: &Layer, rect: PixelRect) -> Result<Canvas, CompositeError> {
        let mut c = Canvas::transparent(rect)?;
        match &layer.kind {
            LayerKind::Group(g) => self.composite_ids(&g.children, rect, &mut c)?,
            LayerKind::Raster(_) | LayerKind::Generator(_) => self.fill_layer(layer.id, &mut c),
            _ => {}
        }
        Ok(c)
    }

    /// Blend a rendered source over the backdrop with the layer's mode and
    /// opacity.
    fn blend_source(&self, src: &Canvas, layer: &Layer, backdrop: &mut Canvas) {
        self.blend_into(src, layer, backdrop, false);
    }

    /// Composite a rendered source *atop* the buffer, keeping the buffer's
    /// alpha. Used for the members of a clipping group.
    fn blend_atop_buffer(&self, src: &Canvas, layer: &Layer, buf: &mut Canvas) {
        self.blend_into(src, layer, buf, true);
    }

    fn blend_into(&self, src: &Canvas, layer: &Layer, dst: &mut Canvas, atop: bool) {
        debug_assert_eq!(src.rect(), dst.rect());
        let w = layer.effective_opacity() * layer.effective_fill_opacity();
        if w <= 0.0 {
            return;
        }
        let mode = layer.blend_mode;
        let bctx = BlendContext {
            space: &self.space,
            blend_space: self.opts.blend_space,
        };
        let rect = dst.rect();
        let stride = rect.width as i64;
        let (level, seed) = (self.level, self.opts.dissolve_seed);
        let source = src.pixels();
        for (i, d) in dst.pixels_mut().iter_mut().enumerate() {
            let sp = source[i];
            if sp[3] <= 0.0 {
                continue;
            }
            let straight = unpremultiply(sp);
            let rgb = [straight[0], straight[1], straight[2]];
            let mut sa = sp[3] * w;
            if mode == BlendMode::Dissolve {
                let x = rect.x + (i as i64) % stride;
                let y = rect.y + (i as i64) / stride;
                sa = if dissolve_keeps_source(sa, dissolve_noise(x, y, level, seed)) {
                    1.0
                } else {
                    0.0
                };
            }
            *d = if atop {
                blend_atop(*d, rgb, sa, mode, &bctx)
            } else {
                blend_over(*d, rgb, sa, mode, &bctx)
            };
        }
    }

    /// The alpha multiplier an adjustment layer's mask applies over `rect`, or
    /// `None` when no mask can change the result.
    ///
    /// An adjustment contributes no pixels, so the only thing its transform can
    /// move is a **linked** mask. That case is handled exactly as
    /// [`Ctx::render_source`] handles content: the mask is read in the layer's
    /// own space over the pre-image of `rect` and resampled forward through the
    /// transform. Both the sampled rect and the resampler are shared with the
    /// content path, so the tile-cache key — which hashes the pre-image for a
    /// linked mask — describes what this actually reads.
    ///
    /// An unlinked mask, or an identity transform, is plain document-space
    /// coverage. A singular transform leaves nothing to sample, so the mask
    /// covers nothing and the adjustment does not apply.
    fn adjustment_coverage(
        &self,
        layer: &Layer,
        rect: PixelRect,
    ) -> Result<Option<Vec<f32>>, CompositeError> {
        let t = self.level_transform(layer);
        let Some(mask) = self.active_mask(layer) else {
            return Ok(None);
        };
        if !mask.linked || is_identity(&t) {
            return self.mask_coverage(layer, rect);
        }
        // Carry the scalar field through the shared bilinear resampler in the
        // alpha channel; anything outside the pre-image reads 0, which is the
        // same "no coverage" an absent mask tile means. The extent of the
        // mask's stored tiles, grown by the feather that reaches out of them,
        // bounds what is worth sampling — but only when a *missing* tile really
        // does mean no coverage. An inverted mask, or one below full density,
        // covers everything its tiles do not, and has no bound at all.
        let bounds = (mask.coverage(0.0) <= 0.0).then(|| {
            self.mask_sample(mask, self.tile_map_bounds(PixelKey::Mask(mask.id)))
                .0
        });
        let out = self.render_via_transform(&t, rect, bounds, &|src_rect| {
            let mut c = Canvas::transparent(src_rect)?;
            if let Some(cov) = self.mask_coverage(layer, src_rect)? {
                for (px, k) in c.pixels_mut().iter_mut().zip(&cov) {
                    *px = [0.0, 0.0, 0.0, *k];
                }
            }
            Ok(c)
        })?;
        Ok(Some(out.pixels().iter().map(|p| p[3]).collect()))
    }

    /// Rewrite a backdrop in place. Alpha is never touched.
    ///
    /// The weight is opacity × fill opacity, the same product
    /// [`Ctx::blend_into`] uses. While no layer effects are rendered the two are
    /// indistinguishable on any layer (see the crate docs' "Not yet" list); an
    /// adjustment layer is no exception, and hashing `fill_opacity` into the
    /// tile key would otherwise evict tiles to repaint an identical picture.
    fn apply_adjustment(
        &self,
        prepared: &PreparedAdjustment,
        layer: &Layer,
        backdrop: &mut Canvas,
        cov: Option<&[f32]>,
    ) {
        let w = layer.effective_opacity() * layer.effective_fill_opacity();
        if w <= 0.0 {
            return;
        }
        let space = &self.space;
        for (i, d) in backdrop.pixels_mut().iter_mut().enumerate() {
            let k = w * cov.map_or(1.0, |c| c[i]);
            if k <= 0.0 || d[3] <= 0.0 {
                continue;
            }
            let a = d[3];
            let straight = unpremultiply(*d);
            let adj = prepared.apply([straight[0], straight[1], straight[2]], space);
            *d = lerp4(*d, [adj[0] * a, adj[1] * a, adj[2] * a, a], k);
        }
    }

    /// The mask's resolved alpha multiplier over `rect`, or `None` when the
    /// layer has no mask that can change the composite.
    fn mask_coverage(
        &self,
        layer: &Layer,
        rect: PixelRect,
    ) -> Result<Option<Vec<f32>>, CompositeError> {
        let Some(mask) = self.active_mask(layer) else {
            return Ok(None);
        };
        let (sample, radii) = self.mask_sample(mask, rect);
        let total: i64 = radii.iter().sum();
        let mut raw = vec![0.0f32; Canvas::area(sample)?];
        self.fill_mask(mask.id, sample, &mut raw);
        if total > 0 {
            raw = blur(raw, sample.width as usize, sample.height as usize, &radii);
        }
        let stride = sample.width as usize;
        let mut out = Vec::with_capacity(Canvas::area(rect)?);
        for y in rect.y..rect.bottom() {
            let row = (y - sample.y) as usize * stride;
            for x in rect.x..rect.right() {
                out.push(mask.coverage(raw[row + (x - sample.x) as usize]));
            }
        }
        Ok(Some(out))
    }

    /// The rect a mask read over `base` touches, and the box radii to blur it
    /// with.
    ///
    /// Normally that is `base` grown by the feather's reach. A feather large
    /// enough that the grown rect could not be allocated has its radii scaled
    /// down to what fits instead: the alternative is refusing the frame over a
    /// slider value, and a feather is a look, not a promise about a pixel.
    ///
    /// Radii and rect always agree, which is what keeps the answer independent
    /// of the region asked for — a blur reaching past its buffer would read the
    /// zeros outside and so depend on where the buffer happened to end. The
    /// tile cache key calls this too, so a clamped feather is described by its
    /// key exactly like an unclamped one.
    fn mask_sample(&self, mask: &LayerMask, base: PixelRect) -> (PixelRect, [i64; 3]) {
        let mut radii = self.feather_radii(mask.feather_px());
        let total: i64 = radii.iter().sum();
        if total <= 0 || base.is_empty() {
            return (base, [0; 3]);
        }
        if let Ok(r) = expand_rect(base, total) {
            return (r, radii);
        }
        let max = max_margin(base);
        if max <= 0 {
            return (base, [0; 3]);
        }
        for r in radii.iter_mut() {
            *r = *r * max / total;
        }
        match expand_rect(base, radii.iter().sum()) {
            Ok(r) => (r, radii),
            Err(_) => (base, [0; 3]),
        }
    }

    /// Box radii approximating a Gaussian of the mask's feather, in pixels at
    /// this mip level.
    ///
    /// `feather_px` is a radius in *document* pixels, so it shrinks with the
    /// level. The Gaussian sigma is taken as `radius / 3`, the usual convention
    /// that puts the kernel's ±3σ support at the stated radius, and it is then
    /// approximated by three iterated box blurs — the standard O(1)-per-pixel
    /// construction, and the reason a large feather does not cost a large
    /// kernel.
    fn feather_radii(&self, feather_px: f32) -> [i64; 3] {
        let scale = 2.0f32.powi(self.level as i32);
        let sigma = feather_px / scale / 3.0;
        if !sigma.is_finite() || sigma <= 0.05 {
            return [0; 3];
        }
        boxes_for_gauss(sigma)
    }

    /// Decode one stored RGBA8 colour into linear.
    fn decode_rgb(&self, r: u8, g: u8, b: u8) -> [f32; 3] {
        match &self.decode {
            Some(lut) => [lut[r as usize], lut[g as usize], lut[b as usize]],
            None => to_linear(
                &self.space,
                [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0],
            ),
        }
    }

    /// Read a raster layer's stored tiles into `out`.
    fn fill_layer(&self, id: LayerId, out: &mut Canvas) {
        let Some(map) = self.doc.layer_tiles(id) else {
            return;
        };
        let rect = out.rect();
        let needed = Tile::byte_len(PixelFormat::Rgba8);
        for coord in tile_coords_for(rect, self.level) {
            let Some(hash) = map.get(coord) else { continue };
            let Some(data) = self.source.tile(hash) else {
                continue;
            };
            if data.len() < needed {
                continue;
            }
            let (ox, oy) = coord.pixel_origin();
            let x0 = rect.x.max(ox);
            let x1 = rect.right().min(ox + TILE_SIZE as i64);
            let y0 = rect.y.max(oy);
            let y1 = rect.bottom().min(oy + TILE_SIZE as i64);
            for y in y0..y1 {
                for x in x0..x1 {
                    let s = (((y - oy) as usize) * TILE_SIZE as usize + (x - ox) as usize) * 4;
                    let a = data[s + 3] as f32 / 255.0;
                    let lin = self.decode_rgb(data[s], data[s + 1], data[s + 2]);
                    let Some(i) = out.index_of(x, y) else {
                        continue;
                    };
                    out.pixels_mut()[i] = [lin[0] * a, lin[1] * a, lin[2] * a, a];
                }
            }
        }
    }

    /// Read a mask's stored 8-bit coverage tiles into `out` (row-major over
    /// `rect`). Absent tiles stay at 0.0 — zero coverage, as `editor-core`
    /// documents.
    fn fill_mask(&self, mask: MaskId, rect: PixelRect, out: &mut [f32]) {
        let Some(map) = self.doc.pixels.tiles(PixelKey::Mask(mask)) else {
            return;
        };
        let stride = rect.width as usize;
        for coord in tile_coords_for(rect, self.level) {
            let Some(hash) = map.get(coord) else { continue };
            let Some(data) = self.source.tile(hash) else {
                continue;
            };
            if data.len() < editor_core::MASK_TILE_BYTES {
                continue;
            }
            let (ox, oy) = coord.pixel_origin();
            let x0 = rect.x.max(ox);
            let x1 = rect.right().min(ox + TILE_SIZE as i64);
            let y0 = rect.y.max(oy);
            let y1 = rect.bottom().min(oy + TILE_SIZE as i64);
            for y in y0..y1 {
                let drow = (y - rect.y) as usize * stride;
                let srow = (y - oy) as usize * TILE_SIZE as usize;
                for x in x0..x1 {
                    out[drow + (x - rect.x) as usize] =
                        data[srow + (x - ox) as usize] as f32 / 255.0;
                }
            }
        }
    }

    /// The layer's transform expressed in this mip level's pixel space.
    ///
    /// A transform is authored in level-0 document pixels, so at level `L` it
    /// is conjugated by a uniform `2^-L` scale: the linear part is unchanged
    /// and the translation shrinks with the image. A transform holding a
    /// non-finite component is treated as the identity — the layer renders
    /// untransformed rather than disappearing into NaN.
    fn level_transform(&self, layer: &Layer) -> Affine2 {
        let m = layer.transform;
        if !m.to_cols_array().iter().all(|v| v.is_finite()) {
            return Affine2::IDENTITY;
        }
        if self.level == 0 {
            return m;
        }
        let s = 2.0f32.powi(-(self.level as i32));
        Affine2::from_scale(Vec2::splat(s)) * m * Affine2::from_scale(Vec2::splat(1.0 / s))
    }

    /// A hash of every input that decides this tile's pixels.
    ///
    /// Covers the document's geometry and colour space, the compositing
    /// options, every layer property that reaches the maths, and the content
    /// hash of each layer and mask tile the traversal will actually read for
    /// this coordinate — including the wider rects a transform's pre-image or a
    /// mask's feather pull in. Two composites with equal keys therefore produce
    /// equal pixels, which is what makes the tile cache safe to trust.
    ///
    /// One documented collision: two different ICC profiles hash alike, because
    /// [`ColorSpace::IccProfile`] takes an identity path in `color` and so
    /// cannot change a pixel.
    pub(crate) fn tile_input_key(&self, coord: TileCoord) -> u64 {
        let mut h = DefaultHasher::new();
        self.level.hash(&mut h);
        coord.x.hash(&mut h);
        coord.y.hash(&mut h);
        coord.level.hash(&mut h);
        self.width.hash(&mut h);
        self.height.hash(&mut h);
        self.space.name().hash(&mut h);
        self.opts.hash(&mut h);
        self.hash_ids(self.doc.layers.root(), tile_rect(coord), &mut h);
        h.finish()
    }

    fn hash_ids(&self, ids: &[LayerId], rect: PixelRect, h: &mut DefaultHasher) {
        for &id in ids {
            let Some(layer) = self.doc.layers.get(id) else {
                0xFFu8.hash(h);
                continue;
            };
            hash_layer_props(layer, h);
            let t = self.level_transform(layer);
            let identity = is_identity(&t);
            // The same rect the traversal will sample, bounds and all — a
            // superset would only hash tiles nobody reads, but for a strong
            // minification that superset is thousands of tiles per layer.
            let content_rect = if identity {
                rect
            } else {
                clip_to(preimage_rect(&t, rect), self.content_bounds(layer))
            };
            match &layer.kind {
                LayerKind::Group(g) => {
                    let child_rect = if self.is_pass_through(layer, g) {
                        rect
                    } else {
                        content_rect
                    };
                    self.hash_ids(&g.children, child_rect, h);
                }
                LayerKind::Raster(_) | LayerKind::Generator(_) => {
                    self.hash_tiles(PixelKey::Layer(id), content_rect, h);
                }
                _ => {}
            }
            if let Some(mask) = layer.effective_mask() {
                // A linked mask is read in the layer's own space, so it is the
                // pre-image that must be hashed — for an adjustment layer too,
                // which is why `Ctx::adjustment_coverage` samples exactly this
                // rect. An unlinked mask never moved, so hash `rect` itself.
                //
                // The content path reads a linked mask over the *content*
                // bound and the adjustment path over the *mask* bound, so the
                // union of the two is hashed: hashing less than a path reads is
                // how a cache goes stale.
                let base = if identity {
                    rect
                } else if mask.linked {
                    let mask_bound = (mask.coverage(0.0) <= 0.0).then(|| {
                        self.mask_sample(mask, self.tile_map_bounds(PixelKey::Mask(mask.id)))
                            .0
                    });
                    let bounds = match (self.content_bounds(layer), mask_bound) {
                        (Some(c), Some(m)) => Some(union_rects(c, m)),
                        // Either path is unbounded, so neither is the hash: an
                        // inverted mask covers everywhere its tiles are not.
                        _ => None,
                    };
                    clip_to(preimage_rect(&t, rect), bounds)
                } else {
                    rect
                };
                let (mrect, _) = self.mask_sample(mask, base);
                self.hash_tiles(PixelKey::Mask(mask.id), mrect, h);
            }
        }
    }

    fn hash_tiles(&self, key: PixelKey, rect: PixelRect, h: &mut DefaultHasher) {
        let map = self.doc.pixels.tiles(key);
        if tile_span(rect) > MAX_KEYED_TILES {
            // A strongly minified layer samples a rect spanning a million tile
            // coordinates, nearly all of them empty. Hashing every tile the map
            // *holds* is a superset of any rect, and costs what the layer
            // actually stores rather than what its transform spans.
            2u8.hash(h);
            match map {
                Some(m) => {
                    m.len().hash(h);
                    for (coord, hash) in m.iter() {
                        coord.x.hash(h);
                        coord.y.hash(h);
                        coord.level.hash(h);
                        hash.0.hash(h);
                    }
                }
                None => 0usize.hash(h),
            }
            return;
        }
        for coord in tile_coords_for(rect, self.level) {
            coord.x.hash(h);
            coord.y.hash(h);
            match map.and_then(|m| m.get(coord)) {
                Some(hash) => {
                    1u8.hash(h);
                    hash.0.hash(h);
                }
                None => 0u8.hash(h),
            }
        }
    }
}

const EMPTY_RECT: PixelRect = PixelRect::new(0, 0, 0, 0);

/// How many tile coordinates a cache key will enumerate one at a time before
/// falling back to hashing a whole tile map. 4096 is a 64x64 block of tiles,
/// far past any rect a sane transform samples.
const MAX_KEYED_TILES: u64 = 4096;

/// Number of tile coordinates whose squares overlap `rect`.
fn tile_span(rect: PixelRect) -> u64 {
    if rect.is_empty() {
        return 0;
    }
    let t = TILE_SIZE as i64;
    let cols = (rect.right() - 1).div_euclid(t) - rect.x.div_euclid(t) + 1;
    let rows = (rect.bottom() - 1).div_euclid(t) - rect.y.div_euclid(t) + 1;
    (cols.max(0) as u64).saturating_mul(rows.max(0) as u64)
}

fn is_identity(t: &Affine2) -> bool {
    t.abs_diff_eq(Affine2::IDENTITY, 1e-6)
}

fn multiply_alpha(c: &mut Canvas, cov: &[f32]) {
    for (px, k) in c.pixels_mut().iter_mut().zip(cov) {
        for ch in px.iter_mut() {
            *ch *= k;
        }
    }
}

fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    for i in 0..4 {
        out[i] = a[i] + (b[i] - a[i]) * t;
    }
    out
}

/// Every tile coordinate whose square overlaps `rect` at `level`.
///
/// Unlike [`TileGrid::visible_tiles`] this is not clamped to a grid extent: a
/// layer keeps pixels past the canvas edge, and a transform's pre-image can
/// reach into negative coordinates.
fn tile_coords_for(rect: PixelRect, level: u8) -> Vec<TileCoord> {
    if rect.is_empty() {
        return Vec::new();
    }
    let t = TILE_SIZE as i64;
    let x0 = rect.x.div_euclid(t);
    let x1 = (rect.right() - 1).div_euclid(t);
    let y0 = rect.y.div_euclid(t);
    let y1 = (rect.bottom() - 1).div_euclid(t);
    let mut out = Vec::new();
    for ty in y0..=y1 {
        for tx in x0..=x1 {
            // A tile grid is addressed by `i32`; anything past it holds no
            // tiles by construction.
            if let (Ok(x), Ok(y)) = (i32::try_from(tx), i32::try_from(ty)) {
                out.push(TileCoord::new(x, y, level));
            }
        }
    }
    out
}

/// Grow a rect by `margin` on every side.
///
/// Refused rather than wrapped when the grown rect runs off either end of the
/// coordinate space or past the canvas ceiling; `rect.x` reaches here straight
/// from a transform's pre-image, so `i64::MIN` is reachable input.
fn expand_rect(rect: PixelRect, margin: i64) -> Result<PixelRect, CompositeError> {
    if margin <= 0 || rect.is_empty() {
        return Ok(rect);
    }
    let too_large = || CompositeError::RegionTooLarge {
        pixels: (rect.width as u64)
            .saturating_add(2 * margin as u64)
            .saturating_mul((rect.height as u64).saturating_add(2 * margin as u64)),
        max: crate::MAX_CANVAS_PIXELS,
    };
    let (Some(x0), Some(y0)) = (rect.x.checked_sub(margin), rect.y.checked_sub(margin)) else {
        return Err(too_large());
    };
    let (Some(x1), Some(y1)) = (
        rect.right().checked_add(margin),
        rect.bottom().checked_add(margin),
    ) else {
        return Err(too_large());
    };
    let Some(r) = rect_from_bounds(x0, y0, x1, y1) else {
        return Err(too_large());
    };
    Canvas::area(r)?;
    Ok(r)
}

/// The rect in layer space that `rect` in document space maps back to, with a
/// two-pixel margin so bilinear taps at the border still have real data.
///
/// An empty rect comes back for a singular or numerically hopeless transform:
/// such a layer collapses to nothing rather than producing NaN coordinates.
/// "Numerically hopeless" includes a finite, non-singular transform whose
/// pre-image simply cannot be written down — `Affine2::from_scale(Vec2::new(
/// 1e-30, 1e30))` has determinant 1 and maps a tile back to something 4e30
/// pixels wide. Every step here saturates or is checked, because `f32 as i64`
/// pins such a coordinate at `i64::MAX` and the margin arithmetic on top of it
/// would otherwise overflow — a panic on a rayon worker, mid-frame, from a
/// number a user can reach by dragging a handle.
///
/// The result can still be enormous (a minified layer's pre-image grows with
/// the inverse of its scale). Bounding that is the caller's job; see
/// `Ctx::render_via_transform`.
fn preimage_rect(t: &Affine2, rect: PixelRect) -> PixelRect {
    if rect.is_empty() {
        return EMPTY_RECT;
    }
    let det = t.matrix2.determinant();
    if !det.is_finite() || det.abs() < 1e-12 {
        return EMPTY_RECT;
    }
    let inv = t.inverse();
    let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
    for (cx, cy) in [
        (rect.x, rect.y),
        (rect.right(), rect.y),
        (rect.x, rect.bottom()),
        (rect.right(), rect.bottom()),
    ] {
        let p = inv.transform_point2(Vec2::new(cx as f32, cy as f32));
        if !p.x.is_finite() || !p.y.is_finite() {
            return EMPTY_RECT;
        }
        lo = lo.min(p);
        hi = hi.max(p);
    }
    let x0 = (lo.x.floor() as i64).saturating_sub(2);
    let y0 = (lo.y.floor() as i64).saturating_sub(2);
    let x1 = (hi.x.ceil() as i64).saturating_add(2);
    let y1 = (hi.y.ceil() as i64).saturating_add(2);
    rect_from_bounds(x0, y0, x1, y1).unwrap_or(EMPTY_RECT)
}

/// The rect in document space that `rect` in layer space maps forward to, with
/// the same two-pixel margin — a destination pixel can be reached by bilinear
/// taps from up to a pixel outside the mapped shape.
///
/// `None` when the image cannot be written down as a rect, which for a caller
/// asking "where can this land?" means "anywhere".
fn image_rect(t: &Affine2, rect: PixelRect) -> Option<PixelRect> {
    if rect.is_empty() {
        return Some(EMPTY_RECT);
    }
    let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
    for (cx, cy) in [
        (rect.x, rect.y),
        (rect.right(), rect.y),
        (rect.x, rect.bottom()),
        (rect.right(), rect.bottom()),
    ] {
        let p = t.transform_point2(Vec2::new(cx as f32, cy as f32));
        if !p.x.is_finite() || !p.y.is_finite() {
            return None;
        }
        lo = lo.min(p);
        hi = hi.max(p);
    }
    rect_from_bounds(
        (lo.x.floor() as i64).saturating_sub(2),
        (lo.y.floor() as i64).saturating_sub(2),
        (hi.x.ceil() as i64).saturating_add(2),
        (hi.y.ceil() as i64).saturating_add(2),
    )
}

/// A rect from half-open bounds, or `None` when the extent does not fit a
/// `u32`.
fn rect_from_bounds(x0: i64, y0: i64, x1: i64, y1: i64) -> Option<PixelRect> {
    let (w, h) = (x1.checked_sub(x0)?, y1.checked_sub(y0)?);
    let (width, height) = (u32::try_from(w).ok()?, u32::try_from(h).ok()?);
    Some(PixelRect::new(x0, y0, width, height))
}

fn rect_area(rect: PixelRect) -> u64 {
    rect.width as u64 * rect.height as u64
}

/// The overlap of two rects, empty when they do not meet.
fn intersect_rects(a: PixelRect, b: PixelRect) -> PixelRect {
    if a.is_empty() || b.is_empty() {
        return EMPTY_RECT;
    }
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = a.right().min(b.right());
    let y1 = a.bottom().min(b.bottom());
    if x1 <= x0 || y1 <= y0 {
        return EMPTY_RECT;
    }
    rect_from_bounds(x0, y0, x1, y1).unwrap_or(EMPTY_RECT)
}

/// `rect` clipped to `bounds`, or unchanged when there are none.
fn clip_to(rect: PixelRect, bounds: Option<PixelRect>) -> PixelRect {
    match bounds {
        Some(b) => intersect_rects(rect, b),
        None => rect,
    }
}

/// The smallest rect covering both, ignoring an empty one.
fn union_rects(a: PixelRect, b: PixelRect) -> PixelRect {
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    rect_from_bounds(
        a.x.min(b.x),
        a.y.min(b.y),
        a.right().max(b.right()),
        a.bottom().max(b.bottom()),
    )
    .unwrap_or(a)
}

/// Halve a rect along its longer axis. `None` when it is a single pixel.
fn split_rect(rect: PixelRect) -> Option<(PixelRect, PixelRect)> {
    if rect.width >= rect.height {
        if rect.width < 2 {
            return None;
        }
        let a = rect.width / 2;
        Some((
            PixelRect::new(rect.x, rect.y, a, rect.height),
            PixelRect::new(rect.x + a as i64, rect.y, rect.width - a, rect.height),
        ))
    } else {
        if rect.height < 2 {
            return None;
        }
        let a = rect.height / 2;
        Some((
            PixelRect::new(rect.x, rect.y, rect.width, a),
            PixelRect::new(rect.x, rect.y + a as i64, rect.width, rect.height - a),
        ))
    }
}

/// A `side`-by-`side` window at the centre of `rect`, or `rect` when it is
/// already that small.
fn centre_window(rect: PixelRect, side: u32) -> PixelRect {
    if rect.width <= side && rect.height <= side {
        return rect;
    }
    let half = side as i64 / 2;
    let cx = rect.x.saturating_add(rect.width as i64 / 2);
    let cy = rect.y.saturating_add(rect.height as i64 / 2);
    PixelRect::new(
        cx.saturating_sub(half),
        cy.saturating_sub(half),
        rect.width.min(side),
        rect.height.min(side),
    )
}

/// The largest margin [`expand_rect`] will accept for `rect`.
fn max_margin(rect: PixelRect) -> i64 {
    let (mut lo, mut hi) = (0i64, 1i64 << 32);
    while lo < hi {
        let mid = lo + (hi - lo + 1) / 2;
        if expand_rect(rect, mid).is_ok() {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// Resample `src` (in layer space) through `t` into `dst_rect` (document
/// space), bilinearly.
///
/// Interpolating **premultiplied** samples is one reason the working buffer is
/// premultiplied: averaging straight colour across a transparent edge drags the
/// transparent pixel's meaningless RGB into the result and leaves a dark halo.
fn resample(src: &Canvas, t: &Affine2, dst_rect: PixelRect) -> Result<Canvas, CompositeError> {
    let mut out = Canvas::transparent(dst_rect)?;
    if src.pixels().is_empty() || dst_rect.is_empty() {
        return Ok(out);
    }
    let det = t.matrix2.determinant();
    if !det.is_finite() || det.abs() < 1e-12 {
        return Ok(out);
    }
    let inv = t.inverse();
    let stride = dst_rect.width as i64;
    for (i, o) in out.pixels_mut().iter_mut().enumerate() {
        let x = dst_rect.x + (i as i64) % stride;
        let y = dst_rect.y + (i as i64) / stride;
        let p = inv.transform_point2(Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
        *o = bilinear(src, p.x - 0.5, p.y - 0.5);
    }
    Ok(out)
}

fn bilinear(src: &Canvas, fx: f32, fy: f32) -> [f32; 4] {
    if !fx.is_finite() || !fy.is_finite() {
        return [0.0; 4];
    }
    let x0f = fx.floor();
    let y0f = fy.floor();
    let tx = fx - x0f;
    let ty = fy - y0f;
    // `f32 as i64` saturates rather than wrapping, so an absurd coordinate
    // lands outside the canvas and reads transparent.
    // ...and `saturating_add` keeps the +1 neighbour from wrapping at that
    // saturated bound, which overflows in a debug build.
    let (x0, y0) = (x0f as i64, y0f as i64);
    let (x1, y1) = (x0.saturating_add(1), y0.saturating_add(1));
    let top = lerp4(src.get(x0, y0), src.get(x1, y0), tx);
    let bottom = lerp4(src.get(x0, y1), src.get(x1, y1), tx);
    lerp4(top, bottom, ty)
}

/// Box radii whose three iterated passes approximate a Gaussian of `sigma`.
///
/// The standard construction (Kovesi): pick an odd box width just below the
/// ideal, then use the wider one for the remaining passes so the combined
/// variance matches `3 * sigma²`.
fn boxes_for_gauss(sigma: f32) -> [i64; 3] {
    const N: f32 = 3.0;
    let ideal = (12.0 * sigma * sigma / N + 1.0).sqrt();
    let mut wl = ideal.floor() as i64;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wl = wl.max(1);
    let wu = wl + 2;
    let wlf = wl as f32;
    let m = ((12.0 * sigma * sigma - N * wlf * wlf - 4.0 * N * wlf - 3.0 * N) / (-4.0 * wlf - 4.0))
        .round();
    let m = if m.is_finite() { m as i64 } else { 0 };
    let mut radii = [0i64; 3];
    for (i, r) in radii.iter_mut().enumerate() {
        let w = if (i as i64) < m { wl } else { wu };
        *r = (w - 1) / 2;
    }
    radii
}

/// Three separable box passes over a `w * h` buffer. Outside the buffer reads
/// as 0.0, which is exactly what an absent mask tile means.
fn blur(mut buf: Vec<f32>, w: usize, h: usize, radii: &[i64; 3]) -> Vec<f32> {
    if w == 0 || h == 0 {
        return buf;
    }
    for &r in radii {
        let r = r.max(0) as usize;
        if r == 0 {
            continue;
        }
        buf = box_pass(&buf, w, h, r, true);
        buf = box_pass(&buf, w, h, r, false);
    }
    buf
}

fn box_pass(src: &[f32], w: usize, h: usize, r: usize, horizontal: bool) -> Vec<f32> {
    let mut out = vec![0.0f32; src.len()];
    let (outer, inner, step, jump) = if horizontal {
        (h, w, 1usize, w)
    } else {
        (w, h, w, 1usize)
    };
    let window = (2 * r + 1) as f64;
    for o in 0..outer {
        let base = o * jump;
        // Seed the window for position 0: indices -r..=r, clipped to the line.
        let mut sum: f64 = 0.0;
        for k in 0..=r.min(inner - 1) {
            sum += src[base + k * step] as f64;
        }
        for i in 0..inner {
            out[base + i * step] = (sum / window) as f32;
            if i >= r {
                sum -= src[base + (i - r) * step] as f64;
            }
            let add = i + r + 1;
            if add < inner {
                sum += src[base + add * step] as f64;
            }
        }
    }
    out
}

fn hash_f32(v: f32, h: &mut DefaultHasher) {
    v.to_bits().hash(h);
}

/// Every property of a layer that can change a pixel.
///
/// Deliberately excludes `name` and `locked`: neither reaches the compositing
/// maths, and including them would evict cached tiles on a rename.
fn hash_layer_props(layer: &Layer, h: &mut DefaultHasher) {
    layer.id.0.as_bytes().hash(h);
    layer.visible.hash(h);
    hash_f32(layer.opacity, h);
    hash_f32(layer.fill_opacity, h);
    layer.blend_mode.shader_index().hash(h);
    layer.clipping.hash(h);
    for v in layer.transform.to_cols_array() {
        hash_f32(v, h);
    }
    match &layer.mask {
        Some(m) => {
            1u8.hash(h);
            m.id.0.as_bytes().hash(h);
            m.kind.hash(h);
            m.linked.hash(h);
            m.enabled.hash(h);
            m.inverted.hash(h);
            hash_f32(m.density(), h);
            hash_f32(m.feather_px(), h);
        }
        None => 0u8.hash(h),
    }
    match &layer.kind {
        LayerKind::Raster(_) => 0u8.hash(h),
        LayerKind::Group(g) => {
            1u8.hash(h);
            g.blending.hash(h);
            for c in &g.children {
                c.0.as_bytes().hash(h);
            }
        }
        LayerKind::Adjustment(a) => {
            2u8.hash(h);
            hash_adjustment(&a.kind, h);
        }
        LayerKind::Text(t) => {
            3u8.hash(h);
            t.text.hash(h);
            t.font_family.hash(h);
            hash_f32(t.size_px, h);
        }
        LayerKind::Shape(s) => {
            4u8.hash(h);
            s.path_svg.hash(h);
        }
        LayerKind::SmartObject(s) => {
            5u8.hash(h);
            s.asset.0.as_bytes().hash(h);
            s.linked.hash(h);
        }
        LayerKind::Generator(g) => {
            6u8.hash(h);
            g.provenance_key.hash(h);
        }
    }
}

fn hash_adjustment(kind: &layer_model::AdjustmentKind, h: &mut DefaultHasher) {
    use layer_model::AdjustmentKind as A;
    match kind {
        A::Levels {
            black,
            white,
            gamma,
        } => {
            0u8.hash(h);
            for v in [black, white, gamma] {
                hash_f32(*v, h);
            }
        }
        A::Curves { points } => {
            1u8.hash(h);
            points.len().hash(h);
            for p in points {
                hash_f32(p[0], h);
                hash_f32(p[1], h);
            }
        }
        A::Exposure { stops } => {
            2u8.hash(h);
            hash_f32(*stops, h);
        }
        A::HueSaturation {
            hue,
            saturation,
            lightness,
        } => {
            3u8.hash(h);
            for v in [hue, saturation, lightness] {
                hash_f32(*v, h);
            }
        }
        A::ColorBalance {
            shadows,
            midtones,
            highlights,
        } => {
            4u8.hash(h);
            for band in [shadows, midtones, highlights] {
                for v in band {
                    hash_f32(*v, h);
                }
            }
        }
        A::BrightnessContrast {
            brightness,
            contrast,
        } => {
            5u8.hash(h);
            hash_f32(*brightness, h);
            hash_f32(*contrast, h);
        }
        A::Vibrance {
            vibrance,
            saturation,
        } => {
            6u8.hash(h);
            hash_f32(*vibrance, h);
            hash_f32(*saturation, h);
        }
        A::BlackAndWhite { weights, tint } => {
            7u8.hash(h);
            for v in weights {
                hash_f32(*v, h);
            }
            tint.is_some().hash(h);
            for v in tint.iter().flatten() {
                hash_f32(*v, h);
            }
        }
        A::PhotoFilter {
            color_srgb,
            density,
            preserve_luminosity,
        } => {
            8u8.hash(h);
            for v in color_srgb {
                hash_f32(*v, h);
            }
            hash_f32(*density, h);
            preserve_luminosity.hash(h);
        }
        A::ChannelMixer { rows, monochrome } => {
            9u8.hash(h);
            for row in rows {
                for v in row {
                    hash_f32(*v, h);
                }
            }
            monochrome.hash(h);
        }
        A::Invert => 10u8.hash(h),
        A::Posterize { levels } => {
            11u8.hash(h);
            levels.hash(h);
        }
        A::Threshold { level } => {
            12u8.hash(h);
            hash_f32(*level, h);
        }
        A::GradientMap { stops, reverse } => {
            13u8.hash(h);
            stops.len().hash(h);
            for (p, c) in stops {
                hash_f32(*p, h);
                for v in c {
                    hash_f32(*v, h);
                }
            }
            reverse.hash(h);
        }
        A::SelectiveColor { ranges, relative } => {
            14u8.hash(h);
            for range in ranges {
                for v in range {
                    hash_f32(*v, h);
                }
            }
            relative.hash(h);
        }
        A::Auto { mode, clip } => {
            15u8.hash(h);
            mode.hash(h);
            hash_f32(*clip, h);
        }
        // The five wider spellings. They carry a different tag from their
        // narrow counterparts so a `LevelsFull` that happens to hold the same
        // numbers as a `Levels` still hashes differently — they are different
        // documents, and the cache key must say so.
        A::LevelsFull {
            composite,
            red,
            green,
            blue,
        } => {
            16u8.hash(h);
            for band in [composite, red, green, blue] {
                for v in band {
                    hash_f32(*v, h);
                }
            }
        }
        A::CurvesFull {
            composite,
            red,
            green,
            blue,
        } => {
            17u8.hash(h);
            for points in [composite, red, green, blue] {
                points.len().hash(h);
                for p in points {
                    hash_f32(p[0], h);
                    hash_f32(p[1], h);
                }
            }
        }
        A::ExposureFull {
            stops,
            offset,
            gamma,
        } => {
            18u8.hash(h);
            for v in [stops, offset, gamma] {
                hash_f32(*v, h);
            }
        }
        A::HueSaturationFull {
            hue,
            saturation,
            lightness,
            colorize,
        } => {
            19u8.hash(h);
            for v in [hue, saturation, lightness] {
                hash_f32(*v, h);
            }
            colorize.is_some().hash(h);
            for v in colorize.iter().flatten() {
                hash_f32(*v, h);
            }
        }
        A::ColorBalanceFull {
            shadows,
            midtones,
            highlights,
            preserve_luminosity,
        } => {
            20u8.hash(h);
            for band in [shadows, midtones, highlights] {
                for v in band {
                    hash_f32(*v, h);
                }
            }
            preserve_luminosity.hash(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_coords_cover_a_rect_including_negative_space() {
        let coords = tile_coords_for(PixelRect::new(-1, -1, 2, 2), 0);
        assert_eq!(coords.len(), 4, "{coords:?}");
        assert!(coords.contains(&TileCoord::new(-1, -1, 0)));
        assert!(coords.contains(&TileCoord::new(0, 0, 0)));
        assert!(tile_coords_for(PixelRect::new(0, 0, 0, 5), 0).is_empty());
        assert_eq!(
            tile_coords_for(PixelRect::new(0, 0, TILE_SIZE, TILE_SIZE), 3),
            vec![TileCoord::new(0, 0, 3)]
        );
    }

    #[test]
    fn expanding_a_rect_grows_it_on_every_side() {
        let r = expand_rect(PixelRect::new(10, 20, 4, 6), 3).unwrap();
        assert_eq!(r, PixelRect::new(7, 17, 10, 12));
        // Zero and negative margins are no-ops.
        let same = PixelRect::new(10, 20, 4, 6);
        assert_eq!(expand_rect(same, 0).unwrap(), same);
        assert_eq!(expand_rect(same, -5).unwrap(), same);
        // An absurd margin is refused, not wrapped.
        assert!(expand_rect(same, i64::from(u32::MAX)).is_err());
    }

    #[test]
    fn a_singular_transform_yields_no_preimage_instead_of_nan() {
        let squash = Affine2::from_scale(Vec2::new(1.0, 0.0));
        assert_eq!(
            preimage_rect(&squash, PixelRect::new(0, 0, 4, 4)),
            EMPTY_RECT
        );
        // And resampling through it produces transparency, not garbage.
        let src = Canvas::transparent(PixelRect::new(0, 0, 4, 4)).unwrap();
        let out = resample(&src, &squash, PixelRect::new(0, 0, 4, 4)).unwrap();
        assert!(out.pixels().iter().all(|p| *p == [0.0; 4]));
    }

    #[test]
    fn an_extreme_but_non_singular_transform_yields_no_preimage_rather_than_overflowing() {
        // Finite, and determinant exactly 1.0, so neither of the guards above
        // catches it — but a tile maps back to something 4e30 pixels wide. The
        // `f32 as i64` cast pins that at `i64::MAX`, and the margin arithmetic
        // on top of it must not wrap (release) or panic (debug) there.
        let t = Affine2::from_scale(Vec2::new(1e-30, 1e30));
        assert!(t.matrix2.determinant().is_finite());
        assert!((t.matrix2.determinant() - 1.0).abs() < 1e-6);
        assert_eq!(
            preimage_rect(&t, PixelRect::new(0, 0, 256, 256)),
            EMPTY_RECT
        );
        // The other axis, and a negative-going one, take the same path.
        let flip = Affine2::from_scale(Vec2::new(-1e30, -1e-30));
        assert_eq!(
            preimage_rect(&flip, PixelRect::new(-8, -8, 256, 256)),
            EMPTY_RECT
        );
    }

    #[test]
    fn a_translation_preimage_is_the_rect_moved_back() {
        let t = Affine2::from_translation(Vec2::new(10.0, -4.0));
        let r = preimage_rect(&t, PixelRect::new(0, 0, 8, 8));
        // Shifted by -10/+4 and grown by the 2px bilinear margin on each side.
        assert_eq!(r, PixelRect::new(-12, 2, 12, 12));
    }

    #[test]
    fn a_minified_preimage_is_huge_and_the_forward_image_is_small() {
        // The pre-image itself is deliberately *not* clamped: it is the honest
        // answer, and `render_via_transform` is what bounds the allocation.
        let t = Affine2::from_scale(Vec2::splat(0.02));
        let pre = preimage_rect(&t, PixelRect::new(0, 0, 256, 256));
        assert!(rect_area(pre) > crate::MAX_CANVAS_PIXELS, "{pre:?}");
        // Forward: a 1024x512 layer lands in about 21x11 pixels, plus margin.
        let img = image_rect(&t, PixelRect::new(0, 0, 1024, 512)).unwrap();
        assert_eq!(img, PixelRect::new(-2, -2, 25, 15));
        // A transform that maps a real rect out of the coordinate space has no
        // representable image, which reads as "anywhere".
        assert_eq!(
            image_rect(
                &Affine2::from_scale(Vec2::splat(1e30)),
                PixelRect::new(0, 0, 1024, 512)
            ),
            None
        );
    }

    #[test]
    fn rect_helpers_intersect_union_split_and_centre() {
        let a = PixelRect::new(0, 0, 10, 10);
        let b = PixelRect::new(5, -5, 10, 10);
        assert_eq!(intersect_rects(a, b), PixelRect::new(5, 0, 5, 5));
        assert_eq!(intersect_rects(a, PixelRect::new(50, 50, 2, 2)), EMPTY_RECT);
        assert_eq!(intersect_rects(a, EMPTY_RECT), EMPTY_RECT);
        assert_eq!(union_rects(a, b), PixelRect::new(0, -5, 15, 15));
        assert_eq!(union_rects(a, EMPTY_RECT), a);
        assert_eq!(union_rects(EMPTY_RECT, b), b);
        assert_eq!(clip_to(a, None), a);
        assert_eq!(clip_to(a, Some(b)), PixelRect::new(5, 0, 5, 5));

        // Splitting halves the longer axis and loses nothing.
        let (l, r) = split_rect(PixelRect::new(3, 7, 9, 4)).unwrap();
        assert_eq!(l, PixelRect::new(3, 7, 4, 4));
        assert_eq!(r, PixelRect::new(7, 7, 5, 4));
        let (top, bottom) = split_rect(PixelRect::new(3, 7, 4, 9)).unwrap();
        assert_eq!(top, PixelRect::new(3, 7, 4, 4));
        assert_eq!(bottom, PixelRect::new(3, 11, 4, 5));
        assert_eq!(split_rect(PixelRect::new(0, 0, 1, 1)), None);

        // A centre window keeps the middle, and is a no-op when it already fits.
        assert_eq!(
            centre_window(PixelRect::new(-100, -100, 200, 200), 8),
            PixelRect::new(-4, -4, 8, 8)
        );
        let small = PixelRect::new(0, 0, 3, 3);
        assert_eq!(centre_window(small, 8), small);
    }

    #[test]
    fn expanding_a_rect_at_the_edge_of_the_coordinate_space_is_refused_not_wrapped() {
        // `preimage_rect` saturates, so `i64::MIN` is a coordinate that really
        // reaches `expand_rect`.
        let r = PixelRect::new(i64::MIN, 0, 4, 4);
        assert!(expand_rect(r, 8).is_err());
        assert_eq!(max_margin(r), 0);
        // A normal rect keeps a usable margin, and it is the largest one that
        // still allocates.
        let normal = PixelRect::new(0, 0, 256, 256);
        let m = max_margin(normal);
        assert!(m > 1000, "{m}");
        assert!(expand_rect(normal, m).is_ok());
        assert!(expand_rect(normal, m + 1).is_err());
    }

    #[test]
    fn a_zero_radius_blur_is_the_identity_and_a_box_pass_preserves_the_mean() {
        let buf: Vec<f32> = (0..16).map(|i| i as f32 / 16.0).collect();
        assert_eq!(blur(buf.clone(), 4, 4, &[0; 3]), buf);

        // A constant field survives a box pass exactly — the normalisation is
        // by the full window even at the edges, so a uniform region stays
        // uniform in the interior.
        let flat = vec![1.0f32; 49];
        let out = box_pass(&flat, 7, 7, 1, true);
        assert!((out[3 * 7 + 3] - 1.0).abs() < 1e-6, "{:?}", out[24]);
    }

    #[test]
    fn boxes_for_gauss_grow_with_sigma_and_stay_odd_width() {
        let small = boxes_for_gauss(1.0);
        let large = boxes_for_gauss(8.0);
        assert!(small.iter().sum::<i64>() < large.iter().sum::<i64>());
        assert!(small.iter().all(|r| *r >= 0));
        // Radius r corresponds to width 2r+1, which is odd by construction.
        assert!(large.iter().all(|r| *r > 0));
    }

    #[test]
    fn tile_span_counts_the_coordinates_a_rect_touches() {
        assert_eq!(tile_span(EMPTY_RECT), 0);
        assert_eq!(tile_span(PixelRect::new(0, 0, TILE_SIZE, TILE_SIZE)), 1);
        assert_eq!(tile_span(PixelRect::new(-1, -1, 2, 2)), 4);
        assert_eq!(tile_span(PixelRect::new(0, 0, TILE_SIZE * 3, TILE_SIZE)), 3);
        // The point of the count: a minified layer's sampled rect spans far
        // more coordinates than a cache key should enumerate one by one.
        assert!(tile_span(PixelRect::new(0, 0, 300_000, 300_000)) > MAX_KEYED_TILES);
        // And it does not overflow at the ends of the coordinate space.
        assert!(tile_span(PixelRect::new(i64::MIN, i64::MIN, u32::MAX, u32::MAX)) > 0);
    }

    #[test]
    fn content_bounds_follow_what_a_layer_actually_stores() {
        use crate::testkit::TestDoc;
        use layer_model::{AdjustmentKind, Layer, TextLayer};

        let mut t = TestDoc::linear(1024, 256);
        // One tile, over at tile column 1: the layer's content is 256..512,
        // not the whole document.
        let raster = t.push_raster("Patch");
        t.paint_tile(raster, TileCoord::new(1, 0, 0), [255, 0, 0, 255]);
        let empty = t.push_raster("Nothing stored");
        let adj = t.push_adjustment("Exposure", AdjustmentKind::Exposure { stops: 1.0 });
        let text = t.push(Layer::with_kind(
            "Words",
            LayerKind::Text(TextLayer::default()),
        ));
        // A group holding the raster layer, translated by 1000 in x.
        let group = t.push_group("Group");
        let moved = t.push_child(group, Layer::raster("Moved"));
        t.paint_tile(moved, TileCoord::new(0, 0, 0), [0, 0, 255, 255]);
        t.doc.layers.get_mut(moved).unwrap().transform =
            Affine2::from_translation(Vec2::new(1000.0, 0.0));

        let ctx = Ctx::new(&t.doc, &t.src, 0, CompositeOptions::default()).unwrap();
        let of = |id| ctx.content_bounds(t.doc.layers.get(id).unwrap());
        assert_eq!(of(raster), Some(PixelRect::new(256, 0, 256, 256)));
        assert_eq!(of(empty), Some(EMPTY_RECT), "no tiles, no content");
        assert_eq!(of(adj), Some(EMPTY_RECT), "an adjustment paints nothing");
        assert_eq!(of(text), Some(EMPTY_RECT), "no text rasterizer yet");
        // The child's own tile is 0..256; the group sees it where the child's
        // transform puts it, plus the resampler's margin.
        assert_eq!(of(group), Some(PixelRect::new(998, -2, 260, 260)));

        // A level the tiles were not stored at holds nothing.
        let l1 = Ctx::new(&t.doc, &t.src, 1, CompositeOptions::default()).unwrap();
        assert_eq!(
            l1.content_bounds(t.doc.layers.get(raster).unwrap()),
            Some(EMPTY_RECT)
        );
    }

    #[test]
    fn an_unallocatable_feather_is_clamped_to_a_reach_that_fits() {
        use crate::source::MemoryTileSource;
        use editor_core::Document;
        use layer_model::{LayerMask, MaskId};

        let doc = Document::new(64, 64, "test");
        let src = MemoryTileSource::new();
        let ctx = Ctx::new(&doc, &src, 0, CompositeOptions::default()).unwrap();
        let base = PixelRect::new(0, 0, TILE_SIZE, TILE_SIZE);

        // A feather that fits is passed through untouched, margin and all.
        let mut mask = LayerMask::new(MaskId::new());
        mask.set_feather_px(12.0).unwrap();
        let (rect, radii) = ctx.mask_sample(&mask, base);
        let total: i64 = radii.iter().sum();
        assert!(total > 0);
        assert_eq!(rect, expand_rect(base, total).unwrap());

        // A feather nobody could allocate the buffer for is scaled down rather
        // than failing the frame — and the rect still matches the radii, which
        // is what keeps the blur from reading past its own buffer and so makes
        // the answer independent of the region asked for.
        mask.set_feather_px(1.0e7).unwrap();
        let (rect, radii) = ctx.mask_sample(&mask, base);
        let clamped: i64 = radii.iter().sum();
        let asked: i64 = ctx.feather_radii(1.0e7).iter().sum();
        assert!(clamped > 0, "some feather survives");
        assert!(
            clamped < asked / 100,
            "and far less than the {asked} asked for"
        );
        assert_eq!(rect, expand_rect(base, clamped).unwrap());
        assert!(Canvas::area(rect).is_ok(), "the sample buffer allocates");
        // As much as fits, give or take the rounding of three integer radii.
        let max = max_margin(base);
        assert!(clamped <= max && clamped >= max - 3, "{clamped} vs {max}");
    }

    #[test]
    fn bilinear_interpolates_between_neighbours() {
        let mut c = Canvas::transparent(PixelRect::new(0, 0, 2, 1)).unwrap();
        c.set(0, 0, [0.0, 0.0, 0.0, 1.0]);
        c.set(1, 0, [1.0, 0.0, 0.0, 1.0]);
        let mid = bilinear(&c, 0.5, 0.0);
        assert!((mid[0] - 0.5).abs() < 1e-6, "{mid:?}");
        // Exactly on a sample is that sample.
        assert_eq!(bilinear(&c, 1.0, 0.0), [1.0, 0.0, 0.0, 1.0]);
        // Far outside is transparent, and a non-finite coordinate too.
        assert_eq!(bilinear(&c, 1e30, 0.0), [0.0; 4]);
        assert_eq!(bilinear(&c, f32::NAN, 0.0), [0.0; 4]);
    }
}
