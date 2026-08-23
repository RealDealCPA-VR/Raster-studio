//! Rasterisation.
//!
//! Shaped glyphs are scaled and filled into an 8-bit **coverage** mask
//! positioned in layer space. Coverage is the fraction of the pixel the glyph
//! outline covers — an area, so it is already a *linear* quantity, and
//! [`fill_linear`] uses it directly as alpha in linear premultiplied space.
//! Nothing here ever encodes to sRGB, which is exactly what stops text edges
//! from going muddy the way a gamma-space blend does.
//!
//! Rasterised glyphs are cached by (face, glyph, size, weight, horizontal
//! subpixel bin, synthesis flags) in [`GlyphRasterCache`]. That bin is part of
//! the key, so a glyph landing at x=10.0 and the same glyph at x=10.5 are two
//! distinct, separately cached images rather than one image snapped to the
//! pixel grid.
//!
//! **Subpixel positioning is horizontal only.** Baselines are snapped to whole
//! pixels — [`GlyphRasterCache::glyph_image`] floors the pen's y before binning
//! it — which is the usual vertical hinting: every glyph on a line shares one
//! baseline, so snapping keeps their horizontal stems on the same pixel rows
//! instead of blurring each one differently. The cost is that moving a layer by
//! a fraction of a pixel vertically does not move its text by that fraction.

use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::{CacheKey, CacheKeyFlags, SwashCache, SwashContent, SwashImage, Weight};

use crate::font::{FontId, FontLibrary};
use crate::layout::{Rect, ShapedGlyph, ShapedText};

/// Faux-bold smear width as a fraction of the em size.
const SYNTHETIC_BOLD_FACTOR: f32 = 0.03;

/// Largest pen coordinate handed to the scaler, in layer pixels.
///
/// The scaler splits a pen position into an `i32` pixel and a subpixel bin,
/// and its rounding step adds one to that pixel — which overflows for a
/// position anywhere near `i32::MAX`. Ten million pixels is far outside any
/// canvas, so a position beyond it is clamped rather than allowed to panic.
const MAX_PEN_PX: f32 = 1.0e7;

/// Clamp a pen coordinate into the range the scaler's integer maths can hold;
/// a non-finite position becomes zero.
fn clamp_pen(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(-MAX_PEN_PX, MAX_PEN_PX)
    }
}

/// An 8-bit coverage mask positioned in layer space.
///
/// `origin_x`/`origin_y` are the layer-space integer coordinates of pixel
/// `(0, 0)`; the mask covers `origin_x .. origin_x + width` horizontally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageMask {
    /// Layer-space x of column 0.
    pub origin_x: i32,
    /// Layer-space y of row 0.
    pub origin_y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Row-major coverage, `width * height` bytes.
    pub data: Vec<u8>,
}

impl CoverageMask {
    /// An all-zero mask of the given geometry.
    #[must_use]
    pub fn new(origin_x: i32, origin_y: i32, width: u32, height: u32) -> Self {
        Self {
            origin_x,
            origin_y,
            width,
            height,
            data: vec![0; (width as usize) * (height as usize)],
        }
    }

