//! Filter ▸ Blur Gallery ▸ Field / Iris / Tilt-Shift / Path / Spin (W9-O).
//!
//! One dialog for the five gallery blurs. It holds a
//! [`filters::blur_gallery::BlurGallery`] in *document* pixels and a bounded
//! copy of the active layer to preview it on; the preview is the same blur
//! [`BlurGallery::scaled`] down to the copy. The blur's geometry is edited
//! with handles drawn over the preview — pins (Field), centre / radii / focus
//! (Iris), centre / focus line / transition line / rotation (Tilt-Shift), the
//! path's points (Path), centre / radii (Spin) — and its amounts with the
//! sliders above it.
//!
//! Nothing touches the document until the dialog confirms: the confirmation
//! is a [`BlurGallerySpec`], which the shell parks for the
//! `MenuAction::BlurGallery` arm, and that arm applies the blur to the
//! full-resolution layer as one undoable step. Cancel writes nothing. The
//! road is Liquify's (see [`super::liquify`]): no `Dialog` impl, because a
//! blur is not a `DialogAction`.

use design::tokens::palette::ColorRole;
use design::tokens::Space;
use design::{color32, current_tokens};
use egui::{Context, Sense};
use filters::blur_gallery::{
    BlurGallery, BlurGalleryKind, FieldPin, MAX_FIELD_PINS, MAX_GALLERY_BLUR, MAX_PATH_POINTS,
    MAX_PATH_SPEED, MAX_SPIN_ANGLE,
};
use filters::FilterBuffer;

use super::chrome::{action_row, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth};
use super::controls::checkbox_row;
use super::liquify::{bounded_copy, PREVIEW_MAX_SIDE};
use super::sizes;
use crate::strings::tr;

/// A confirmed gallery blur: what to apply, and the document size it was
/// laid out on.
#[derive(Clone, PartialEq, Debug)]
pub struct BlurGallerySpec {
    pub image_size: (u32, u32),
    pub gallery: BlurGallery,
}

impl BlurGallerySpec {
    pub fn kind(&self) -> BlurGalleryKind {
        self.gallery.kind()
    }

    /// Whether the blur changes nothing.
    pub fn is_identity(&self) -> bool {
        self.gallery.is_identity()
    }

    /// The blur applied to `src` (document-sized).
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        self.gallery.apply(src)
    }
}

/// One on-image handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handle {
    /// Field Blur pin `i`.
    Pin(usize),
    /// Path Blur point `i`.
    Point(usize),
    /// The centre of an Iris, Tilt-Shift or Spin blur.
    Center,
    /// The first half-axis of an Iris or Spin ellipse; also turns it.
    RadiusX,
    /// The second half-axis of an Iris or Spin ellipse.
    RadiusY,
    /// Iris: where the sharp region ends. Tilt-Shift: the focus line.
    Focus,
    /// Tilt-Shift: where the ramp to full blur ends.
    Transition,
    /// Tilt-Shift: the band's angle.
    Rotate,
}

fn rotate(v: [f32; 2], deg: f32) -> [f32; 2] {
    let (s, c) = deg.to_radians().sin_cos();
    [v[0] * c - v[1] * s, v[0] * s + v[1] * c]
}

fn add(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn len(v: [f32; 2]) -> f32 {
    (v[0] * v[0] + v[1] * v[1]).sqrt()
}

fn dot(a: [f32; 2], b: [f32; 2]) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}

/// Filter ▸ Blur Gallery ▸ one of the five.
pub struct BlurGalleryDialog {
    gallery: BlurGallery,
    image_size: (u32, u32),
    /// The active layer's pixels, bounded for the preview.
    source: FilterBuffer,
    texture: Option<egui::TextureHandle>,
    dirty: bool,
    dragging: Option<Handle>,
    /// The Field pin the Blur slider edits.
    selected_pin: usize,
    /// Where the canvas was drawn last frame (screen points).
    canvas: Option<egui::Rect>,
}

impl BlurGalleryDialog {
    /// Over the active layer's full-resolution pixels, with the kind's
    /// default geometry laid out on the document.
    pub fn new(kind: BlurGalleryKind, layer: &FilterBuffer) -> Self {
        let (w, h) = layer.dimensions();
        Self {
            gallery: BlurGallery::default_for(kind, w, h),
            image_size: (w, h),
            source: bounded_copy(layer, PREVIEW_MAX_SIDE),
            texture: None,
            dirty: true,
            dragging: None,
            selected_pin: 0,
            canvas: None,
        }
    }

