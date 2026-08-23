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
//!
//! # Channel isolation lives here
//!
//! The Channels panel's component toggles are a *view* setting: hiding the red
//! channel must change what is on screen without changing the file. This is the
//! one place both are true at once — the composite is already in hand and the
//! upload has not happened yet — so [`ChannelMask`] is applied to every buffer
//! on its way to the texture. See [`CanvasPresenter::set_channel_mask`].

use raster::{PixelRect, TileCoord, TILE_SIZE};
use render::{GpuContext, GpuTexture};

use crate::doc::{DocumentError, DocumentId, OpenDocument};

/// Which colour components of the composite reach the screen.
///
/// The Channels panel's row per component, as a value the upload path can
/// apply. Alpha is never masked: a channel toggle answers "what colour is
/// shown", not "what is transparent", and zeroing alpha would dissolve the
/// image instead of isolating a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelMask {
    /// Red, green, blue — the three components every colour space this build
    /// supports has, which `the_mask_covers_every_component_the_panel_lists`
    /// pins against [`ui::panels::channels::component_names`].
    pub components: [bool; 3],
}

impl Default for ChannelMask {
    fn default() -> Self {
        Self::ALL
    }
}

impl ChannelMask {
    /// Everything visible — the composite as the document defines it.
    pub const ALL: Self = Self {
        components: [true; 3],
    };

    /// The mask the Channels panel currently describes.
    pub fn from_channels(channels: &ui::panels::channels::ChannelsState) -> Self {
        let mut components = [true; 3];
        for (i, slot) in components.iter_mut().enumerate() {
            *slot = channels.component_visible(i);
        }
        Self { components }
    }

    /// `true` when nothing is hidden, so the upload path can skip the pass.
    pub fn is_identity(&self) -> bool {
        *self == Self::ALL
    }