    /// A mask with no pixels at all.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            origin_x: 0,
            origin_y: 0,
            width: 0,
            height: 0,
            data: Vec::new(),
        }
    }

    /// Whether the mask has no pixels.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Coverage at a layer-space pixel; `0` outside the mask.
    #[must_use]
    pub fn coverage(&self, x: i32, y: i32) -> u8 {
        let (Some(col), Some(row)) = (
            usize::try_from(x - self.origin_x).ok(),
            usize::try_from(y - self.origin_y).ok(),
        ) else {
            return 0;
        };
        if col >= self.width as usize || row >= self.height as usize {
            return 0;
        }
        self.data[row * self.width as usize + col]
    }

    /// The mask's own rectangle in layer space.
    #[must_use]
    pub fn rect(&self) -> Rect {
        Rect {
            x: self.origin_x as f32,
            y: self.origin_y as f32,
            width: self.width as f32,
            height: self.height as f32,
        }
    }

    /// Tight rectangle around the non-zero coverage, or `None` if blank.
    #[must_use]
    pub fn ink_bounds(&self) -> Option<Rect> {
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        let mut any = false;
        for row in 0..self.height {
            for col in 0..self.width {
                if self.data[(row * self.width + col) as usize] != 0 {
                    any = true;
                    min_x = min_x.min(col);
                    min_y = min_y.min(row);
                    max_x = max_x.max(col);
                    max_y = max_y.max(row);
                }
            }
        }
        any.then(|| Rect {
            x: (self.origin_x + min_x as i32) as f32,
            y: (self.origin_y + min_y as i32) as f32,
            width: (max_x - min_x + 1) as f32,
            height: (max_y - min_y + 1) as f32,
        })
    }

    /// Sum of every coverage byte — a cheap "how much ink is here" measure.
    #[must_use]
    pub fn total_coverage(&self) -> u64 {
        self.data.iter().map(|&v| u64::from(v)).sum()
    }

    /// Union-composite one sample: `c' = c + v(1 - c)`, in coverage units.
    fn blend(&mut self, x: i32, y: i32, value: u8) {
        if value == 0 {
            return;
        }
        let (Ok(col), Ok(row)) = (
            usize::try_from(x - self.origin_x),
            usize::try_from(y - self.origin_y),
        ) else {
            return;
        };
        if col >= self.width as usize || row >= self.height as usize {
            return;
        }
        let slot = &mut self.data[row * self.width as usize + col];
        let existing = u32::from(*slot);
        let added = u32::from(value) * (255 - existing) / 255;
        *slot = (existing + added).min(255) as u8;
    }
}

/// A rasterised glyph, positioned relative to its pen origin the way the
/// scaler reports it: `left` is the offset to the bitmap's left edge, `top`
/// the distance *up* from the baseline to its top edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlyphImage {
    /// X offset from the pen origin to the bitmap's left column.
    pub left: i32,
    /// Height of the bitmap's top edge above the pen origin.
    pub top: i32,
    /// Bitmap width.
    pub width: u32,
    /// Bitmap height.
    pub height: u32,
    /// Row-major coverage.
    pub data: Vec<u8>,
}

/// Cache key for one rasterised glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    /// Face the glyph belongs to.
    pub font: FontId,
    /// Glyph index.
    pub glyph_id: u16,
    /// Bit pattern of the em size, so the key stays hashable.
    pub size_bits: u32,
    /// Requested weight (drives the `wght` axis of variable faces).
    pub weight: u16,
    /// Quarter-pixel bin of the fractional x position.
    ///
    /// There is deliberately no y counterpart: baselines snap to whole pixels
    /// (see the module documentation), so the vertical bin would be zero for
    /// every glyph the engine ever rasterises and would only advertise
    /// positioning the crate does not do.
    pub x_bin: u8,
    /// Emboldened copy.
    pub synthetic_bold: bool,
    /// Skewed copy.
    pub synthetic_italic: bool,
}

/// Cache of rasterised glyphs.
#[derive(Debug)]
pub struct GlyphRasterCache {
    swash: SwashCache,
    images: HashMap<GlyphKey, Option<Arc<GlyphImage>>>,
    hits: u64,
    misses: u64,
}