    pub fn kind(&self) -> BlurGalleryKind {
        self.gallery.kind()
    }

    /// The title, which is the menu row's.
    pub fn title(&self) -> String {
        crate::menu::MenuAction::BlurGallery(self.kind()).label()
    }

    pub fn gallery(&self) -> &BlurGallery {
        &self.gallery
    }

    /// Replace the blur (same kind or not).
    pub fn set_gallery(&mut self, gallery: BlurGallery) {
        self.gallery = gallery;
        self.selected_pin = 0;
        self.dirty = true;
    }

    /// Back to the kind's default.
    pub fn reset(&mut self) {
        let (w, h) = self.image_size;
        self.set_gallery(BlurGallery::default_for(self.kind(), w, h));
        self.dragging = None;
    }

    /// Preview texels per document pixel.
    fn preview_scale(&self) -> f32 {
        let (pw, _) = self.source.dimensions();
        pw as f32 / self.image_size.0.max(1) as f32
    }

    /// The preview: the blur, scaled to the bounded copy, over it.
    pub fn preview(&self) -> FilterBuffer {
        self.gallery
            .scaled(self.preview_scale())
            .apply(&self.source)
    }

    /// Where the canvas was drawn in the last frame.
    pub fn canvas_rect(&self) -> Option<egui::Rect> {
        self.canvas
    }

    /// Every handle and where it sits, document pixels.
    pub fn handles(&self) -> Vec<(Handle, [f32; 2])> {
        match &self.gallery {
            BlurGallery::Field(f) => f
                .pins
                .iter()
                .enumerate()
                .map(|(i, p)| (Handle::Pin(i), p.pos))
                .collect(),
            BlurGallery::Path(p) => p
                .points
                .iter()
                .enumerate()
                .map(|(i, q)| (Handle::Point(i), *q))
                .collect(),
            BlurGallery::Iris(i) => {
                let f = i.focus.clamp(0.0, 1.0) * std::f32::consts::FRAC_1_SQRT_2;
                vec![
                    (Handle::Center, i.center),
                    (
                        Handle::RadiusX,
                        add(i.center, rotate([i.radii[0], 0.0], i.rotation_deg)),
                    ),
                    (
                        Handle::RadiusY,
                        add(i.center, rotate([0.0, i.radii[1]], i.rotation_deg)),
                    ),
                    (
                        Handle::Focus,
                        add(
                            i.center,
                            rotate([i.radii[0] * f, -i.radii[1] * f], i.rotation_deg),
                        ),
                    ),
                ]
            }
            BlurGallery::Spin(s) => vec![
                (Handle::Center, s.center),
                (
                    Handle::RadiusX,
                    add(s.center, rotate([s.radii[0], 0.0], s.rotation_deg)),
                ),
                (
                    Handle::RadiusY,
                    add(s.center, rotate([0.0, s.radii[1]], s.rotation_deg)),
                ),
            ],
            BlurGallery::TiltShift(t) => {
                let dir = rotate([1.0, 0.0], t.angle_deg);
                let n = rotate([0.0, 1.0], t.angle_deg);
                let arm = self.image_size.0.min(self.image_size.1).max(1) as f32 * 0.3;
                vec![
                    (Handle::Center, t.center),
                    (
                        Handle::Focus,
                        add(t.center, [n[0] * t.focus, n[1] * t.focus]),
                    ),
                    (
                        Handle::Transition,
                        add(
                            t.center,
                            [
                                n[0] * (t.focus + t.transition),
                                n[1] * (t.focus + t.transition),
                            ],
                        ),
                    ),
                    (Handle::Rotate, add(t.center, [dir[0] * arm, dir[1] * arm])),
                ]
            }
        }
    }

