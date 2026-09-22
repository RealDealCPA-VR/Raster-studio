//! The Layers panel's "Refine Mask…" item (a top-level Layer item beside the
//! Layer Mask submenu) — card 060's edge-refinement dialog.
//!
//! The dialog is deliberately labelled edge refinement: feather, shift
//! (expand/contract), smooth, and a contrast curve over the mask's existing
//! coverage — morphology over what is already there, never automatic subject
//! recognition. The preview composites the layer's pixels against the
//! REFINED coverage over a user-chosen backdrop (black / white / checker)
//! entirely inside the dialog: the document is not touched until the user
//! confirms, and confirming emits one [`DialogAction::RefineMask`] the shell
//! turns into one undoable transaction. Cancel restores the baseline by
//! construction — there is nothing to restore, because nothing was written.
//!
//! RECORDED approximations and caveats (ledger row 060 / T043 remainder):
//! - The content the host feeds is the layer's RAW store pixels
//!   (`pixels::read_layer`, canvas-indexed 1:1) — on a TRANSFORMED layer
//!   those are not the displayed pixels, so the preview composites coherently
//!   only at an identity layer transform; the confirmation writes through the
//!   pose-aware writer either way.
//! - The preview ignores the layer's opacity and blend mode (raw store
//!   bytes are composited at full strength).
//! - Radii are in DOCUMENT pixels: the pipeline runs over the pose-aware
//!   canvas-space coverage and the confirmation writes back through the
//!   pose-aware writer, so "feather 4 px" means 4 document pixels of ramp —
//!   the same unit `selection::modify`'s morphology speaks.
//! - On a canvas larger than [`PREVIEW_MAX_SIDE`] the preview runs the whole
//!   pipeline (morphology included) on a nearest-downscaled copy, so preview
//!   radii act in DOWNSCALED pixels and read stronger than the confirm's
//!   full-resolution result; the step is also coarse (just past a multiple of
//!   the cap the preview drops to about half the cap). The confirmation is
//!   always exact.
//! - The checkerboard backdrop is an approximation of the editor's (fixed
//!   neutrals at one-preview-pixel cells, not the theme's 8px cells).

use crate::dialogs::action::DialogAction;
use crate::dialogs::chrome::{
    action_row, modal, resolve, Dialog, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use crate::strings::tr;
use egui::{Context, Ui};
use selection::RefineParams;

/// What the preview composites the masked content over.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PreviewBackground {
    /// Solid black — makes light halos obvious.
    Black,
    /// Solid white — makes dark halos obvious.
    White,
    /// An approximation of the editor's checkerboard (fixed neutrals at
    /// one-preview-pixel cells) — transparency, the export's actual look.
    #[default]
    Checker,
}

impl PreviewBackground {
    /// The labels, in swatch order.
    pub const ALL: &'static [PreviewBackground] = &[
        PreviewBackground::Black,
        PreviewBackground::White,
        PreviewBackground::Checker,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            PreviewBackground::Black => tr("ui.refine_mask.background.black"),
            PreviewBackground::White => tr("ui.refine_mask.background.white"),
            PreviewBackground::Checker => tr("ui.refine_mask.background.checker"),
        }
    }

    /// The backdrop this choice paints under the masked content.
    fn backdrop(self) -> [u8; 3] {
        match self {
            PreviewBackground::Black => [0, 0, 0],
            PreviewBackground::White => [255, 255, 255],
            // The checkerboard's light square (the dark square is drawn by
            // the checker pattern below).
            PreviewBackground::Checker => [200, 200, 200],
        }
    }
}

/// The refinement parameters one confirmation bakes.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct RefineMaskSpec {
    /// Soften the edge: the ramp reaches this many pixels to each side.
    pub feather_px: f32,
    /// Shift the edge: positive expands, negative contracts.
    pub shift_px: i32,
    /// Round the boundary (removes spikes and stair-steps).
    pub smooth_px: u32,
    /// Push the coverage ramp away from the 50% midpoint, 0.0..=1.0.
    pub contrast: f32,
    /// What the preview composites over (a preview-only choice; it never
    /// reaches the mask).
    pub background: PreviewBackground,
}

impl RefineMaskSpec {
    /// The morphology parameters this spec runs, in the pipeline's own units.
    pub fn params(&self) -> RefineParams {
        RefineParams {
            feather_px: self.feather_px,
            shift_px: self.shift_px,
            smooth_px: self.smooth_px,
            contrast: self.contrast,
        }
    }

