//! The Layers panel's "Remove Color Fringe…" item — card 062's edge colour
//! cleanup dialog.
//!
//! This is a separate edit from mask refinement (card 060): it RECOLOURS the
//! layer's boundary pixels toward the nearby interior ink (the classic
//! green/orange halo after a cutout) and never touches coverage. The card's
//! reversible-operation requirement is met by the shell-side landing: the
//! confirmation is ONE labelled transaction whose undo restores the exact
//! original RGB (see `menu_bridge::defringe_with`). Cancel restores by
//! construction — the dialog writes nothing.
//!
//! The preview composites the CLEANED content against the UNCHANGED baseline
//! coverage over a backdrop (black / white / checker), reusing card 060's
//! backdrop vocabulary so halos can be judged the same way.
//!
//! RECORDED caveats (mirroring row 060's, same mixed-space family):
//! - The content the host feeds is the layer's RAW store pixels
//!   (`pixels::read_layer`, canvas-indexed 1:1) — on a TRANSFORMED layer
//!   those are not the displayed pixels, so the preview aligns only at an
//!   identity layer transform; the confirmation writes through the pose-aware
//!   pixel writer either way.
//! - Radii are in DOCUMENT pixels over the pose-aware canvas-space coverage.
//! - Large canvases preview on a nearest-downscaled copy (the same
//!   [`super::refine_mask::PREVIEW_MAX_SIDE`] cap); the confirmation is
//!   always full-resolution.

use super::refine_mask::{
    composite_preview, PreviewBackground, RefineMaskDialog, PREVIEW_MAX_SIDE,
};
use crate::dialogs::action::DialogAction;
use crate::dialogs::chrome::{
    action_row, modal, resolve, Dialog, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use crate::strings::tr;
use egui::{Context, Ui};
use filters::defringe::{DefringeParams, MAX_DEFRINGE_RADIUS};

/// The parameters one confirmation bakes. A zero radius or zero strength is
/// the identity — the dialog refuses to confirm it.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct DefringeSpec {
    /// How far from the boundary the cleanup samples its interior reference.
    pub radius_px: u32,
    /// 0.0..=1.0 — how far the boundary RGB moves toward the reference.
    pub strength: f32,
    /// What the preview composites over (a preview-only choice).
    pub background: PreviewBackground,
}

impl DefringeSpec {
    /// Whether anything would change. A non-finite strength is treated as
    /// identity here so the dialog can never confirm what the engine would
    /// only refuse later.
    pub fn is_identity(&self) -> bool {
        self.radius_px == 0 || !self.strength.is_finite() || self.strength <= 0.0
    }

    /// The engine parameters this spec runs.
    pub fn params(&self) -> DefringeParams {
        DefringeParams {
            radius: self.radius_px,
            strength: self.strength,
        }
    }
}

/// The Remove Color Fringe dialog over one layer's pixels and mask coverage.
pub struct DefringeDialog {
    spec: DefringeSpec,
    /// The layer's RAW store pixels (canvas-indexed 1:1; a transformed
    /// layer's displayed appearance differs — recorded in the module doc).
    content: Vec<u8>,
    /// The mask's baseline coverage, canvas space, `width * height` bytes —
    /// read-only here: the cleanup recolours, it never regrades coverage.
    coverage: Vec<u8>,
    width: u32,
    height: u32,
    /// The preview texture, rebuilt when the parameters move.
    preview: Option<(egui::TextureHandle, [u32; 2])>,
    preview_dirty: bool,
}

