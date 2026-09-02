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

use raster::mipmap::{downsample_rgba8_2x, level_count, level_dimensions, MipError};
use raster::{PixelRect, TileCoord, TILE_SIZE};
use render::{GpuContext, GpuTexture};

use crate::doc::{DocumentError, DocumentId, OpenDocument};

/// The most texels the presenter will put on the GPU for one document.
///
/// A device's `max_texture_dimension_2d` bounds each side but not the area: on
/// a 32768-per-side adapter a square document could ask for a gigapixel
/// texture, 4 GiB of video memory, and be refused by an allocation failure
/// instead of by a check. 2^27 texels is 512 MiB of RGBA8, and it is above
/// `8192 x 8192` — the largest document that worked before this change — so
/// nothing that used to present at full resolution now presents downscaled.
pub const MAX_PRESENT_TEXELS: u64 = 1 << 27;

/// Level-0 pixels one band of an oversized composite may cover.
///
/// The downscale path composites the document in horizontal bands rather than
/// whole: a 300000 x 3333 canvas is a gigapixel, and the compositor's canvas
/// holds `[f32; 4]` per pixel, so "composite it all and then shrink it" is 16
/// GiB. Four megapixels is a 64 MiB band.
const BAND_BUDGET_PX: u64 = 4 << 20;

/// The coarsest fit at which a level-0 tile still owns texels of its own.
///
/// [`TILE_SIZE`] is 256 and tiles are aligned to it, so at level `L` a tile
/// covers exactly `256 >> L` texels per side and lands on an aligned rectangle
/// of the fitted texture — down to `L == 8`, where a whole tile is one texel.
/// At level 9 and beyond several tiles share a texel, so no tile can be
/// uploaded without pixels from tiles the dirty set did not name, and the
/// presenter recomposites the whole document instead.
pub const MAX_TILED_LEVEL: u8 = 8;

/// How the document is fitted onto the one texture the canvas samples.
///
/// A document may be bigger than the GPU will hold — `raster`'s import limits
/// allow 65535 px per side and the New Document dialog allows more still, while
/// a device offers 8192 to 32768 — so the presenter shows a *downscaled* view
/// of an oversized document rather than handing the driver a texture it cannot
/// make. `level` is the mip level the document is presented at: 0 is the
/// document's own resolution and is what every document that fits gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PresentFit {
    /// Power-of-two downscale, as a mip level. `0` means "not downscaled".
    pub level: u8,
    /// Width of the presented texture, in texels.
    pub width: u32,
    /// Height of the presented texture, in texels.
    pub height: u32,
}

impl PresentFit {
    /// The coarsest-fidelity-preserving fit for a `width` x `height` document
    /// on a device whose textures may be at most `limit` px per side.
    ///
    /// Returns the *first* (largest) mip level that fits, so a document the GPU
    /// can hold is never downscaled.
    pub fn choose(width: u32, height: u32, limit: u32) -> Self {
        let limit = limit.max(1);
        let levels = level_count(width, height);
        for level in 0..levels {
            let (w, h) = level_dimensions(width, height, level);
            if w <= limit && h <= limit && u64::from(w) * u64::from(h) <= MAX_PRESENT_TEXELS {
                return Self {
                    level,
                    width: w,
                    height: h,
                };
            }
        }
        // Unreachable while `limit >= 1`: the last level is 1x1. Total anyway,
        // because "no level fits" must not be a panic on the frame path.
        let level = levels.saturating_sub(1);
        let (width, height) = level_dimensions(width, height, level);
        Self {
            level,
            width,
            height,
        }
    }

    /// `true` when the document is shown at its own resolution.
    pub fn is_exact(&self) -> bool {
        self.level == 0
    }

    /// The linear downscale factor, `2^level`.
    pub fn scale(&self) -> u32 {
        1u32 << self.level.min(31)
    }

    /// `true` when a level-0 tile of a `width` x `height` document lands on an
    /// aligned rectangle of this texture, so one dirty tile can be uploaded on
    /// its own instead of recompositing the document.
    ///
    /// This is what keeps a brush dab cheap on an oversized document. Without
    /// it every dirty frame composites and re-uploads the whole canvas on the
    /// frame path: a 134 Mpx document takes about 5.4 s per dab that way, and
    /// even the 8256x5504 camera JPEG this whole change exists for takes about
    /// 1.8 s — a hang in place of the crash.
    ///
    /// Two things have to hold. The level must be at most [`MAX_TILED_LEVEL`],
    /// so a tile is not sharing a texel with its neighbours; and the fit must
    /// really be `2^level` document pixels per texel, which the `max(1)` clamp
    /// in [`level_dimensions`] breaks for a document narrower or shorter than
    /// `2^level`.
    pub fn supports_tiled_upload(&self, width: u32, height: u32) -> bool {
        if self.is_exact() {
            return true;
        }
        if self.level > MAX_TILED_LEVEL {
            return false;
        }
        let shift = u32::from(self.level);
        self.width == width >> shift && self.height == height >> shift
    }
}

