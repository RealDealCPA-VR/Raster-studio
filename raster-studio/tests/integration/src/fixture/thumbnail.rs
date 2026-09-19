//! Deterministic composition fixtures for the thumbnail workflow (plan Task 003).
//!
//! Everything here is generated, not sampled: the same call with the same
//! arguments produces the same bytes on every run, so tests can pin exact
//! probe pixels and content hashes. Two canvas sizes share one geometry — a
//! small CI scene ([`SMALL_CANVAS`]) and the full-size manual/performance
//! scene ([`FULL_CANVAS`], the 3628 × 2041 working size the reference
//! screenshot's status bar shows).
//!
//! # What the fixtures prove — and what they do not
//!
//! The "portrait" is a synthetic head-and-shoulders silhouette with a soft
//! edge. It exercises alpha/coverage **geometry** — feather bands, holes,
//! asymmetry, off-canvas content — and says nothing about real hair or glasses
//! extraction. Tests relying on it label that coverage synthetic; portrait
//! quality on a real photograph is the human gate of plan Task 064.
//!
//! Text layers carry the three fields `TextLayer` persists today (E01). The
//! acceptance scene's white/green headline colours cannot be represented yet;
//! when Task 015/016 widen the schema these layers extend without changing
//! geometry.
//!
//! Font: the embedded DejaVu Sans (`dejavu` crate, freely licensed) — never an
//! installed system font. Composites are byte-deterministic **within a
//! process**; across machines, installed-font seeding may change glyph
//! resolution, so cross-machine byte identity is not claimed.

use std::path::{Path, PathBuf};

use app_shell::doc::OpenDocument;
use glam::Affine2;
use layer_model::{
    AdjustmentKind, AdjustmentLayer, ClippingMode, Layer, LayerId, LayerKind, MaskId, TextLayer,
};

use crate::app::{self, DocExt};

/// The small canvas every CI test uses.
pub const SMALL_CANVAS: (u32, u32) = (256, 144);

/// The full-size working canvas from the reference screenshot's status bar
/// (3628 × 2041 at 33.33% zoom). Only manual/performance runs build this.
pub const FULL_CANVAS: (u32, u32) = (3628, 2041);

/// The family name the embedded fixture font registers under.
pub const FONT_FAMILY: &str = "DejaVu Sans";

/// Load the embedded licensed fixture font into the compositor's library.
///
/// The compositor seeds its library from installed fonts on first use; loading
/// DejaVu here guarantees the fixture family resolves to *these* bytes in this
/// process. Never call `compositor::no_fonts()` from this suite: tests run in
/// parallel threads and share the library.
pub fn load_fixture_font() -> usize {
    compositor::load_font(dejavu::sans::regular().to_vec())
}

/// FNV-1a, 64-bit — the deterministic content hash the fixture documents use.
///
/// Enough to prove "the same bytes came back", trivial to recompute anywhere,
/// and not claimed to be a cryptographic digest.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// An axis-aligned rectangle in document pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// The scene geometry, derived from the canvas so both sizes share proportions.
///
/// Proportions (of canvas width `w` and height `h`):
///
/// - oversized source: `1.5 w × 16/9 h` — strictly larger than the canvas on
///   both axes, so placement has real off-canvas content to keep or clip;
/// - logo: side `4h/9`, at `(w/12, h/12)` — upper-left, with a transparent
///   centre hole and a notch, so hit-testing and compositing see through it;
/// - portrait: `3w/8 × 8h/9`, at `(9w/16, h/16)` — right of centre;
/// - headline `0.30 h` type at `(0.08 w, 0.55 h)`; subhead `0.10 h` at
///   `(0.58 w, 0.06 h)`; two group alternatives `0.08 h` near the bottom.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub canvas: (u32, u32),
    pub oversized: (u32, u32),
    pub portrait: Rect,
    pub logo: Rect,
    pub headline_px: f32,
    pub subhead_px: f32,
    pub alt_px: f32,
    pub headline_origin: (f32, f32),
    pub subhead_origin: (f32, f32),
    pub alt_a_origin: (f32, f32),
    pub alt_b_origin: (f32, f32),
}

/// The layout for a canvas size. Deterministic integer arithmetic.
pub fn layout(canvas_w: u32, canvas_h: u32) -> Layout {
    let portrait = Rect {
        x: canvas_w * 9 / 16,
        y: canvas_h / 16,
        w: canvas_w * 3 / 8,
        h: canvas_h * 8 / 9,
    };
    let side = canvas_h * 4 / 9;
    Layout {
        canvas: (canvas_w, canvas_h),
        oversized: (canvas_w * 3 / 2, canvas_h * 16 / 9),
        logo: Rect {
            x: canvas_w / 12,
            y: canvas_h / 12,
            w: side,
            h: side,
        },
        portrait,
        headline_px: canvas_h as f32 * 0.30,
        subhead_px: canvas_h as f32 * 0.10,
        alt_px: canvas_h as f32 * 0.08,
        headline_origin: (canvas_w as f32 * 0.08, canvas_h as f32 * 0.55),
        subhead_origin: (canvas_w as f32 * 0.58, canvas_h as f32 * 0.06),
        alt_a_origin: (canvas_w as f32 * 0.10, canvas_h as f32 * 0.80),
        alt_b_origin: (canvas_w as f32 * 0.10, canvas_h as f32 * 0.90),
    }
}

