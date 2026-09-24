//! Edit ▸ Perspective Warp (W10-G): draw quads, then drag their corners.
//!
//! Two modes, as in Photoshop:
//!
//! * **Layout** — a drag on the preview draws a quad over one plane of the
//!   image; dragging an existing corner re-shapes the quad (source and target
//!   together, since nothing has been warped yet).
//! * **Warp** — dragging a corner moves only where it goes; the plane under
//!   the quad follows through that quad's homography
//!   ([`filters::perspective_warp`]).
//!
//! The preview is the warp over a bounded copy of the layer. Enter commits a
//! [`PerspectiveWarpSpec`] — the quads in document pixels — which the shell
//! parks for the `PerspectiveWarp` menu arm; that arm warps the
//! full-resolution layer as one undoable step. Escape writes nothing.

use design::tokens::palette::ColorRole;
use design::tokens::Space;
use design::{color32, current_tokens};
use egui::{Context, Sense};
use filters::perspective_warp::{PerspectiveWarp, WarpQuad};
use filters::FilterBuffer;

use super::chrome::{action_row, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth};
use super::controls::combo;
use super::liquify::{bounded_copy, PREVIEW_MAX_SIDE};
use super::sizes;
use crate::strings::tr;

/// Which half of the tool the canvas is in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum WarpMode {
    /// Draw and shape quads.
    #[default]
    Layout,
    /// Drag the corners to warp.
    Warp,
}

impl WarpMode {
    pub const ALL: [WarpMode; 2] = [WarpMode::Layout, WarpMode::Warp];

    pub fn label(self) -> &'static str {
        match self {
            WarpMode::Layout => "Layout",
            WarpMode::Warp => "Warp",
        }
    }
}

/// A confirmed Perspective Warp, in document pixels.
#[derive(Clone, PartialEq, Debug)]
pub struct PerspectiveWarpSpec {
    pub image_size: (u32, u32),
    pub warp: PerspectiveWarp,
}

impl PerspectiveWarpSpec {
    pub fn is_identity(&self) -> bool {
        self.warp.is_identity()
    }

    /// The warp applied to a full-resolution layer.
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        self.warp.apply(src)
    }
}

/// What a drag on the canvas is doing.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Drag {
    /// Drawing a new quad from this document point.
    Drawing([f32; 2]),
    /// Moving corner `corner` of quad `quad`.
    Corner { quad: usize, corner: usize },
}

/// Edit ▸ Perspective Warp.
pub struct PerspectiveWarpDialog {
    width: u32,
    height: u32,
    source: FilterBuffer,
    mode: WarpMode,
    quads: Vec<WarpQuad>,
    drag: Option<Drag>,
    /// The rectangle being drawn, in document points, for the overlay.
    rubber: Option<([f32; 2], [f32; 2])>,
    texture: Option<egui::TextureHandle>,
    dirty: bool,
    canvas: Option<egui::Rect>,
}

impl PerspectiveWarpDialog {
    /// Over the active layer's full-resolution pixels.
    pub fn new(layer: &FilterBuffer) -> Self {
        let (width, height) = layer.dimensions();
        Self {
            width,
            height,
            source: bounded_copy(layer, PREVIEW_MAX_SIDE),
            mode: WarpMode::Layout,
            quads: Vec::new(),
            drag: None,
            rubber: None,
            texture: None,
            dirty: true,
            canvas: None,
        }
    }

    pub fn title(&self) -> String {
        crate::menu::MenuAction::PerspectiveWarp.label()
    }

