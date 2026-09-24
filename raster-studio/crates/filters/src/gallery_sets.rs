//! Filter Gallery sets: Photoshop's Artistic, Brush Strokes, Sketch and
//! Texture effects, and the stackable effect list the gallery applies.
//!
//! Every effect is a deterministic image operation: a pure function of the
//! source pixels, its parameter values and a fixed per-effect seed (the
//! noise-like ones hash the destination coordinate, see [`crate::rng`]), so a
//! stack previewed on a downscaled proxy and applied to the full layer is the
//! same recipe, and two runs are bit-identical.
//!
//! # Working space
//!
//! The effects operate on **straight** (unpremultiplied) linear RGB in
//! `[0, 1]` and hand alpha through unchanged: they are painterly re-renders of
//! the colour, not of the coverage, so a half-transparent brush stroke is
//! re-rendered as the colour it is. Luminance is Rec. 709 on those values.
//!
//! The Sketch set draws in *ink* on *paper*, black on white — Photoshop's
//! default foreground and background colours — except Conte Crayon, which
//! uses a sepia crayon on cream paper, as Photoshop's default does.
//!
//! # Parameters
//!
//! Each effect lists its parameters as [`GalleryParam`]s: a key, a label, an
//! inclusive integer range and a default, matching Photoshop's slider ranges
//! where Photoshop has the same control. Values are clamped into range and a
//! non-finite value reads as the default, so no value can make an effect
//! panic or run unbounded.

use serde::{Deserialize, Serialize};

use crate::blur::gaussian_blur;
use crate::buffer::FilterBuffer;
use crate::noise::median;
use crate::rng::{hash_unit, Perlin};
use crate::stylize::oil_paint;
use crate::support::{EdgeMode, Sampling};
use rayon::prelude::*;

/// The four gallery folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GallerySet {
    Artistic,
    BrushStrokes,
    Sketch,
    Texture,
}

impl GallerySet {
    /// Every set, in the gallery's folder order.
    pub const ALL: [GallerySet; 4] = [
        GallerySet::Artistic,
        GallerySet::BrushStrokes,
        GallerySet::Sketch,
        GallerySet::Texture,
    ];

    /// The folder's name as Photoshop shows it.
    pub fn name(self) -> &'static str {
        match self {
            GallerySet::Artistic => "Artistic",
            GallerySet::BrushStrokes => "Brush Strokes",
            GallerySet::Sketch => "Sketch",
            GallerySet::Texture => "Texture",
        }
    }

    /// The effects in this folder, in Photoshop's order.
    pub fn effects(self) -> impl Iterator<Item = GalleryEffect> {
        GalleryEffect::ALL
            .into_iter()
            .filter(move |e| e.set() == self)
    }
}

/// One integer slider of a gallery effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GalleryParam {
    /// Stable key, for presets and tests.
    pub key: &'static str,
    /// The label the gallery shows.
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

const fn p(
    key: &'static str,
    label: &'static str,
    min: f32,
    max: f32,
    default: f32,
) -> GalleryParam {
    GalleryParam {
        key,
        label,
        min,
        max,
        default,
    }
}

macro_rules! effects {
    ($( $variant:ident => $name:literal, $set:ident, [$($param:expr),* $(,)?]; )*) => {
        /// One Filter Gallery effect. See each variant's `apply_*` function
        /// for the operation it performs.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum GalleryEffect { $($variant,)* }

        impl GalleryEffect {
            /// Every effect, grouped by set in Photoshop's order.
            pub const ALL: [GalleryEffect; 43] = [$(GalleryEffect::$variant,)*];

            /// The effect's name as Photoshop shows it.
            pub fn name(self) -> &'static str {
                match self { $(GalleryEffect::$variant => $name,)* }
            }

            /// The folder the effect lives in.
            pub fn set(self) -> GallerySet {
                match self { $(GalleryEffect::$variant => GallerySet::$set,)* }
            }

            /// The effect's sliders, in display order.
            pub fn params(self) -> &'static [GalleryParam] {
                match self { $(GalleryEffect::$variant => {
                    const P: &[GalleryParam] = &[$($param),*];
                    P
                })* }
            }
        }
    };
}