/// The fully opaque background: a vertical two-stop gradient.
///
/// Probe-exact at the small size: the top row is exactly `(16, 24, 36)` and the
/// bottom row exactly `(40, 52, 64)`.
pub fn background_rgba8(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    let denom = h.saturating_sub(1).max(1) as f32;
    for y in 0..h {
        let t = y as f32 / denom;
        let row = [
            (16.0 + 24.0 * t).round() as u8,
            (24.0 + 28.0 * t).round() as u8,
            (36.0 + 28.0 * t).round() as u8,
            255,
        ];
        for _ in 0..w {
            out.extend_from_slice(&row);
        }
    }
    out
}

/// The asymmetric oversized source with labeled coloured corners.
///
/// Strictly larger than its canvas in both axes (small scene: 384 × 256 into
/// 256 × 144), with a different ramp in x than in y and four distinct corner
/// labels — top-left red, top-right green, bottom-left blue, bottom-right
/// yellow — plus a white centre dot. Every property a clipping or fitting bug
/// needs to become visible: asymmetric, larger than the canvas, labeled.
pub fn oversized_source_rgba8(w: u32, h: u32) -> Vec<u8> {
    let corner = (w.min(h) / 16).max(12);
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h {
        for x in 0..w {
            let mut rgba = [
                (60u32 + 80 * x / w.max(1)) as u8,
                (40u32 + 60 * y / h.max(1)) as u8,
                90,
                255,
            ];
            if x < corner && y < corner {
                rgba = [200, 32, 32, 255]; // top-left: red
            } else if x >= w.saturating_sub(corner) && y < corner {
                rgba = [32, 190, 70, 255]; // top-right: green
            } else if x < corner && y >= h.saturating_sub(corner) {
                rgba = [40, 96, 230, 255]; // bottom-left: blue
            } else if x >= w.saturating_sub(corner) && y >= h.saturating_sub(corner) {
                rgba = [235, 205, 45, 255]; // bottom-right: yellow
            } else if x.abs_diff(w / 2) < 2 && y.abs_diff(h / 2) < 2 {
                rgba = [255, 255, 255, 255]; // centre probe
            }
            out.extend_from_slice(&rgba);
        }
    }
    out
}

/// The RGBA logo: a teal ring with a fully transparent centre hole, a soft
/// outer edge, and a wedge notch cut out of it — asymmetric on purpose.
pub fn logo_rgba8(side: u32) -> Vec<u8> {
    let c = (side.saturating_sub(1)) as f32 / 2.0;
    let inner = side as f32 * 0.22;
    let outer = side as f32 * 0.48;
    let feather = 1.5_f32;
    let mut out = Vec::with_capacity(side as usize * side as usize * 4);
    for y in 0..side {
        for x in 0..side {
            let dx = x as f32 - c;
            let dy = y as f32 - c;
            let d = dx.hypot(dy);
            // Soft inner (hole) edge and soft outer edge, multiplied.
            let hole = ((d - inner) / feather).clamp(0.0, 1.0);
            let rim = ((outer - d) / feather).clamp(0.0, 1.0);
            let mut a = (rim * hole * 255.0).round() as u8;
            // The notch: a wedge from 20° to 90° (screen angles, +y down) is
            // fully transparent — an asymmetric hole a hit test can fall into.
            let mut deg = dy.atan2(dx).to_degrees();
            if deg < 0.0 {
                deg += 360.0;
            }
            if (20.0..=90.0).contains(&deg) && d >= inner {
                a = 0;
            }
            out.extend_from_slice(&[30, 160, 170, a]);
        }
    }
    out
}