    /// Whether anything would change: an all-identity spec has nothing to do.
    pub fn is_identity(&self) -> bool {
        self.feather_px <= 0.0 && self.shift_px == 0 && self.smooth_px == 0 && self.contrast <= 0.0
    }
}

/// The Refine Mask dialog over one layer's pixels and mask coverage.
pub struct RefineMaskDialog {
    spec: RefineMaskSpec,
    /// The layer's RAW store pixels (canvas-indexed 1:1; a transformed
    /// layer's displayed appearance differs — recorded in the module doc).
    content: Vec<u8>,
    /// The mask's baseline coverage, canvas space, `width * height` bytes.
    baseline: Vec<u8>,
    width: u32,
    height: u32,
    /// The preview texture, rebuilt when the parameters move.
    preview: Option<(egui::TextureHandle, [u32; 2])>,
    /// Set whenever the spec moves, so the next frame rebuilds the preview.
    preview_dirty: bool,
}

impl RefineMaskDialog {
    /// Start from a different parameter set (the dialog registry seeds a
    /// non-identity spec so Enter confirms).
    pub fn with_spec(
        spec: RefineMaskSpec,
        content: Vec<u8>,
        baseline: Vec<u8>,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            spec,
            ..Self::new(content, baseline, width, height)
        }
    }

    /// Build over the layer's RAW store pixels and the mask's baseline
    /// coverage (canvas-aligned, `width * height` samples).
    pub fn new(content: Vec<u8>, baseline: Vec<u8>, width: u32, height: u32) -> Self {
        Self {
            spec: RefineMaskSpec::default(),
            content,
            baseline,
            width,
            height,
            preview: None,
            preview_dirty: true,
        }
    }

    /// The refined coverage for the CURRENT parameters at FULL resolution —
    /// the exact bytes a confirmation would bake, so the preview and the
    /// confirm cannot drift.
    pub fn refined_coverage(&self) -> Result<Vec<u8>, String> {
        self.refined_coverage_sized(self.width, self.height, &self.baseline)
    }

    /// The pipeline over an arbitrary-size copy of the baseline (the preview
    /// runs it on a downscaled buffer on large canvases; the confirm always
    /// runs full resolution), canvas-aligned by sampling.
    fn refined_coverage_sized(
        &self,
        width: u32,
        height: u32,
        baseline: &[u8],
    ) -> Result<Vec<u8>, String> {
        let mask =
            editor_core::SelectionMask::new(glam::IVec2::ZERO, width, height, baseline.to_vec())
                .map_err(|e| e.to_string())?;
        let refined =
            selection::refine_mask(&mask, &self.spec.params()).map_err(|e| e.to_string())?;
        // The refined mask's rect is its own (feathering can grow it): align
        // the result back to CANVAS space by sampling, so the preview and the
        // confirm bake the same canvas-aligned bytes.
        let mut out = vec![0u8; (width * height) as usize];
        for y in 0..height {
            for x in 0..width {
                out[(y * width + x) as usize] =
                    refined.coverage_at(glam::IVec2::new(x as i32, y as i32));
            }
        }
        Ok(out)
    }

    /// Nearest-neighbour downscale of a byte plane by an integer step.
    pub(crate) fn downscale_plane(plane: &[u8], w: u32, h: u32, step: u32) -> (Vec<u8>, u32, u32) {
        let dw = (w / step).max(1);
        let dh = (h / step).max(1);
        let mut out = vec![0u8; (dw * dh) as usize];
        for y in 0..dh {
            for x in 0..dw {
                let sx = (x * step).min(w - 1) as usize;
                let sy = (y * step).min(h - 1) as usize;
                out[(y * dw + x) as usize] = plane[sy * w as usize + sx];
            }
        }
        (out, dw, dh)
    }

    /// Nearest-neighbour downscale of an RGBA buffer by an integer step.
    pub(crate) fn downscale_rgba(rgba: &[u8], w: u32, h: u32, step: u32) -> (Vec<u8>, u32, u32) {
        let dw = (w / step).max(1);
        let dh = (h / step).max(1);
        let mut out = vec![0u8; (dw * dh * 4) as usize];
        for y in 0..dh {
            for x in 0..dw {
                let sx = (x * step).min(w - 1) as usize;
                let sy = (y * step).min(h - 1) as usize;
                for k in 0..4 {
                    out[(y * dw + x) as usize * 4 + k] = rgba[(sy * w as usize + sx) * 4 + k];
                }
            }
        }
        (out, dw, dh)
    }

    /// The preview bytes and their dimensions (downscaled on large canvases
    /// so the widget and the per-slider-tick recompute stay bounded; the
    /// confirmation always runs at full resolution).
    pub fn preview_sized(&self) -> Result<(Vec<u8>, u32, u32), String> {
        let largest = self.width.max(self.height);
        if largest <= PREVIEW_MAX_SIDE {
            let coverage = self.refined_coverage()?;
            return Ok((
                composite_preview(
                    &self.content,
                    &coverage,
                    self.width,
                    self.height,
                    self.spec.background,
                ),
                self.width,
                self.height,
            ));
        }
        let step = largest.div_ceil(PREVIEW_MAX_SIDE);
        let (content, _cw, _ch) =
            Self::downscale_rgba(&self.content, self.width, self.height, step);
        let (baseline, bw, bh) =
            Self::downscale_plane(&self.baseline, self.width, self.height, step);
        let coverage = self.refined_coverage_sized(bw, bh, &baseline)?;
        Ok((
            composite_preview(&content, &coverage, bw, bh, self.spec.background),
            bw,
            bh,
        ))
    }

    /// Composite the layer against the refined coverage over the chosen
    /// backdrop — the preview bytes, pure and document-free.
    pub fn preview_rgba(&self) -> Result<Vec<u8>, String> {
        self.preview_sized().map(|(rgba, _, _)| rgba)
    }

    /// Test seam (cross-crate: the shell's tests drive the host-fed dialog):
    /// one raw content pixel.
    pub fn content_pixel_for_test(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y * self.width + x) as usize * 4;
        [
            self.content.get(i).copied().unwrap_or(0),
            self.content.get(i + 1).copied().unwrap_or(0),
            self.content.get(i + 2).copied().unwrap_or(0),
            self.content.get(i + 3).copied().unwrap_or(0),
        ]
    }

    /// Test seam: swap the parameter set (and re-mark the preview stale).
    pub fn set_spec_for_test(&mut self, spec: RefineMaskSpec) {
        self.spec = spec;
        self.preview_dirty = true;
    }

    /// Draw one frame.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DialogAction> {
        let keys = DialogKeys::read(ctx);
        let mut outcome = resolve(self, keys);
        let drawn = modal(
            ctx,
            "refine-mask",
            self.title(),
            Some(tr("ui.refine_mask.subtitle")),
            DialogWidth::Wide,
            |ui| self.body(ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => DialogOutcome::Open,
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut Ui) -> Option<DialogButton> {
        let spec_before = self.spec;
        // The four morphological controls, in pipeline order.
        egui::Grid::new("refine-fields")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label(tr("ui.refine_mask.feather"));
                ui.add(
                    egui::Slider::new(&mut self.spec.feather_px, 0.0..=64.0)
                        .suffix(tr("ui.refine_mask.px.suffix"))
                        .text(tr("ui.refine_mask.feather")),
                );
                ui.end_row();

                ui.label(tr("ui.refine_mask.shift"));
                ui.add(
                    egui::Slider::new(&mut self.spec.shift_px, -64..=64)
                        .suffix(tr("ui.refine_mask.px.suffix"))
                        .text(tr("ui.refine_mask.shift")),
                );
                ui.end_row();

                ui.label(tr("ui.refine_mask.smooth"));
                ui.add(
                    egui::Slider::new(&mut self.spec.smooth_px, 0..=32)
                        .suffix(tr("ui.refine_mask.px.suffix"))
                        .text(tr("ui.refine_mask.smooth")),
                );
                ui.end_row();

                ui.label(tr("ui.refine_mask.contrast"));
                ui.add(
                    egui::Slider::new(&mut self.spec.contrast, 0.0..=1.0)
                        .text(tr("ui.refine_mask.contrast")),
                );
                ui.end_row();
            });

        // The backdrop swatches: the same mask geometry against three
        // backdrops is how halo problems get seen.
        ui.horizontal(|ui| {
            ui.label(tr("ui.refine_mask.background.label"));
            for bg in PreviewBackground::ALL {
                if ui
                    .selectable_label(self.spec.background == *bg, bg.label())
                    .clicked()
                {
                    self.spec.background = *bg;
                }
            }
        });

        if spec_before != self.spec {
            self.preview_dirty = true;
        }
        // The preview: content masked by the refined coverage over the
        // backdrop, rebuilt only when the parameters move.
        if self.preview.is_none() || self.preview_dirty {
            if let Ok((rgba, pw, ph)) = self.preview_sized() {
                let img =
                    egui::ColorImage::from_rgba_unmultiplied([pw as usize, ph as usize], &rgba);
                let tex = ui.ctx().load_texture(
                    "refine-mask-preview",
                    img,
                    egui::TextureOptions::NEAREST,
                );
                self.preview = Some((tex, [pw, ph]));
                self.preview_dirty = false;
            }
        }
        if let Some((tex, [pw, ph])) = &self.preview {
            // Fit within the dialog: the longest side capped, aspect kept.
            let scale = (PREVIEW_MAX_SIDE as f32 / (*pw).max(*ph) as f32).min(2.0);
            let size = egui::vec2(*pw as f32 * scale, *ph as f32 * scale);
            ui.image((tex.id(), size));
        }

        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &[],
        )
    }
}