/// The rectangle of the *fitted* texture that `coord`'s tile owns, in texels.
///
/// At level 0 this is [`tile_upload_rect`] unchanged. Downscaled, the tile's
/// document rectangle is divided by `2^level`: tiles are aligned to
/// [`TILE_SIZE`] = 256, so for `fit.level <= 8` the origin `(tx * 256, ty *
/// 256)` divides exactly and the tile owns a `256 >> level` square of texels
/// with no texel shared with any other tile.
///
/// `None` when the tile owns no whole texel — it is outside the canvas, the
/// far edge's leftover columns have no texel of their own (mip dimensions
/// floor), or [`PresentFit::supports_tiled_upload`] says this fit has no
/// per-tile mapping at all.
pub fn fitted_tile_rect(
    coord: TileCoord,
    width: u32,
    height: u32,
    fit: PresentFit,
) -> Option<PixelRect> {
    let rect = tile_upload_rect(coord, width, height)?;
    if fit.is_exact() {
        return Some(rect);
    }
    if !fit.supports_tiled_upload(width, height) {
        return None;
    }
    let shift = u32::from(fit.level);
    // `tile_upload_rect` clamps to the canvas, so both edges are in `0..=width`
    // / `0..=height` and the casts cannot lose anything.
    let x0 = (rect.x as u32) >> shift;
    let y0 = (rect.y as u32) >> shift;
    let x1 = ((rect.x as u32 + rect.width) >> shift).min(fit.width);
    let y1 = ((rect.y as u32 + rect.height) >> shift).min(fit.height);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(PixelRect::new(
        i64::from(x0),
        i64::from(y0),
        x1 - x0,
        y1 - y0,
    ))
}

/// The document pixels that `texels` of the fitted texture average.
///
/// The exact inverse of the division [`fitted_tile_rect`] does, which is what
/// makes the per-tile downscale agree with the whole-document one: both
/// average from an origin that is a multiple of `2^level`, so the pairs the
/// filter forms at every halving are the same pairs.
fn source_rect_for(texels: PixelRect, level: u8) -> PixelRect {
    let shift = u32::from(level);
    PixelRect::new(
        texels.x << shift,
        texels.y << shift,
        texels.width << shift,
        texels.height << shift,
    )
}