impl DefringeDialog {
    /// Start from a different parameter set (the registry seeds a
    /// non-identity spec so Enter confirms).
    pub fn with_spec(
        spec: DefringeSpec,
        content: Vec<u8>,
        coverage: Vec<u8>,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            spec,
            ..Self::new(content, coverage, width, height)
        }
    }

    /// Build over the layer's RAW store pixels and the mask's baseline
    /// coverage (canvas-aligned, `width * height` samples).
    pub fn new(content: Vec<u8>, coverage: Vec<u8>, width: u32, height: u32) -> Self {
        Self {
            spec: DefringeSpec::default(),
            content,
            coverage,
            width,
            height,
            preview: None,
            preview_dirty: true,
        }
    }

    /// The cleaned content for the CURRENT parameters at FULL resolution —
    /// the exact bytes a confirmation would bake, so the preview and the
    /// confirm cannot drift.
    pub fn cleaned_content(&self) -> Vec<u8> {
        self.cleaned_content_sized(self.width, self.height, &self.content)
    }

    fn cleaned_content_sized(&self, width: u32, height: u32, content: &[u8]) -> Vec<u8> {
        let mut out = content.to_vec();
        // The engine errors only on malformed geometry, which the host never
        // produces; a preview must not panic over it either way.
        let _ = filters::defringe::defringe(
            &mut out,
            &self.coverage,
            width,
            height,
            self.spec.params(),
        );
        out
    }

    /// The preview bytes and their dimensions (downscaled on large canvases;
    /// the confirmation always runs at full resolution).
    pub fn preview_sized(&self) -> Result<(Vec<u8>, u32, u32), String> {
        let largest = self.width.max(self.height);
        if largest <= PREVIEW_MAX_SIDE {
            let coverage = self.cleaned_content();
            return Ok((
                composite_preview(
                    &coverage,
                    &self.coverage,
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
            RefineMaskDialog::downscale_rgba(&self.content, self.width, self.height, step);
        let (cov, bw, bh) =
            RefineMaskDialog::downscale_plane(&self.coverage, self.width, self.height, step);
        let mut cleaned: Vec<u8> = content;
        let _ = filters::defringe::defringe(&mut cleaned, &cov, bw, bh, self.spec.params());
        Ok((
            composite_preview(&cleaned, &cov, bw, bh, self.spec.background),
            bw,
            bh,
        ))
    }

    /// The preview bytes (the widget's pixels, pure and document-free).
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
    pub fn set_spec_for_test(&mut self, spec: DefringeSpec) {
        self.spec = spec;
        self.preview_dirty = true;
    }

    /// Draw one frame.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DialogAction> {
        let keys = DialogKeys::read(ctx);
        let mut outcome = resolve(self, keys);
        let drawn = modal(
            ctx,
            "defringe",
            self.title(),
            Some(tr("ui.defringe.subtitle")),
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
        egui::Grid::new("defringe-fields")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label(tr("ui.defringe.radius"));
                ui.add(
                    egui::Slider::new(&mut self.spec.radius_px, 0..=MAX_DEFRINGE_RADIUS)
                        .suffix(tr("ui.refine_mask.px.suffix"))
                        .text(tr("ui.defringe.radius")),
                );
                ui.end_row();

                ui.label(tr("ui.defringe.strength"));
                ui.add(
                    egui::Slider::new(&mut self.spec.strength, 0.0..=1.0)
                        .text(tr("ui.defringe.strength")),
                );
                ui.end_row();
            });

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
        if self.preview.is_none() || self.preview_dirty {
            if let Ok((rgba, pw, ph)) = self.preview_sized() {
                let img =
                    egui::ColorImage::from_rgba_unmultiplied([pw as usize, ph as usize], &rgba);
                let tex =
                    ui.ctx()
                        .load_texture("defringe-preview", img, egui::TextureOptions::NEAREST);
                self.preview = Some((tex, [pw, ph]));
                self.preview_dirty = false;
            }
        }
        if let Some((tex, [pw, ph])) = &self.preview {
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

impl Dialog for DefringeDialog {
    fn title(&self) -> &'static str {
        tr("ui.defringe.title")
    }

    fn confirm_label(&self) -> &'static str {
        tr("ui.defringe.confirm")
    }

    fn confirm(&self) -> Option<DialogAction> {
        (!self.spec.is_identity()).then_some(DialogAction::Defringe(Box::new(self.spec)))
    }

    fn blocked_reason(&self) -> Option<String> {
        self.spec
            .is_identity()
            .then(|| tr("ui.defringe.nothing.to.clean").to_string())
    }
}

impl std::fmt::Debug for DefringeDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefringeDialog")
            .field("spec", &self.spec)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn half_plane(w: u32, h: u32) -> Vec<u8> {
        let mut c = vec![0u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..w / 2 {
                c[(y * w + x) as usize] = 255;
            }
        }
        c
    }

    /// Opaque grey interior with a GREEN fringe column just inside the edge.
    fn fringed(w: u32, h: u32) -> Vec<u8> {
        let mut c = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 4;
                if x < w as usize / 2 {
                    c[i] = if x == w as usize / 2 - 1 { 20 } else { 40 };
                    c[i + 1] = if x == w as usize / 2 - 1 { 230 } else { 40 };
                    c[i + 2] = 40;
                    c[i + 3] = 255;
                }
            }
        }
        c
    }

    #[test]
    fn the_preview_shows_the_cleaned_content_over_each_backdrop() {
        let w = 16u32;
        let h = 16u32;
        let spec = DefringeSpec {
            radius_px: 3,
            strength: 1.0,
            background: PreviewBackground::White,
        };
        let dialog = DefringeDialog {
            spec,
            ..DefringeDialog::new(fringed(w, h), half_plane(w, h), w, h)
        };
        let rgba = dialog.preview_rgba().unwrap();
        let at = |x: usize, y: usize| (y * w as usize + x) * 4;
        // Deep interior keeps the ink (grey 40)...
        assert_eq!(rgba[at(4, 8)], 40);
        // ...and the fringe column is pulled toward it: the green channel
        // drops from 230 to well under half. Under a NO-OP preview it would
        // be 230 blended by the coverage (255 → 230 straight through).
        assert!(
            rgba[at(7, 8) + 1] < 128,
            "the fringe is visibly reduced: {}",
            rgba[at(7, 8) + 1]
        );
        // The concealed side shows the white backdrop.
        assert_eq!(rgba[at(13, 8)], 255);
    }

    #[test]
    fn the_preview_composites_by_the_unchanged_baseline_coverage() {
        // The cleanup recolours; the preview's soft edge must sit where the
        // BASELINE coverage's ramp is — not where a refined mask would put it.
        let w = 16u32;
        let h = 16u32;
        let spec = DefringeSpec {
            radius_px: 3,
            strength: 1.0,
            background: PreviewBackground::Black,
        };
        let dialog = DefringeDialog {
            spec,
            ..DefringeDialog::new(fringed(w, h), half_plane(w, h), w, h)
        };
        let rgba = dialog.preview_rgba().unwrap();
        // x=8 is fully concealed by the baseline: black, whatever the cleanup did.
        assert_eq!(rgba[(8 * w as usize + 8) * 4], 0);
        assert_eq!(rgba[(8 * w as usize + 8) * 4 + 1], 0);
    }

    #[test]
    fn an_identity_spec_confirms_nothing_and_previewing_never_mutates_it() {
        let dialog = DefringeDialog::new(fringed(8, 8), half_plane(8, 8), 8, 8);
        assert!(dialog.confirm().is_none(), "identity confirms nothing");
        assert!(dialog.blocked_reason().is_some());
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
        let spec = DefringeSpec {
            radius_px: 4,
            strength: 0.7,
            background: PreviewBackground::Checker,
        };
        let dialog = DefringeDialog {
            spec,
            ..DefringeDialog::new(fringed(8, 8), half_plane(8, 8), 8, 8)
        };
        match dialog.confirm() {
            Some(DialogAction::Defringe(boxed)) => assert_eq!(*boxed, spec),
            other => panic!("the confirmation carries the spec: {other:?}"),
        }
    }
}