/// The synthetic portrait: a head disc over a shoulders ellipse, soft edges.
///
/// Interior fully opaque, exterior fully transparent, and a feathered boundary
/// so every mask/refine card has a gradient to reason about. It is *not* a
/// photograph; see the module docs.
pub fn portrait_rgba8(pw: u32, ph: u32) -> Vec<u8> {
    let head_c = (pw as f32 * 0.5, ph as f32 * 0.34);
    let head_r = pw as f32 * 0.30;
    let torso_c = (pw as f32 * 0.5, ph as f32 * 0.98);
    let torso_rx = pw as f32 * 0.36;
    let torso_ry = ph as f32 * 0.42;
    let feather = 2.5_f32;
    // The torso's margin comes out of a normalized ellipse; scale it into
    // approximate pixels with the smaller radius so both shapes feather over
    // the same pixel distance the tests observe.
    let torso_units = torso_rx.min(torso_ry);
    let mut out = Vec::with_capacity(pw as usize * ph as usize * 4);
    for y in 0..ph {
        let t = y as f32 / ph.max(1) as f32;
        let rgb = [
            (150.0 + 30.0 * t).round() as u8,
            (140.0 + 28.0 * t).round() as u8,
            (135.0 + 26.0 * t).round() as u8,
        ];
        for x in 0..pw {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let head_margin = head_r - (px - head_c.0).hypot(py - head_c.1);
            let de = ((px - torso_c.0) / torso_rx).powi(2) + ((py - torso_c.1) / torso_ry).powi(2);
            let torso_margin = (1.0 - de.max(0.0).sqrt()) * torso_units;
            let margin = head_margin.max(torso_margin);
            let a = ((margin / feather).clamp(0.0, 1.0) * 255.0).round() as u8;
            out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
        }
    }
    out
}

/// The portrait layer's mask coverage at a document pixel.
///
/// Deliberately *different* from the portrait's own alpha: a horizontal ramp
/// across the portrait's width (transparent left of `x0 + 5% pw`, opaque right
/// of `x0 + 30% pw`), zero outside the portrait rectangle. Mask ≠ alpha is the
/// distinction the mask cards (057–059) assert against.
pub fn portrait_mask_coverage(l: &Layout, x: u32, y: u32) -> u8 {
    let p = l.portrait;
    if !p.contains(x, y) {
        return 0;
    }
    let start = p.x + p.w * 5 / 100;
    let full = p.x + p.w * 30 / 100;
    if x < start {
        0
    } else if x >= full {
        255
    } else {
        (((x - start) as f32 / (full - start).max(1) as f32) * 255.0).round() as u8
    }
}

/// The four fixture images, written as PNGs into `dir`, deterministically.
#[derive(Debug, Clone)]
pub struct ThumbnailAssets {
    pub background: PathBuf,
    pub oversized: PathBuf,
    pub logo: PathBuf,
    pub portrait: PathBuf,
}

/// Write the fixture images as PNGs into `dir` (deterministic bytes).
pub fn write_assets(
    dir: &Path,
    canvas_w: u32,
    canvas_h: u32,
) -> Result<ThumbnailAssets, CodecError> {
    let l = layout(canvas_w, canvas_h);
    let write = |name: &str, w: u32, h: u32, rgba: &[u8]| -> Result<PathBuf, CodecError> {
        let path = dir.join(name);
        crate::fixture::write_image(&path, ExportFormat::Png, w, h, rgba)?;
        Ok(path)
    };
    Ok(ThumbnailAssets {
        background: write(
            "thumbnail-background.png",
            l.canvas.0,
            l.canvas.1,
            &background_rgba8(l.canvas.0, l.canvas.1),
        )?,
        oversized: write(
            "thumbnail-oversized.png",
            l.oversized.0,
            l.oversized.1,
            &oversized_source_rgba8(l.oversized.0, l.oversized.1),
        )?,
        logo: write(
            "thumbnail-logo.png",
            l.logo.w,
            l.logo.h,
            &logo_rgba8(l.logo.w),
        )?,
        portrait: write(
            "thumbnail-portrait.png",
            l.portrait.w,
            l.portrait.h,
            &portrait_rgba8(l.portrait.w, l.portrait.h),
        )?,
    })
}

use raster::{CodecError, ExportFormat};

/// Layer ids a built scene hands back, for structure and probe assertions.
#[derive(Debug, Clone, Copy)]
pub struct ThumbnailIds {
    pub background: LayerId,
    pub headline: LayerId,
    pub subhead: LayerId,
    pub portrait: LayerId,
    pub portrait_mask: MaskId,
    pub tone: LayerId,
    pub alternatives: LayerId,
    pub alt_a: LayerId,
    pub alt_b: LayerId,
    pub logos: LayerId,
    pub logo: LayerId,
}

/// The built scene: an `OpenDocument` plus the ids and geometry of everything.
#[derive(Debug)]
pub struct ThumbnailScene {
    pub doc: OpenDocument,
    pub ids: ThumbnailIds,
    pub layout: Layout,
}

