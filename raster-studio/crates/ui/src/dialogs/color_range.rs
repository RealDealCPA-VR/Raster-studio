//! Select ▸ Color Range… — pick a colour, set how far the selection reaches
//! from it, and watch the selection form before committing it.
//!
//! Photopea's dialog: a sampled colour (the eyedropper, or a click on the
//! preview itself), a Fuzziness slider (0–200, the same units the falloff
//! reads), Invert, and a preview that shows either the *selection* — coverage
//! as grayscale, white selected, black not — or the image it is being made
//! from. The preview runs over a bounded copy of the active layer (at most
//! [`PREVIEW_MAX_SIDE`] on the long side), so a slider tick costs a thumbnail,
//! not a canvas.
//!
//! The preview and the commit cannot drift: both go through
//! [`ColorRangeSpec::mask`], the one function that turns a spec and an RGBA
//! buffer into coverage. The dialog runs it over the preview; the shell runs it
//! over the full-resolution layer when the confirmed spec reaches the
//! `ColorRange` menu arm, which sets the selection as one undoable
//! `SetSelection` step.
//!
//! No [`super::chrome::Dialog`] impl, for the reason Trim has none: the
//! confirmation is a [`ColorRangeSpec`], not a
//! [`super::action::DialogAction`], and the shell parks it for the menu arm.

use editor_core::SelectionMask;
use egui::{Context, Sense};

use super::chrome::{
    action_row_with_extras, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::color_picker::{Eyedropper, ScreenSampler};
use super::controls::{checkbox_row, swatch_readonly};
use super::{ids, sizes};
use crate::strings::tr;

/// The preview's bounded long side, in pixels. A larger layer is
/// nearest-downscaled once, when the dialog opens.
pub const PREVIEW_MAX_SIDE: u32 = 256;

/// The top of the Fuzziness scale (Photopea's range).
pub const MAX_FUZZINESS: u32 = 200;

/// The Fuzziness the dialog opens at — Photopea's, and the value the old
/// dialog-less route used (`selection::ColorRangeOptions::default`).
pub const DEFAULT_FUZZINESS: u32 = 40;

/// A confirmed Color Range: the colour, the reach, and whether to invert.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ColorRangeSpec {
    /// Straight-alpha RGBA8. Alpha is carried but not compared: the range is
    /// a question about colour.
    pub color: [u8; 4],
    /// 0..=[`MAX_FUZZINESS`]: the colour distance, in 1/255 steps, at which
    /// coverage falls to zero.
    pub fuzziness: u32,
    /// Select everything *but* the range.
    pub invert: bool,
}

impl ColorRangeSpec {
    /// A spec over `color` at the opening fuzziness.
    pub fn new(color: [u8; 4]) -> Self {
        Self {
            color,
            fuzziness: DEFAULT_FUZZINESS,
            invert: false,
        }
    }

    /// The falloff options the `selection` crate reads.
    pub fn options(&self) -> selection::ColorRangeOptions {
        selection::ColorRangeOptions {
            fuzziness: self.fuzziness.min(MAX_FUZZINESS) as f32 / 255.0,
            ..Default::default()
        }
    }

    /// Coverage over an RGBA8 buffer `width × height` at the origin: the
    /// preview's source and the committed selection both come from here.
    pub fn mask(&self, rgba: &[u8], width: u32, height: u32) -> Result<SelectionMask, String> {
        let image =
            selection::ImageBuffer::from_rgba8(glam::IVec2::ZERO, width, height, rgba.to_vec())
                .map_err(|e| e.to_string())?;
        let mut target = self.color;
        target[3] = u8::MAX;
        let mask = selection::color_range(&image.view(), target, &self.options())
            .map_err(|e| e.to_string())?;
        if !self.invert {
            return Ok(mask);
        }
        let canvas = selection::Rect::new(
            glam::IVec2::ZERO,
            glam::IVec2::new(width as i32, height as i32),
        );
        selection::invert(&mask, canvas).map_err(|e| e.to_string())
    }

    /// The coverage as one byte per pixel over `width × height` — what the
    /// preview draws as grayscale.
    pub fn coverage(&self, rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
        let mask = self.mask(rgba, width, height)?;
        let mut out = vec![0u8; (width as usize) * (height as usize)];
        for y in 0..height {
            for x in 0..width {
                out[(y * width + x) as usize] =
                    mask.coverage_at(glam::IVec2::new(x as i32, y as i32));
            }
        }
        Ok(out)
    }
}

