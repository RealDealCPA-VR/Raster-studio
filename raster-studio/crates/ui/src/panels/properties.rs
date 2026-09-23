//! The Properties panel and the Adjustments panel.
//!
//! # Context-sensitive means *derived*, not remembered
//!
//! Properties shows whatever the user last put the focus on, but it does not
//! keep a copy of it. [`PropertiesSubject::resolve`] re-derives the subject
//! from the document plus a small [`PropertyFocus`] hint every frame, so a
//! subject cannot outlive the thing it describes: delete the masked layer and
//! the panel falls back on its own, with no stale id to guard against at every
//! read.
//!
//! The fallbacks are the interesting part and they are all tested — asking for
//! mask properties on a layer with no mask shows the *layer*, not an empty
//! panel, because an empty panel reads as a bug.

use editor_core::{Command, Document, LayerPatch, Patch};
use glam::{Affine2, Vec2};
use layer_model::{AdjustmentKind, LayerId, LayerKind, LayerMask, MaskError, Rgba, ShapeStroke};

use crate::intent::Intent;
use crate::menu::{AdjustmentId, LayerClass};

/// What the user last clicked, which decides what Properties talks about.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum PropertyFocus {
    /// The layer row itself.
    #[default]
    Layer,
    /// The layer's mask thumbnail.
    Mask,
}

/// What the Properties panel is showing.
#[derive(Clone, PartialEq, Debug)]
pub enum PropertiesSubject {
    /// No layer is active. The panel says so rather than showing an empty box.
    Nothing,
    /// A plain layer: name, opacity, blend, locks.
    Layer(LayerId),
    /// A layer mask: density, feather, invert, link.
    Mask(LayerId),
    /// An adjustment layer's parameters, live-editable.
    Adjustment {
        layer: LayerId,
        id: Option<AdjustmentId>,
    },
    /// A text layer: hands off to Character and Paragraph.
    Text(LayerId),
    /// A shape layer: fill, stroke, path.
    Shape(LayerId),
    /// A smart object: its source, and the Replace / Edit Contents actions.
    SmartObject(LayerId),
}

impl PropertiesSubject {
    /// Decide what to show.
    pub fn resolve(doc: &Document, active: Option<LayerId>, focus: PropertyFocus) -> Self {
        let Some(id) = active else {
            return PropertiesSubject::Nothing;
        };
        let Some(layer) = doc.layers.get(id) else {
            return PropertiesSubject::Nothing;
        };
        if focus == PropertyFocus::Mask && layer.mask.is_some() {
            return PropertiesSubject::Mask(id);
        }
        match &layer.kind {
            LayerKind::Adjustment(a) => PropertiesSubject::Adjustment {
                layer: id,
                id: adjustment_id_of(&a.kind),
            },
            LayerKind::Text(_) => PropertiesSubject::Text(id),
            LayerKind::Shape(_) => PropertiesSubject::Shape(id),
            LayerKind::SmartObject(_) => PropertiesSubject::SmartObject(id),
            _ => PropertiesSubject::Layer(id),
        }
    }

    /// The layer this subject describes, if any.
    pub const fn layer(&self) -> Option<LayerId> {
        match self {
            PropertiesSubject::Nothing => None,
            PropertiesSubject::Layer(id)
            | PropertiesSubject::Mask(id)
            | PropertiesSubject::Adjustment { layer: id, .. }
            | PropertiesSubject::Text(id)
            | PropertiesSubject::Shape(id)
            | PropertiesSubject::SmartObject(id) => Some(*id),
        }
    }

    /// The panel's heading.
    pub fn title(&self) -> &'static str {
        match self {
            PropertiesSubject::Nothing => "Properties",
            PropertiesSubject::Layer(_) => "Layer Properties",
            PropertiesSubject::Mask(_) => "Mask Properties",
            PropertiesSubject::Adjustment { .. } => "Adjustment",
            PropertiesSubject::Text(_) => "Text Properties",
            PropertiesSubject::Shape(_) => "Shape Properties",
            PropertiesSubject::SmartObject(_) => "Smart Object",
        }
    }
}

/// Which [`AdjustmentId`] a stored kind corresponds to.
///
/// `None` for `Auto`, which has no panel entry of its own — it is the three
/// Auto commands in the Image menu, not a one-click adjustment layer.
pub fn adjustment_id_of(kind: &AdjustmentKind) -> Option<AdjustmentId> {
    Some(match kind {
        AdjustmentKind::Levels { .. } | AdjustmentKind::LevelsFull { .. } => AdjustmentId::Levels,
        AdjustmentKind::Curves { .. } | AdjustmentKind::CurvesFull { .. } => AdjustmentId::Curves,
        AdjustmentKind::Exposure { .. } | AdjustmentKind::ExposureFull { .. } => {
            AdjustmentId::Exposure
        }
        AdjustmentKind::HueSaturation { .. } | AdjustmentKind::HueSaturationFull { .. } => {
            AdjustmentId::HueSaturation
        }
        AdjustmentKind::ColorBalance { .. } | AdjustmentKind::ColorBalanceFull { .. } => {
            AdjustmentId::ColorBalance
        }
        AdjustmentKind::BrightnessContrast { .. } => AdjustmentId::BrightnessContrast,
        AdjustmentKind::Vibrance { .. } => AdjustmentId::Vibrance,
        AdjustmentKind::BlackAndWhite { .. } => AdjustmentId::BlackAndWhite,
        AdjustmentKind::PhotoFilter { .. } => AdjustmentId::PhotoFilter,
        AdjustmentKind::ChannelMixer { .. } => AdjustmentId::ChannelMixer,
        AdjustmentKind::Invert => AdjustmentId::Invert,
        AdjustmentKind::Posterize { .. } => AdjustmentId::Posterize,
        AdjustmentKind::Threshold { .. } => AdjustmentId::Threshold,
        AdjustmentKind::GradientMap { .. } => AdjustmentId::GradientMap,
        AdjustmentKind::SelectiveColor { .. } => AdjustmentId::SelectiveColor,
        AdjustmentKind::Auto { .. } => return None,
    })
}

/// Editing a mask's own numbers.
///
/// `LayerMask` validates through setters, so an out-of-range value returns
/// [`MaskError`] rather than being clamped silently — and the panel refuses to
/// emit rather than sending the document a value it would reject.
pub struct MaskProperties;

impl MaskProperties {
    /// The mask on a layer, if it has one.
    pub fn of(doc: &Document, layer: LayerId) -> Option<&LayerMask> {
        doc.layers.get(layer)?.mask.as_ref()
    }

    fn edit(
        doc: &Document,
        layer: LayerId,
        f: impl FnOnce(&mut LayerMask) -> Result<(), MaskError>,
    ) -> Option<Command> {
        let before = doc.layers.get(layer)?.mask.clone()?;
        let mut mask = before.clone();
        f(&mut mask).ok()?;
        (mask != before).then_some(Command::SetLayerProperties {
            layer_id: layer,
            patch: LayerPatch {
                mask: Patch::Set(mask),
                ..Default::default()
            },
        })
    }

    pub fn set_density(doc: &Document, layer: LayerId, density: f32) -> Option<Command> {
        Self::edit(doc, layer, |m| m.set_density(density))
    }

    pub fn set_feather(doc: &Document, layer: LayerId, feather_px: f32) -> Option<Command> {
        Self::edit(doc, layer, |m| m.set_feather_px(feather_px))
    }

    pub fn set_inverted(doc: &Document, layer: LayerId, inverted: bool) -> Option<Command> {
        Self::edit(doc, layer, |m| {
            m.inverted = inverted;
            Ok(())
        })
    }

    pub fn set_enabled(doc: &Document, layer: LayerId, enabled: bool) -> Option<Command> {
        Self::edit(doc, layer, |m| {
            m.enabled = enabled;
            Ok(())
        })
    }

    pub fn set_linked(doc: &Document, layer: LayerId, linked: bool) -> Option<Command> {
        Self::edit(doc, layer, |m| {
            m.linked = linked;
            Ok(())
        })
    }
}

