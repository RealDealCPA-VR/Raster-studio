//! The menu bar, as a value.
//!
//! # The rule this module exists to enforce
//!
//! **A menu item that does nothing is a bug.** Every item resolves, against the
//! current [`MenuContext`], to exactly one of two things:
//!
//! * [`Resolution::Enabled`] carrying the [`Intent`] performing it produces, or
//! * [`Resolution::Disabled`] carrying a sentence saying *why*, which the UI
//!   shows on hover.
//!
//! There is no third case and no `Option`, so "I forgot to wire this one up"
//! is a compile error or a test failure rather than a dead item the user
//! discovers. `every_item_in_every_menu_resolves` walks all nine menus across a
//! range of document states and asserts exactly that.
//!
//! # Where the commands come from
//!
//! Wherever the edit is fully determined by what the UI already knows, the item
//! resolves straight to an [`editor_core::Command`] — Layer ▸ Arrange ▸ Bring
//! Forward is a [`Command::MoveLayer`] with the index worked out here, and
//! Delete Layer is a [`Command::DeleteLayer`]. Items that need a dialog, the
//! file system, or a pass over pixels resolve to [`Intent::Action`] carrying
//! the [`MenuAction`] itself: still an enumerable value a test can assert on,
//! just performed elsewhere.

use editor_core::{Command, Document, History, LayerPatch, Patch};
use layer_model::{
    AdjustmentKind, AdjustmentLayer, ClippingMode, Layer, LayerId, LayerKind, LayerMask, LockState,
    MaskId,
};
use raster::ExportFormat;

use crate::dock::{DockState, LayoutId, PanelId};
use crate::intent::{ClipboardState, Intent, ViewFlag, ViewFlags};
use crate::shortcut::{Key, Shortcut};

// ---------------------------------------------------------------------------
// Payload vocabularies
// ---------------------------------------------------------------------------

/// The adjustments offered in Image ▸ Adjustments and in the Adjustments panel.
///
/// One variant per adjustment `layer_model` can store, so an entry here always
/// has somewhere to go.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum AdjustmentId {
    BrightnessContrast,
    Levels,
    Curves,
    Exposure,
    Vibrance,
    HueSaturation,
    ColorBalance,
    BlackAndWhite,
    PhotoFilter,
    ChannelMixer,
    Invert,
    Posterize,
    Threshold,
    GradientMap,
    SelectiveColor,
}

impl AdjustmentId {
    pub const ALL: &'static [AdjustmentId] = &[
        AdjustmentId::BrightnessContrast,
        AdjustmentId::Levels,
        AdjustmentId::Curves,
        AdjustmentId::Exposure,
        AdjustmentId::Vibrance,
        AdjustmentId::HueSaturation,
        AdjustmentId::ColorBalance,
        AdjustmentId::BlackAndWhite,
        AdjustmentId::PhotoFilter,
        AdjustmentId::ChannelMixer,
        AdjustmentId::Invert,
        AdjustmentId::Posterize,
        AdjustmentId::Threshold,
        AdjustmentId::GradientMap,
        AdjustmentId::SelectiveColor,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            AdjustmentId::BrightnessContrast => "Brightness/Contrast",
            AdjustmentId::Levels => "Levels",
            AdjustmentId::Curves => "Curves",
            AdjustmentId::Exposure => "Exposure",
            AdjustmentId::Vibrance => "Vibrance",
            AdjustmentId::HueSaturation => "Hue/Saturation",
            AdjustmentId::ColorBalance => "Color Balance",
            AdjustmentId::BlackAndWhite => "Black & White",
            AdjustmentId::PhotoFilter => "Photo Filter",
            AdjustmentId::ChannelMixer => "Channel Mixer",
            AdjustmentId::Invert => "Invert",
            AdjustmentId::Posterize => "Posterize",
            AdjustmentId::Threshold => "Threshold",
            AdjustmentId::GradientMap => "Gradient Map",
            AdjustmentId::SelectiveColor => "Selective Color",
        }
    }

    /// The stored parameters a freshly created adjustment layer starts with.
    ///
    /// Every one that *can* be the identity is, so adding an adjustment layer
    /// changes no pixel until the user moves a control. Four cannot be: invert,
    /// threshold, black & white and posterize are all defined as changing every
    /// pixel, and they start at the settings Photoshop starts them at.
    /// `a_new_adjustment_layer_carries_readable_starting_parameters` pins
    /// exactly which four, so a fifth cannot be added by accident.
    pub fn identity_kind(self) -> AdjustmentKind {
        match self {
            AdjustmentId::BrightnessContrast => AdjustmentKind::BrightnessContrast {
                brightness: 0.0,
                contrast: 0.0,
            },
            AdjustmentId::Levels => AdjustmentKind::Levels {
                black: 0.0,
                white: 1.0,
                gamma: 1.0,
            },
            AdjustmentId::Curves => AdjustmentKind::Curves {
                points: vec![[0.0, 0.0], [1.0, 1.0]],
            },
            AdjustmentId::Exposure => AdjustmentKind::Exposure { stops: 0.0 },
            AdjustmentId::Vibrance => AdjustmentKind::Vibrance {
                vibrance: 0.0,
                saturation: 0.0,
            },
            AdjustmentId::HueSaturation => AdjustmentKind::HueSaturation {
                hue: 0.0,
                saturation: 0.0,
                lightness: 0.0,
            },
            AdjustmentId::ColorBalance => AdjustmentKind::ColorBalance {
                shadows: [0.0; 3],
                midtones: [0.0; 3],
                highlights: [0.0; 3],
            },
            AdjustmentId::BlackAndWhite => AdjustmentKind::BlackAndWhite {
                weights: adjustments::BW_DEFAULT_WEIGHTS,
                tint: None,
            },
            AdjustmentId::PhotoFilter => AdjustmentKind::PhotoFilter {
                color_srgb: [1.0, 0.5, 0.1],
                density: 0.0,
                preserve_luminosity: true,
            },
            AdjustmentId::ChannelMixer => AdjustmentKind::ChannelMixer {
                rows: [
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                ],
                monochrome: false,
            },
            AdjustmentId::Invert => AdjustmentKind::Invert,
            AdjustmentId::Posterize => AdjustmentKind::Posterize { levels: 4 },
            AdjustmentId::Threshold => AdjustmentKind::Threshold { level: 0.5 },
            AdjustmentId::GradientMap => AdjustmentKind::GradientMap {
                stops: vec![(0.0, [0.0, 0.0, 0.0]), (1.0, [1.0, 1.0, 1.0])],
                reverse: false,
            },
            AdjustmentId::SelectiveColor => AdjustmentKind::SelectiveColor {
                ranges: [[0.0; 4]; 9],
                relative: true,
            },
        }
    }

    /// The command that adds this adjustment as a new layer.
    pub fn create_command(self) -> Command {
        Command::create_layer(Layer::with_kind(
            self.label(),
            LayerKind::Adjustment(AdjustmentLayer {
                kind: self.identity_kind(),
            }),
        ))
    }
}

/// A filter the `filters` crate actually implements.
///
/// The variant list is deliberately not aspirational: every one maps to a
/// function that exists, so there is no Filter-menu item that opens a dialog
/// with nothing behind it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum FilterId {
    // Blur
    BoxBlur,
    GaussianBlur,
    LensBlur,
    MotionBlur,
    RadialBlur,
    SurfaceBlur,
    // Sharpen
    SmartSharpen,
    UnsharpMask,
    // Noise
    AddNoise,
    Despeckle,
    DustAndScratches,
    Median,
    ReduceNoise,
    // Distort
    Pinch,
    PolarCoordinates,
    Ripple,
    Shear,
    Spherize,
    Twirl,
    Wave,
    ZigZag,
    // Pixelate
    ColorHalftone,
    Crystallize,
    Mosaic,
    Pointillize,
    // Render
    Clouds,
    DifferenceClouds,
    Fibers,
    GradientFill,
    LensFlare,
    // Stylize
    Diffuse,
    Emboss,
    FindEdges,
    OilPaint,
    Solarize,
    Wind,
    // Other
    Custom,
    HighPass,
    Maximum,
    Minimum,
    Offset,
}

/// A submenu of the Filter menu.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum FilterGroup {
    Blur,
    Sharpen,
    Noise,
    Distort,
    Pixelate,
    Render,
    Stylize,
    Other,
}

impl FilterGroup {
    pub const ALL: &'static [FilterGroup] = &[
        FilterGroup::Blur,
        FilterGroup::Sharpen,
        FilterGroup::Noise,
        FilterGroup::Distort,
        FilterGroup::Pixelate,
        FilterGroup::Render,
        FilterGroup::Stylize,
        FilterGroup::Other,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            FilterGroup::Blur => "Blur",
            FilterGroup::Sharpen => "Sharpen",
            FilterGroup::Noise => "Noise",
            FilterGroup::Distort => "Distort",
            FilterGroup::Pixelate => "Pixelate",
            FilterGroup::Render => "Render",
            FilterGroup::Stylize => "Stylize",
            FilterGroup::Other => "Other",
        }
    }
}

impl FilterId {
    pub const ALL: &'static [FilterId] = &[
        FilterId::BoxBlur,
        FilterId::GaussianBlur,
        FilterId::LensBlur,
        FilterId::MotionBlur,
        FilterId::RadialBlur,
        FilterId::SurfaceBlur,
        FilterId::SmartSharpen,
        FilterId::UnsharpMask,
        FilterId::AddNoise,
        FilterId::Despeckle,
        FilterId::DustAndScratches,
        FilterId::Median,
        FilterId::ReduceNoise,
        FilterId::Pinch,
        FilterId::PolarCoordinates,
        FilterId::Ripple,
        FilterId::Shear,
        FilterId::Spherize,
        FilterId::Twirl,
        FilterId::Wave,
        FilterId::ZigZag,
        FilterId::ColorHalftone,
        FilterId::Crystallize,
        FilterId::Mosaic,
        FilterId::Pointillize,
        FilterId::Clouds,
        FilterId::DifferenceClouds,
        FilterId::Fibers,
        FilterId::GradientFill,
        FilterId::LensFlare,
        FilterId::Diffuse,
        FilterId::Emboss,
        FilterId::FindEdges,
        FilterId::OilPaint,
        FilterId::Solarize,
        FilterId::Wind,
        FilterId::Custom,
        FilterId::HighPass,
        FilterId::Maximum,
        FilterId::Minimum,
        FilterId::Offset,
    ];

    pub const fn group(self) -> FilterGroup {
        match self {
            FilterId::BoxBlur
            | FilterId::GaussianBlur
            | FilterId::LensBlur
            | FilterId::MotionBlur
            | FilterId::RadialBlur
            | FilterId::SurfaceBlur => FilterGroup::Blur,
            FilterId::SmartSharpen | FilterId::UnsharpMask => FilterGroup::Sharpen,
            FilterId::AddNoise
            | FilterId::Despeckle
            | FilterId::DustAndScratches
            | FilterId::Median
            | FilterId::ReduceNoise => FilterGroup::Noise,
            FilterId::Pinch
            | FilterId::PolarCoordinates
            | FilterId::Ripple
            | FilterId::Shear
            | FilterId::Spherize
            | FilterId::Twirl
            | FilterId::Wave
            | FilterId::ZigZag => FilterGroup::Distort,
            FilterId::ColorHalftone
            | FilterId::Crystallize
            | FilterId::Mosaic
            | FilterId::Pointillize => FilterGroup::Pixelate,
            FilterId::Clouds
            | FilterId::DifferenceClouds
            | FilterId::Fibers
            | FilterId::GradientFill
            | FilterId::LensFlare => FilterGroup::Render,
            FilterId::Diffuse
            | FilterId::Emboss
            | FilterId::FindEdges
            | FilterId::OilPaint
            | FilterId::Solarize
            | FilterId::Wind => FilterGroup::Stylize,
            FilterId::Custom
            | FilterId::HighPass
            | FilterId::Maximum
            | FilterId::Minimum
            | FilterId::Offset => FilterGroup::Other,
        }
    }

    /// Menu label. A trailing ellipsis means the filter opens a dialog; a
    /// filter with no parameters applies immediately and carries none.
    pub const fn label(self) -> &'static str {
        match self {
            FilterId::BoxBlur => "Box Blur…",
            FilterId::GaussianBlur => "Gaussian Blur…",
            FilterId::LensBlur => "Lens Blur…",
            FilterId::MotionBlur => "Motion Blur…",
            FilterId::RadialBlur => "Radial Blur…",
            FilterId::SurfaceBlur => "Surface Blur…",
            FilterId::SmartSharpen => "Smart Sharpen…",
            FilterId::UnsharpMask => "Unsharp Mask…",
            FilterId::AddNoise => "Add Noise…",
            FilterId::Despeckle => "Despeckle",
            FilterId::DustAndScratches => "Dust & Scratches…",
            FilterId::Median => "Median…",
            FilterId::ReduceNoise => "Reduce Noise…",
            FilterId::Pinch => "Pinch…",
            FilterId::PolarCoordinates => "Polar Coordinates…",
            FilterId::Ripple => "Ripple…",
            FilterId::Shear => "Shear…",
            FilterId::Spherize => "Spherize…",
            FilterId::Twirl => "Twirl…",
            FilterId::Wave => "Wave…",
            FilterId::ZigZag => "ZigZag…",
            FilterId::ColorHalftone => "Color Halftone…",
            FilterId::Crystallize => "Crystallize…",
            FilterId::Mosaic => "Mosaic…",
            FilterId::Pointillize => "Pointillize…",
            FilterId::Clouds => "Clouds",
            FilterId::DifferenceClouds => "Difference Clouds",
            FilterId::Fibers => "Fibers…",
            FilterId::GradientFill => "Gradient…",
            FilterId::LensFlare => "Lens Flare…",
            FilterId::Diffuse => "Diffuse…",
            FilterId::Emboss => "Emboss…",
            FilterId::FindEdges => "Find Edges",
            FilterId::OilPaint => "Oil Paint…",
            FilterId::Solarize => "Solarize",
            FilterId::Wind => "Wind…",
            FilterId::Custom => "Custom…",
            FilterId::HighPass => "High Pass…",
            FilterId::Maximum => "Maximum…",
            FilterId::Minimum => "Minimum…",
            FilterId::Offset => "Offset…",
        }
    }
}

/// One of the ten layer-style slots.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum EffectSlot {
    DropShadow,
    InnerShadow,
    OuterGlow,
    InnerGlow,
    BevelEmboss,
    Satin,
    ColorOverlay,
    GradientOverlay,
    PatternOverlay,
    Stroke,
}

