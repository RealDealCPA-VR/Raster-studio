//! Filter ▸ Distort ▸ Displace… with an external displacement map (W10-D).
//!
//! Photoshop's Displace asks for a *second image* — the displacement map —
//! and moves every pixel of the layer by what that map holds: its red
//! channel drives the horizontal shift and its green the vertical, 128 is no
//! shift, 0 the full negative scale and 255 the full positive one
//! ([`filters::displace::encoded_map`]).
//!
//! The map is chosen from [`DisplaceMapSource`]s the host hands over when it
//! opens the dialog — every open document, flattened — plus any image file
//! loaded with the Load… button (the host answers
//! [`DisplaceMapDialog::take_file_request`] with a file dialog and calls
//! [`DisplaceMapDialog::load_map`]). The two Photoshop choices sit beside
//! the scales: how a map of another size is laid over the layer
//! ([`DisplaceFit`]: Stretch to Fit or Tile) and what a shift that leaves
//! the image reads ([`DisplaceEdges`]: Wrap Around or Repeat Edge Pixels).
//!
//! The preview is the real filter over a bounded copy of the layer, the
//! scales shrunk by the copy's own scale so a shift reads the same distance
//! on screen. Enter commits a [`DisplaceMapSpec`], which the shell parks for
//! the `Filter(Displace)` arm; that arm displaces the full-resolution layer
//! as one undoable step.

use egui::Context;
use filters::displace::{displace, encoded_map, DisplaceEdges, DisplaceFit};
use filters::FilterBuffer;

use super::chrome::{action_row, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth};
use super::controls::combo;
use super::liquify::bounded_copy;
use super::sizes;
use crate::menu::{FilterId, MenuAction};
use crate::strings::tr;

/// The longest side of the copy the preview displaces.
pub const DISPLACE_PREVIEW_SIDE: u32 = 256;

/// One map the dialog can choose: a name and the map as
/// [`filters::displace::displace`] reads it (see [`encoded_map`]).
#[derive(Clone, PartialEq, Debug)]
pub struct DisplaceMapSource {
    pub name: String,
    pub map: FilterBuffer,
}

impl DisplaceMapSource {
    /// A map from straight RGBA8 pixels — an open document's composite or a
    /// decoded file. `None` for an empty image or a short buffer.
    pub fn from_rgba8(
        name: impl Into<String>,
        width: u32,
        height: u32,
        rgba8: &[u8],
    ) -> Option<Self> {
        Some(Self {
            name: name.into(),
            map: encoded_map(width, height, rgba8)?,
        })
    }
}

/// A confirmed Displace: the map, the scales in document pixels, and the two
/// Photoshop choices.
#[derive(Clone, PartialEq, Debug)]
pub struct DisplaceMapSpec {
    /// The document size the dialog opened over.
    pub image_size: (u32, u32),
    pub map: FilterBuffer,
    pub scale_x: f32,
    pub scale_y: f32,
    pub fit: DisplaceFit,
    pub edges: DisplaceEdges,
}

impl DisplaceMapSpec {
    /// Whether applying would move nothing: both scales zero.
    pub fn is_identity(&self) -> bool {
        self.scale_x == 0.0 && self.scale_y == 0.0
    }

    /// The displacement applied to `src`.
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        displace(
            src,
            &self.map,
            self.scale_x,
            self.scale_y,
            self.fit,
            self.edges.sampling(),
        )
    }
}

/// The largest shift either scale may ask for, in pixels (Photoshop's range).
pub const DISPLACE_SCALE_LIMIT: f32 = 999.0;

/// Filter ▸ Distort ▸ Displace….
pub struct DisplaceMapDialog {
    proxy: FilterBuffer,
    image_size: (u32, u32),
    maps: Vec<DisplaceMapSource>,
    chosen: usize,
    scale_x: f32,
    scale_y: f32,
    fit: DisplaceFit,
    edges: DisplaceEdges,
    file_request: bool,
    error: Option<String>,
    texture: Option<egui::TextureHandle>,
    dirty: bool,
}

impl std::fmt::Debug for DisplaceMapDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DisplaceMapDialog")
            .field("image_size", &self.image_size)
            .field(
                "maps",
                &self.maps.iter().map(|m| &m.name).collect::<Vec<_>>(),
            )
            .field("chosen", &self.chosen)
            .field("scale", &(self.scale_x, self.scale_y))
            .field("fit", &self.fit)
            .field("edges", &self.edges)
            .finish_non_exhaustive()
    }
}

impl DisplaceMapDialog {
    /// Over the active layer's full-resolution pixels, offering `maps`
    /// (the first is chosen). `None` when there is no map to offer — a
    /// Displace with no map cannot run.
    pub fn new(layer: &FilterBuffer, maps: Vec<DisplaceMapSource>) -> Option<Self> {
        if maps.is_empty() {
            return None;
        }
        Some(Self {
            proxy: bounded_copy(layer, DISPLACE_PREVIEW_SIDE),
            image_size: layer.dimensions(),
            maps,
            chosen: 0,
            scale_x: 10.0,
            scale_y: 10.0,
            fit: DisplaceFit::default(),
            edges: DisplaceEdges::default(),
            file_request: false,
            error: None,
            texture: None,
            dirty: true,
        })
    }