/// The preview's bounded size: a canvas whose largest side exceeds this is
/// nearest-downscaled (content, baseline, and morphology together) before the
/// pipeline runs, so the widget and the per-slider-tick recompute stay
/// interactive. The confirmation always runs at full resolution.
pub(crate) const PREVIEW_MAX_SIDE: u32 = 256;

/// Composite `content` against `coverage` over `background`'s backdrop: the
/// coverage is the content's alpha ramp, straight-alpha over the backdrop.
pub fn composite_preview(
    content: &[u8],
    coverage: &[u8],
    width: u32,
    height: u32,
    background: PreviewBackground,
) -> Vec<u8> {
    let n = (width as usize) * (height as usize);
    let mut out = vec![0u8; n * 4];
    for i in 0..n {
        let c = coverage.get(i).copied().unwrap_or(0) as f32 / 255.0;
        let (r, g, b, a) = (
            content.get(i * 4).copied().unwrap_or(0),
            content.get(i * 4 + 1).copied().unwrap_or(0),
            content.get(i * 4 + 2).copied().unwrap_or(0),
            content.get(i * 4 + 3).copied().unwrap_or(0),
        );
        let src_a = a as f32 / 255.0 * c;
        let backdrop = background.backdrop();
        let (br, bg, bb) = (backdrop[0], backdrop[1], backdrop[2]);
        // The checkerboard: two-tone under the mask so transparency reads.
        let (br, bg, bb) = if background == PreviewBackground::Checker
            && (i as u32 / width.max(1) + i as u32 % width.max(1)).is_multiple_of(2)
        {
            (
                br.saturating_sub(40),
                bg.saturating_sub(40),
                bb.saturating_sub(40),
            )
        } else {
            (br, bg, bb)
        };
        let px = i * 4;
        out[px] = (r as f32 * src_a + br as f32 * (1.0 - src_a)).round() as u8;
        out[px + 1] = (g as f32 * src_a + bg as f32 * (1.0 - src_a)).round() as u8;
        out[px + 2] = (b as f32 * src_a + bb as f32 * (1.0 - src_a)).round() as u8;
        out[px + 3] = 255;
    }
    out
}

