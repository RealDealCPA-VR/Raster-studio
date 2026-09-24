//! W9-D: the composite a retouching stroke reads when its Sample choice is
//! Current & Below or All Layers.
//!
//! [`tools::tool::CompositeSampler`] is the tools crate's seam; this is the
//! shell's answer. It is built at the press of such a stroke (declared as a
//! child module of `tool_input`, which fills
//! [`tools::ToolContext::composite_sampler`] with it) from the document as
//! committed then — a clone of the document and of its tile store, which is a
//! map of shared pointers, not a copy of the pixels — and composites lazily:
//! only the tiles the stroke's reads cover, each once per stroke (cached),
//! through the same compositor the canvas uses.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use compositor::{Canvas, CompositeOptions, MemoryTileSource};
use editor_core::Document;
use layer_model::LayerId;
use raster::{PixelRect, TILE_SIZE};
use tools::tool::{CompositeSampler, SampleLayers};
use tools::ToolError;

/// The shell's composite for one stroke.
pub(crate) struct DocumentCompositeSampler {
    /// The document as committed at the press.
    document: Document,
    /// Its tile bytes (shared pointers).
    tiles: MemoryTileSource,
    /// The layer the stroke paints: Current & Below composites it and what
    /// lies under it.
    active: Option<LayerId>,
    /// Paint-target pixel → document point (the inverse of the shell's
    /// `sample_to_layer`); `None` is the identity.
    target_to_document: Option<glam::Affine2>,
    /// [`Self::document`] with every layer above [`Self::active`] hidden,
    /// built the first time Current & Below is asked for.
    below: OnceLock<Document>,
    /// Composited document tiles, keyed by (Current & Below?, tile x, tile y).
    cache: Mutex<HashMap<(bool, i64, i64), Canvas>>,
}

impl DocumentCompositeSampler {
    pub(crate) fn new(
        document: &Document,
        tiles: &MemoryTileSource,
        active: Option<LayerId>,
        sample_to_layer: Option<glam::Affine2>,
    ) -> Self {
        Self {
            document: document.clone(),
            tiles: tiles.clone(),
            active,
            target_to_document: sample_to_layer
                .map(|m| m.inverse())
                .filter(|m| *m != glam::Affine2::IDENTITY),
            below: OnceLock::new(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The document a scope composites.
    fn scoped(&self, layers: SampleLayers) -> &Document {
        match layers {
            SampleLayers::CurrentAndBelow => self
                .below
                .get_or_init(|| at_and_below(&self.document, self.active)),
            _ => &self.document,
        }
    }

    /// The composite of `layers` over the DOCUMENT rect `region`, assembled
    /// from cached whole tiles.
    fn document_region(
        &self,
        layers: SampleLayers,
        region: PixelRect,
    ) -> Result<Canvas, ToolError> {
        let mut out = Canvas::transparent(region).map_err(|_| ToolError::Degenerate)?;
        if region.is_empty() {
            return Ok(out);
        }
        let below = layers == SampleLayers::CurrentAndBelow;
        let ts = i64::from(TILE_SIZE);
        let (tx0, ty0) = (region.x.div_euclid(ts), region.y.div_euclid(ts));
        let (tx1, ty1) = (
            (region.right() - 1).div_euclid(ts),
            (region.bottom() - 1).div_euclid(ts),
        );
        let doc = self.scoped(layers);
        let mut cache = self.cache.lock().map_err(|_| ToolError::Degenerate)?;
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let tile = match cache.entry((below, tx, ty)) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(e) => {
                        let rect = PixelRect::new(tx * ts, ty * ts, TILE_SIZE, TILE_SIZE);
                        e.insert(
                            compositor::composite_region(
                                doc,
                                &self.tiles,
                                rect,
                                0,
                                CompositeOptions::default(),
                            )
                            .map_err(|_| ToolError::Degenerate)?,
                        )
                    }
                };
                out.blit_from(tile);
            }
        }
        Ok(out)
    }
}

impl CompositeSampler for DocumentCompositeSampler {
    fn composite(
        &self,
        layers: SampleLayers,
        rect: PixelRect,
    ) -> Result<filters::FilterBuffer, ToolError> {
        let pixels = match self.target_to_document {
            None => self.document_region(layers, rect)?.pixels().to_vec(),
            Some(m) => {
                // A moved/scaled layer: each target pixel's centre is mapped
                // into the document and the composite read there (nearest).
                let corners = [
                    (rect.x, rect.y),
                    (rect.right(), rect.y),
                    (rect.x, rect.bottom()),
                    (rect.right(), rect.bottom()),
                ]
                .map(|(x, y)| m.transform_point2(glam::Vec2::new(x as f32, y as f32)));
                let lo = corners
                    .iter()
                    .fold(glam::Vec2::splat(f32::MAX), |a, c| a.min(*c));
                let hi = corners
                    .iter()
                    .fold(glam::Vec2::splat(f32::MIN), |a, c| a.max(*c));
                if !lo.is_finite() || !hi.is_finite() {
                    return Err(ToolError::Degenerate);
                }
                let (x0, y0) = (lo.x.floor() as i64 - 1, lo.y.floor() as i64 - 1);
                let (x1, y1) = (hi.x.ceil() as i64 + 1, hi.y.ceil() as i64 + 1);
                let region = PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32);
                let canvas = self.document_region(layers, region)?;
                let mut px = Vec::with_capacity(rect.width as usize * rect.height as usize);
                for y in rect.y..rect.bottom() {
                    for x in rect.x..rect.right() {
                        let d = m.transform_point2(glam::Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
                        px.push(canvas.get(d.x.floor() as i64, d.y.floor() as i64));
                    }
                }
                px
            }
        };
        Ok(filters::FilterBuffer::from_pixels(
            rect.width,
            rect.height,
            pixels,
        )?)
    }
}

/// `document` with every layer composited above `active` hidden — the layers
/// before it in depth-first (top-most first) order that are not its
/// ancestors. Its ancestors and its own subtree stay as they were.
fn at_and_below(document: &Document, active: Option<LayerId>) -> Document {
    let mut out = document.clone();
    let Some(active) = active else {
        return out;
    };
    let mut ancestors = Vec::new();
    let mut at = document.layers.parent_of(active);
    while let Some(parent) = at {
        if ancestors.contains(&parent) {
            break;
        }
        ancestors.push(parent);
        at = document.layers.parent_of(parent);
    }
    for id in document.layers.iter_depth_first() {
        if id == active {
            break;
        }
        if ancestors.contains(&id) {
            continue;
        }
        if let Some(layer) = out.layers.get_mut(id) {
            layer.visible = false;
        }
    }
    out
}