/// Live editing of an adjustment layer's parameters.
///
/// Produces an [`Intent::EditLayerKind`], not a `Command` — see that variant's
/// documentation for why, and what is missing from `editor-core`.
pub fn edit_adjustment(doc: &Document, layer: LayerId, kind: AdjustmentKind) -> Option<Intent> {
    let current = match &doc.layers.get(layer)?.kind {
        LayerKind::Adjustment(a) => a,
        _ => return None,
    };
    (current.kind != kind).then(|| Intent::EditLayerKind {
        layer,
        kind: Box::new(LayerKind::Adjustment(layer_model::AdjustmentLayer { kind })),
    })
}

/// The Adjustments panel: one button per adjustment, each creating a layer.
///
/// The whole panel is a function of [`AdjustmentId::ALL`], so an adjustment
/// added to the vocabulary appears here with no edit.
pub struct AdjustmentsPanel;

impl AdjustmentsPanel {
    /// Every button, in panel order.
    pub fn entries() -> &'static [AdjustmentId] {
        AdjustmentId::ALL
    }

    /// The command a button emits.
    pub fn create(id: AdjustmentId) -> Command {
        id.create_command()
    }

    /// The icon key for a button, so the grid reads at a glance.
    ///
    /// A *key* into [`crate::icons::ui_icon`], never a symbol. All fifteen of
    /// these were symbols once — `"◐"`, `"⊿"`, `"∿"`, `"⋔"` and the rest — and
    /// egui's default font stack has none of them, so all fourteen visible
    /// buttons in the panel were tofu boxes.
    pub const fn icon(id: AdjustmentId) -> &'static str {
        match id {
            AdjustmentId::BrightnessContrast => "adj-brightness-contrast",
            AdjustmentId::Levels => "adj-levels",
            AdjustmentId::Curves => "adj-curves",
            AdjustmentId::Exposure => "adj-exposure",
            AdjustmentId::Vibrance => "adj-vibrance",
            AdjustmentId::HueSaturation => "adj-hue-saturation",
            AdjustmentId::ColorBalance => "adj-color-balance",
            AdjustmentId::BlackAndWhite => "adj-black-and-white",
            AdjustmentId::PhotoFilter => "adj-photo-filter",
            AdjustmentId::ChannelMixer => "adj-channel-mixer",
            AdjustmentId::Invert => "adj-invert",
            AdjustmentId::Posterize => "adj-posterize",
            AdjustmentId::Threshold => "adj-threshold",
            AdjustmentId::GradientMap => "adj-gradient-map",
            AdjustmentId::SelectiveColor => "adj-selective-color",
        }
    }
}

/// Whether a layer class has anything for the Properties panel beyond the
/// common block. Used to decide whether to draw a second section.
///
/// W3-J: a smart object has one — its source and the Replace / Edit Contents
/// actions — so it is no longer the odd class out.
pub const fn has_kind_properties(class: LayerClass) -> bool {
    matches!(
        class,
        LayerClass::Adjustment
            | LayerClass::Text
            | LayerClass::Shape
            | LayerClass::Group
            | LayerClass::SmartObject
    )
}

/// Stable ids for the W3-J controls, so a headless test can find them.
pub mod ids {
    use layer_model::LayerId;

    /// The Transform block's disclosure.
    pub fn transform_toggle() -> egui::Id {
        egui::Id::new("raster-properties-transform-toggle")
    }
    /// The Transform block's X field.
    pub fn transform_x(layer: LayerId) -> egui::Id {
        egui::Id::new(("raster-properties-x", layer))
    }
    /// The Transform block's Y field.
    pub fn transform_y(layer: LayerId) -> egui::Id {
        egui::Id::new(("raster-properties-y", layer))
    }
    /// The Transform block's W field.
    pub fn transform_w(layer: LayerId) -> egui::Id {
        egui::Id::new(("raster-properties-w", layer))
    }
    /// The Transform block's H field.
    pub fn transform_h(layer: LayerId) -> egui::Id {
        egui::Id::new(("raster-properties-h", layer))
    }
    /// The Align picker.
    pub fn align_picker(layer: LayerId) -> egui::Id {
        egui::Id::new(("raster-properties-align-picker", layer))
    }
    /// One Align row inside the picker.
    pub fn align(edge: super::AlignEdge) -> egui::Id {
        egui::Id::new(("raster-properties-align", edge as u8))
    }
    /// The shape page's fill on/off toggle.
    pub fn shape_fill_enabled(layer: LayerId) -> egui::Id {
        egui::Id::new(("raster-properties-shape-fill", layer))
    }
    /// The shape page's corner-radius slider (W3-J).
    pub fn shape_radius(layer: LayerId) -> egui::Id {
        egui::Id::new(("raster-properties-shape-radius", layer))
    }
    /// The shape page's stroke on/off toggle.
    pub fn shape_stroke_enabled(layer: LayerId) -> egui::Id {
        egui::Id::new(("raster-properties-shape-stroke", layer))
    }
    /// The text page's family field.
    pub fn text_family(layer: LayerId) -> egui::Id {
        egui::Id::new(("raster-properties-text-family", layer))
    }
    /// The smart-object page's Replace Contents… button.
    pub fn replace_contents() -> egui::Id {
        egui::Id::new("raster-properties-replace-contents")
    }
    /// The smart-object page's Edit Contents… button.
    pub fn edit_contents() -> egui::Id {
        egui::Id::new("raster-properties-edit-contents")
    }
}

// ---------------------------------------------------------------------------
// W3-J: the Transform block and the Align row
// ---------------------------------------------------------------------------

/// Where a layer's content lands on the canvas, in document pixels: the box
/// of what the layer actually draws, mapped through its own transform.
///
/// For a text run or a shape path that is the compositor's ink box. For a
/// pixel-owning layer (raster, generator, smart object) it is the alpha ink
/// of its tiles ([`RasterInk`]) - not the stored-tile extent, which counts a
/// 256-pixel tile whole and would make a 100-pixel dab read as 256 wide.
/// `None` when the layer has nothing to measure (an empty or fully
/// transparent raster layer, an adjustment) or, for a pixel-owning layer,
/// when no current [`RasterInk`] measurement was handed in.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LayerFrame {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl LayerFrame {
    /// The frame's top-left corner.
    pub fn min(&self) -> Vec2 {
        Vec2::new(self.x, self.y)
    }
}

/// The alpha ink of one pixel-owning layer, measured by whoever holds its
/// tile bytes.
///
/// The UI sees a document's tile *hashes* but not the bytes behind them, so
/// it cannot scan a raster layer's alpha itself. The application measures
/// it ([`RasterInk::measure`], the compositor's cached `alpha_bounds`) and
/// publishes it each frame ([`RasterInks::publish`]); the measurement carries
/// a fingerprint of the tile hashes it read, so a stroke painted since
/// invalidates it instead of leaving a stale box on screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RasterInk {
    /// Fingerprint of the layer's level-0 tile hashes the ink was read from.
    pub tiles: u64,
    /// The ink box in the layer's own pixel space (before its transform);
    /// `None` when every stored pixel is transparent.
    pub rect: Option<raster::PixelRect>,
}

impl RasterInk {
    /// `true` for the kinds whose pixels live in tiles.
    pub fn owns_pixels(kind: &LayerKind) -> bool {
        matches!(
            kind,
            LayerKind::Raster(_) | LayerKind::Generator(_) | LayerKind::SmartObject(_)
        )
    }

    /// A fingerprint of the layer's level-0 tile hashes, order-independent.
    pub fn fingerprint(doc: &Document, layer: LayerId) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut tiles: Vec<(raster::TileCoord, raster::TileHash)> = doc
            .pixels
            .tiles(editor_core::PixelKey::Layer(layer))
            .map(|map| map.iter().filter(|(c, _)| c.level == 0).collect())
            .unwrap_or_default();
        tiles.sort_unstable_by_key(|(c, _)| *c);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        tiles.hash(&mut hasher);
        hasher.finish()
    }

    /// Measure one pixel-owning layer's alpha ink from the tile bytes in
    /// `source`. `None` for any other kind, or a layer that is gone.
    pub fn measure<S: compositor::TileSource + ?Sized>(
        doc: &Document,
        source: &S,
        layer: LayerId,
    ) -> Option<Self> {
        if !Self::owns_pixels(&doc.layers.get(layer)?.kind) {
            return None;
        }
        let rect = compositor::bounds::alpha_bounds(
            doc,
            source,
            layer,
            0,
            compositor::CompositeOptions::default(),
        )
        .ok()
        .flatten()
        .filter(|r| r.width > 0 && r.height > 0);
        Some(Self {
            tiles: Self::fingerprint(doc, layer),
            rect,
        })
    }
}