/// What the preview shows.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum ColorRangeView {
    /// The selection as grayscale: white selected, black not.
    #[default]
    Selection,
    /// The image the range is sampled from.
    Image,
}

impl ColorRangeView {
    pub const ALL: [ColorRangeView; 2] = [ColorRangeView::Selection, ColorRangeView::Image];

    pub fn label(self) -> &'static str {
        match self {
            ColorRangeView::Selection => tr("ui.color_range.view.selection"),
            ColorRangeView::Image => tr("ui.color_range.view.image"),
        }
    }
}

/// Select ▸ Color Range….
pub struct ColorRangeDialog {
    spec: ColorRangeSpec,
    /// The bounded preview source: straight RGBA8, `pw × ph`.
    preview: Vec<u8>,
    pw: u32,
    ph: u32,
    view: ColorRangeView,
    eyedropper: Eyedropper,
    texture: Option<egui::TextureHandle>,
    /// Set whenever the spec or the view moves, so the next frame rebuilds
    /// the texture.
    dirty: bool,
}

impl std::fmt::Debug for ColorRangeDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColorRangeDialog")
            .field("spec", &self.spec)
            .field("preview", &(self.pw, self.ph))
            .field("view", &self.view)
            .field("eyedropper", &self.eyedropper)
            .finish()
    }
}

impl ColorRangeDialog {
    /// Over `rgba` (`width × height`, straight RGBA8 — the active layer),
    /// starting from `color` (the foreground). The buffer is downscaled to
    /// the bounded preview here and not kept at full size.
    pub fn new(color: [u8; 4], rgba: &[u8], width: u32, height: u32) -> Self {
        let (preview, pw, ph) = bounded_preview(rgba, width, height);
        Self {
            spec: ColorRangeSpec::new(color),
            preview,
            pw,
            ph,
            view: ColorRangeView::Selection,
            eyedropper: Eyedropper::Idle,
            texture: None,
            dirty: true,
        }
    }

    pub fn spec(&self) -> ColorRangeSpec {
        self.spec
    }

    /// The bounded preview's size.
    pub fn preview_size(&self) -> (u32, u32) {
        (self.pw, self.ph)
    }

    pub fn set_fuzziness(&mut self, fuzziness: u32) {
        self.spec.fuzziness = fuzziness.min(MAX_FUZZINESS);
        self.dirty = true;
    }

    pub fn set_invert(&mut self, invert: bool) {
        self.spec.invert = invert;
        self.dirty = true;
    }

    pub fn set_color(&mut self, color: [u8; 4]) {
        self.spec.color = color;
        self.dirty = true;
    }

    pub fn view(&self) -> ColorRangeView {
        self.view
    }

    pub fn set_view(&mut self, view: ColorRangeView) {
        self.view = view;
        self.dirty = true;
    }

    pub fn eyedropper(&self) -> Eyedropper {
        self.eyedropper
    }

    pub fn arm_eyedropper(&mut self) {
        self.eyedropper = Eyedropper::Armed;
    }

    /// Take the colour of preview pixel `(x, y)` — a click on the preview.
    pub fn sample_preview(&mut self, x: u32, y: u32) -> bool {
        if x >= self.pw || y >= self.ph {
            return false;
        }
        let i = ((y * self.pw + x) * 4) as usize;
        let Some(px) = self.preview.get(i..i + 4) else {
            return false;
        };
        self.set_color([px[0], px[1], px[2], px[3]]);
        self.eyedropper = Eyedropper::Idle;
        true
    }

    /// Take a screen sample through the shell's sampler. A no-op unless the
    /// eyedropper is armed, and it disarms whether or not the read worked —
    /// an eyedropper that stays up after a failed read traps every click.
    pub fn sample_screen(&mut self, screen_pos: [f32; 2], sampler: &dyn ScreenSampler) -> bool {
        if self.eyedropper != Eyedropper::Armed {
            return false;
        }
        self.eyedropper = Eyedropper::Idle;
        match sampler.sample(screen_pos) {
            Some(rgba) => {
                self.set_color(rgba.map(super::controls::to_byte));
                true
            }
            None => false,
        }
    }

