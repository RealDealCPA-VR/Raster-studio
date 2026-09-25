//! W13-N: Select ▸ Magic Cut… — Photopea's guided cutout window.
//!
//! The active layer is shown in a pane. The user paints **foreground**
//! strokes over what to keep and **background** strokes over what to drop;
//! Preview (and OK) turns the strokes into a mask with GrabCut
//! ([`selection::grabcut`]: the strokes are hard labels, everything else is
//! decided by the colour models and the contrast-sensitive cut — the same
//! engine Select ▸ Subject runs on its own seeds), then runs Refine Edge's
//! pipeline ([`selection::refine_mask`]: smooth, shift, feather) over it.
//! The confirmed [`MagicCutSpec`] carries the finished full-resolution
//! mask and where it goes — a selection, the layer's mask or a new layer —
//! and the application lands it as ONE undo step.
//!
//! # How the cut is computed
//!
//! [`cut_mask`] reduces the layer to a working grid (the longest side at
//! most [`WORKING_SIDE`]), paints the strokes into a trimap on it (stroke
//! cells are hard labels; unpainted cells inside the foreground strokes'
//! box, grown by a margin, start as probably-foreground, the rest as
//! probably-background), runs the cut, keeps only the foreground connected
//! to a foreground stroke, and brings the result back to full resolution
//! bilinearly before the refinement.

use editor_core::SelectionMask;
use egui::{Context, Sense};
use glam::IVec2;
use selection::grabcut::{grabcut, Label};
use selection::RefineParams;

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::combo;
use super::sizes;
use crate::strings::tr;
use design::{color32, current_tokens, ColorRole};

/// Longest side of the grid GrabCut runs on.
pub const WORKING_SIDE: u32 = 256;

/// GrabCut rounds per cut.
const ROUNDS: u32 = 5;

/// Longest side of the pane's preview texture. The layer itself can be far
/// larger than the GPU's largest texture (egui panics on upload past it), and
/// the pane never draws more than a column wide, so the preview is a bounded
/// copy — tinted per cut on the copy, never on the full-resolution layer.
pub const PREVIEW_MAX_SIDE: u32 = 512;

/// A nearest-neighbour copy of `rgba` (`width * height`) whose longest side
/// is at most [`PREVIEW_MAX_SIDE`], with its size.
fn bounded_preview(rgba: &[u8], width: u32, height: u32) -> (Vec<u8>, u32, u32) {
    let expected = width as usize * height as usize * 4;
    if width == 0 || height == 0 || rgba.len() < expected {
        return (vec![0; 4], 1, 1);
    }
    let long = width.max(height);
    if long <= PREVIEW_MAX_SIDE {
        return (rgba[..expected].to_vec(), width, height);
    }
    let k = PREVIEW_MAX_SIDE as f32 / long as f32;
    let pw = ((width as f32 * k).round() as u32).clamp(1, PREVIEW_MAX_SIDE);
    let ph = ((height as f32 * k).round() as u32).clamp(1, PREVIEW_MAX_SIDE);
    let mut out = vec![0u8; pw as usize * ph as usize * 4];
    for y in 0..ph {
        let sy = (((y as f32 + 0.5) * height as f32 / ph as f32) as u32).min(height - 1);
        for x in 0..pw {
            let sx = (((x as f32 + 0.5) * width as f32 / pw as f32) as u32).min(width - 1);
            let s = (sy as usize * width as usize + sx as usize) * 4;
            let d = (y as usize * pw as usize + x as usize) * 4;
            out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
        }
    }
    (out, pw, ph)
}

/// Where the confirmed cut lands.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum MagicCutOutput {
    /// Replace the selection.
    #[default]
    Selection,
    /// Add a layer mask to the active layer.
    LayerMask,
    /// Copy the cut pixels to a new layer.
    NewLayer,
}

impl MagicCutOutput {
    pub const ALL: [MagicCutOutput; 3] = [
        MagicCutOutput::Selection,
        MagicCutOutput::LayerMask,
        MagicCutOutput::NewLayer,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MagicCutOutput::Selection => tr("ui.magic_cut.output.selection"),
            MagicCutOutput::LayerMask => tr("ui.magic_cut.output.mask"),
            MagicCutOutput::NewLayer => tr("ui.magic_cut.output.layer"),
        }
    }
}