/// The [`RasterInk`] of the layers the Properties panel may measure: the
/// active layer and, for a group, every layer under it.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct RasterInks(std::collections::HashMap<LayerId, RasterInk>);

impl RasterInks {
    fn slot() -> egui::Id {
        egui::Id::new("raster-properties-raster-inks")
    }

    /// Measure `layer` and every pixel-owning layer under it.
    pub fn measure<S: compositor::TileSource + ?Sized>(
        doc: &Document,
        source: &S,
        layer: LayerId,
    ) -> Self {
        const MAX_DEPTH: usize = 64;
        let mut inks = Self::default();
        let mut stack = vec![(layer, 0usize)];
        while let Some((id, depth)) = stack.pop() {
            let Some(l) = doc.layers.get(id) else {
                continue;
            };
            if let LayerKind::Group(g) = &l.kind {
                if depth < MAX_DEPTH {
                    stack.extend(g.children.iter().map(|&c| (c, depth + 1)));
                }
            } else if let Some(ink) = RasterInk::measure(doc, source, id) {
                inks.0.insert(id, ink);
            }
        }
        inks
    }

    /// Record one measurement.
    pub fn insert(&mut self, layer: LayerId, ink: RasterInk) {
        self.0.insert(layer, ink);
    }

    /// The layer's ink if it was measured from the tiles it holds now.
    pub fn current(&self, doc: &Document, layer: LayerId) -> Option<&RasterInk> {
        self.0
            .get(&layer)
            .filter(|ink| ink.tiles == RasterInk::fingerprint(doc, layer))
    }

    /// Hand this frame's measurements to the panel.
    pub fn publish(self, ctx: &egui::Context) {
        ctx.data_mut(|d| d.insert_temp(Self::slot(), self));
    }

    /// The measurements last published, or none.
    pub fn published(ctx: &egui::Context) -> Self {
        ctx.data(|d| d.get_temp::<Self>(Self::slot()))
            .unwrap_or_default()
    }
}

/// Which canvas edge or centre an Align button snaps the layer to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum AlignEdge {
    Left,
    HorizontalCenter,
    Right,
    Top,
    VerticalCenter,
    Bottom,
}

impl AlignEdge {
    /// Every edge, in the row's order.
    pub const ALL: &'static [AlignEdge] = &[
        AlignEdge::Left,
        AlignEdge::HorizontalCenter,
        AlignEdge::Right,
        AlignEdge::Top,
        AlignEdge::VerticalCenter,
        AlignEdge::Bottom,
    ];

    /// The button's short label.
    pub const fn label(self) -> &'static str {
        match self {
            AlignEdge::Left => "Left",
            AlignEdge::HorizontalCenter => "Center",
            AlignEdge::Right => "Right",
            AlignEdge::Top => "Top",
            AlignEdge::VerticalCenter => "Middle",
            AlignEdge::Bottom => "Bottom",
        }
    }

    /// The catalogue key of the button's tooltip.
    pub const fn tip_key(self) -> &'static str {
        match self {
            AlignEdge::Left => "ui.docks.align.left",
            AlignEdge::HorizontalCenter => "ui.docks.align.hcenter",
            AlignEdge::Right => "ui.docks.align.right",
            AlignEdge::Top => "ui.docks.align.top",
            AlignEdge::VerticalCenter => "ui.docks.align.vcenter",
            AlignEdge::Bottom => "ui.docks.align.bottom",
        }
    }
}

/// The Properties panel's Transform block: X / Y / W / H and Align.
///
/// Every edit is one [`Command::TransformLayer`] — a document-space delta
/// pre-multiplied onto the layer's own transform, exactly what the canvas
/// gizmo emits — so a typed width is one undo step and composes with a
/// rotated or skewed layer the way a drag would.
pub struct Transform;

impl Transform {
    /// Smallest width or height a field accepts, in document pixels. A zero
    /// scale is not invertible and could never be undone.
    pub const MIN_SIZE_PX: f32 = 0.01;

    /// The layer's document-space frame, or `None` when there is nothing to
    /// measure or the layer is gone.
    ///
    /// Pixel-owning layers are measured from `inks` (their alpha ink, which
    /// only the application's tile store can scan); a layer missing from it,
    /// or whose tiles changed since it was measured, has no frame rather
    /// than a wrong one. Text and shapes use the compositor's ink box. A
    /// group is the union of its children's frames. The box is mapped
    /// through the layer's transform *exactly* - not through the padded
    /// image rect the compositor sizes its buffers with, which adds a
    /// two-pixel resampling margin under any non-trivial transform - so a
    /// typed W of 250 reads back as 250, not 254.
    pub fn frame(doc: &Document, layer: LayerId, inks: &RasterInks) -> Option<LayerFrame> {
        let (lo, hi) = Self::parent_space_box(doc, layer, inks, 0)?;
        let size = hi - lo;
        (size.x > 0.0 && size.y > 0.0).then_some(LayerFrame {
            x: lo.x,
            y: lo.y,
            width: size.x,
            height: size.y,
        })
    }