    /// The title, which is the menu row's.
    pub fn title(&self) -> String {
        MenuAction::Filter(FilterId::Displace).label()
    }

    /// The maps on offer, in menu order.
    pub fn maps(&self) -> &[DisplaceMapSource] {
        &self.maps
    }

    /// The map a confirm would use.
    pub fn chosen(&self) -> usize {
        self.chosen
    }

    /// Choose map `index`.
    pub fn choose(&mut self, index: usize) {
        if index < self.maps.len() && index != self.chosen {
            self.chosen = index;
            self.dirty = true;
        }
    }

    /// Set both scales, in document pixels; clamped to Photoshop's range, a
    /// non-finite value reads as zero.
    pub fn set_scale(&mut self, x: f32, y: f32) {
        let clamp = |v: f32| {
            if v.is_finite() {
                v.clamp(-DISPLACE_SCALE_LIMIT, DISPLACE_SCALE_LIMIT)
            } else {
                0.0
            }
        };
        self.scale_x = clamp(x);
        self.scale_y = clamp(y);
        self.dirty = true;
    }

    pub fn set_fit(&mut self, fit: DisplaceFit) {
        self.fit = fit;
        self.dirty = true;
    }

    pub fn set_edges(&mut self, edges: DisplaceEdges) {
        self.edges = edges;
        self.dirty = true;
    }

    /// What the Load… button does: ask the host for an image file to use as
    /// the map (answered through [`Self::take_file_request`]).
    pub fn request_map_file(&mut self) {
        self.file_request = true;
    }

    /// Whether the Load… button was pressed since the last take. Consumed
    /// on read: the host answers it once with a file dialog.
    pub fn take_file_request(&mut self) -> bool {
        std::mem::take(&mut self.file_request)
    }

    /// Add a decoded file as a map and choose it. Refused (with the reason
    /// shown in the dialog) for an empty image or a short buffer.
    pub fn load_map(&mut self, name: &str, width: u32, height: u32, rgba8: &[u8]) -> bool {
        match DisplaceMapSource::from_rgba8(name, width, height, rgba8) {
            Some(source) => {
                self.maps.push(source);
                self.chosen = self.maps.len() - 1;
                self.error = None;
                self.dirty = true;
                true
            }
            None => {
                self.error = Some(format!("{name}: {width}x{height}"));
                false
            }
        }
    }

    /// Show why a file could not become a map.
    pub fn set_map_error(&mut self, message: impl Into<String>) {
        self.error = Some(message.into());
    }

    /// The reason the last load failed, if it did.
    pub fn map_error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// What a confirm would commit right now.
    pub fn spec(&self) -> DisplaceMapSpec {
        DisplaceMapSpec {
            image_size: self.image_size,
            map: self.maps[self.chosen].map.clone(),
            scale_x: self.scale_x,
            scale_y: self.scale_y,
            fit: self.fit,
            edges: self.edges,
        }
    }

    /// The preview: the real filter over the bounded copy, the scales
    /// shrunk by the copy's scale.
    pub fn preview(&self) -> FilterBuffer {
        let k = self.proxy.dimensions().0 as f32 / self.image_size.0.max(1) as f32;
        let mut spec = self.spec();
        spec.scale_x *= k;
        spec.scale_y *= k;
        spec.apply(&self.proxy)
    }

    /// Why the primary action is unavailable.
    pub fn blocked_reason(&self) -> Option<String> {
        self.spec()
            .is_identity()
            .then(|| tr("ui.adjustment.blocked.identity").to_string())
    }