/// One painted stroke, in document pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct MagicCutStroke {
    /// `true` paints what to keep, `false` what to drop.
    pub foreground: bool,
    /// Brush radius, document pixels.
    pub radius: f32,
    /// The stroke's path, document pixels.
    pub points: Vec<[f32; 2]>,
}

/// What OK hands the application.
#[derive(Clone, Debug, PartialEq)]
pub struct MagicCutSpec {
    /// The cut over the whole canvas (origin 0,0), refined.
    pub mask: SelectionMask,
    pub output: MagicCutOutput,
}

/// Why a cut could not be made — a sentence for the window.
fn nothing(key: &'static str) -> String {
    tr(key).to_string()
}

/// A straight-alpha pixel over mid grey, as RGB `0..=255`.
fn over_grey(px: &[u8]) -> [f64; 3] {
    let a = f64::from(px[3]) / 255.0;
    let c = |v: u8| f64::from(v) * a + 128.0 * (1.0 - a);
    [c(px[0]), c(px[1]), c(px[2])]
}

/// Mark every cell of a `gw * gh` grid within `radius` (grid cells) of the
/// polyline `points` (grid coordinates) with `label`.
fn paint(
    labels: &mut [Label],
    gw: usize,
    gh: usize,
    points: &[[f32; 2]],
    radius: f32,
    label: Label,
) {
    let r = radius.max(0.5);
    let segments: Vec<([f32; 2], [f32; 2])> = if points.len() == 1 {
        vec![(points[0], points[0])]
    } else {
        points.windows(2).map(|w| (w[0], w[1])).collect()
    };
    for (a, b) in segments {
        let x0 = (a[0].min(b[0]) - r).floor().max(0.0) as usize;
        let y0 = (a[1].min(b[1]) - r).floor().max(0.0) as usize;
        let x1 = ((a[0].max(b[0]) + r).ceil().max(0.0) as usize).min(gw.saturating_sub(1));
        let y1 = ((a[1].max(b[1]) + r).ceil().max(0.0) as usize).min(gh.saturating_sub(1));
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len2 = dx * dx + dy * dy;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let t = if len2 > 0.0 {
                    (((px - a[0]) * dx + (py - a[1]) * dy) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (cx, cy) = (a[0] + t * dx, a[1] + t * dy);
                if (px - cx).powi(2) + (py - cy).powi(2) <= r * r {
                    labels[y * gw + x] = label;
                }
            }
        }
    }
}

