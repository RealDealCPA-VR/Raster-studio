//! W13X-3: Layer ▸ Layer Style ▸ Scale Effects… — Photopea's percent question.
//!
//! The dialog asks one number, the scale (1–1000%), with a Preview switch.
//! It hands back the percent and nothing else: `app-shell`'s
//! `layer_ops_w13::scale_effects` scales every size and distance of the
//! active layer's style as one undoable step. Like Trim, there is no
//! [`super::chrome::Dialog`] impl — the shell's dialog host pushes the
//! confirmed percent as the `LayerExtra(ScaleEffects(percent))` pick.
//!
//! The preview is an image the host renders (the document composited with
//! the style scaled to the dialog's percent) and hands in through
//! [`ScaleEffectsDialog::set_preview_image`]; the dialog asks for a new one
//! through [`ScaleEffectsDialog::preview_wanted`] whenever the percent moves
//! while Preview is on. The document itself is never touched before the
//! confirmation, so Cancel needs nothing restored.

use std::ops::RangeInclusive;

use egui::Context;

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::{checkbox_row, integer};
use super::sizes;
use crate::strings::tr;

/// The percentages Scale Effects accepts, as Photoshop's field does.
pub const SCALE_EFFECTS_PERCENT: RangeInclusive<u16> = 1..=1000;

/// The percent the dialog opens on: the style as it is.
pub const SCALE_EFFECTS_IDENTITY: u16 = 100;

/// One host-rendered preview, and the percent it shows.
struct PreviewImage {
    percent: u16,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

/// Layer ▸ Layer Style ▸ Scale Effects….
pub struct ScaleEffectsDialog {
    percent: u16,
    preview: bool,
    image: Option<PreviewImage>,
    /// The uploaded texture and the percent it was uploaded for.
    texture: Option<(egui::TextureHandle, u16)>,
}

impl Default for ScaleEffectsDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ScaleEffectsDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScaleEffectsDialog")
            .field("percent", &self.percent)
            .field("preview", &self.preview)
            .field("preview_percent", &self.preview_percent())
            .finish_non_exhaustive()
    }
}

impl ScaleEffectsDialog {
    /// Opens at 100% with Preview on, as Photopea's does.
    pub fn new() -> Self {
        Self {
            percent: SCALE_EFFECTS_IDENTITY,
            preview: true,
            image: None,
            texture: None,
        }
    }

    pub fn percent(&self) -> u16 {
        self.percent
    }

    /// Set the percent, clamped into [`SCALE_EFFECTS_PERCENT`].
    pub fn set_percent(&mut self, percent: u16) {
        self.percent = percent.clamp(*SCALE_EFFECTS_PERCENT.start(), *SCALE_EFFECTS_PERCENT.end());
    }

    pub fn preview_enabled(&self) -> bool {
        self.preview
    }

    pub fn set_preview(&mut self, on: bool) {
        self.preview = on;
    }

    /// The percent the host should render a preview for: `Some` while
    /// Preview is on and the image in hand shows another percent (or none).
    pub fn preview_wanted(&self) -> Option<u16> {
        (self.preview && self.preview_percent() != Some(self.percent)).then_some(self.percent)
    }

    /// The percent the preview in hand shows, if there is one.
    pub fn preview_percent(&self) -> Option<u16> {
        self.image.as_ref().map(|i| i.percent)
    }

    /// Hand in the host's rendering of the document at `percent`: straight
    /// RGBA8, `width` x `height`. A buffer of the wrong length is dropped.
    pub fn set_preview_image(&mut self, percent: u16, rgba: Vec<u8>, width: u32, height: u32) {
        if rgba.len() != (width as usize) * (height as usize) * 4 || width == 0 || height == 0 {
            return;
        }
        self.image = Some(PreviewImage {
            percent,
            rgba,
            width,
            height,
        });
    }