impl GlyphRasterCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            swash: SwashCache::new(),
            images: HashMap::new(),
            hits: 0,
            misses: 0,
        }
    }

    /// Number of distinct glyph images held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.images.len()
    }

    /// Whether the cache holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// Lookups served from the cache.
    #[must_use]
    pub const fn hits(&self) -> u64 {
        self.hits
    }

    /// Lookups that had to rasterise.
    #[must_use]
    pub const fn misses(&self) -> u64 {
        self.misses
    }

    /// Drop every cached image.
    pub fn clear(&mut self) {
        self.images.clear();
        self.swash.image_cache.clear();
        self.swash.outline_command_cache.clear();
        self.hits = 0;
        self.misses = 0;
    }

    /// Rasterise (or fetch) one glyph, returning the image and the integer
    /// pen position it should be blitted at.
    pub fn glyph_image(
        &mut self,
        library: &mut FontLibrary,
        glyph: &ShapedGlyph,
    ) -> (Option<Arc<GlyphImage>>, i32, i32) {
        let flags = if glyph.synthetic_italic {
            CacheKeyFlags::FAKE_ITALIC
        } else {
            CacheKeyFlags::empty()
        };
        let (cache_key, pen_x, pen_y) = CacheKey::new(
            glyph.font.0,
            glyph.glyph_id,
            glyph.size_px,
            // `CacheKey::new` splits x into a pixel and a quarter-pixel bin.
            // The y is pre-floored instead, which is what snaps the baseline
            // to the pixel grid: the bin it produces is then always zero, and
            // `pen_y` is the floor. `floor`, never `trunc` — truncation rounds
            // towards zero and would put a baseline above y = 0 one pixel
            // lower than the same baseline below it.
            (clamp_pen(glyph.draw_x), clamp_pen(glyph.draw_y.floor())),
            Weight(glyph.weight.0),
            flags,
        );
        debug_assert_eq!(
            cache_key.y_bin,
            cosmic_text::SubpixelBin::Zero,
            "the pre-floored baseline must leave no vertical subpixel bin — \
             GlyphKey has no y component precisely because of that"
        );
        let key = GlyphKey {
            font: glyph.font,
            glyph_id: glyph.glyph_id,
            size_bits: glyph.size_px.to_bits(),
            weight: glyph.weight.0,
            x_bin: bin_index(cache_key.x_bin),
            synthetic_bold: glyph.synthetic_bold,
            synthetic_italic: glyph.synthetic_italic,
        };
        if let Some(cached) = self.images.get(&key) {
            self.hits += 1;
            return (cached.clone(), pen_x, pen_y);
        }
        self.misses += 1;
        let image = self
            .swash
            .get_image_uncached(library.system_mut(), cache_key)
            .and_then(|image| convert(&image))
            .map(|image| {
                if glyph.synthetic_bold {
                    embolden(&image, synthetic_bold_radius(glyph.size_px))
                } else {
                    image
                }
            })
            .map(Arc::new);
        self.images.insert(key, image.clone());
        (image, pen_x, pen_y)
    }
}

impl Default for GlyphRasterCache {
    fn default() -> Self {
        Self::new()
    }
}

/// A glyph image together with where it lands in layer space.
#[derive(Debug, Clone)]
struct Placement {
    image: Arc<GlyphImage>,
    left: i32,
    top: i32,
    color: [f32; 4],
}

fn placements(
    library: &mut FontLibrary,
    cache: &mut GlyphRasterCache,
    text: &ShapedText,
) -> Vec<Placement> {
    let mut out = Vec::with_capacity(text.glyphs.len());
    for glyph in &text.glyphs {
        let color = text.style_of(glyph).color;
        let (image, pen_x, pen_y) = cache.glyph_image(library, glyph);
        if let Some(image) = image {
            if image.width == 0 || image.height == 0 {
                continue;
            }
            out.push(Placement {
                left: pen_x + image.left,
                top: pen_y - image.top,
                image,
                color,
            });
        }
    }
    out
}

