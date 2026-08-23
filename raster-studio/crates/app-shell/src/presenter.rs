//! Getting the compositor's answer onto the screen.
//!
//! The pipeline is `document -> compositor -> visible tiles -> GPU -> screen`,
//! and this module is the third arrow. It holds one document-sized RGBA8
//! texture and keeps it in step with the document:
//!
//! * the first frame (and any canvas resize) composites the whole canvas and
//!   uploads it;
//! * every frame after that uploads only the rectangles of the tiles the edits
//!   touched ([`crate::dirty`]), and the compositor recomputes only those tiles
//!   because its cache is keyed by the inputs that produced each one.
//!
//! There is no second source of pixels. The canvas draws this texture and
//! nothing else, so what is on screen is always the composite of the document.

use raster::{PixelRect, TileCoord, TILE_SIZE};
use render::{GpuContext, GpuTexture};

use crate::doc::{DocumentError, DocumentId, OpenDocument};

/// The part of `coord`'s tile that lies inside a `width` x `height` canvas.
///
/// `None` for a tile entirely outside it — a layer may hold tiles beyond the
/// canvas edge (they come back if the canvas grows), and uploading one would
/// be a write past the texture.
pub fn tile_upload_rect(coord: TileCoord, width: u32, height: u32) -> Option<PixelRect> {
    if coord.level != 0 {
        return None;
    }
    let (ox, oy) = coord.pixel_origin();
    let x0 = ox.max(0);
    let y0 = oy.max(0);
    let x1 = (ox + TILE_SIZE as i64).min(width as i64);
    let y1 = (oy + TILE_SIZE as i64).min(height as i64);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
}

/// What one [`CanvasPresenter::sync`] did — enough for a caller to know whether
/// the renderer has to be re-pointed, and enough for a test to see that a small
/// edit stayed small.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncReport {
    /// The texture was created or recreated; the renderer must bind it again.
    pub texture_replaced: bool,
    /// Whole-canvas uploads performed.
    pub full_uploads: u32,
    /// Individual tile rectangles uploaded.
    pub tile_uploads: u32,
}

impl SyncReport {
    pub fn did_nothing(&self) -> bool {
        !self.texture_replaced && self.full_uploads == 0 && self.tile_uploads == 0
    }
}

/// Whether the texture has to be built from scratch.
///
/// `showing` is what the presenter currently holds: the document it was built
/// for and its canvas size. **The document id is half the answer**: one
/// presenter serves the whole window, so switching to another tab of the same
/// dimensions would otherwise keep showing the previous document's pixels — the
/// incremental path only uploads *dirty* tiles, and a freshly activated
/// document usually has none.
pub fn needs_rebuild(
    showing: Option<(DocumentId, (u32, u32))>,
    doc: DocumentId,
    size: (u32, u32),
) -> bool {
    showing != Some((doc, size))
}

/// The document-sized GPU texture the canvas samples.
#[derive(Default)]
pub struct CanvasPresenter {
    texture: Option<GpuTexture>,
    /// The document and canvas size [`CanvasPresenter::texture`] holds.
    showing: Option<(DocumentId, (u32, u32))>,
}

impl CanvasPresenter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn texture(&self) -> Option<&GpuTexture> {
        self.texture.as_ref()
    }

    /// The canvas size the current texture was built for.
    pub fn size(&self) -> (u32, u32) {
        self.showing.map(|(_, size)| size).unwrap_or((1, 1))
    }

    /// The document the current texture was built for.
    pub fn showing(&self) -> Option<DocumentId> {
        self.showing.map(|(id, _)| id)
    }

    /// Bring the texture in step with `doc`.
    pub fn sync(
        &mut self,
        gpu: &GpuContext,
        doc: &mut OpenDocument,
    ) -> Result<SyncReport, DocumentError> {
        let width = doc.document.width().max(1);
        let height = doc.document.height().max(1);
        let dirty = doc.take_dirty();

        if self.texture.is_none() || needs_rebuild(self.showing, doc.id(), (width, height)) {
            let rgba = doc.composite(PixelRect::new(0, 0, width, height))?;
            self.texture = Some(GpuTexture::from_rgba8(
                gpu,
                width,
                height,
                &rgba,
                "document-composite",
            ));
            self.showing = Some((doc.id(), (width, height)));
            return Ok(SyncReport {
                texture_replaced: true,
                full_uploads: 1,
                tile_uploads: 0,
            });
        }
        if dirty.is_empty() {
            return Ok(SyncReport::default());
        }

        let texture = self.texture.as_ref().expect("checked immediately above");
        let mut report = SyncReport::default();
        if dirty.is_all() {
            let rgba = doc.composite(PixelRect::new(0, 0, width, height))?;
            write_rect(gpu, texture, PixelRect::new(0, 0, width, height), &rgba);
            report.full_uploads = 1;
        } else {
            for coord in dirty.tiles() {
                let Some(rect) = tile_upload_rect(coord, width, height) else {
                    continue;
                };
                let rgba = doc.composite(rect)?;
                write_rect(gpu, texture, rect, &rgba);
                report.tile_uploads += 1;
            }
            if report.tile_uploads == 0 {
                return Ok(SyncReport::default());
            }
        }

        // Level 0 changed, so the minified levels the canvas samples while
        // zoomed out are now stale. Rebuilding the chain is one render pass per
        // level on the GPU; leaving it out makes an edit invisible until the
        // user zooms in.
        if texture.mip_level_count > 1 {
            gpu.mip_generator(texture.texture.format()).generate(
                gpu,
                &texture.texture,
                texture.mip_level_count,
            );
        }
        Ok(report)
    }
}