impl EffectSlot {
    pub const ALL: &'static [EffectSlot] = &[
        EffectSlot::DropShadow,
        EffectSlot::InnerShadow,
        EffectSlot::OuterGlow,
        EffectSlot::InnerGlow,
        EffectSlot::BevelEmboss,
        EffectSlot::Satin,
        EffectSlot::ColorOverlay,
        EffectSlot::GradientOverlay,
        EffectSlot::PatternOverlay,
        EffectSlot::Stroke,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            EffectSlot::DropShadow => "Drop Shadow…",
            EffectSlot::InnerShadow => "Inner Shadow…",
            EffectSlot::OuterGlow => "Outer Glow…",
            EffectSlot::InnerGlow => "Inner Glow…",
            EffectSlot::BevelEmboss => "Bevel & Emboss…",
            EffectSlot::Satin => "Satin…",
            EffectSlot::ColorOverlay => "Color Overlay…",
            EffectSlot::GradientOverlay => "Gradient Overlay…",
            EffectSlot::PatternOverlay => "Pattern Overlay…",
            EffectSlot::Stroke => "Stroke…",
        }
    }

    /// Whether this slot is filled on a layer.
    pub fn is_set(self, effects: &layer_model::LayerEffects) -> bool {
        match self {
            EffectSlot::DropShadow => effects.drop_shadow.is_some(),
            EffectSlot::InnerShadow => effects.inner_shadow.is_some(),
            EffectSlot::OuterGlow => effects.outer_glow.is_some(),
            EffectSlot::InnerGlow => effects.inner_glow.is_some(),
            EffectSlot::BevelEmboss => effects.bevel_emboss.is_some(),
            EffectSlot::Satin => effects.satin.is_some(),
            EffectSlot::ColorOverlay => effects.color_overlay.is_some(),
            EffectSlot::GradientOverlay => effects.gradient_overlay.is_some(),
            EffectSlot::PatternOverlay => effects.pattern_overlay.is_some(),
            EffectSlot::Stroke => effects.stroke.is_some(),
        }
    }
}

/// Where in its sibling list a layer is being sent.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Arrange {
    BringToFront,
    BringForward,
    SendBackward,
    SendToBack,
}

impl Arrange {
    pub const ALL: &'static [Arrange] = &[
        Arrange::BringToFront,
        Arrange::BringForward,
        Arrange::SendBackward,
        Arrange::SendToBack,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Arrange::BringToFront => "Bring to Front",
            Arrange::BringForward => "Bring Forward",
            Arrange::SendBackward => "Send Backward",
            Arrange::SendToBack => "Send to Back",
        }
    }

    fn shortcut(self) -> Shortcut {
        match self {
            Arrange::BringToFront => Shortcut::ctrl_shift_key(Key::RightBracket),
            Arrange::BringForward => Shortcut::ctrl_key(Key::RightBracket),
            Arrange::SendBackward => Shortcut::ctrl_key(Key::LeftBracket),
            Arrange::SendToBack => Shortcut::ctrl_shift_key(Key::LeftBracket),
        }
    }

    /// The index this move lands the layer on, given where it is now.
    ///
    /// Layers are stored top-most first, so index `0` is the front. The index
    /// is the destination in the sibling list *after* the layer has been
    /// removed from it, which is what [`layer_model::LayerTree::move_layer`]
    /// takes; that is why "send to back" is `siblings - 1` and not `siblings`.
    pub fn target_index(self, index: usize, siblings: usize) -> usize {
        match self {
            Arrange::BringToFront => 0,
            Arrange::BringForward => index.saturating_sub(1),
            Arrange::SendBackward => (index + 1).min(siblings.saturating_sub(1)),
            Arrange::SendToBack => siblings.saturating_sub(1),
        }
    }

    /// `true` when the move would change nothing.
    pub fn is_noop(self, index: usize, siblings: usize) -> bool {
        siblings <= 1 || self.target_index(index, siblings) == index
    }

    /// Why the move is unavailable, when it is.
    const fn blocked_reason(self) -> &'static str {
        match self {
            Arrange::BringToFront | Arrange::BringForward => {
                "The layer is already at the front of its group"
            }
            Arrange::SendBackward | Arrange::SendToBack => {
                "The layer is already at the back of its group"
            }
        }
    }
}

/// A transform offered under Edit ▸ Transform.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum TransformOp {
    Scale,
    Rotate,
    Skew,
    Distort,
    Perspective,
    Warp,
    Rotate180,
    Rotate90Cw,
    Rotate90Ccw,
    FlipHorizontal,
    FlipVertical,
}

impl TransformOp {
    pub const ALL: &'static [TransformOp] = &[
        TransformOp::Scale,
        TransformOp::Rotate,
        TransformOp::Skew,
        TransformOp::Distort,
        TransformOp::Perspective,
        TransformOp::Warp,
        TransformOp::Rotate180,
        TransformOp::Rotate90Cw,
        TransformOp::Rotate90Ccw,
        TransformOp::FlipHorizontal,
        TransformOp::FlipVertical,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            TransformOp::Scale => "Scale",
            TransformOp::Rotate => "Rotate",
            TransformOp::Skew => "Skew",
            TransformOp::Distort => "Distort",
            TransformOp::Perspective => "Perspective",
            TransformOp::Warp => "Warp",
            TransformOp::Rotate180 => "Rotate 180°",
            TransformOp::Rotate90Cw => "Rotate 90° Clockwise",
            TransformOp::Rotate90Ccw => "Rotate 90° Counter Clockwise",
            TransformOp::FlipHorizontal => "Flip Horizontal",
            TransformOp::FlipVertical => "Flip Vertical",
        }
    }
}

/// A whole-canvas rotation under Image ▸ Image Rotation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum CanvasRotation {
    Deg180,
    Deg90Cw,
    Deg90Ccw,
    Arbitrary,
    FlipHorizontal,
    FlipVertical,
}

impl CanvasRotation {
    pub const ALL: &'static [CanvasRotation] = &[
        CanvasRotation::Deg180,
        CanvasRotation::Deg90Cw,
        CanvasRotation::Deg90Ccw,
        CanvasRotation::Arbitrary,
        CanvasRotation::FlipHorizontal,
        CanvasRotation::FlipVertical,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            CanvasRotation::Deg180 => "180°",
            CanvasRotation::Deg90Cw => "90° Clockwise",
            CanvasRotation::Deg90Ccw => "90° Counter Clockwise",
            CanvasRotation::Arbitrary => "Arbitrary…",
            CanvasRotation::FlipHorizontal => "Flip Canvas Horizontal",
            CanvasRotation::FlipVertical => "Flip Canvas Vertical",
        }
    }
}

/// A Select ▸ Modify operation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum ModifySelection {
    Border,
    Smooth,
    Expand,
    Contract,
    Feather,
}

impl ModifySelection {
    pub const ALL: &'static [ModifySelection] = &[
        ModifySelection::Border,
        ModifySelection::Smooth,
        ModifySelection::Expand,
        ModifySelection::Contract,
        ModifySelection::Feather,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            ModifySelection::Border => "Border…",
            ModifySelection::Smooth => "Smooth…",
            ModifySelection::Expand => "Expand…",
            ModifySelection::Contract => "Contract…",
            ModifySelection::Feather => "Feather…",
        }
    }
}

/// A document colour mode.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum ColorMode {
    Rgb,
    Grayscale,
    Lab,
    Cmyk,
    Indexed,
}

impl ColorMode {
    pub const ALL: &'static [ColorMode] = &[
        ColorMode::Rgb,
        ColorMode::Grayscale,
        ColorMode::Lab,
        ColorMode::Cmyk,
        ColorMode::Indexed,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            ColorMode::Rgb => "RGB Color",
            ColorMode::Grayscale => "Grayscale",
            ColorMode::Lab => "Lab Color",
            ColorMode::Cmyk => "CMYK Color",
            ColorMode::Indexed => "Indexed Color",
        }
    }

    /// Whether this build can convert a document into the mode.
    ///
    /// The unsupported ones are listed and *disabled with a reason* rather than
    /// hidden, so the menu tells the truth about what the product does and does
    /// not do yet.
    pub const fn is_supported(self) -> bool {
        matches!(self, ColorMode::Rgb | ColorMode::Grayscale | ColorMode::Lab)
    }
}

/// A Layer ▸ Layer Mask operation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum MaskOp {
    RevealAll,
    HideAll,
    RevealSelection,
    HideSelection,
    Delete,
    Apply,
    Toggle,
    ToggleLink,
}

impl MaskOp {
    pub const ALL: &'static [MaskOp] = &[
        MaskOp::RevealAll,
        MaskOp::HideAll,
        MaskOp::RevealSelection,
        MaskOp::HideSelection,
        MaskOp::Delete,
        MaskOp::Apply,
        MaskOp::Toggle,
        MaskOp::ToggleLink,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            MaskOp::RevealAll => "Reveal All",
            MaskOp::HideAll => "Hide All",
            MaskOp::RevealSelection => "Reveal Selection",
            MaskOp::HideSelection => "Hide Selection",
            MaskOp::Delete => "Delete Mask",
            MaskOp::Apply => "Apply Mask",
            MaskOp::Toggle => "Disable / Enable Mask",
            MaskOp::ToggleLink => "Link / Unlink Mask",
        }
    }

    /// `true` when the operation adds a mask (and therefore needs the layer not
    /// to have one already).
    const fn creates(self) -> bool {
        matches!(
            self,
            MaskOp::RevealAll | MaskOp::HideAll | MaskOp::RevealSelection | MaskOp::HideSelection
        )
    }

    /// `true` when the operation needs an active selection.
    const fn needs_selection(self) -> bool {
        matches!(self, MaskOp::RevealSelection | MaskOp::HideSelection)
    }
}

/// A Layer ▸ Rasterize target.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum RasterizeTarget {
    Layer,
    LayerStyle,
    Text,
    Shape,
    SmartObject,
    AllLayers,
}

impl RasterizeTarget {
    pub const ALL: &'static [RasterizeTarget] = &[
        RasterizeTarget::Layer,
        RasterizeTarget::LayerStyle,
        RasterizeTarget::Text,
        RasterizeTarget::Shape,
        RasterizeTarget::SmartObject,
        RasterizeTarget::AllLayers,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            RasterizeTarget::Layer => "Layer",
            RasterizeTarget::LayerStyle => "Layer Style",
            RasterizeTarget::Text => "Text",
            RasterizeTarget::Shape => "Shape",
            RasterizeTarget::SmartObject => "Smart Object",
            RasterizeTarget::AllLayers => "All Layers",
        }
    }
}

/// A Layer ▸ New Fill Layer kind.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum FillLayerKind {
    SolidColor,
    Gradient,
    Pattern,
}

impl FillLayerKind {
    pub const ALL: &'static [FillLayerKind] = &[
        FillLayerKind::SolidColor,
        FillLayerKind::Gradient,
        FillLayerKind::Pattern,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            FillLayerKind::SolidColor => "Solid Color…",
            FillLayerKind::Gradient => "Gradient…",
            FillLayerKind::Pattern => "Pattern…",
        }
    }
}

/// A View ▸ zoom command.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum ZoomCommand {
    In,
    Out,
    FitOnScreen,
    ActualPixels,
    PrintSize,
}

