//! W9-G: the vector mask in the drawn chrome.
//!
//! The Layers panel draws a vector-mask thumbnail well (the path's own
//! rendering) and the Properties panel edits the vector mask's density,
//! feather, invert and enable. A vector-only mask has no live pixel half, so
//! neither panel offers the pixel mask's well or rows for it. Every control
//! here is found on screen by its stable id and driven with real input.

use editor_core::{Command, Document, History, LayerPatch, Patch};
use layer_model::{Layer, LayerId, LayerMask, MaskId, VectorMask};
use ui::dock::{LayoutId, PanelId};
use ui::view::ids;
use ui::{Intent, Workspace};

const SCREEN: egui::Vec2 = egui::vec2(1400.0, 900.0);

struct Harness {
    ctx: egui::Context,
    workspace: Workspace,
    doc: Document,
    history: History,
    /// Every texture image uploaded so far, by the texture's id.
    textures: Vec<(egui::TextureId, egui::ColorImage)>,
    /// The last frame's painted shapes.
    shapes: Vec<egui::epaint::ClippedShape>,
}

impl Harness {
    fn with_document(doc: Document) -> Self {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let style = design::style_for(design::Theme::Dark);
        ctx.set_style_of(egui::Theme::Dark, style.clone());
        ctx.set_style_of(egui::Theme::Light, style);
        let mut workspace = Workspace::new();
        workspace.dock.apply_layout(LayoutId::Minimal);
        workspace.dock.set_open(PanelId::Layers, true);
        Self {
            ctx,
            workspace,
            doc,
            history: History::new(),
            textures: Vec::new(),
            shapes: Vec::new(),
        }
    }

    /// Show only the Properties panel, so its rows are on screen.
    fn only_properties(&mut self) {
        self.workspace.dock.apply_layout(LayoutId::Minimal);
        self.workspace.dock.set_open(PanelId::Properties, true);
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> Vec<Intent> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            events,
            ..Default::default()
        };
        let out = self.ctx.run(input, |ctx| {
            self.workspace.ui(ctx, &self.doc, &self.history);
        });
        for (id, delta) in out.textures_delta.set {
            if let egui::ImageData::Color(image) = delta.image {
                self.textures.push((id, (*image).clone()));
            }
        }
        self.shapes = out.shapes;
        self.workspace.drain_intents()
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(Vec::new());
        }
    }

    fn drawn(&mut self, id: egui::Id) -> Option<egui::Rect> {
        self.settle();
        self.ctx.read_response(id).map(|r| r.rect)
    }

    fn rect(&mut self, id: egui::Id) -> egui::Rect {
        self.drawn(id)
            .unwrap_or_else(|| panic!("{id:?} was not drawn"))
    }

    fn click(&mut self, id: egui::Id) -> Vec<Intent> {
        let at = self.rect(id).center();
        self.frame(vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ])
    }

    fn drag(&mut self, id: egui::Id, by: egui::Vec2) -> Vec<Intent> {
        let from = self.rect(id).center();
        let to = from + by;
        let mut out = self.frame(vec![
            egui::Event::PointerMoved(from),
            egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
        ]);
        for step in 1..=4 {
            let at = from + by * (step as f32 / 4.0);
            out.extend(self.frame(vec![egui::Event::PointerMoved(at)]));
        }
        out.extend(self.frame(vec![
            egui::Event::PointerMoved(to),
            egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ]));
        out
    }

    /// Apply every document command the intents carry, as the application
    /// would, so the next frame draws the edited document.
    fn apply(&mut self, intents: &[Intent]) {
        for c in intents.iter().filter_map(Intent::as_command) {
            self.history.apply(&mut self.doc, c.clone()).unwrap();
        }
    }
}

/// The numeric field of one Properties slider row, by its label.
fn field(label: &str) -> egui::Id {
    egui::Id::new(("raster-numeric-field", label))
}

fn mask_patches(intents: &[Intent]) -> Vec<LayerMask> {
    intents
        .iter()
        .filter_map(Intent::as_command)
        .filter_map(|c| match c {
            Command::SetLayerProperties {
                patch:
                    LayerPatch {
                        mask: Patch::Set(m),
                        ..
                    },
                ..
            } => Some(m.clone()),
            _ => None,
        })
        .collect()
}

/// A 320x240 document whose one layer carries a triangular vector mask
/// (top-left half of the canvas); with `pixel` it has a pixel mask too.
fn triangle_document(pixel: bool) -> (Document, LayerId) {
    let mut doc = Document::new(320, 240, "Test");
    let a = doc.layers.insert_at(Layer::raster("Top"), None, 0).unwrap();
    let v = VectorMask::new("M0 0 L320 0 L0 240 Z");
    let mask = if pixel {
        let mut m = LayerMask::new(MaskId::new());
        m.vector = Some(Box::new(v));
        m
    } else {
        LayerMask::vector_only(MaskId::new(), v)
    };
    doc.layers.get_mut(a).unwrap().mask = Some(mask);
    doc.set_active_layer(Some(a)).unwrap();
    (doc, a)
}