    pub fn mode(&self) -> WarpMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: WarpMode) {
        self.mode = mode;
    }

    pub fn quads(&self) -> &[WarpQuad] {
        &self.quads
    }

    /// Layout: add the rectangle `a`-`b` (document points) as a quad.
    /// Refused (returns `None`) in Warp mode or when it has no area.
    pub fn add_quad(&mut self, a: [f32; 2], b: [f32; 2]) -> Option<usize> {
        if self.mode != WarpMode::Layout || (a[0] - b[0]).abs() < 2.0 || (a[1] - b[1]).abs() < 2.0 {
            return None;
        }
        let clamp = |p: [f32; 2]| {
            [
                p[0].clamp(0.0, self.width as f32),
                p[1].clamp(0.0, self.height as f32),
            ]
        };
        let (a, b) = (clamp(a), clamp(b));
        self.quads.push(WarpQuad::rect(a[0], a[1], b[0], b[1]));
        self.dirty = true;
        Some(self.quads.len() - 1)
    }

    /// Move a corner to document point `p`: in Layout both the drawn and the
    /// warped corner (the quad is re-shaped), in Warp only the warped one.
    pub fn move_corner(&mut self, quad: usize, corner: usize, p: [f32; 2]) {
        let mode = self.mode;
        if let Some(q) = self.quads.get_mut(quad) {
            if corner < 4 {
                if mode == WarpMode::Layout {
                    q.source[corner] = p;
                }
                q.target[corner] = p;
                self.dirty = true;
            }
        }
    }

    /// Throw every quad away.
    pub fn reset(&mut self) {
        self.quads.clear();
        self.drag = None;
        self.dirty = true;
    }

    /// The corner within `radius` document pixels of `p`, nearest first:
    /// the warped corners (what is drawn) are the handles.
    fn corner_near(&self, p: [f32; 2], radius: f32) -> Option<(usize, usize)> {
        let mut best: Option<((usize, usize), f32)> = None;
        for (qi, q) in self.quads.iter().enumerate() {
            for (ci, c) in q.target.iter().enumerate() {
                let d = ((c[0] - p[0]).powi(2) + (c[1] - p[1]).powi(2)).sqrt();
                if d <= radius && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some(((qi, ci), d));
                }
            }
        }
        best.map(|(hit, _)| hit)
    }

    fn warp(&self) -> PerspectiveWarp {
        PerspectiveWarp {
            quads: self.quads.clone(),
        }
    }

    /// The preview: the warp over the bounded copy.
    pub fn preview(&self) -> FilterBuffer {
        let k = self.source.dimensions().0 as f32 / self.width.max(1) as f32;
        self.warp().scaled(k).apply(&self.source)
    }

    pub fn canvas_rect(&self) -> Option<egui::Rect> {
        self.canvas
    }

    pub fn confirm(&self) -> Option<PerspectiveWarpSpec> {
        let spec = PerspectiveWarpSpec {
            image_size: (self.width, self.height),
            warp: self.warp(),
        };
        (!spec.is_identity()).then_some(spec)
    }

    pub fn blocked_reason(&self) -> Option<String> {
        self.confirm()
            .is_none()
            .then(|| tr("ui.adjustment.blocked.identity").to_string())
    }

    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<PerspectiveWarpSpec> {
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

    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<PerspectiveWarpSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let title = self.title();
        let drawn = modal(
            ctx,
            "perspective-warp",
            &title,
            None,
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
        combo(
            ui,
            "perspective-warp-mode",
            &mut self.mode,
            &WarpMode::ALL,
            |m| m.label().to_string(),
            |_| None,
        );
        self.canvas_widget(ui);
        let blocked = self.blocked_reason();
        action_row(
            ui,
            tr("ui.adjustment.confirm"),
            blocked.as_deref(),
            &[tr("ui.adjustment.reset")],
        )
    }

    fn canvas_widget(&mut self, ui: &mut egui::Ui) {
        if self.texture.is_none() || self.dirty {
            let rgba = self.preview().to_rgba8();
            let (pw, ph) = self.source.dimensions();
            let image = egui::ColorImage::from_rgba_unmultiplied([pw as usize, ph as usize], &rgba);
            self.texture = Some(ui.ctx().load_texture(
                "perspective-warp-preview",
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
            egui::Id::new(("raster-studio-perspective-warp", "canvas")),
            Sense::click_and_drag(),
        );
        egui::Image::new((texture.id(), size)).paint_at(ui, rect);
        self.canvas = Some(rect);

        let per_px = rect.width() / self.width.max(1) as f32;
        let to_doc = |pos: egui::Pos2| -> [f32; 2] {
            let local = pos - rect.min;
            [local.x / per_px, local.y / per_px]
        };
        let to_screen =
            |p: [f32; 2]| -> egui::Pos2 { rect.min + egui::Vec2::new(p[0], p[1]) * per_px };
        let handle = Space::Small.pt();
        let pick = (handle * 2.0) / per_px;

        if response.drag_started() {
            // Where the button went down, not where the pointer is once the
            // drag threshold was crossed: a quad starts at the press.
            let origin = ui
                .input(|i| i.pointer.press_origin())
                .or_else(|| response.interact_pointer_pos());
            if let Some(pos) = origin {
                let p = to_doc(pos);
                self.drag = match self.corner_near(p, pick) {
                    Some((quad, corner)) => Some(Drag::Corner { quad, corner }),
                    None if self.mode == WarpMode::Layout => Some(Drag::Drawing(p)),
                    None => None,
                };
            }
        }
        if response.dragged() {
            if let Some(pos) = response.interact_pointer_pos() {
                let p = to_doc(pos);
                match self.drag {
                    Some(Drag::Corner { quad, corner }) => {
                        self.move_corner(quad, corner, p);
                        ui.ctx().request_repaint();
                    }
                    Some(Drag::Drawing(start)) => self.rubber = Some((start, p)),
                    None => {}
                }
            }
        }
        if response.drag_stopped() {
            if let (Some(Drag::Drawing(start)), Some(pos)) =
                (self.drag, response.interact_pointer_pos())
            {
                let _ = self.add_quad(start, to_doc(pos));
            }
            self.drag = None;
            self.rubber = None;
        }

        let t = current_tokens(ui);
        let painter = ui.painter_at(rect);
        let edge = egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::Accent)),
        );
        for q in &self.quads {
            let pts = q.target.map(to_screen);
            for i in 0..4 {
                painter.line_segment([pts[i], pts[(i + 1) % 4]], edge);
            }
            let fill = color32(t.palette.color(ColorRole::Accent));
            let ring = egui::Stroke::new(
                t.borders.hairline,
                color32(t.palette.color(ColorRole::TextOnAccent)),
            );
            for p in pts {
                painter.circle_filled(p, handle, fill);
                painter.circle_stroke(p, handle, ring);
            }
        }
        if let Some((a, b)) = self.rubber {
            let r = egui::Rect::from_two_pos(to_screen(a), to_screen(b));
            painter.rect_stroke(r, 0.0, edge);
        }
    }
}