impl ZoomCommand {
    pub const ALL: &'static [ZoomCommand] = &[
        ZoomCommand::In,
        ZoomCommand::Out,
        ZoomCommand::FitOnScreen,
        ZoomCommand::ActualPixels,
        ZoomCommand::PrintSize,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            ZoomCommand::In => "Zoom In",
            ZoomCommand::Out => "Zoom Out",
            ZoomCommand::FitOnScreen => "Fit on Screen",
            ZoomCommand::ActualPixels => "100%",
            ZoomCommand::PrintSize => "Print Size",
        }
    }

    fn shortcut(self) -> Option<Shortcut> {
        Some(match self {
            ZoomCommand::In => Shortcut::ctrl_key(Key::Plus),
            ZoomCommand::Out => Shortcut::ctrl_key(Key::Minus),
            ZoomCommand::FitOnScreen => Shortcut::ctrl('0'),
            ZoomCommand::ActualPixels => Shortcut::ctrl('1'),
            ZoomCommand::PrintSize => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// The action vocabulary
// ---------------------------------------------------------------------------

/// Everything a menu item can ask for.
///
/// Payload-carrying variants keep the list finite: one `Filter(FilterId)`
/// rather than forty variants, and the same value is what the Adjustments panel
/// and the layers panel's buttons emit, so the application handles one
/// vocabulary and not three.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MenuAction {
    // ---- File ----------------------------------------------------------
    NewDocument,
    Open,
    OpenRecent(usize),
    CloseDocument,
    CloseAll,
    Save,
    SaveAs,
    Export(ExportFormat),
    ExportLayers,
    PlaceEmbedded,
    PlaceLinked,
    FileInfo,
    Print,
    Quit,

    // ---- Edit ----------------------------------------------------------
    Undo,
    Redo,
    Cut,
    Copy,
    CopyMerged,
    Paste,
    PasteInto,
    ClearPixels,
    FillDialog,
    StrokeDialog,
    FreeTransform,
    Transform(TransformOp),
    DefinePattern,
    DefineBrush,
    KeyboardShortcuts,
    Preferences,

    // ---- Image ---------------------------------------------------------
    SetColorMode(ColorMode),
    ApplyAdjustment(AdjustmentId),
    AutoTone,
    AutoContrast,
    AutoColor,
    ImageSize,
    CanvasSize,
    RotateCanvas(CanvasRotation),
    CropToSelection,
    Trim,
    RevealAll,
    DuplicateDocument,

    // ---- Layer ---------------------------------------------------------
    NewLayer,
    NewGroup,
    NewFillLayer(FillLayerKind),
    NewAdjustmentLayer(AdjustmentId),
    LayerViaCopy,
    LayerViaCut,
    DuplicateLayer,
    DeleteLayer,
    Mask(MaskOp),
    /// Open the editor for the *active adjustment layer's* parameters.
    ///
    /// Deliberately not [`MenuAction::ApplyAdjustment`], which bakes a new
    /// adjustment into a pixel layer and is gated on there being editable
    /// pixels. This one's subject is the adjustment layer itself, so it is
    /// enabled in exactly the state where `ApplyAdjustment` is not: when the
    /// active layer *is* an adjustment. The Properties panel's "Open editor…"
    /// emits this, and `whatever_the_properties_panel_offers_for_an_adjustment_
    /// is_enabled_there` pins the two together.
    EditAdjustmentLayer,
    CreateClippingMask,
    ReleaseClippingMask,
    BlendingOptions,
    LayerStyle(EffectSlot),
    ClearLayerStyle,
    ConvertToSmartObject,
    Rasterize(RasterizeTarget),
    GroupLayers,
    UngroupLayers,
    ArrangeLayer(Arrange),
    MergeDown,
    MergeVisible,
    FlattenImage,
    ToggleLayerVisibility,

    // ---- Select --------------------------------------------------------
    SelectAll,
    Deselect,
    Reselect,
    InverseSelection,
    SelectAllLayers,
    DeselectLayers,
    ColorRange,
    SelectSubject,
    Modify(ModifySelection),
    GrowSelection,
    SimilarSelection,
    TransformSelection,
    SaveSelection,
    LoadSelection,

    // ---- Filter --------------------------------------------------------
    LastFilter,
    FilterGallery,
    Filter(FilterId),

    // ---- View ----------------------------------------------------------
    Zoom(ZoomCommand),
    ToggleView(ViewFlag),
    /// Put the canvas back upright. The *view* rotation, not the image's —
    /// nothing about the document changes, which is why it is not an
    /// [`Intent::Document`].
    ResetViewRotation,
    /// Choose the unit the rulers and the readouts measure in.
    SetRulerUnit(crate::dialogs::units::Unit),

    // ---- Window --------------------------------------------------------
    ApplyLayout(LayoutId),
    TogglePanel(PanelId),
    SetTheme(design::Theme),

    // ---- Help ----------------------------------------------------------
    Help,
    ReleaseNotes,
    ReportIssue,
    About,
}

/// The outcome of asking whether an item can be used right now.
#[derive(Clone, PartialEq, Debug)]
pub enum Resolution {
    /// Usable; clicking emits this.
    Enabled(Intent),
    /// Not usable, and this sentence says why. Never empty — the tests check.
    Disabled(&'static str),
}

impl Resolution {
    pub fn is_enabled(&self) -> bool {
        matches!(self, Resolution::Enabled(_))
    }

    pub fn intent(&self) -> Option<&Intent> {
        match self {
            Resolution::Enabled(i) => Some(i),
            Resolution::Disabled(_) => None,
        }
    }

    /// The sentence shown on a disabled item's tooltip.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Resolution::Disabled(r) => Some(r),
            Resolution::Enabled(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// The context an item is resolved against
// ---------------------------------------------------------------------------

/// What kind of layer the active layer is.
///
/// A `Copy` summary rather than a borrow of [`LayerKind`], because the context
/// is a snapshot: it is built once per frame and then read by every item.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LayerClass {
    Raster,
    Group,
    Adjustment,
    Text,
    Shape,
    SmartObject,
    Generator,
}

impl LayerClass {
    pub fn of(kind: &LayerKind) -> Self {
        match kind {
            LayerKind::Raster(_) => LayerClass::Raster,
            LayerKind::Group(_) => LayerClass::Group,
            LayerKind::Adjustment(_) => LayerClass::Adjustment,
            LayerKind::Text(_) => LayerClass::Text,
            LayerKind::Shape(_) => LayerClass::Shape,
            LayerKind::SmartObject(_) => LayerClass::SmartObject,
            LayerKind::Generator(_) => LayerClass::Generator,
        }
    }

    /// Whether the layer owns pixels a paint tool or a filter can touch.
    pub const fn owns_pixels(self) -> bool {
        matches!(self, LayerClass::Raster | LayerClass::Generator)
    }

    pub const fn label(self) -> &'static str {
        match self {
            LayerClass::Raster => "Raster",
            LayerClass::Group => "Group",
            LayerClass::Adjustment => "Adjustment",
            LayerClass::Text => "Text",
            LayerClass::Shape => "Shape",
            LayerClass::SmartObject => "Smart Object",
            LayerClass::Generator => "Generator",
        }
    }
}

/// The facts about the active layer that menu enablement turns on.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ActiveLayer {
    pub id: LayerId,
    pub class: LayerClass,
    pub visible: bool,
    pub locked: LockState,
    pub has_mask: bool,
    pub mask_enabled: bool,
    pub has_effects: bool,
    pub is_clipping: bool,
    pub parent: Option<LayerId>,
    /// Position among its siblings, top-most first.
    pub index: usize,
    pub sibling_count: usize,
}

impl ActiveLayer {
    /// Read the facts out of a document.
    pub fn from_document(doc: &Document, id: LayerId) -> Option<Self> {
        let layer = doc.layers.get(id)?;
        let parent = doc.layers.parent_of(id);
        let siblings = doc.layers.siblings_of(id).map(|s| s.len()).unwrap_or(0);
        Some(Self {
            id,
            class: LayerClass::of(&layer.kind),
            visible: layer.visible,
            locked: layer.locked,
            has_mask: layer.mask.is_some(),
            mask_enabled: layer.mask.as_ref().is_some_and(|m| m.enabled),
            has_effects: !layer.effects.is_empty(),
            is_clipping: layer.is_clipping(),
            parent,
            index: doc.layers.index_in_parent(id).unwrap_or(0),
            sibling_count: siblings,
        })
    }

    /// `true` when there is a sibling directly beneath this layer — the one a
    /// merge-down or a clipping mask would use.
    pub const fn has_layer_below(&self) -> bool {
        self.index + 1 < self.sibling_count
    }
}

/// Everything menu enablement reads.
///
/// Built once per frame with [`MenuContext::from_document`] and then treated as
/// immutable, so every item in a frame agrees about the world.
#[derive(Clone, PartialEq, Debug)]
pub struct MenuContext {
    pub has_document: bool,
    pub is_dirty: bool,
    pub has_path: bool,
    /// The names of the recently opened files, most recent first, as the File
    /// menu should label them. The list's length *is* the recent-file count —
    /// there is deliberately no second counter to fall out of step with it.
    pub recent_files: Vec<String>,
    pub open_documents: usize,
    pub can_undo: bool,
    pub can_redo: bool,
    pub undo_label: Option<String>,
    pub redo_label: Option<String>,
    pub clipboard: ClipboardState,
    pub has_selection: bool,
    /// A selection was deselected and can be brought back.
    pub has_stored_selection: bool,
    /// Named selections previously saved into the document.
    pub saved_selections: usize,
    pub layer_count: usize,
    pub selected_layers: usize,
    pub active: Option<ActiveLayer>,
    pub last_filter: Option<FilterId>,
    pub color_mode: ColorMode,
    pub view: ViewFlags,
    /// The canvas view is turned off-axis, so Reset View Rotation has
    /// something to do. A *view* fact rather than a document one, read off the
    /// canvas camera by [`crate::Workspace::menu_context`].
    pub view_rotated: bool,
    /// The unit the rulers currently read in, so the Rulers submenu can tick
    /// the one in use.
    pub ruler_unit: crate::dialogs::units::Unit,
    pub dock: DockState,
    pub theme: design::Theme,
}

impl Default for MenuContext {
    /// The state before a document is open: almost everything is disabled, and
    /// each disabled item still says why.
    fn default() -> Self {
        Self {
            has_document: false,
            is_dirty: false,
            has_path: false,
            recent_files: Vec::new(),
            open_documents: 0,
            can_undo: false,
            can_redo: false,
            undo_label: None,
            redo_label: None,
            clipboard: ClipboardState::EMPTY,
            has_selection: false,
            has_stored_selection: false,
            saved_selections: 0,
            layer_count: 0,
            selected_layers: 0,
            active: None,
            last_filter: None,
            color_mode: ColorMode::Rgb,
            view: ViewFlags::defaults(),
            view_rotated: false,
            ruler_unit: crate::dialogs::units::Unit::Pixels,
            dock: DockState::default(),
            theme: design::Theme::default(),
        }
    }
}

impl MenuContext {
    /// Read the document-derived half of the context. The rest — clipboard,
    /// recents, theme, dock — belongs to the application and is filled in by
    /// the caller.
    pub fn from_document(doc: &Document, history: &History) -> Self {
        let active = doc
            .active_layer()
            .and_then(|id| ActiveLayer::from_document(doc, id));
        Self {
            has_document: true,
            is_dirty: doc.is_dirty(),
            has_path: doc.path().is_some(),
            open_documents: 1,
            can_undo: history.can_undo(),
            can_redo: history.can_redo(),
            undo_label: history.undo_label().map(str::to_owned),
            redo_label: history.redo_label().map(str::to_owned),
            // Deliberately not `!selection.is_empty()`. `Selection::None`
            // answers `false` to `is_empty` — with nothing selected every pixel
            // is selected — so the naive spelling would enable Deselect on a
            // document that has never had a selection. See the documentation on
            // `Selection::is_empty`.
            has_selection: doc.selection.bounds().is_some(),
            layer_count: doc.layers.len(),
            selected_layers: usize::from(active.is_some()),
            active,
            ..Self::default()
        }
    }

    fn need_document(&self) -> Option<&'static str> {
        (!self.has_document).then_some("No document is open")
    }

    fn need_layer(&self) -> Result<ActiveLayer, &'static str> {
        if !self.has_document {
            return Err("No document is open");
        }
        self.active.ok_or("Select a layer first")
    }

    fn need_pixel_layer(&self) -> Result<ActiveLayer, &'static str> {
        let layer = self.need_layer()?;
        if layer.class.owns_pixels() {
            Ok(layer)
        } else {
            Err("This works on a pixel layer; the active layer is not one")
        }
    }

    /// The active layer, when it is an adjustment layer.
    ///
    /// The exact complement of [`MenuContext::need_pixel_layer`] for the
    /// adjustment case, and the reason [`MenuAction::EditAdjustmentLayer`]
    /// exists: the Properties panel offers "Open editor…" precisely when the
    /// active layer is an adjustment, which is when `need_editable_pixels`
    /// refuses.
    fn need_adjustment_layer(&self) -> Result<ActiveLayer, &'static str> {
        let layer = self.need_layer()?;
        if layer.class == LayerClass::Adjustment {
            Ok(layer)
        } else {
            Err("The active layer is not an adjustment layer")
        }
    }

    fn need_editable_pixels(&self) -> Result<ActiveLayer, &'static str> {
        let layer = self.need_pixel_layer()?;
        if layer.locked.blocks_pixel_edit() {
            Err("The layer's pixels are locked")
        } else {
            Ok(layer)
        }
    }

    fn need_selection(&self) -> Option<&'static str> {
        self.need_document()
            .or((!self.has_selection).then_some("There is no selection"))
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Shorthand for an item that resolves to its own action.
fn act(action: MenuAction) -> Resolution {
    Resolution::Enabled(Intent::Action(action))
}

fn cmd(command: Command) -> Resolution {
    Resolution::Enabled(Intent::Document(command))
}

fn gate(reason: Option<&'static str>, resolution: Resolution) -> Resolution {
    match reason {
        Some(r) => Resolution::Disabled(r),
        None => resolution,
    }
}

impl MenuAction {
    /// Every action the application can ask [`MenuAction::resolve`] about,
    /// menu-reachable or not.
    ///
    /// The "no dead item" gate used to walk [`menu_bar`] instead, which meant
    /// it never saw the actions a *panel* emits and no menu lists —
    /// [`MenuAction::ToggleLayerVisibility`] is one. Those are exactly the
    /// items nobody is looking at, so they are the ones that quietly stop
    /// resolving. Two tests hold this list honest:
    /// `every_action_a_menu_offers_is_in_the_full_list` proves it is a
    /// superset of the menus, and `the_full_action_list_repeats_nothing`
    /// proves it does not pad itself.
    pub fn all() -> Vec<MenuAction> {
        let mut out = vec![
            // ---- File ----
            MenuAction::NewDocument,
            MenuAction::Open,
        ];
        out.extend((0..MAX_RECENT_FILES).map(MenuAction::OpenRecent));
        out.extend([MenuAction::CloseDocument, MenuAction::CloseAll]);
        out.extend([MenuAction::Save, MenuAction::SaveAs]);
        out.extend(ExportFormat::ALL.iter().copied().map(MenuAction::Export));
        out.extend([
            MenuAction::ExportLayers,
            MenuAction::PlaceEmbedded,
            MenuAction::PlaceLinked,
            MenuAction::FileInfo,
            MenuAction::Print,
            MenuAction::Quit,
            // ---- Edit ----
            MenuAction::Undo,
            MenuAction::Redo,
            MenuAction::Cut,
            MenuAction::Copy,
            MenuAction::CopyMerged,
            MenuAction::Paste,
            MenuAction::PasteInto,
            MenuAction::ClearPixels,
            MenuAction::FillDialog,
            MenuAction::StrokeDialog,
            MenuAction::FreeTransform,
        ]);
        out.extend(TransformOp::ALL.iter().copied().map(MenuAction::Transform));
        out.extend([
            MenuAction::DefinePattern,
            MenuAction::DefineBrush,
            MenuAction::KeyboardShortcuts,
            MenuAction::Preferences,
        ]);
        // ---- Image ----
        out.extend(ColorMode::ALL.iter().copied().map(MenuAction::SetColorMode));
        out.extend(
            AdjustmentId::ALL
                .iter()
                .copied()
                .map(MenuAction::ApplyAdjustment),
        );
        out.extend([
            MenuAction::AutoTone,
            MenuAction::AutoContrast,
            MenuAction::AutoColor,
            MenuAction::ImageSize,
            MenuAction::CanvasSize,
        ]);
        out.extend(
            CanvasRotation::ALL
                .iter()
                .copied()
                .map(MenuAction::RotateCanvas),
        );
        out.extend([
            MenuAction::CropToSelection,
            MenuAction::Trim,
            MenuAction::RevealAll,
            MenuAction::DuplicateDocument,
            // ---- Layer ----
            MenuAction::NewLayer,
            MenuAction::NewGroup,
        ]);
        out.extend(
            FillLayerKind::ALL
                .iter()
                .copied()
                .map(MenuAction::NewFillLayer),
        );
        out.extend(
            AdjustmentId::ALL
                .iter()
                .copied()
                .map(MenuAction::NewAdjustmentLayer),
        );
        out.extend([
            MenuAction::LayerViaCopy,
            MenuAction::LayerViaCut,
            MenuAction::DuplicateLayer,
            MenuAction::DeleteLayer,
        ]);
        out.extend(MaskOp::ALL.iter().copied().map(MenuAction::Mask));
        out.extend([
            MenuAction::EditAdjustmentLayer,
            MenuAction::CreateClippingMask,
            MenuAction::ReleaseClippingMask,
            MenuAction::BlendingOptions,
        ]);
        out.extend(EffectSlot::ALL.iter().copied().map(MenuAction::LayerStyle));
        out.push(MenuAction::ClearLayerStyle);
        out.push(MenuAction::ConvertToSmartObject);
        out.extend(
            RasterizeTarget::ALL
                .iter()
                .copied()
                .map(MenuAction::Rasterize),
        );
        out.extend([MenuAction::GroupLayers, MenuAction::UngroupLayers]);
        out.extend(Arrange::ALL.iter().copied().map(MenuAction::ArrangeLayer));
        out.extend([
            MenuAction::MergeDown,
            MenuAction::MergeVisible,
            MenuAction::FlattenImage,
            // Emitted by the Layers panel's eye, and by no menu at all.
            MenuAction::ToggleLayerVisibility,
            // ---- Select ----
            MenuAction::SelectAll,
            MenuAction::Deselect,
            MenuAction::Reselect,
            MenuAction::InverseSelection,
            MenuAction::SelectAllLayers,
            MenuAction::DeselectLayers,
            MenuAction::ColorRange,
            MenuAction::SelectSubject,
        ]);
        out.extend(ModifySelection::ALL.iter().copied().map(MenuAction::Modify));
        out.extend([
            MenuAction::GrowSelection,
            MenuAction::SimilarSelection,
            MenuAction::TransformSelection,
            MenuAction::SaveSelection,
            MenuAction::LoadSelection,
            // ---- Filter ----
            MenuAction::LastFilter,
            MenuAction::FilterGallery,
        ]);
        out.extend(FilterId::ALL.iter().copied().map(MenuAction::Filter));
        // ---- View ----
        out.extend(ZoomCommand::ALL.iter().copied().map(MenuAction::Zoom));
        out.push(MenuAction::ResetViewRotation);
        out.extend(
            crate::dialogs::units::Unit::ALL
                .iter()
                .copied()
                .map(MenuAction::SetRulerUnit),
        );
        out.extend(ViewFlag::ALL.iter().copied().map(MenuAction::ToggleView));
        // ---- Window ----
        out.extend(LayoutId::ALL.iter().copied().map(MenuAction::ApplyLayout));
        out.extend(PanelId::ALL.iter().copied().map(MenuAction::TogglePanel));
        out.extend(design::Theme::ALL.iter().copied().map(MenuAction::SetTheme));
        // ---- Help ----
        out.extend([
            MenuAction::Help,
            MenuAction::ReleaseNotes,
            MenuAction::ReportIssue,
            MenuAction::About,
        ]);
        out
    }

