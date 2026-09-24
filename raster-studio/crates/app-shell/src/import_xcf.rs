//! W10-F: a GIMP `.xcf` opened as a *layered* document.
//!
//! `raster::codec::formats::xcf` parses the layer tree (and decodes the
//! flattened composite for the codec's flat route); this module turns that
//! tree into the document model the way [`super::document_from_psd`] does a
//! `.psd`: one raster layer per GIMP layer at its own offset (off-canvas
//! pixels kept), one group per layer group, with the name, opacity,
//! visibility and blend mode of each. What does not map is written into the
//! [`PsdNotes`] the open route shows as the import report:
//!
//! * GIMP modes with no equivalent here (Grain extract, Grain merge, and any
//!   code the reader does not know) open as Normal, by name;
//! * Soft light opens as Soft Light, whose formula differs slightly from
//!   GIMP's;
//! * an applied layer mask is multiplied into the layer's alpha, not kept as
//!   a mask;
//! * a greyscale image opens as RGB, an 8-bit linear one converted to sRGB;
//! * the reader's own notes (a floating selection that was left out, a layer
//!   whose item path pointed nowhere) are passed on. Its notes about
//!   Dissolve and pass-through groups are dropped: both map exactly here.

use std::io::Read;
use std::path::Path;

use compositor::MemoryTileSource;
use editor_core::pixels::{PixelKey, TileDelta};
use editor_core::{Document, History};
use layer_model::{BlendMode, GroupBlending, GroupLayer, Layer, LayerId, LayerKind};
use raster::codec::formats::xcf::{self, XcfDocument, XcfLayer, XcfMode};

use super::{tile_edits_for_rgba, ImportError, ImportedDocument, PsdImport, PsdNotes};

/// `true` when the file at `path` is a GIMP XCF, by content (not by name).
pub fn looks_like_xcf(path: &Path) -> bool {
    let mut head = [0u8; 9];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && xcf::looks_like_xcf(&head)
}

/// Read an `.xcf` from disk, bounded by what the decode limits allow.
pub fn read_xcf_bytes(path: &Path) -> Result<Vec<u8>, ImportError> {
    let limits = raster::ImportLimits::default();
    let cap = limits.max_alloc_bytes.saturating_mul(4);
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(raster::CodecError::LimitExceeded(format!(
            "the file is larger than {cap} bytes"
        ))
        .into());
    }
    Ok(bytes)
}

/// The document model's blend mode for a GIMP mode, or `None` when there is
/// no equivalent (the layer then opens as Normal and is reported).
fn blend_mode(mode: XcfMode, code: u32) -> Option<BlendMode> {
    if code == 1 {
        return Some(BlendMode::Dissolve);
    }
    Some(match mode {
        XcfMode::Normal | XcfMode::PassThrough => BlendMode::Normal,
        XcfMode::Multiply => BlendMode::Multiply,
        XcfMode::Screen => BlendMode::Screen,
        XcfMode::Overlay => BlendMode::Overlay,
        XcfMode::SoftLight => BlendMode::SoftLight,
        XcfMode::HardLight => BlendMode::HardLight,
        XcfMode::Difference => BlendMode::Difference,
        XcfMode::Addition => BlendMode::LinearDodge,
        XcfMode::Subtract => BlendMode::Subtract,
        XcfMode::Darken => BlendMode::Darken,
        XcfMode::Lighten => BlendMode::Lighten,
        XcfMode::Divide => BlendMode::Divide,
        XcfMode::Dodge => BlendMode::ColorDodge,
        XcfMode::Burn => BlendMode::ColorBurn,
        XcfMode::GrainExtract | XcfMode::GrainMerge | XcfMode::Unsupported(_) => return None,
    })
}

fn mode_name(mode: XcfMode) -> String {
    match mode {
        XcfMode::GrainExtract => "Grain extract".to_string(),
        XcfMode::GrainMerge => "Grain merge".to_string(),
        XcfMode::Unsupported(code) => format!("blend mode {code}"),
        other => format!("{other:?}"),
    }
}

struct Build<'a> {
    xcf: &'a XcfDocument<'a>,
    document: Document,
    tiles: MemoryTileSource,
    notes: PsdNotes,
    limits: raster::ImportLimits,
    /// Pixel bytes decoded so far, against [`Build::budget`].
    decoded: u64,
    budget: u64,
}