/// A decoration rectangle reduced to something a mask can actually hold.
///
/// Empty and non-finite rules are dropped — a NaN layer origin produces NaN
/// rules, and casting those to `i32` saturates to zero, which would stretch the
/// mask from zero out to wherever the glyphs landed. Finite but absurd rules
/// are clamped to the same range as a pen position, so a rule out at 10^12 px
/// cannot ask for a gigapixel mask while the glyphs sit at the clamped pen.
fn drawable_rect(rect: &Rect) -> Option<Rect> {
    if !(rect.x.is_finite()
        && rect.y.is_finite()
        && rect.width.is_finite()
        && rect.height.is_finite())
        || rect.width <= 0.0
        || rect.height <= 0.0
    {
        return None;
    }
    let x0 = rect.x.clamp(-MAX_PEN_PX, MAX_PEN_PX);
    let y0 = rect.y.clamp(-MAX_PEN_PX, MAX_PEN_PX);
    let x1 = rect.right().clamp(-MAX_PEN_PX, MAX_PEN_PX);
    let y1 = rect.bottom().clamp(-MAX_PEN_PX, MAX_PEN_PX);
    (x1 > x0 && y1 > y0).then(|| Rect::from_corners(x0, y0, x1, y1))
}

fn bounds(text: &ShapedText, placed: &[Placement]) -> Option<(i32, i32, i32, i32)> {
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    let mut any = false;
    for placement in placed {
        any = true;
        min_x = min_x.min(placement.left);
        min_y = min_y.min(placement.top);
        max_x = max_x.max(placement.left + placement.image.width as i32);
        max_y = max_y.max(placement.top + placement.image.height as i32);
    }
    for decoration in &text.decorations {
        let Some(rect) = drawable_rect(&decoration.rect) else {
            continue;
        };
        any = true;
        min_x = min_x.min(rect.x.floor() as i32);
        min_y = min_y.min(rect.y.floor() as i32);
        max_x = max_x.max(rect.right().ceil() as i32);
        max_y = max_y.max(rect.bottom().ceil() as i32);
    }
    any.then_some((min_x, min_y, max_x, max_y))
}

/// Rasterise every glyph and decoration of a shaped text into one coverage
/// mask, sized exactly to the ink it contains.
///
/// The result is empty (zero-sized) for text with nothing to draw, including
/// the empty string and whitespace-only strings.
pub fn rasterize(
    library: &mut FontLibrary,
    cache: &mut GlyphRasterCache,
    text: &ShapedText,
) -> CoverageMask {
    let placed = placements(library, cache, text);
    let Some((min_x, min_y, max_x, max_y)) = bounds(text, &placed) else {
        return CoverageMask::empty();
    };
    let mut mask = CoverageMask::new(min_x, min_y, (max_x - min_x) as u32, (max_y - min_y) as u32);
    for placement in &placed {
        blit(&mut mask, placement);
    }
    for decoration in &text.decorations {
        fill_rect(&mut mask, &decoration.rect);
    }
    mask
}

fn blit(mask: &mut CoverageMask, placement: &Placement) {
    let image = &placement.image;
    for row in 0..image.height {
        for col in 0..image.width {
            let value = image.data[(row * image.width + col) as usize];
            mask.blend(
                placement.left + col as i32,
                placement.top + row as i32,
                value,
            );
        }
    }
}

fn fill_rect(mask: &mut CoverageMask, rect: &Rect) {
    let Some(rect) = drawable_rect(rect) else {
        return;
    };
    let x0 = rect.x;
    let x1 = rect.right();
    let y0 = rect.y;
    let y1 = rect.bottom();
    for py in (y0.floor() as i32)..(y1.ceil() as i32) {
        let row_cover = (y1.min(py as f32 + 1.0) - y0.max(py as f32)).clamp(0.0, 1.0);
        if row_cover <= 0.0 {
            continue;
        }
        for px in (x0.floor() as i32)..(x1.ceil() as i32) {
            let col_cover = (x1.min(px as f32 + 1.0) - x0.max(px as f32)).clamp(0.0, 1.0);
            let value = (row_cover * col_cover * 255.0).round().clamp(0.0, 255.0) as u8;
            mask.blend(px, py, value);
        }
    }
}

/// A linear, **premultiplied** RGBA image, ready for the compositor.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearImage {
    /// Layer-space x of column 0.
    pub origin_x: i32,
    /// Layer-space y of row 0.
    pub origin_y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Row-major RGBA, four floats per pixel, premultiplied by alpha.
    pub data: Vec<f32>,
}