fn write_rect(gpu: &GpuContext, texture: &GpuTexture, rect: PixelRect, rgba8: &[u8]) {
    debug_assert_eq!(
        rgba8.len(),
        (rect.width as usize) * (rect.height as usize) * 4
    );
    gpu.queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &texture.texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: rect.x as u32,
                y: rect.y as u32,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        rgba8,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(4 * rect.width),
            rows_per_image: Some(rect.height),
        },
        wgpu::Extent3d {
            width: rect.width,
            height: rect.height,
            depth_or_array_layers: 1,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_tile_inside_the_canvas_uploads_whole() {
        let r = tile_upload_rect(TileCoord::new(1, 0, 0), TILE_SIZE * 3, TILE_SIZE * 2).unwrap();
        assert_eq!(r, PixelRect::new(TILE_SIZE as i64, 0, TILE_SIZE, TILE_SIZE));
    }

    #[test]
    fn an_edge_tile_is_clipped_to_the_canvas() {
        // A 300x200 canvas: the tile at (1,0) holds only 44 real columns.
        let r = tile_upload_rect(TileCoord::new(1, 0, 0), 300, 200).unwrap();
        assert_eq!(r, PixelRect::new(TILE_SIZE as i64, 0, 300 - TILE_SIZE, 200));
        let r = tile_upload_rect(TileCoord::new(0, 0, 0), 300, 200).unwrap();
        assert_eq!(r, PixelRect::new(0, 0, TILE_SIZE, 200));
    }

    #[test]
    fn a_tile_outside_the_canvas_uploads_nothing() {
        // Off the right edge, off the bottom, and in negative space — a layer
        // may hold tiles there, and writing one would run past the texture.
        assert_eq!(tile_upload_rect(TileCoord::new(2, 0, 0), 300, 200), None);
        assert_eq!(tile_upload_rect(TileCoord::new(0, 1, 0), 300, 200), None);
        assert_eq!(tile_upload_rect(TileCoord::new(-1, 0, 0), 300, 200), None);
        assert_eq!(tile_upload_rect(TileCoord::new(0, -1, 0), 300, 200), None);
    }

    #[test]
    fn a_negative_tile_that_overlaps_the_origin_is_clipped_not_dropped() {
        // Not reachable from a document whose layers start at the origin, but
        // the clamp is what makes that a property rather than an assumption.
        let r = tile_upload_rect(TileCoord::new(0, 0, 0), 10, 10).unwrap();
        assert_eq!(r, PixelRect::new(0, 0, 10, 10));
    }

    #[test]
    fn only_level_zero_is_a_rectangle_of_this_texture() {
        assert_eq!(tile_upload_rect(TileCoord::new(0, 0, 1), 1024, 1024), None);
    }

    #[test]
    fn switching_tabs_rebuilds_even_when_the_canvas_size_matches() {
        // One presenter serves the window. Two documents of the same size are
        // the case where "same size, nothing dirty" would leave the previous
        // document's pixels on screen under the new document's tab.
        let a = DocumentId(1);
        let b = DocumentId(2);
        assert!(needs_rebuild(None, a, (100, 100)), "nothing shown yet");
        assert!(!needs_rebuild(Some((a, (100, 100))), a, (100, 100)));
        assert!(
            needs_rebuild(Some((a, (100, 100))), b, (100, 100)),
            "a different document of identical size must still rebuild"
        );
        assert!(
            needs_rebuild(Some((a, (100, 100))), a, (100, 101)),
            "a resized canvas needs a new texture"
        );
    }

    #[test]
    fn a_report_that_did_nothing_says_so() {
        assert!(SyncReport::default().did_nothing());
        assert!(!SyncReport {
            tile_uploads: 1,
            ..Default::default()
        }
        .did_nothing());
        assert!(!SyncReport {
            texture_replaced: true,
            ..Default::default()
        }
        .did_nothing());
    }
}