#[test]
fn a_vector_only_mask_shows_its_own_thumbnail_and_no_pixel_well() {
    let (doc, a) = triangle_document(false);
    let mut h = Harness::with_document(doc);
    assert!(
        h.drawn(ids::layer_mask_thumb(a)).is_none(),
        "a vector-only mask has no pixel half, so no pixel well"
    );
    // Read the well's rect in the same frame as the shapes below.
    let well = h.rect(ids::layer_vector_mask_thumb(a));
    // The well paints a texture over its whole rect…
    let painted: Vec<egui::TextureId> = h
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Mesh(m)
                if m.texture_id != egui::TextureId::default()
                    && m.calc_bounds().expand(0.5).contains_rect(well)
                    && well.expand(0.5).contains_rect(m.calc_bounds()) =>
            {
                Some(m.texture_id)
            }
            _ => None,
        })
        .collect();
    assert_eq!(painted.len(), 1, "one texture fills the vector well");
    // …and that texture is the triangle: covered top-left, clear bottom-right.
    let (_, image) = h
        .textures
        .iter()
        .rev()
        .find(|(id, _)| *id == painted[0])
        .expect("the texture was uploaded");
    let [w, hgt] = image.size;
    let at = |x: usize, y: usize| image.pixels[y * w + x].r();
    assert_eq!(at(1, 1), 255, "inside the triangle");
    assert_eq!(at(w - 2, hgt - 2), 0, "outside the triangle");
}

#[test]
fn a_layer_with_both_masks_shows_both_wells() {
    let (doc, a) = triangle_document(true);
    let mut h = Harness::with_document(doc);
    let pixel = h.rect(ids::layer_mask_thumb(a));
    let vector = h.rect(ids::layer_vector_mask_thumb(a));
    assert!(vector.left() >= pixel.right(), "the vector well follows");
}

#[test]
fn properties_edits_the_vector_masks_density_and_feather() {
    let (doc, a) = triangle_document(false);
    let mut h = Harness::with_document(doc);
    // Clicking the vector well turns Properties to the mask.
    let intents = h.click(ids::layer_vector_mask_thumb(a));
    assert!(intents.iter().any(|i| matches!(
        i,
        Intent::SelectLayers { active: Some(l), .. } if *l == a
    )));
    assert_eq!(
        h.workspace.property_focus,
        ui::panels::properties::PropertyFocus::Mask
    );
    h.only_properties();
    // A vector-only mask offers the vector rows, not the dead pixel ones.
    assert!(h.drawn(field("Vector density")).is_some());
    assert!(h.drawn(field("Vector feather")).is_some());
    assert!(
        h.drawn(field("Density")).is_none(),
        "the pixel half is not live; its Density would change nothing"
    );
    assert!(h.drawn(field("Feather")).is_none());

    let intents = h.drag(field("Vector density"), egui::vec2(-40.0, 0.0));
    let patches = mask_patches(&intents);
    let last = patches.last().expect("the drag emits a mask patch");
    let v = last.vector.as_deref().expect("the vector mask is edited");
    assert!(v.density() < 1.0, "density went down: {}", v.density());
    assert_eq!(last.density(), 1.0, "the pixel half is untouched");
    h.apply(&intents);

    let intents = h.drag(field("Vector feather"), egui::vec2(20.0, 0.0));
    let patches = mask_patches(&intents);
    let v = patches
        .last()
        .and_then(|m| m.vector.clone())
        .expect("the drag emits a vector-mask patch");
    assert!(v.feather_px() > 0.0, "feather went up: {}", v.feather_px());
}

#[test]
fn with_both_masks_properties_offers_both_sets_of_rows() {
    let (doc, _) = triangle_document(true);
    let mut h = Harness::with_document(doc);
    h.only_properties();
    h.workspace.property_focus = ui::panels::properties::PropertyFocus::Mask;
    for label in ["Vector density", "Vector feather", "Density", "Feather"] {
        assert!(h.drawn(field(label)).is_some(), "{label} is offered");
    }
    // The pixel Density edits the pixel half and leaves the vector alone.
    let intents = h.drag(field("Density"), egui::vec2(-40.0, 0.0));
    let last = mask_patches(&intents).pop().expect("a mask patch");
    assert!(last.density() < 1.0);
    assert_eq!(last.vector.as_deref().map(|v| v.density()), Some(1.0));
}