impl LinearImage {
    /// A fully transparent image of the given geometry.
    #[must_use]
    pub fn new(origin_x: i32, origin_y: i32, width: u32, height: u32) -> Self {
        Self {
            origin_x,
            origin_y,
            width,
            height,
            data: vec![0.0; (width as usize) * (height as usize) * 4],
        }
    }

    /// Whether the image has no pixels.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// The premultiplied linear RGBA at a layer-space pixel.
    #[must_use]
    pub fn pixel(&self, x: i32, y: i32) -> [f32; 4] {
        let (Ok(col), Ok(row)) = (
            usize::try_from(x - self.origin_x),
            usize::try_from(y - self.origin_y),
        ) else {
            return [0.0; 4];
        };
        if col >= self.width as usize || row >= self.height as usize {
            return [0.0; 4];
        }
        let base = (row * self.width as usize + col) * 4;
        [
            self.data[base],
            self.data[base + 1],
            self.data[base + 2],
            self.data[base + 3],
        ]
    }

    /// Source-over composite of `src` on top of `self`, in premultiplied
    /// linear space. Geometry must match.
    pub fn composite_over(&mut self, src: &Self) {
        if src.origin_x != self.origin_x
            || src.origin_y != self.origin_y
            || src.width != self.width
            || src.height != self.height
        {
            return;
        }
        for (dst, src) in self.data.chunks_exact_mut(4).zip(src.data.chunks_exact(4)) {
            let inv = 1.0 - src[3];
            for channel in 0..4 {
                dst[channel] = src[channel] + dst[channel] * inv;
            }
        }
    }
}

/// Fill a coverage mask with one linear straight RGBA colour, producing a
/// linear premultiplied image.
///
/// Coverage is area, hence linear: it multiplies alpha directly, with no
/// gamma decode/encode round trip anywhere.
#[must_use]
pub fn fill_linear(mask: &CoverageMask, color: [f32; 4]) -> LinearImage {
    let mut out = LinearImage::new(mask.origin_x, mask.origin_y, mask.width, mask.height);
    for (pixel, &value) in out.data.chunks_exact_mut(4).zip(mask.data.iter()) {
        let alpha = f32::from(value) / 255.0 * color[3];
        pixel[0] = color[0] * alpha;
        pixel[1] = color[1] * alpha;
        pixel[2] = color[2] * alpha;
        pixel[3] = alpha;
    }
    out
}

/// Rasterise and fill shaped text, honouring each style run's own colour.
///
/// Glyphs sharing a colour are accumulated into one coverage mask before being
/// filled, so overlapping glyphs of the same colour do not double-darken; the
/// colour groups are then composited over one another in first-use order.
pub fn render_linear(
    library: &mut FontLibrary,
    cache: &mut GlyphRasterCache,
    text: &ShapedText,
) -> LinearImage {
    let placed = placements(library, cache, text);
    let Some((min_x, min_y, max_x, max_y)) = bounds(text, &placed) else {
        return LinearImage::new(0, 0, 0, 0);
    };
    let width = (max_x - min_x) as u32;
    let height = (max_y - min_y) as u32;

    let mut order: Vec<[u32; 4]> = Vec::new();
    let mut groups: HashMap<[u32; 4], CoverageMask> = HashMap::new();
    let group_for = |color: [f32; 4],
                     order: &mut Vec<[u32; 4]>,
                     groups: &mut HashMap<[u32; 4], CoverageMask>|
     -> [u32; 4] {
        let key = color_key(color);
        groups.entry(key).or_insert_with(|| {
            order.push(key);
            CoverageMask::new(min_x, min_y, width, height)
        });
        key
    };

    let mut colors: HashMap<[u32; 4], [f32; 4]> = HashMap::new();
    for placement in &placed {
        let key = group_for(placement.color, &mut order, &mut groups);
        colors.insert(key, placement.color);
        if let Some(mask) = groups.get_mut(&key) {
            blit(mask, placement);
        }
    }
    for decoration in &text.decorations {
        let key = group_for(decoration.color, &mut order, &mut groups);
        colors.insert(key, decoration.color);
        if let Some(mask) = groups.get_mut(&key) {
            fill_rect(mask, &decoration.rect);
        }
    }

    let mut out = LinearImage::new(min_x, min_y, width, height);
    for key in order {
        let (Some(mask), Some(&color)) = (groups.get(&key), colors.get(&key)) else {
            continue;
        };
        out.composite_over(&fill_linear(mask, color));
    }
    out
}