    /// The handle nearest `at` within `pick` document pixels.
    pub fn handle_near(&self, at: [f32; 2], pick: f32) -> Option<Handle> {
        self.handles()
            .into_iter()
            .map(|(h, p)| (h, len(sub(p, at))))
            .filter(|(_, d)| *d <= pick)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(h, _)| h)
    }

    /// Move `handle` to `to` (document pixels), the route a drag takes.
    pub fn drag_handle(&mut self, handle: Handle, to: [f32; 2]) {
        let (w, h) = self.image_size;
        let to = [to[0].clamp(0.0, w as f32), to[1].clamp(0.0, h as f32)];
        match (&mut self.gallery, handle) {
            (BlurGallery::Field(f), Handle::Pin(i)) => {
                if let Some(p) = f.pins.get_mut(i) {
                    p.pos = to;
                    self.selected_pin = i;
                }
            }
            (BlurGallery::Path(p), Handle::Point(i)) => {
                if let Some(q) = p.points.get_mut(i) {
                    *q = to;
                }
            }
            (BlurGallery::Iris(i), Handle::Center) => i.center = to,
            (BlurGallery::Spin(s), Handle::Center) => s.center = to,
            (BlurGallery::TiltShift(t), Handle::Center) => t.center = to,
            (BlurGallery::Iris(i), Handle::RadiusX) => {
                let v = sub(to, i.center);
                i.radii[0] = len(v).max(1.0);
                i.rotation_deg = v[1].atan2(v[0]).to_degrees();
            }
            (BlurGallery::Spin(s), Handle::RadiusX) => {
                let v = sub(to, s.center);
                s.radii[0] = len(v).max(1.0);
                s.rotation_deg = v[1].atan2(v[0]).to_degrees();
            }
            (BlurGallery::Iris(i), Handle::RadiusY) => {
                i.radii[1] = len(sub(to, i.center)).max(1.0);
            }
            (BlurGallery::Spin(s), Handle::RadiusY) => {
                s.radii[1] = len(sub(to, s.center)).max(1.0);
            }
            (BlurGallery::Iris(i), Handle::Focus) => {
                let local = rotate(sub(to, i.center), -i.rotation_deg);
                let r = ((local[0] / i.radii[0].max(1.0)).powi(2)
                    + (local[1] / i.radii[1].max(1.0)).powi(2))
                .sqrt();
                i.focus = r.clamp(0.0, 0.99);
            }
            (BlurGallery::TiltShift(t), Handle::Focus) => {
                let n = rotate([0.0, 1.0], t.angle_deg);
                t.focus = dot(sub(to, t.center), n).max(0.0);
            }
            (BlurGallery::TiltShift(t), Handle::Transition) => {
                let n = rotate([0.0, 1.0], t.angle_deg);
                t.transition = (dot(sub(to, t.center), n) - t.focus).max(0.0);
            }
            (BlurGallery::TiltShift(t), Handle::Rotate) => {
                let v = sub(to, t.center);
                if len(v) > 0.5 {
                    t.angle_deg = v[1].atan2(v[0]).to_degrees();
                }
            }
            _ => return,
        }
        self.dirty = true;
    }

    /// A press on no handle: Field adds a pin, Path appends a point. Returns
    /// the new handle, if one was made.
    pub fn press_empty(&mut self, at: [f32; 2]) -> Option<Handle> {
        let made = match &mut self.gallery {
            BlurGallery::Field(f) if f.pins.len() < MAX_FIELD_PINS => {
                let blur = f.pins.get(self.selected_pin).map_or(15.0, |p| p.blur);
                f.pins.push(FieldPin { pos: at, blur });
                self.selected_pin = f.pins.len() - 1;
                Some(Handle::Pin(f.pins.len() - 1))
            }
            BlurGallery::Path(p) if p.points.len() < MAX_PATH_POINTS => {
                p.points.push(at);
                Some(Handle::Point(p.points.len() - 1))
            }
            _ => None,
        };
        if made.is_some() {
            self.dirty = true;
        }
        made
    }

    /// Remove a pin or a path point (a secondary click on it).
    pub fn remove(&mut self, handle: Handle) {
        match (&mut self.gallery, handle) {
            (BlurGallery::Field(f), Handle::Pin(i)) if i < f.pins.len() => {
                f.pins.remove(i);
                self.selected_pin = 0;
            }
            (BlurGallery::Path(p), Handle::Point(i)) if i < p.points.len() => {
                p.points.remove(i);
            }
            _ => return,
        }
        self.dirty = true;
    }

    /// The spec a confirmation hands over. An identity blur is refused by
    /// the menu arm with a reason, not here.
    pub fn confirm(&self) -> Option<BlurGallerySpec> {
        Some(BlurGallerySpec {
            image_size: self.image_size,
            gallery: self.gallery.clone(),
        })
    }

    /// Escape and Enter, without drawing — Escape wins.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<BlurGallerySpec> {
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
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<BlurGallerySpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let title = self.title();
        let drawn = modal(
            ctx,
            "blur-gallery",
            &title,
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
        let before = self.gallery.clone();
        let selected = self.selected_pin;
        egui::Grid::new("blur-gallery-params")
            .num_columns(2)
            .show(ui, |ui| match &mut self.gallery {
                BlurGallery::Field(f) => {
                    if let Some(pin) = f.pins.get_mut(selected) {
                        ui.label("Blur");
                        ui.add(
                            egui::Slider::new(&mut pin.blur, 0.0..=MAX_GALLERY_BLUR)
                                .logarithmic(true),
                        );
                        ui.end_row();
                    }
                }
                BlurGallery::Iris(i) => {
                    ui.label("Blur");
                    ui.add(
                        egui::Slider::new(&mut i.blur, 0.0..=MAX_GALLERY_BLUR).logarithmic(true),
                    );
                    ui.end_row();
                    ui.label("Focus");
                    ui.add(egui::Slider::new(&mut i.focus, 0.0..=0.99));
                    ui.end_row();
                }
                BlurGallery::TiltShift(t) => {
                    ui.label("Blur");
                    ui.add(
                        egui::Slider::new(&mut t.blur, 0.0..=MAX_GALLERY_BLUR).logarithmic(true),
                    );
                    ui.end_row();
                    ui.label("Distortion");
                    ui.add(egui::Slider::new(&mut t.distortion, -1.0..=1.0));
                    ui.end_row();
                    ui.label("");
                    checkbox_row(ui, "Symmetric", &mut t.symmetric);
                    ui.end_row();
                }
                BlurGallery::Path(p) => {
                    ui.label("Speed");
                    ui.add(egui::Slider::new(&mut p.speed, 0.0..=MAX_PATH_SPEED).logarithmic(true));
                    ui.end_row();
                }
                BlurGallery::Spin(s) => {
                    ui.label("Angle");
                    ui.add(egui::Slider::new(&mut s.angle_deg, 0.0..=MAX_SPIN_ANGLE));
                    ui.end_row();
                    ui.label("Feather");
                    ui.add(egui::Slider::new(&mut s.feather, 0.0..=1.0));
                    ui.end_row();
                }
            });
        if before != self.gallery {
            self.dirty = true;
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
            let rgba = self.preview().to_rgba8();
            let (pw, ph) = self.source.dimensions();
            let image = egui::ColorImage::from_rgba_unmultiplied([pw as usize, ph as usize], &rgba);
            self.texture = Some(ui.ctx().load_texture(
                "blur-gallery-preview",
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
            egui::Id::new(("raster-studio-blur-gallery", "canvas")),
            Sense::click_and_drag(),
        );
        egui::Image::new((texture.id(), size)).paint_at(ui, rect);
        self.canvas = Some(rect);

        let per_px = rect.width() / self.image_size.0.max(1) as f32;
        let to_doc = |pos: egui::Pos2| -> [f32; 2] {
            let local = pos - rect.min;
            [local.x / per_px, local.y / per_px]
        };
        let to_screen =
            |p: [f32; 2]| -> egui::Pos2 { rect.min + egui::Vec2::new(p[0], p[1]) * per_px };
        let handle_radius = Space::Small.pt();
        let pick = (handle_radius * 2.0) / per_px;

        if response.secondary_clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                if let Some(h) = self.handle_near(to_doc(pos), pick) {
                    self.remove(h);
                }
            }
        } else if response.drag_started() || response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let p = to_doc(pos);
                self.dragging = self.handle_near(p, pick).or_else(|| self.press_empty(p));
                if let Some(Handle::Pin(i)) = self.dragging {
                    self.selected_pin = i;
                }
            }
        }
        if response.dragged() {
            if let (Some(h), Some(pos)) = (self.dragging, response.interact_pointer_pos()) {
                self.drag_handle(h, to_doc(pos));
                ui.ctx().request_repaint();
            }
        }
        if response.drag_stopped() || !response.is_pointer_button_down_on() {
            self.dragging = None;
        }

        let t = current_tokens(ui);
        let painter = ui.painter_at(rect);
        let guide = egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::Accent)),
        );
        let ellipse = |center: [f32; 2], radii: [f32; 2], rot: f32, k: f32| -> Vec<egui::Pos2> {
            (0..64)
                .map(|i| {
                    let a = i as f32 / 64.0 * std::f32::consts::TAU;
                    let v = rotate([radii[0] * k * a.cos(), radii[1] * k * a.sin()], rot);
                    to_screen(add(center, v))
                })
                .collect()
        };
        match &self.gallery {
            BlurGallery::Iris(i) => {
                painter.add(egui::Shape::closed_line(
                    ellipse(i.center, i.radii, i.rotation_deg, 1.0),
                    guide,
                ));
                painter.add(egui::Shape::closed_line(
                    ellipse(i.center, i.radii, i.rotation_deg, i.focus.clamp(0.0, 1.0)),
                    guide,
                ));
            }
            BlurGallery::Spin(s) => {
                painter.add(egui::Shape::closed_line(
                    ellipse(s.center, s.radii, s.rotation_deg, 1.0),
                    guide,
                ));
            }
            BlurGallery::TiltShift(ts) => {
                let dir = rotate([1.0, 0.0], ts.angle_deg);
                let n = rotate([0.0, 1.0], ts.angle_deg);
                let reach = (self.image_size.0 + self.image_size.1) as f32;
                for offset in [
                    ts.focus,
                    -ts.focus,
                    ts.focus + ts.transition,
                    -(ts.focus + ts.transition),
                ] {
                    let base = add(ts.center, [n[0] * offset, n[1] * offset]);
                    painter.line_segment(
                        [
                            to_screen(sub(base, [dir[0] * reach, dir[1] * reach])),
                            to_screen(add(base, [dir[0] * reach, dir[1] * reach])),
                        ],
                        guide,
                    );
                }
            }
            BlurGallery::Path(p) => {
                let line: Vec<egui::Pos2> = p.points.iter().map(|q| to_screen(*q)).collect();
                if line.len() >= 2 {
                    painter.add(egui::Shape::line(line, guide));
                }
            }
            BlurGallery::Field(_) => {}
        }
        let fill = color32(t.palette.color(ColorRole::Accent));
        let ring = egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::TextOnAccent)),
        );
        for (_, at) in self.handles() {
            let c = to_screen(at);
            painter.circle_filled(c, handle_radius, fill);
            painter.circle_stroke(c, handle_radius, ring);
        }
    }
}