impl Build<'_> {
    /// Insert `items` (top first, as GIMP lists them) under `parent`.
    fn items(&mut self, items: &[XcfLayer], parent: Option<LayerId>) -> Result<(), ImportError> {
        for (index, item) in items.iter().enumerate() {
            let mut layer = if item.is_group {
                Layer::with_kind(
                    item.name.clone(),
                    LayerKind::Group(GroupLayer {
                        children: Vec::new(),
                        collapsed: false,
                        blending: if item.mode == XcfMode::PassThrough {
                            GroupBlending::PassThrough
                        } else {
                            GroupBlending::Isolated
                        },
                    }),
                )
            } else {
                Layer::raster(item.name.clone())
            };
            layer.visible = item.visible;
            layer.opacity = item.opacity.clamp(0.0, 1.0);
            layer.blend_mode = match blend_mode(item.mode, item.mode_code) {
                Some(mode) => mode,
                None => {
                    self.notes.push(format!(
                        "layer {:?} uses GIMP's {}, which has no equivalent here; it opened \
                         as Normal",
                        item.name,
                        mode_name(item.mode)
                    ));
                    BlendMode::Normal
                }
            };
            if item.mode == XcfMode::SoftLight {
                self.notes.push(format!(
                    "layer {:?} opened as Soft Light, whose formula differs slightly from \
                     GIMP's",
                    item.name
                ));
            }
            let id = self.document.layers.insert_at(layer, parent, index)?;
            if item.is_group {
                self.items(&item.children, Some(id))?;
                continue;
            }
            if item.width == 0 || item.height == 0 {
                continue;
            }
            self.decoded = self
                .decoded
                .saturating_add(u64::from(item.width) * u64::from(item.height) * 4);
            if self.decoded > self.budget {
                return Err(raster::CodecError::LimitExceeded(format!(
                    "the XCF's layers hold more than {} bytes of pixels",
                    self.budget
                ))
                .into());
            }
            let rgba = self.xcf.layer_pixels(item, self.limits)?;
            if item.has_applied_mask {
                self.notes.push(format!(
                    "layer {:?}'s mask was applied to its pixels",
                    item.name
                ));
            }
            let right = i64::from(item.x) + i64::from(item.width);
            let bottom = i64::from(item.y) + i64::from(item.height);
            let (Ok(right), Ok(bottom)) = (i32::try_from(right), i32::try_from(bottom)) else {
                return Err(raster::CodecError::LimitExceeded(format!(
                    "layer {:?} lies outside the addressable canvas",
                    item.name
                ))
                .into());
            };
            let rect = psd::Rect::new(item.x, item.y, right, bottom);
            let edits = tile_edits_for_rgba(&rgba, rect, &mut self.tiles);
            if !edits.is_empty() {
                let delta = TileDelta::new(edits).map_err(editor_core::CommandError::from)?;
                self.document.pixels.apply(PixelKey::Layer(id), &delta);
            }
        }
        Ok(())
    }
}

/// An `.xcf` turned into a layered document, and what did not map.
pub fn document_from_xcf(
    bytes: &[u8],
    title: &str,
    history_depth: usize,
) -> Result<PsdImport, ImportError> {
    let limits = raster::ImportLimits::default();
    let parsed = xcf::read(bytes, limits)?;
    let (width, height) = (parsed.width, parsed.height);
    if width == 0 || height == 0 || !editor_core::canvas_size_is_supported(width, height) {
        return Err(raster::CodecError::LimitExceeded(format!(
            "a {width}x{height} canvas is not one this build can open"
        ))
        .into());
    }
    let mut notes = PsdNotes::default();
    if parsed.grey {
        notes.push("a greyscale image was opened as RGB");
    }
    // GIMP 2.8's modes (codes below 23) blend in gamma space in GIMP; here
    // every layer blends in linear light, as GIMP 2.10's own modes do.
    let mut legacy = false;
    let mut walk: Vec<&XcfLayer> = parsed.layers.iter().collect();
    while let Some(item) = walk.pop() {
        legacy |= item.mode_code < 23;
        walk.extend(item.children.iter());
    }
    if legacy {
        notes.push(
            "this file uses GIMP 2.8 (legacy) layer modes, which GIMP blends in gamma space; \
             they opened as the matching modes here, which blend in linear light, so \
             partly transparent or blended layers can look lighter",
        );
    }
    // The reader's notes, less the two this document model maps exactly.
    let mut exact = Vec::new();
    let mut stack: Vec<&XcfLayer> = parsed.layers.iter().collect();
    while let Some(item) = stack.pop() {
        if item.mode_code == 1 {
            exact.push(format!("layer {:?} uses Dissolve", item.name));
        }
        if item.mode == XcfMode::PassThrough {
            exact.push(format!("group {:?} passes through", item.name));
        }
        stack.extend(item.children.iter());
    }
    for note in &parsed.notes {
        if !exact.iter().any(|prefix| note.starts_with(prefix.as_str())) {
            notes.push(note.clone());
        }
    }

    let mut build = Build {
        xcf: &parsed,
        document: Document::new(width, height, title),
        tiles: MemoryTileSource::new(),
        notes,
        limits,
        decoded: 0,
        budget: limits.max_alloc_bytes.saturating_mul(4),
    };
    build.items(&parsed.layers, None)?;
    let Build {
        mut document,
        tiles,
        notes,
        ..
    } = build;

    let order = document.layers.iter_depth_first();
    let active = match order
        .iter()
        .copied()
        .find(|id| document.layers.get(*id).is_some_and(|l| !l.is_group()))
        .or_else(|| order.first().copied())
    {
        Some(id) => id,
        None => {
            // No layers at all: one empty raster layer, as GIMP shows it.
            document
                .layers
                .insert_at(Layer::raster("Background"), None, 0)?
        }
    };
    document
        .set_active_layer(Some(active))
        .expect("the active layer was taken from this tree");
    // Opening a file is not an edit.
    document.mark_saved();
    Ok(PsdImport {
        imported: ImportedDocument {
            document,
            history: History::with_limit(history_depth),
            tiles,
            layer: active,
        },
        notes,
        merged_preview: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../../raster/src/formats/testdata/layered_modes.xcf");

    #[test]
    fn malformed_xcf_bytes_error_and_never_panic() {
        for n in 0..FIXTURE.len() {
            let _ = document_from_xcf(&FIXTURE[..n], "t", 8);
        }
        let mut flipped = FIXTURE.to_vec();
        for i in 9..flipped.len() {
            let saved = flipped[i];
            flipped[i] ^= 0xa5;
            let _ = document_from_xcf(&flipped, "t", 8);
            flipped[i] = saved;
        }
        assert!(document_from_xcf(b"gimp xcf ", "t", 8).is_err());
    }
}