/// A text layer at `origin` with the fixture family. Carries the three fields
/// `TextLayer` persists today; positioned through the layer transform, which
/// is how the compositor places a text block (E01: no richer fields yet).
fn text_layer(name: &str, text: &str, size_px: f32, origin: (f32, f32)) -> Layer {
    let mut layer = Layer::with_kind(
        name,
        LayerKind::Text(TextLayer {
            text: text.to_string(),
            font_family: FONT_FAMILY.to_string(),
            size_px,
            ..Default::default()
        }),
    );
    layer.transform = Affine2::from_translation(glam::Vec2::new(origin.0, origin.1));
    layer
}

/// Build the layered thumbnail scene through the application's own commands.
///
/// Root stack, top to bottom: `Alternatives` group → `Portrait tone` (clipped
/// adjustment) → `Portrait` (raster + raster mask) → `Headline` (text) →
/// `Subhead` (text) → `Background` (raster). `Alternatives` contains, top to
/// bottom: `Headline A` (text), `Logos` (nested group holding the logo
/// raster), `Headline B` (text, hidden).
pub fn build_scene(canvas_w: u32, canvas_h: u32, title: &str) -> ThumbnailScene {
    let l = layout(canvas_w, canvas_h);
    let mut doc = app::blank(canvas_w, canvas_h, title);
    let bg_buf = background_rgba8(canvas_w, canvas_h);
    let portrait_buf = portrait_rgba8(l.portrait.w, l.portrait.h);
    let logo_buf = logo_rgba8(l.logo.w);

    // The layer File > New already made becomes the background.
    let background = doc
        .document
        .active_layer()
        .expect("File > New makes a layer");
    doc.set_props(
        background,
        editor_core::LayerPatch {
            name: Some("Background".to_string()),
            ..Default::default()
        },
    );
    doc.paint_canvas(background, &|x, y| {
        let i = (y as usize * canvas_w as usize + x as usize) * 4;
        [bg_buf[i], bg_buf[i + 1], bg_buf[i + 2], bg_buf[i + 3]]
    });

    let subhead = doc.add_layer(text_layer(
        "Subhead",
        "edit me",
        l.subhead_px,
        l.subhead_origin,
    ));
    let headline = doc.add_layer(text_layer(
        "Headline",
        "THUMBNAIL",
        l.headline_px,
        l.headline_origin,
    ));
    let portrait = doc.add_layer(Layer::raster("Portrait"));
    let tone = doc.add_layer(Layer::with_kind(
        "Portrait tone",
        LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Levels {
                black: 0.0,
                white: 1.0,
                gamma: 1.25,
            },
        }),
    ));
    doc.set_props(
        tone,
        editor_core::LayerPatch {
            clipping: Some(ClippingMode::ClipToBelow),
            ..Default::default()
        },
    );

    let alternatives = doc.add_layer(Layer::group("Alternatives"));
    // add_child inserts at index 0 = top-most, so insert bottom-up to end up
    // with [Headline A, Logos, Headline B] top to bottom.
    let alt_b = doc.add_child(
        alternatives,
        text_layer("Headline B", "ALT B", l.alt_px, l.alt_b_origin),
    );
    let logos = doc.add_child(alternatives, Layer::group("Logos"));
    let alt_a = doc.add_child(
        alternatives,
        text_layer("Headline A", "ALT A", l.alt_px, l.alt_a_origin),
    );
    let logo = doc.add_child(logos, Layer::raster("Logo"));

    // Paint the raster layers at their document positions.
    doc.paint_canvas(portrait, &|x, y| {
        if l.portrait.contains(x, y) {
            let i = ((y - l.portrait.y) as usize * l.portrait.w as usize
                + (x - l.portrait.x) as usize)
                * 4;
            [
                portrait_buf[i],
                portrait_buf[i + 1],
                portrait_buf[i + 2],
                portrait_buf[i + 3],
            ]
        } else {
            [0, 0, 0, 0]
        }
    });
    doc.paint_canvas(logo, &|x, y| {
        if l.logo.contains(x, y) {
            let i = ((y - l.logo.y) as usize * l.logo.w as usize + (x - l.logo.x) as usize) * 4;
            [
                logo_buf[i],
                logo_buf[i + 1],
                logo_buf[i + 2],
                logo_buf[i + 3],
            ]
        } else {
            [0, 0, 0, 0]
        }
    });

    // The portrait mask: the horizontal ramp, distinct from the alpha.
    let portrait_mask = doc.attach_mask(portrait);
    doc.paint_canvas_mask(portrait, &|x, y| portrait_mask_coverage(&l, x, y));

    // Headline B is the hidden alternative.
    doc.set_props(
        alt_b,
        editor_core::LayerPatch {
            visible: Some(false),
            ..Default::default()
        },
    );

    ThumbnailScene {
        doc,
        ids: ThumbnailIds {
            background,
            headline,
            subhead,
            portrait,
            portrait_mask,
            tone,
            alternatives,
            alt_a,
            alt_b,
            logos,
            logo,
        },
        layout: l,
    }
}