impl Dialog for RefineMaskDialog {
    fn title(&self) -> &'static str {
        tr("ui.refine_mask.title")
    }

    fn confirm_label(&self) -> &'static str {
        tr("ui.refine_mask.confirm")
    }

    fn confirm(&self) -> Option<DialogAction> {
        (!self.spec.is_identity()).then_some(DialogAction::RefineMask(Box::new(self.spec)))
    }

    fn blocked_reason(&self) -> Option<String> {
        self.spec
            .is_identity()
            .then(|| tr("ui.refine_mask.nothing.to.refine").to_string())
    }
}

impl std::fmt::Debug for RefineMaskDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefineMaskDialog")
            .field("spec", &self.spec)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hard half-plane: left half revealed, right half concealed.
    fn half_plane(w: u32, h: u32) -> Vec<u8> {
        let mut c = vec![0u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..w / 2 {
                c[(y * w + x) as usize] = 255;
            }
        }
        c
    }

    fn opaque(w: u32, h: u32) -> Vec<u8> {
        [200u8, 120, 40, 255].repeat((w * h * 4) as usize / 4)
    }

    #[test]
    fn the_preview_shows_the_mask_geometry_over_each_backdrop() {
        let w = 16u32;
        let h = 16u32;
        for bg in PreviewBackground::ALL {
            let rgba = RefineMaskDialog {
                spec: RefineMaskSpec {
                    background: *bg,
                    ..Default::default()
                },
                ..RefineMaskDialog::new(opaque(w, h), half_plane(w, h), w, h)
            }
            .preview_rgba()
            .unwrap();
            let at = |x: usize, y: usize| (y * w as usize + x) * 4;
            // The revealed side shows the content's ink...
            assert_eq!(rgba[at(2, 8)], 200, "{bg:?}: revealed keeps the ink");
            // ...the concealed side shows the backdrop...
            let expected = match bg {
                PreviewBackground::Black => 0,
                PreviewBackground::White => 255,
                PreviewBackground::Checker => 200, // the light square at even parity
            };
            assert_eq!(
                rgba[at(13, 8)],
                expected,
                "{bg:?}: concealed shows the backdrop"
            );
            // ...and the preview is opaque (the backdrop IS the alpha).
            assert_eq!(rgba[at(13, 8) + 3], 255);
        }
    }

    #[test]
    fn the_preview_geometry_matches_the_refined_coverage() {
        // Feather the hard edge: the preview's soft band must sit exactly
        // where the refined coverage's ramp is.
        let w = 32u32;
        let h = 16u32;
        let spec = RefineMaskSpec {
            feather_px: 4.0,
            ..Default::default()
        };
        let dialog = RefineMaskDialog {
            spec,
            ..RefineMaskDialog::new(opaque(w, h), half_plane(w, h), w, h)
        };
        let coverage = dialog.refined_coverage().unwrap();
        let rgba = dialog.preview_rgba().unwrap();
        let at = |x: usize, y: usize| (y * w as usize + x) * 4;
        // Far from the edge the geometry is unchanged...
        assert_eq!(rgba[at(4, 8)], 200);
        assert_eq!(coverage[8 * w as usize + 4], 255);
        // ...and the ramp crosses the 50% midpoint at the old boundary: the
        // pixel LEFT of the edge is above mid-grey, the pixel RIGHT of it
        // below (the coverage pixel [16,17) lies entirely past the edge).
        let left = coverage[8 * w as usize + 15];
        let right = coverage[8 * w as usize + 16];
        assert!(left > 128, "the revealed side of the ramp: {left}");
        assert!(right < 128, "the concealed side of the ramp: {right}");
        // The preview blends the ink over the backdrop BY the refined
        // coverage: expected r = ink*c + backdrop*(1-c) at each ramp pixel.
        let blend = |x: usize, cov: u8| -> i32 {
            let c = cov as f32 / 255.0;
            let backdrop = if (x as u32 + 8).is_multiple_of(2) {
                160.0
            } else {
                200.0
            };
            (200.0 * c + backdrop * (1.0 - c)).round() as i32
        };
        assert_eq!(
            rgba[at(16, 8)] as i32,
            blend(16, right),
            "the preview blends exactly by the refined coverage at x=16"
        );
        assert_eq!(
            rgba[at(15, 8)] as i32,
            blend(15, left),
            "the preview blends exactly by the refined coverage at x=15"
        );
    }

    #[test]
    fn the_preview_composites_the_raw_content_by_the_refined_coverage() {
        let dialog = RefineMaskDialog::new(opaque(8, 8), half_plane(8, 8), 8, 8);
        assert!(
            dialog.confirm().is_none(),
            "an all-identity spec confirms nothing"
        );
        assert!(
            dialog.blocked_reason().is_some(),
            "the disabled button says why"
        );
        // Cancel writes nothing BY CONSTRUCTION: this module's only output is
        // the DialogAction a confirmation emits — there is no code path
        // from the dialog to the document. Pin the guarantee the card names:
        // drawing previews and cancelling leaves `confirm()` exactly as it
        // was (the spec is Copy and unchanged by previewing).
        let before = dialog.confirm();
        let _ = dialog.preview_rgba().unwrap();
        assert_eq!(
            dialog.confirm(),
            before,
            "previewing never mutates the spec"
        );
    }

    #[test]
    fn a_confirmed_spec_round_trips_through_the_action() {
        let spec = RefineMaskSpec {
            feather_px: 4.0,
            shift_px: -2,
            smooth_px: 1,
            contrast: 0.5,
            background: PreviewBackground::White,
        };
        let dialog = RefineMaskDialog {
            spec,
            ..RefineMaskDialog::new(opaque(8, 8), half_plane(8, 8), 8, 8)
        };
        match dialog.confirm() {
            Some(DialogAction::RefineMask(boxed)) => assert_eq!(*boxed, spec),
            other => panic!("the confirmation carries the spec: {other:?}"),
        }
    }
}