    /// The layer's content box mapped through its own transform, as
    /// `(min, max)`. `depth` bounds the recursion on a malformed tree.
    fn parent_space_box(
        doc: &Document,
        layer: LayerId,
        inks: &RasterInks,
        depth: usize,
    ) -> Option<(Vec2, Vec2)> {
        const MAX_DEPTH: usize = 64;
        if depth > MAX_DEPTH {
            return None;
        }
        let layer_ref = doc.layers.get(layer)?;
        let transform = layer_ref.transform;
        if !transform.to_cols_array().iter().all(|v| v.is_finite()) {
            return None;
        }
        let (x0, y0, x1, y1) = match &layer_ref.kind {
            kind if RasterInk::owns_pixels(kind) => {
                let rect = inks.current(doc, layer)?.rect?;
                let (x0, y0) = (rect.x as f32, rect.y as f32);
                (x0, y0, x0 + rect.width as f32, y0 + rect.height as f32)
            }
            LayerKind::Group(group) => {
                let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
                for &child in &group.children {
                    if let Some((a, b)) = Self::parent_space_box(doc, child, inks, depth + 1) {
                        lo = lo.min(a);
                        hi = hi.max(b);
                    }
                }
                if !(lo.x < hi.x && lo.y < hi.y) {
                    return None;
                }
                (lo.x, lo.y, hi.x, hi.y)
            }
            _ => {
                // The bounds pass shapes text and paths itself; the source
                // only matters for pixel *contents*, which ink boxes do not
                // read.
                let source = compositor::MemoryTileSource::new();
                let rect = compositor::bounds::content_bounds(
                    doc,
                    &source,
                    layer,
                    0,
                    compositor::CompositeOptions::default(),
                )
                .ok()??;
                if rect.width == 0 || rect.height == 0 {
                    return None;
                }
                let (x0, y0) = (rect.x as f32, rect.y as f32);
                (x0, y0, x0 + rect.width as f32, y0 + rect.height as f32)
            }
        };
        let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
        for corner in [
            Vec2::new(x0, y0),
            Vec2::new(x1, y0),
            Vec2::new(x0, y1),
            Vec2::new(x1, y1),
        ] {
            let p = transform.transform_point2(corner);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        (lo.x < hi.x && lo.y < hi.y).then_some((lo, hi))
    }

    /// `true` when the layer's position lock refuses every transform, so the
    /// block is drawn read-only rather than emitting doomed commands.
    pub fn is_locked(doc: &Document, layer: LayerId) -> bool {
        doc.layers
            .get(layer)
            .is_some_and(|l| l.locked.blocks_transform())
    }

    fn delta(doc: &Document, layer: LayerId, delta: Affine2) -> Option<Command> {
        if Self::is_locked(doc, layer) {
            return None;
        }
        let matrix = delta.to_cols_array();
        if !matrix.iter().all(|v| v.is_finite()) {
            return None;
        }
        if delta.abs_diff_eq(Affine2::IDENTITY, 1e-6) {
            return None;
        }
        Some(Command::TransformLayer {
            layer_id: layer,
            matrix,
        })
    }

    fn translate(doc: &Document, layer: LayerId, by: Vec2) -> Option<Command> {
        Self::delta(doc, layer, Affine2::from_translation(by))
    }

    /// Scale the frame about its own top-left corner so `width` and `height`
    /// come out as asked; the corner stays put, which is what a typed size
    /// means.
    fn scale_about_min(
        doc: &Document,
        layer: LayerId,
        frame: LayerFrame,
        scale: Vec2,
    ) -> Option<Command> {
        let min = frame.min();
        let delta = Affine2::from_translation(min)
            * Affine2::from_scale(scale)
            * Affine2::from_translation(-min);
        Self::delta(doc, layer, delta)
    }

    /// Move the frame's left edge to `x`.
    pub fn set_x(doc: &Document, layer: LayerId, inks: &RasterInks, x: f32) -> Option<Command> {
        if !x.is_finite() {
            return None;
        }
        let frame = Self::frame(doc, layer, inks)?;
        Self::translate(doc, layer, Vec2::new(x - frame.x, 0.0))
    }

    /// Move the frame's top edge to `y`.
    pub fn set_y(doc: &Document, layer: LayerId, inks: &RasterInks, y: f32) -> Option<Command> {
        if !y.is_finite() {
            return None;
        }
        let frame = Self::frame(doc, layer, inks)?;
        Self::translate(doc, layer, Vec2::new(0.0, y - frame.y))
    }

    /// Resize the frame to `width` pixels wide, keeping its left edge.
    pub fn set_width(
        doc: &Document,
        layer: LayerId,
        inks: &RasterInks,
        width: f32,
    ) -> Option<Command> {
        if !width.is_finite() || width < Self::MIN_SIZE_PX {
            return None;
        }
        let frame = Self::frame(doc, layer, inks)?;
        Self::scale_about_min(doc, layer, frame, Vec2::new(width / frame.width, 1.0))
    }

    /// Resize the frame to `height` pixels tall, keeping its top edge.
    pub fn set_height(
        doc: &Document,
        layer: LayerId,
        inks: &RasterInks,
        height: f32,
    ) -> Option<Command> {
        if !height.is_finite() || height < Self::MIN_SIZE_PX {
            return None;
        }
        let frame = Self::frame(doc, layer, inks)?;
        Self::scale_about_min(doc, layer, frame, Vec2::new(1.0, height / frame.height))
    }

    /// Snap the frame to one canvas edge or centre.
    pub fn align(
        doc: &Document,
        layer: LayerId,
        inks: &RasterInks,
        edge: AlignEdge,
    ) -> Option<Command> {
        let frame = Self::frame(doc, layer, inks)?;
        let (cw, ch) = (doc.width() as f32, doc.height() as f32);
        let by = match edge {
            AlignEdge::Left => Vec2::new(-frame.x, 0.0),
            AlignEdge::HorizontalCenter => Vec2::new((cw - frame.width) / 2.0 - frame.x, 0.0),
            AlignEdge::Right => Vec2::new(cw - frame.width - frame.x, 0.0),
            AlignEdge::Top => Vec2::new(0.0, -frame.y),
            AlignEdge::VerticalCenter => Vec2::new(0.0, (ch - frame.height) / 2.0 - frame.y),
            AlignEdge::Bottom => Vec2::new(0.0, ch - frame.height - frame.y),
        };
        Self::translate(doc, layer, by)
    }
}

// ---------------------------------------------------------------------------
// W3-J: the shape page
// ---------------------------------------------------------------------------

/// The shape page's edits: fill on/off and colour, stroke on/off, colour and
/// width. Each returns the [`Intent::EditLayerKind`] that replaces the shape,
/// or `None` when nothing would change — the same rule every other panel
/// control follows.
///
/// The corner radius lives in the path itself: [`layer_model::ShapeLayer`]
/// keeps only the path, so [`ShapeProperties::corner_radius`] recognises a
/// rectangle or rounded rectangle - the exact path the Rectangle and Rounded
/// Rectangle tools write (`vector::shapes::rounded_rect`) - and
/// [`ShapeProperties::set_corner_radius`] rewrites it with the new radius. Any
/// other path has no corners to round, and the page says so instead of
/// drawing a dead slider.
pub struct ShapeProperties;

/// How far a parsed point may sit from the regenerated one and still count as
/// the same rectangle: well above the SVG writer's six decimals, well below a
/// pixel.
const RECT_MATCH_EPSILON: f64 = 1e-3;

fn same_path(a: &vector::Path, b: &vector::Path) -> bool {
    use vector::PathEl as E;
    let near = |p: vector::Point, q: vector::Point| {
        (p.x - q.x).abs() <= RECT_MATCH_EPSILON && (p.y - q.y).abs() <= RECT_MATCH_EPSILON
    };
    a.elements().len() == b.elements().len()
        && a.elements()
            .iter()
            .zip(b.elements())
            .all(|pair| match pair {
                (E::MoveTo(p), E::MoveTo(q)) | (E::LineTo(p), E::LineTo(q)) => near(*p, *q),
                (E::QuadTo(p1, p2), E::QuadTo(q1, q2)) => near(*p1, *q1) && near(*p2, *q2),
                (E::CurveTo(p1, p2, p3), E::CurveTo(q1, q2, q3)) => {
                    near(*p1, *q1) && near(*p2, *q2) && near(*p3, *q3)
                }
                (E::ClosePath, E::ClosePath) => true,
                _ => false,
            })
}

/// W3-J: the bounds and uniform corner radius of a path that is exactly a
/// rectangle (radius 0) or a uniformly rounded rectangle, as the shape tools
/// write them; `None` for any other path.
pub fn rect_corner_radius(path_svg: &str) -> Option<(vector::Bounds, f64)> {
    let path = vector::parse_svg(path_svg).ok()?;
    let bounds = path.bounds();
    if !(bounds.width() > 0.0 && bounds.height() > 0.0) {
        return None;
    }
    if same_path(&path, &vector::shapes::rect(bounds)) {
        return Some((bounds, 0.0));
    }
    let Some(vector::PathEl::MoveTo(start)) = path.elements().first() else {
        return None;
    };
    let radius = start.x - bounds.min.x;
    let candidate = vector::shapes::rounded_rect(bounds, vector::CornerRadii::uniform(radius));
    (radius > 0.0 && same_path(&path, &candidate)).then_some((bounds, radius))
}

impl ShapeProperties {
    fn edit(
        doc: &Document,
        layer: LayerId,
        f: impl FnOnce(&mut layer_model::ShapeLayer),
    ) -> Option<Intent> {
        let LayerKind::Shape(current) = &doc.layers.get(layer)?.kind else {
            return None;
        };
        let mut next = current.clone();
        f(&mut next);
        (next != *current).then(|| Intent::EditLayerKind {
            layer,
            kind: Box::new(LayerKind::Shape(next)),
        })
    }

    /// The shape's fill, `None` when unfilled.
    pub fn fill(doc: &Document, layer: LayerId) -> Option<Option<Rgba>> {
        match &doc.layers.get(layer)?.kind {
            LayerKind::Shape(s) => Some(s.fill),
            _ => None,
        }
    }

    /// The shape's stroke, `None` when unstroked.
    pub fn stroke(doc: &Document, layer: LayerId) -> Option<Option<ShapeStroke>> {
        match &doc.layers.get(layer)?.kind {
            LayerKind::Shape(s) => Some(s.stroke.clone()),
            _ => None,
        }
    }

    pub fn set_fill(doc: &Document, layer: LayerId, fill: Option<Rgba>) -> Option<Intent> {
        if fill.is_some_and(|c| !c.iter().all(|v| v.is_finite())) {
            return None;
        }
        Self::edit(doc, layer, |s| s.fill = fill)
    }

    /// Turn the fill on (the shape model's own default paint when there was
    /// none) or off.
    pub fn set_fill_enabled(doc: &Document, layer: LayerId, on: bool) -> Option<Intent> {
        Self::edit(doc, layer, |s| {
            if !on {
                s.fill = None;
            } else if s.fill.is_none() {
                s.fill = layer_model::ShapeLayer::default().fill;
            }
        })
    }