impl std::fmt::Debug for BlurGalleryDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlurGalleryDialog")
            .field("gallery", &self.gallery)
            .field("image", &self.image_size)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::super::chrome::test_support::frame;
    use super::*;

    fn checker(w: u32, h: u32) -> FilterBuffer {
        let mut buf = FilterBuffer::filled(w, h, [0.0, 0.0, 0.0, 1.0]).unwrap();
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 1.0 } else { 0.0 };
                buf.set(x, y, [v, v, v, 1.0]);
            }
        }
        buf
    }

    fn run(ctx: &egui::Context, dialog: &mut BlurGalleryDialog, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(1400.0, 900.0),
            )),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            let _ = dialog.show(ctx);
        });
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// Press at `from`, drag in steps to `to`, release — over real frames.
    fn drag(ctx: &egui::Context, dialog: &mut BlurGalleryDialog, from: egui::Pos2, to: egui::Pos2) {
        run(
            ctx,
            dialog,
            vec![egui::Event::PointerMoved(from), button(from, true)],
        );
        for step in 1..=6 {
            let p = from + (to - from) * (step as f32 / 6.0);
            run(ctx, dialog, vec![egui::Event::PointerMoved(p)]);
        }
        run(ctx, dialog, vec![button(to, false)]);
        run(ctx, dialog, Vec::new());
    }

    fn laid_out(dialog: &mut BlurGalleryDialog) -> (egui::Context, egui::Rect) {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        run(&ctx, dialog, Vec::new());
        run(&ctx, dialog, Vec::new());
        let rect = dialog.canvas_rect().expect("the canvas was drawn");
        (ctx, rect)
    }

    #[test]
    fn every_kind_opens_previews_and_confirms_its_blur() {
        let src = checker(64, 48);
        for kind in BlurGalleryKind::ALL {
            let mut dialog = BlurGalleryDialog::new(kind, &src);
            assert_eq!(dialog.kind(), kind);
            assert_eq!(dialog.title(), kind.label());
            assert_ne!(dialog.preview(), src, "{kind:?}: the preview blurs");
            let _ = frame(|ctx| dialog.show(ctx));
            assert!(dialog.canvas_rect().is_some());
            match dialog.resolve(DialogKeys::CONFIRM) {
                DialogOutcome::Confirmed(spec) => {
                    assert_eq!(spec.kind(), kind);
                    assert_eq!(spec.image_size, (64, 48));
                    // The preview is not bounded at this size: it is the
                    // full-resolution result exactly.
                    assert_eq!(spec.apply(&src), dialog.preview());
                }
                other => panic!("Enter did not confirm: {other:?}"),
            }
            assert_eq!(dialog.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
        }
    }

    #[test]
    fn dragging_the_iris_centre_on_the_drawn_canvas_moves_it() {
        let src = checker(64, 64);
        let mut dialog = BlurGalleryDialog::new(BlurGalleryKind::Iris, &src);
        let (ctx, rect) = laid_out(&mut dialog);
        let per_px = rect.width() / 64.0;
        let at = |x: f32, y: f32| rect.min + egui::Vec2::new(x, y) * per_px;
        // Every handle is drawn inside the canvas.
        for (_, p) in dialog.handles() {
            assert!(rect.expand(1.0).contains(at(p[0], p[1])), "{p:?}");
        }
        drag(&ctx, &mut dialog, at(32.0, 32.0), at(20.0, 24.0));
        let BlurGallery::Iris(i) = dialog.gallery() else {
            panic!("not an iris")
        };
        assert!(
            (i.center[0] - 20.0).abs() < 1.0 && (i.center[1] - 24.0).abs() < 1.0,
            "the centre followed the drag: {:?}",
            i.center
        );
    }

    #[test]
    fn clicking_empty_canvas_adds_a_field_pin_and_dragging_it_moves_it() {
        let src = checker(64, 64);
        let mut dialog = BlurGalleryDialog::new(BlurGalleryKind::Field, &src);
        let (ctx, rect) = laid_out(&mut dialog);
        let per_px = rect.width() / 64.0;
        let at = |x: f32, y: f32| rect.min + egui::Vec2::new(x, y) * per_px;
        drag(&ctx, &mut dialog, at(10.0, 10.0), at(12.0, 50.0));
        let BlurGallery::Field(f) = dialog.gallery() else {
            panic!("not a field")
        };
        assert_eq!(f.pins.len(), 2, "the press on empty canvas added a pin");
        let p = f.pins[1].pos;
        assert!(
            (p[0] - 12.0).abs() < 1.0 && (p[1] - 50.0).abs() < 1.0,
            "{p:?}"
        );
    }

    #[test]
    fn tilt_shift_handles_edit_the_band() {
        let src = checker(100, 100);
        let mut dialog = BlurGalleryDialog::new(BlurGalleryKind::TiltShift, &src);
        dialog.drag_handle(Handle::Focus, [50.0, 60.0]);
        dialog.drag_handle(Handle::Transition, [50.0, 75.0]);
        dialog.drag_handle(Handle::Rotate, [50.0, 90.0]);
        let BlurGallery::TiltShift(t) = dialog.gallery() else {
            panic!("not a tilt-shift")
        };
        assert!((t.focus - 10.0).abs() < 1e-3);
        assert!((t.transition - 15.0).abs() < 1e-3);
        assert!((t.angle_deg - 90.0).abs() < 1e-3);
        dialog.reset();
        assert_eq!(
            dialog.gallery(),
            &BlurGallery::default_for(BlurGalleryKind::TiltShift, 100, 100)
        );
    }

    #[test]
    fn the_preview_is_bounded_and_the_spec_is_document_sized() {
        let src = FilterBuffer::filled(1200, 600, [0.5; 4]).unwrap();
        let dialog = BlurGalleryDialog::new(BlurGalleryKind::Spin, &src);
        assert_eq!(dialog.source.dimensions(), (512, 256));
        assert_eq!(dialog.confirm().unwrap().image_size, (1200, 600));
    }
}
