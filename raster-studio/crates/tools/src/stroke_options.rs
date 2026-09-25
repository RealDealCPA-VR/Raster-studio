//! W13-H: the options-bar controls Photopea gives the Eraser, the Colour
//! Replacement tool, the Background Eraser and Sharpen, and the rules the
//! stroke engine ([`crate::stroke`]) follows for them.
//!
//! * Eraser **Mode** — Brush (the brush as set), Pencil (the brush's size,
//!   hard and aliased: every pixel is either erased or not) and Block (a
//!   fixed [`BLOCK_SIZE`]-pixel square at full strength, whatever the brush,
//!   opacity or flow).
//! * Colour Replacement **Mode** — Hue, Saturation, Colour or Luminosity:
//!   which of the foreground's components replace the pixel's.
//! * **Sampling** (Colour Replacement, Background Eraser) — Continuous (each
//!   dab measures against the colour under its own centre), Once (the colour
//!   under the press) or Background Swatch (the background colour).
//! * **Limits** (Colour Replacement, Background Eraser) — Discontiguous
//!   (every matching pixel under the brush), Contiguous (only the matching
//!   pixels connected to a dab centre through matching pixels under the
//!   brush) or Find Edges (Contiguous, but the flood does not step past a
//!   pixel that sits on a sharp luminance edge, so a thin line of a
//!   near-matching colour still holds it back).
//! * **Anti-alias** (Colour Replacement) — on, a pixel is replaced in
//!   proportion to how close it is to the sampled colour; off, every pixel
//!   within the tolerance is replaced fully.
//! * **Protect Foreground Colour** (Background Eraser) — a pixel within the
//!   tolerance of the foreground colour is never erased.
//! * **Protect Detail** (Sharpen) — each sharpened pixel may leave the range
//!   of its 3×3 neighbourhood before the sharpen by at most
//!   [`PROTECT_DETAIL_MARGIN`] of that range, so the stroke still crisps an
//!   edge but without the bright / dark halo and noise blow-up a plain
//!   unsharp mask leaves.

use std::collections::VecDeque;
use std::sync::OnceLock;

use filters::FilterBuffer;
use glam::IVec2;

use crate::brush::{BrushDynamics, BrushSettings, BrushTip, Dab, SampledTip, TipId};
use crate::error::ToolError;
use crate::patch::ColorPatch;
use crate::stroke::{encoded_luma, similarity, StrokeOp, StrokeTool};
use crate::symmetry::{
    SymmetryMode, MAX_SEGMENTS, MIN_SEGMENTS, SYMMETRY_KEY, SYMMETRY_SEGMENTS_KEY,
};
use crate::tool::ToolSetting;

/// The Eraser's Mode drop-down (a Choice indexing [`EraserMode::CHOICES`]).
pub const ERASER_MODE_KEY: &str = "eraser_mode";
/// Colour Replacement's Mode drop-down ([`ReplaceMode::CHOICES`]).
pub const REPLACE_MODE_KEY: &str = "replace_mode";
/// Sampling ([`Sampling::CHOICES`]), Colour Replacement and Background Eraser.
pub const SAMPLING_KEY: &str = "sampling";
/// Limits ([`Limits::CHOICES`]), Colour Replacement and Background Eraser.
pub const LIMITS_KEY: &str = "limits";
/// Colour Replacement's Anti-alias checkbox.
pub const ANTIALIAS_KEY: &str = "antialias";
/// The Background Eraser's Protect Foreground Colour checkbox.
pub const PROTECT_FOREGROUND_KEY: &str = "protect_foreground";
/// Sharpen's Protect Detail checkbox.
pub const PROTECT_DETAIL_KEY: &str = "protect_detail";

/// The side of the Eraser's Block, in pixels of the layer being erased.
pub const BLOCK_SIZE: f32 = 16.0;

/// How far apart (on the display curve) two neighbouring pixels' luminance
/// must be for Find Edges to treat the pair as an edge the flood stops at.
pub const EDGE_STEP: f32 = 0.1;

/// The Eraser's Mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EraserMode {
    #[default]
    Brush,
    Pencil,
    Block,
}

impl EraserMode {
    pub const CHOICES: &'static [&'static str] = &["Brush", "Pencil", "Block"];

    pub fn from_choice(index: usize) -> Self {
        match index {
            0 => EraserMode::Brush,
            1 => EraserMode::Pencil,
            _ => EraserMode::Block,
        }
    }

    /// The brush a press in this mode stamps with, from the brush the
    /// options bar holds.
    pub fn press_settings(self, brush: BrushSettings) -> BrushSettings {
        match self {
            EraserMode::Brush => brush,
            EraserMode::Pencil => BrushSettings {
                hardness: 1.0,
                aliased: true,
                ..brush
            },
            EraserMode::Block => BrushSettings {
                size: BLOCK_SIZE,
                hardness: 1.0,
                spacing: 0.25,
                angle: 0.0,
                roundness: 1.0,
                opacity: 1.0,
                flow: 1.0,
                size_pressure: false,
                flow_pressure: false,
                opacity_pressure: false,
                aliased: true,
                tip: BrushTip::Sampled(block_tip()),
                dynamics: BrushDynamics::default(),
                ..brush
            },
        }
    }
}

/// The Block's tip: a solid square, registered once per process under a
/// fixed id (not a content hash, so it can never collide with an imported
/// tip's).
fn block_tip() -> TipId {
    static REGISTERED: OnceLock<TipId> = OnceLock::new();
    *REGISTERED.get_or_init(|| {
        let id = TipId(*b"raster-studio/eraser-block-tip/1");
        let side = BLOCK_SIZE as u32;
        let tip = SampledTip::new(side, side, vec![255; (side * side) as usize])
            .expect("a 16x16 square is a valid tip");
        crate::brush::register_sampled_tip(id, tip);
        id
    })
}