effects! {
    // ---- Artistic -----------------------------------------------------------
    ColoredPencil => "Colored Pencil", Artistic, [
        p("pencil_width", "Pencil Width", 1.0, 24.0, 4.0),
        p("stroke_pressure", "Stroke Pressure", 0.0, 15.0, 8.0),
        p("paper_brightness", "Paper Brightness", 0.0, 50.0, 25.0),
    ];
    Cutout => "Cutout", Artistic, [
        p("levels", "Number of Levels", 2.0, 8.0, 4.0),
        p("edge_simplicity", "Edge Simplicity", 0.0, 10.0, 4.0),
        p("edge_fidelity", "Edge Fidelity", 1.0, 3.0, 2.0),
    ];
    DryBrush => "Dry Brush", Artistic, [
        p("brush_size", "Brush Size", 0.0, 10.0, 2.0),
        p("brush_detail", "Brush Detail", 0.0, 10.0, 8.0),
        p("texture", "Texture", 1.0, 3.0, 1.0),
    ];
    FilmGrain => "Film Grain", Artistic, [
        p("grain", "Grain", 0.0, 20.0, 4.0),
        p("highlight_area", "Highlight Area", 0.0, 20.0, 0.0),
        p("intensity", "Intensity", 0.0, 10.0, 10.0),
    ];
    Fresco => "Fresco", Artistic, [
        p("brush_size", "Brush Size", 0.0, 10.0, 2.0),
        p("brush_detail", "Brush Detail", 0.0, 10.0, 8.0),
        p("texture", "Texture", 1.0, 3.0, 1.0),
    ];
    NeonGlow => "Neon Glow", Artistic, [
        p("glow_size", "Glow Size", -24.0, 24.0, 5.0),
        p("glow_brightness", "Glow Brightness", 0.0, 50.0, 15.0),
        p("glow_hue", "Glow Hue", 0.0, 360.0, 190.0),
    ];
    PaintDaubs => "Paint Daubs", Artistic, [
        p("brush_size", "Brush Size", 1.0, 50.0, 8.0),
        p("sharpness", "Sharpness", 0.0, 40.0, 7.0),
    ];
    PaletteKnife => "Palette Knife", Artistic, [
        p("stroke_size", "Stroke Size", 1.0, 50.0, 25.0),
        p("stroke_detail", "Stroke Detail", 1.0, 3.0, 3.0),
        p("softness", "Softness", 0.0, 10.0, 0.0),
    ];
    PlasticWrap => "Plastic Wrap", Artistic, [
        p("highlight_strength", "Highlight Strength", 0.0, 20.0, 15.0),
        p("detail", "Detail", 1.0, 15.0, 9.0),
        p("smoothness", "Smoothness", 1.0, 15.0, 7.0),
    ];
    PosterEdges => "Poster Edges", Artistic, [
        p("edge_thickness", "Edge Thickness", 0.0, 10.0, 2.0),
        p("edge_intensity", "Edge Intensity", 0.0, 10.0, 1.0),
        p("posterization", "Posterization", 0.0, 6.0, 2.0),
    ];
    RoughPastels => "Rough Pastels", Artistic, [
        p("stroke_length", "Stroke Length", 0.0, 40.0, 6.0),
        p("stroke_detail", "Stroke Detail", 1.0, 20.0, 4.0),
        p("scaling", "Scaling", 50.0, 200.0, 100.0),
    ];
    SmudgeStick => "Smudge Stick", Artistic, [
        p("stroke_length", "Stroke Length", 0.0, 10.0, 2.0),
        p("highlight_area", "Highlight Area", 0.0, 20.0, 0.0),
        p("intensity", "Intensity", 0.0, 10.0, 10.0),
    ];
    Sponge => "Sponge", Artistic, [
        p("brush_size", "Brush Size", 0.0, 10.0, 2.0),
        p("definition", "Definition", 0.0, 25.0, 12.0),
        p("smoothness", "Smoothness", 1.0, 15.0, 5.0),
    ];
    Underpainting => "Underpainting", Artistic, [
        p("brush_size", "Brush Size", 0.0, 40.0, 6.0),
        p("texture_coverage", "Texture Coverage", 0.0, 40.0, 16.0),
    ];
    Watercolor => "Watercolor", Artistic, [
        p("brush_detail", "Brush Detail", 1.0, 14.0, 9.0),
        p("shadow_intensity", "Shadow Intensity", 0.0, 10.0, 1.0),
        p("texture", "Texture", 1.0, 3.0, 1.0),
    ];
    // ---- Brush Strokes ------------------------------------------------------
    AccentedEdges => "Accented Edges", BrushStrokes, [
        p("edge_width", "Edge Width", 1.0, 14.0, 2.0),
        p("edge_brightness", "Edge Brightness", 0.0, 50.0, 38.0),
        p("smoothness", "Smoothness", 1.0, 15.0, 5.0),
    ];
    AngledStrokes => "Angled Strokes", BrushStrokes, [
        p("direction_balance", "Direction Balance", 0.0, 100.0, 50.0),
        p("stroke_length", "Stroke Length", 3.0, 50.0, 15.0),
        p("sharpness", "Sharpness", 0.0, 10.0, 3.0),
    ];
    Crosshatch => "Crosshatch", BrushStrokes, [
        p("stroke_length", "Stroke Length", 3.0, 50.0, 9.0),
        p("sharpness", "Sharpness", 0.0, 20.0, 6.0),
        p("strength", "Strength", 1.0, 3.0, 1.0),
    ];
    DarkStrokes => "Dark Strokes", BrushStrokes, [
        p("balance", "Balance", 0.0, 10.0, 5.0),
        p("black_intensity", "Black Intensity", 0.0, 10.0, 6.0),
        p("white_intensity", "White Intensity", 0.0, 10.0, 2.0),
    ];
    InkOutlines => "Ink Outlines", BrushStrokes, [
        p("stroke_length", "Stroke Length", 1.0, 50.0, 4.0),
        p("dark_intensity", "Dark Intensity", 0.0, 50.0, 20.0),
        p("light_intensity", "Light Intensity", 0.0, 50.0, 10.0),
    ];
    Spatter => "Spatter", BrushStrokes, [
        p("spray_radius", "Spray Radius", 0.0, 25.0, 10.0),
        p("smoothness", "Smoothness", 1.0, 15.0, 5.0),
    ];
    SprayedStrokes => "Sprayed Strokes", BrushStrokes, [
        p("stroke_length", "Stroke Length", 0.0, 20.0, 12.0),
        p("spray_radius", "Spray Radius", 0.0, 25.0, 7.0),
        p("direction", "Stroke Direction", 0.0, 3.0, 0.0),
    ];
    SumiE => "Sumi-e", BrushStrokes, [
        p("stroke_width", "Stroke Width", 3.0, 15.0, 10.0),
        p("stroke_pressure", "Stroke Pressure", 0.0, 15.0, 2.0),
        p("contrast", "Contrast", 0.0, 40.0, 16.0),
    ];
    // ---- Sketch -------------------------------------------------------------
    BasRelief => "Bas Relief", Sketch, [
        p("detail", "Detail", 1.0, 15.0, 13.0),
        p("smoothness", "Smoothness", 1.0, 15.0, 3.0),
        p("light", "Light", 0.0, 7.0, 0.0),
    ];
    ChalkCharcoal => "Chalk & Charcoal", Sketch, [
        p("charcoal_area", "Charcoal Area", 0.0, 20.0, 6.0),
        p("chalk_area", "Chalk Area", 0.0, 20.0, 6.0),
        p("stroke_pressure", "Stroke Pressure", 0.0, 5.0, 1.0),
    ];
    Charcoal => "Charcoal", Sketch, [
        p("thickness", "Charcoal Thickness", 1.0, 7.0, 1.0),
        p("detail", "Detail", 0.0, 5.0, 5.0),
        p("balance", "Light/Dark Balance", 0.0, 100.0, 50.0),
    ];
    Chrome => "Chrome", Sketch, [
        p("detail", "Detail", 0.0, 10.0, 4.0),
        p("smoothness", "Smoothness", 0.0, 10.0, 7.0),
    ];
    ConteCrayon => "Conte Crayon", Sketch, [
        p("foreground_level", "Foreground Level", 1.0, 15.0, 11.0),
        p("background_level", "Background Level", 1.0, 15.0, 7.0),
        p("scaling", "Scaling", 50.0, 200.0, 100.0),
    ];
    GraphicPen => "Graphic Pen", Sketch, [
        p("stroke_length", "Stroke Length", 1.0, 15.0, 15.0),
        p("balance", "Light/Dark Balance", 0.0, 100.0, 50.0),
        p("direction", "Stroke Direction", 0.0, 3.0, 0.0),
    ];
    HalftonePattern => "Halftone Pattern", Sketch, [
        p("size", "Size", 1.0, 12.0, 1.0),
        p("contrast", "Contrast", 0.0, 50.0, 5.0),
        p("pattern", "Pattern Type", 0.0, 2.0, 0.0),
    ];
    NotePaper => "Note Paper", Sketch, [
        p("image_balance", "Image Balance", 0.0, 50.0, 25.0),
        p("graininess", "Graininess", 0.0, 20.0, 10.0),
        p("relief", "Relief", 0.0, 25.0, 11.0),
    ];
    Photocopy => "Photocopy", Sketch, [
        p("detail", "Detail", 1.0, 24.0, 7.0),
        p("darkness", "Darkness", 1.0, 50.0, 8.0),
    ];
    Plaster => "Plaster", Sketch, [
        p("image_balance", "Image Balance", 0.0, 50.0, 20.0),
        p("smoothness", "Smoothness", 1.0, 15.0, 2.0),
        p("light", "Light", 0.0, 7.0, 0.0),
    ];
    Reticulation => "Reticulation", Sketch, [
        p("density", "Density", 0.0, 50.0, 12.0),
        p("foreground_level", "Foreground Level", 0.0, 50.0, 40.0),
        p("background_level", "Background Level", 0.0, 50.0, 5.0),
    ];
    Stamp => "Stamp", Sketch, [
        p("balance", "Light/Dark Balance", 0.0, 50.0, 25.0),
        p("smoothness", "Smoothness", 1.0, 50.0, 5.0),
    ];
    TornEdges => "Torn Edges", Sketch, [
        p("image_balance", "Image Balance", 0.0, 50.0, 25.0),
        p("smoothness", "Smoothness", 1.0, 15.0, 11.0),
        p("contrast", "Contrast", 1.0, 25.0, 17.0),
    ];
    WaterPaper => "Water Paper", Sketch, [
        p("fiber_length", "Fiber Length", 3.0, 50.0, 15.0),
        p("brightness", "Brightness", 0.0, 100.0, 60.0),
        p("contrast", "Contrast", 0.0, 100.0, 80.0),
    ];
    // ---- Texture ------------------------------------------------------------
    Craquelure => "Craquelure", Texture, [
        p("crack_spacing", "Crack Spacing", 2.0, 100.0, 15.0),
        p("crack_depth", "Crack Depth", 0.0, 10.0, 6.0),
        p("crack_brightness", "Crack Brightness", 0.0, 10.0, 9.0),
    ];
    Grain => "Grain", Texture, [
        p("intensity", "Intensity", 0.0, 100.0, 40.0),
        p("contrast", "Contrast", 0.0, 100.0, 50.0),
        p("grain_type", "Grain Type", 0.0, 3.0, 0.0),
    ];
    MosaicTiles => "Mosaic Tiles", Texture, [
        p("tile_size", "Tile Size", 2.0, 100.0, 12.0),
        p("grout_width", "Grout Width", 1.0, 15.0, 3.0),
        p("lighten_grout", "Lighten Grout", 0.0, 10.0, 9.0),
    ];
    Patchwork => "Patchwork", Texture, [
        p("square_size", "Square Size", 0.0, 10.0, 4.0),
        p("relief", "Relief", 0.0, 25.0, 8.0),
    ];
    StainedGlass => "Stained Glass", Texture, [
        p("cell_size", "Cell Size", 2.0, 50.0, 10.0),
        p("border_thickness", "Border Thickness", 1.0, 20.0, 4.0),
        p("light_intensity", "Light Intensity", 0.0, 10.0, 3.0),
    ];
    Texturizer => "Texturizer", Texture, [
        p("scaling", "Scaling", 50.0, 200.0, 100.0),
        p("relief", "Relief", 0.0, 50.0, 4.0),
        p("texture", "Texture", 0.0, 3.0, 2.0),
        p("light", "Light", 0.0, 7.0, 0.0),
    ];
}

impl GalleryEffect {
    /// The effect's parameters at their defaults.
    pub fn defaults(self) -> Vec<f32> {
        self.params().iter().map(|p| p.default).collect()
    }