/// The cut `strokes` make over `rgba` (`width * height`, straight RGBA8),
/// refined by `refine`, as a canvas-sized mask.
pub fn cut_mask(
    rgba: &[u8],
    width: u32,
    height: u32,
    strokes: &[MagicCutStroke],
    refine: &RefineParams,
) -> Result<SelectionMask, String> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || rgba.len() != w * h * 4 {
        return Err(nothing("ui.magic_cut.no_pixels"));
    }
    if !strokes.iter().any(|s| s.foreground && !s.points.is_empty()) {
        return Err(nothing("ui.magic_cut.need_foreground"));
    }
    // The working grid: the layer averaged over `f x f` blocks.
    let f = (width.max(height).div_ceil(WORKING_SIDE)).max(1) as usize;
    let (gw, gh) = (w.div_ceil(f), h.div_ceil(f));
    let mut grid = vec![[0.0f64; 3]; gw * gh];
    for (gy, row) in grid.chunks_mut(gw).enumerate() {
        for (gx, cell) in row.iter_mut().enumerate() {
            let (mut sum, mut n) = ([0.0f64; 3], 0.0);
            for y in gy * f..((gy + 1) * f).min(h) {
                for x in gx * f..((gx + 1) * f).min(w) {
                    let c = over_grey(&rgba[(y * w + x) * 4..(y * w + x) * 4 + 4]);
                    for k in 0..3 {
                        sum[k] += c[k];
                    }
                    n += 1.0;
                }
            }
            *cell = [sum[0] / n, sum[1] / n, sum[2] / n];
        }
    }
    // The trimap: probable labels from the foreground strokes' box, then the
    // strokes themselves as hard labels (background painted last wins).
    let scale = 1.0 / f as f32;
    let mut lo = [f32::MAX; 2];
    let mut hi = [f32::MIN; 2];
    for s in strokes.iter().filter(|s| s.foreground) {
        for p in &s.points {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k] - s.radius);
                hi[k] = hi[k].max(p[k] + s.radius);
            }
        }
    }
    let margin = 0.15 * width.max(height) as f32;
    let (bx0, by0) = ((lo[0] - margin) * scale, (lo[1] - margin) * scale);
    let (bx1, by1) = ((hi[0] + margin) * scale, (hi[1] + margin) * scale);
    let mut trimap = vec![Label::ProbablyBackground; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            let (cx, cy) = (gx as f32 + 0.5, gy as f32 + 0.5);
            if cx >= bx0 && cx <= bx1 && cy >= by0 && cy <= by1 {
                trimap[gy * gw + gx] = Label::ProbablyForeground;
            }
        }
    }
    for foreground in [true, false] {
        for s in strokes.iter().filter(|s| s.foreground == foreground) {
            let pts: Vec<[f32; 2]> = s
                .points
                .iter()
                .map(|p| [p[0] * scale, p[1] * scale])
                .collect();
            let label = if foreground {
                Label::Foreground
            } else {
                Label::Background
            };
            paint(&mut trimap, gw, gh, &pts, s.radius * scale, label);
        }
    }
    let models =
        grabcut(&grid, gw, gh, &mut trimap, ROUNDS, &mut |_| {}).map_err(|e| e.to_string())?;
    if models.is_none() {
        return Err(nothing("ui.magic_cut.need_background"));
    }
    // Keep the foreground connected to a foreground stroke.
    let mut keep = vec![false; gw * gh];
    let mut stack: Vec<usize> = (0..gw * gh)
        .filter(|&i| trimap[i] == Label::Foreground)
        .collect();
    for &i in &stack {
        keep[i] = true;
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % gw, i / gw);
        let mut visit = |nx: usize, ny: usize| {
            let j = ny * gw + nx;
            if !keep[j] && trimap[j].is_foreground() {
                keep[j] = true;
                stack.push(j);
            }
        };
        if x > 0 {
            visit(x - 1, y);
        }
        if x + 1 < gw {
            visit(x + 1, y);
        }
        if y > 0 {
            visit(x, y - 1);
        }
        if y + 1 < gh {
            visit(x, y + 1);
        }
    }
    // Back to full resolution, bilinearly between cell centres.
    let at = |gx: isize, gy: isize| -> f32 {
        let gx = gx.clamp(0, gw as isize - 1) as usize;
        let gy = gy.clamp(0, gh as isize - 1) as usize;
        if keep[gy * gw + gx] {
            255.0
        } else {
            0.0
        }
    };
    let mut coverage = vec![0u8; w * h];
    for y in 0..h {
        let fy = (y as f32 + 0.5) / f as f32 - 0.5;
        let (y0, ty) = (fy.floor() as isize, fy - fy.floor());
        for x in 0..w {
            let fx = (x as f32 + 0.5) / f as f32 - 0.5;
            let (x0, tx) = (fx.floor() as isize, fx - fx.floor());
            let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx;
            let bottom = at(x0, y0 + 1) * (1.0 - tx) + at(x0 + 1, y0 + 1) * tx;
            coverage[y * w + x] = (top * (1.0 - ty) + bottom * ty).round() as u8;
        }
    }
    let mask =
        SelectionMask::new(IVec2::ZERO, width, height, coverage).map_err(|e| e.to_string())?;
    let refined = selection::refine_mask(&mask, refine).map_err(|e| e.to_string())?;
    if refined.is_empty() {
        return Err(nothing("ui.magic_cut.empty"));
    }
    Ok(refined)
}