    /// The preview's selection coverage, one byte per preview pixel — the
    /// same function the commit runs, over the bounded source.
    pub fn preview_coverage(&self) -> Result<Vec<u8>, String> {
        self.spec.coverage(&self.preview, self.pw, self.ph)
    }

    /// The RGBA bytes the preview draws for the current view.
    pub fn preview_rgba(&self) -> Result<Vec<u8>, String> {
        match self.view {
            ColorRangeView::Image => Ok(self.preview.clone()),
            ColorRangeView::Selection => Ok(self
                .preview_coverage()?
                .into_iter()
                .flat_map(|c| [c, c, c, u8::MAX])
                .collect()),
        }
    }

    pub fn title(&self) -> &'static str {
        tr("ui.color_range.title")
    }

    /// Never blocked: every colour and every fuzziness in range is a
    /// selection, even an empty one.
    pub fn blocked_reason(&self) -> Option<String> {
        None
    }

    pub fn confirm(&self) -> Option<ColorRangeSpec> {
        Some(self.spec)
    }

    /// Escape and Enter, without drawing. Escape leaves an armed eyedropper
    /// before it leaves the dialog.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<ColorRangeSpec> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm {
            if let Some(spec) = self.confirm() {
                return DialogOutcome::Confirmed(spec);
            }
        }
        DialogOutcome::Open
    }

    /// Draw one frame and fold the keyboard, the eyedropper and the action
    /// row into one outcome.
    pub fn show(
        &mut self,
        ctx: &Context,
        sampler: Option<&dyn ScreenSampler>,
    ) -> DialogOutcome<ColorRangeSpec> {
        let keys = DialogKeys::read(ctx);
        if keys.cancel && self.eyedropper == Eyedropper::Armed {
            self.eyedropper = Eyedropper::Idle;
            return DialogOutcome::Open;
        }
        if self.eyedropper == Eyedropper::Armed {
            match sampler {
                Some(sampler) => {
                    let pressed = ctx.input(|i| {
                        i.pointer
                            .primary_pressed()
                            .then(|| i.pointer.interact_pos())
                            .flatten()
                    });
                    if let Some(pos) = pressed {
                        self.sample_screen([pos.x, pos.y], sampler);
                    }
                }
                None => self.eyedropper = Eyedropper::Idle,
            }
        }
        let mut outcome = self.resolve(keys);
        let has_sampler = sampler.is_some();
        let drawn = modal(
            ctx,
            "color-range",
            self.title(),
            Some(tr("ui.color_range.subtitle")),
            DialogWidth::Standard,
            |ui| self.body(ui, has_sampler),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => {
                    self.arm_eyedropper();
                    DialogOutcome::Open
                }
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui, has_sampler: bool) -> Option<DialogButton> {
        let before = self.spec;
        ui.horizontal(|ui| {
            swatch_readonly(
                ui,
                ids::color_range_color(),
                self.spec.color.map(super::controls::from_byte),
                sizes::swatch(),
            );
            caption(ui, tr("ui.color_range.sampled.colour"));
        });
        let mut fuzziness = self.spec.fuzziness;
        design::inspector_field(ui, tr("ui.color_range.fuzziness"), |ui| {
            ui.push_id(ids::color_range_fuzziness(), |ui| {
                ui.add(egui::Slider::new(&mut fuzziness, 0..=MAX_FUZZINESS));
            });
        });
        if fuzziness != self.spec.fuzziness {
            self.set_fuzziness(fuzziness);
        }
        let mut invert = self.spec.invert;
        if checkbox_row(ui, tr("ui.color_range.invert"), &mut invert).changed() {
            self.set_invert(invert);
        }
        ui.horizontal(|ui| {
            for view in ColorRangeView::ALL {
                if ui
                    .selectable_label(self.view == view, view.label())
                    .clicked()
                    && self.view != view
                {
                    self.set_view(view);
                }
            }
        });
        if before != self.spec {
            self.dirty = true;
        }
        self.preview_widget(ui);
        if self.eyedropper == Eyedropper::Armed {
            caption(ui, tr("ui.color_range.click.to.sample"));
        } else {
            caption(ui, tr("ui.color_range.click.preview"));
        }
        let eyedropper_blocked =
            (!has_sampler).then(|| tr("ui.color_picker.this.window.cannot.read.screen.pixels"));
        action_row_with_extras(
            ui,
            tr("ui.color_range.select"),
            self.blocked_reason().as_deref(),
            &[(tr("ui.color_range.eyedropper"), eyedropper_blocked)],
        )
    }

    /// The preview image: rebuilt when the spec or the view moved, drawn at a
    /// fitted size, and clickable — a click samples the pixel under it.
    fn preview_widget(&mut self, ui: &mut egui::Ui) {
        if self.texture.is_none() || self.dirty {
            if let Ok(rgba) = self.preview_rgba() {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [self.pw as usize, self.ph as usize],
                    &rgba,
                );
                self.texture = Some(ui.ctx().load_texture(
                    "color-range-preview",
                    image,
                    egui::TextureOptions::NEAREST,
                ));
                self.dirty = false;
            }
        }
        let Some(texture) = &self.texture else {
            return;
        };
        let long = sizes::filter_preview_width();
        let scale = long / self.pw.max(self.ph).max(1) as f32;
        let size = egui::Vec2::new(self.pw as f32 * scale, self.ph as f32 * scale);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let response = ui.interact(rect, ids::color_range_preview(), Sense::click());
        egui::Image::new((texture.id(), size)).paint_at(ui, rect);
        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let local = pos - rect.min;
                let x = (local.x / scale).floor();
                let y = (local.y / scale).floor();
                if x >= 0.0 && y >= 0.0 {
                    self.sample_preview(x as u32, y as u32);
                }
            }
        }
    }
}