    /// Apply the effect to `src` with `values` (one per [`Self::params`]
    /// entry; missing or non-finite entries read as the default, and every
    /// value is rounded and clamped into its range).
    pub fn apply(self, src: &FilterBuffer, values: &[f32]) -> FilterBuffer {
        if src.is_empty() {
            return src.clone();
        }
        let v = Values::resolve(self, values);
        let seed = 0x05A1_1E27 ^ (self as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let img = Img::from_buffer(src);
        let out = match self {
            GalleryEffect::ColoredPencil => colored_pencil(src, &img, &v),
            GalleryEffect::Cutout => cutout(src, &v),
            GalleryEffect::DryBrush => dry_brush(src, &v, seed),
            GalleryEffect::FilmGrain => film_grain(&img, &v, seed),
            GalleryEffect::Fresco => fresco(src, &v, seed),
            GalleryEffect::NeonGlow => neon_glow(&img, &v),
            GalleryEffect::PaintDaubs => paint_daubs(src, &v),
            GalleryEffect::PaletteKnife => palette_knife(src, &v),
            GalleryEffect::PlasticWrap => plastic_wrap(&img, &v),
            GalleryEffect::PosterEdges => poster_edges(&img, &v),
            GalleryEffect::RoughPastels => rough_pastels(src, &v, seed),
            GalleryEffect::SmudgeStick => smudge_stick(src, &v),
            GalleryEffect::Sponge => sponge(src, &v, seed),
            GalleryEffect::Underpainting => underpainting(src, &v),
            GalleryEffect::Watercolor => watercolor(src, &v, seed),
            GalleryEffect::AccentedEdges => accented_edges(&img, &v),
            GalleryEffect::AngledStrokes => angled_strokes(src, &img, &v),
            GalleryEffect::Crosshatch => crosshatch(src, &img, &v),
            GalleryEffect::DarkStrokes => dark_strokes(src, &img, &v),
            GalleryEffect::InkOutlines => ink_outlines(src, &img, &v),
            GalleryEffect::Spatter => spatter(&img, v.get(0), v.get(1), seed),
            GalleryEffect::SprayedStrokes => sprayed_strokes(src, &v, seed),
            GalleryEffect::SumiE => sumi_e(src, &v),
            GalleryEffect::BasRelief => bas_relief(&img, &v),
            GalleryEffect::ChalkCharcoal => chalk_charcoal(&img, &v, seed),
            GalleryEffect::Charcoal => charcoal(src, &img, &v),
            GalleryEffect::Chrome => chrome(&img, &v),
            GalleryEffect::ConteCrayon => conte_crayon(&img, &v, seed),
            GalleryEffect::GraphicPen => graphic_pen(&img, &v, seed),
            GalleryEffect::HalftonePattern => halftone_pattern(&img, &v),
            GalleryEffect::NotePaper => note_paper(&img, &v, seed),
            GalleryEffect::Photocopy => photocopy(&img, &v),
            GalleryEffect::Plaster => plaster(&img, &v),
            GalleryEffect::Reticulation => reticulation(&img, &v, seed),
            GalleryEffect::Stamp => stamp(&img, &v),
            GalleryEffect::TornEdges => torn_edges(&img, &v, seed),
            GalleryEffect::WaterPaper => water_paper(src, &v, seed),
            GalleryEffect::Craquelure => craquelure(&img, &v, seed),
            GalleryEffect::Grain => grain(&img, &v, seed),
            GalleryEffect::MosaicTiles => mosaic_tiles(&img, &v),
            GalleryEffect::Patchwork => patchwork(&img, &v, seed),
            GalleryEffect::StainedGlass => stained_glass(&img, &v, seed),
            GalleryEffect::Texturizer => texturizer(&img, &v, seed),
        };
        out.to_buffer()
    }
}

/// One entry of the gallery's effect list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GalleryLayer {
    pub effect: GalleryEffect,
    /// One value per [`GalleryEffect::params`] entry.
    pub values: Vec<f32>,
    /// The eye toggle: a hidden layer stays in the list but is skipped.
    #[serde(default = "visible_default")]
    pub visible: bool,
}

fn visible_default() -> bool {
    true
}

impl GalleryLayer {
    /// `effect` at its default parameters, visible.
    pub fn new(effect: GalleryEffect) -> Self {
        Self {
            effect,
            values: effect.defaults(),
            visible: true,
        }
    }
}

/// The gallery's stacked effect list, applied bottom (index 0) to top.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GalleryStack {
    #[serde(default)]
    pub layers: Vec<GalleryLayer>,
}

impl GalleryStack {
    /// Whether applying the stack could change nothing because no layer is
    /// visible.
    pub fn is_identity(&self) -> bool {
        !self.layers.iter().any(|l| l.visible)
    }

    /// Run every visible layer over `src`, in order.
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        let mut out = src.clone();
        for layer in self.layers.iter().filter(|l| l.visible) {
            out = layer.effect.apply(&out, &layer.values);
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Parameter resolution
// ---------------------------------------------------------------------------

struct Values(Vec<f32>);

impl Values {
    fn resolve(effect: GalleryEffect, raw: &[f32]) -> Self {
        Values(
            effect
                .params()
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let v = raw.get(i).copied().filter(|v| v.is_finite());
                    v.unwrap_or(p.default).round().clamp(p.min, p.max)
                })
                .collect(),
        )
    }

    fn get(&self, i: usize) -> f32 {
        self.0.get(i).copied().unwrap_or(0.0)
    }
}

// ---------------------------------------------------------------------------
// Working images
// ---------------------------------------------------------------------------

/// A straight-alpha image.
#[derive(Clone)]
struct Img {
    w: u32,
    h: u32,
    px: Vec<[f32; 4]>,
}

/// One scalar per pixel.
struct Plane {
    w: u32,
    h: u32,
    v: Vec<f32>,
}

fn build<T: Send>(w: u32, h: u32, f: impl Fn(u32, u32) -> T + Sync) -> Vec<T> {
    let w = w as usize;
    (0..w * h as usize)
        .into_par_iter()
        .map(|i| f((i % w) as u32, (i / w) as u32))
        .collect()
}

fn luma(c: [f32; 4]) -> f32 {
    (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]).clamp(0.0, 1.0)
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if e1 <= e0 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn posterize(v: f32, levels: f32) -> f32 {
    let n = levels.max(2.0) - 1.0;
    (v.clamp(0.0, 1.0) * n).round() / n
}

/// The unit direction a 0..=7 light index names, at 45-degree steps
/// counter-clockwise from "light from the left".
fn light_dir(index: f32) -> (f32, f32) {
    let a = (index * 45.0).to_radians();
    (a.cos(), -a.sin())
}

impl Img {
    fn from_buffer(b: &FilterBuffer) -> Self {
        let (w, h) = b.dimensions();
        Img {
            w,
            h,
            px: b
                .pixels()
                .iter()
                .map(|p| color::unpremultiply(*p))
                .collect(),
        }
    }

    fn to_buffer(&self) -> FilterBuffer {
        let px = self
            .px
            .iter()
            .map(|p| {
                let a = p[3].clamp(0.0, 1.0);
                color::premultiply([
                    p[0].clamp(0.0, 1.0),
                    p[1].clamp(0.0, 1.0),
                    p[2].clamp(0.0, 1.0),
                    a,
                ])
            })
            .collect();
        FilterBuffer::from_pixels(self.w, self.h, px).expect("same size as the source")
    }

    fn at(&self, x: i64, y: i64) -> [f32; 4] {
        let xi = x.clamp(0, i64::from(self.w) - 1) as usize;
        let yi = y.clamp(0, i64::from(self.h) - 1) as usize;
        self.px[yi * self.w as usize + xi]
    }

    fn get(&self, x: u32, y: u32) -> [f32; 4] {
        self.px[(y * self.w + x) as usize]
    }

    fn luma(&self) -> Plane {
        Plane {
            w: self.w,
            h: self.h,
            v: self.px.iter().map(|p| luma(*p)).collect(),
        }
    }

    /// A new image whose colour at every pixel is `f(x, y, colour)`; alpha is
    /// the source's.
    fn map(&self, f: impl Fn(u32, u32, [f32; 4]) -> [f32; 3] + Sync) -> Img {
        let px = build(self.w, self.h, |x, y| {
            let c = self.get(x, y);
            let o = f(x, y, c);
            [o[0], o[1], o[2], c[3]]
        });
        Img {
            w: self.w,
            h: self.h,
            px,
        }
    }

    /// The same colour for all three channels.
    fn grey(&self, f: impl Fn(u32, u32, [f32; 4]) -> f32 + Sync) -> Img {
        self.map(|x, y, c| {
            let g = f(x, y, c);
            [g, g, g]
        })
    }
}

impl Plane {
    fn at(&self, x: i64, y: i64) -> f32 {
        let xi = x.clamp(0, i64::from(self.w) - 1) as usize;
        let yi = y.clamp(0, i64::from(self.h) - 1) as usize;
        self.v[yi * self.w as usize + xi]
    }

    fn get(&self, x: u32, y: u32) -> f32 {
        self.v[(y * self.w + x) as usize]
    }

    fn from_fn(w: u32, h: u32, f: impl Fn(u32, u32) -> f32 + Sync) -> Plane {
        Plane {
            w,
            h,
            v: build(w, h, f),
        }
    }

    /// Separable Gaussian with clamped edges; sigma is capped at 32.
    fn blur(&self, sigma: f32) -> Plane {
        if !sigma.is_finite() || sigma <= 0.0 {
            return Plane {
                w: self.w,
                h: self.h,
                v: self.v.clone(),
            };
        }
        let sigma = sigma.min(32.0);
        let r = (sigma * 3.0).ceil() as i64;
        let mut k: Vec<f32> = (-r..=r)
            .map(|i| (-((i * i) as f32) / (2.0 * sigma * sigma)).exp())
            .collect();
        let sum: f32 = k.iter().sum();
        k.iter_mut().for_each(|v| *v /= sum);
        let horizontal = Plane::from_fn(self.w, self.h, |x, y| {
            k.iter()
                .enumerate()
                .map(|(i, w)| w * self.at(i64::from(x) + i as i64 - r, i64::from(y)))
                .sum()
        });
        Plane::from_fn(self.w, self.h, |x, y| {
            k.iter()
                .enumerate()
                .map(|(i, w)| w * horizontal.at(i64::from(x), i64::from(y) + i as i64 - r))
                .sum()
        })
    }