/// Select ▸ Magic Cut….
pub struct MagicCutDialog {
    width: u32,
    height: u32,
    /// The layer, straight RGBA8, `width * height`.
    rgba: Vec<u8>,
    /// The pane's bounded copy of the layer ([`bounded_preview`]) and its
    /// size.
    preview: (Vec<u8>, u32, u32),
    strokes: Vec<MagicCutStroke>,
    /// Painting the foreground (keep) rather than the background.
    painting_foreground: bool,
    /// Brush radius, document pixels.
    radius: f32,
    /// Refine Edge: smooth, shift, feather.
    smooth: u32,
    shift: i32,
    feather: f32,
    output: MagicCutOutput,
    /// The last computed cut, drawn over the pane.
    result: Option<SelectionMask>,
    /// Why the last cut failed.
    error: Option<String>,
    texture: Option<egui::TextureHandle>,
    /// The texture no longer shows the strokes or the cut.
    dirty: bool,
    /// The stroke the pointer is painting.
    live: Option<usize>,
}

/// Stable ids for a headless test.
pub mod ids {
    /// The painting pane.
    pub fn pane() -> egui::Id {
        egui::Id::new("raster-magic-cut-pane")
    }
    /// The Foreground / Background brush toggle.
    pub fn brush_toggle() -> egui::Id {
        egui::Id::new("raster-magic-cut-brush")
    }
}

