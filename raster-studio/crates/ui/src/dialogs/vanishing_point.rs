//! Filter ▸ Vanishing Point… — edit inside a perspective plane (W10-D).
//!
//! The dialog opens over the active layer with a plane inset a quarter from
//! each side ([`PerspectivePlane::centered`]). In **Plane** mode the four
//! corner handles drag to lie the plane over a surface in the picture; the
//! plane's grid is drawn through its homography, so the lines bunch up as
//! the surface recedes, and an unusable plane (a bow-tie or a collapsed
//! side) is outlined in the danger colour and edits nothing.
//!
//! * **Clone** mode is the perspective clone stamp: Alt+click on the plane
//!   sets the source, then clicks and drags paint dabs whose content comes
//!   from the same offset *on the plane* — so a patch cloned from the near,
//!   large end of a floor lands foreshortened at the far end. The offset is
//!   aligned (fixed by the first dab after the source is set), as in
//!   Photoshop.
//! * **Paste** mode lays the image Edit ▸ Copy last put on the clipboard
//!   into the plane, centred where the plane is clicked, its width the Size
//!   fraction of the plane's, its height following the image's aspect and
//!   the plane's estimated one (its mean side lengths).
//!
//! Every edit is a [`VanishingOp`] in plane coordinates, so the preview (the
//! ops over a bounded copy, the plane scaled with it) and the full-resolution
//! apply are the same recipe. Undo takes back the last edit. Enter commits a
//! [`VanishingPointSpec`], which the shell parks for the `VanishingPoint`
//! menu arm; that arm applies the whole session to the layer as ONE undo
//! step.

use design::tokens::palette::ColorRole;
use design::tokens::Space;
use design::{color32, current_tokens};
use egui::{Context, Sense};
use filters::vanishing_point::{PerspectivePlane, VanishingOp, VanishingPointEdit};
use filters::FilterBuffer;

use super::chrome::{action_row, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth};
use super::controls::combo;
use super::liquify::{bounded_copy, PREVIEW_MAX_SIDE};
use super::sizes;
use crate::strings::tr;

/// How many cells a side of the drawn grid has.
pub const VANISHING_GRID_DIVISIONS: u32 = 8;

/// What a click on the plane does.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum VanishingMode {
    /// Drag the plane's corners.
    #[default]
    Plane,
    /// The perspective clone stamp.
    Clone,
    /// Paste the clipboard into the plane.
    Paste,
}

impl VanishingMode {
    pub const ALL: [VanishingMode; 3] = [
        VanishingMode::Plane,
        VanishingMode::Clone,
        VanishingMode::Paste,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            VanishingMode::Plane => "Plane",
            VanishingMode::Clone => "Clone",
            VanishingMode::Paste => "Paste",
        }
    }
}

/// A confirmed Vanishing Point session.
#[derive(Clone, PartialEq, Debug)]
pub struct VanishingPointSpec {
    /// The document size the dialog opened over.
    pub image_size: (u32, u32),
    /// The plane in document pixels and the edits in plane coordinates.
    pub edit: VanishingPointEdit,
}

impl VanishingPointSpec {
    /// Whether applying changes nothing.
    pub fn is_identity(&self) -> bool {
        self.edit.is_identity()
    }

    /// The session applied to `src` (document-sized).
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        self.edit.apply(src)
    }
}

/// Filter ▸ Vanishing Point….
pub struct VanishingPointDialog {
    /// The layer, bounded for the preview.
    source: FilterBuffer,
    width: u32,
    height: u32,
    plane: PerspectivePlane,
    ops: Vec<VanishingOp>,
    mode: VanishingMode,
    /// What Paste lays into the plane: the clipboard when the dialog opened.
    paste: Option<FilterBuffer>,
    /// Paste width as a fraction of the plane's width.
    paste_size: f32,
    /// Clone brush radius in plane units.
    radius: f32,
    clone_source: Option<[f32; 2]>,
    clone_offset: Option<[f32; 2]>,
    last_dab: Option<[f32; 2]>,
    dragging_corner: Option<usize>,
    texture: Option<egui::TextureHandle>,
    dirty: bool,
    canvas: Option<egui::Rect>,
}