/// Nearest-downscale `rgba` so its long side is at most [`PREVIEW_MAX_SIDE`].
fn bounded_preview(rgba: &[u8], width: u32, height: u32) -> (Vec<u8>, u32, u32) {
    let expected = (width as usize) * (height as usize) * 4;
    if width == 0 || height == 0 || rgba.len() < expected {
        return (vec![0; 4], 1, 1);
    }
    let long = width.max(height);
    if long <= PREVIEW_MAX_SIDE {
        return (rgba[..expected].to_vec(), width, height);
    }
    let step = long.div_ceil(PREVIEW_MAX_SIDE);
    let pw = (width / step).max(1);
    let ph = (height / step).max(1);
    let mut out = vec![0u8; (pw * ph * 4) as usize];
    for y in 0..ph {
        for x in 0..pw {
            let sx = (x * step).min(width - 1) as usize;
            let sy = (y * step).min(height - 1) as usize;
            let s = (sy * width as usize + sx) * 4;
            let d = ((y * pw + x) * 4) as usize;
            out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
        }
    }
    (out, pw, ph)
}

#[cfg(test)]
mod tests {
    use super::super::chrome::test_support::Harness;
    use super::*;

    /// A 4×1 strip: red, near-red, green, blue.
    fn strip() -> Vec<u8> {
        vec![
            255, 0, 0, 255, //
            240, 10, 10, 255, //
            0, 255, 0, 255, //
            0, 0, 255, 255,
        ]
    }

    #[test]
    fn the_preview_and_the_commit_are_the_same_function() {
        let rgba = strip();
        let dialog = ColorRangeDialog::new([255, 0, 0, 255], &rgba, 4, 1);
        let spec = dialog.confirm().unwrap();
        let committed = spec.coverage(&rgba, 4, 1).unwrap();
        assert_eq!(dialog.preview_coverage().unwrap(), committed);
        assert_eq!(committed[0], 255, "the sampled colour is fully selected");
        assert!(committed[1] > 0, "a near colour is partly selected");
        assert_eq!(committed[2], 0, "a far colour is not selected");
    }

    #[test]
    fn fuzziness_widens_the_range_and_invert_flips_it() {
        let rgba = strip();
        let mut dialog = ColorRangeDialog::new([255, 0, 0, 255], &rgba, 4, 1);
        dialog.set_fuzziness(0);
        let tight = dialog.preview_coverage().unwrap();
        assert_eq!(tight[1], 0, "no fuzziness selects only the exact colour");
        dialog.set_fuzziness(MAX_FUZZINESS);
        let wide = dialog.preview_coverage().unwrap();
        assert!(wide[1] > 0);
        dialog.set_invert(true);
        let inverted = dialog.preview_coverage().unwrap();
        for (a, b) in wide.iter().zip(&inverted) {
            assert_eq!(u16::from(*a) + u16::from(*b), 255);
        }
        // Past the scale is clamped, not stored.
        dialog.set_fuzziness(900);
        assert_eq!(dialog.spec().fuzziness, MAX_FUZZINESS);
    }