    /// The label this action wears in a menu.
    pub fn label(self) -> String {
        match self {
            MenuAction::NewDocument => "New…".into(),
            MenuAction::Open => "Open…".into(),
            MenuAction::OpenRecent(i) => format!("Recent {}", i + 1),
            MenuAction::CloseDocument => "Close".into(),
            MenuAction::CloseAll => "Close All".into(),
            MenuAction::Save => "Save".into(),
            MenuAction::SaveAs => "Save As…".into(),
            MenuAction::Export(f) => format!("{}…", f.extension().to_uppercase()),
            MenuAction::ExportLayers => "Export Layers…".into(),
            MenuAction::PlaceEmbedded => "Place Embedded…".into(),
            MenuAction::PlaceLinked => "Place Linked…".into(),
            MenuAction::FileInfo => "File Info…".into(),
            MenuAction::Print => "Print…".into(),
            MenuAction::Quit => "Quit".into(),

            MenuAction::Undo => "Undo".into(),
            MenuAction::Redo => "Redo".into(),
            MenuAction::Cut => "Cut".into(),
            MenuAction::Copy => "Copy".into(),
            MenuAction::CopyMerged => "Copy Merged".into(),
            MenuAction::Paste => "Paste".into(),
            MenuAction::PasteInto => "Paste Into".into(),
            MenuAction::ClearPixels => "Clear".into(),
            MenuAction::FillDialog => "Fill…".into(),
            MenuAction::StrokeDialog => "Stroke…".into(),
            MenuAction::FreeTransform => "Free Transform".into(),
            MenuAction::Transform(t) => t.label().into(),
            MenuAction::DefinePattern => "Define Pattern…".into(),
            MenuAction::DefineBrush => "Define Brush Preset…".into(),
            MenuAction::KeyboardShortcuts => "Keyboard Shortcuts…".into(),
            MenuAction::Preferences => "Preferences…".into(),

            MenuAction::SetColorMode(m) => m.label().into(),
            MenuAction::ApplyAdjustment(a) => format!("{}…", a.label()),
            MenuAction::AutoTone => "Auto Tone".into(),
            MenuAction::AutoContrast => "Auto Contrast".into(),
            MenuAction::AutoColor => "Auto Color".into(),
            MenuAction::ImageSize => "Image Size…".into(),
            MenuAction::CanvasSize => "Canvas Size…".into(),
            MenuAction::RotateCanvas(r) => r.label().into(),
            MenuAction::CropToSelection => "Crop".into(),
            MenuAction::Trim => "Trim…".into(),
            MenuAction::RevealAll => "Reveal All".into(),
            MenuAction::DuplicateDocument => "Duplicate…".into(),

            MenuAction::NewLayer => "Layer".into(),
            MenuAction::NewGroup => "Group".into(),
            MenuAction::NewFillLayer(k) => k.label().into(),
            MenuAction::NewAdjustmentLayer(a) => a.label().into(),
            MenuAction::LayerViaCopy => "Layer via Copy".into(),
            MenuAction::LayerViaCut => "Layer via Cut".into(),
            MenuAction::DuplicateLayer => "Duplicate Layer…".into(),
            MenuAction::DeleteLayer => "Delete Layer".into(),
            MenuAction::Mask(m) => m.label().into(),
            MenuAction::EditAdjustmentLayer => "Edit Adjustment…".into(),
            MenuAction::CreateClippingMask => "Create Clipping Mask".into(),
            MenuAction::ReleaseClippingMask => "Release Clipping Mask".into(),
            MenuAction::BlendingOptions => "Blending Options…".into(),
            MenuAction::LayerStyle(s) => s.label().into(),
            MenuAction::ClearLayerStyle => "Clear Layer Style".into(),
            MenuAction::ConvertToSmartObject => "Convert to Smart Object".into(),
            MenuAction::Rasterize(t) => t.label().into(),
            MenuAction::GroupLayers => "Group Layers".into(),
            MenuAction::UngroupLayers => "Ungroup Layers".into(),
            MenuAction::ArrangeLayer(a) => a.label().into(),
            MenuAction::MergeDown => "Merge Down".into(),
            MenuAction::MergeVisible => "Merge Visible".into(),
            MenuAction::FlattenImage => "Flatten Image".into(),
            MenuAction::ToggleLayerVisibility => "Show / Hide Layer".into(),

            MenuAction::SelectAll => "All".into(),
            MenuAction::Deselect => "Deselect".into(),
            MenuAction::Reselect => "Reselect".into(),
            MenuAction::InverseSelection => "Inverse".into(),
            MenuAction::SelectAllLayers => "All Layers".into(),
            MenuAction::DeselectLayers => "Deselect Layers".into(),
            MenuAction::ColorRange => "Color Range…".into(),
            MenuAction::SelectSubject => "Subject".into(),
            MenuAction::Modify(m) => m.label().into(),
            MenuAction::GrowSelection => "Grow".into(),
            MenuAction::SimilarSelection => "Similar".into(),
            MenuAction::TransformSelection => "Transform Selection".into(),
            MenuAction::SaveSelection => "Save Selection…".into(),
            MenuAction::LoadSelection => "Load Selection…".into(),

            MenuAction::LastFilter => "Last Filter".into(),
            MenuAction::FilterGallery => "Filter Gallery…".into(),
            MenuAction::Filter(f) => f.label().into(),

            MenuAction::Zoom(z) => z.label().into(),
            MenuAction::ToggleView(f) => f.label().into(),
            MenuAction::ResetViewRotation => "Reset View Rotation".into(),
            MenuAction::SetRulerUnit(unit) => unit.label().into(),

            MenuAction::ApplyLayout(l) => l.title().into(),
            MenuAction::TogglePanel(p) => p.title().into(),
            MenuAction::SetTheme(t) => t.name().into(),

            MenuAction::Help => "Raster Studio Help".into(),
            MenuAction::ReleaseNotes => "Release Notes".into(),
            MenuAction::ReportIssue => "Report an Issue".into(),
            MenuAction::About => "About Raster Studio".into(),
        }
    }

    /// The label this action wears *in a given frame*.
    ///
    /// Three items say more when they can: Undo and Redo name the step they
    /// would move, and an Open Recent slot names its file rather than its
    /// number. Everything else falls through to [`MenuAction::label`], which is
    /// the context-free spelling.
    pub fn label_in(self, ctx: &MenuContext) -> String {
        match self {
            MenuAction::OpenRecent(i) => match ctx.recent_files.get(i) {
                Some(name) if !name.is_empty() => name.clone(),
                _ => self.label(),
            },
            MenuAction::Undo => match ctx.undo_label.as_deref() {
                Some(step) => format!("Undo {step}"),
                None => self.label(),
            },
            MenuAction::Redo => match ctx.redo_label.as_deref() {
                Some(step) => format!("Redo {step}"),
                None => self.label(),
            },
            _ => self.label(),
        }
    }

    /// The chord that performs this action without opening the menu.
    pub fn shortcut(self) -> Option<Shortcut> {
        Some(match self {
            MenuAction::NewDocument => Shortcut::ctrl('n'),
            MenuAction::Open => Shortcut::ctrl('o'),
            MenuAction::CloseDocument => Shortcut::ctrl('w'),
            MenuAction::CloseAll => Shortcut::ctrl_alt('w'),
            MenuAction::Save => Shortcut::ctrl('s'),
            MenuAction::SaveAs => Shortcut::ctrl_shift('s'),
            MenuAction::Quit => Shortcut::ctrl('q'),

            MenuAction::Undo => Shortcut::ctrl('z'),
            MenuAction::Redo => Shortcut::ctrl_shift('z'),
            MenuAction::Cut => Shortcut::ctrl('x'),
            MenuAction::Copy => Shortcut::ctrl('c'),
            MenuAction::CopyMerged => Shortcut::ctrl_shift('c'),
            MenuAction::Paste => Shortcut::ctrl('v'),
            MenuAction::PasteInto => Shortcut::ctrl_shift('v'),
            MenuAction::ClearPixels => Shortcut::bare(Key::Delete),
            MenuAction::FillDialog => Shortcut::shift('f'),
            MenuAction::FreeTransform => Shortcut::ctrl('t'),
            MenuAction::KeyboardShortcuts => Shortcut::ctrl_alt_shift('k'),
            MenuAction::Preferences => Shortcut::ctrl('k'),

            MenuAction::AutoTone => Shortcut::ctrl_shift('l'),
            MenuAction::AutoContrast => Shortcut::ctrl_alt_shift('l'),
            MenuAction::AutoColor => Shortcut::ctrl_shift('b'),
            MenuAction::ImageSize => Shortcut::ctrl_alt('i'),
            MenuAction::CanvasSize => Shortcut::ctrl_alt('c'),

            MenuAction::NewLayer => Shortcut::ctrl_shift('n'),
            MenuAction::LayerViaCopy => Shortcut::ctrl('j'),
            MenuAction::LayerViaCut => Shortcut::ctrl_shift('j'),
            MenuAction::CreateClippingMask => Shortcut::ctrl_alt('g'),
            MenuAction::GroupLayers => Shortcut::ctrl('g'),
            MenuAction::UngroupLayers => Shortcut::ctrl_shift('g'),
            MenuAction::ArrangeLayer(a) => a.shortcut(),
            MenuAction::MergeDown => Shortcut::ctrl('e'),
            MenuAction::MergeVisible => Shortcut::ctrl_shift('e'),

            MenuAction::SelectAll => Shortcut::ctrl('a'),
            MenuAction::Deselect => Shortcut::ctrl('d'),
            MenuAction::Reselect => Shortcut::ctrl_shift('d'),
            MenuAction::InverseSelection => Shortcut::ctrl_shift('i'),
            MenuAction::SelectAllLayers => Shortcut::ctrl_alt('a'),
            MenuAction::Modify(ModifySelection::Feather) => Shortcut::shift('6'),

            MenuAction::LastFilter => Shortcut::ctrl('f'),

            MenuAction::Zoom(z) => return z.shortcut(),
            MenuAction::ToggleView(ViewFlag::Rulers) => Shortcut::ctrl('r'),
            MenuAction::ToggleView(ViewFlag::Grid) => Shortcut::ctrl_key(Key::Quote),
            MenuAction::ToggleView(ViewFlag::Guides) => Shortcut::ctrl_key(Key::Semicolon),
            MenuAction::ToggleView(ViewFlag::Snap) => Shortcut::ctrl_shift_key(Key::Semicolon),

            MenuAction::TogglePanel(PanelId::Brushes) => Shortcut::bare(Key::F(5)),
            MenuAction::TogglePanel(PanelId::Color) => Shortcut::bare(Key::F(6)),
            MenuAction::TogglePanel(PanelId::Layers) => Shortcut::bare(Key::F(7)),
            MenuAction::TogglePanel(PanelId::Info) => Shortcut::bare(Key::F(8)),

            MenuAction::Help => Shortcut::bare(Key::F(1)),

            _ => return None,
        })
    }

    /// Whether this item shows a checkmark, and whether it is currently on.
    ///
    /// `None` for an item that is not a toggle.
    pub fn checked(self, ctx: &MenuContext) -> Option<bool> {
        Some(match self {
            MenuAction::ToggleView(flag) => ctx.view.get(flag),
            MenuAction::TogglePanel(panel) => ctx.dock.is_open(panel),
            MenuAction::SetTheme(theme) => ctx.theme == theme,
            MenuAction::ApplyLayout(layout) => ctx.dock.layout() == Some(layout),
            MenuAction::SetColorMode(mode) => ctx.color_mode == mode,
            MenuAction::SetRulerUnit(unit) => ctx.ruler_unit == unit,
            MenuAction::ToggleLayerVisibility => ctx.active.map(|l| l.visible)?,
            _ => return None,
        })
    }