fn color_key(color: [f32; 4]) -> [u32; 4] {
    [
        color[0].to_bits(),
        color[1].to_bits(),
        color[2].to_bits(),
        color[3].to_bits(),
    ]
}

fn bin_index(bin: cosmic_text::SubpixelBin) -> u8 {
    match bin {
        cosmic_text::SubpixelBin::Zero => 0,
        cosmic_text::SubpixelBin::One => 1,
        cosmic_text::SubpixelBin::Two => 2,
        cosmic_text::SubpixelBin::Three => 3,
    }
}

/// How far, in whole pixels, faux bold smears a glyph to each side at a given
/// em size.
///
/// The smear is symmetric: the emboldened bitmap grows by this much on the
/// left *and* on the right, and [`GlyphImage::left`] is pulled back by the same
/// amount, so the thickened stem stays centred on the stem it came from and
/// the glyph keeps the advance the shaper gave it.
#[must_use]
pub fn synthetic_bold_radius(size_px: f32) -> u32 {
    if !size_px.is_finite() {
        return 1;
    }
    ((size_px * SYNTHETIC_BOLD_FACTOR).round().max(1.0) as u32).min(16)
}

fn convert(image: &SwashImage) -> Option<GlyphImage> {
    let width = image.placement.width;
    let height = image.placement.height;
    if width == 0 || height == 0 {
        return None;
    }
    let expected = (width as usize) * (height as usize);
    let data = match image.content {
        SwashContent::Mask => {
            if image.data.len() < expected {
                return None;
            }
            image.data[..expected].to_vec()
        }
        SwashContent::Color => {
            if image.data.len() < expected * 4 {
                return None;
            }
            image
                .data
                .chunks_exact(4)
                .take(expected)
                .map(|p| p[3])
                .collect()
        }
        SwashContent::SubpixelMask => {
            if image.data.len() < expected * 4 {
                return None;
            }
            image
                .data
                .chunks_exact(4)
                .take(expected)
                .map(|p| {
                    let sum = u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]);
                    (sum / 3) as u8
                })
                .collect()
        }
    };
    Some(GlyphImage {
        left: image.placement.left,
        top: image.placement.top,
        width,
        height,
        data,
    })
}

/// Faux bold: a horizontal max-dilation by `radius` pixels each side.
///
/// This is what "synthetic bold" is — the stems get thicker because every
/// covered pixel smears sideways — and doing it on the coverage mask keeps it
/// gamma-neutral.
fn embolden(image: &GlyphImage, radius: u32) -> GlyphImage {
    if radius == 0 {
        return image.clone();
    }
    let radius_i = radius as i32;
    let width = image.width + radius * 2;
    let mut data = vec![0u8; (width as usize) * (image.height as usize)];
    for row in 0..image.height {
        for col in 0..width {
            let mut best = 0u8;
            for delta in -radius_i..=radius_i {
                let source = col as i32 - radius_i + delta;
                if source < 0 || source >= image.width as i32 {
                    continue;
                }
                let value = image.data[(row * image.width + source as u32) as usize];
                best = best.max(value);
            }
            data[(row * width + col) as usize] = best;
        }
    }
    GlyphImage {
        left: image.left - radius_i,
        top: image.top,
        width,
        height: image.height,
        data,
    }
}