    /// Turn the stroke on (with the default stroke when there was none) or off.
    pub fn set_stroke_enabled(doc: &Document, layer: LayerId, on: bool) -> Option<Intent> {
        Self::edit(doc, layer, |s| {
            if on {
                if s.stroke.is_none() {
                    s.stroke = Some(ShapeStroke::default());
                }
            } else {
                s.stroke = None;
            }
        })
    }

    pub fn set_stroke_color(doc: &Document, layer: LayerId, color: Rgba) -> Option<Intent> {
        if !color.iter().all(|v| v.is_finite()) {
            return None;
        }
        Self::edit(doc, layer, |s| {
            if let Some(stroke) = s.stroke.as_mut() {
                stroke.color = color;
            }
        })
    }

    /// W3-J: the shape's uniform corner radius, in the shape's own pixels,
    /// when it is a rectangle or rounded rectangle; `None` for any other path
    /// (or a layer that is not a shape).
    pub fn corner_radius(doc: &Document, layer: LayerId) -> Option<f32> {
        match &doc.layers.get(layer)?.kind {
            LayerKind::Shape(s) => rect_corner_radius(&s.path_svg).map(|(_, r)| r as f32),
            _ => None,
        }
    }

    /// W3-J: re-round a rectangle's corners. The radius is clamped to half
    /// the shorter side (the most a corner can take); a path that is not a
    /// rectangle is left alone.
    pub fn set_corner_radius(doc: &Document, layer: LayerId, radius: f32) -> Option<Intent> {
        if !radius.is_finite() {
            return None;
        }
        let LayerKind::Shape(current) = &doc.layers.get(layer)?.kind else {
            return None;
        };
        let (bounds, _) = rect_corner_radius(&current.path_svg)?;
        let max = bounds.width().min(bounds.height()) * 0.5;
        let r = f64::from(radius).clamp(0.0, max);
        let path = vector::shapes::rounded_rect(bounds, vector::CornerRadii::uniform(r));
        let svg = vector::to_svg(&path);
        Self::edit(doc, layer, |s| s.path_svg = svg)
    }

    pub fn set_stroke_width(doc: &Document, layer: LayerId, width_px: f32) -> Option<Intent> {
        if !width_px.is_finite() || width_px < 0.0 {
            return None;
        }
        Self::edit(doc, layer, |s| {
            if let Some(stroke) = s.stroke.as_mut() {
                stroke.width_px = width_px;
            }
        })
    }
}

/// A shape colour as egui's picker sees it. Shape paint is straight-alpha in
/// the document's own space, so this is a plain 8-bit quantisation — no
/// transfer curve — which is also what makes the control stable: the same
/// model colour always shows as the same swatch, so a picker that closes
/// unchanged emits nothing.
#[must_use]
pub fn shape_to_swatch(rgba: Rgba) -> egui::Color32 {
    let ch = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgba_unmultiplied(ch(rgba[0]), ch(rgba[1]), ch(rgba[2]), ch(rgba[3]))
}

/// The picker's swatch back into shape paint.
#[must_use]
pub fn swatch_to_shape(srgb: egui::Color32) -> Rgba {
    let [r, g, b, a] = srgb.to_srgba_unmultiplied();
    let ch = |c: u8| f32::from(c) / 255.0;
    [ch(r), ch(g), ch(b), ch(a)]
}

// ---------------------------------------------------------------------------
// W3-J: the smart-object page
// ---------------------------------------------------------------------------

/// What the smart-object page says about the layer's source.
#[derive(Clone, PartialEq, Debug)]
pub struct SmartObjectSource {
    /// The file name the object was placed from (embedded) or the linked
    /// file's name; empty when the asset table has no row for it.
    pub name: String,
    /// `true` for a linked source, `false` for an embedded one.
    pub linked: bool,
}

/// The source a smart-object layer draws from, or `None` when the layer is
/// not a smart object.
pub fn smart_object_source(doc: &Document, layer: LayerId) -> Option<SmartObjectSource> {
    let LayerKind::SmartObject(so) = &doc.layers.get(layer)?.kind else {
        return None;
    };
    let name = match doc.asset_origin(so.asset) {
        Some(layer_model::AssetOrigin::Embedded { name, .. }) => name.clone(),
        Some(layer_model::AssetOrigin::Linked { path }) => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        None => String::new(),
    };
    Some(SmartObjectSource {
        name,
        linked: so.linked,
    })
}

/// The menu actions the smart-object page's two buttons route to. The page
/// does not reimplement either: Replace Contents… and Edit Contents… emit the
/// same actions as the Layer ▸ Smart Objects rows, and the page asks
/// `MenuAction::resolve` for each button's enablement, exactly as the menu
/// does, so the two cannot disagree.
pub const REPLACE_CONTENTS: crate::menu::MenuAction = crate::menu::MenuAction::ReplaceContents;
pub const EDIT_CONTENTS: crate::menu::MenuAction = crate::menu::MenuAction::EditSmartObjectContents;

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::History;
    use layer_model::{AdjustmentLayer, Layer, MaskId, ShapeLayer, TextLayer};

    fn doc_with(kind: LayerKind) -> (Document, LayerId) {
        let mut doc = Document::new(32, 32, "Test");
        let id = doc.layers.push_root(Layer::with_kind("L", kind)).unwrap();
        doc.set_active_layer(Some(id)).unwrap();
        (doc, id)
    }

    #[test]
    fn with_no_active_layer_the_panel_says_nothing_rather_than_showing_a_box() {
        let doc = Document::new(8, 8, "Empty");
        let s = PropertiesSubject::resolve(&doc, None, PropertyFocus::Layer);
        assert_eq!(s, PropertiesSubject::Nothing);
        assert_eq!(s.layer(), None);
        assert_eq!(s.title(), "Properties");
    }

