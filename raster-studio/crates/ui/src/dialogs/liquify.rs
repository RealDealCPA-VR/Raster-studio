//! Filter ▸ Liquify… — brush warping over a live preview (W7-H).
//!
//! The dialog owns a [`LiquifyField`] the size of the document and a bounded
//! copy of the active layer's pixels to preview it on. Every brush stroke on
//! the dialog's own canvas edits the field; the preview is the field applied
//! to that copy. Nothing touches the document until the dialog confirms: the
//! confirmation is a [`LiquifySpec`] carrying the field, which the shell parks
//! for the `Liquify` menu arm, and that arm applies the same field to the
//! full-resolution layer as one undoable step. Cancel writes nothing.
//!
//! No [`super::chrome::Dialog`] impl: that trait confirms to a
//! [`super::action::DialogAction`], and a warp field is not one of those —
//! the same road Trim takes. `show` still folds Escape, Enter and the action
//! row into one [`DialogOutcome`].
//!
//! The canvas is drawn at a bounded texture size (at most
//! [`PREVIEW_MAX_SIDE`] texels on the long side) and fitted to the dialog's
//! pane; pointer positions are mapped back to *document* pixels, so the brush
//! size is in document pixels whatever the preview scale.

use design::tokens::palette::ColorRole;
use design::{color32, current_tokens};
use egui::{Context, Sense};
use filters::liquify::{LiquifyBrush, LiquifyField, LiquifyTool, MAX_BRUSH_SIZE, MIN_BRUSH_SIZE};
use filters::FilterBuffer;

use super::chrome::{action_row, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth};
use super::sizes;
use crate::strings::tr;

/// The long side of the preview texture, in texels.
pub const PREVIEW_MAX_SIDE: u32 = 512;

/// A confirmed Liquify: the warp to apply to the active layer.
#[derive(Clone, PartialEq, Debug)]
pub struct LiquifySpec {
    pub field: LiquifyField,
}

impl LiquifySpec {
    /// Whether the warp moves nothing.
    pub fn is_identity(&self) -> bool {
        self.field.is_identity()
    }

    /// The warp applied to `src` (any size; see [`LiquifyField::apply`]).
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        self.field.apply(src)
    }
}

/// `src` resampled so its long side is at most `max_side`.
pub(crate) fn bounded_copy(src: &FilterBuffer, max_side: u32) -> FilterBuffer {
    let (w, h) = src.dimensions();
    let long = w.max(h);
    if long <= max_side || long == 0 {
        return src.clone();
    }
    let k = max_side as f32 / long as f32;
    let pw = ((w as f32 * k).round() as u32).max(1);
    let ph = ((h as f32 * k).round() as u32).max(1);
    let mut out = match FilterBuffer::transparent(pw, ph) {
        Ok(b) => b,
        Err(_) => return src.clone(),
    };
    let sx = w as f32 / pw as f32;
    let sy = h as f32 / ph as f32;
    for y in 0..ph {
        for x in 0..pw {
            let px = src.sample_bilinear(
                (x as f32 + 0.5) * sx,
                (y as f32 + 0.5) * sy,
                filters::EdgeMode::Clamp,
            );
            out.set(x, y, px);
        }
    }
    out
}

/// Filter ▸ Liquify….
pub struct LiquifyDialog {
    tool: LiquifyTool,
    brush: LiquifyBrush,
    field: LiquifyField,
    /// The active layer's pixels, bounded for the preview.
    source: FilterBuffer,
    texture: Option<egui::TextureHandle>,
    dirty: bool,
    /// The last brush position of the drag in progress, document pixels.
    last: Option<[f32; 2]>,
    /// Where the canvas was drawn last frame (screen points).
    canvas: Option<egui::Rect>,
}

impl LiquifyDialog {
    /// Over the active layer's full-resolution pixels. Only a bounded copy is
    /// kept; the field is sized to the document.
    pub fn new(layer: &FilterBuffer) -> Self {
        let (w, h) = layer.dimensions();
        Self {
            tool: LiquifyTool::default(),
            brush: LiquifyBrush::default(),
            field: LiquifyField::new(w, h),
            source: bounded_copy(layer, PREVIEW_MAX_SIDE),
            texture: None,
            dirty: true,
            last: None,
            canvas: None,
        }
    }