impl MagicCutDialog {
    /// Over the active layer's pixels (`width * height`, straight RGBA8).
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        let radius = (width.max(height) as f32 / 40.0).max(2.0);
        let preview = bounded_preview(&rgba, width, height);
        Self {
            width,
            height,
            rgba,
            preview,
            strokes: Vec::new(),
            painting_foreground: true,
            radius,
            smooth: 0,
            shift: 0,
            feather: 0.0,
            output: MagicCutOutput::Selection,
            result: None,
            error: None,
            texture: None,
            dirty: true,
            live: None,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The size of the texture the pane uploads — never past
    /// [`PREVIEW_MAX_SIDE`] on either side.
    pub fn preview_size(&self) -> (u32, u32) {
        (self.preview.1, self.preview.2)
    }

    pub fn strokes(&self) -> &[MagicCutStroke] {
        &self.strokes
    }

    /// Add a stroke directly — tests and scripts.
    pub fn add_stroke(&mut self, stroke: MagicCutStroke) {
        self.strokes.push(stroke);
        self.result = None;
        self.dirty = true;
    }

    pub fn set_output(&mut self, output: MagicCutOutput) {
        self.output = output;
    }

    pub fn output(&self) -> MagicCutOutput {
        self.output
    }

    /// Which brush the pane paints with.
    pub fn painting_foreground(&self) -> bool {
        self.painting_foreground
    }

    fn refine(&self) -> RefineParams {
        RefineParams {
            feather_px: self.feather,
            shift_px: self.shift,
            smooth_px: self.smooth,
            contrast: 0.0,
        }
    }

    /// Compute the cut of the strokes painted so far.
    pub fn compute(&mut self) -> Result<SelectionMask, String> {
        let out = cut_mask(
            &self.rgba,
            self.width,
            self.height,
            &self.strokes,
            &self.refine(),
        );
        match &out {
            Ok(mask) => {
                self.result = Some(mask.clone());
                self.error = None;
            }
            Err(e) => {
                self.result = None;
                self.error = Some(e.clone());
            }
        }
        self.dirty = true;
        out
    }

    /// The spec OK hands over: the cut, computed now if it is not current.
    pub fn confirm(&mut self) -> Option<MagicCutSpec> {
        let mask = match self.result.clone() {
            Some(mask) => mask,
            None => self.compute().ok()?,
        };
        Some(MagicCutSpec {
            mask,
            output: self.output,
        })
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<MagicCutSpec> {
        let keys = DialogKeys::read(ctx);
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        let mut outcome = DialogOutcome::Open;
        if keys.confirm {
            if let Some(spec) = self.confirm() {
                return DialogOutcome::Confirmed(spec);
            }
        }
        let drawn = modal(
            ctx,
            "magic-cut",
            tr("ui.magic_cut.title"),
            Some(tr("ui.magic_cut.subtitle")),
            DialogWidth::Split,
            |ui| self.body(ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                // Preview.
                DialogButton::Extra(0) => {
                    let _ = self.compute();
                    DialogOutcome::Open
                }
                // Clear the strokes.
                DialogButton::Extra(_) => {
                    self.strokes.clear();
                    self.result = None;
                    self.error = None;
                    self.dirty = true;
                    DialogOutcome::Open
                }
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        ui.horizontal(|ui| {
            let label = if self.painting_foreground {
                tr("ui.magic_cut.brush.foreground")
            } else {
                tr("ui.magic_cut.brush.background")
            };
            let toggle = ui.add(egui::Button::new(label).selected(true));
            let toggle = ui.interact(toggle.rect, ids::brush_toggle(), Sense::click());
            if toggle.clicked() {
                self.painting_foreground = !self.painting_foreground;
            }
            ui.label(tr("ui.magic_cut.brush.size"));
            let most = self.width.max(self.height).max(4) as f32 / 4.0;
            ui.add(egui::Slider::new(&mut self.radius, 1.0..=most));
        });
        caption(ui, tr("ui.magic_cut.hint"));
        self.pane(ui);
        if let Some(error) = &self.error {
            caption(ui, error.clone());
        }
        egui::Grid::new("raster-magic-cut-refine")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label(tr("ui.magic_cut.smooth"));
                ui.add(egui::Slider::new(&mut self.smooth, 0..=20));
                ui.end_row();
                ui.label(tr("ui.magic_cut.shift"));
                ui.add(egui::Slider::new(&mut self.shift, -20..=20));
                ui.end_row();
                ui.label(tr("ui.magic_cut.feather"));
                ui.add(egui::Slider::new(&mut self.feather, 0.0..=40.0));
                ui.end_row();
                ui.label(tr("ui.magic_cut.output"));
                combo(
                    ui,
                    "raster-magic-cut-output",
                    &mut self.output,
                    &MagicCutOutput::ALL,
                    |o| o.label().to_string(),
                    |_| None,
                );
                ui.end_row();
            });
        let blocked = (!self.strokes.iter().any(|s| s.foreground))
            .then(|| tr("ui.magic_cut.need_foreground"));
        action_row(
            ui,
            tr("ui.magic_cut.ok"),
            blocked,
            &[tr("ui.magic_cut.preview"), tr("ui.magic_cut.clear")],
        )
    }

    /// The pane's bounded copy of the layer with the last cut painted in:
    /// each preview cell reads the cut at the document pixel under its
    /// centre.
    fn preview_rgba(&self, ui: &egui::Ui) -> Vec<u8> {
        let t = current_tokens(ui);
        let dim = t.palette.color(ColorRole::ShadowColor);
        let (base, pw, ph) = &self.preview;
        let mut rgba = base.clone();
        if let Some(mask) = &self.result {
            let k = f32::from(dim.a.max(160)) / 255.0;
            let sx = self.width as f32 / *pw as f32;
            let sy = self.height as f32 / *ph as f32;
            for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let x =
                    ((((i % *pw as usize) as f32 + 0.5) * sx) as i32).min(self.width as i32 - 1);
                let y =
                    ((((i / *pw as usize) as f32 + 0.5) * sy) as i32).min(self.height as i32 - 1);
                let keep = f32::from(mask.coverage_at(IVec2::new(x, y))) / 255.0;
                let a = k * (1.0 - keep);
                let tint = [dim.r, dim.g, dim.b];
                for c in 0..3 {
                    px[c] = (f32::from(px[c]) * (1.0 - a) + f32::from(tint[c]) * a).round() as u8;
                }
                px[3] = px[3].max((255.0 * a) as u8);
            }
        }
        rgba
    }

    fn pane(&mut self, ui: &mut egui::Ui) {
        if self.texture.is_none() || self.dirty {
            let rgba = self.preview_rgba(ui);
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [self.preview.1 as usize, self.preview.2 as usize],
                &rgba,
            );
            self.texture = Some(ui.ctx().load_texture(
                "magic-cut-preview",
                image,
                egui::TextureOptions::LINEAR,
            ));
            self.dirty = false;
        }
        let Some(texture) = self.texture.clone() else {
            return;
        };
        let long = sizes::preview_column_width();
        let scale = long / self.width.max(self.height).max(1) as f32;
        let size = egui::Vec2::new(self.width as f32 * scale, self.height as f32 * scale);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let response = ui.interact(rect, ids::pane(), Sense::click_and_drag());
        egui::Image::new((texture.id(), size)).paint_at(ui, rect);
        let to_doc = |pos: egui::Pos2| -> [f32; 2] {
            let local = pos - rect.min;
            [local.x / scale, local.y / scale]
        };
        let pressed = response.is_pointer_button_down_on();
        match (pressed, response.interact_pointer_pos()) {
            (true, Some(pos)) => {
                let here = to_doc(pos);
                match self.live {
                    Some(i) => self.strokes[i].points.push(here),
                    None => {
                        self.strokes.push(MagicCutStroke {
                            foreground: self.painting_foreground,
                            radius: self.radius,
                            points: vec![here],
                        });
                        self.live = Some(self.strokes.len() - 1);
                        self.result = None;
                    }
                }
                ui.ctx().request_repaint();
            }
            _ => self.live = None,
        }
        let t = current_tokens(ui);
        let painter = ui.painter_at(rect);
        for s in &self.strokes {
            let role = if s.foreground {
                ColorRole::Success
            } else {
                ColorRole::Danger
            };
            let stroke = egui::Stroke::new(s.radius * scale * 2.0, color32(t.palette.color(role)));
            let pts: Vec<egui::Pos2> = s
                .points
                .iter()
                .map(|p| rect.min + egui::vec2(p[0] * scale, p[1] * scale))
                .collect();
            if pts.len() == 1 {
                painter.circle_filled(pts[0], s.radius * scale, stroke.color);
            } else {
                painter.add(egui::Shape::line(pts, stroke));
            }
        }
        if let Some(pos) = response.hover_pos() {
            painter.circle_stroke(
                pos,
                self.radius * scale,
                egui::Stroke::new(
                    t.borders.hairline,
                    color32(t.palette.color(ColorRole::Accent)),
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 120;
    const H: u32 = 90;

    fn inside(x: u32, y: u32) -> bool {
        (x as f32 + 0.5 - 60.0).powi(2) + (y as f32 + 0.5 - 45.0).powi(2) < 25.0 * 25.0
    }

    /// A textured orange disc on a textured teal ground.
    fn image() -> Vec<u8> {
        let mut rgba = Vec::new();
        for y in 0..H {
            for x in 0..W {
                let t = ((x * 7 + y * 13) % 11) as i32 * 3 - 15;
                let c: [i32; 3] = if inside(x, y) {
                    [220 + t / 2, 130 + t, 40]
                } else {
                    [40, 120 + t, 150 - t]
                };
                rgba.extend(c.map(|v| v.clamp(0, 255) as u8));
                rgba.push(255);
            }
        }
        rgba
    }

    fn strokes() -> Vec<MagicCutStroke> {
        vec![
            MagicCutStroke {
                foreground: true,
                radius: 4.0,
                points: vec![[50.0, 45.0], [70.0, 45.0]],
            },
            MagicCutStroke {
                foreground: false,
                radius: 4.0,
                points: vec![[5.0, 5.0], [115.0, 5.0], [115.0, 85.0]],
            },
        ]
    }

    #[test]
    fn every_string_resolves_through_the_catalogue() {
        for key in [
            "ui.magic_cut.title",
            "ui.magic_cut.subtitle",
            "ui.magic_cut.hint",
            "ui.magic_cut.brush.foreground",
            "ui.magic_cut.brush.background",
            "ui.magic_cut.brush.size",
            "ui.magic_cut.smooth",
            "ui.magic_cut.shift",
            "ui.magic_cut.feather",
            "ui.magic_cut.output",
            "ui.magic_cut.output.selection",
            "ui.magic_cut.output.mask",
            "ui.magic_cut.output.layer",
            "ui.magic_cut.ok",
            "ui.magic_cut.preview",
            "ui.magic_cut.clear",
            "ui.magic_cut.need_foreground",
            "ui.magic_cut.need_background",
            "ui.magic_cut.no_pixels",
            "ui.magic_cut.empty",
        ] {
            assert!(!tr(key).is_empty(), "{key} is not in the catalogue");
        }
    }

    #[test]
    fn strokes_on_the_disc_and_the_ground_cut_out_the_disc() {
        let mask = cut_mask(&image(), W, H, &strokes(), &RefineParams::default()).unwrap();
        let (mut inter, mut union) = (0u32, 0u32);
        for y in 0..H {
            for x in 0..W {
                let a = mask.coverage_at(IVec2::new(x as i32, y as i32)) >= 128;
                let b = inside(x, y);
                inter += u32::from(a && b);
                union += u32::from(a || b);
            }
        }
        let iou = f64::from(inter) / f64::from(union);
        assert!(iou > 0.9, "the cut matches the disc only {iou:.3}");
    }

    #[test]
    fn a_cut_needs_a_foreground_stroke() {
        let only_background = vec![strokes().remove(1)];
        assert_eq!(
            cut_mask(&image(), W, H, &only_background, &RefineParams::default()).unwrap_err(),
            tr("ui.magic_cut.need_foreground")
        );
    }

    #[test]
    fn feathering_softens_the_edge() {
        let hard = cut_mask(&image(), W, H, &strokes(), &RefineParams::default()).unwrap();
        let soft = cut_mask(
            &image(),
            W,
            H,
            &strokes(),
            &RefineParams {
                feather_px: 6.0,
                ..RefineParams::default()
            },
        )
        .unwrap();
        let partial =
            |m: &SelectionMask| m.coverage().iter().filter(|v| **v > 0 && **v < 255).count();
        assert!(partial(&soft) > partial(&hard));
    }
    /// A layer wider than the GPU's largest texture: the window is drawn —
    /// before and after a cut repaints the preview — with a texture bounded
    /// by [`PREVIEW_MAX_SIDE`]. Uploading the layer itself panicked in egui
    /// ("maximum texture side is 8192").
    #[test]
    fn a_layer_past_the_gpu_texture_limit_draws_a_bounded_preview() {
        let (w, h) = (9000u32, 900u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x = i as u32 % w;
            *px = if (4000..5000).contains(&x) {
                [230, 120, 30, 255]
            } else {
                [20, 140, 150, 255]
            };
        }
        let mut dialog = MagicCutDialog::new(w, h, rgba);
        dialog.add_stroke(MagicCutStroke {
            foreground: true,
            radius: 80.0,
            points: vec![[4200.0, 450.0], [4800.0, 450.0]],
        });
        dialog.add_stroke(MagicCutStroke {
            foreground: false,
            radius: 80.0,
            points: vec![[100.0, 450.0], [3000.0, 450.0]],
        });
        dialog.add_stroke(MagicCutStroke {
            foreground: false,
            radius: 80.0,
            points: vec![[6000.0, 450.0], [8900.0, 450.0]],
        });
        let ctx = Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let draw = |dialog: &mut MagicCutDialog| {
            let input = egui::RawInput {
                max_texture_side: Some(8192),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                let _ = dialog.show(ctx);
            });
            let side = dialog
                .texture
                .as_ref()
                .expect("the pane uploaded its preview")
                .size();
            assert!(
                side[0] as u32 <= PREVIEW_MAX_SIDE && side[1] as u32 <= PREVIEW_MAX_SIDE,
                "preview texture {side:?}"
            );
        };
        draw(&mut dialog);
        dialog.compute().expect("the strokes cut the orange band");
        draw(&mut dialog);
        assert_eq!(dialog.preview_size(), (PREVIEW_MAX_SIDE, 51));
    }
}