    /// Whether this item is usable right now, and what it does if it is.
    ///
    /// The whole contract of this module lives in this one function: it returns
    /// [`Resolution`], which has no "neither" case.
    pub fn resolve(self, ctx: &MenuContext) -> Resolution {
        match self {
            // ---- File ------------------------------------------------------
            MenuAction::NewDocument | MenuAction::Open | MenuAction::Quit => act(self),
            MenuAction::OpenRecent(i) => gate(
                (i >= ctx.recent_files.len()).then_some("This slot has no recent file"),
                act(self),
            ),
            MenuAction::CloseDocument
            | MenuAction::Save
            | MenuAction::SaveAs
            | MenuAction::ExportLayers
            | MenuAction::PlaceEmbedded
            | MenuAction::PlaceLinked
            | MenuAction::FileInfo
            | MenuAction::Print
            | MenuAction::DuplicateDocument => gate(ctx.need_document(), act(self)),
            MenuAction::CloseAll => gate(
                (ctx.open_documents == 0).then_some("No document is open"),
                act(self),
            ),
            MenuAction::Export(_) => gate(ctx.need_document(), act(self)),

            // ---- Edit ------------------------------------------------------
            MenuAction::Undo => gate((!ctx.can_undo).then_some("Nothing to undo"), act(self)),
            MenuAction::Redo => gate((!ctx.can_redo).then_some("Nothing to redo"), act(self)),
            MenuAction::Copy | MenuAction::CopyMerged => match ctx.need_pixel_layer() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::Cut | MenuAction::ClearPixels => match ctx.need_editable_pixels() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::Paste => gate(
                ctx.need_document()
                    .or(ctx.clipboard.is_empty().then_some("The clipboard is empty")),
                act(self),
            ),
            MenuAction::PasteInto => gate(
                ctx.need_document()
                    .or(ctx.clipboard.is_empty().then_some("The clipboard is empty"))
                    .or((!ctx.has_selection).then_some("Paste Into needs a selection")),
                act(self),
            ),
            MenuAction::FillDialog | MenuAction::StrokeDialog => match ctx.need_editable_pixels() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::FreeTransform | MenuAction::Transform(_) => match ctx.need_layer() {
                Ok(l) if l.locked.blocks_transform() => {
                    Resolution::Disabled("The layer's position is locked")
                }
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::DefinePattern => gate(
                ctx.need_selection()
                    .map(|_| "Define Pattern needs a rectangular selection"),
                act(self),
            ),
            MenuAction::DefineBrush => gate(ctx.need_selection(), act(self)),
            MenuAction::KeyboardShortcuts | MenuAction::Preferences => act(self),

            // ---- Image -----------------------------------------------------
            MenuAction::SetColorMode(mode) => gate(
                ctx.need_document().or((!mode.is_supported())
                    .then_some("This build cannot convert to that colour mode yet")),
                act(self),
            ),
            MenuAction::ApplyAdjustment(_)
            | MenuAction::AutoTone
            | MenuAction::AutoContrast
            | MenuAction::AutoColor => match ctx.need_editable_pixels() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::ImageSize
            | MenuAction::CanvasSize
            | MenuAction::RotateCanvas(_)
            | MenuAction::Trim
            | MenuAction::RevealAll => gate(ctx.need_document(), act(self)),
            MenuAction::CropToSelection => gate(ctx.need_selection(), act(self)),

            // ---- Layer -----------------------------------------------------
            MenuAction::NewLayer => gate(
                ctx.need_document(),
                cmd(Command::create_layer(Layer::raster(next_layer_name(
                    ctx.layer_count,
                )))),
            ),
            MenuAction::NewGroup => gate(
                ctx.need_document(),
                cmd(Command::create_layer(Layer::group("Group"))),
            ),
            MenuAction::NewAdjustmentLayer(id) => {
                gate(ctx.need_document(), cmd(id.create_command()))
            }
            MenuAction::NewFillLayer(_) => gate(ctx.need_document(), act(self)),
            MenuAction::LayerViaCopy => match ctx.need_pixel_layer() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::LayerViaCut => match ctx.need_editable_pixels() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::DuplicateLayer => match ctx.need_layer() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::DeleteLayer => match ctx.need_layer() {
                Ok(l) if l.locked.all => Resolution::Disabled("The layer is locked"),
                Ok(l) => cmd(Command::DeleteLayer { layer_id: l.id }),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::ToggleLayerVisibility => match ctx.need_layer() {
                Ok(l) => cmd(Command::SetLayerProperties {
                    layer_id: l.id,
                    patch: LayerPatch {
                        visible: Some(!l.visible),
                        ..Default::default()
                    },
                }),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::Mask(op) => resolve_mask(op, ctx),
            MenuAction::EditAdjustmentLayer => match ctx.need_adjustment_layer() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::CreateClippingMask => match ctx.need_layer() {
                Ok(l) if l.is_clipping => Resolution::Disabled("The layer already clips"),
                Ok(l) if !l.has_layer_below() => {
                    Resolution::Disabled("There is no layer below to clip to")
                }
                Ok(l) => cmd(Command::SetLayerProperties {
                    layer_id: l.id,
                    patch: LayerPatch {
                        clipping: Some(ClippingMode::ClipToBelow),
                        ..Default::default()
                    },
                }),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::ReleaseClippingMask => match ctx.need_layer() {
                Ok(l) if !l.is_clipping => Resolution::Disabled("The layer does not clip"),
                Ok(l) => cmd(Command::SetLayerProperties {
                    layer_id: l.id,
                    patch: LayerPatch {
                        clipping: Some(ClippingMode::None),
                        ..Default::default()
                    },
                }),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::BlendingOptions | MenuAction::LayerStyle(_) => match ctx.need_layer() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::ClearLayerStyle => match ctx.need_layer() {
                Ok(l) if !l.has_effects => Resolution::Disabled("The layer has no style to clear"),
                Ok(l) => cmd(Command::SetLayerProperties {
                    layer_id: l.id,
                    patch: LayerPatch {
                        effects: Some(Box::default()),
                        ..Default::default()
                    },
                }),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::ConvertToSmartObject => match ctx.need_layer() {
                Ok(l) if l.class == LayerClass::SmartObject => {
                    Resolution::Disabled("The layer is already a smart object")
                }
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::Rasterize(target) => resolve_rasterize(target, ctx),
            MenuAction::GroupLayers => gate(
                ctx.need_document()
                    .or((ctx.selected_layers == 0).then_some("Select a layer first")),
                act(self),
            ),
            MenuAction::UngroupLayers => match ctx.need_layer() {
                Ok(l) if l.class != LayerClass::Group => {
                    Resolution::Disabled("Only a group can be ungrouped")
                }
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::ArrangeLayer(a) => match ctx.need_layer() {
                Ok(l) if a.is_noop(l.index, l.sibling_count) => {
                    Resolution::Disabled(a.blocked_reason())
                }
                Ok(l) => cmd(Command::MoveLayer {
                    layer_id: l.id,
                    parent: l.parent,
                    index: a.target_index(l.index, l.sibling_count),
                }),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::MergeDown => match ctx.need_layer() {
                Ok(l) if !l.has_layer_below() => {
                    Resolution::Disabled("There is no layer below to merge into")
                }
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::MergeVisible | MenuAction::FlattenImage => gate(
                ctx.need_document()
                    .or((ctx.layer_count < 2).then_some("There is only one layer")),
                act(self),
            ),

            // ---- Select ----------------------------------------------------
            MenuAction::SelectAll | MenuAction::ColorRange => gate(ctx.need_document(), act(self)),
            MenuAction::Deselect
            | MenuAction::InverseSelection
            | MenuAction::Modify(_)
            | MenuAction::TransformSelection
            | MenuAction::SaveSelection => gate(ctx.need_selection(), act(self)),
            MenuAction::Reselect => gate(
                ctx.need_document()
                    .or((!ctx.has_stored_selection).then_some("There is no selection to restore")),
                act(self),
            ),
            MenuAction::LoadSelection => gate(
                ctx.need_document()
                    .or((ctx.saved_selections == 0).then_some("No selection has been saved")),
                act(self),
            ),
            MenuAction::SelectAllLayers => gate(
                ctx.need_document()
                    .or((ctx.layer_count == 0).then_some("The document has no layers")),
                act(self),
            ),
            MenuAction::DeselectLayers => gate(
                ctx.need_document()
                    .or((ctx.selected_layers == 0).then_some("No layer is selected")),
                Resolution::Enabled(Intent::SelectLayers {
                    layers: Vec::new(),
                    active: None,
                }),
            ),
            MenuAction::SelectSubject
            | MenuAction::GrowSelection
            | MenuAction::SimilarSelection => match ctx.need_pixel_layer() {
                Ok(_) if self != MenuAction::SelectSubject && !ctx.has_selection => {
                    Resolution::Disabled("There is no selection")
                }
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },

            // ---- Filter ----------------------------------------------------
            MenuAction::LastFilter => match ctx.need_editable_pixels() {
                Ok(_) if ctx.last_filter.is_none() => {
                    Resolution::Disabled("No filter has been applied yet")
                }
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::FilterGallery | MenuAction::Filter(_) => match ctx.need_editable_pixels() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },

            // ---- View ------------------------------------------------------
            MenuAction::Zoom(_) => gate(ctx.need_document(), act(self)),
            MenuAction::SetRulerUnit(unit) => gate(
                (ctx.ruler_unit == unit).then_some("The rulers already read in this unit"),
                Resolution::Enabled(Intent::SetRulerUnit(unit)),
            ),
            MenuAction::ResetViewRotation => gate(
                ctx.need_document()
                    .or((!ctx.view_rotated).then_some("The view is already upright")),
                act(self),
            ),
            MenuAction::ToggleView(flag) => gate(
                ctx.need_document(),
                Resolution::Enabled(Intent::SetViewFlag {
                    flag,
                    on: !ctx.view.get(flag),
                }),
            ),

            // ---- Window ----------------------------------------------------
            MenuAction::ApplyLayout(layout) => Resolution::Enabled(Intent::ApplyLayout(layout)),
            MenuAction::TogglePanel(panel) => Resolution::Enabled(Intent::SetPanelOpen {
                panel,
                open: !ctx.dock.is_open(panel),
            }),
            MenuAction::SetTheme(theme) => gate(
                (ctx.theme == theme).then_some("This appearance is already in use"),
                Resolution::Enabled(Intent::SetTheme(theme)),
            ),

            // ---- Help ------------------------------------------------------
            MenuAction::Help
            | MenuAction::ReleaseNotes
            | MenuAction::ReportIssue
            | MenuAction::About => act(self),
        }
    }
}

fn resolve_mask(op: MaskOp, ctx: &MenuContext) -> Resolution {
    let layer = match ctx.need_layer() {
        Ok(l) => l,
        Err(r) => return Resolution::Disabled(r),
    };
    if op.creates() && layer.has_mask {
        return Resolution::Disabled("The layer already has a mask");
    }
    if !op.creates() && !layer.has_mask {
        return Resolution::Disabled("The layer has no mask");
    }
    if op.needs_selection() && !ctx.has_selection {
        return Resolution::Disabled("There is no selection");
    }
    match op {
        // Adding a mask is a property patch, so it is a command outright. The
        // *contents* of a Reveal/Hide Selection mask are pixels, which the
        // application rasterises — but the attach is the same command.
        MaskOp::RevealAll | MaskOp::HideAll | MaskOp::RevealSelection | MaskOp::HideSelection => {
            cmd(Command::SetLayerProperties {
                layer_id: layer.id,
                patch: LayerPatch {
                    mask: Patch::Set(LayerMask::new(MaskId::new())),
                    ..Default::default()
                },
            })
        }
        MaskOp::Delete => cmd(Command::SetLayerProperties {
            layer_id: layer.id,
            patch: LayerPatch {
                mask: Patch::Clear,
                ..Default::default()
            },
        }),
        // Applying a mask bakes coverage into pixels; that is not a property
        // patch, so it goes back to the application.
        MaskOp::Apply => act(MenuAction::Mask(op)),
        MaskOp::Toggle | MaskOp::ToggleLink => act(MenuAction::Mask(op)),
    }
}

fn resolve_rasterize(target: RasterizeTarget, ctx: &MenuContext) -> Resolution {
    if let Some(reason) = ctx.need_document() {
        return Resolution::Disabled(reason);
    }
    if target == RasterizeTarget::AllLayers {
        return gate(
            (ctx.layer_count == 0).then_some("The document has no layers"),
            act(MenuAction::Rasterize(target)),
        );
    }
    let layer = match ctx.need_layer() {
        Ok(l) => l,
        Err(r) => return Resolution::Disabled(r),
    };
    let ok = match target {
        RasterizeTarget::Layer => !layer.class.owns_pixels(),
        RasterizeTarget::LayerStyle => layer.has_effects,
        RasterizeTarget::Text => layer.class == LayerClass::Text,
        RasterizeTarget::Shape => layer.class == LayerClass::Shape,
        RasterizeTarget::SmartObject => layer.class == LayerClass::SmartObject,
        RasterizeTarget::AllLayers => true,
    };
    if ok {
        act(MenuAction::Rasterize(target))
    } else {
        Resolution::Disabled(match target {
            RasterizeTarget::Layer => "The layer is already pixels",
            RasterizeTarget::LayerStyle => "The layer has no style to rasterize",
            RasterizeTarget::Text => "The active layer is not a text layer",
            RasterizeTarget::Shape => "The active layer is not a shape layer",
            RasterizeTarget::SmartObject => "The active layer is not a smart object",
            RasterizeTarget::AllLayers => unreachable!("handled above"),
        })
    }
}

/// The name a newly created layer gets.
fn next_layer_name(existing: usize) -> String {
    format!("Layer {}", existing + 1)
}

// ---------------------------------------------------------------------------
// The menu structure
// ---------------------------------------------------------------------------

/// One row of a menu.
#[derive(Clone, Debug)]
pub enum Entry {
    Item(MenuAction),
    /// A hairline between groups of items.
    Separator,
    Submenu {
        label: &'static str,
        entries: Vec<Entry>,
    },
}

impl Entry {
    fn submenu(label: &'static str, entries: Vec<Entry>) -> Self {
        Entry::Submenu { label, entries }
    }

    /// Every action reachable from this entry, submenus included.
    pub fn actions(&self) -> Vec<MenuAction> {
        match self {
            Entry::Item(a) => vec![*a],
            Entry::Separator => Vec::new(),
            Entry::Submenu { entries, .. } => entries.iter().flat_map(Entry::actions).collect(),
        }
    }
}

/// One top-level menu.
#[derive(Clone, Debug)]
pub struct Menu {
    pub title: &'static str,
    pub entries: Vec<Entry>,
}

impl Menu {
    /// Every action in this menu, submenus included.
    pub fn actions(&self) -> Vec<MenuAction> {
        self.entries.iter().flat_map(Entry::actions).collect()
    }
}

fn item(action: MenuAction) -> Entry {
    Entry::Item(action)
}

fn items<T: Copy>(source: &[T], f: impl Fn(T) -> MenuAction) -> Vec<Entry> {
    source.iter().map(|t| item(f(*t))).collect()
}

/// The whole menu bar, in order.
///
/// Rebuilt per frame rather than cached: it is a few hundred `Vec` pushes, and
/// a cached structure would need invalidating every time the recent-file list
/// changed, which is the class of bug this crate is trying not to have.
pub fn menu_bar(recent_files: usize) -> Vec<Menu> {
    vec![
        file_menu(recent_files),
        edit_menu(),
        image_menu(),
        layer_menu(),
        select_menu(),
        filter_menu(),
        view_menu(),
        window_menu(),
        help_menu(),
    ]
}

/// How many recent files the File menu lists.
pub const MAX_RECENT_FILES: usize = 10;

fn file_menu(recent_files: usize) -> Menu {
    let recent_slots = recent_files.clamp(1, MAX_RECENT_FILES);
    Menu {
        title: "File",
        entries: vec![
            item(MenuAction::NewDocument),
            item(MenuAction::Open),
            Entry::submenu(
                "Open Recent",
                (0..recent_slots)
                    .map(|i| item(MenuAction::OpenRecent(i)))
                    .collect(),
            ),
            Entry::Separator,
            item(MenuAction::CloseDocument),
            item(MenuAction::CloseAll),
            Entry::Separator,
            item(MenuAction::Save),
            item(MenuAction::SaveAs),
            Entry::Separator,
            Entry::submenu(
                "Export As",
                ExportFormat::ALL
                    .iter()
                    .map(|f| item(MenuAction::Export(*f)))
                    .collect(),
            ),
            item(MenuAction::ExportLayers),
            Entry::Separator,
            item(MenuAction::PlaceEmbedded),
            item(MenuAction::PlaceLinked),
            Entry::Separator,
            item(MenuAction::FileInfo),
            item(MenuAction::Print),
            Entry::Separator,
            item(MenuAction::Quit),
        ],
    }
}

fn edit_menu() -> Menu {
    Menu {
        title: "Edit",
        entries: vec![
            item(MenuAction::Undo),
            item(MenuAction::Redo),
            Entry::Separator,
            item(MenuAction::Cut),
            item(MenuAction::Copy),
            item(MenuAction::CopyMerged),
            item(MenuAction::Paste),
            item(MenuAction::PasteInto),
            item(MenuAction::ClearPixels),
            Entry::Separator,
            item(MenuAction::FillDialog),
            item(MenuAction::StrokeDialog),
            Entry::Separator,
            item(MenuAction::FreeTransform),
            Entry::submenu("Transform", items(TransformOp::ALL, MenuAction::Transform)),
            Entry::Separator,
            item(MenuAction::DefinePattern),
            item(MenuAction::DefineBrush),
            Entry::Separator,
            item(MenuAction::KeyboardShortcuts),
            item(MenuAction::Preferences),
        ],
    }
}

fn image_menu() -> Menu {
    Menu {
        title: "Image",
        entries: vec![
            Entry::submenu("Mode", items(ColorMode::ALL, MenuAction::SetColorMode)),
            Entry::Separator,
            Entry::submenu(
                "Adjustments",
                items(AdjustmentId::ALL, MenuAction::ApplyAdjustment),
            ),
            Entry::Separator,
            item(MenuAction::AutoTone),
            item(MenuAction::AutoContrast),
            item(MenuAction::AutoColor),
            Entry::Separator,
            item(MenuAction::ImageSize),
            item(MenuAction::CanvasSize),
            Entry::submenu(
                "Image Rotation",
                items(CanvasRotation::ALL, MenuAction::RotateCanvas),
            ),
            item(MenuAction::CropToSelection),
            item(MenuAction::Trim),
            item(MenuAction::RevealAll),
            Entry::Separator,
            item(MenuAction::DuplicateDocument),
        ],
    }
}

fn layer_menu() -> Menu {
    Menu {
        title: "Layer",
        entries: vec![
            Entry::submenu(
                "New",
                vec![
                    item(MenuAction::NewLayer),
                    item(MenuAction::NewGroup),
                    Entry::Separator,
                    item(MenuAction::LayerViaCopy),
                    item(MenuAction::LayerViaCut),
                ],
            ),
            Entry::submenu(
                "New Fill Layer",
                items(FillLayerKind::ALL, MenuAction::NewFillLayer),
            ),
            Entry::submenu(
                "New Adjustment Layer",
                items(AdjustmentId::ALL, MenuAction::NewAdjustmentLayer),
            ),
            item(MenuAction::EditAdjustmentLayer),
            item(MenuAction::DuplicateLayer),
            item(MenuAction::DeleteLayer),
            Entry::Separator,
            Entry::submenu("Layer Mask", items(MaskOp::ALL, MenuAction::Mask)),
            item(MenuAction::CreateClippingMask),
            item(MenuAction::ReleaseClippingMask),
            Entry::Separator,
            Entry::submenu("Layer Style", {
                let mut e = vec![item(MenuAction::BlendingOptions), Entry::Separator];
                e.extend(items(EffectSlot::ALL, MenuAction::LayerStyle));
                e.push(Entry::Separator);
                e.push(item(MenuAction::ClearLayerStyle));
                e
            }),
            item(MenuAction::ConvertToSmartObject),
            Entry::submenu(
                "Rasterize",
                items(RasterizeTarget::ALL, MenuAction::Rasterize),
            ),
            Entry::Separator,
            item(MenuAction::GroupLayers),
            item(MenuAction::UngroupLayers),
            Entry::submenu("Arrange", items(Arrange::ALL, MenuAction::ArrangeLayer)),
            Entry::Separator,
            item(MenuAction::MergeDown),
            item(MenuAction::MergeVisible),
            item(MenuAction::FlattenImage),
        ],
    }
}

fn select_menu() -> Menu {
    Menu {
        title: "Select",
        entries: vec![
            item(MenuAction::SelectAll),
            item(MenuAction::Deselect),
            item(MenuAction::Reselect),
            item(MenuAction::InverseSelection),
            Entry::Separator,
            item(MenuAction::SelectAllLayers),
            item(MenuAction::DeselectLayers),
            Entry::Separator,
            item(MenuAction::ColorRange),
            item(MenuAction::SelectSubject),
            Entry::Separator,
            Entry::submenu("Modify", items(ModifySelection::ALL, MenuAction::Modify)),
            item(MenuAction::GrowSelection),
            item(MenuAction::SimilarSelection),
            Entry::Separator,
            item(MenuAction::TransformSelection),
            Entry::Separator,
            item(MenuAction::SaveSelection),
            item(MenuAction::LoadSelection),
        ],
    }
}

fn filter_menu() -> Menu {
    let mut entries = vec![
        item(MenuAction::LastFilter),
        Entry::Separator,
        item(MenuAction::FilterGallery),
        Entry::Separator,
    ];
    for group in FilterGroup::ALL {
        entries.push(Entry::submenu(
            group.label(),
            FilterId::ALL
                .iter()
                .filter(|f| f.group() == *group)
                .map(|f| item(MenuAction::Filter(*f)))
                .collect(),
        ));
    }
    Menu {
        title: "Filter",
        entries,
    }
}

fn view_menu() -> Menu {
    let mut entries: Vec<Entry> = items(ZoomCommand::ALL, MenuAction::Zoom);
    entries.push(Entry::Separator);
    // The view's own orientation: the flips are toggles (they stay on, and the
    // menu shows a checkmark), the rotation reset is a one-shot.
    entries.push(item(MenuAction::ResetViewRotation));
    entries.push(Entry::submenu(
        "Rulers",
        items(crate::dialogs::units::Unit::ALL, MenuAction::SetRulerUnit),
    ));
    entries.push(Entry::Separator);
    entries.extend(items(ViewFlag::ALL, MenuAction::ToggleView));
    Menu {
        title: "View",
        entries,
    }
}

fn window_menu() -> Menu {
    let mut entries = vec![Entry::submenu(
        "Workspace",
        items(LayoutId::ALL, MenuAction::ApplyLayout),
    )];
    entries.push(Entry::Separator);
    entries.extend(items(PanelId::ALL, MenuAction::TogglePanel));
    entries.push(Entry::Separator);
    entries.push(Entry::submenu(
        "Appearance",
        items(design::Theme::ALL, MenuAction::SetTheme),
    ));
    Menu {
        title: "Window",
        entries,
    }
}

fn help_menu() -> Menu {
    Menu {
        title: "Help",
        entries: vec![
            item(MenuAction::Help),
            item(MenuAction::KeyboardShortcuts),
            Entry::Separator,
            item(MenuAction::ReleaseNotes),
            item(MenuAction::ReportIssue),
            Entry::Separator,
            item(MenuAction::About),
        ],
    }
}

/// Find the action a chord performs, searching the whole menu bar.
///
/// Used by the shell to run a shortcut without opening a menu, so a shortcut
/// and its menu item can never diverge.
pub fn action_for_shortcut(shortcut: Shortcut, recent_files: usize) -> Option<MenuAction> {
    menu_bar(recent_files)
        .iter()
        .flat_map(Menu::actions)
        .find(|a| a.shortcut() == Some(shortcut))
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_model::{BlendMode, LayerEffects};
    use std::collections::HashSet;

    /// The actions a user can reach by opening a menu.
    fn menu_actions() -> Vec<MenuAction> {
        menu_bar(3).iter().flat_map(Menu::actions).collect()
    }

    /// Every action, including the ones only a panel emits.
    ///
    /// The gates below walk *this* rather than [`menu_actions`]. Walking the
    /// menus only meant `ToggleLayerVisibility`, which no menu lists, was never
    /// checked for a resolution, a reason, or a label.
    fn all_actions() -> Vec<MenuAction> {
        MenuAction::all()
    }

    #[test]
    fn every_action_a_menu_offers_is_in_the_full_list() {
        let full: HashSet<MenuAction> = all_actions().into_iter().collect();
        // Every recent-file count, because the File menu grows its Open Recent
        // submenu with the list and the full list has to cover the longest one.
        for recent in 0..=MAX_RECENT_FILES {
            for action in menu_bar(recent).iter().flat_map(Menu::actions) {
                assert!(
                    full.contains(&action),
                    "{action:?} is in a menu but not in MenuAction::all, so no gate sees it"
                );
            }
        }
    }

    #[test]
    fn the_full_action_list_repeats_nothing() {
        let list = all_actions();
        let unique: HashSet<MenuAction> = list.iter().copied().collect();
        assert_eq!(
            unique.len(),
            list.len(),
            "MenuAction::all lists a duplicate"
        );
        assert!(list.len() > 150, "the action list shrank: {}", list.len());
    }

    #[test]
    fn an_action_only_a_panel_emits_is_still_covered_by_the_gates() {
        // The exact hole a review found: `ToggleLayerVisibility` exists only to
        // be emitted by the Layers panel's eye, appears in no menu, and so was
        // never once resolved by the gate that is supposed to prove no control
        // is a dead end.
        let full: HashSet<MenuAction> = all_actions().into_iter().collect();
        let menued: HashSet<MenuAction> = menu_actions().into_iter().collect();
        let panel_only = MenuAction::ToggleLayerVisibility;
        assert!(full.contains(&panel_only), "{panel_only:?} is in no gate");
        assert!(
            !menued.contains(&panel_only),
            "{panel_only:?} is menu-reachable now, so this test no longer proves anything"
        );
        // And every action a panel emits is in the list, menu-reachable or not.
        for action in [
            MenuAction::EditAdjustmentLayer,
            MenuAction::BlendingOptions,
            MenuAction::NewLayer,
            MenuAction::NewGroup,
        ] {
            assert!(full.contains(&action), "{action:?} is in no gate");
        }
    }

    /// A document with a group holding two raster layers, plus one raster layer
    /// at the root beneath the group.
    fn stacked_document() -> (Document, LayerId, LayerId, LayerId) {
        // `push_root` inserts at the *top* of the stack, so a fixture built
        // with it comes out back to front. Append explicitly instead, so the
        // z-order in the test reads the way it does in the panel.
        let mut doc = Document::new(64, 64, "Test");
        let group = doc
            .layers
            .insert_at(Layer::group("Group"), None, 0)
            .unwrap();
        let bottom = doc
            .layers
            .insert_at(Layer::raster("Bottom"), None, 1)
            .unwrap();
        let top = doc
            .layers
            .insert_at(Layer::raster("Inside"), Some(group), 0)
            .unwrap();
        assert_eq!(doc.layers.root(), &[group, bottom]);
        (doc, group, top, bottom)
    }

    fn ctx_with_layer(doc: &Document, id: LayerId) -> MenuContext {
        MenuContext {
            has_document: true,
            layer_count: doc.layers.len(),
            selected_layers: 1,
            active: ActiveLayer::from_document(doc, id),
            ..Default::default()
        }
    }

    // ---- the contract -----------------------------------------------------

    #[test]
    fn every_item_in_every_menu_resolves_to_a_command_or_a_reason() {
        let (doc, group, inside, _bottom) = stacked_document();
        let contexts = [
            MenuContext::default(),
            MenuContext {
                has_document: true,
                ..Default::default()
            },
            ctx_with_layer(&doc, group),
            ctx_with_layer(&doc, inside),
            MenuContext {
                has_document: true,
                can_undo: true,
                can_redo: true,
                has_selection: true,
                has_stored_selection: true,
                saved_selections: 2,
                recent_files: vec!["a.png".into(), "b.psd".into(), "c.rstudio".into()],
                open_documents: 1,
                last_filter: Some(FilterId::GaussianBlur),
                clipboard: ClipboardState {
                    pixels: true,
                    layers: true,
                },
                ..ctx_with_layer(&doc, inside)
            },
        ];
        for ctx in &contexts {
            for action in all_actions() {
                match action.resolve(ctx) {
                    Resolution::Enabled(_) => {}
                    Resolution::Disabled(reason) => assert!(
                        !reason.trim().is_empty(),
                        "{action:?} is disabled with no reason"
                    ),
                }
            }
        }
    }

    #[test]
    fn no_menu_item_is_a_dead_end() {
        // Restated as its own gate: an enabled item must carry an intent, and a
        // disabled one must carry a sentence. There is no third possibility.
        let ctx = MenuContext::default();
        for action in all_actions() {
            let r = action.resolve(&ctx);
            assert_eq!(
                r.intent().is_some(),
                r.reason().is_none(),
                "{action:?} is both/neither enabled and disabled"
            );
        }
    }

    #[test]
    fn every_action_has_a_label() {
        for action in all_actions() {
            assert!(!action.label().trim().is_empty(), "{action:?}");
        }
    }

    #[test]
    fn no_two_menu_items_claim_the_same_chord() {
        let mut seen: Vec<(Shortcut, MenuAction)> = Vec::new();
        for action in menu_actions() {
            let Some(chord) = action.shortcut() else {
                continue;
            };
            if let Some((_, other)) = seen.iter().find(|(c, a)| *c == chord && *a != action) {
                panic!("{chord} is claimed by both {action:?} and {other:?}");
            }
            seen.push((chord, action));
        }
        assert!(seen.len() > 30, "the menu bar lost its shortcuts");
    }

    #[test]
    fn a_chord_resolves_back_to_its_own_action() {
        assert_eq!(
            action_for_shortcut(Shortcut::ctrl('z'), 0),
            Some(MenuAction::Undo)
        );
        assert_eq!(
            action_for_shortcut(Shortcut::ctrl_shift('z'), 0),
            Some(MenuAction::Redo)
        );
        assert_eq!(
            action_for_shortcut(Shortcut::ctrl_key(Key::RightBracket), 0),
            Some(MenuAction::ArrangeLayer(Arrange::BringForward))
        );
        assert_eq!(action_for_shortcut(Shortcut::ctrl('9'), 0), None);
    }

    #[test]
    fn the_menu_bar_has_the_nine_expected_menus() {
        let titles: Vec<&str> = menu_bar(0).iter().map(|m| m.title).collect();
        assert_eq!(
            titles,
            vec!["File", "Edit", "Image", "Layer", "Select", "Filter", "View", "Window", "Help"]
        );
    }

    // ---- enablement -------------------------------------------------------

    #[test]
    fn undo_and_redo_track_the_history() {
        let empty = MenuContext {
            has_document: true,
            ..Default::default()
        };
        assert_eq!(
            MenuAction::Undo.resolve(&empty).reason(),
            Some("Nothing to undo")
        );
        assert_eq!(
            MenuAction::Redo.resolve(&empty).reason(),
            Some("Nothing to redo")
        );
        let full = MenuContext {
            can_undo: true,
            can_redo: true,
            ..empty
        };
        assert!(MenuAction::Undo.resolve(&full).is_enabled());
        assert!(MenuAction::Redo.resolve(&full).is_enabled());
    }

    #[test]
    fn paste_is_disabled_with_an_empty_clipboard() {
        let ctx = MenuContext {
            has_document: true,
            ..Default::default()
        };
        assert_eq!(
            MenuAction::Paste.resolve(&ctx).reason(),
            Some("The clipboard is empty")
        );
        let with = MenuContext {
            clipboard: ClipboardState {
                pixels: true,
                layers: false,
            },
            ..ctx
        };
        assert!(MenuAction::Paste.resolve(&with).is_enabled());
        // Paste Into needs a selection on top of the clipboard.
        assert_eq!(
            MenuAction::PasteInto.resolve(&with).reason(),
            Some("Paste Into needs a selection")
        );
        assert!(MenuAction::PasteInto
            .resolve(&MenuContext {
                has_selection: true,
                ..with
            })
            .is_enabled());
    }

    #[test]
    fn with_no_document_almost_everything_says_so() {
        let ctx = MenuContext::default();
        for action in [
            MenuAction::Save,
            MenuAction::ImageSize,
            MenuAction::SelectAll,
            MenuAction::Zoom(ZoomCommand::In),
            MenuAction::Export(ExportFormat::Png),
        ] {
            assert_eq!(
                action.resolve(&ctx).reason(),
                Some("No document is open"),
                "{action:?}"
            );
        }
        // ...but the ones that create or quit stay live.
        assert!(MenuAction::NewDocument.resolve(&ctx).is_enabled());
        assert!(MenuAction::Open.resolve(&ctx).is_enabled());
        assert!(MenuAction::Quit.resolve(&ctx).is_enabled());
        assert!(MenuAction::Preferences.resolve(&ctx).is_enabled());
    }

    #[test]
    fn selection_items_need_a_selection() {
        let ctx = MenuContext {
            has_document: true,
            ..Default::default()
        };
        for action in [
            MenuAction::Deselect,
            MenuAction::InverseSelection,
            MenuAction::Modify(ModifySelection::Feather),
            MenuAction::TransformSelection,
            MenuAction::SaveSelection,
            MenuAction::CropToSelection,
        ] {
            assert_eq!(
                action.resolve(&ctx).reason(),
                Some("There is no selection"),
                "{action:?}"
            );
        }
        let selected = MenuContext {
            has_selection: true,
            ..ctx
        };
        for action in [MenuAction::Deselect, MenuAction::InverseSelection] {
            assert!(action.resolve(&selected).is_enabled(), "{action:?}");
        }
    }

    #[test]
    fn reselect_needs_a_selection_that_was_deselected() {
        let ctx = MenuContext {
            has_document: true,
            ..Default::default()
        };
        assert_eq!(
            MenuAction::Reselect.resolve(&ctx).reason(),
            Some("There is no selection to restore")
        );
        assert!(MenuAction::Reselect
            .resolve(&MenuContext {
                has_stored_selection: true,
                ..ctx
            })
            .is_enabled());
    }

    #[test]
    fn cut_is_refused_on_a_pixel_locked_layer() {
        let (doc, _g, inside, _b) = stacked_document();
        let mut ctx = ctx_with_layer(&doc, inside);
        assert!(MenuAction::Cut.resolve(&ctx).is_enabled());
        ctx.active.as_mut().unwrap().locked = LockState {
            pixels: true,
            ..LockState::default()
        };
        assert_eq!(
            MenuAction::Cut.resolve(&ctx).reason(),
            Some("The layer's pixels are locked")
        );
        // Copy does not write, so it stays available.
        assert!(MenuAction::Copy.resolve(&ctx).is_enabled());
    }

    #[test]
    fn filters_refuse_a_layer_that_owns_no_pixels() {
        let (doc, group, inside, _b) = stacked_document();
        assert!(MenuAction::Filter(FilterId::GaussianBlur)
            .resolve(&ctx_with_layer(&doc, inside))
            .is_enabled());
        assert_eq!(
            MenuAction::Filter(FilterId::GaussianBlur)
                .resolve(&ctx_with_layer(&doc, group))
                .reason(),
            Some("This works on a pixel layer; the active layer is not one")
        );
    }

    #[test]
    fn last_filter_waits_until_a_filter_has_been_run() {
        let (doc, _g, inside, _b) = stacked_document();
        let ctx = ctx_with_layer(&doc, inside);
        assert_eq!(
            MenuAction::LastFilter.resolve(&ctx).reason(),
            Some("No filter has been applied yet")
        );
        assert!(MenuAction::LastFilter
            .resolve(&MenuContext {
                last_filter: Some(FilterId::Mosaic),
                ..ctx
            })
            .is_enabled());
    }

    #[test]
    fn an_unsupported_colour_mode_is_disabled_rather_than_hidden() {
        let ctx = MenuContext {
            has_document: true,
            ..Default::default()
        };
        assert!(MenuAction::SetColorMode(ColorMode::Rgb)
            .resolve(&ctx)
            .is_enabled());
        assert_eq!(
            MenuAction::SetColorMode(ColorMode::Cmyk)
                .resolve(&ctx)
                .reason(),
            Some("This build cannot convert to that colour mode yet")
        );
        // And it is still in the menu, so the user can see the product's edge.
        assert!(menu_actions().contains(&MenuAction::SetColorMode(ColorMode::Cmyk)));
    }

    #[test]
    fn ungroup_only_applies_to_a_group() {
        let (doc, group, inside, _b) = stacked_document();
        assert!(MenuAction::UngroupLayers
            .resolve(&ctx_with_layer(&doc, group))
            .is_enabled());
        assert_eq!(
            MenuAction::UngroupLayers
                .resolve(&ctx_with_layer(&doc, inside))
                .reason(),
            Some("Only a group can be ungrouped")
        );
    }

    #[test]
    fn merge_down_needs_something_underneath() {
        let (doc, group, inside, bottom) = stacked_document();
        // `group` is at root index 0 with `bottom` beneath it.
        assert!(MenuAction::MergeDown
            .resolve(&ctx_with_layer(&doc, group))
            .is_enabled());
        // `bottom` is the last root layer.
        assert_eq!(
            MenuAction::MergeDown
                .resolve(&ctx_with_layer(&doc, bottom))
                .reason(),
            Some("There is no layer below to merge into")
        );
        // `inside` is the only child of its group.
        assert_eq!(
            MenuAction::MergeDown
                .resolve(&ctx_with_layer(&doc, inside))
                .reason(),
            Some("There is no layer below to merge into")
        );
    }

    // ---- items that resolve to real commands ------------------------------

    #[test]
    fn delete_layer_resolves_to_the_delete_command_for_the_active_layer() {
        let (doc, _g, inside, _b) = stacked_document();
        let ctx = ctx_with_layer(&doc, inside);
        assert_eq!(
            MenuAction::DeleteLayer.resolve(&ctx).intent(),
            Some(&Intent::Document(Command::DeleteLayer { layer_id: inside }))
        );
    }

    #[test]
    fn a_fully_locked_layer_cannot_be_deleted() {
        let (doc, _g, inside, _b) = stacked_document();
        let mut ctx = ctx_with_layer(&doc, inside);
        ctx.active.as_mut().unwrap().locked = LockState {
            all: true,
            ..LockState::default()
        };
        assert_eq!(
            MenuAction::DeleteLayer.resolve(&ctx).reason(),
            Some("The layer is locked")
        );
    }

    #[test]
    fn arrange_resolves_to_a_move_with_the_right_index() {
        let (doc, group, _i, bottom) = stacked_document();
        // Root order is [group, bottom]; `bottom` sits at index 1.
        let ctx = ctx_with_layer(&doc, bottom);
        assert_eq!(
            MenuAction::ArrangeLayer(Arrange::BringForward)
                .resolve(&ctx)
                .intent(),
            Some(&Intent::Document(Command::MoveLayer {
                layer_id: bottom,
                parent: None,
                index: 0,
            }))
        );
        assert_eq!(
            MenuAction::ArrangeLayer(Arrange::SendBackward)
                .resolve(&ctx)
                .reason(),
            Some("The layer is already at the back of its group")
        );

        let ctx = ctx_with_layer(&doc, group);
        assert_eq!(
            MenuAction::ArrangeLayer(Arrange::SendToBack)
                .resolve(&ctx)
                .intent(),
            Some(&Intent::Document(Command::MoveLayer {
                layer_id: group,
                parent: None,
                index: 1,
            }))
        );
        assert_eq!(
            MenuAction::ArrangeLayer(Arrange::BringToFront)
                .resolve(&ctx)
                .reason(),
            Some("The layer is already at the front of its group")
        );
    }

    #[test]
    fn arrange_targets_are_indices_after_the_layer_is_lifted_out() {
        // Three siblings, moving the middle one.
        assert_eq!(Arrange::BringToFront.target_index(1, 3), 0);
        assert_eq!(Arrange::BringForward.target_index(1, 3), 0);
        assert_eq!(Arrange::SendBackward.target_index(1, 3), 2);
        assert_eq!(Arrange::SendToBack.target_index(1, 3), 2);
        // A lone layer cannot be arranged at all.
        for a in Arrange::ALL {
            assert!(a.is_noop(0, 1), "{a:?}");
        }
    }

    #[test]
    fn an_arrange_command_actually_applies_to_the_tree() {
        // The index arithmetic is the part most likely to be off by one, so
        // resolve it and run it rather than trusting the number.
        let (mut doc, group, _i, bottom) = stacked_document();
        let ctx = ctx_with_layer(&doc, bottom);
        let Some(Intent::Document(command)) = MenuAction::ArrangeLayer(Arrange::BringToFront)
            .resolve(&ctx)
            .intent()
            .cloned()
        else {
            panic!("bring to front did not resolve to a command");
        };
        let mut history = History::new();
        history.apply(&mut doc, command).expect("apply");
        assert_eq!(doc.layers.root(), &[bottom, group]);
    }

    #[test]
    fn clipping_resolves_to_a_property_patch_and_needs_a_layer_below() {
        let (doc, group, inside, bottom) = stacked_document();
        let ctx = ctx_with_layer(&doc, group);
        match MenuAction::CreateClippingMask.resolve(&ctx).intent() {
            Some(Intent::Document(Command::SetLayerProperties { layer_id, patch })) => {
                assert_eq!(*layer_id, group);
                assert_eq!(patch.clipping, Some(ClippingMode::ClipToBelow));
            }
            other => panic!("unexpected resolution: {other:?}"),
        }
        assert_eq!(
            MenuAction::CreateClippingMask
                .resolve(&ctx_with_layer(&doc, bottom))
                .reason(),
            Some("There is no layer below to clip to")
        );
        assert_eq!(
            MenuAction::CreateClippingMask
                .resolve(&ctx_with_layer(&doc, inside))
                .reason(),
            Some("There is no layer below to clip to")
        );
        // Releasing needs the layer to be clipping in the first place.
        assert_eq!(
            MenuAction::ReleaseClippingMask.resolve(&ctx).reason(),
            Some("The layer does not clip")
        );
    }

    #[test]
    fn adding_a_mask_is_a_command_and_a_second_one_is_refused() {
        let (doc, _g, inside, _b) = stacked_document();
        let ctx = ctx_with_layer(&doc, inside);
        match MenuAction::Mask(MaskOp::RevealAll).resolve(&ctx).intent() {
            Some(Intent::Document(Command::SetLayerProperties { layer_id, patch })) => {
                assert_eq!(*layer_id, inside);
                assert!(matches!(patch.mask, Patch::Set(_)));
            }
            other => panic!("unexpected resolution: {other:?}"),
        }
        let masked = MenuContext {
            active: Some(ActiveLayer {
                has_mask: true,
                mask_enabled: true,
                ..ctx.active.unwrap()
            }),
            ..ctx.clone()
        };
        assert_eq!(
            MenuAction::Mask(MaskOp::RevealAll)
                .resolve(&masked)
                .reason(),
            Some("The layer already has a mask")
        );
        // ...and deleting is the other way round.
        assert_eq!(
            MenuAction::Mask(MaskOp::Delete).resolve(&ctx).reason(),
            Some("The layer has no mask")
        );
        match MenuAction::Mask(MaskOp::Delete).resolve(&masked).intent() {
            Some(Intent::Document(Command::SetLayerProperties { patch, .. })) => {
                assert!(matches!(patch.mask, Patch::Clear));
            }
            other => panic!("unexpected resolution: {other:?}"),
        }
    }

    #[test]
    fn a_mask_from_a_selection_needs_a_selection() {
        let (doc, _g, inside, _b) = stacked_document();
        let ctx = ctx_with_layer(&doc, inside);
        assert_eq!(
            MenuAction::Mask(MaskOp::RevealSelection)
                .resolve(&ctx)
                .reason(),
            Some("There is no selection")
        );
        assert!(MenuAction::Mask(MaskOp::RevealSelection)
            .resolve(&MenuContext {
                has_selection: true,
                ..ctx
            })
            .is_enabled());
    }

    #[test]
    fn clearing_a_layer_style_is_a_command_and_needs_a_style_to_clear() {
        let (doc, _g, inside, _b) = stacked_document();
        let ctx = ctx_with_layer(&doc, inside);
        assert_eq!(
            MenuAction::ClearLayerStyle.resolve(&ctx).reason(),
            Some("The layer has no style to clear")
        );
        let styled = MenuContext {
            active: Some(ActiveLayer {
                has_effects: true,
                ..ctx.active.unwrap()
            }),
            ..ctx
        };
        match MenuAction::ClearLayerStyle.resolve(&styled).intent() {
            Some(Intent::Document(Command::SetLayerProperties { patch, .. })) => {
                assert_eq!(patch.effects.as_deref(), Some(&LayerEffects::default()));
            }
            other => panic!("unexpected resolution: {other:?}"),
        }
    }

    #[test]
    fn a_new_adjustment_layer_carries_readable_starting_parameters() {
        let ctx = MenuContext {
            has_document: true,
            ..Default::default()
        };
        // Five adjustments have no identity setting: inverting, thresholding,
        // desaturating, posterizing and mapping to a gradient all change every
        // pixel by definition, so a layer of one is visible the moment it is
        // created — which is Photoshop's behaviour too. Every *other*
        // adjustment must start as a no-op, so adding a layer and not touching
        // it changes nothing and can be undone with no visible flicker.
        //
        // The set is derived and compared whole rather than asserted per item,
        // so a sixth appearing is a failure that names it.
        let mut visible_on_creation = Vec::new();
        for id in AdjustmentId::ALL {
            let resolution = MenuAction::NewAdjustmentLayer(*id).resolve(&ctx);
            let Some(Intent::Document(Command::CreateLayer { layer })) = resolution.intent() else {
                panic!("{id:?} did not resolve to a create");
            };
            let LayerKind::Adjustment(a) = &layer.kind else {
                panic!("{id:?} did not create an adjustment layer");
            };
            let parsed = adjustments::Adjustment::try_from_layer_kind(&a.kind)
                .unwrap_or_else(|e| panic!("{id:?} produced unreadable parameters: {e}"));
            if !parsed.is_identity() {
                visible_on_creation.push(*id);
            }
            assert_eq!(layer.name, id.label());
            assert_eq!(layer.blend_mode, BlendMode::Normal);
            assert!(layer.visible);
        }
        visible_on_creation.sort_unstable();
        let mut expected = vec![
            AdjustmentId::BlackAndWhite,
            AdjustmentId::Invert,
            AdjustmentId::Posterize,
            AdjustmentId::Threshold,
            AdjustmentId::GradientMap,
        ];
        expected.sort_unstable();
        assert_eq!(visible_on_creation, expected);
    }

    #[test]
    fn a_new_layer_is_numbered_after_the_ones_already_there() {
        let ctx = MenuContext {
            has_document: true,
            layer_count: 4,
            ..Default::default()
        };
        let resolution = MenuAction::NewLayer.resolve(&ctx);
        let Some(Intent::Document(Command::CreateLayer { layer })) = resolution.intent() else {
            panic!("New Layer did not resolve to a create");
        };
        assert_eq!(layer.name, "Layer 5");
    }

    #[test]
    fn toggling_a_panel_asks_for_the_opposite_of_what_it_is() {
        let ctx = MenuContext::default();
        assert!(ctx.dock.is_open(PanelId::Layers));
        assert_eq!(
            MenuAction::TogglePanel(PanelId::Layers)
                .resolve(&ctx)
                .intent(),
            Some(&Intent::SetPanelOpen {
                panel: PanelId::Layers,
                open: false,
            })
        );
        assert_eq!(
            MenuAction::TogglePanel(PanelId::Paths)
                .resolve(&ctx)
                .intent(),
            Some(&Intent::SetPanelOpen {
                panel: PanelId::Paths,
                open: true,
            })
        );
        assert_eq!(
            MenuAction::TogglePanel(PanelId::Layers).checked(&ctx),
            Some(true)
        );
    }

    #[test]
    fn a_view_toggle_reports_its_check_state_and_flips_it() {
        let ctx = MenuContext {
            has_document: true,
            ..Default::default()
        };
        assert_eq!(
            MenuAction::ToggleView(ViewFlag::Rulers).checked(&ctx),
            Some(true)
        );
        assert_eq!(
            MenuAction::ToggleView(ViewFlag::Rulers)
                .resolve(&ctx)
                .intent(),
            Some(&Intent::SetViewFlag {
                flag: ViewFlag::Rulers,
                on: false,
            })
        );
        assert_eq!(
            MenuAction::ToggleView(ViewFlag::PixelGrid)
                .resolve(&ctx)
                .intent(),
            Some(&Intent::SetViewFlag {
                flag: ViewFlag::PixelGrid,
                on: true,
            })
        );
    }

    #[test]
    fn the_current_theme_is_checked_and_cannot_be_re_chosen() {
        let ctx = MenuContext {
            theme: design::Theme::Dark,
            ..Default::default()
        };
        assert_eq!(
            MenuAction::SetTheme(design::Theme::Dark).checked(&ctx),
            Some(true)
        );
        assert_eq!(
            MenuAction::SetTheme(design::Theme::Dark)
                .resolve(&ctx)
                .reason(),
            Some("This appearance is already in use")
        );
        assert!(MenuAction::SetTheme(design::Theme::Light)
            .resolve(&ctx)
            .is_enabled());
    }

    #[test]
    fn recent_file_slots_past_the_end_are_disabled() {
        let ctx = MenuContext {
            recent_files: vec!["seaside.png".into(), "portrait.psd".into()],
            ..Default::default()
        };
        assert!(MenuAction::OpenRecent(0).resolve(&ctx).is_enabled());
        assert!(MenuAction::OpenRecent(1).resolve(&ctx).is_enabled());
        assert_eq!(
            MenuAction::OpenRecent(2).resolve(&ctx).reason(),
            Some("This slot has no recent file")
        );
        // The menu always shows at least one slot, so the feature is
        // discoverable even before anything has been opened.
        assert!(menu_bar(0)
            .iter()
            .flat_map(Menu::actions)
            .any(|a| a == MenuAction::OpenRecent(0)));
    }

    #[test]
    fn a_recent_slot_is_labelled_with_its_file_not_its_number() {
        let ctx = MenuContext {
            recent_files: vec!["seaside.png".into(), "portrait.psd".into()],
            ..Default::default()
        };
        assert_eq!(MenuAction::OpenRecent(0).label_in(&ctx), "seaside.png");
        assert_eq!(MenuAction::OpenRecent(1).label_in(&ctx), "portrait.psd");
        // An empty slot still says something rather than drawing a blank row.
        assert_eq!(MenuAction::OpenRecent(2).label_in(&ctx), "Recent 3");
        // ...and a name that arrived empty falls back the same way.
        let blank = MenuContext {
            recent_files: vec![String::new()],
            ..Default::default()
        };
        assert_eq!(MenuAction::OpenRecent(0).label_in(&blank), "Recent 1");
    }

    #[test]
    fn undo_and_redo_name_the_step_they_would_move() {
        let ctx = MenuContext {
            can_undo: true,
            undo_label: Some("Create Layer".into()),
            redo_label: Some("Delete Layer".into()),
            ..Default::default()
        };
        assert_eq!(MenuAction::Undo.label_in(&ctx), "Undo Create Layer");
        assert_eq!(MenuAction::Redo.label_in(&ctx), "Redo Delete Layer");
        assert_eq!(MenuAction::Undo.label_in(&MenuContext::default()), "Undo");
    }

    #[test]
    fn every_item_labels_itself_in_every_context_it_is_drawn_in() {
        let (doc, group, inside, _b) = stacked_document();
        for ctx in [
            MenuContext::default(),
            ctx_with_layer(&doc, group),
            ctx_with_layer(&doc, inside),
        ] {
            for action in all_actions() {
                assert!(
                    !action.label_in(&ctx).trim().is_empty(),
                    "{action:?} drew a blank row"
                );
            }
        }
    }

    #[test]
    fn editing_an_adjustment_layer_is_enabled_exactly_where_applying_one_is_not() {
        // The Properties panel's "Open editor…" is drawn when the active layer
        // *is* an adjustment. That is precisely the state in which
        // `ApplyAdjustment` — which bakes a new adjustment into pixels — is
        // refused, so the two cannot be the same action.
        let mut doc = Document::new(32, 32, "Test");
        let adjustment = doc
            .layers
            .push_root(Layer::with_kind(
                "Curves",
                LayerKind::Adjustment(AdjustmentLayer {
                    kind: AdjustmentId::Curves.identity_kind(),
                }),
            ))
            .unwrap();
        let raster = doc.layers.push_root(Layer::raster("Photo")).unwrap();

        let on_adjustment = ctx_with_layer(&doc, adjustment);
        assert!(MenuAction::EditAdjustmentLayer
            .resolve(&on_adjustment)
            .is_enabled());
        assert_eq!(
            MenuAction::ApplyAdjustment(AdjustmentId::Curves)
                .resolve(&on_adjustment)
                .reason(),
            Some("This works on a pixel layer; the active layer is not one")
        );

        // ...and on a pixel layer it is the other way round.
        let on_raster = ctx_with_layer(&doc, raster);
        assert!(MenuAction::ApplyAdjustment(AdjustmentId::Curves)
            .resolve(&on_raster)
            .is_enabled());
        assert_eq!(
            MenuAction::EditAdjustmentLayer.resolve(&on_raster).reason(),
            Some("The active layer is not an adjustment layer")
        );
        // With nothing selected it says the simpler thing.
        assert_eq!(
            MenuAction::EditAdjustmentLayer
                .resolve(&MenuContext {
                    has_document: true,
                    ..Default::default()
                })
                .reason(),
            Some("Select a layer first")
        );
    }

    #[test]
    fn rasterize_targets_check_the_layer_kind() {
        let (doc, group, inside, _b) = stacked_document();
        // A raster layer is already pixels.
        assert_eq!(
            MenuAction::Rasterize(RasterizeTarget::Layer)
                .resolve(&ctx_with_layer(&doc, inside))
                .reason(),
            Some("The layer is already pixels")
        );
        // A group is not.
        assert!(MenuAction::Rasterize(RasterizeTarget::Layer)
            .resolve(&ctx_with_layer(&doc, group))
            .is_enabled());
        assert_eq!(
            MenuAction::Rasterize(RasterizeTarget::Text)
                .resolve(&ctx_with_layer(&doc, inside))
                .reason(),
            Some("The active layer is not a text layer")
        );
    }

    #[test]
    fn the_context_reads_the_document_it_is_given() {
        let (mut doc, _g, inside, _b) = stacked_document();
        doc.set_active_layer(Some(inside)).unwrap();
        let mut history = History::new();
        history
            .apply(&mut doc, Command::create_layer(Layer::raster("Another")))
            .unwrap();
        let ctx = MenuContext::from_document(&doc, &history);
        assert!(ctx.has_document);
        assert!(ctx.can_undo);
        assert!(!ctx.can_redo);
        assert_eq!(ctx.undo_label.as_deref(), Some("Create Layer"));
        assert_eq!(ctx.layer_count, doc.layers.len());
        assert_eq!(ctx.active.map(|l| l.id), Some(inside));
        assert!(ctx.is_dirty);
    }

    #[test]
    fn the_active_layer_facts_match_the_tree() {
        let (doc, group, inside, bottom) = stacked_document();
        let g = ActiveLayer::from_document(&doc, group).unwrap();
        assert_eq!(g.class, LayerClass::Group);
        assert_eq!(g.index, 0);
        assert_eq!(g.sibling_count, 2);
        assert!(g.has_layer_below());
        assert_eq!(g.parent, None);

        let i = ActiveLayer::from_document(&doc, inside).unwrap();
        assert_eq!(i.class, LayerClass::Raster);
        assert_eq!(i.parent, Some(group));
        assert_eq!(i.sibling_count, 1);
        assert!(!i.has_layer_below());

        let b = ActiveLayer::from_document(&doc, bottom).unwrap();
        assert_eq!(b.index, 1);
        assert!(!b.has_layer_below());
    }

    #[test]
    fn every_filter_belongs_to_exactly_one_group_and_appears_once() {
        let menu = filter_menu();
        let listed: Vec<FilterId> = menu
            .actions()
            .into_iter()
            .filter_map(|a| match a {
                MenuAction::Filter(f) => Some(f),
                _ => None,
            })
            .collect();
        let unique: HashSet<FilterId> = listed.iter().copied().collect();
        assert_eq!(unique.len(), listed.len(), "a filter is listed twice");
        assert_eq!(
            unique,
            FilterId::ALL.iter().copied().collect::<HashSet<_>>(),
            "a filter is missing from the menu"
        );
        for f in FilterId::ALL {
            assert!(!f.label().is_empty(), "{f:?}");
            assert!(FilterGroup::ALL.contains(&f.group()), "{f:?}");
        }
    }

    #[test]
    fn every_adjustment_appears_in_both_the_image_and_layer_menus() {
        let image: HashSet<AdjustmentId> = image_menu()
            .actions()
            .into_iter()
            .filter_map(|a| match a {
                MenuAction::ApplyAdjustment(id) => Some(id),
                _ => None,
            })
            .collect();
        let layer: HashSet<AdjustmentId> = layer_menu()
            .actions()
            .into_iter()
            .filter_map(|a| match a {
                MenuAction::NewAdjustmentLayer(id) => Some(id),
                _ => None,
            })
            .collect();
        let all: HashSet<AdjustmentId> = AdjustmentId::ALL.iter().copied().collect();
        assert_eq!(image, all);
        assert_eq!(layer, all);
    }

    #[test]
    fn every_panel_is_reachable_from_the_window_menu() {
        let listed: HashSet<PanelId> = window_menu()
            .actions()
            .into_iter()
            .filter_map(|a| match a {
                MenuAction::TogglePanel(p) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(listed, PanelId::ALL.iter().copied().collect::<HashSet<_>>());
    }

    #[test]
    fn effect_slots_read_the_layers_effect_block() {
        let mut effects = LayerEffects::default();
        for slot in EffectSlot::ALL {
            assert!(!slot.is_set(&effects), "{slot:?}");
        }
        effects.drop_shadow = Some(layer_model::ShadowEffect::default());
        assert!(EffectSlot::DropShadow.is_set(&effects));
        assert!(!EffectSlot::InnerShadow.is_set(&effects));
        assert_eq!(effects.count(), 1);
    }
}