    pub fn title(&self) -> &'static str {
        "Liquify"
    }

    pub fn tool(&self) -> LiquifyTool {
        self.tool
    }

    pub fn set_tool(&mut self, tool: LiquifyTool) {
        self.tool = tool;
    }

    pub fn brush(&self) -> LiquifyBrush {
        self.brush
    }

    pub fn set_brush(&mut self, brush: LiquifyBrush) {
        self.brush = brush.sanitized();
    }

    /// The field as it stands.
    pub fn field(&self) -> &LiquifyField {
        &self.field
    }

    /// Paint one stroke segment with the current tool, in document pixels —
    /// the route the canvas's pointer takes.
    pub fn stroke(&mut self, from: [f32; 2], to: [f32; 2]) {
        self.field.stroke(self.tool, self.brush, from, to);
        self.dirty = true;
    }

    /// Throw every stroke away and start over.
    pub fn reset(&mut self) {
        let (w, h) = self.field.image_size();
        self.field = LiquifyField::new(w, h);
        self.last = None;
        self.dirty = true;
    }

    /// The preview: the field over the bounded copy.
    pub fn preview(&self) -> FilterBuffer {
        self.field.apply(&self.source)
    }

    /// Where the canvas was drawn in the last frame, for a caller that drives
    /// the pointer.
    pub fn canvas_rect(&self) -> Option<egui::Rect> {
        self.canvas
    }

    /// The spec a confirmation hands over. Always available: an untouched
    /// field is refused by the menu arm with a reason, not here.
    pub fn confirm(&self) -> Option<LiquifySpec> {
        Some(LiquifySpec {
            field: self.field.clone(),
        })
    }

    /// Escape and Enter, without drawing — Escape wins.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<LiquifySpec> {
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

    /// Draw one frame.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<LiquifySpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "liquify",
            self.title(),
            Some(tr("ui.adjustment.subtitle")),
            DialogWidth::Split,
            |ui| self.body(ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => {
                    self.reset();
                    DialogOutcome::Open
                }
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        ui.horizontal_wrapped(|ui| {
            for tool in LiquifyTool::ALL {
                if ui
                    .selectable_label(self.tool == tool, tool.label())
                    .clicked()
                {
                    self.tool = tool;
                }
            }
        });
        let before = self.brush;
        egui::Grid::new("liquify-brush")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Size");
                ui.add(
                    egui::Slider::new(&mut self.brush.size, MIN_BRUSH_SIZE..=MAX_BRUSH_SIZE)
                        .logarithmic(true),
                );
                ui.end_row();
                ui.label("Pressure");
                ui.add(egui::Slider::new(&mut self.brush.pressure, 0.0..=1.0));
                ui.end_row();
                ui.label(tr("ui.adjustment.density"));
                ui.add(egui::Slider::new(&mut self.brush.density, 0.0..=1.0));
                ui.end_row();
            });
        if before != self.brush {
            self.brush = self.brush.sanitized();
        }
        self.canvas_widget(ui);
        action_row(
            ui,
            tr("ui.adjustment.confirm"),
            None,
            &[tr("ui.adjustment.reset")],
        )
    }

    fn canvas_widget(&mut self, ui: &mut egui::Ui) {
        if self.texture.is_none() || self.dirty {
            let rgba = self.preview_rgba(ui);
            let (pw, ph) = self.source.dimensions();
            let image = egui::ColorImage::from_rgba_unmultiplied([pw as usize, ph as usize], &rgba);
            self.texture = Some(ui.ctx().load_texture(
                "liquify-preview",
                image,
                egui::TextureOptions::LINEAR,
            ));
            self.dirty = false;
        }
        let Some(texture) = &self.texture else {
            return;
        };
        let (pw, ph) = self.source.dimensions();
        let long = sizes::preview_column_width();
        let scale = long / pw.max(ph).max(1) as f32;
        let size = egui::Vec2::new(pw as f32 * scale, ph as f32 * scale);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let response = ui.interact(
            rect,
            egui::Id::new(("raster-studio-liquify", "canvas")),
            Sense::click_and_drag(),
        );
        egui::Image::new((texture.id(), size)).paint_at(ui, rect);
        self.canvas = Some(rect);

        let (dw, _) = self.field.image_size();
        // Screen points per document pixel.
        let per_px = rect.width() / dw.max(1) as f32;
        let to_doc = |pos: egui::Pos2| -> [f32; 2] {
            let local = pos - rect.min;
            [local.x / per_px, local.y / per_px]
        };
        let pressed = response.is_pointer_button_down_on();
        match (pressed, response.interact_pointer_pos()) {
            (true, Some(pos)) => {
                let here = to_doc(pos);
                let from = self.last.unwrap_or(here);
                if self.last.is_none() || from != here || self.tool.acts_in_place() {
                    self.stroke(from, here);
                }
                self.last = Some(here);
                ui.ctx().request_repaint();
            }
            _ => self.last = None,
        }
        if let Some(pos) = response.hover_pos().or(response.interact_pointer_pos()) {
            let t = current_tokens(ui);
            ui.painter().circle_stroke(
                pos,
                self.brush.radius() * per_px,
                egui::Stroke::new(
                    t.borders.hairline,
                    color32(t.palette.color(ColorRole::Accent)),
                ),
            );
        }
    }

    /// The preview's straight-alpha bytes, with the freeze mask tinted in.
    fn preview_rgba(&self, ui: &egui::Ui) -> Vec<u8> {
        let mut rgba = self.preview().to_rgba8();
        let tint = current_tokens(ui).palette.color(ColorRole::DangerSubtle);
        let (pw, ph) = self.source.dimensions();
        let (dw, dh) = self.field.image_size();
        let kx = dw as f32 / pw.max(1) as f32;
        let ky = dh as f32 / ph.max(1) as f32;
        let strength = tint.a as f32 / 255.0;
        for y in 0..ph {
            for x in 0..pw {
                let frozen = self
                    .field
                    .freeze_at((x as f32 + 0.5) * kx, (y as f32 + 0.5) * ky);
                if frozen <= 0.0 {
                    continue;
                }
                let m = frozen * strength;
                let i = ((y * pw + x) * 4) as usize;
                for (c, target) in [tint.r, tint.g, tint.b].into_iter().enumerate() {
                    let v = rgba[i + c] as f32;
                    rgba[i + c] = (v + (target as f32 - v) * m).round() as u8;
                }
                rgba[i + 3] = rgba[i + 3].max((m * 255.0) as u8);
            }
        }
        rgba
    }
}