/// Colour Replacement's Mode: which components of the foreground colour
/// replace the pixel's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplaceMode {
    Hue,
    Saturation,
    #[default]
    Color,
    Luminosity,
}

impl ReplaceMode {
    pub const CHOICES: &'static [&'static str] = &["Hue", "Saturation", "Color", "Luminosity"];

    pub fn from_choice(index: usize) -> Self {
        match index {
            0 => ReplaceMode::Hue,
            1 => ReplaceMode::Saturation,
            2 => ReplaceMode::Color,
            _ => ReplaceMode::Luminosity,
        }
    }
}

/// Where a tolerance-driven stroke takes the colour it measures against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sampling {
    Continuous,
    #[default]
    Once,
    BackgroundSwatch,
}

impl Sampling {
    pub const CHOICES: &'static [&'static str] = &["Continuous", "Once", "Background Swatch"];

    pub fn from_choice(index: usize) -> Self {
        match index {
            0 => Sampling::Continuous,
            1 => Sampling::Once,
            _ => Sampling::BackgroundSwatch,
        }
    }
}

/// Which matching pixels under the brush a tolerance-driven stroke changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Limits {
    #[default]
    Discontiguous,
    Contiguous,
    FindEdges,
}

impl Limits {
    pub const CHOICES: &'static [&'static str] = &["Discontiguous", "Contiguous", "Find Edges"];

    pub fn from_choice(index: usize) -> Self {
        match index {
            0 => Limits::Discontiguous,
            1 => Limits::Contiguous,
            _ => Limits::FindEdges,
        }
    }
}

/// The W13-H retouching options a [`StrokeTool`] holds.
///
/// [`Default`] is the engine's behaviour before these controls existed
/// (Colour mode, sampled Once, Discontiguous, anti-aliased, nothing
/// protected), which is what a directly-built [`StrokeTool`] keeps;
/// [`RetouchOptions::photopea`] is what the registry's tools start with and
/// what the options bar shows by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetouchOptions {
    pub replace_mode: ReplaceMode,
    pub sampling: Sampling,
    pub limits: Limits,
    pub antialias: bool,
    pub protect_foreground: bool,
    pub protect_detail: bool,
}

impl Default for RetouchOptions {
    fn default() -> Self {
        Self {
            replace_mode: ReplaceMode::Color,
            sampling: Sampling::Once,
            limits: Limits::Discontiguous,
            antialias: true,
            protect_foreground: false,
            protect_detail: false,
        }
    }
}

impl RetouchOptions {
    /// Photoshop / Photopea's defaults: Colour, Continuous, Contiguous,
    /// anti-aliased, foreground unprotected, Protect Detail on.
    pub fn photopea() -> Self {
        Self {
            sampling: Sampling::Continuous,
            limits: Limits::Contiguous,
            protect_detail: true,
            ..Self::default()
        }
    }

    /// `true` when the answer at a pixel depends on pixels (or dabs) away
    /// from it, so a tile cannot be previewed on its own.
    pub fn is_regional(&self) -> bool {
        self.sampling == Sampling::Continuous || self.limits != Limits::Discontiguous
    }
}

/// The per-pixel gate of a tolerance-driven op (Colour Replacement, the
/// Background Eraser), aligned to `patch`: how much of the stroke each
/// pixel takes, `0..=1`, before the stroke's own coverage.
///
/// `base` is the colour sampled at the press (Once) or the background
/// swatch; `protect` the premultiplied foreground when Protect Foreground
/// is on; `dabs` the dabs being rendered, whose centres Continuous samples
/// and Contiguous / Find Edges flood from; `covered` the stroke's coverage
/// aligned to `patch` — the flood never leaves it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn region_gate(
    patch: &ColorPatch,
    covered: &[f32],
    base: [f32; 4],
    tolerance: f32,
    opts: &RetouchOptions,
    antialias: bool,
    protect: Option<[f32; 4]>,
    dabs: &[Dab],
) -> Vec<f32> {
    let px = patch.buffer().pixels();
    let (w, h) = (patch.width() as i32, patch.height() as i32);
    let origin = patch.origin();
    let mut gate: Vec<f32> = match opts.sampling {
        Sampling::Once | Sampling::BackgroundSwatch => {
            px.iter().map(|p| similarity(*p, base, tolerance)).collect()
        }
        Sampling::Continuous => {
            let mut g = vec![0.0f32; px.len()];
            for d in dabs {
                let Some(ci) = patch.index_of(d.center_pixel()) else {
                    continue;
                };
                let sample = px[ci];
                let (lo, hi) = d.bounds();
                for y in lo.y.max(origin.y)..hi.y.min(origin.y + h) {
                    for x in lo.x.max(origin.x)..hi.x.min(origin.x + w) {
                        let i = ((y - origin.y) * w + (x - origin.x)) as usize;
                        let s = similarity(px[i], sample, tolerance);
                        if s > g[i] {
                            g[i] = s;
                        }
                    }
                }
            }
            g
        }
    };
    if !antialias {
        for g in &mut gate {
            *g = if *g > 0.0 { 1.0 } else { 0.0 };
        }
    }
    if let Some(fg) = protect {
        for (g, p) in gate.iter_mut().zip(px) {
            if similarity(*p, fg, tolerance) > 0.0 {
                *g = 0.0;
            }
        }
    }
    if opts.limits == Limits::Discontiguous {
        return gate;
    }
    // Contiguous / Find Edges: keep only what a 4-connected flood from the
    // dab centres reaches through gated, covered pixels.
    let open = |i: usize| gate[i] > 0.0 && covered.get(i).is_some_and(|c| *c > 0.0);
    let luma: Vec<f32> = if opts.limits == Limits::FindEdges {
        px.iter().map(|p| encoded_luma(*p)).collect()
    } else {
        Vec::new()
    };
    let neighbours = |i: usize| {
        let (x, y) = ((i as i32) % w, (i as i32) / w);
        [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)]
            .into_iter()
            .filter(move |(nx, ny)| *nx >= 0 && *ny >= 0 && *nx < w && *ny < h)
            .map(move |(nx, ny)| (ny * w + nx) as usize)
    };
    let on_edge =
        |i: usize| !luma.is_empty() && neighbours(i).any(|n| (luma[n] - luma[i]).abs() > EDGE_STEP);
    let mut reached = vec![false; gate.len()];
    let mut queue = VecDeque::new();
    for d in dabs {
        let p: IVec2 = d.center_pixel();
        if let Some(i) = patch.index_of(p) {
            if open(i) && !reached[i] {
                reached[i] = true;
                queue.push_back(i);
            }
        }
    }
    while let Some(i) = queue.pop_front() {
        if on_edge(i) {
            continue;
        }
        for n in neighbours(i) {
            if !reached[n] && open(n) {
                reached[n] = true;
                queue.push_back(n);
            }
        }
    }
    for (g, r) in gate.iter_mut().zip(&reached) {
        if !*r {
            *g = 0.0;
        }
    }
    gate
}

