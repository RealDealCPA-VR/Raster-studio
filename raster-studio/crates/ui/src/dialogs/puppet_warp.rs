//! Edit ▸ Puppet Warp — pins on a mesh over the layer's ink (W7-H).
//!
//! Opening the dialog lays a [`PuppetMesh`] over every pixel of the active
//! layer that holds ink (see [`PuppetMesh::from_alpha`]). A click on the mesh
//! adds a pin at the nearest vertex; dragging a pin moves it and the mesh
//! follows, as-rigid-as-possible or by linear blend ([`PuppetMode`]); a
//! secondary click removes a pin. The preview is the warp over a bounded copy
//! of the layer.
//!
//! Enter (or the primary button) commits: the confirmation is a
//! [`PuppetWarpSpec`] — the mesh and its deformed vertices — which the shell
//! parks for the `PuppetWarp` menu arm, and that arm warps the
//! full-resolution layer as one undoable step. Escape writes nothing.
//!
//! No [`super::chrome::Dialog`] impl, for the reason Trim and Liquify give.

use design::tokens::palette::ColorRole;
use design::tokens::Space;
use design::{color32, current_tokens};
use egui::{Context, Sense};
use filters::puppet::{MeshDensity, Pin, PuppetMesh, PuppetMode};
use filters::FilterBuffer;

use super::chrome::{action_row, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth};
use super::controls::{checkbox_row, combo};
use super::liquify::{bounded_copy, PREVIEW_MAX_SIDE};
use super::sizes;
use crate::strings::tr;

/// A confirmed Puppet Warp: the mesh and where its vertices went.
#[derive(Clone, PartialEq, Debug)]
pub struct PuppetWarpSpec {
    pub mesh: PuppetMesh,
    pub deformed: Vec<[f32; 2]>,
}

impl PuppetWarpSpec {
    /// Whether no vertex moved.
    pub fn is_identity(&self) -> bool {
        self.deformed.as_slice() == self.mesh.rest()
    }

    /// The warp applied to `src` (any size; see [`PuppetMesh::warp`]).
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        self.mesh.warp(src, &self.deformed)
    }
}

/// Edit ▸ Puppet Warp.
pub struct PuppetWarpDialog {
    /// The layer's coverage, one byte per document pixel, kept to rebuild
    /// the mesh when the density changes.
    alpha: Vec<u8>,
    width: u32,
    height: u32,
    source: FilterBuffer,
    density: MeshDensity,
    mode: PuppetMode,
    mesh: Option<PuppetMesh>,
    pins: Vec<Pin>,
    deformed: Vec<[f32; 2]>,
    show_mesh: bool,
    /// The pin being dragged.
    dragging: Option<usize>,
    texture: Option<egui::TextureHandle>,
    dirty: bool,
    canvas: Option<egui::Rect>,
}

impl PuppetWarpDialog {
    /// Over the active layer's full-resolution pixels.
    pub fn new(layer: &FilterBuffer) -> Self {
        let (width, height) = layer.dimensions();
        let alpha: Vec<u8> = layer
            .pixels()
            .iter()
            .map(|p| (p[3].clamp(0.0, 1.0) * 255.0).round() as u8)
            .collect();
        let mut dialog = Self {
            alpha,
            width,
            height,
            source: bounded_copy(layer, PREVIEW_MAX_SIDE),
            density: MeshDensity::default(),
            mode: PuppetMode::default(),
            mesh: None,
            pins: Vec::new(),
            deformed: Vec::new(),
            show_mesh: true,
            dragging: None,
            texture: None,
            dirty: true,
            canvas: None,
        };
        dialog.rebuild_mesh();
        dialog
    }

    fn rebuild_mesh(&mut self) {
        self.mesh = PuppetMesh::from_alpha(&self.alpha, self.width, self.height, self.density);
        self.pins.clear();
        self.deformed = self
            .mesh
            .as_ref()
            .map(|m| m.rest().to_vec())
            .unwrap_or_default();
        self.dragging = None;
        self.dirty = true;
    }

    fn redeform(&mut self) {
        if let Some(mesh) = &self.mesh {
            self.deformed = mesh.deform(&self.pins, self.mode);
        }
        self.dirty = true;
    }

    /// The title, which is the menu row's.
    pub fn title(&self) -> String {
        crate::menu::MenuAction::PuppetWarp.label()
    }

    pub fn mesh(&self) -> Option<&PuppetMesh> {
        self.mesh.as_ref()
    }

    pub fn pins(&self) -> &[Pin] {
        &self.pins
    }

    pub fn set_mode(&mut self, mode: PuppetMode) {
        self.mode = mode;
        self.redeform();
    }

    /// Change the mesh density. The mesh is rebuilt, so the pins go.
    pub fn set_density(&mut self, density: MeshDensity) {
        if density != self.density {
            self.density = density;
            self.rebuild_mesh();
        }
    }