    #[test]
    fn an_active_layer_that_left_the_document_falls_back_to_nothing() {
        let (mut doc, id) = doc_with(LayerKind::Raster(Default::default()));
        doc.layers.remove(id).unwrap();
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Layer),
            PropertiesSubject::Nothing
        );
    }

    #[test]
    fn the_subject_follows_the_layer_kind() {
        let (doc, id) = doc_with(LayerKind::Raster(Default::default()));
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Layer),
            PropertiesSubject::Layer(id)
        );

        let (doc, id) = doc_with(LayerKind::Text(TextLayer::default()));
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Layer),
            PropertiesSubject::Text(id)
        );

        let (doc, id) = doc_with(LayerKind::Shape(ShapeLayer::default()));
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Layer),
            PropertiesSubject::Shape(id)
        );

        let (doc, id) = doc_with(LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Invert,
        }));
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Layer),
            PropertiesSubject::Adjustment {
                layer: id,
                id: Some(AdjustmentId::Invert),
            }
        );
    }

    #[test]
    fn focusing_a_mask_shows_the_mask_and_only_when_there_is_one() {
        let (mut doc, id) = doc_with(LayerKind::Raster(Default::default()));
        // No mask yet: mask focus falls back on the layer, not an empty panel.
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Mask),
            PropertiesSubject::Layer(id)
        );
        doc.layers.get_mut(id).unwrap().mask = Some(LayerMask::new(MaskId::new()));
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Mask),
            PropertiesSubject::Mask(id)
        );
        // ...and layer focus still shows the layer.
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Layer),
            PropertiesSubject::Layer(id)
        );
    }

    #[test]
    fn a_mask_on_an_adjustment_layer_still_shows_the_mask_when_focused() {
        let (mut doc, id) = doc_with(LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Invert,
        }));
        doc.layers.get_mut(id).unwrap().mask = Some(LayerMask::new(MaskId::new()));
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Mask),
            PropertiesSubject::Mask(id)
        );
    }

    #[test]
    fn every_subject_has_a_title() {
        let (doc, id) = doc_with(LayerKind::Raster(Default::default()));
        for s in [
            PropertiesSubject::Nothing,
            PropertiesSubject::Layer(id),
            PropertiesSubject::Mask(id),
            PropertiesSubject::Adjustment {
                layer: id,
                id: Some(AdjustmentId::Curves),
            },
            PropertiesSubject::Text(id),
            PropertiesSubject::Shape(id),
        ] {
            assert!(!s.title().is_empty(), "{s:?}");
        }
        drop(doc);
    }

    // ---- mask properties --------------------------------------------------

    #[test]
    fn mask_density_and_feather_emit_patches_that_apply() {
        let (mut doc, id) = doc_with(LayerKind::Raster(Default::default()));
        doc.layers.get_mut(id).unwrap().mask = Some(LayerMask::new(MaskId::new()));
        let mut history = History::new();

        let command = MaskProperties::set_density(&doc, id, 0.4).expect("in range");
        history.apply(&mut doc, command).expect("apply");
        assert_eq!(MaskProperties::of(&doc, id).unwrap().density(), 0.4);

        let command = MaskProperties::set_feather(&doc, id, 6.0).expect("in range");
        history.apply(&mut doc, command).expect("apply");
        assert_eq!(MaskProperties::of(&doc, id).unwrap().feather_px(), 6.0);

        let command = MaskProperties::set_inverted(&doc, id, true).expect("changed");
        history.apply(&mut doc, command).expect("apply");
        assert!(MaskProperties::of(&doc, id).unwrap().inverted);
    }

    #[test]
    fn an_out_of_range_mask_value_emits_nothing_rather_than_a_doomed_command() {
        let (mut doc, id) = doc_with(LayerKind::Raster(Default::default()));
        doc.layers.get_mut(id).unwrap().mask = Some(LayerMask::new(MaskId::new()));
        assert!(MaskProperties::set_density(&doc, id, 5.0).is_none());
        assert!(MaskProperties::set_density(&doc, id, f32::NAN).is_none());
        assert!(MaskProperties::set_feather(&doc, id, -1.0).is_none());
        // The mask is untouched.
        let mask = MaskProperties::of(&doc, id).unwrap();
        assert_eq!(mask.density(), 1.0);
        assert_eq!(mask.feather_px(), 0.0);
    }

    #[test]
    fn writing_a_mask_value_it_already_holds_emits_nothing() {
        let (mut doc, id) = doc_with(LayerKind::Raster(Default::default()));
        doc.layers.get_mut(id).unwrap().mask = Some(LayerMask::new(MaskId::new()));
        assert!(MaskProperties::set_density(&doc, id, 1.0).is_none());
        assert!(MaskProperties::set_inverted(&doc, id, false).is_none());
        assert!(MaskProperties::set_enabled(&doc, id, true).is_none());
    }

    #[test]
    fn a_layer_with_no_mask_emits_no_mask_commands() {
        let (doc, id) = doc_with(LayerKind::Raster(Default::default()));
        assert!(MaskProperties::of(&doc, id).is_none());
        assert!(MaskProperties::set_density(&doc, id, 0.5).is_none());
        assert!(MaskProperties::set_linked(&doc, id, false).is_none());
    }

    // ---- adjustments ------------------------------------------------------

    #[test]
    fn editing_an_adjustment_emits_the_new_parameters() {
        let (doc, id) = doc_with(LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Posterize { levels: 256 },
        }));
        let intent = edit_adjustment(&doc, id, AdjustmentKind::Posterize { levels: 8 })
            .expect("a real change");
        let Intent::EditLayerKind { layer, kind } = intent else {
            panic!("unexpected intent");
        };
        assert_eq!(layer, id);
        assert_eq!(
            *kind,
            LayerKind::Adjustment(AdjustmentLayer {
                kind: AdjustmentKind::Posterize { levels: 8 }
            })
        );
    }

    #[test]
    fn editing_an_adjustment_to_what_it_already_is_emits_nothing() {
        let (doc, id) = doc_with(LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Invert,
        }));
        assert!(edit_adjustment(&doc, id, AdjustmentKind::Invert).is_none());
    }

    #[test]
    fn a_non_adjustment_layer_refuses_adjustment_edits() {
        let (doc, id) = doc_with(LayerKind::Raster(Default::default()));
        assert!(edit_adjustment(&doc, id, AdjustmentKind::Invert).is_none());
    }

    #[test]
    fn every_stored_adjustment_kind_maps_back_to_a_panel_entry() {
        for id in AdjustmentId::ALL {
            assert_eq!(
                adjustment_id_of(&id.identity_kind()),
                Some(*id),
                "{id:?} did not round trip"
            );
        }
        // The wide spellings map back to the same entry as the narrow ones.
        assert_eq!(
            adjustment_id_of(&AdjustmentKind::LevelsFull {
                composite: [0.0, 1.0, 1.0, 0.0, 1.0],
                red: [0.0, 1.0, 1.0, 0.0, 1.0],
                green: [0.0, 1.0, 1.0, 0.0, 1.0],
                blue: [0.0, 1.0, 1.0, 0.0, 1.0],
            }),
            Some(AdjustmentId::Levels)
        );
        // Auto has no panel entry, and says so rather than guessing.
        assert_eq!(
            adjustment_id_of(&AdjustmentKind::Auto {
                mode: layer_model::AutoAdjustment::Tone,
                clip: 0.001,
            }),
            None
        );
    }

    #[test]
    fn the_adjustments_panel_offers_every_adjustment_with_an_icon_key() {
        assert_eq!(AdjustmentsPanel::entries().len(), AdjustmentId::ALL.len());
        let mut keys: Vec<&str> = AdjustmentId::ALL
            .iter()
            .map(|id| AdjustmentsPanel::icon(*id))
            .collect();
        assert!(keys.iter().all(|k| !k.is_empty()));
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "two adjustments share an icon key");
    }

    #[test]
    fn an_adjustments_panel_button_creates_that_adjustment() {
        let command = AdjustmentsPanel::create(AdjustmentId::Threshold);
        let Command::CreateLayer { layer } = command else {
            panic!("expected a create");
        };
        assert_eq!(
            adjustment_id_of(match &layer.kind {
                LayerKind::Adjustment(a) => &a.kind,
                _ => panic!("not an adjustment layer"),
            }),
            Some(AdjustmentId::Threshold)
        );
    }

    #[test]
    fn only_the_kinds_with_extra_controls_get_a_second_section() {
        assert!(has_kind_properties(LayerClass::Adjustment));
        assert!(has_kind_properties(LayerClass::Text));
        assert!(has_kind_properties(LayerClass::Shape));
        assert!(has_kind_properties(LayerClass::Group));
        assert!(has_kind_properties(LayerClass::SmartObject));
        assert!(!has_kind_properties(LayerClass::Raster));
    }

    // ---- W3-J: transform, shape and smart-object pages --------------------

    /// A 100 x 50 shape at the origin: bounds the compositor can measure
    /// without a single stored pixel.
    fn shape_document() -> (Document, LayerId) {
        doc_with(LayerKind::Shape(ShapeLayer::from_svg("M0 0 H100 V50 H0 Z")))
    }

    #[test]
    fn a_smart_object_gets_its_own_subject_and_title() {
        let (doc, id) = doc_with(LayerKind::SmartObject(layer_model::SmartObjectLayer {
            asset: layer_model::AssetId::new(),
            linked: false,
        }));
        let s = PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Layer);
        assert_eq!(s, PropertiesSubject::SmartObject(id));
        assert_eq!(s.layer(), Some(id));
        assert!(!s.title().is_empty());
    }

    #[test]
    fn the_frame_is_the_shape_s_document_bounds() {
        let (doc, id) = shape_document();
        let frame = Transform::frame(&doc, id, &RasterInks::default()).expect("a shape has bounds");
        assert_eq!(
            frame,
            LayerFrame {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 50.0
            }
        );
    }

    #[test]
    fn an_empty_raster_layer_has_no_frame_and_emits_no_transform() {
        let (doc, id) = doc_with(LayerKind::Raster(Default::default()));
        assert!(Transform::frame(&doc, id, &RasterInks::default()).is_none());
        assert!(Transform::set_width(&doc, id, &RasterInks::default(), 10.0).is_none());
        assert!(Transform::align(&doc, id, &RasterInks::default(), AlignEdge::Right).is_none());
    }

    /// A 400 x 300 document whose raster layer holds `dabs` - opaque
    /// squares `(x, y, side)` - in the tile store, the way brush strokes
    /// leave it: each dab lives inside stored 256-pixel tiles that are
    /// mostly transparent. Returns the tile bytes too, as the application
    /// holds them.
    fn raster_document(
        dabs: &[(usize, usize, usize)],
    ) -> (Document, LayerId, compositor::MemoryTileSource) {
        use editor_core::{PixelTarget, TileEdit};
        let mut doc = Document::new(400, 300, "Raster");
        let id = doc
            .layers
            .push_root(Layer::with_kind(
                "Paint",
                LayerKind::Raster(Default::default()),
            ))
            .unwrap();
        doc.set_active_layer(Some(id)).unwrap();
        let side = raster::TILE_SIZE as usize;
        let mut tiles: std::collections::BTreeMap<(usize, usize), Vec<u8>> = Default::default();
        for &(x0, y0, n) in dabs {
            for y in y0..y0 + n {
                for x in x0..x0 + n {
                    let bytes = tiles
                        .entry((x / side, y / side))
                        .or_insert_with(|| vec![0u8; side * side * 4]);
                    let i = ((y % side) * side + x % side) * 4;
                    bytes[i..i + 4].copy_from_slice(&[200, 30, 30, 255]);
                }
            }
        }
        let mut source = compositor::MemoryTileSource::new();
        let edits: Vec<TileEdit> = tiles
            .into_iter()
            .map(|((tx, ty), bytes)| {
                let hash = source.insert_bytes(bytes);
                TileEdit::set(raster::TileCoord::new(tx as i32, ty as i32, 0), hash)
            })
            .collect();
        let paint = Command::paint_tiles(PixelTarget::Layer(id), edits).unwrap();
        History::new().apply(&mut doc, paint).unwrap();
        (doc, id, source)
    }

    #[test]
    fn a_raster_layer_s_frame_is_its_alpha_ink_not_its_stored_tiles() {
        // One 100 x 100 dab at (20, 30): one stored tile, 256 x 256.
        let (doc, id, source) = raster_document(&[(20, 30, 100)]);
        let inks = RasterInks::measure(&doc, &source, id);
        assert_eq!(
            Transform::frame(&doc, id, &inks),
            Some(LayerFrame {
                x: 20.0,
                y: 30.0,
                width: 100.0,
                height: 100.0
            })
        );
        // Without the application's measurement the panel knows nothing
        // rather than guessing the tile box.
        assert!(Transform::frame(&doc, id, &RasterInks::default()).is_none());
    }

    #[test]
    fn align_moves_a_raster_dab_flush_with_the_canvas_edges() {
        let (mut doc, id, source) = raster_document(&[(0, 0, 100)]);
        let inks = RasterInks::measure(&doc, &source, id);
        let mut history = History::new();
        let at = |doc: &Document| {
            let f = Transform::frame(doc, id, &inks).unwrap();
            (f.x, f.y, f.width, f.height)
        };
        step(&mut history, &mut doc, |d| {
            Transform::align(d, id, &inks, AlignEdge::Right)
        });
        assert_eq!(at(&doc), (300.0, 0.0, 100.0, 100.0), "flush right");
        step(&mut history, &mut doc, |d| {
            Transform::align(d, id, &inks, AlignEdge::Bottom)
        });
        assert_eq!(at(&doc), (300.0, 200.0, 100.0, 100.0), "flush bottom");
        step(&mut history, &mut doc, |d| {
            Transform::align(d, id, &inks, AlignEdge::HorizontalCenter)
        });
        step(&mut history, &mut doc, |d| {
            Transform::align(d, id, &inks, AlignEdge::VerticalCenter)
        });
        assert_eq!(at(&doc), (150.0, 100.0, 100.0, 100.0), "centred");
    }

    #[test]
    fn a_typed_width_scales_a_full_raster_background_to_exactly_that_width() {
        // A 400 x 300 background spans four stored tiles (512 x 512).
        let (mut doc, id, source) = raster_document(&[(0, 0, 300), (100, 0, 300)]);
        let inks = RasterInks::measure(&doc, &source, id);
        let frame = Transform::frame(&doc, id, &inks).unwrap();
        assert_eq!((frame.width, frame.height), (400.0, 300.0));
        let mut history = History::new();
        step(&mut history, &mut doc, |d| {
            Transform::set_width(d, id, &inks, 200.0)
        });
        assert_eq!(history.undo_depth(), 1);
        let frame = Transform::frame(&doc, id, &inks).unwrap();
        assert_eq!((frame.x, frame.width, frame.height), (0.0, 200.0, 300.0));
    }

    #[test]
    fn a_stale_ink_measurement_is_refused_once_the_tiles_change() {
        use editor_core::{PixelTarget, TileEdit};
        let (mut doc, id, mut source) = raster_document(&[(0, 0, 100)]);
        let inks = RasterInks::measure(&doc, &source, id);
        assert!(inks.current(&doc, id).is_some());
        // A stroke lands in a new tile: the hashes the measurement read are
        // no longer the layer's, so the old box must not be shown.
        let side = raster::TILE_SIZE as usize;
        let hash = source.insert_bytes(vec![255u8; side * side * 4]);
        let paint = Command::paint_tiles(
            PixelTarget::Layer(id),
            [TileEdit::set(raster::TileCoord::new(1, 0, 0), hash)],
        )
        .unwrap();
        History::new().apply(&mut doc, paint).unwrap();
        assert!(inks.current(&doc, id).is_none());
        assert!(Transform::frame(&doc, id, &inks).is_none());
        // Re-measured, it covers both.
        let inks = RasterInks::measure(&doc, &source, id);
        let f = Transform::frame(&doc, id, &inks).unwrap();
        assert_eq!((f.x, f.y, f.width, f.height), (0.0, 0.0, 512.0, 256.0));
    }

    #[test]
    fn a_group_s_frame_is_the_union_of_its_children_s_ink() {
        use editor_core::{PixelTarget, TileEdit};
        let (mut doc, a, mut source) = raster_document(&[(10, 10, 20)]);
        let b = doc
            .layers
            .push_root(Layer::with_kind("B", LayerKind::Raster(Default::default())))
            .unwrap();
        let side = raster::TILE_SIZE as usize;
        let mut bytes = vec![0u8; side * side * 4];
        for y in 100..140usize {
            for x in 200..250usize {
                let i = (y * side + x) * 4;
                bytes[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
        }
        let hash = source.insert_bytes(bytes);
        let paint = Command::paint_tiles(
            PixelTarget::Layer(b),
            [TileEdit::set(raster::TileCoord::new(0, 0, 0), hash)],
        )
        .unwrap();
        History::new().apply(&mut doc, paint).unwrap();
        let group = doc
            .layers
            .push_root(Layer::with_kind("G", LayerKind::Group(Default::default())))
            .unwrap();
        for child in [a, b] {
            doc.layers.move_layer(child, Some(group), 0).unwrap();
        }
        let inks = RasterInks::measure(&doc, &source, group);
        assert_eq!(
            Transform::frame(&doc, group, &inks),
            Some(LayerFrame {
                x: 10.0,
                y: 10.0,
                width: 240.0,
                height: 130.0
            })
        );
    }

    /// Apply the command a builder makes from the current document, as one
    /// history entry.
    fn step(
        history: &mut History,
        doc: &mut Document,
        make: impl FnOnce(&Document) -> Option<Command>,
    ) {
        let command = make(doc).expect("a real command");
        history.apply(doc, command).expect("apply");
    }

    #[test]
    fn a_typed_width_scales_about_the_left_edge_as_one_command() {
        let (mut doc, id) = shape_document();
        let mut history = History::new();
        let command =
            Transform::set_width(&doc, id, &RasterInks::default(), 200.0).expect("a real resize");
        assert!(matches!(command, Command::TransformLayer { .. }));
        history.apply(&mut doc, command).expect("apply");
        assert_eq!(history.undo_depth(), 1, "one undo step");
        let frame = Transform::frame(&doc, id, &RasterInks::default()).unwrap();
        assert_eq!((frame.x, frame.width), (0.0, 200.0));
        assert_eq!((frame.y, frame.height), (0.0, 50.0), "height untouched");

        // Height too, keeping the top edge; and X / Y translate.
        step(&mut history, &mut doc, |d| {
            Transform::set_height(d, id, &RasterInks::default(), 25.0)
        });
        step(&mut history, &mut doc, |d| {
            Transform::set_x(d, id, &RasterInks::default(), 10.0)
        });
        step(&mut history, &mut doc, |d| {
            Transform::set_y(d, id, &RasterInks::default(), 7.0)
        });
        let frame = Transform::frame(&doc, id, &RasterInks::default()).unwrap();
        assert_eq!(
            frame,
            LayerFrame {
                x: 10.0,
                y: 7.0,
                width: 200.0,
                height: 25.0
            }
        );
        // And it all undoes back to the shape the document started with.
        for _ in 0..4 {
            history.undo(&mut doc).unwrap();
        }
        assert_eq!(
            Transform::frame(&doc, id, &RasterInks::default()).unwrap(),
            LayerFrame {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 50.0
            }
        );
    }

    #[test]
    fn a_width_the_layer_already_has_or_cannot_have_emits_nothing() {
        let (doc, id) = shape_document();
        assert!(Transform::set_width(&doc, id, &RasterInks::default(), 100.0).is_none());
        assert!(Transform::set_width(&doc, id, &RasterInks::default(), 0.0).is_none());
        assert!(Transform::set_width(&doc, id, &RasterInks::default(), -4.0).is_none());
        assert!(Transform::set_width(&doc, id, &RasterInks::default(), f32::NAN).is_none());
        assert!(Transform::set_x(&doc, id, &RasterInks::default(), f32::INFINITY).is_none());
    }

    #[test]
    fn a_position_locked_layer_refuses_the_transform_block() {
        let (mut doc, id) = shape_document();
        doc.layers.get_mut(id).unwrap().locked = layer_model::LockState {
            position: true,
            ..Default::default()
        };
        assert!(Transform::is_locked(&doc, id));
        assert!(Transform::set_width(&doc, id, &RasterInks::default(), 200.0).is_none());
        assert!(Transform::align(&doc, id, &RasterInks::default(), AlignEdge::Bottom).is_none());
    }

    #[test]
    fn align_snaps_the_frame_to_the_canvas_edges_and_centres() {
        // A 32 x 32 canvas holding a 100 x 50 shape: every edge is a move.
        let (mut doc, id) = shape_document();
        let mut history = History::new();
        let expect = |doc: &Document, x: f32, y: f32| {
            let f = Transform::frame(doc, id, &RasterInks::default()).unwrap();
            assert_eq!((f.x, f.y), (x, y));
        };
        step(&mut history, &mut doc, |d| {
            Transform::align(d, id, &RasterInks::default(), AlignEdge::Right)
        });
        expect(&doc, 32.0 - 100.0, 0.0);
        step(&mut history, &mut doc, |d| {
            Transform::align(d, id, &RasterInks::default(), AlignEdge::Bottom)
        });
        expect(&doc, 32.0 - 100.0, 32.0 - 50.0);
        step(&mut history, &mut doc, |d| {
            Transform::align(d, id, &RasterInks::default(), AlignEdge::HorizontalCenter)
        });
        expect(&doc, (32.0 - 100.0) / 2.0, 32.0 - 50.0);
        step(&mut history, &mut doc, |d| {
            Transform::align(d, id, &RasterInks::default(), AlignEdge::VerticalCenter)
        });
        expect(&doc, (32.0 - 100.0) / 2.0, (32.0 - 50.0) / 2.0);
        step(&mut history, &mut doc, |d| {
            Transform::align(d, id, &RasterInks::default(), AlignEdge::Left)
        });
        step(&mut history, &mut doc, |d| {
            Transform::align(d, id, &RasterInks::default(), AlignEdge::Top)
        });
        expect(&doc, 0.0, 0.0);
        // Already flush: nothing to emit.
        assert!(Transform::align(&doc, id, &RasterInks::default(), AlignEdge::Left).is_none());
    }

    #[test]
    fn the_shape_page_edits_fill_and_stroke_and_refuses_no_ops() {
        let (doc, id) = shape_document();
        assert_eq!(
            ShapeProperties::fill(&doc, id),
            Some(Some([0.0, 0.0, 0.0, 1.0]))
        );
        assert_eq!(ShapeProperties::stroke(&doc, id), Some(None));
        // Same fill again: nothing.
        assert!(ShapeProperties::set_fill(&doc, id, Some([0.0, 0.0, 0.0, 1.0])).is_none());
        assert!(ShapeProperties::set_fill(&doc, id, Some([f32::NAN, 0.0, 0.0, 1.0])).is_none());
        // Stroke colour and width go nowhere while there is no stroke.
        assert!(ShapeProperties::set_stroke_color(&doc, id, [1.0, 0.0, 0.0, 1.0]).is_none());
        assert!(ShapeProperties::set_stroke_width(&doc, id, 3.0).is_none());

        let Some(Intent::EditLayerKind { kind, .. }) = ShapeProperties::set_fill(&doc, id, None)
        else {
            panic!("expected an edit");
        };
        assert!(matches!(*kind, LayerKind::Shape(ref s) if s.fill.is_none()));

        let Some(Intent::EditLayerKind { kind, .. }) =
            ShapeProperties::set_stroke_enabled(&doc, id, true)
        else {
            panic!("expected an edit");
        };
        let LayerKind::Shape(stroked) = *kind else {
            panic!("not a shape");
        };
        assert_eq!(stroked.stroke, Some(ShapeStroke::default()));
        // With the stroke on, colour and width edit it.
        let (doc2, id2) = doc_with(LayerKind::Shape(stroked));
        let Some(Intent::EditLayerKind { kind, .. }) =
            ShapeProperties::set_stroke_width(&doc2, id2, 4.0)
        else {
            panic!("expected an edit");
        };
        assert!(
            matches!(*kind, LayerKind::Shape(ref s) if s.stroke.as_ref().unwrap().width_px == 4.0)
        );
        assert!(ShapeProperties::set_stroke_width(&doc2, id2, -1.0).is_none());
        // A raster layer is not a shape.
        let (raster, rid) = doc_with(LayerKind::Raster(Default::default()));
        assert!(ShapeProperties::fill(&raster, rid).is_none());
        assert!(ShapeProperties::set_fill(&raster, rid, None).is_none());
    }

    #[test]
    fn the_fill_toggle_restores_the_model_default_paint() {
        let (doc, id) = doc_with(LayerKind::Shape(ShapeLayer {
            fill: None,
            ..ShapeLayer::from_svg("M0 0 H10 V10 Z")
        }));
        let Some(Intent::EditLayerKind { kind, .. }) =
            ShapeProperties::set_fill_enabled(&doc, id, true)
        else {
            panic!("expected an edit");
        };
        assert!(matches!(*kind, LayerKind::Shape(ref s) if s.fill == ShapeLayer::default().fill));
        assert!(
            ShapeProperties::set_fill_enabled(&doc, id, false).is_none(),
            "already off"
        );
    }

    #[test]
    fn shape_colours_round_trip_through_the_picker_swatch() {
        let c = [0.25, 0.5, 1.0, 0.75];
        let back = swatch_to_shape(shape_to_swatch(c));
        for (a, b) in c.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1.0 / 255.0, "{c:?} -> {back:?}");
        }
        assert_eq!(shape_to_swatch(c), shape_to_swatch(back), "stable");
    }

    #[test]
    fn the_smart_object_page_names_its_source() {
        let asset = layer_model::AssetId::new();
        let (mut doc, id) = doc_with(LayerKind::SmartObject(layer_model::SmartObjectLayer {
            asset,
            linked: false,
        }));
        // No asset row yet: the page still resolves, with an empty name.
        assert_eq!(
            smart_object_source(&doc, id),
            Some(SmartObjectSource {
                name: String::new(),
                linked: false
            })
        );
        doc.set_asset_origin(layer_model::AssetRecord {
            id: asset,
            origin: layer_model::AssetOrigin::Embedded {
                name: "logo.png".into(),
                bytes: Vec::new(),
            },
            source_size: None,
        });
        assert_eq!(
            smart_object_source(&doc, id).unwrap().name,
            "logo.png".to_string()
        );
        doc.set_asset_origin(layer_model::AssetRecord {
            id: asset,
            origin: layer_model::AssetOrigin::Linked {
                path: std::path::PathBuf::from("assets/photo.jpg"),
            },
            source_size: None,
        });
        assert_eq!(
            smart_object_source(&doc, id).unwrap().name,
            "photo.jpg".to_string()
        );
        // Not a smart object: no page.
        let (raster, rid) = doc_with(LayerKind::Raster(Default::default()));
        assert!(smart_object_source(&raster, rid).is_none());
    }
}