impl std::fmt::Debug for LiquifyDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiquifyDialog")
            .field("tool", &self.tool)
            .field("brush", &self.brush)
            .field("image", &self.field.image_size())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::super::chrome::test_support::frame;
    use super::*;

    fn bar(w: u32, h: u32) -> FilterBuffer {
        let mut buf = FilterBuffer::filled(w, h, [1.0; 4]).unwrap();
        for y in 0..h {
            for x in w / 3..w / 3 + 2 {
                buf.set(x, y, [0.0, 0.0, 0.0, 1.0]);
            }
        }
        buf
    }

    #[test]
    fn an_untouched_dialog_previews_and_confirms_the_identity() {
        let src = bar(64, 64);
        let dialog = LiquifyDialog::new(&src);
        assert_eq!(dialog.preview(), src);
        let spec = dialog.confirm().unwrap();
        assert!(spec.is_identity());
        assert_eq!(spec.apply(&src), src);
        assert_eq!(dialog.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
        assert!(dialog.resolve(DialogKeys::NONE).is_open());
    }

    #[test]
    fn a_stroke_changes_the_preview_and_the_confirmed_spec_and_reset_undoes_it() {
        let src = bar(64, 64);
        let mut dialog = LiquifyDialog::new(&src);
        dialog.set_brush(LiquifyBrush {
            size: 30.0,
            pressure: 1.0,
            density: 0.5,
        });
        dialog.stroke([22.0, 32.0], [32.0, 32.0]);
        assert_ne!(dialog.preview(), src);
        match dialog.resolve(DialogKeys::CONFIRM) {
            DialogOutcome::Confirmed(spec) => {
                assert!(!spec.is_identity());
                assert_eq!(spec.apply(&src), dialog.preview());
            }
            other => panic!("Enter did not confirm: {other:?}"),
        }
        dialog.reset();
        assert!(dialog.confirm().unwrap().is_identity());
    }

    #[test]
    fn the_preview_is_bounded_and_the_field_is_document_sized() {
        let src = FilterBuffer::filled(1200, 600, [0.5; 4]).unwrap();
        let dialog = LiquifyDialog::new(&src);
        assert_eq!(dialog.source.dimensions(), (512, 256));
        assert_eq!(dialog.field().image_size(), (1200, 600));
    }

    #[test]
    fn dragging_on_the_drawn_canvas_warps_the_field() {
        // Lay the dialog out once, read its canvas rect back, then press,
        // drag and release inside it over real egui frames.
        let src = bar(64, 64);
        let mut dialog = LiquifyDialog::new(&src);
        dialog.set_brush(LiquifyBrush {
            size: 30.0,
            pressure: 1.0,
            density: 0.5,
        });
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1400.0, 900.0));
        let run = |ctx: &egui::Context, dialog: &mut LiquifyDialog, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            };
            let mut outcome = DialogOutcome::Open;
            let _ = ctx.run(input, |ctx| outcome = dialog.show(ctx));
            outcome
        };
        assert!(run(&ctx, &mut dialog, Vec::new()).is_open());
        assert!(run(&ctx, &mut dialog, Vec::new()).is_open());
        let rect = dialog.canvas_rect().expect("the canvas was drawn");
        let at = |x: f32, y: f32| rect.min + egui::Vec2::new(x, y) * (rect.width() / 64.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let start = at(22.0, 32.0);
        run(
            &ctx,
            &mut dialog,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        for step in 1..=5 {
            let p = at(22.0 + 2.0 * step as f32, 32.0);
            run(&ctx, &mut dialog, vec![egui::Event::PointerMoved(p)]);
        }
        let end = at(32.0, 32.0);
        run(&ctx, &mut dialog, vec![button(end, false)]);
        assert!(
            !dialog.field().is_identity(),
            "the drag on the drawn canvas warped the field"
        );
        let d = dialog.field().displacement_at(27.0, 32.0);
        assert!(
            d[0] < -1.0,
            "content moved right: the result reads from the left: {d:?}"
        );
    }

    #[test]
    fn the_dialog_draws_in_both_themes_without_panicking() {
        let src = bar(64, 64);
        let mut dialog = LiquifyDialog::new(&src);
        dialog.set_tool(LiquifyTool::FreezeMask);
        dialog.stroke([20.0, 20.0], [20.0, 20.0]);
        let _ = frame(|ctx| dialog.show(ctx));
        assert!(dialog.canvas_rect().is_some());
    }
}