impl std::fmt::Debug for VanishingPointDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VanishingPointDialog")
            .field("size", &(self.width, self.height))
            .field("plane", &self.plane)
            .field("ops", &self.ops.len())
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl VanishingPointDialog {
    /// Over the active layer's full-resolution pixels; `paste` is the image
    /// on the clipboard, if any.
    pub fn new(layer: &FilterBuffer, paste: Option<FilterBuffer>) -> Self {
        let (width, height) = layer.dimensions();
        Self {
            source: bounded_copy(layer, PREVIEW_MAX_SIDE),
            width,
            height,
            plane: PerspectivePlane::centered(width, height),
            ops: Vec::new(),
            mode: VanishingMode::default(),
            paste: paste.filter(|p| !p.is_empty()),
            paste_size: 0.5,
            radius: 0.08,
            clone_source: None,
            clone_offset: None,
            last_dab: None,
            dragging_corner: None,
            texture: None,
            dirty: true,
            canvas: None,
        }
    }

    /// The title, which is the menu row's name.
    pub fn title(&self) -> &'static str {
        crate::menu::VANISHING_POINT_TITLE
    }

    pub fn plane(&self) -> PerspectivePlane {
        self.plane
    }

    pub fn ops(&self) -> &[VanishingOp] {
        &self.ops
    }

    pub fn mode(&self) -> VanishingMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: VanishingMode) {
        self.mode = mode;
        self.last_dab = None;
    }

    pub fn has_paste(&self) -> bool {
        self.paste.is_some()
    }

    /// Move corner `index` (0..4: top-left, top-right, bottom-right,
    /// bottom-left) to document point `p`, clamped into the image.
    pub fn set_corner(&mut self, index: usize, p: [f32; 2]) {
        if index < 4 && p.iter().all(|v| v.is_finite()) {
            self.plane.corners[index] = [
                p[0].clamp(0.0, self.width as f32),
                p[1].clamp(0.0, self.height as f32),
            ];
            self.dirty = true;
        }
    }

    /// Set the clone brush radius (plane units, 0.005..=0.5).
    pub fn set_radius(&mut self, radius: f32) {
        if radius.is_finite() {
            self.radius = radius.clamp(0.005, 0.5);
        }
    }

    /// Set the paste width (fraction of the plane's width, 0.05..=1).
    pub fn set_paste_size(&mut self, size: f32) {
        if size.is_finite() {
            self.paste_size = size.clamp(0.05, 1.0);
        }
    }

    /// Plane coordinates of document point `p`, if it lies on the plane.
    fn on_plane(&self, p: [f32; 2]) -> Option<[f32; 2]> {
        if !self.plane.is_valid() {
            return None;
        }
        let uv = self.plane.to_plane(p)?;
        ((0.0..=1.0).contains(&uv[0]) && (0.0..=1.0).contains(&uv[1])).then_some(uv)
    }

    /// Alt+click: the clone source, at document point `p`. Refused off the
    /// plane.
    pub fn set_clone_source(&mut self, p: [f32; 2]) -> bool {
        match self.on_plane(p) {
            Some(uv) => {
                self.clone_source = Some(uv);
                self.clone_offset = None;
                self.last_dab = None;
                true
            }
            None => false,
        }
    }

    /// One clone dab centred on document point `p`. Refused off the plane
    /// or before a source is set.
    pub fn clone_at(&mut self, p: [f32; 2]) -> bool {
        let (Some(uv), Some(source)) = (self.on_plane(p), self.clone_source) else {
            return false;
        };
        let offset = *self
            .clone_offset
            .get_or_insert([source[0] - uv[0], source[1] - uv[1]]);
        self.ops.push(VanishingOp::Clone {
            from: [uv[0] + offset[0], uv[1] + offset[1]],
            to: uv,
            radius: self.radius,
        });
        self.last_dab = Some(uv);
        self.dirty = true;
        true
    }

    /// The plane's width over its height, estimated from its mean side
    /// lengths in document pixels.
    fn plane_aspect(&self) -> f32 {
        let c = self.plane.corners;
        let len = |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
        let across = 0.5 * (len(c[0], c[1]) + len(c[3], c[2]));
        let down = 0.5 * (len(c[0], c[3]) + len(c[1], c[2]));
        if down > 0.0 && across > 0.0 {
            across / down
        } else {
            1.0
        }
    }

    /// Paste the clipboard centred on document point `p`. Refused off the
    /// plane or with nothing to paste.
    pub fn paste_at(&mut self, p: [f32; 2]) -> bool {
        let (Some(uv), Some(image)) = (self.on_plane(p), self.paste.clone()) else {
            return false;
        };
        let (iw, ih) = image.dimensions();
        let w = self.paste_size;
        let h = w * ih as f32 / iw.max(1) as f32 * self.plane_aspect();
        self.ops.push(VanishingOp::Paste {
            image,
            rect: [
                uv[0] - w / 2.0,
                uv[1] - h / 2.0,
                uv[0] + w / 2.0,
                uv[1] + h / 2.0,
            ],
        });
        self.dirty = true;
        true
    }

    /// Take back the last edit.
    pub fn undo_op(&mut self) {
        if self.ops.pop().is_some() {
            self.dirty = true;
        }
    }

    /// The whole session as it would commit.
    pub fn edit(&self) -> VanishingPointEdit {
        VanishingPointEdit {
            plane: self.plane,
            ops: self.ops.clone(),
        }
    }

    /// The preview: the session over the bounded copy, the plane scaled by
    /// the copy's scale.
    pub fn preview(&self) -> FilterBuffer {
        let k = self.source.dimensions().0 as f32 / self.width.max(1) as f32;
        VanishingPointEdit {
            plane: self.plane.scaled(k),
            ops: self.ops.clone(),
        }
        .apply(&self.source)
    }

    pub fn canvas_rect(&self) -> Option<egui::Rect> {
        self.canvas
    }

    /// What a confirm commits, when it would change something.
    pub fn confirm(&self) -> Option<VanishingPointSpec> {
        let spec = VanishingPointSpec {
            image_size: (self.width, self.height),
            edit: self.edit(),
        };
        (!spec.is_identity()).then_some(spec)
    }

    /// Why the primary action is unavailable.
    pub fn blocked_reason(&self) -> Option<String> {
        self.confirm()
            .is_none()
            .then(|| tr("ui.adjustment.blocked.identity").to_string())
    }

    /// Escape and Enter, without drawing — Escape wins.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<VanishingPointSpec> {
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
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<VanishingPointSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "vanishing-point",
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
                    self.undo_op();
                    DialogOutcome::Open
                }
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        ui.horizontal(|ui| {
            let mut mode = self.mode;
            if combo(
                ui,
                "vanishing-point-mode",
                &mut mode,
                &VanishingMode::ALL,
                |m| m.label().to_string(),
                |_| None,
            ) {
                self.set_mode(mode);
            }
            match self.mode {
                VanishingMode::Plane => {}
                VanishingMode::Clone => {
                    let mut r = self.radius;
                    ui.label("Radius");
                    if ui.add(egui::Slider::new(&mut r, 0.005..=0.5)).changed() {
                        self.set_radius(r);
                    }
                }
                VanishingMode::Paste => {
                    let mut s = self.paste_size;
                    ui.label("Size");
                    if ui.add(egui::Slider::new(&mut s, 0.05..=1.0)).changed() {
                        self.set_paste_size(s);
                    }
                }
            }
        });
        self.canvas_widget(ui);
        let blocked = self.blocked_reason();
        action_row(
            ui,
            tr("ui.adjustment.confirm"),
            blocked.as_deref(),
            &["Undo"],
        )
    }

    fn canvas_widget(&mut self, ui: &mut egui::Ui) {
        if self.texture.is_none() || self.dirty {
            let rgba = self.preview().to_rgba8();
            let (pw, ph) = self.source.dimensions();
            let image = egui::ColorImage::from_rgba_unmultiplied([pw as usize, ph as usize], &rgba);
            self.texture = Some(ui.ctx().load_texture(
                "vanishing-point-preview",
                image,
                egui::TextureOptions::LINEAR,
            ));
            self.dirty = false;
        }
        let Some(texture) = self.texture.clone() else {
            return;
        };
        let (pw, ph) = self.source.dimensions();
        let long = sizes::preview_column_width();
        let scale = long / pw.max(ph).max(1) as f32;
        let size = egui::Vec2::new(pw as f32 * scale, ph as f32 * scale);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let response = ui.interact(
            rect,
            egui::Id::new(("raster-studio-vanishing-point", "canvas")),
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
        let alt = ui.input(|i| i.modifiers.alt);

        let pressed = response.drag_started() || response.clicked();
        if let Some(pos) = response.interact_pointer_pos() {
            let p = to_doc(pos);
            match self.mode {
                VanishingMode::Plane => {
                    if pressed {
                        self.dragging_corner = self
                            .plane
                            .corners
                            .iter()
                            .enumerate()
                            .map(|(i, c)| {
                                (i, ((c[0] - p[0]).powi(2) + (c[1] - p[1]).powi(2)).sqrt())
                            })
                            .filter(|(_, d)| *d <= pick)
                            .min_by(|a, b| a.1.total_cmp(&b.1))
                            .map(|(i, _)| i);
                    }
                    if response.dragged() {
                        if let Some(i) = self.dragging_corner {
                            self.set_corner(i, p);
                            ui.ctx().request_repaint();
                        }
                    }
                }
                VanishingMode::Clone => {
                    if pressed && alt {
                        self.set_clone_source(p);
                    } else if pressed {
                        self.clone_at(p);
                    } else if response.dragged() && !alt {
                        let far_enough = match (self.last_dab, self.on_plane(p)) {
                            (Some(last), Some(uv)) => {
                                ((uv[0] - last[0]).powi(2) + (uv[1] - last[1]).powi(2)).sqrt()
                                    >= self.radius * 0.25
                            }
                            (None, Some(_)) => true,
                            _ => false,
                        };
                        if far_enough {
                            self.clone_at(p);
                            ui.ctx().request_repaint();
                        }
                    }
                }
                VanishingMode::Paste => {
                    if response.clicked() {
                        self.paste_at(p);
                    }
                }
            }
        }
        if response.drag_stopped() || !response.is_pointer_button_down_on() {
            self.dragging_corner = None;
        }

        let t = current_tokens(ui);
        let painter = ui.painter_at(rect);
        let valid = self.plane.is_valid();
        let line_color = if valid {
            color32(t.palette.color(ColorRole::Accent))
        } else {
            color32(t.palette.color(ColorRole::Danger))
        };
        let grid = egui::Stroke::new(t.borders.hairline, line_color);
        if valid {
            for [a, b] in self.plane.grid_lines(VANISHING_GRID_DIVISIONS) {
                painter.line_segment([to_screen(a), to_screen(b)], grid);
            }
        }
        let c = self.plane.corners.map(to_screen);
        for i in 0..4 {
            painter.line_segment([c[i], c[(i + 1) % 4]], grid);
        }
        let ring = egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::TextOnAccent)),
        );
        for corner in c {
            painter.circle_filled(corner, handle, line_color);
            painter.circle_stroke(corner, handle, ring);
        }
        if let (VanishingMode::Clone, Some(source)) = (self.mode, self.clone_source) {
            if let Some(p) = self.plane.to_image(source) {
                painter.circle_stroke(to_screen(p), handle, ring);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backdrop() -> FilterBuffer {
        FilterBuffer::filled(120, 100, [0.0, 0.0, 1.0, 1.0]).unwrap()
    }

    fn floor(dialog: &mut VanishingPointDialog) {
        for (i, p) in [[40.0, 20.0], [80.0, 20.0], [110.0, 90.0], [10.0, 90.0]]
            .into_iter()
            .enumerate()
        {
            dialog.set_corner(i, p);
        }
    }

    #[test]
    fn a_paste_lands_through_the_planes_homography_and_confirms_as_one_spec() {
        let src = backdrop();
        let red = FilterBuffer::filled(8, 8, [1.0, 0.0, 0.0, 1.0]).unwrap();
        let mut dialog = VanishingPointDialog::new(&src, Some(red));
        floor(&mut dialog);
        assert!(dialog.blocked_reason().is_some(), "no edit yet");
        dialog.set_mode(VanishingMode::Paste);
        dialog.set_paste_size(0.5);
        let centre = dialog.plane().to_image([0.5, 0.5]).unwrap();
        assert!(dialog.paste_at(centre));
        assert!(!dialog.paste_at([1.0, 1.0]), "off the plane");
        let spec = match dialog.resolve(DialogKeys::CONFIRM) {
            DialogOutcome::Confirmed(spec) => spec,
            other => panic!("Enter did not commit: {other:?}"),
        };
        assert_eq!(spec.image_size, (120, 100));
        let out = spec.apply(&src);
        let plane = dialog.plane();
        let VanishingOp::Paste { rect, .. } = spec.edit.ops[0].clone() else {
            panic!("not a paste");
        };
        // Each corner of the pasted rectangle, just inside, is red where the
        // homography puts it.
        let inset = 0.02;
        for uv in [
            [rect[0] + inset, rect[1] + inset],
            [rect[2] - inset, rect[1] + inset],
            [rect[2] - inset, rect[3] - inset],
            [rect[0] + inset, rect[3] - inset],
        ] {
            let p = plane.to_image(uv).unwrap();
            assert_eq!(
                out.get(p[0] as u32, p[1] as u32),
                [1.0, 0.0, 0.0, 1.0],
                "{uv:?}"
            );
        }
        assert_eq!(
            out.get(2, 2),
            [0.0, 0.0, 1.0, 1.0],
            "off the plane untouched"
        );
        dialog.undo_op();
        assert!(dialog.ops().is_empty());
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
    }

    #[test]
    fn clone_needs_a_source_and_keeps_its_offset() {
        let mut src = backdrop();
        for y in 0..100 {
            for x in 0..60 {
                src.set(x, y, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        let mut dialog = VanishingPointDialog::new(&src, None);
        floor(&mut dialog);
        dialog.set_mode(VanishingMode::Clone);
        let plane = dialog.plane();
        let at = |uv: [f32; 2]| plane.to_image(uv).unwrap();
        assert!(!dialog.clone_at(at([0.8, 0.6])), "no source yet");
        assert!(dialog.set_clone_source(at([0.2, 0.6])));
        assert!(dialog.clone_at(at([0.8, 0.6])));
        assert!(dialog.clone_at(at([0.8, 0.7])));
        match &dialog.ops()[1] {
            VanishingOp::Clone { from, to, .. } => {
                assert!((from[0] - 0.2).abs() < 1e-4 && (to[1] - 0.7).abs() < 1e-4);
                assert!((from[1] - 0.7).abs() < 1e-4, "aligned offset");
            }
            other => panic!("{other:?}"),
        }
        let out = dialog.confirm().unwrap().apply(&src);
        let p = at([0.8, 0.6]);
        assert!(
            out.get(p[0] as u32, p[1] as u32)[0] > 0.99,
            "white cloned in"
        );
    }

    #[test]
    fn dragging_a_drawn_corner_moves_the_plane() {
        let mut dialog = VanishingPointDialog::new(&backdrop(), None);
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1400.0, 900.0));
        let run =
            |ctx: &egui::Context, dialog: &mut VanishingPointDialog, events: Vec<egui::Event>| {
                let input = egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                };
                let mut outcome = DialogOutcome::Open;
                let _ = ctx.run(input, |ctx| outcome = dialog.show(ctx));
                outcome
            };
        run(&ctx, &mut dialog, Vec::new());
        run(&ctx, &mut dialog, Vec::new());
        let rect = dialog.canvas_rect().expect("the canvas was drawn");
        let k = rect.width() / 120.0;
        let at = |p: [f32; 2]| rect.min + egui::Vec2::new(p[0], p[1]) * k;
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let start = dialog.plane().corners[0];
        let s = at(start);
        run(
            &ctx,
            &mut dialog,
            vec![egui::Event::PointerMoved(s), button(s, true)],
        );
        for step in 1..=5 {
            let p = at([start[0] + 2.0 * step as f32, start[1] - step as f32]);
            run(&ctx, &mut dialog, vec![egui::Event::PointerMoved(p)]);
        }
        let end = at([start[0] + 10.0, start[1] - 5.0]);
        run(&ctx, &mut dialog, vec![button(end, false)]);
        let moved = dialog.plane().corners[0];
        assert!(
            (moved[0] - (start[0] + 10.0)).abs() < 1.5 && (moved[1] - (start[1] - 5.0)).abs() < 1.5,
            "the corner followed the drag: {start:?} -> {moved:?}"
        );
        assert!(dialog.plane().is_valid());
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = VanishingPointDialog::new(&backdrop(), None);
            assert!(dialog.show(ctx).is_open());
            dialog.set_mode(VanishingMode::Clone);
            assert!(dialog.show(ctx).is_open());
        });
    }
}