impl std::fmt::Debug for PerspectiveWarpDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PerspectiveWarpDialog")
            .field("size", &(self.width, self.height))
            .field("mode", &self.mode)
            .field("quads", &self.quads)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer() -> FilterBuffer {
        let mut b = FilterBuffer::transparent(64, 64).unwrap();
        for y in 0..64 {
            for x in 0..64 {
                let v = ((x / 8 + y / 8) % 2) as f32;
                b.set(x, y, [v, 0.5, 1.0 - v, 1.0]);
            }
        }
        b
    }

    #[test]
    fn a_quad_left_unmoved_is_the_identity_and_blocked() {
        let src = layer();
        let mut dialog = PerspectiveWarpDialog::new(&src);
        dialog.add_quad([8.0, 8.0], [40.0, 48.0]).unwrap();
        assert!(dialog.confirm().is_none());
        assert!(dialog.blocked_reason().is_some());
        assert_eq!(dialog.preview(), src);
    }

    #[test]
    fn warp_mode_moves_only_the_target_corner() {
        let src = layer();
        let mut dialog = PerspectiveWarpDialog::new(&src);
        let q = dialog.add_quad([8.0, 8.0], [40.0, 48.0]).unwrap();
        dialog.set_mode(WarpMode::Warp);
        assert!(
            dialog.add_quad([0.0, 0.0], [9.0, 9.0]).is_none(),
            "no drawing in Warp"
        );
        dialog.move_corner(q, 1, [46.0, 4.0]);
        let quad = dialog.quads()[q];
        assert_eq!(quad.source[1], [40.0, 8.0]);
        assert_eq!(quad.target[1], [46.0, 4.0]);
        let spec = match dialog.resolve(DialogKeys::CONFIRM) {
            DialogOutcome::Confirmed(spec) => spec,
            other => panic!("Enter did not commit: {other:?}"),
        };
        assert_eq!(spec.image_size, (64, 64));
        assert_ne!(spec.apply(&src), src);
        assert_eq!(
            dialog.preview(),
            spec.apply(&src),
            "the preview is the full warp"
        );
        dialog.reset();
        assert!(dialog.quads().is_empty());
    }

    #[test]
    fn dragging_on_the_drawn_canvas_draws_a_quad_then_warps_a_corner() {
        let src = layer();
        let mut dialog = PerspectiveWarpDialog::new(&src);
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1400.0, 900.0));
        let run = |ctx: &egui::Context, d: &mut PerspectiveWarpDialog, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                let _ = d.show(ctx);
            });
        };
        run(&ctx, &mut dialog, Vec::new());
        run(&ctx, &mut dialog, Vec::new());
        let rect = dialog.canvas_rect().expect("the canvas was drawn");
        let at = |x: f32, y: f32| rect.min + egui::Vec2::new(x, y) * (rect.width() / 64.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let drag = |ctx: &egui::Context,
                    d: &mut PerspectiveWarpDialog,
                    from: (f32, f32),
                    to: (f32, f32)| {
            let p0 = at(from.0, from.1);
            run(
                ctx,
                d,
                vec![egui::Event::PointerMoved(p0), button(p0, true)],
            );
            for step in 1..=6 {
                let k = step as f32 / 6.0;
                let p = at(from.0 + (to.0 - from.0) * k, from.1 + (to.1 - from.1) * k);
                run(ctx, d, vec![egui::Event::PointerMoved(p)]);
            }
            let p1 = at(to.0, to.1);
            run(ctx, d, vec![button(p1, false)]);
            run(ctx, d, Vec::new());
        };
        // Layout: a drag across empty canvas draws one quad.
        drag(&ctx, &mut dialog, (10.0, 10.0), (50.0, 44.0));
        assert_eq!(dialog.quads().len(), 1, "one drag, one quad");
        let q = dialog.quads()[0];
        assert!(
            (q.source[0][0] - 10.0).abs() < 1.5 && (q.source[2][1] - 44.0).abs() < 1.5,
            "the quad spans the drag: {q:?}"
        );
        // Warp: dragging the top-right corner moves only its target.
        dialog.set_mode(WarpMode::Warp);
        let corner = q.target[1];
        drag(
            &ctx,
            &mut dialog,
            (corner[0], corner[1]),
            (corner[0] + 8.0, corner[1] - 6.0),
        );
        let moved = dialog.quads()[0];
        assert_eq!(moved.source, q.source, "the drawn quad is where it was");
        assert!(
            (moved.target[1][0] - (corner[0] + 8.0)).abs() < 1.5,
            "the corner followed the drag: {corner:?} -> {:?}",
            moved.target[1]
        );
        assert!(dialog.confirm().is_some());
    }
}