    /// Sobel gradient at a pixel.
    fn sobel(&self, x: u32, y: u32) -> (f32, f32) {
        let (x, y) = (i64::from(x), i64::from(y));
        let s = |dx: i64, dy: i64| self.at(x + dx, y + dy);
        let gx = (s(1, -1) + 2.0 * s(1, 0) + s(1, 1)) - (s(-1, -1) + 2.0 * s(-1, 0) + s(-1, 1));
        let gy = (s(-1, 1) + 2.0 * s(0, 1) + s(1, 1)) - (s(-1, -1) + 2.0 * s(0, -1) + s(1, -1));
        (gx / 4.0, gy / 4.0)
    }

    fn edge(&self, x: u32, y: u32) -> f32 {
        let (gx, gy) = self.sobel(x, y);
        (gx * gx + gy * gy).sqrt()
    }

    /// Directional relief: how much the height rises toward the light.
    fn relief(&self, x: u32, y: u32, light: (f32, f32), reach: f32) -> f32 {
        let (x, y) = (x as f32, y as f32);
        let a = self.at(
            (x + light.0 * reach).round() as i64,
            (y + light.1 * reach).round() as i64,
        );
        let b = self.at(
            (x - light.0 * reach).round() as i64,
            (y - light.1 * reach).round() as i64,
        );
        a - b
    }
}

fn gauss(src: &FilterBuffer, sigma: f32) -> FilterBuffer {
    gaussian_blur(src, sigma, EdgeMode::Clamp)
}

fn streak(src: &FilterBuffer, angle: f32, length: f32) -> FilterBuffer {
    crate::blur::motion_blur(src, angle, length, Sampling::clamped())
}

/// `base + (base - blur(base)) * amount`, per channel.
fn unsharp(base: &Img, sigma: f32, amount: f32) -> Img {
    if amount <= 0.0 {
        return base.clone();
    }
    let blurred = Img::from_buffer(&gauss(&base.to_buffer(), sigma));
    base.map(|x, y, c| {
        let b = blurred.get(x, y);
        [
            c[0] + (c[0] - b[0]) * amount,
            c[1] + (c[1] - b[1]) * amount,
            c[2] + (c[2] - b[2]) * amount,
        ]
    })
}

fn noise(seed: u64, x: u32, y: u32) -> f32 {
    hash_unit(seed, i64::from(x), i64::from(y))
}