/// How far past its 3×3 neighbourhood's range (as a fraction of that range)
/// Protect Detail lets a sharpened pixel go.
pub const PROTECT_DETAIL_MARGIN: f32 = 0.25;

/// Sharpen's Protect Detail: clamp every pixel of `sharp` to the per-channel
/// range of the 3×3 neighbourhood of the same pixel in `orig` (edges
/// clamped), widened by [`PROTECT_DETAIL_MARGIN`] of that range on each
/// side, so the sharpen steepens an edge with only a small overshoot.
pub(crate) fn protect_detail(
    orig: &FilterBuffer,
    sharp: &FilterBuffer,
) -> Result<FilterBuffer, ToolError> {
    let (w, h) = (orig.width(), orig.height());
    let mut out = Vec::with_capacity(sharp.pixels().len());
    for y in 0..h {
        for x in 0..w {
            let mut lo = [f32::INFINITY; 4];
            let mut hi = [f32::NEG_INFINITY; 4];
            for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let p = orig.get(nx, ny);
                    for c in 0..4 {
                        lo[c] = lo[c].min(p[c]);
                        hi[c] = hi[c].max(p[c]);
                    }
                }
            }
            let s = sharp.get(x, y);
            let mut px = [0.0; 4];
            for c in 0..4 {
                let m = (hi[c] - lo[c]) * PROTECT_DETAIL_MARGIN;
                px[c] = s[c].clamp(lo[c] - m, hi[c] + m);
            }
            out.push(px);
        }
    }
    Ok(FilterBuffer::from_pixels(w, h, out)?)
}

impl StrokeTool {
    /// W13-H: answer the symmetry, Eraser Mode and retouching keys, or
    /// `None` when `key` is not one of them for this tool's op (the caller
    /// then answers it, or refuses it as unknown).
    pub(crate) fn w13h_setting(
        &mut self,
        key: &str,
        setting: ToolSetting,
    ) -> Option<Result<(), ToolError>> {
        let paints = matches!(self.op, StrokeOp::Paint { .. } | StrokeOp::Erase);
        let erases = matches!(self.op, StrokeOp::Erase);
        let replaces = matches!(self.op, StrokeOp::ColorReplacement { .. });
        let bg = matches!(self.op, StrokeOp::BackgroundErase { .. });
        let sharpens = matches!(self.op, StrokeOp::Sharpen { .. });
        let mismatch = || {
            Some(Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            }))
        };
        let r = &mut self.retouch;
        match key {
            SYMMETRY_KEY if paints => match setting {
                ToolSetting::Choice(i) => self.symmetry.mode = SymmetryMode::from_choice(i),
                _ => return mismatch(),
            },
            SYMMETRY_SEGMENTS_KEY if paints => match setting {
                ToolSetting::Int(n) => {
                    self.symmetry.segments = n.clamp(MIN_SEGMENTS, MAX_SEGMENTS) as u32
                }
                _ => return mismatch(),
            },
            ERASER_MODE_KEY if erases => match setting {
                ToolSetting::Choice(i) => self.eraser_mode = EraserMode::from_choice(i),
                _ => return mismatch(),
            },
            REPLACE_MODE_KEY if replaces => match setting {
                ToolSetting::Choice(i) => r.replace_mode = ReplaceMode::from_choice(i),
                _ => return mismatch(),
            },
            ANTIALIAS_KEY if replaces => match setting {
                ToolSetting::Bool(v) => r.antialias = v,
                _ => return mismatch(),
            },
            SAMPLING_KEY if replaces || bg => match setting {
                ToolSetting::Choice(i) => r.sampling = Sampling::from_choice(i),
                _ => return mismatch(),
            },
            LIMITS_KEY if replaces || bg => match setting {
                ToolSetting::Choice(i) => r.limits = Limits::from_choice(i),
                _ => return mismatch(),
            },
            PROTECT_FOREGROUND_KEY if bg => match setting {
                ToolSetting::Bool(v) => r.protect_foreground = v,
                _ => return mismatch(),
            },
            PROTECT_DETAIL_KEY if sharpens => match setting {
                ToolSetting::Bool(v) => r.protect_detail = v,
                _ => return mismatch(),
            },
            _ => return None,
        }
        Some(Ok(()))
    }
}