    /// Escape and Enter, without drawing — Escape wins.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<DisplaceMapSpec> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm && self.blocked_reason().is_none() {
            return DialogOutcome::Confirmed(self.spec());
        }
        DialogOutcome::Open
    }

    /// Draw one frame.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DisplaceMapSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let title = self.title();
        let drawn = modal(
            ctx,
            "displace-map",
            &title,
            Some(tr("ui.adjustment.subtitle")),
            DialogWidth::Split,
            |ui| self.body(ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm if self.blocked_reason().is_none() => {
                    DialogOutcome::Confirmed(self.spec())
                }
                DialogButton::Confirm => DialogOutcome::Open,
                DialogButton::Extra(_) => {
                    self.request_map_file();
                    DialogOutcome::Open
                }
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(sizes::params_column_width());
                let (mut x, mut y) = (self.scale_x, self.scale_y);
                let range = -DISPLACE_SCALE_LIMIT..=DISPLACE_SCALE_LIMIT;
                egui::Grid::new("displace-map-fields")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Horizontal");
                        let dx = ui.add(egui::Slider::new(&mut x, range.clone()));
                        ui.end_row();
                        ui.label("Vertical");
                        let dy = ui.add(egui::Slider::new(&mut y, range));
                        ui.end_row();
                        if dx.changed() || dy.changed() {
                            self.set_scale(x, y);
                        }
                        let mut fit = self.fit;
                        ui.label("");
                        if combo(
                            ui,
                            "displace-map-fit",
                            &mut fit,
                            &DisplaceFit::ALL,
                            |f| f.label().to_string(),
                            |_| None,
                        ) {
                            self.set_fit(fit);
                        }
                        ui.end_row();
                        let mut edges = self.edges;
                        ui.label("");
                        if combo(
                            ui,
                            "displace-map-edges",
                            &mut edges,
                            &DisplaceEdges::ALL,
                            |e| e.label().to_string(),
                            |_| None,
                        ) {
                            self.set_edges(edges);
                        }
                        ui.end_row();
                        let mut chosen = self.chosen;
                        let indices: Vec<usize> = (0..self.maps.len()).collect();
                        ui.label("Map");
                        let names: Vec<String> = self.maps.iter().map(|m| m.name.clone()).collect();
                        if combo(
                            ui,
                            "displace-map-source",
                            &mut chosen,
                            &indices,
                            |i| names[i].clone(),
                            |_| None,
                        ) {
                            self.choose(chosen);
                        }
                        ui.end_row();
                    });
                if let Some(error) = &self.error {
                    ui.label(error.as_str());
                }
            });
            self.preview_widget(ui);
        });
        let blocked = self.blocked_reason();
        action_row(
            ui,
            tr("ui.adjustment.confirm"),
            blocked.as_deref(),
            &["Load…"],
        )
    }

    fn preview_widget(&mut self, ui: &mut egui::Ui) {
        if self.texture.is_none() || self.dirty {
            let buffer = self.preview();
            let (w, h) = buffer.dimensions();
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [w as usize, h as usize],
                &buffer.to_rgba8(),
            );
            self.texture = Some(ui.ctx().load_texture(
                "displace-map-preview",
                image,
                egui::TextureOptions::LINEAR,
            ));
            self.dirty = false;
        }
        if let Some(texture) = &self.texture {
            let (w, h) = self.proxy.dimensions();
            let k = sizes::preview_column_width() / w.max(h).max(1) as f32;
            ui.add(
                egui::Image::new(texture)
                    .fit_to_exact_size(egui::Vec2::new(w as f32 * k, h as f32 * k)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                px.push([x as f32 / w as f32, y as f32 / h as f32, 0.3, 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    fn flat(w: u32, h: u32, r: u8, g: u8) -> Vec<u8> {
        (0..w * h).flat_map(|_| [r, g, 128, 255]).collect()
    }

    fn doc_map(name: &str, r: u8, g: u8) -> DisplaceMapSource {
        DisplaceMapSource::from_rgba8(name, 8, 8, &flat(8, 8, r, g)).unwrap()
    }

    #[test]
    fn no_map_no_dialog() {
        assert!(DisplaceMapDialog::new(&ramp(8, 8), Vec::new()).is_none());
    }

    #[test]
    fn a_zero_map_confirms_the_identity_and_a_known_map_shifts() {
        let src = ramp(32, 24);
        let mut dialog = DisplaceMapDialog::new(
            &src,
            vec![doc_map("Neutral", 128, 128), doc_map("Right", 255, 128)],
        )
        .unwrap();
        let spec = match dialog.resolve(DialogKeys::CONFIRM) {
            DialogOutcome::Confirmed(spec) => spec,
            other => panic!("Enter did not commit: {other:?}"),
        };
        assert_eq!(spec.image_size, (32, 24));
        assert_eq!(spec.apply(&src).pixels(), src.pixels(), "zero map moves");
        dialog.choose(1);
        dialog.set_scale(4.0, 0.0);
        dialog.set_edges(DisplaceEdges::RepeatEdgePixels);
        let spec = dialog.spec();
        let out = spec.apply(&src);
        assert_eq!(out.get(10, 5), src.get(14, 5), "+4 px in x");
        // Wrap Around reads the far side past the right edge.
        dialog.set_edges(DisplaceEdges::WrapAround);
        let wrapped = dialog.spec().apply(&src);
        assert_eq!(wrapped.get(30, 5), src.get(2, 5));
        // Zero scales cannot commit.
        dialog.set_scale(0.0, 0.0);
        assert!(dialog.blocked_reason().is_some());
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
        assert_eq!(dialog.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
    }

    #[test]
    fn a_loaded_file_joins_the_list_and_is_chosen() {
        let src = ramp(16, 16);
        let mut dialog = DisplaceMapDialog::new(&src, vec![doc_map("Doc", 128, 128)]).unwrap();
        assert!(dialog.load_map("map.png", 4, 4, &flat(4, 4, 0, 128)));
        assert_eq!(dialog.maps().len(), 2);
        assert_eq!(dialog.chosen(), 1);
        assert!(!dialog.load_map("broken.png", 4, 4, &[1, 2, 3]));
        assert!(dialog.map_error().is_some());
        assert_eq!(dialog.chosen(), 1, "a refused file changes nothing");
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog =
                DisplaceMapDialog::new(&ramp(16, 16), vec![doc_map("Doc", 200, 60)]).unwrap();
            assert!(dialog.show(ctx).is_open());
            assert!(!dialog.take_file_request());
        });
    }
}