    /// Why OK is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<String> {
        (self.percent == SCALE_EFFECTS_IDENTITY)
            .then(|| tr("ui.scale_effects.unchanged").to_string())
    }

    /// The percent a confirmation carries.
    pub fn confirm(&self) -> Option<u16> {
        self.blocked_reason().is_none().then_some(self.percent)
    }

    /// Escape and Enter, without drawing. Escape wins.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<u16> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        match self.confirm() {
            Some(p) if keys.confirm => DialogOutcome::Confirmed(p),
            _ => DialogOutcome::Open,
        }
    }

    pub fn title(&self) -> &'static str {
        tr("ui.scale_effects.title")
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<u16> {
        let keys = DialogKeys::read(ctx);
        let drawn = modal(
            ctx,
            "scale-effects",
            self.title(),
            None,
            DialogWidth::Narrow,
            |ui| self.body(ui),
        );
        let mut out = self.resolve(keys);
        if let Some(Some(button)) = drawn {
            out = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => DialogOutcome::Open,
            };
        }
        out
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        caption(ui, tr("ui.scale_effects.subtitle"));
        ui.horizontal(|ui| {
            ui.label(tr("ui.scale_effects.scale"));
            let mut value = i64::from(self.percent);
            integer(
                ui,
                &mut value,
                i64::from(*SCALE_EFFECTS_PERCENT.start())..=i64::from(*SCALE_EFFECTS_PERCENT.end()),
            );
            self.set_percent(u16::try_from(value).unwrap_or(SCALE_EFFECTS_IDENTITY));
            ui.label(tr("ui.scale_effects.percent_sign"));
        });
        checkbox_row(ui, tr("ui.scale_effects.preview"), &mut self.preview);
        if self.preview {
            self.draw_preview(ui);
        }
        action_row(
            ui,
            tr("ui.scale_effects.ok"),
            self.blocked_reason().as_deref(),
            &[],
        )
    }

    fn draw_preview(&mut self, ui: &mut egui::Ui) {
        let Some(image) = &self.image else {
            caption(ui, tr("ui.scale_effects.rendering"));
            return;
        };
        let stale = self
            .texture
            .as_ref()
            .is_none_or(|(_, shown)| *shown != image.percent);
        if stale {
            let pixels = egui::ColorImage::from_rgba_unmultiplied(
                [image.width as usize, image.height as usize],
                &image.rgba,
            );
            let texture = ui.ctx().load_texture(
                "scale-effects-preview",
                pixels,
                egui::TextureOptions::LINEAR,
            );
            self.texture = Some((texture, image.percent));
        }
        if let Some((texture, _)) = &self.texture {
            ui.add(egui::Image::new(texture).max_size(sizes::style_preview()));
        }
    }

    /// Test seam: the texture the last frame drew, if any.
    pub fn preview_texture_id(&self) -> Option<egui::TextureId> {
        self.texture.as_ref().map(|(t, _)| t.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_opens_at_100_percent_with_preview_on_and_ok_blocked() {
        let dialog = ScaleEffectsDialog::new();
        assert_eq!(dialog.percent(), 100);
        assert!(dialog.preview_enabled());
        assert_eq!(dialog.confirm(), None);
        assert!(dialog.blocked_reason().is_some());
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
        assert_eq!(dialog.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
    }

    #[test]
    fn any_percent_in_range_confirms_and_the_ends_clamp() {
        let mut dialog = ScaleEffectsDialog::new();
        dialog.set_percent(37);
        assert_eq!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(37)
        );
        dialog.set_percent(0);
        assert_eq!(dialog.percent(), 1);
        dialog.set_percent(5000);
        assert_eq!(dialog.percent(), 1000);
        assert_eq!(dialog.confirm(), Some(1000));
    }

    #[test]
    fn a_preview_is_wanted_only_while_on_and_stale() {
        let mut dialog = ScaleEffectsDialog::new();
        dialog.set_percent(37);
        assert_eq!(dialog.preview_wanted(), Some(37));
        dialog.set_preview_image(37, vec![0; 4 * 4 * 4], 4, 4);
        assert_eq!(dialog.preview_wanted(), None);
        dialog.set_percent(38);
        assert_eq!(dialog.preview_wanted(), Some(38));
        dialog.set_preview(false);
        assert_eq!(dialog.preview_wanted(), None);
        // A buffer that does not match its size is refused.
        dialog.set_preview_image(38, vec![0; 3], 4, 4);
        assert_eq!(dialog.preview_percent(), Some(37));
    }

    /// Every text a frame painted, nested shapes included, and whether a
    /// mesh drew `texture`.
    fn painted(
        shapes: &[egui::epaint::ClippedShape],
        texture: Option<egui::TextureId>,
    ) -> (Vec<String>, bool) {
        fn walk(
            shape: &egui::Shape,
            texture: Option<egui::TextureId>,
            text: &mut Vec<String>,
            drew: &mut bool,
        ) {
            match shape {
                egui::Shape::Text(t) => text.push(t.galley.text().to_string()),
                egui::Shape::Mesh(m) => *drew |= Some(m.texture_id) == texture,
                egui::Shape::Rect(r) => *drew |= Some(r.fill_texture_id) == texture,
                egui::Shape::Vec(inner) => inner.iter().for_each(|s| walk(s, texture, text, drew)),
                _ => {}
            }
        }
        let mut text = Vec::new();
        let mut drew = false;
        shapes
            .iter()
            .for_each(|c| walk(&c.shape, texture, &mut text, &mut drew));
        (text, drew)
    }

    #[test]
    fn the_frame_draws_the_scale_field_the_preview_switch_and_the_image() {
        let ctx = Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut dialog = ScaleEffectsDialog::new();
        dialog.set_percent(37);
        dialog.set_preview_image(37, [200u8, 40, 40, 255].repeat(8 * 8), 8, 8);
        let mut last = (Vec::new(), false);
        for _ in 0..3 {
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                assert!(dialog.show(ctx).is_open());
            });
            last = painted(&out.shapes, dialog.preview_texture_id());
        }
        let (text, drew) = last;
        for want in ["Scale Effects", "Scale:", "37", "%", "Preview", "OK"] {
            assert!(text.iter().any(|t| t == want), "{want:?} in {text:?}");
        }
        assert!(drew, "the preview image is drawn");
        // Preview off: the image is not drawn.
        dialog.set_preview(false);
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            let _ = dialog.show(ctx);
        });
        assert!(!painted(&out.shapes, dialog.preview_texture_id()).1);
    }
}