/// Downscale a straight-alpha RGBA8 image by `2^level`, one halving at a time.
///
/// This is [`raster::mipmap::downsample_rgba8_2x`] applied `level` times, and
/// it is deliberately not a hand-rolled block average. The buffer coming out of
/// the compositor is sRGB-encoded — the texture is `Rgba8UnormSrgb` — and
/// `raster::mipmap`'s module doc names both reasons averaging those bytes is
/// wrong:
///
/// * averaging gamma-encoded values darkens every level, because the mean of
///   the encoded values is below the encoding of the mean. A 2x2 black/white
///   checker halves to 128 that way and to 188 in linear light;
/// * averaging straight-alpha RGB pulls the colour of fully transparent texels
///   into their neighbours — the dark fringe.
///
/// `downsample_rgba8_2x` converts to linear, premultiplies, averages,
/// un-premultiplies and re-encodes, and on an odd axis it uses a 3-tap
/// polyphase kernel rather than dropping the trailing row or column, so a
/// downscaled presentation is the same image the engine's own mip chain would
/// hold rather than a shifted crop of it.
///
/// Returns the pixels and their dimensions, which are always
/// [`raster::mipmap::level_dimensions`] of the input — repeated `max(1,
/// floor(d / 2))` and `max(1, d >> level)` agree for every `d` and every
/// `level`.
pub fn downscale_levels(
    rgba8: &[u8],
    width: u32,
    height: u32,
    level: u8,
) -> Result<(Vec<u8>, u32, u32), MipError> {
    if level == 0 {
        return Ok((rgba8.to_vec(), width, height));
    }
    let (mut buf, mut w, mut h) = downsample_rgba8_2x(rgba8, width, height)?;
    for _ in 1..level {
        let (next, nw, nh) = downsample_rgba8_2x(&buf, w, h)?;
        buf = next;
        w = nw;
        h = nh;
    }
    Ok((buf, w, h))
}

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
    /// How that canvas size was fitted onto the texture. Part of the rebuild
    /// condition: a document presented downscaled on one device would be a
    /// stale, wrongly sized texture if the fit ever changed under it.
    fit: PresentFit,
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

    /// How the document is fitted onto the texture — see [`PresentFit`].
    ///
    /// [`CanvasPresenter::size`] stays the *document's* size whatever this
    /// says, because that is what the camera maps: a downscaled texture is
    /// stretched back over the document's own extent, so the image the user
    /// pans and zooms is in the same place either way, just softer.
    pub fn fit(&self) -> PresentFit {
        self.fit
    }

    /// The size of the texture currently on the GPU, which is the document's
    /// size unless it had to be downscaled to fit.
    pub fn texture_size(&self) -> (u32, u32) {
        self.texture
            .as_ref()
            .map(|t| (t.width, t.height))
            .unwrap_or((0, 0))
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
        // Never larger than the device will actually make. `create_texture`
        // returns no `Result`: an oversized request reaches the driver, comes
        // back through the uncaptured-error handler, and under this build's
        // `panic = "abort"` kills the process — on a file as ordinary as a
        // 8256x5504 camera JPEG.
        let fit = PresentFit::choose(width, height, gpu.max_texture_dimension_2d());
        let mut dirty = doc.take_dirty();
        if std::mem::take(&mut self.mask_dirty) {
            // A channel toggle changes every pixel and dirties no tile.
            dirty.mark_all();
        }

        if self.texture.is_none()
            || needs_rebuild(self.showing, doc.id(), (width, height))
            || self.fit != fit
        {
            let rgba = self.composite_fitted(doc, fit)?;
            self.texture = Some(GpuTexture::from_rgba8(
                gpu,
                fit.width,
                fit.height,
                &rgba,
                "document-composite",
            )?);
            self.showing = Some((doc.id(), (width, height)));
            self.fit = fit;
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
        // A downscaled document still gets a per-tile upload, because tiles are
        // 256 px and aligned: for `fit.level <= 8` a dirty tile is an aligned
        // `256 >> level` square of this texture and nothing else touches those
        // texels. Only a fit coarser than that — where tiles share a texel —
        // has to recomposite the whole document, and recompositing on every
        // dab is seconds of frozen UI on a document this large.
        if dirty.is_all() || !fit.supports_tiled_upload(width, height) {
            let whole = PixelRect::new(0, 0, fit.width, fit.height);
            let rgba = self.composite_fitted(doc, fit)?;
            let texture = self.texture.as_ref().expect("checked immediately above");
            write_rect(gpu, texture, whole, &rgba);
            report.full_uploads = 1;
        } else {
            for coord in dirty.tiles() {
                let Some(texels) = fitted_tile_rect(coord, width, height, fit) else {
                    continue;
                };
                let rgba = self.composite_masked(doc, source_rect_for(texels, fit.level))?;
                let (rgba, w, h) = downscale_levels(
                    &rgba,
                    texels.width << u32::from(fit.level),
                    texels.height << u32::from(fit.level),
                    fit.level,
                )?;
                debug_assert_eq!((w, h), (texels.width, texels.height));
                let texture = self.texture.as_ref().expect("checked immediately above");
                write_rect(gpu, texture, texels, &rgba);
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

    /// The whole document as the texture wants it: `fit.width` x `fit.height`
    /// RGBA8, masked, and downscaled when the document does not fit the GPU.
    ///
    /// The downscale path composites in horizontal bands. Compositing the whole
    /// canvas first would need `16 * width * height` bytes of `[f32; 4]` canvas
    /// — 16 GiB for a gigapixel document — which is the allocation that would
    /// replace one crash with another.
    fn composite_fitted(
        &self,
        doc: &mut OpenDocument,
        fit: PresentFit,
    ) -> Result<Vec<u8>, DocumentError> {
        if fit.is_exact() {
            return self.composite_masked(doc, PixelRect::new(0, 0, fit.width, fit.height));
        }
        let scale = fit.scale();
        let canvas_w = doc.document.width();
        let canvas_h = doc.document.height();
        if canvas_w == 0 || canvas_h == 0 {
            // No pixels to average. The texture still has to exist — the canvas
            // samples it every frame — so it is transparent at the fitted size.
            return Ok(vec![0u8; (fit.width as usize) * (fit.height as usize) * 4]);
        }
        // Only the part of the canvas the presented texels actually cover: mip
        // dimensions floor, so up to `scale - 1` rows and columns at the far
        // edge have no texel of their own.
        //
        // Clamped to the canvas, because a very lopsided document (1 x 300000
        // fitted onto a 2048 device is level 8, so one texel is 256 px wide on
        // an axis one pixel wide) would otherwise composite a band reaching
        // past the canvas edge and average 255 columns of transparency into
        // every texel — a document that faded away instead of crashing.
        let covered_w = (fit.width * scale).min(canvas_w);
        let rows_per_band = (BAND_BUDGET_PX / (u64::from(covered_w.max(1)) * u64::from(scale)))
            .clamp(1, u64::from(fit.height)) as u32;

        let mut out = Vec::with_capacity((fit.width as usize) * (fit.height as usize) * 4);
        let mut row = 0u32;
        while row < fit.height {
            let rows = rows_per_band.min(fit.height - row);
            let top = row * scale;
            let band = PixelRect::new(
                0,
                i64::from(top),
                covered_w,
                (rows * scale).min(canvas_h.saturating_sub(top)),
            );
            let rgba = self.composite_masked(doc, band)?;
            let (small, sw, sh) = downscale_levels(&rgba, band.width, band.height, fit.level)?;
            debug_assert_eq!((sw, sh), (fit.width, rows));
            out.extend_from_slice(&small);
            row += rows;
        }
        debug_assert_eq!(out.len(), (fit.width as usize) * (fit.height as usize) * 4);
        Ok(out)
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

// -------------------------------------------------- the selection overlay ---

/// Stroke width of the ants, in framebuffer pixels.
///
/// Two, not one: a single device pixel disappears against a busy image at any
/// scaling factor above 1, and the light run has to sit *on* the dark one.
pub const ANTS_WIDTH_PX: f32 = 2.0;

/// The unbroken run drawn under the dashes, straight-alpha **linear** RGBA.
pub const ANTS_BASE: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// The crawling dashes drawn on top of it, straight-alpha **linear** RGBA.
///
/// Two colours rather than one because a one-colour outline vanishes wherever
/// the image beneath it happens to match — see `ui::canvas::ants`, which is
/// where the geometry of both runs comes from.
pub const ANTS_DASH: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// How opaque a coverage sample has to be to count as inside the selection.
/// The same midpoint `ui::canvas::workspace` traces at.
const OUTLINE_THRESHOLD: u8 = 128;

/// The traced selection boundary, recomputed only when it actually changed.
///
/// `selection::outline` is one pass over the whole coverage mask, so tracing it
/// on every frame of a 4K document would cost more than compositing one. The
/// key is the selection *and* the canvas size, because
/// [`selection::outline_selection`] measures a mask against the canvas.
#[derive(Default)]
pub struct SelectionOutline {
    key: Option<(editor_core::Selection, u32, u32)>,
    loops: Vec<selection::Polyline>,
}

impl SelectionOutline {
    pub fn new() -> Self {
        Self::default()
    }

    /// The boundary loops of `document`'s selection, in document pixel-corner
    /// coordinates. Empty when nothing is selected.
    ///
    /// [`editor_core::Selection::None`] is deliberately *not* traced.
    /// `selection::outline_selection` answers it with the canvas outline —
    /// which is the region it selects — and drawing that would put marching
    /// ants around every document that has no selection at all.
    pub fn of(&mut self, document: &editor_core::Document) -> &[selection::Polyline] {
        let key = (
            document.selection.clone(),
            document.width(),
            document.height(),
        );
        if self.key.as_ref() != Some(&key) {
            self.loops = match &document.selection {
                editor_core::Selection::None => Vec::new(),
                sel => {
                    let canvas =
                        selection::Rect::from_xywh(0, 0, document.width(), document.height());
                    // A selection too large or too odd to trace draws no ants
                    // at all: an outline that is wrong is worse than one that
                    // is missing.
                    selection::outline_selection(sel, canvas, OUTLINE_THRESHOLD).unwrap_or_default()
                }
            };
            self.key = Some(key);
        }
        &self.loops
    }
}

/// The marching ants for `doc` at `time_secs`, in framebuffer pixels.
///
/// The camera and the viewport are the *same* ones
/// [`crate::tool_input::ToolPointer`] routes a click against, so the ants land
/// on exactly the pixels a click at that point would hit — which is what makes
/// the outline agree with the selection it is tracing rather than sitting a
/// panel's width away from it.
pub fn selection_ants(
    outline: &mut SelectionOutline,
    doc: &OpenDocument,
    time_secs: f64,
    style: &ui::canvas::AntsStyle,
) -> ui::canvas::AntsGeometry {
    let viewport = crate::tool_input::canvas_viewport(doc.camera.viewport_size);
    let camera = crate::tool_input::canvas_camera_of(&doc.camera);
    let phase = ui::canvas::ants_phase(time_secs, style);
    let loops = outline.of(&doc.document);
    if loops.is_empty() {
        return ui::canvas::AntsGeometry::default();
    }
    ui::canvas::ants::build(loops, &camera, &viewport, style, phase)
}

/// Turn ants geometry into the segments [`render::Overlay`] draws.
///
/// The whole outline first, in [`ANTS_BASE`], then the dashes on top in
/// [`ANTS_DASH`] — order matters, because the second run is what has to be
/// visible over the first.
pub fn ants_segments(geometry: &ui::canvas::AntsGeometry) -> Vec<render::Segment> {
    let mut out = Vec::new();
    for ring in &geometry.outlines {
        for pair in ring.windows(2) {
            out.push(render::Segment::new(
                pair[0],
                pair[1],
                ANTS_WIDTH_PX,
                ANTS_BASE,
            ));
        }
    }
    for [a, b] in &geometry.dashes {
        out.push(render::Segment::new(*a, *b, ANTS_WIDTH_PX, ANTS_DASH));
    }
    out
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
            color_space: color::ColorSpace::Srgb,
            icc_profile: None,
        }
    }

    /// A 64x64 document with the camera at 100%, centred — the same fixture
    /// `tool_input`'s tests route clicks against.
    fn framed(width: u32, height: u32) -> OpenDocument {
        let image = swatch(width, height);
        let mut doc = crate::doc::OpenDocument::from_import(
            crate::doc::DocumentId(1),
            crate::import::document_from_image(&image, "swatch.png", 100).unwrap(),
        );
        doc.set_viewport(glam::Vec2::new(400.0, 300.0));
        doc.camera.zoom = 1.0;
        doc.camera.center = glam::Vec2::new(width as f32 / 2.0, height as f32 / 2.0);
        doc
    }

    /// The gap this whole overlay exists for: a selection was invisible.
    /// `render::Canvas` draws one texture, that texture is the composite, and
    /// the composite has no idea a selection exists — so a marquee changed the
    /// document and not one pixel of the picture.
    #[test]
    fn a_selection_produces_an_outline_to_draw_and_no_selection_produces_none() {
        let mut doc = framed(64, 64);
        let mut outline = SelectionOutline::new();
        let style = ui::canvas::AntsStyle::default();

        // Nothing selected: nothing to draw. Deliberately *not* the canvas
        // outline, which is what `selection::outline_selection` answers
        // `Selection::None` with.
        let none = selection_ants(&mut outline, &doc, 0.0, &style);
        assert!(none.is_empty(), "an unselected document grew ants");
        assert!(ants_segments(&none).is_empty());

        doc.document.selection = editor_core::Selection::Rect {
            min: glam::IVec2::new(10, 12),
            max: glam::IVec2::new(40, 44),
        };
        let some = selection_ants(&mut outline, &doc, 0.0, &style);
        assert!(!some.is_empty(), "a marquee produced no outline");
        assert_eq!(some.outlines.len(), 1, "one rectangle is one loop");
        assert!(!some.dashes.is_empty(), "the outline has no dashes on it");

        let segments = ants_segments(&some);
        assert!(segments.len() >= some.dashes.len() + 4);
        // Two colours, base and dash, or the ants vanish over an image that
        // happens to match one of them.
        assert!(segments.iter().any(|s| s.color == ANTS_BASE));
        assert!(segments.iter().any(|s| s.color == ANTS_DASH));
        assert!(segments.iter().all(|s| s.a.is_finite() && s.b.is_finite()));

        // The loop is where the camera puts it: document (10, 12) is screen
        // (200 - 32 + 10, 150 - 32 + 12) at this fixture's camera.
        let corner = glam::Vec2::new(200.0 - 32.0 + 10.0, 150.0 - 32.0 + 12.0);
        assert!(
            some.outlines[0]
                .iter()
                .any(|p| (*p - corner).length() < 0.5),
            "the outline is not where the camera puts the selection: {:?}",
            some.outlines[0]
        );
    }

    /// The ants march: the same selection at a later moment puts the dashes
    /// somewhere else, while the outline under them does not move.
    #[test]
    fn the_ants_move_with_the_clock() {
        let mut doc = framed(64, 64);
        doc.document.selection = editor_core::Selection::Rect {
            min: glam::IVec2::new(4, 4),
            max: glam::IVec2::new(60, 60),
        };
        let mut outline = SelectionOutline::new();
        let style = ui::canvas::AntsStyle::default();
        let a = selection_ants(&mut outline, &doc, 0.0, &style);
        let b = selection_ants(
            &mut outline,
            &doc,
            f64::from(style.dash() / style.speed_pt_per_sec),
            &style,
        );
        assert_ne!(a.dashes, b.dashes, "the ants stood still");
        assert_eq!(a.outlines, b.outlines, "the outline itself moved");
        assert_ne!(ants_segments(&a), ants_segments(&b));
    }

    /// The trace is cached: it is one pass over the whole coverage mask, and it
    /// runs on every frame the selection is visible.
    #[test]
    fn the_outline_is_retraced_only_when_the_selection_changes() {
        let mut doc = framed(32, 32);
        let mut outline = SelectionOutline::new();
        assert!(outline.of(&doc.document).is_empty());

        doc.document.selection = editor_core::Selection::Rect {
            min: glam::IVec2::new(2, 2),
            max: glam::IVec2::new(20, 20),
        };
        let first = outline.of(&doc.document).to_vec();
        assert_eq!(first.len(), 1);
        assert_eq!(outline.of(&doc.document), first.as_slice());

        // A different selection retraces...
        doc.document.selection = editor_core::Selection::Rect {
            min: glam::IVec2::new(2, 2),
            max: glam::IVec2::new(10, 10),
        };
        assert_ne!(outline.of(&doc.document), first.as_slice());
        // ...and so does the same selection on a canvas of another size, which
        // is what the mask is measured against.
        let traced = outline.of(&doc.document).to_vec();
        doc.document.meta.size = glam::UVec2::new(64, 64);
        let _ = outline.of(&doc.document);
        assert_eq!(
            outline.of(&doc.document),
            traced.as_slice(),
            "a rectangle's outline should not depend on the canvas size"
        );
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
    fn a_document_the_gpu_can_hold_is_never_downscaled() {
        // The fit must be inert for every document that already worked. The
        // largest of those is 8192 square — the WebGPU baseline that
        // `wgpu::Limits::default` used to pin every device to — so
        // `MAX_PRESENT_TEXELS` sits above 8192*8192 on purpose and no document
        // that presented at full resolution before now presents softer.
        for limit in [8192u32, 16384, 32768] {
            for (w, h) in [(1, 1), (1920, 1080), (4096, 4096), (8192, 8192)] {
                let fit = PresentFit::choose(w, h, limit);
                assert!(fit.is_exact(), "{w}x{h} on a {limit} device was downscaled");
                assert_eq!((fit.width, fit.height), (w, h));
                assert_eq!(fit.scale(), 1);
            }
        }
    }

    #[test]
    fn a_document_past_the_device_limit_is_fitted_instead_of_refused() {
        // The one-click crash: a Nikon Z8 JPEG is 8256x5504 and the WebGPU
        // baseline is 8192 per side. On a device that really does stop at 8192
        // this must present — downscaled — rather than reaching `create_texture`
        // with a size it will not make.
        let fit = PresentFit::choose(8256, 5504, 8192);
        assert_eq!(fit.level, 1);
        assert_eq!((fit.width, fit.height), (4128, 2752));

        // Nothing may ever come back from `choose` that the device would refuse.
        for limit in [1u32, 2048, 8192, 16384, 32768] {
            for (w, h) in [
                (8256, 5504),
                (9504, 6336),
                (65_535, 65_535),
                (300_000, 3_333),
                (1, 300_000),
                (31_622, 31_622),
            ] {
                let fit = PresentFit::choose(w, h, limit);
                assert!(
                    fit.width <= limit && fit.height <= limit,
                    "{w}x{h} on a {limit} device fitted to {}x{}",
                    fit.width,
                    fit.height
                );
                assert!(
                    u64::from(fit.width) * u64::from(fit.height) <= MAX_PRESENT_TEXELS,
                    "{w}x{h} fitted to {}x{}, past the texel budget",
                    fit.width,
                    fit.height
                );
                assert!(fit.width >= 1 && fit.height >= 1);
                assert_eq!(
                    (fit.width, fit.height),
                    raster::mipmap::level_dimensions(w, h, fit.level),
                    "the fit must be a real mip level of the document"
                );
            }
        }
    }

    #[test]
    fn the_texel_budget_bounds_a_document_whose_sides_both_fit() {
        // A square gigapixel document is under a 32768-per-side limit on both
        // axes and is still 4 GiB of texture. The side limit alone would let it
        // through to an allocation failure.
        let fit = PresentFit::choose(31_622, 31_622, 32_768);
        assert!(!fit.is_exact(), "a gigapixel texture was requested whole");
        assert!(u64::from(fit.width) * u64::from(fit.height) <= MAX_PRESENT_TEXELS);
    }

    #[test]
    fn a_lopsided_document_does_not_average_the_void_past_its_own_edge() {
        // A document far taller than it is wide fits by a level chosen for the
        // long axis, so one texel can be wider than the whole canvas. The band
        // must stop at the canvas edge: averaging the transparency beyond it
        // would present a document that faded out instead of one that crashed.
        // No GPU needed — this is the CPU half of the present path.
        let image = swatch(4, 64);
        let mut doc = crate::doc::OpenDocument::from_import(
            crate::doc::DocumentId(1),
            crate::import::document_from_image(&image, "swatch.png", 100).unwrap(),
        );
        let presenter = CanvasPresenter::new();
        // Level 3: one texel is 8 document pixels wide, and the document is 4.
        let fit = PresentFit {
            level: 3,
            width: 1,
            height: 8,
        };
        let rgba = presenter.composite_fitted(&mut doc, fit).unwrap();
        assert_eq!(rgba.len(), 8 * 4, "one column of eight texels");
        for (i, px) in rgba.chunks_exact(4).enumerate() {
            assert_eq!(px[3], 255, "texel {i} lost coverage the document has");
            assert!(
                px[0].abs_diff(200) <= 1 && px[1].abs_diff(120) <= 1 && px[2].abs_diff(40) <= 1,
                "texel {i} is {px:?}, not the document's colour"
            );
        }
    }

    #[test]
    fn downscaling_averages_the_block_it_covers() {
        // 4x2 of two flat colours, halved: each output texel is the average of
        // its own 2x2 block and nothing else, so a downscale that read the
        // wrong block or the wrong stride cannot pass. Flat blocks round-trip
        // exactly through linear light, so these are equalities.
        let mut src = Vec::new();
        for _ in 0..2 {
            src.extend_from_slice(&[200, 0, 0, 255]);
            src.extend_from_slice(&[200, 0, 0, 255]);
            src.extend_from_slice(&[0, 0, 100, 255]);
            src.extend_from_slice(&[0, 0, 100, 255]);
        }
        let (out, w, h) = downscale_levels(&src, 4, 2, 1).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(out.len(), 8, "2x1 texels of RGBA8");
        assert_eq!(&out[0..4], &[200, 0, 0, 255]);
        assert_eq!(&out[4..8], &[0, 0, 100, 255]);
    }

    #[test]
    fn downscaling_a_checker_does_not_darken_it() {
        // The defect this test exists for: the presenter used to average the
        // *stored* bytes, which are sRGB-encoded because the texture is
        // `Rgba8UnormSrgb`. Half black and half white is 50% light, which
        // encodes to 188 — not to 128, the mean of the encodings. Averaging
        // the bytes made every downscaled presentation darker and
        // contrast-crushed, and no flat-colour test could see it.
        let white = [255u8, 255, 255, 255];
        let black = [0u8, 0, 0, 255];
        let mut src = Vec::new();
        for px in [white, black, black, white] {
            src.extend_from_slice(&px);
        }
        let (out, w, h) = downscale_levels(&src, 2, 2, 1).unwrap();
        assert_eq!((w, h), (1, 1));
        for (c, name) in out[..3].iter().zip(["r", "g", "b"]) {
            assert!(
                c.abs_diff(188) <= 1,
                "{name} halved to {c}; 128 is the gamma-encoded average, 188 is \
                 the light the checker actually emits"
            );
        }
        assert_eq!(out[3], 255, "an opaque checker stayed opaque");
    }

    #[test]
    fn a_mid_grey_ramp_survives_a_four_times_downscale_without_darkening() {
        // Two halvings of a 4-step ramp down to one texel. In linear light the
        // answer is 150; averaging the encoded bytes gives their arithmetic
        // mean, 128, and a chain of such averages is what makes a zoomed-out
        // document look muddy.
        let mut src = Vec::new();
        for _ in 0..4 {
            for v in [32u8, 96, 160, 224] {
                src.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let (out, w, h) = downscale_levels(&src, 4, 4, 2).unwrap();
        assert_eq!((w, h), (1, 1));
        assert!(
            out[0].abs_diff(150) <= 2,
            "the ramp collapsed to {}, not the 150 its light averages to",
            out[0]
        );
        assert!(out[0] > 140, "a 4x downscale darkened a mid-grey ramp");

        // A flat mid-grey must not drift at all, however many levels it goes
        // through: rounding bias would show up here first.
        let flat = vec![119u8; 16 * 16 * 4];
        let mut flat: Vec<u8> = flat;
        for px in flat.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let (out, w, h) = downscale_levels(&flat, 16, 16, 4).unwrap();
        assert_eq!((w, h), (1, 1));
        assert_eq!(&out[..], &[119, 119, 119, 255]);
    }

    #[test]
    fn downscaling_does_not_pull_transparent_black_into_its_neighbours() {
        // One opaque red texel among three fully transparent ones. Averaging
        // straight alpha gives a quarter-strength red — the dark fringe. The
        // colour is unchanged; only the coverage falls.
        let mut src = vec![0u8; 4 * 4];
        src[0..4].copy_from_slice(&[255, 0, 0, 255]);
        let (out, _, _) = downscale_levels(&src, 2, 2, 1).unwrap();
        assert_eq!(out[0], 255, "the red was diluted by transparent texels");
        assert_eq!(out[3], 64, "coverage is a quarter");

        // A block with no coverage at all has no colour to recover.
        let (out, _, _) = downscale_levels(&[0u8; 16], 2, 2, 1).unwrap();
        assert_eq!(out, vec![0, 0, 0, 0]);
    }

    #[test]
    fn downscaling_by_zero_levels_is_the_identity() {
        let src: Vec<u8> = (0..16).collect();
        assert_eq!(downscale_levels(&src, 2, 2, 0).unwrap(), (src, 2, 2));
    }

    #[test]
    fn an_odd_axis_keeps_the_pixel_a_crop_would_drop() {
        // 3x1 halved. The old hand-rolled filter averaged the first two texels
        // and discarded the third — a shifted crop, and an error that
        // accumulates at every odd level of a chain. `raster::mipmap`'s 3-tap
        // polyphase kernel gives all three pixels weight, in linear light.
        let src = vec![
            10, 10, 10, 255, //
            30, 30, 30, 255, //
            99, 99, 99, 255,
        ];
        let (out, w, h) = downscale_levels(&src, 3, 1, 1).unwrap();
        assert_eq!((w, h), (1, 1));
        assert_ne!(out[0], 20, "the third pixel was cropped away");
        assert!(
            out[0].abs_diff(61) <= 2,
            "3 greys averaged in linear light are 61, got {}",
            out[0]
        );
    }

    #[test]
    fn a_downscale_lands_on_the_mip_level_it_names() {
        // The band loop and the per-tile loop both size their writes from
        // `level_dimensions`, so repeated halving has to agree with it for
        // every extent — including the ones where `max(1)` clamps.
        for (w, h) in [(1u32, 1u32), (3, 7), (256, 64), (1000, 1000), (65_535, 9)] {
            for level in 0..=10u8 {
                let src = vec![128u8; (w as usize) * (h as usize) * 4];
                let (out, ow, oh) = downscale_levels(&src, w, h, level).unwrap();
                assert_eq!(
                    (ow, oh),
                    level_dimensions(w, h, level),
                    "{w}x{h} at level {level}"
                );
                assert_eq!(out.len(), (ow as usize) * (oh as usize) * 4);
            }
        }
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
    fn a_downscaled_tile_is_still_an_aligned_rectangle_of_the_texture() {
        // The whole point of the per-tile downscaled path: a 256 px tile is a
        // `256 >> level` square of the fitted texture at an origin that divides
        // exactly, so one dab uploads one small rectangle instead of the
        // document.
        let (w, h) = (32_769u32, 4_096u32);
        let fit = PresentFit::choose(w, h, 32_768);
        assert_eq!(fit.level, 1);
        assert!(fit.supports_tiled_upload(w, h));
        assert_eq!(
            fitted_tile_rect(TileCoord::new(0, 0, 0), w, h, fit),
            Some(PixelRect::new(0, 0, 128, 128))
        );
        assert_eq!(
            fitted_tile_rect(TileCoord::new(1, 2, 0), w, h, fit),
            Some(PixelRect::new(128, 256, 128, 128))
        );
        // Level 3: one texel is 8 document pixels, so a tile is 32 texels.
        let fit = PresentFit {
            level: 3,
            width: w >> 3,
            height: h >> 3,
        };
        assert!(fit.supports_tiled_upload(w, h));
        assert_eq!(
            fitted_tile_rect(TileCoord::new(3, 1, 0), w, h, fit),
            Some(PixelRect::new(96, 32, 32, 32))
        );
        // ...and the document pixels that rectangle averages are exactly the
        // ones the tile owns.
        let src = source_rect_for(PixelRect::new(96, 32, 32, 32), 3);
        assert_eq!(src, PixelRect::new(768, 256, 256, 256));
        assert_eq!(
            src,
            tile_upload_rect(TileCoord::new(3, 1, 0), w, h).unwrap(),
            "the source rect must be the tile itself"
        );
    }

    #[test]
    fn a_tile_that_shares_texels_with_its_neighbours_has_no_rectangle_of_its_own() {
        // At level 9 a texel spans two tiles, so a per-tile upload would have
        // to average pixels the dirty set never named. That fit falls back to
        // the whole-document path rather than presenting a half-averaged texel.
        let (w, h) = (300_000u32, 300_000u32);
        let fit = PresentFit {
            level: 9,
            width: w >> 9,
            height: h >> 9,
        };
        assert!(!fit.supports_tiled_upload(w, h));
        assert_eq!(fitted_tile_rect(TileCoord::new(0, 0, 0), w, h, fit), None);

        // MAX_TILED_LEVEL itself is exactly "one tile, one texel".
        let fit = PresentFit {
            level: MAX_TILED_LEVEL,
            width: w >> MAX_TILED_LEVEL,
            height: h >> MAX_TILED_LEVEL,
        };
        assert!(fit.supports_tiled_upload(w, h));
        assert_eq!(
            fitted_tile_rect(TileCoord::new(5, 7, 0), w, h, fit),
            Some(PixelRect::new(5, 7, 1, 1))
        );
    }

    #[test]
    fn a_fit_the_max_one_clamp_moved_has_no_per_tile_mapping() {
        // A 1 x 300000 document on a small device fits at a level where one
        // texel would be 256 px on an axis one pixel wide. `level_dimensions`
        // clamps that to 1, so the texture is no longer `2^level` pixels per
        // texel and the tile arithmetic would write the wrong texels.
        let (w, h) = (1u32, 300_000u32);
        let fit = PresentFit::choose(w, h, 2048);
        assert!(!fit.is_exact());
        assert_eq!(fit.width, 1);
        assert!(
            !fit.supports_tiled_upload(w, h),
            "a clamped axis is not a power-of-two division"
        );
        assert_eq!(fitted_tile_rect(TileCoord::new(0, 0, 0), w, h, fit), None);
    }

    #[test]
    fn the_leftover_columns_past_the_last_texel_upload_nothing() {
        // Mip dimensions floor, so up to `2^level - 1` columns at the far edge
        // have no texel. A tile made only of those must not write past the
        // texture — or, worse, into the last real column.
        let (w, h) = (513u32, 512u32);
        let fit = PresentFit {
            level: 1,
            width: 256,
            height: 256,
        };
        assert!(fit.supports_tiled_upload(w, h));
        // The tile at x=512 holds one document column, which is half a texel.
        let rect = tile_upload_rect(TileCoord::new(2, 0, 0), w, h).unwrap();
        assert_eq!(rect.width, 1);
        assert_eq!(fitted_tile_rect(TileCoord::new(2, 0, 0), w, h, fit), None);
        // Its neighbour still owns its own 128 texels and nothing beyond them.
        let texels = fitted_tile_rect(TileCoord::new(1, 0, 0), w, h, fit).unwrap();
        assert_eq!(texels, PixelRect::new(128, 0, 128, 128));
        assert!(texels.x + i64::from(texels.width) <= i64::from(fit.width));
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