/// W13-H: every option driven through the real tools the registry builds —
/// `registry::make` and `Tool::set_setting`, the seam the options bar feeds —
/// and judged by the pixels the stroke leaves.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry;
    use crate::symmetry::SymmetryMode;
    use crate::tiles::MemoryTiles;
    use crate::tool::{PointerEvent, Tool, ToolContext, ToolId};
    use editor_core::{Command, PixelKey};
    use layer_model::LayerId;
    use raster::PixelRect;

    const W: i64 = 64;
    const H: i64 = 64;
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const BLUE: [u8; 4] = [40, 80, 200, 255];
    const GREEN: [u8; 4] = [30, 200, 60, 255];
    const BLACK_F: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
    const WHITE_F: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    const RED_F: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    fn linear(c: [u8; 4]) -> [f32; 4] {
        [
            color::srgb8_to_linear(c[0]),
            color::srgb8_to_linear(c[1]),
            color::srgb8_to_linear(c[2]),
            c[3] as f32 / 255.0,
        ]
    }

    struct Canvas {
        tiles: MemoryTiles,
        layer: LayerId,
        key: PixelKey,
        w: i64,
        h: i64,
    }

    impl Canvas {
        fn new(fill: [u8; 4]) -> Self {
            Self::sized(W, H, fill)
        }

        fn sized(w: i64, h: i64, fill: [u8; 4]) -> Self {
            let layer = LayerId::new();
            let key = PixelKey::Layer(layer);
            let mut me = Self {
                tiles: MemoryTiles::new(),
                layer,
                key,
                w,
                h,
            };
            me.rect(0, 0, w, h, fill);
            me
        }

        fn canvas(&self) -> PixelRect {
            PixelRect::new(0, 0, self.w as u32, self.h as u32)
        }

        fn rect(&mut self, x0: i64, y0: i64, w: i64, h: i64, c: [u8; 4]) {
            for y in y0..y0 + h {
                for x in x0..x0 + w {
                    self.tiles.put_pixel(self.key, x, y, c);
                }
            }
        }

        fn px(&self, x: i64, y: i64) -> [u8; 4] {
            self.tiles.pixel(self.key, x, y)
        }

        /// Press at the first point, drag through the rest, release at the
        /// last; land the stroke's command.
        fn stroke(&mut self, tool: &mut dyn Tool, pts: &[(f32, f32)], fg: [f32; 4], bg: [f32; 4]) {
            let canvas = self.canvas();
            let mut ctx = ToolContext::new(&mut self.tiles, canvas)
                .with_layer(self.layer)
                .with_foreground(fg);
            ctx.background = bg;
            let (x0, y0) = pts[0];
            tool.on_pointer_down(&mut ctx, PointerEvent::at(x0, y0))
                .unwrap();
            for (x, y) in &pts[1..] {
                tool.on_pointer_move(&mut ctx, PointerEvent::at(*x, *y))
                    .unwrap();
            }
            let (x1, y1) = *pts.last().unwrap();
            tool.on_pointer_up(&mut ctx, PointerEvent::at(x1, y1))
                .unwrap();
            let cmds = ctx.drain();
            drop(ctx);
            for c in &cmds {
                if let Command::PaintTiles { delta, .. } = c {
                    self.tiles.apply_delta(self.key, delta);
                }
            }
        }
    }

    fn tool(id: ToolId, settings: &[(&str, ToolSetting)]) -> Box<dyn Tool> {
        let mut t = registry::make(id);
        for (k, v) in settings {
            t.set_setting(k, *v)
                .unwrap_or_else(|e| panic!("{id:?} refused {k}: {e}"));
        }
        t
    }

    fn line(a: (f32, f32), b: (f32, f32), n: usize) -> Vec<(f32, f32)> {
        (0..=n)
            .map(|i| {
                let t = i as f32 / n as f32;
                (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
            })
            .collect()
    }

    fn changed(a: [u8; 4], b: [u8; 4]) -> u32 {
        (0..4)
            .map(|c| (a[c] as i32 - b[c] as i32).unsigned_abs())
            .sum()
    }

    fn sym(mode: SymmetryMode) -> ToolSetting {
        let index = SymmetryMode::ALL.iter().position(|m| *m == mode).unwrap();
        ToolSetting::Choice(index)
    }

    // ----------------------------------------------------------- symmetry --

    #[test]
    fn a_vertical_symmetry_brush_stroke_paints_the_mirrored_pixels() {
        let paint = |mode: SymmetryMode| {
            let mut cv = Canvas::new(WHITE);
            let mut t = tool(
                ToolId::Brush,
                &[
                    ("size", ToolSetting::Float(6.0)),
                    ("hardness", ToolSetting::Float(1.0)),
                    (crate::symmetry::SYMMETRY_KEY, sym(mode)),
                ],
            );
            cv.stroke(
                t.as_mut(),
                &line((10.5, 20.5), (14.5, 20.5), 4),
                BLACK_F,
                WHITE_F,
            );
            cv
        };
        let off = paint(SymmetryMode::Off);
        let on = paint(SymmetryMode::Vertical);
        assert!(off.px(12, 20)[0] < 50, "the stroke itself paints");
        assert_eq!(off.px(51, 20), WHITE, "no symmetry, no mirror");
        assert!(on.px(12, 20)[0] < 50, "the stroke itself still paints");
        // x = 12.5 mirrors to 64 - 12.5 = 51.5 across the centre line.
        assert!(
            on.px(51, 20)[0] < 50,
            "the mirrored pixel: {:?}",
            on.px(51, 20)
        );
        assert_eq!(on.px(51, 43), WHITE, "a vertical mirror does not flip y");
    }

    #[test]
    fn radial_and_mandala_symmetry_paint_every_rotated_copy() {
        let click = |mode: SymmetryMode, segments: i32| {
            let mut cv = Canvas::new(WHITE);
            let mut t = tool(
                ToolId::Brush,
                &[
                    ("size", ToolSetting::Float(6.0)),
                    ("hardness", ToolSetting::Float(1.0)),
                    (crate::symmetry::SYMMETRY_KEY, sym(mode)),
                    (
                        crate::symmetry::SYMMETRY_SEGMENTS_KEY,
                        ToolSetting::Int(segments),
                    ),
                ],
            );
            cv.stroke(t.as_mut(), &[(10.5, 20.5)], BLACK_F, WHITE_F);
            cv
        };
        let radial = click(SymmetryMode::Radial, 4);
        // d = (-21.5, -11.5) about (32, 32): quarter turns land at
        // (43.5, 10.5), (53.5, 43.5) and (20.5, 53.5).
        for (x, y) in [(10, 20), (43, 10), (53, 43), (20, 53)] {
            assert!(radial.px(x, y)[0] < 50, "radial copy at ({x}, {y})");
        }
        assert_eq!(radial.px(53, 20), WHITE, "a radial copy is not a mirror");
        let mandala = click(SymmetryMode::Mandala, 4);
        for (x, y) in [(10, 20), (43, 10), (53, 43), (20, 53), (53, 20)] {
            assert!(mandala.px(x, y)[0] < 50, "mandala copy at ({x}, {y})");
        }
        let diagonal = click(SymmetryMode::Diagonal, 4);
        // (dx, dy) = (-21.5, -11.5) swaps to (-11.5, -21.5): (20.5, 10.5).
        assert!(diagonal.px(20, 10)[0] < 50);
        assert_eq!(diagonal.px(53, 20), WHITE);
    }

    #[test]
    fn the_pencil_and_the_eraser_mirror_too() {
        let mut cv = Canvas::new(WHITE);
        let mut pencil = tool(
            ToolId::Pencil,
            &[
                ("size", ToolSetting::Float(1.0)),
                (crate::symmetry::SYMMETRY_KEY, sym(SymmetryMode::Horizontal)),
            ],
        );
        cv.stroke(pencil.as_mut(), &[(20.5, 5.5)], BLACK_F, WHITE_F);
        assert_eq!(cv.px(20, 5), [0, 0, 0, 255]);
        assert_eq!(cv.px(20, 58), [0, 0, 0, 255], "y = 5.5 mirrors to 58.5");
        assert_eq!(cv.px(43, 5), WHITE);

        let mut cv = Canvas::new(WHITE);
        let mut eraser = tool(
            ToolId::Eraser,
            &[
                ("size", ToolSetting::Float(4.0)),
                ("hardness", ToolSetting::Float(1.0)),
                (crate::symmetry::SYMMETRY_KEY, sym(SymmetryMode::DualAxis)),
            ],
        );
        cv.stroke(eraser.as_mut(), &[(8.5, 8.5)], BLACK_F, WHITE_F);
        for (x, y) in [(8, 8), (55, 8), (8, 55), (55, 55)] {
            assert_eq!(cv.px(x, y)[3], 0, "dual-axis erase at ({x}, {y})");
        }
        assert_eq!(cv.px(32, 8), WHITE);
    }

    // --------------------------------------------------------- eraser mode --

    fn erase_click(mode: usize, extra: &[(&str, ToolSetting)]) -> Canvas {
        let mut cv = Canvas::new(WHITE);
        let mut settings = vec![(ERASER_MODE_KEY, ToolSetting::Choice(mode))];
        settings.extend_from_slice(extra);
        let mut t = tool(ToolId::Eraser, &settings);
        cv.stroke(t.as_mut(), &[(20.3, 20.7)], BLACK_F, WHITE_F);
        cv
    }

    fn alphas(cv: &Canvas) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..H {
            for x in 0..W {
                v.push(cv.px(x, y)[3]);
            }
        }
        v
    }

    #[test]
    fn eraser_mode_pencil_erases_hard_where_brush_leaves_a_soft_rim() {
        let soft = [
            ("size", ToolSetting::Float(12.0)),
            ("hardness", ToolSetting::Float(0.3)),
        ];
        let brush = alphas(&erase_click(0, &soft));
        let pencil = alphas(&erase_click(1, &soft));
        assert!(
            brush.iter().any(|a| *a > 0 && *a < 255),
            "a soft Brush erase leaves partial alpha"
        );
        assert!(pencil.contains(&0), "the Pencil erases");
        assert!(
            pencil.iter().all(|a| *a == 0 || *a == 255),
            "the Pencil erases every pixel fully or not at all"
        );
    }

    #[test]
    fn eraser_mode_block_erases_a_full_strength_sixteen_pixel_square() {
        // The brush's own size, hardness and a half opacity are ignored.
        let cv = erase_click(
            2,
            &[
                ("size", ToolSetting::Float(5.0)),
                ("hardness", ToolSetting::Float(0.0)),
                ("opacity", ToolSetting::Float(0.5)),
            ],
        );
        let a = alphas(&cv);
        assert_eq!(a.iter().filter(|a| **a == 0).count(), 256, "16 x 16 erased");
        assert!(a.iter().all(|a| *a == 0 || *a == 255), "at full strength");
        // (20.3, 20.7) snaps to (20, 21): the square is x 12..=27, y 13..=28,
        // corners included — a round dab would miss them.
        for (x, y) in [(12, 13), (27, 13), (12, 28), (27, 28)] {
            assert_eq!(cv.px(x, y)[3], 0, "corner ({x}, {y})");
        }
        for (x, y) in [(11, 13), (28, 13), (12, 12), (12, 29)] {
            assert_eq!(cv.px(x, y)[3], 255, "outside ({x}, {y})");
        }
    }

    // --------------------------------------------------- colour replacement --

    /// A blue field; one Colour Replacement click in red at its middle.
    fn replace_click(settings: &[(&str, ToolSetting)]) -> [u8; 4] {
        let mut cv = Canvas::new(BLUE);
        let mut all = vec![
            ("size", ToolSetting::Float(20.0)),
            ("hardness", ToolSetting::Float(1.0)),
        ];
        all.extend_from_slice(settings);
        let mut t = tool(ToolId::ColorReplacement, &all);
        cv.stroke(t.as_mut(), &[(32.5, 32.5)], RED_F, WHITE_F);
        cv.px(32, 32)
    }

    #[test]
    fn each_colour_replacement_mode_takes_its_own_component() {
        let mode = |i: usize| replace_click(&[(REPLACE_MODE_KEY, ToolSetting::Choice(i))]);
        let (hue, sat, col, lum) = (mode(0), mode(1), mode(2), mode(3));
        assert!(col[0] > col[2], "Colour turns blue red: {col:?}");
        assert!(hue[0] > hue[2], "Hue turns blue red: {hue:?}");
        assert!(lum[2] > lum[0], "Luminosity keeps the blue hue: {lum:?}");
        assert!(sat[2] > sat[0], "Saturation keeps the blue hue: {sat:?}");
        for (a, b) in [
            (hue, sat),
            (hue, col),
            (hue, lum),
            (sat, col),
            (sat, lum),
            (col, lum),
        ] {
            assert_ne!(a, b, "every mode answers differently");
        }
        for m in [hue, sat, col, lum] {
            assert_ne!(m, BLUE, "every mode changes the pixel");
        }
    }

    /// A blue field with a green stripe across it; a Colour Replacement or
    /// Background Eraser stroke along the diagonal that starts on the blue.
    fn striped_stroke(id: ToolId, settings: &[(&str, ToolSetting)], bg: [f32; 4]) -> Canvas {
        let mut cv = Canvas::new(BLUE);
        cv.rect(0, 28, W, 8, GREEN);
        let mut all = vec![
            ("size", ToolSetting::Float(40.0)),
            ("hardness", ToolSetting::Float(1.0)),
            ("spacing", ToolSetting::Float(0.05)),
            (LIMITS_KEY, ToolSetting::Choice(0)),
        ];
        all.extend_from_slice(settings);
        let mut t = tool(id, &all);
        cv.stroke(t.as_mut(), &line((10.0, 10.0), (54.0, 54.0), 20), RED_F, bg);
        cv
    }

    #[test]
    fn colour_replacement_sampling_once_continuous_and_background_swatch() {
        let s = |i: usize| [(SAMPLING_KEY, ToolSetting::Choice(i))];
        let once = striped_stroke(ToolId::ColorReplacement, &s(1), WHITE_F);
        assert!(
            once.px(32, 12)[0] > once.px(32, 12)[2],
            "Once recolours the blue"
        );
        assert_eq!(
            once.px(32, 31),
            GREEN,
            "Once leaves the stripe it never sampled"
        );

        let cont = striped_stroke(ToolId::ColorReplacement, &s(0), WHITE_F);
        assert!(
            cont.px(32, 12)[0] > cont.px(32, 12)[2],
            "Continuous recolours the blue"
        );
        let stripe = cont.px(32, 31);
        assert!(
            stripe[0] > stripe[1],
            "Continuous samples the stripe too: {stripe:?}"
        );

        let swatch = striped_stroke(ToolId::ColorReplacement, &s(2), linear(GREEN));
        assert_eq!(
            swatch.px(32, 12),
            BLUE,
            "the background swatch is green, not blue"
        );
        let stripe = swatch.px(32, 31);
        assert!(
            stripe[0] > stripe[1],
            "the swatch colour is recoloured: {stripe:?}"
        );
    }

    /// A blue field cut by a vertical bar; one click left of the bar whose
    /// brush reaches past it.
    fn barred_click(id: ToolId, bar: [u8; 4], settings: &[(&str, ToolSetting)]) -> Canvas {
        let mut cv = Canvas::new(BLUE);
        cv.rect(30, 0, 4, H, bar);
        let mut all = vec![
            ("size", ToolSetting::Float(30.0)),
            ("hardness", ToolSetting::Float(1.0)),
            (SAMPLING_KEY, ToolSetting::Choice(1)),
        ];
        all.extend_from_slice(settings);
        let mut t = tool(id, &all);
        cv.stroke(t.as_mut(), &[(22.5, 32.5)], RED_F, WHITE_F);
        cv
    }

    #[test]
    fn colour_replacement_limits_discontiguous_contiguous_and_find_edges() {
        let l = |i: usize| (LIMITS_KEY, ToolSetting::Choice(i));
        let dis = barred_click(ToolId::ColorReplacement, GREEN, &[l(0)]);
        assert_ne!(dis.px(35, 32), BLUE, "Discontiguous reaches past the bar");
        let con = barred_click(ToolId::ColorReplacement, GREEN, &[l(1)]);
        assert_ne!(
            con.px(22, 32),
            BLUE,
            "Contiguous recolours the clicked side"
        );
        assert_eq!(con.px(35, 32), BLUE, "Contiguous stops at the bar");
        // A dark-blue bar inside a wide tolerance: Contiguous floods through
        // it, Find Edges stops at its luminance edge.
        let dark = [10, 20, 60, 255];
        let wide = (TOLERANCE, ToolSetting::Float(1.0));
        let con = barred_click(ToolId::ColorReplacement, dark, &[l(1), wide]);
        assert_ne!(
            con.px(35, 32),
            BLUE,
            "Contiguous crosses a within-tolerance bar"
        );
        let edges = barred_click(ToolId::ColorReplacement, dark, &[l(2), wide]);
        assert_ne!(
            edges.px(22, 32),
            BLUE,
            "Find Edges recolours the clicked side"
        );
        assert_eq!(edges.px(35, 32), BLUE, "Find Edges stops at the edge");
    }

    const TOLERANCE: &str = "tolerance";

    #[test]
    fn colour_replacement_anti_alias_off_replaces_a_near_match_fully() {
        let near = [40, 80, 150, 255];
        let run = |aa: bool| {
            let mut cv = Canvas::new(BLUE);
            cv.rect(36, 30, 4, 4, near);
            let mut t = tool(
                ToolId::ColorReplacement,
                &[
                    ("size", ToolSetting::Float(30.0)),
                    ("hardness", ToolSetting::Float(1.0)),
                    (TOLERANCE, ToolSetting::Float(0.3)),
                    (SAMPLING_KEY, ToolSetting::Choice(1)),
                    (LIMITS_KEY, ToolSetting::Choice(0)),
                    (ANTIALIAS_KEY, ToolSetting::Bool(aa)),
                ],
            );
            cv.stroke(t.as_mut(), &[(30.5, 32.5)], RED_F, WHITE_F);
            (cv.px(30, 32), cv.px(37, 31))
        };
        let (on_exact, on_near) = run(true);
        let (off_exact, off_near) = run(false);
        assert_eq!(
            on_exact, off_exact,
            "an exact match is replaced fully either way"
        );
        assert_ne!(
            on_near, near,
            "anti-aliased, a near match is partly replaced"
        );
        assert!(
            changed(off_near, near) > changed(on_near, near) + 20,
            "without anti-alias the near match is replaced fully: {off_near:?} vs {on_near:?}"
        );
    }

    // ---------------------------------------------------- background eraser --

    #[test]
    fn background_eraser_sampling_limits_and_protect_foreground() {
        let s = |i: usize| [(SAMPLING_KEY, ToolSetting::Choice(i))];
        let once = striped_stroke(ToolId::BackgroundEraser, &s(1), WHITE_F);
        assert_eq!(once.px(32, 12)[3], 0, "Once erases the blue it sampled");
        assert_eq!(once.px(32, 31), GREEN, "Once keeps the stripe");
        let cont = striped_stroke(ToolId::BackgroundEraser, &s(0), WHITE_F);
        assert_eq!(cont.px(32, 31)[3], 0, "Continuous erases the stripe too");
        let swatch = striped_stroke(ToolId::BackgroundEraser, &s(2), linear(GREEN));
        assert_eq!(
            swatch.px(32, 12),
            BLUE,
            "the swatch is green: blue survives"
        );
        assert_eq!(swatch.px(32, 31)[3], 0, "the swatch colour is erased");

        let l = |i: usize| (LIMITS_KEY, ToolSetting::Choice(i));
        let dis = barred_click(ToolId::BackgroundEraser, GREEN, &[l(0)]);
        assert_eq!(dis.px(35, 32)[3], 0, "Discontiguous erases past the bar");
        let con = barred_click(ToolId::BackgroundEraser, GREEN, &[l(1)]);
        assert_eq!(con.px(22, 32)[3], 0, "Contiguous erases the clicked side");
        assert_eq!(con.px(35, 32), BLUE, "Contiguous stops at the bar");

        let protect = |on: bool| {
            let mut cv = Canvas::new(BLUE);
            let mut t = tool(
                ToolId::BackgroundEraser,
                &[
                    ("size", ToolSetting::Float(20.0)),
                    (PROTECT_FOREGROUND_KEY, ToolSetting::Bool(on)),
                ],
            );
            cv.stroke(t.as_mut(), &[(32.5, 32.5)], linear(BLUE), WHITE_F);
            cv.px(32, 32)
        };
        assert_eq!(protect(false)[3], 0, "unprotected, the blue is erased");
        assert_eq!(protect(true), BLUE, "the foreground blue is protected");
    }

    #[test]
    fn the_made_background_eraser_samples_once_as_its_options_bar_declares() {
        // The options bar's declared default and the made tool agree: Once.
        let spec = registry::info(ToolId::BackgroundEraser)
            .and_then(|i| i.options.iter().find(|o| o.key == SAMPLING_KEY))
            .expect("the Background Eraser declares Sampling");
        assert!(
            matches!(
                spec.kind,
                crate::registry::OptionKind::Choice { default: 1, .. }
            ),
            "the declared Sampling default is Once: {:?}",
            spec.kind
        );
        // With no Sampling set, a stroke started on the blue keeps the
        // green stripe it crosses (Continuous would erase it).
        let made = striped_stroke(ToolId::BackgroundEraser, &[], WHITE_F);
        assert_eq!(made.px(32, 12)[3], 0, "the blue first touched is erased");
        assert_eq!(made.px(32, 31), GREEN, "the stripe it crosses is kept");
        // Photopea's other defaults stand: Contiguous is on.
        let bar = barred_click(ToolId::BackgroundEraser, GREEN, &[]);
        assert_eq!(
            bar.px(35, 32),
            BLUE,
            "the default Contiguous stops at the bar"
        );
    }

    // --------------------------------------------------------- live preview --

    /// Drive `pts` with a live preview after every sample, folding the
    /// answers as the shell does; return (the folded preview, the release's
    /// tiles).
    fn preview_and_release(
        t: &mut dyn Tool,
        cv: &mut Canvas,
        pts: &[(f32, f32)],
        fg: [f32; 4],
    ) -> (
        std::collections::BTreeMap<raster::TileCoord, crate::stroke::LiveTile>,
        std::collections::BTreeMap<raster::TileCoord, crate::stroke::LiveTile>,
    ) {
        use crate::stroke::LiveTile;
        use crate::tiles::TileAccess;
        let mut preview = std::collections::BTreeMap::new();
        let canvas = cv.canvas();
        let cmds = {
            let mut ctx = ToolContext::new(&mut cv.tiles, canvas)
                .with_layer(cv.layer)
                .with_foreground(fg);
            t.on_pointer_down(&mut ctx, PointerEvent::at(pts[0].0, pts[0].1))
                .unwrap();
            for (x, y) in &pts[1..] {
                t.on_pointer_move(&mut ctx, PointerEvent::at(*x, *y))
                    .unwrap();
                if let Some(live) = t.live_paint(&mut ctx).unwrap() {
                    if live.replace {
                        preview.clear();
                    }
                    for (coord, tile) in live.tiles {
                        match tile {
                            LiveTile::Committed => {
                                preview.remove(&coord);
                            }
                            other => {
                                preview.insert(coord, other);
                            }
                        }
                    }
                }
            }
            let (x, y) = *pts.last().unwrap();
            t.on_pointer_up(&mut ctx, PointerEvent::at(x, y)).unwrap();
            ctx.drain()
        };
        let Some(Command::PaintTiles { delta, .. }) = cmds.first() else {
            panic!("the release emitted {cmds:?}");
        };
        let mut committed = std::collections::BTreeMap::new();
        for edit in delta.iter() {
            let tile = match edit.hash {
                Some(h) => LiveTile::Bytes(cv.tiles.bytes(h).expect("committed").to_vec()),
                None => LiveTile::Cleared,
            };
            committed.insert(edit.coord, tile);
        }
        cv.tiles.apply_delta(cv.key, delta);
        (preview, committed)
    }

    #[test]
    fn the_live_preview_shows_the_mirrored_dabs_and_the_regional_gates_the_release_commits() {
        let pts = line((10.5, 20.5), (18.5, 26.5), 6);
        let mut cv = Canvas::new(WHITE);
        let mut brush = tool(
            ToolId::Brush,
            &[
                ("size", ToolSetting::Float(6.0)),
                (crate::symmetry::SYMMETRY_KEY, sym(SymmetryMode::Mandala)),
            ],
        );
        let (preview, committed) = preview_and_release(brush.as_mut(), &mut cv, &pts, BLACK_F);
        assert!(!committed.is_empty());
        assert_eq!(
            preview, committed,
            "a symmetric brush previews what it commits"
        );

        // A Contiguous Colour Replacement across the seam of two tiles. The
        // pocket at x 250..=255 (tile 0) is walled in green on three sides
        // and opens only to the right, into tile 1: the release's flood
        // reaches it from the dab centres in tile 1. A preview sample whose
        // new dabs touch only tile 0 must still answer it the same way, so
        // the whole stroke is re-answered (a lone tile 0 has no seed that
        // reaches the pocket).
        let mut cv = Canvas::sized(512, 64, BLUE);
        cv.rect(248, 26, 2, 13, GREEN);
        cv.rect(248, 26, 8, 2, GREEN);
        cv.rect(248, 37, 8, 2, GREEN);
        let mut replace = tool(
            ToolId::ColorReplacement,
            &[
                ("size", ToolSetting::Float(30.0)),
                (SAMPLING_KEY, ToolSetting::Choice(1)),
                (LIMITS_KEY, ToolSetting::Choice(1)),
            ],
        );
        let pts = line((300.0, 45.0), (200.0, 45.0), 10);
        let (preview, committed) = preview_and_release(replace.as_mut(), &mut cv, &pts, RED_F);
        assert!(!committed.is_empty());
        assert_eq!(
            preview, committed,
            "a regional gate previews what it commits"
        );
        assert_ne!(cv.px(252, 32), BLUE, "the release reached the pocket");
        assert_eq!(cv.px(200, 10), BLUE, "outside the brush nothing moves");
    }

    // -------------------------------------------------------------- sharpen --

    #[test]
    fn sharpen_protect_detail_steepens_an_edge_with_a_quarter_of_the_overshoot() {
        let run = |protect: bool| {
            let mut cv = Canvas::new([64, 64, 64, 255]);
            cv.rect(36, 0, W - 36, H, [192, 192, 192, 255]);
            // A soft ramp between the two levels, x 28..36.
            for (i, x) in (28..36).enumerate() {
                let v = 64 + (i as u8 + 1) * 14;
                cv.rect(x, 0, 1, H, [v, v, v, 255]);
            }
            let before: Vec<[u8; 4]> = (0..W).map(|x| cv.px(x, 32)).collect();
            let mut t = tool(
                ToolId::Sharpen,
                &[
                    ("size", ToolSetting::Float(30.0)),
                    ("hardness", ToolSetting::Float(1.0)),
                    ("amount", ToolSetting::Float(4.0)),
                    (PROTECT_DETAIL_KEY, ToolSetting::Bool(protect)),
                ],
            );
            cv.stroke(
                t.as_mut(),
                &line((32.0, 20.0), (32.0, 44.0), 12),
                BLACK_F,
                WHITE_F,
            );
            let after: Vec<[u8; 4]> = (0..W).map(|x| cv.px(x, 32)).collect();
            (before, after)
        };
        let (before, plain) = run(false);
        let (_, protected) = run(true);
        // How far the row strays outside the edge's own 64..=192.
        let overshoot = |row: &[[u8; 4]]| {
            row.iter()
                .map(|p| (64i32 - p[0] as i32).max(p[0] as i32 - 192).max(0))
                .max()
                .unwrap_or(0)
        };
        assert!(
            overshoot(&plain) >= 10,
            "a plain sharpen overshoots the edge: {plain:?}"
        );
        assert!(
            overshoot(&protected) * 4 <= overshoot(&plain),
            "Protect Detail keeps the overshoot small: {} vs {}: {protected:?}",
            overshoot(&protected),
            overshoot(&plain)
        );
        assert_ne!(protected, before, "Protect Detail still sharpens");
    }
}