    #[test]
    fn a_large_layer_is_previewed_through_a_bounded_copy() {
        let (w, h) = (1024u32, 300u32);
        let rgba = vec![9u8; (w * h * 4) as usize];
        let dialog = ColorRangeDialog::new([9, 9, 9, 255], &rgba, w, h);
        let (pw, ph) = dialog.preview_size();
        assert!(
            pw <= PREVIEW_MAX_SIDE && ph <= PREVIEW_MAX_SIDE,
            "{pw}x{ph}"
        );
        assert_eq!(dialog.preview_coverage().unwrap().len(), (pw * ph) as usize);
    }

    #[test]
    fn the_selection_view_is_grayscale_coverage_and_the_image_view_is_the_source() {
        let rgba = strip();
        let mut dialog = ColorRangeDialog::new([0, 255, 0, 255], &rgba, 4, 1);
        let gray = dialog.preview_rgba().unwrap();
        assert_eq!(&gray[8..12], &[255, 255, 255, 255], "green is selected");
        assert_eq!(&gray[12..16], &[0, 0, 0, 255], "blue is not");
        dialog.set_view(ColorRangeView::Image);
        assert_eq!(dialog.preview_rgba().unwrap(), rgba);
    }

    struct Fixed([f32; 4]);
    impl ScreenSampler for Fixed {
        fn sample(&self, _: [f32; 2]) -> Option<[f32; 4]> {
            Some(self.0)
        }
    }

    #[test]
    fn the_eyedropper_samples_through_the_shell_sampler_only_when_armed() {
        let rgba = strip();
        let mut dialog = ColorRangeDialog::new([255, 0, 0, 255], &rgba, 4, 1);
        let blue = Fixed([0.0, 0.0, 1.0, 1.0]);
        assert!(!dialog.sample_screen([1.0, 1.0], &blue), "unarmed ignores");
        dialog.arm_eyedropper();
        assert!(dialog.sample_screen([1.0, 1.0], &blue));
        assert_eq!(dialog.spec().color, [0, 0, 255, 255]);
        assert_eq!(dialog.eyedropper(), Eyedropper::Idle);
        assert_eq!(dialog.preview_coverage().unwrap()[3], 255);
    }

    #[test]
    fn enter_confirms_the_spec_and_escape_cancels() {
        let dialog = ColorRangeDialog::new([1, 2, 3, 255], &strip(), 4, 1);
        assert_eq!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(ColorRangeSpec::new([1, 2, 3, 255]))
        );
        assert_eq!(dialog.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
    }

    /// The drawn route: the preview is a real rectangle on screen, and a
    /// click on its right-hand quarter samples the blue pixel under it —
    /// which moves the range, so the selection preview now selects blue.
    #[test]
    fn clicking_the_drawn_preview_samples_the_pixel_under_the_pointer() {
        let harness = Harness::new();
        let mut dialog = ColorRangeDialog::new([255, 0, 0, 255], &strip(), 4, 1);
        let rect = harness.settle(ids::color_range_preview(), |ctx| {
            let _ = dialog.show(ctx, None);
        });
        assert!(rect.width() > 0.0 && rect.height() > 0.0, "{rect:?}");
        let at = egui::pos2(rect.max.x - rect.width() / 8.0, rect.center().y);
        harness.frame(Harness::click_events(at), |ctx| {
            let _ = dialog.show(ctx, None);
        });
        assert_eq!(dialog.spec().color, [0, 0, 255, 255]);
        assert_eq!(dialog.preview_coverage().unwrap()[3], 255);
        assert_eq!(dialog.preview_coverage().unwrap()[0], 0);
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = ColorRangeDialog::new([255, 0, 0, 255], &strip(), 4, 1);
            assert!(dialog.show(ctx, None).is_open());
            dialog.set_view(ColorRangeView::Image);
            assert!(dialog.show(ctx, None).is_open());
        });
    }
}