/// Voronoi over a jittered grid of `cell`-sized squares: the distances to the
/// nearest and second-nearest sites, and the nearest site's position.
fn voronoi(x: f32, y: f32, cell: f32, seed: u64) -> (f32, f32, [f32; 2]) {
    let cx = (x / cell).floor() as i64;
    let cy = (y / cell).floor() as i64;
    let mut best = (f32::MAX, f32::MAX, [x, y]);
    for gy in cy - 1..=cy + 1 {
        for gx in cx - 1..=cx + 1 {
            let sx = (gx as f32 + 0.15 + 0.7 * hash_unit(seed, gx, gy)) * cell;
            let sy = (gy as f32 + 0.15 + 0.7 * hash_unit(seed ^ 0xA5A5, gx, gy)) * cell;
            let d = ((sx - x).powi(2) + (sy - y).powi(2)).sqrt();
            if d < best.0 {
                best = (d, best.0, [sx, sy]);
            } else if d < best.1 {
                best.1 = d;
            }
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Artistic
// ---------------------------------------------------------------------------

/// Colored Pencil: diagonal pencil hatching whose coverage follows the
/// darkness of the image (scaled by Stroke Pressure), plus the Sobel edges,
/// laid over paper of the chosen brightness. Covered pixels keep the source
/// colour; uncovered ones show the paper.
fn colored_pencil(_src: &FilterBuffer, img: &Img, v: &Values) -> Img {
    let period = v.get(0) + 1.0;
    let pressure = v.get(1) / 15.0;
    let paper = 0.5 + v.get(2) / 100.0;
    let l = img.luma().blur(0.7);
    img.map(|x, y, c| {
        let hatch = 0.5 + 0.5 * (std::f32::consts::TAU * (x as f32 - y as f32) / period).sin();
        let cover = ((1.0 - l.get(x, y)) * (0.4 + 0.6 * pressure) * hatch + l.edge(x, y) * 2.0)
            .clamp(0.0, 1.0);
        [
            mix(paper, c[0] * 0.9, cover),
            mix(paper, c[1] * 0.9, cover),
            mix(paper, c[2] * 0.9, cover),
        ]
    })
}

/// Cutout: a Gaussian blur whose radius grows with Edge Simplicity and
/// shrinks with Edge Fidelity, then each channel posterized to Number of
/// Levels — flat paper-cut shapes.
fn cutout(src: &FilterBuffer, v: &Values) -> Img {
    let levels = v.get(0);
    let sigma = v.get(1) * 0.6 + (4.0 - v.get(2)) * 0.3;
    let b = Img::from_buffer(&gauss(src, sigma));
    b.map(|_, _, c| {
        [
            posterize(c[0], levels),
            posterize(c[1], levels),
            posterize(c[2], levels),
        ]
    })
}

/// Dry Brush: the oil-paint rank filter (radius Brush Size + 1, luminance
/// bands from Brush Detail) plus a hashed bristle texture scaled by Texture.
fn dry_brush(src: &FilterBuffer, v: &Values, seed: u64) -> Img {
    let oil = Img::from_buffer(&oil_paint(
        src,
        v.get(0) as u32 + 1,
        (v.get(1) as u32 + 2) * 2,
        EdgeMode::Clamp,
    ));
    let amount = v.get(2) * 0.04;
    oil.map(|x, y, c| {
        let n = (noise(seed, x, y / 2) - 0.5) * amount;
        [c[0] + n, c[1] + n, c[2] + n]
    })
}

/// Film Grain: hashed grain (amplitude Grain / 20) weighted toward the
/// shadows, and a highlight bloom over the brightest Highlight Area / 40 of
/// the tonal range at strength Intensity / 10.
fn film_grain(img: &Img, v: &Values, seed: u64) -> Img {
    let amp = v.get(0) / 20.0 * 0.6;
    let area = v.get(1) / 40.0;
    let intensity = v.get(2) / 10.0;
    img.map(|x, y, c| {
        let l = luma(c);
        let n = (noise(seed, x, y) - 0.5) * amp * (1.0 - 0.5 * l);
        let bloom = if area > 0.0 && l > 1.0 - area {
            (l - (1.0 - area)) / area * intensity
        } else {
            0.0
        };
        let f = |ch: f32| mix(ch + n, 1.0, bloom.clamp(0.0, 1.0));
        [f(c[0]), f(c[1]), f(c[2])]
    })
}

/// Fresco: oil-paint dabs, a contrast boost of 1.5 around mid grey, the Sobel
/// edges darkened in, and a hashed plaster texture scaled by Texture.
fn fresco(src: &FilterBuffer, v: &Values, seed: u64) -> Img {
    let oil = Img::from_buffer(&oil_paint(
        src,
        v.get(0) as u32 + 1,
        v.get(1) as u32 + 4,
        EdgeMode::Clamp,
    ));
    let l = oil.luma();
    let amount = v.get(2) * 0.03;
    oil.map(|x, y, c| {
        let dark = 1.0 - (l.edge(x, y) * 0.8).min(0.8);
        let n = (noise(seed, x, y) - 0.5) * amount;
        let f = |ch: f32| ((ch - 0.5) * 1.5 + 0.5) * dark + n;
        [f(c[0]), f(c[1]), f(c[2])]
    })
}

/// A saturated colour at `hue` degrees.
fn hue_rgb(hue: f32) -> [f32; 3] {
    let h = (hue.rem_euclid(360.0)) / 60.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    match h as u32 {
        0 => [1.0, x, 0.0],
        1 => [x, 1.0, 0.0],
        2 => [0.0, 1.0, x],
        3 => [0.0, x, 1.0],
        4 => [x, 0.0, 1.0],
        _ => [1.0, 0.0, x],
    }
}

/// Neon Glow: the image reduced to a dark monochrome (35% of its luminance)
/// with the Sobel edges, blurred by |Glow Size| / 2, added back in the glow
/// hue at Glow Brightness / 15. A negative size inverts the glow, lighting
/// the flat areas instead of the edges.
fn neon_glow(img: &Img, v: &Values) -> Img {
    let size = v.get(0);
    let bright = v.get(1) / 15.0;
    let tint = hue_rgb(v.get(2));
    let l = img.luma();
    let edges = Plane::from_fn(img.w, img.h, |x, y| (l.edge(x, y) * 3.0).min(1.0));
    let glow = edges.blur(size.abs() / 2.0 + 0.5);
    img.map(|x, y, _| {
        let g = glow.get(x, y);
        let g = if size < 0.0 { 1.0 - g } else { g } * bright;
        let base = l.get(x, y) * 0.35;
        [base + tint[0] * g, base + tint[1] * g, base + tint[2] * g]
    })
}

/// Paint Daubs: a median of radius Brush Size / 4 + 1 (flat daubs) sharpened
/// by an unsharp mask of amount Sharpness / 10.
fn paint_daubs(src: &FilterBuffer, v: &Values) -> Img {
    let m = Img::from_buffer(&median(src, v.get(0) as u32 / 4 + 1, EdgeMode::Clamp));
    unsharp(&m, 1.5, v.get(1) / 10.0)
}

/// Palette Knife: broad oil-paint strokes (radius Stroke Size / 5 + 1, bands
/// Stroke Detail * 3 + 2), softened by a Gaussian of Softness * 0.4.
fn palette_knife(src: &FilterBuffer, v: &Values) -> Img {
    let oil = oil_paint(
        src,
        v.get(0) as u32 / 5 + 1,
        v.get(1) as u32 * 3 + 2,
        EdgeMode::Clamp,
    );
    Img::from_buffer(&gauss(&oil, v.get(2) * 0.4))
}

/// Plastic Wrap: specular highlights on the contour ridges of the smoothed
/// luminance — `(0.5 + 0.5 cos(2 pi L Detail / 3))^6` — added toward white
/// at Highlight Strength / 20.
fn plastic_wrap(img: &Img, v: &Values) -> Img {
    let strength = v.get(0) / 20.0;
    let detail = v.get(1);
    let l = img.luma().blur(v.get(2) * 0.5);
    img.map(|x, y, c| {
        let ridge = (0.5 + 0.5 * (std::f32::consts::TAU * l.get(x, y) * detail / 3.0).cos())
            .powi(6)
            * strength;
        [
            mix(c[0], 1.0, ridge),
            mix(c[1], 1.0, ridge),
            mix(c[2], 1.0, ridge),
        ]
    })
}

/// Poster Edges: each channel posterized to Posterization + 2 levels, with
/// black drawn where the Sobel edge of the (Edge Thickness-blurred)
/// luminance, scaled by Edge Intensity + 1, is strong.
fn poster_edges(img: &Img, v: &Values) -> Img {
    let l = img.luma().blur(v.get(0) * 0.3 + 0.5);
    let k = (v.get(1) + 1.0) * 3.0;
    let levels = v.get(2) + 2.0;
    img.map(|x, y, c| {
        let ink = 1.0 - (l.edge(x, y) * k - 0.2).clamp(0.0, 1.0);
        [
            posterize(c[0], levels) * ink,
            posterize(c[1], levels) * ink,
            posterize(c[2], levels) * ink,
        ]
    })
}

/// Rough Pastels: a 45-degree streak of Stroke Length, over a Perlin canvas
/// texture (feature size Scaling% of 4 px) at strength Stroke Detail / 80.
fn rough_pastels(src: &FilterBuffer, v: &Values, seed: u64) -> Img {
    let s = Img::from_buffer(&streak(src, 45.0, v.get(0)));
    let amount = v.get(1) / 80.0;
    let scale = 4.0 * v.get(2) / 100.0;
    let perlin = Perlin::new(seed);
    s.map(|x, y, c| {
        let t = perlin.noise(x as f32 / scale + 0.37, y as f32 / (scale * 0.5) + 0.61) * amount;
        [c[0] + t, c[1] + t, c[2] + t]
    })
}

/// Smudge Stick: a 45-degree streak of Stroke Length * 3, the shadows
/// deepened by 10%, and the same highlight bloom as Film Grain.
fn smudge_stick(src: &FilterBuffer, v: &Values) -> Img {
    let s = Img::from_buffer(&streak(src, 45.0, v.get(0) * 3.0));
    let area = v.get(1) / 40.0;
    let intensity = v.get(2) / 10.0;
    s.map(|_, _, c| {
        let l = luma(c);
        let bloom = if area > 0.0 && l > 1.0 - area {
            ((l - (1.0 - area)) / area * intensity).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let f = |ch: f32| mix(ch - (1.0 - l) * 0.1, 1.0, bloom);
        [f(c[0]), f(c[1]), f(c[2])]
    })
}

/// Sponge: a Gaussian of Brush Size * 0.5, modulated by fractal Perlin
/// blotches (feature size Smoothness + 2 px) at strength Definition / 40.
fn sponge(src: &FilterBuffer, v: &Values, seed: u64) -> Img {
    let b = Img::from_buffer(&gauss(src, v.get(0) * 0.5));
    let depth = v.get(1) / 40.0;
    let scale = v.get(2) + 2.0;
    let perlin = Perlin::new(seed);
    b.map(|x, y, c| {
        let n = perlin.fbm(x as f32 / scale + 0.37, y as f32 / scale + 0.61, 3, 0.5);
        let f = 1.0 + n * depth * 2.0;
        [c[0] * f, c[1] * f, c[2] * f]
    })
}

/// Underpainting: a Gaussian of Brush Size * 0.3, saturation reduced by 20%,
/// over a 4-pixel canvas weave at strength Texture Coverage / 400.
fn underpainting(src: &FilterBuffer, v: &Values) -> Img {
    let b = Img::from_buffer(&gauss(src, v.get(0) * 0.3));
    let amount = v.get(1) / 400.0;
    b.map(|x, y, c| {
        let l = luma(c);
        let weave = if ((x / 2) % 2 == 0) ^ ((y / 2) % 2 == 0) {
            amount
        } else {
            -amount
        };
        let f = |ch: f32| mix(l, ch, 0.8) + weave;
        [f(c[0]), f(c[1]), f(c[2])]
    })
}

/// Watercolor: a median of radius (15 - Brush Detail) / 3 + 1 (washes),
/// saturation raised by 20%, pooled pigment darkening the edges by Shadow
/// Intensity, and a hashed paper texture scaled by Texture.
fn watercolor(src: &FilterBuffer, v: &Values, seed: u64) -> Img {
    let m = Img::from_buffer(&median(
        src,
        (15 - v.get(0) as u32) / 3 + 1,
        EdgeMode::Clamp,
    ));
    let l = m.luma();
    let shadow = 0.3 + v.get(1) * 0.07;
    let tex = v.get(2) * 0.02;
    m.map(|x, y, c| {
        let lum = l.get(x, y);
        let pool = 1.0 - (l.edge(x, y) * 4.0 * shadow).min(0.8);
        let n = (noise(seed, x, y) - 0.5) * tex;
        let f = |ch: f32| (lum + (ch - lum) * 1.2) * pool + n;
        [f(c[0]), f(c[1]), f(c[2])]
    })
}

// ---------------------------------------------------------------------------
// Brush Strokes
// ---------------------------------------------------------------------------

/// Accented Edges: the Sobel edges of the luminance (smoothed by Smoothness
/// times 0.3, widened by Edge Width / 2) pull the colour toward a grey of Edge
/// Brightness / 50 — bright chalky edges at high values, ink at low ones.
fn accented_edges(img: &Img, v: &Values) -> Img {
    let width = v.get(0) / 2.0;
    let target = v.get(1) / 50.0;
    let l = img.luma().blur(v.get(2) * 0.3);
    img.map(|x, y, c| {
        let t = (l.edge(x, y) * width * 4.0).clamp(0.0, 1.0);
        [
            mix(c[0], target, t),
            mix(c[1], target, t),
            mix(c[2], target, t),
        ]
    })
}

/// Angled Strokes: 45-degree strokes in the dark tones and 135-degree ones in
/// the light tones (the split at Direction Balance %), each of Stroke Length,
/// then an unsharp mask of Sharpness / 5.
fn angled_strokes(src: &FilterBuffer, img: &Img, v: &Values) -> Img {
    let a = Img::from_buffer(&streak(src, 45.0, v.get(1)));
    let b = Img::from_buffer(&streak(src, 135.0, v.get(1)));
    let split = v.get(0) / 100.0;
    let l = img.luma();
    let mixed = img.map(|x, y, _| {
        let c = if l.get(x, y) < split {
            a.get(x, y)
        } else {
            b.get(x, y)
        };
        [c[0], c[1], c[2]]
    });
    unsharp(&mixed, 1.0, v.get(2) / 5.0)
}

/// Crosshatch: the mean of 45- and 135-degree strokes of Stroke Length,
/// pencil hatching on both diagonals darkening the shadows by Strength / 5,
/// and an unsharp mask of Sharpness / 10.
fn crosshatch(src: &FilterBuffer, img: &Img, v: &Values) -> Img {
    let a = Img::from_buffer(&streak(src, 45.0, v.get(0)));
    let b = Img::from_buffer(&streak(src, 135.0, v.get(0)));
    let strength = v.get(2) / 5.0;
    let l = img.luma();
    let hatched = img.map(|x, y, _| {
        let (p, q) = (a.get(x, y), b.get(x, y));
        let on = (x + y) % 4 == 0 || (x + 4 - y % 4) % 4 == 0;
        let dark = if on {
            (1.0 - l.get(x, y)) * strength
        } else {
            0.0
        };
        let f = |i: usize| (p[i] + q[i]) * 0.5 * (1.0 - dark);
        [f(0), f(1), f(2)]
    });
    unsharp(&hatched, 1.0, v.get(1) / 10.0)
}

/// Dark Strokes: short 45-degree strokes; below the Balance / 10 luminance
/// split the colour is darkened by Black Intensity / 12.5, above it lifted
/// toward white by White Intensity / 20.
fn dark_strokes(src: &FilterBuffer, img: &Img, v: &Values) -> Img {
    let s = Img::from_buffer(&streak(src, 45.0, 6.0));
    let split = v.get(0) / 10.0;
    let black = v.get(1) / 12.5;
    let white = v.get(2) / 20.0;
    let l = img.luma();
    img.map(|x, y, _| {
        let c = s.get(x, y);
        let f = |ch: f32| {
            if l.get(x, y) < split {
                ch * (1.0 - black)
            } else {
                mix(ch, 1.0, white)
            }
        };
        [f(c[0]), f(c[1]), f(c[2])]
    })
}

/// Ink Outlines: 135-degree strokes of Stroke Length, the darks deepened by
/// Dark Intensity / 50 and the lights lifted by Light Intensity / 50, and
/// black ink wherever the Sobel edge is strong.
fn ink_outlines(src: &FilterBuffer, img: &Img, v: &Values) -> Img {
    let s = Img::from_buffer(&streak(src, 135.0, v.get(0)));
    let dark = v.get(1) / 50.0;
    let light = v.get(2) / 50.0;
    let l = img.luma();
    img.map(|x, y, _| {
        let c = s.get(x, y);
        let lum = l.get(x, y);
        let ink = 1.0 - (l.edge(x, y) * 4.0 - 0.1).clamp(0.0, 1.0);
        let f = |ch: f32| mix(ch * (1.0 - dark * (1.0 - lum)), 1.0, light * lum) * ink;
        [f(c[0]), f(c[1]), f(c[2])]
    })
}

/// Spatter: every pixel reads the source at an offset of up to Spray Radius
/// pixels, from a Perlin field of feature size Smoothness plus a per-pixel
/// hashed jitter — a sprayed, airbrushed edge.
fn spatter(img: &Img, radius: f32, smoothness: f32, seed: u64) -> Img {
    if radius <= 0.0 {
        return img.clone();
    }
    let px = Perlin::new(seed);
    let py = Perlin::new(seed ^ 0xDEAD_BEEF);
    let jitter = radius / (smoothness + 1.0);
    let px_out = build(img.w, img.h, |x, y| {
        let (fx, fy) = (x as f32 / smoothness + 0.37, y as f32 / smoothness + 0.61);
        let dx = px.noise(fx, fy) * radius + (noise(seed, x, y) - 0.5) * jitter;
        let dy = py.noise(fx, fy) * radius + (noise(seed ^ 1, x, y) - 0.5) * jitter;
        let s = img.at(
            (x as f32 + dx).round() as i64,
            (y as f32 + dy).round() as i64,
        );
        [s[0], s[1], s[2], img.get(x, y)[3]]
    });
    Img {
        w: img.w,
        h: img.h,
        px: px_out,
    }
}

/// Sprayed Strokes: a stroke of Stroke Length in the Stroke Direction (right
/// diagonal, horizontal, left diagonal, vertical), then Spatter of Spray
/// Radius with smoothness 3.
fn sprayed_strokes(src: &FilterBuffer, v: &Values, seed: u64) -> Img {
    let angle = [45.0, 0.0, 135.0, 90.0][v.get(2) as usize % 4];
    let s = Img::from_buffer(&streak(src, angle, v.get(0)));
    spatter(&s, v.get(1), 3.0, seed)
}

/// Sumi-e: a Gaussian of Stroke Width * 0.2 multiplied by an ink curve —
/// the luminance contrast-stretched by 1 + Contrast / 8 and darkened by
/// Stroke Pressure / 30 — so the darks go to rich black ink.
fn sumi_e(src: &FilterBuffer, v: &Values) -> Img {
    let b = Img::from_buffer(&gauss(src, v.get(0) * 0.2));
    let k = 1.0 + v.get(2) / 8.0;
    let pressure = v.get(1) / 30.0;
    b.map(|_, _, c| {
        let t = (0.5 + (luma(c) - 0.5) * k - pressure).clamp(0.0, 1.0);
        [c[0] * t, c[1] * t, c[2] * t]
    })
}

// ---------------------------------------------------------------------------
// Sketch — ink (black) on paper (white)
// ---------------------------------------------------------------------------

/// Bas Relief: the luminance, smoothed by Smoothness * 0.4, lit from the
/// Light direction (eight 45-degree steps) — relief scaled by Detail —
/// rendered as ink/paper grey.
fn bas_relief(img: &Img, v: &Values) -> Img {
    let detail = v.get(0) / 3.0;
    let l = img.luma().blur(v.get(1) * 0.4);
    let light = light_dir(v.get(2));
    img.grey(|x, y, _| 0.5 + l.relief(x, y, light, 1.0) * detail + (l.get(x, y) - 0.5) * 0.3)
}

/// Chalk & Charcoal: the darks drawn in charcoal streaks on the 45-degree
/// diagonal, the lights in chalk streaks on the 135-degree diagonal, the
/// midtones mid grey. Charcoal Area and Chalk Area widen each band; Stroke
/// Pressure darkens the charcoal.
fn chalk_charcoal(img: &Img, v: &Values, seed: u64) -> Img {
    let dark = 0.25 + v.get(0) / 40.0;
    let light = 0.75 - v.get(1) / 40.0;
    let pressure = v.get(2) * 0.1;
    img.grey(|x, y, c| {
        let l = luma(c);
        let n1 = hash_unit(seed, i64::from(x) + i64::from(y), 0);
        let n2 = hash_unit(seed ^ 7, i64::from(x) - i64::from(y), 0);
        if l < dark {
            l * (0.5 + 0.5 * n1) * (1.0 - pressure)
        } else if l > light {
            mix(l, 1.0, n2)
        } else {
            0.5
        }
    })
}

/// Charcoal: 45-degree strokes of 2 * Charcoal Thickness + 1, thresholded
/// softly at Light/Dark Balance %, with the Sobel edges (scaled by Detail /
/// 5) drawn in as dark lines.
fn charcoal(src: &FilterBuffer, img: &Img, v: &Values) -> Img {
    let s = Img::from_buffer(&streak(src, 45.0, v.get(0) * 2.0 + 1.0)).luma();
    let detail = v.get(1) / 5.0;
    let t = v.get(2) / 100.0;
    let l = img.luma();
    img.grey(|x, y, _| {
        smoothstep(t - 0.15, t + 0.15, s.get(x, y)) * (1.0 - (l.edge(x, y) * detail * 3.0).min(1.0))
    })
}

/// Chrome: the luminance, smoothed by Smoothness * 0.5 + 0.5, folded through
/// `0.5 + 0.5 sin(pi L (2 + Detail))` — polished metal banding.
fn chrome(img: &Img, v: &Values) -> Img {
    let bands = 2.0 + v.get(0);
    let l = img.luma().blur(v.get(1) * 0.5 + 0.5);
    img.grey(|x, y, _| 0.5 + 0.5 * (std::f32::consts::PI * l.get(x, y) * bands).sin())
}

/// Conte Crayon: sepia crayon on cream paper; the crayon's coverage is the
/// darkness weighted by Foreground Level / 15 minus the lightness weighted by
/// Background Level / 15, roughened by a Perlin grain of Scaling%.
fn conte_crayon(img: &Img, v: &Values, seed: u64) -> Img {
    const CRAYON: [f32; 3] = [0.35, 0.12, 0.06];
    const PAPER: [f32; 3] = [0.95, 0.92, 0.85];
    let fg = v.get(0) / 15.0;
    let bg = v.get(1) / 15.0;
    let scale = 3.0 * v.get(2) / 100.0;
    let perlin = Perlin::new(seed);
    img.map(|x, y, c| {
        let l = luma(c);
        let tex = perlin.noise(x as f32 / scale + 0.37, y as f32 / scale + 0.61);
        let cover = ((1.0 - l) * fg - l * bg * 0.3 + tex * 0.1 + 0.2).clamp(0.0, 1.0);
        [
            mix(PAPER[0], CRAYON[0], cover),
            mix(PAPER[1], CRAYON[1], cover),
            mix(PAPER[2], CRAYON[2], cover),
        ]
    })
}

/// Graphic Pen: parallel pen strokes in the Stroke Direction; each stroke
/// segment (2 * Stroke Length long) carries one hashed offset, and a pixel
/// is ink where the luminance plus that offset is below Light/Dark Balance %.
fn graphic_pen(img: &Img, v: &Values, seed: u64) -> Img {
    let len = v.get(0) * 2.0;
    let t = v.get(1) / 100.0;
    let a = [45.0f32, 0.0, 135.0, 90.0][v.get(2) as usize % 4].to_radians();
    let (ux, uy) = (a.cos(), -a.sin());
    img.grey(|x, y, c| {
        let (fx, fy) = (x as f32, y as f32);
        let along = fx * ux + fy * uy;
        let across = -fx * uy + fy * ux;
        let n = hash_unit(seed, across.round() as i64, (along / len).floor() as i64);
        if luma(c) + (n - 0.5) * 0.6 < t {
            0.0
        } else {
            1.0
        }
    })
}

/// Halftone Pattern: cells of 2 * Size + 2 pixels whose contrast-stretched
/// (1 + Contrast / 10) luminance, read at the cell centre, sets a black dot's
/// radius (Pattern 0), a concentric ring's width (1) or a line's thickness
/// (2).
fn halftone_pattern(img: &Img, v: &Values) -> Img {
    let cell = v.get(0) * 2.0 + 2.0;
    let k = 1.0 + v.get(1) / 10.0;
    let pattern = v.get(2) as u32;
    let l = img.luma().blur(cell * 0.3);
    let (cx0, cy0) = (img.w as f32 / 2.0, img.h as f32 / 2.0);
    img.grey(|x, y, _| {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        let (gx, gy) = ((fx / cell).floor(), (fy / cell).floor());
        let (mx, my) = ((gx + 0.5) * cell, (gy + 0.5) * cell);
        let lum = l.at(mx as i64, my as i64);
        let ink = (1.0 - (0.5 + (lum - 0.5) * k)).clamp(0.0, 1.0);
        let inked = match pattern {
            0 => ((fx - mx).powi(2) + (fy - my).powi(2)).sqrt() < ink.sqrt() * cell * 0.7,
            1 => {
                let d = ((fx - cx0).powi(2) + (fy - cy0).powi(2)).sqrt() / cell;
                d.fract() < ink
            }
            _ => (fy / cell).fract() < ink,
        };
        if inked {
            0.0
        } else {
            1.0
        }
    })
}

/// Note Paper: two paper tones split at Image Balance / 50, embossed along
/// their boundary by Relief / 25 and speckled with Graininess / 200 of
/// hashed grain.
fn note_paper(img: &Img, v: &Values, seed: u64) -> Img {
    let t = v.get(0) / 50.0;
    let grain = v.get(1) / 200.0;
    let relief = v.get(2) / 25.0 * 0.3;
    let l = img.luma();
    let mask = Plane::from_fn(img.w, img.h, |x, y| if l.get(x, y) < t { 1.0 } else { 0.0 });
    img.grey(|x, y, _| {
        let tone = if mask.get(x, y) > 0.5 { 0.55 } else { 0.95 };
        tone + mask.relief(x, y, (-0.7, -0.7), 1.0) * relief + (noise(seed, x, y) - 0.5) * grain
    })
}

/// Photocopy: ink where the luminance falls below its own Gaussian (sigma
/// Detail * 0.5 + 0.5) — a high-pass — by more than the paper can hold, the
/// ink density scaled by Darkness.
fn photocopy(img: &Img, v: &Values) -> Img {
    let l = img.luma();
    let b = l.blur(v.get(0) * 0.5 + 0.5);
    let dark = v.get(1) * 2.0;
    img.grey(|x, y, _| 1.0 - ((b.get(x, y) - l.get(x, y)) * dark).clamp(0.0, 1.0))
}

/// Plaster: the darks (below Image Balance / 50, after a Gaussian of
/// Smoothness * 0.5) raised into a plaster relief lit from the Light
/// direction.
fn plaster(img: &Img, v: &Values) -> Img {
    let t = v.get(0) / 50.0;
    let lb = img.luma().blur(v.get(1) * 0.5);
    let light = light_dir(v.get(2));
    let mask = Plane::from_fn(img.w, img.h, |x, y| {
        1.0 - smoothstep(t - 0.05, t + 0.05, lb.get(x, y))
    });
    img.grey(|x, y, _| 0.6 + mask.relief(x, y, light, 1.5) * 1.5 + mask.get(x, y) * 0.1)
}

/// Reticulation: film-emulsion clumps — the luminance plus hashed noise
/// (amplitude Density / 50) thresholded at mid grey; clumped pixels take the
/// Foreground Level ink density, the rest the Background Level density.
fn reticulation(img: &Img, v: &Values, seed: u64) -> Img {
    let density = v.get(0) / 50.0;
    let fg = v.get(1) / 50.0;
    let bg = v.get(2) / 50.0;
    let n = Plane::from_fn(img.w, img.h, |x, y| noise(seed, x, y)).blur(0.7);
    img.grey(|x, y, c| {
        if luma(c) + (n.get(x, y) - 0.5) * density * 2.0 < 0.5 {
            1.0 - fg
        } else {
            1.0 - bg * 0.5
        }
    })
}

/// Stamp: the luminance, smoothed by Smoothness * 0.3, thresholded at
/// Light/Dark Balance / 50 into solid ink or paper.
fn stamp(img: &Img, v: &Values) -> Img {
    let t = v.get(0) / 50.0;
    let lb = img.luma().blur(v.get(1) * 0.3);
    img.grey(|x, y, _| if lb.get(x, y) < t { 0.0 } else { 1.0 })
}

/// Torn Edges: ink/paper thresholded at Image Balance / 50, the boundary
/// torn by a Perlin field (rougher at low Smoothness) and softened over a
/// band of 0.5 / Contrast.
fn torn_edges(img: &Img, v: &Values, seed: u64) -> Img {
    let t = v.get(0) / 50.0;
    let rough = (16.0 - v.get(1)) / 15.0 * 0.2;
    let band = 0.5 / v.get(2);
    let perlin = Perlin::new(seed);
    img.grey(|x, y, c| {
        let n = perlin.noise(x as f32 / 2.0 + 0.37, y as f32 / 2.0 + 0.61);
        smoothstep(t - band, t + band, luma(c) + n * rough)
    })
}

/// Water Paper: a light Gaussian, brightness shifted by (Brightness - 50) /
/// 100 and contrast scaled by Contrast / 80, printed on paper fibres —
/// hashed vertical streaks Fiber Length pixels long.
fn water_paper(src: &FilterBuffer, v: &Values, seed: u64) -> Img {
    let b = Img::from_buffer(&gauss(src, 1.0));
    let fiber = v.get(0);
    let bright = (v.get(1) - 50.0) / 100.0;
    let contrast = v.get(2) / 80.0;
    b.map(|x, y, c| {
        let offset = noise(seed, x, 0) * fiber;
        let n = hash_unit(seed ^ 3, i64::from(x), ((y as f32 + offset) / fiber) as i64);
        let f = |ch: f32| ((ch - 0.5) * contrast + 0.5 + bright) * (0.9 + 0.1 * n);
        [f(c[0]), f(c[1]), f(c[2])]
    })
}

// ---------------------------------------------------------------------------
// Texture
// ---------------------------------------------------------------------------

/// Craquelure: a Voronoi crack network with cells Crack Spacing wide; the
/// cracks (where the two nearest sites are within 1.5 px of equidistant)
/// darken by Crack Depth / 10, and the whole surface dims to 70% + 30% of
/// Crack Brightness / 10.
fn craquelure(img: &Img, v: &Values, seed: u64) -> Img {
    let spacing = v.get(0);
    let depth = v.get(1) / 10.0;
    let bright = 0.7 + 0.3 * v.get(2) / 10.0;
    img.map(|x, y, c| {
        let (f1, f2, _) = voronoi(x as f32 + 0.5, y as f32 + 0.5, spacing, seed);
        let crack = 1.0 - smoothstep(0.0, 1.5, f2 - f1);
        let k = bright * (1.0 - crack * depth);
        [c[0] * k, c[1] * k, c[2] * k]
    })
}

/// Grain: contrast scaled by 0.5 + Contrast / 100 around mid grey, plus
/// grain of amplitude Intensity / 250 — Regular (per-pixel hash), Soft
/// (fine Perlin), Speckle (sparse hashed specks) or Clumped (coarse Perlin),
/// by Grain Type.
fn grain(img: &Img, v: &Values, seed: u64) -> Img {
    let amp = v.get(0) / 250.0;
    let contrast = 0.5 + v.get(1) / 100.0;
    let kind = v.get(2) as u32;
    let perlin = Perlin::new(seed);
    img.map(|x, y, c| {
        let (fx, fy) = (x as f32, y as f32);
        let n = match kind {
            0 => (noise(seed, x, y) - 0.5) * 2.0,
            1 => perlin.noise(fx / 1.5 + 0.37, fy / 1.5 + 0.61),
            2 => {
                let h = noise(seed, x, y);
                if h < 0.05 {
                    -3.0
                } else if h > 0.95 {
                    3.0
                } else {
                    0.0
                }
            }
            _ => perlin.noise(fx / 4.0 + 0.37, fy / 4.0 + 0.61) * 1.5,
        } * amp;
        let f = |ch: f32| 0.5 + (ch - 0.5) * contrast + n;
        [f(c[0]), f(c[1]), f(c[2])]
    })
}

/// Mosaic Tiles: square tiles Tile Size wide, each the colour at its centre
/// with a slight bevel, separated by grout Grout Width wide whose grey is
/// 20% + 60% of Lighten Grout / 10.
fn mosaic_tiles(img: &Img, v: &Values) -> Img {
    let size = v.get(0) as u32;
    let grout = (v.get(1) as u32).min(size.saturating_sub(1));
    let grout_grey = 0.2 + 0.6 * v.get(2) / 10.0;
    img.map(|x, y, _| {
        let (lx, ly) = (x % size, y % size);
        if lx < grout || ly < grout {
            return [grout_grey; 3];
        }
        let c = img.at(i64::from(x - lx + size / 2), i64::from(y - ly + size / 2));
        let bevel = if lx == grout || ly == grout {
            1.15
        } else if lx == size - 1 || ly == size - 1 {
            0.85
        } else {
            1.0
        };
        [c[0] * bevel, c[1] * bevel, c[2] * bevel]
    })
}

/// Patchwork: squares of Square Size + 2 pixels, each filled with the
/// colour at its centre and given a hashed height, whose top/left edges are
/// lit and bottom/right edges shaded by Relief / 25.
fn patchwork(img: &Img, v: &Values, seed: u64) -> Img {
    let size = v.get(0) as u32 + 2;
    let relief = v.get(1) / 25.0 * 0.4;
    img.map(|x, y, _| {
        let (gx, gy) = (x / size, y / size);
        let c = img.at(
            i64::from(gx * size + size / 2),
            i64::from(gy * size + size / 2),
        );
        let height = 0.5 + 0.5 * hash_unit(seed, i64::from(gx), i64::from(gy));
        let (lx, ly) = (x % size, y % size);
        let shade = if lx == 0 || ly == 0 {
            1.0 + relief * height
        } else if lx == size - 1 || ly == size - 1 {
            1.0 - relief * height
        } else {
            1.0
        };
        [c[0] * shade, c[1] * shade, c[2] * shade]
    })
}

/// Stained Glass: Voronoi cells of Cell Size, each filled with the colour
/// at its site and lit toward its centre by Light Intensity / 20, separated
/// by black leading where the two nearest sites are within Border Thickness
/// / 2 of equidistant.
fn stained_glass(img: &Img, v: &Values, seed: u64) -> Img {
    let cell = v.get(0);
    let border = v.get(1) * 0.5;
    let light = v.get(2) / 20.0;
    img.map(|x, y, _| {
        let (f1, f2, site) = voronoi(x as f32 + 0.5, y as f32 + 0.5, cell, seed);
        if f2 - f1 < border {
            return [0.0; 3];
        }
        let c = img.at(site[0] as i64, site[1] as i64);
        let k = 1.0 + light * (1.0 - (f1 / cell).min(1.0));
        [c[0] * k, c[1] * k, c[2] * k]
    })
}

/// Texturizer: a procedural height field — Brick, Burlap, Canvas or
/// Sandstone by Texture, at Scaling% — lit from the Light direction; the
/// relief (Relief / 10) scales the colour up to +/-50%.
fn texturizer(img: &Img, v: &Values, seed: u64) -> Img {
    let s = v.get(0) / 100.0;
    let relief = v.get(1) / 10.0;
    let kind = v.get(2) as u32;
    let light = light_dir(v.get(3));
    let perlin = Perlin::new(seed);
    let height = Plane::from_fn(img.w, img.h, |x, y| {
        let (fx, fy) = (x as f32 / s, y as f32 / s);
        match kind {
            0 => {
                let row = (fy / 8.0).floor();
                let shift = if row as i64 % 2 == 0 { 0.0 } else { 8.0 };
                let bx = (fx + shift).rem_euclid(16.0);
                let by = fy.rem_euclid(8.0);
                if bx < 1.0 || by < 1.0 {
                    0.0
                } else {
                    1.0
                }
            }
            1 => {
                let over = ((fx / 3.0).floor() as i64 + (fy / 3.0).floor() as i64) % 2 == 0;
                let t = if over { fx } else { fy };
                0.5 + 0.5 * (std::f32::consts::TAU * t / 3.0).sin()
            }
            2 => {
                0.5 + 0.25
                    * ((std::f32::consts::TAU * fx / 4.0).sin()
                        + (std::f32::consts::TAU * fy / 4.0).sin())
            }
            _ => 0.5 + 0.5 * perlin.fbm(fx / 6.0 + 0.37, fy / 6.0 + 0.61, 4, 0.5),
        }
    });
    img.map(|x, y, c| {
        let k = 1.0 + (height.relief(x, y, light, 1.0) * relief * 0.5).clamp(-0.5, 0.5);
        [c[0] * k, c[1] * k, c[2] * k]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A colour gradient with real structure: a diagonal ramp, a disc and a
    /// hue sweep, so every effect has edges, tones and colour to act on.
    fn gradient(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let fx = x as f32 / (w - 1) as f32;
                let fy = y as f32 / (h - 1) as f32;
                let disc = ((fx - 0.5).powi(2) + (fy - 0.5).powi(2)).sqrt() < 0.25;
                let base = [fx, fy, 1.0 - fx * fy];
                let c = if disc {
                    [base[2], base[0], base[1]]
                } else {
                    base
                };
                px.push([c[0], c[1], c[2], 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    #[test]
    fn there_are_forty_three_effects_in_four_photoshop_sets() {
        let count = |s: GallerySet| s.effects().count();
        assert_eq!(count(GallerySet::Artistic), 15);
        assert_eq!(count(GallerySet::BrushStrokes), 8);
        assert_eq!(count(GallerySet::Sketch), 14);
        assert_eq!(count(GallerySet::Texture), 6);
        let mut names: Vec<_> = GalleryEffect::ALL.iter().map(|e| e.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 43, "names are unique");
        for e in GalleryEffect::ALL {
            assert!(!e.params().is_empty(), "{e:?} has parameters");
            for p in e.params() {
                assert!(p.min <= p.default && p.default <= p.max, "{e:?} {}", p.key);
            }
        }
    }

    #[test]
    fn every_effect_changes_a_test_gradient_and_is_deterministic() {
        let src = gradient(48, 40);
        for e in GalleryEffect::ALL {
            let a = e.apply(&src, &e.defaults());
            let b = e.apply(&src, &e.defaults());
            assert_eq!(a.dimensions(), src.dimensions(), "{e:?} keeps the size");
            assert_ne!(a.pixels(), src.pixels(), "{e:?} changed nothing");
            assert_eq!(a.pixels(), b.pixels(), "{e:?} is not deterministic");
            // Alpha is handed through.
            assert!(
                a.pixels().iter().all(|p| (p[3] - 1.0).abs() < 1e-6),
                "{e:?}"
            );
        }
    }

    #[test]
    fn a_parameter_moves_the_result() {
        let src = gradient(48, 40);
        for e in GalleryEffect::ALL {
            let base = e.apply(&src, &e.defaults());
            let moved = e.params().iter().enumerate().any(|(i, p)| {
                let mut v = e.defaults();
                v[i] = if p.default == p.max { p.min } else { p.max };
                e.apply(&src, &v).pixels() != base.pixels()
            });
            assert!(moved, "{e:?}: no parameter changes the result");
        }
    }

    #[test]
    fn degenerate_sizes_and_values_do_not_panic() {
        let one = FilterBuffer::filled(1, 1, [0.2, 0.4, 0.6, 1.0]).unwrap();
        let empty = FilterBuffer::transparent(0, 0).unwrap();
        let wild = [f32::NAN, f32::INFINITY, -1e9, 1e9];
        for e in GalleryEffect::ALL {
            let _ = e.apply(&one, &e.defaults());
            let _ = e.apply(&one, &wild);
            let _ = e.apply(&one, &[]);
            assert!(e.apply(&empty, &e.defaults()).is_empty());
        }
    }

    #[test]
    fn the_stack_applies_visible_layers_in_order() {
        let src = gradient(32, 32);
        let a = GalleryLayer::new(GalleryEffect::Cutout);
        let b = GalleryLayer::new(GalleryEffect::Craquelure);
        let stack = GalleryStack {
            layers: vec![a.clone(), b.clone()],
        };
        let by_hand = b.effect.apply(&a.effect.apply(&src, &a.values), &b.values);
        assert_eq!(stack.apply(&src), by_hand);
        let reversed = GalleryStack {
            layers: vec![b.clone(), a.clone()],
        };
        assert_ne!(reversed.apply(&src), by_hand, "order matters");
        let mut hidden = stack.clone();
        hidden.layers[1].visible = false;
        assert_eq!(hidden.apply(&src), a.effect.apply(&src, &a.values));
        hidden.layers[0].visible = false;
        assert!(hidden.is_identity());
        assert_eq!(hidden.apply(&src), src);
        assert!(GalleryStack::default().is_identity());
    }
}