    /// Zero the hidden components of an RGBA8 buffer, in place.
    pub fn apply(&self, rgba8: &mut [u8]) {
        if self.is_identity() {
            return;
        }
        for pixel in rgba8.chunks_exact_mut(4) {
            for (component, visible) in pixel.iter_mut().zip(self.components) {
                if !visible {
                    *component = 0;
                }
            }
        }
    }
}

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
    /// Which components of the composite are being shown.
    mask: ChannelMask,
    /// `true` when [`CanvasPresenter::mask`] changed since the last upload, so
    /// the whole canvas has to be sent again. A channel toggle dirties no tile
    /// — the document did not move — so without this flag the change would
    /// appear only where the user next painted.
    mask_dirty: bool,
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

    /// The components of the composite currently reaching the screen.
    pub fn channel_mask(&self) -> ChannelMask {
        self.mask
    }

    /// Show only these colour components.
    ///
    /// This is what makes the Channels panel's eye toggles change pixels rather
    /// than only their own glyph: the shell reads the workspace's
    /// [`ui::panels::channels::ChannelsState`] every frame and hands the result
    /// here, and the next [`CanvasPresenter::sync`] re-uploads the canvas
    /// through it. Returns `true` when the mask actually moved.
    pub fn set_channel_mask(&mut self, mask: ChannelMask) -> bool {
        if self.mask == mask {
            return false;
        }
        self.mask = mask;
        self.mask_dirty = true;
        true
    }

    /// Bring the texture in step with `doc`.
    pub fn sync(
        &mut self,
        gpu: &GpuContext,
        doc: &mut OpenDocument,
    ) -> Result<SyncReport, DocumentError> {
        let width = doc.document.width().max(1);
        let height = doc.document.height().max(1);
        let mut dirty = doc.take_dirty();
        if std::mem::take(&mut self.mask_dirty) {
            // A channel toggle changes every pixel and dirties no tile.
            dirty.mark_all();
        }

        if self.texture.is_none() || needs_rebuild(self.showing, doc.id(), (width, height)) {
            let rgba = self.composite_masked(doc, PixelRect::new(0, 0, width, height))?;
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

        let mut report = SyncReport::default();
        if dirty.is_all() {
            let whole = PixelRect::new(0, 0, width, height);
            let rgba = self.composite_masked(doc, whole)?;
            let texture = self.texture.as_ref().expect("checked immediately above");
            write_rect(gpu, texture, whole, &rgba);
            report.full_uploads = 1;
        } else {
            for coord in dirty.tiles() {
                let Some(rect) = tile_upload_rect(coord, width, height) else {
                    continue;
                };
                let rgba = self.composite_masked(doc, rect)?;
                let texture = self.texture.as_ref().expect("checked immediately above");
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
        let texture = self.texture.as_ref().expect("checked immediately above");
        if texture.mip_level_count > 1 {
            gpu.mip_generator(texture.texture.format()).generate(
                gpu,
                &texture.texture,
                texture.mip_level_count,
            );
        }
        Ok(report)
    }

    /// Composite a region and hide the channels the user turned off.
    ///
    /// One function so no upload path can forget: the whole-canvas rebuild, the
    /// whole-canvas re-upload and the per-tile upload all go through it.
    fn composite_masked(
        &self,
        doc: &mut OpenDocument,
        rect: PixelRect,
    ) -> Result<Vec<u8>, DocumentError> {
        let mut rgba = doc.composite(rect)?;
        self.mask.apply(&mut rgba);
        Ok(rgba)
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

    /// A flat colour whose three components are all different, so a mask that
    /// zeroed the wrong one could not pass.
    fn swatch(width: u32, height: u32) -> crate::import::DecodedImage {
        let mut rgba8 = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            rgba8.extend_from_slice(&[200, 120, 40, 255]);
        }
        crate::import::DecodedImage {
            width,
            height,
            rgba8,
        }
    }

    #[test]
    fn hiding_a_component_changes_the_composited_pixels() {
        // Defect 6's actual complaint: the Channels panel's eye toggles moved
        // a flag nothing read, so hiding the red channel did not change a
        // single pixel. It changes them here, on the composite itself, one
        // step before the upload.
        let image = swatch(16, 16);
        let mut doc = crate::doc::OpenDocument::from_import(
            crate::doc::DocumentId(1),
            crate::import::document_from_image(&image, "swatch.png", 100).unwrap(),
        );
        let whole = PixelRect::new(0, 0, 16, 16);
        let full = doc.composite(whole).unwrap();
        assert!(full[0] > 0, "the composite starts with red in it");

        let mut isolated = full.clone();
        ChannelMask {
            components: [false, true, true],
        }
        .apply(&mut isolated);
        assert_ne!(isolated, full, "hiding red changed nothing");
        for (px, was) in isolated.chunks_exact(4).zip(full.chunks_exact(4)) {
            assert_eq!(px[0], 0, "red survived");
            assert_eq!(px[1], was[1], "green was not the channel that was hidden");
            assert_eq!(px[2], was[2], "blue was not the channel that was hidden");
            assert_eq!(px[3], was[3], "alpha is not a colour component");
        }

        // ...and showing everything leaves the composite exactly as it was.
        let mut untouched = full.clone();
        ChannelMask::ALL.apply(&mut untouched);
        assert_eq!(untouched, full);
    }

    #[test]
    fn the_mask_is_what_the_channels_panel_currently_says() {
        // The panel is the authority; this is the wire between it and the
        // upload path. A click on a component eye emits the intent the chrome
        // absorbs, so absorbing it is the panel's own state.
        let mut workspace = ui::Workspace::new();
        assert_eq!(
            ChannelMask::from_channels(&workspace.channels),
            ChannelMask::ALL
        );
        workspace.absorb(&ui::Intent::SetChannelVisible {
            channel: ui::panels::channels::ChannelKind::Component(2),
            visible: false,
        });
        assert_eq!(
            ChannelMask::from_channels(&workspace.channels),
            ChannelMask {
                components: [true, true, false]
            }
        );
        assert!(!ChannelMask::from_channels(&workspace.channels).is_identity());
    }

    #[test]
    fn the_mask_covers_every_component_the_panel_lists() {
        // The mask is three bools because every colour space this build
        // supports has three components. This goes red the day one does not,
        // rather than silently ignoring the fourth row's eye.
        for mode in [
            color::ColorSpace::Srgb,
            color::ColorSpace::LinearSrgb,
            color::ColorSpace::DisplayP3,
        ] {
            assert_eq!(
                ui::panels::channels::component_names(&mode).len(),
                ChannelMask::ALL.components.len(),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_mask_that_did_not_move_asks_for_no_upload() {
        // The shell hands the mask over every frame, so "unchanged" has to be
        // free — otherwise a static document would re-upload its whole canvas
        // sixty times a second.
        let mut presenter = CanvasPresenter::new();
        assert_eq!(presenter.channel_mask(), ChannelMask::ALL);
        assert!(!presenter.set_channel_mask(ChannelMask::ALL));
        assert!(!presenter.mask_dirty);
        let hide_blue = ChannelMask {
            components: [true, true, false],
        };
        assert!(presenter.set_channel_mask(hide_blue));
        assert!(presenter.mask_dirty, "the canvas must be sent again");
        assert_eq!(presenter.channel_mask(), hide_blue);
        assert!(!presenter.set_channel_mask(hide_blue));
    }

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