    /// Add a pin at document point `p`, holding the vertex nearest it in the
    /// current pose. Refused (returns `None`) off the mesh or on a vertex
    /// already pinned.
    pub fn add_pin(&mut self, p: [f32; 2]) -> Option<usize> {
        let mesh = self.mesh.as_ref()?;
        let (vertex, dist) = PuppetMesh::nearest_vertex(&self.deformed, p)?;
        if dist > mesh.spacing() || self.pins.iter().any(|pin| pin.vertex == vertex) {
            return None;
        }
        self.pins.push(Pin {
            vertex,
            at: self.deformed[vertex as usize],
        });
        self.dirty = true;
        Some(self.pins.len() - 1)
    }

    /// Move pin `index` to document point `p`.
    pub fn move_pin(&mut self, index: usize, p: [f32; 2]) {
        if let Some(pin) = self.pins.get_mut(index) {
            pin.at = p;
            self.redeform();
        }
    }

    /// Remove pin `index`.
    pub fn remove_pin(&mut self, index: usize) {
        if index < self.pins.len() {
            self.pins.remove(index);
            self.redeform();
        }
    }

    /// Clear every pin.
    pub fn reset(&mut self) {
        self.pins.clear();
        self.redeform();
    }

    /// The pin within `radius` document pixels of `p`, nearest first.
    fn pin_near(&self, p: [f32; 2], radius: f32) -> Option<usize> {
        self.pins
            .iter()
            .enumerate()
            .map(|(i, pin)| {
                (
                    i,
                    ((pin.at[0] - p[0]).powi(2) + (pin.at[1] - p[1]).powi(2)).sqrt(),
                )
            })
            .filter(|(_, d)| *d <= radius)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// The preview: the warp over the bounded copy.
    pub fn preview(&self) -> FilterBuffer {
        match &self.mesh {
            Some(mesh) => mesh.warp(&self.source, &self.deformed),
            None => self.source.clone(),
        }
    }

    pub fn canvas_rect(&self) -> Option<egui::Rect> {
        self.canvas
    }

    /// The spec a confirmation hands over; `None` when the layer has no ink
    /// to lay a mesh on.
    pub fn confirm(&self) -> Option<PuppetWarpSpec> {
        let mesh = self.mesh.clone()?;
        Some(PuppetWarpSpec {
            mesh,
            deformed: self.deformed.clone(),
        })
    }

    /// Why the primary action is unavailable.
    pub fn blocked_reason(&self) -> Option<String> {
        self.mesh
            .is_none()
            .then(|| tr("ui.docks.properties.nothing.to.measure").to_string())
    }

    /// Escape and Enter, without drawing — Escape wins.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<PuppetWarpSpec> {
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
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<PuppetWarpSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let title = self.title();
        let drawn = modal(
            ctx,
            "puppet-warp",
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
        let (mode, density) = (self.mode, self.density);
        ui.horizontal(|ui| {
            ui.label("Mode");
            combo(
                ui,
                "puppet-warp-mode",
                &mut self.mode,
                &PuppetMode::ALL,
                |m| m.label().to_string(),
                |_| None,
            );
            ui.label(tr("ui.adjustment.density"));
            combo(
                ui,
                "puppet-warp-density",
                &mut self.density,
                &MeshDensity::ALL,
                |d| d.label().to_string(),
                |_| None,
            );
            checkbox_row(ui, "Mesh", &mut self.show_mesh);
        });
        if density != self.density {
            let wanted = self.density;
            self.density = density;
            self.set_density(wanted);
        }
        if mode != self.mode {
            self.redeform();
        }
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
                "puppet-warp-preview",
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
            egui::Id::new(("raster-studio-puppet-warp", "canvas")),
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
        let pin_radius = Space::Small.pt();
        let pick = (pin_radius * 2.0) / per_px;

        if response.secondary_clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                if let Some(i) = self.pin_near(to_doc(pos), pick) {
                    self.remove_pin(i);
                }
            }
        } else if response.drag_started() || response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let p = to_doc(pos);
                self.dragging = self.pin_near(p, pick).or_else(|| self.add_pin(p));
            }
        }
        if response.dragged() {
            if let (Some(i), Some(pos)) = (self.dragging, response.interact_pointer_pos()) {
                self.move_pin(i, to_doc(pos));
                ui.ctx().request_repaint();
            }
        }
        if response.drag_stopped() || !response.is_pointer_button_down_on() {
            self.dragging = None;
        }

        let t = current_tokens(ui);
        let painter = ui.painter_at(rect);
        if self.show_mesh {
            if let Some(mesh) = &self.mesh {
                let edge = egui::Stroke::new(
                    t.borders.hairline,
                    color32(t.palette.color(ColorRole::ControlStroke)),
                );
                for tri in mesh.triangles() {
                    let [a, b, c] = tri.map(|i| to_screen(self.deformed[i as usize]));
                    painter.line_segment([a, b], edge);
                    painter.line_segment([b, c], edge);
                    painter.line_segment([c, a], edge);
                }
            }
        }
        let fill = color32(t.palette.color(ColorRole::Accent));
        let ring = egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::TextOnAccent)),
        );
        for pin in &self.pins {
            let c = to_screen(pin.at);
            painter.circle_filled(c, pin_radius, fill);
            painter.circle_stroke(c, pin_radius, ring);
        }
    }
}

impl std::fmt::Debug for PuppetWarpDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PuppetWarpDialog")
            .field("size", &(self.width, self.height))
            .field("mode", &self.mode)
            .field("density", &self.density)
            .field("pins", &self.pins)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An opaque 40x16 bar on a transparent 64x64 layer.
    fn bar() -> FilterBuffer {
        let mut buf = FilterBuffer::transparent(64, 64).unwrap();
        for y in 24..40 {
            for x in 12..52 {
                buf.set(x, y, [x as f32 / 64.0, 0.5, 0.2, 1.0]);
            }
        }
        buf
    }

    #[test]
    fn pins_not_moved_confirm_the_identity() {
        let src = bar();
        let mut dialog = PuppetWarpDialog::new(&src);
        assert!(dialog.add_pin([14.0, 32.0]).is_some());
        assert!(dialog.add_pin([50.0, 32.0]).is_some());
        assert!(dialog.add_pin([2.0, 2.0]).is_none(), "no pin off the ink");
        assert_eq!(dialog.preview(), src);
        let spec = dialog.confirm().unwrap();
        assert!(spec.is_identity());
        assert_eq!(spec.apply(&src), src);
    }

    #[test]
    fn dragging_a_pin_deforms_the_preview_and_the_spec() {
        let src = bar();
        let mut dialog = PuppetWarpDialog::new(&src);
        dialog.add_pin([14.0, 32.0]).unwrap();
        let right = dialog.add_pin([50.0, 32.0]).unwrap();
        let at = dialog.pins()[right].at;
        dialog.move_pin(right, [at[0], at[1] + 10.0]);
        let preview = dialog.preview();
        assert_ne!(preview, src);
        match dialog.resolve(DialogKeys::CONFIRM) {
            DialogOutcome::Confirmed(spec) => {
                assert!(!spec.is_identity());
                assert_eq!(spec.apply(&src), preview);
            }
            other => panic!("Enter did not commit: {other:?}"),
        }
        dialog.reset();
        assert!(dialog.confirm().unwrap().is_identity());
        assert_eq!(dialog.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
    }

    #[test]
    fn a_layer_with_no_ink_cannot_commit_and_says_why() {
        let empty = FilterBuffer::transparent(16, 16).unwrap();
        let dialog = PuppetWarpDialog::new(&empty);
        assert!(dialog.confirm().is_none());
        assert!(dialog.blocked_reason().is_some());
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
    }

    #[test]
    fn clicking_and_dragging_on_the_drawn_canvas_places_and_moves_pins() {
        let src = bar();
        let mut dialog = PuppetWarpDialog::new(&src);
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1400.0, 900.0));
        let run = |ctx: &egui::Context, dialog: &mut PuppetWarpDialog, events: Vec<egui::Event>| {
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
        let at = |x: f32, y: f32| rect.min + egui::Vec2::new(x, y) * (rect.width() / 64.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        // A click on the ink places a pin.
        for (x, y) in [(14.0, 32.0), (50.0, 32.0)] {
            let p = at(x, y);
            run(
                &ctx,
                &mut dialog,
                vec![egui::Event::PointerMoved(p), button(p, true)],
            );
            run(&ctx, &mut dialog, vec![button(p, false)]);
            run(&ctx, &mut dialog, Vec::new());
        }
        assert_eq!(dialog.pins().len(), 2, "two clicks, two pins");
        // Dragging the right pin down deforms the mesh.
        let pin = dialog.pins()[1].at;
        let start = at(pin[0], pin[1]);
        run(
            &ctx,
            &mut dialog,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        for step in 1..=6 {
            let p = at(pin[0], pin[1] + 2.0 * step as f32);
            run(&ctx, &mut dialog, vec![egui::Event::PointerMoved(p)]);
        }
        let end = at(pin[0], pin[1] + 12.0);
        run(&ctx, &mut dialog, vec![button(end, false)]);
        assert_eq!(
            dialog.pins().len(),
            2,
            "the drag moved a pin, it did not add one"
        );
        let moved = dialog.pins()[1].at;
        assert!(
            moved[1] > pin[1] + 6.0,
            "the pin followed the drag: {pin:?} -> {moved:?}"
        );
        assert!(!dialog.confirm().unwrap().is_identity());
    }
}
