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

use editor_core::{Command, Document, Guides, History, LayerPatch, Patch};
use layer_model::{
    AdjustmentKind, AdjustmentLayer, ClippingMode, Layer, LayerId, LayerKind, LockState,
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
    // W4-E: the five Photopea has beyond the classic set.
    Desaturate,
    Equalize,
    ShadowsHighlights,
    ReplaceColor,
    ColorLookup,
    // W7-G: the two Photopea has that were still missing.
    HdrToning,
    MatchColor,
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
        AdjustmentId::Desaturate,
        AdjustmentId::Equalize,
        AdjustmentId::ShadowsHighlights,
        AdjustmentId::HdrToning,
        AdjustmentId::MatchColor,
        AdjustmentId::ReplaceColor,
        AdjustmentId::ColorLookup,
    ];

    /// The adjustments Layer ▸ New Adjustment Layer and the Adjustments panel
    /// offer: every one in [`Self::ALL`] except the six Photopea keeps
    /// destructive-only — Desaturate, Equalize, Shadows/Highlights, HDR
    /// Toning, Match Color and Replace Color live in Image ▸ Adjustments
    /// alone.
    pub const LAYERS: &'static [AdjustmentId] = &[
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
        AdjustmentId::ColorLookup,
    ];

    /// Whether this adjustment can be an adjustment layer (it is in
    /// [`Self::LAYERS`]).
    pub fn is_layer(self) -> bool {
        Self::LAYERS.contains(&self)
    }

    /// Whether Image ▸ Adjustments opens a dialog for this adjustment.
    /// Desaturate, Equalize and Invert ask nothing, in Photoshop and Photopea
    /// as here: the click (or Ctrl+I, W5-E) applies them.
    pub const fn has_dialog(self) -> bool {
        !matches!(
            self,
            AdjustmentId::Desaturate | AdjustmentId::Equalize | AdjustmentId::Invert
        )
    }

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
            AdjustmentId::Desaturate => "Desaturate",
            AdjustmentId::Equalize => "Equalize",
            AdjustmentId::ShadowsHighlights => "Shadows/Highlights",
            AdjustmentId::ReplaceColor => "Replace Color",
            AdjustmentId::ColorLookup => "Color Lookup",
            AdjustmentId::HdrToning => "HDR Toning",
            AdjustmentId::MatchColor => "Match Color",
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
            AdjustmentId::Desaturate => AdjustmentKind::Desaturate,
            AdjustmentId::Equalize => AdjustmentKind::Equalize,
            // Photoshop's opening setting: shadows 35%, tonal width 50%,
            // radius 30 px; highlights off.
            AdjustmentId::ShadowsHighlights => AdjustmentKind::ShadowsHighlights {
                shadows: [0.35, 0.5, 30.0],
                highlights: [0.0, 0.5, 30.0],
            },
            // Photoshop's fuzziness of 40; the dialog replaces the colour
            // with one sampled from the layer when it opens.
            AdjustmentId::ReplaceColor => AdjustmentKind::ReplaceColor {
                color: [1.0, 0.0, 0.0],
                fuzziness: 40.0 / 255.0,
                hue: 0.0,
                saturation: 0.0,
                lightness: 0.0,
            },
            // The identity cube: nothing is looked up until a table is chosen.
            AdjustmentId::ColorLookup => {
                let lut = adjustments::Lut3d::identity(2);
                AdjustmentKind::ColorLookup {
                    name: String::new(),
                    size: lut.size() as u32,
                    table: lut.table().to_vec(),
                }
            }
            // Photoshop's opening "Default" preset: edge glow radius 15 px at
            // strength 0.52, detail 30%, gamma 1, no exposure shift.
            AdjustmentId::HdrToning => AdjustmentKind::HdrToning {
                radius: 15.0,
                strength: 0.52,
                gamma: 1.0,
                exposure: 0.0,
                detail: 0.3,
                vibrance: 0.0,
                saturation: 0.0,
            },
            // No source picked: the target matched to itself, the identity.
            // The dialog measures the layer and offers the open documents
            // and layers as sources.
            AdjustmentId::MatchColor => {
                let n = adjustments::LabStats::NEUTRAL;
                AdjustmentKind::MatchColor {
                    source_mean: n.mean,
                    source_std: n.std,
                    target_mean: n.mean,
                    target_std: n.std,
                    luminance: 1.0,
                    color_intensity: 1.0,
                    fade: 0.0,
                    neutralize: false,
                }
            }
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
    Average,
    Blur,
    BlurMore,
    BoxBlur,
    GaussianBlur,
    LensBlur,
    MotionBlur,
    RadialBlur,
    SmartBlur,
    SurfaceBlur,
    // Sharpen
    Sharpen,
    SharpenEdges,
    SharpenMore,
    SmartSharpen,
    UnsharpMask,
    // Noise
    AddNoise,
    Despeckle,
    DustAndScratches,
    Median,
    ReduceNoise,
    // Distort
    Displace,
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
    Facet,
    Fragment,
    Mezzotint,
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
    Extrude,
    FindEdges,
    OilPaint,
    Solarize,
    Tiles,
    TraceContour,
    Wind,
    // Other
    Custom,
    HighPass,
    Maximum,
    Minimum,
    Offset,
    // W10-C: Photopea's remaining filter rows. Camera Raw and Lens
    // Correction are top-level Filter rows (see `FilterId::is_top_level`).
    CameraRaw,
    LensCorrection,
    LightingEffects,
    HsbHsl,
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
        FilterId::Average,
        FilterId::Blur,
        FilterId::BlurMore,
        FilterId::BoxBlur,
        FilterId::GaussianBlur,
        FilterId::LensBlur,
        FilterId::MotionBlur,
        FilterId::RadialBlur,
        FilterId::SmartBlur,
        FilterId::SurfaceBlur,
        FilterId::Sharpen,
        FilterId::SharpenEdges,
        FilterId::SharpenMore,
        FilterId::SmartSharpen,
        FilterId::UnsharpMask,
        FilterId::AddNoise,
        FilterId::Despeckle,
        FilterId::DustAndScratches,
        FilterId::Median,
        FilterId::ReduceNoise,
        FilterId::Displace,
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
        FilterId::Facet,
        FilterId::Fragment,
        FilterId::Mezzotint,
        FilterId::Mosaic,
        FilterId::Pointillize,
        FilterId::Clouds,
        FilterId::DifferenceClouds,
        FilterId::Fibers,
        FilterId::GradientFill,
        FilterId::LensFlare,
        FilterId::Diffuse,
        FilterId::Emboss,
        FilterId::Extrude,
        FilterId::FindEdges,
        FilterId::OilPaint,
        FilterId::Solarize,
        FilterId::Tiles,
        FilterId::TraceContour,
        FilterId::Wind,
        FilterId::Custom,
        FilterId::HighPass,
        FilterId::Maximum,
        FilterId::Minimum,
        FilterId::Offset,
        FilterId::CameraRaw,
        FilterId::LensCorrection,
        FilterId::LightingEffects,
        FilterId::HsbHsl,
    ];

    pub const fn group(self) -> FilterGroup {
        match self {
            FilterId::Average
            | FilterId::Blur
            | FilterId::BlurMore
            | FilterId::BoxBlur
            | FilterId::GaussianBlur
            | FilterId::LensBlur
            | FilterId::MotionBlur
            | FilterId::RadialBlur
            | FilterId::SmartBlur
            | FilterId::SurfaceBlur => FilterGroup::Blur,
            FilterId::Sharpen
            | FilterId::SharpenEdges
            | FilterId::SharpenMore
            | FilterId::SmartSharpen
            | FilterId::UnsharpMask => FilterGroup::Sharpen,
            FilterId::AddNoise
            | FilterId::Despeckle
            | FilterId::DustAndScratches
            | FilterId::Median
            | FilterId::ReduceNoise => FilterGroup::Noise,
            FilterId::Displace
            | FilterId::Pinch
            | FilterId::PolarCoordinates
            | FilterId::Ripple
            | FilterId::Shear
            | FilterId::Spherize
            | FilterId::Twirl
            | FilterId::Wave
            | FilterId::ZigZag => FilterGroup::Distort,
            FilterId::ColorHalftone
            | FilterId::Crystallize
            | FilterId::Facet
            | FilterId::Fragment
            | FilterId::Mezzotint
            | FilterId::Mosaic
            | FilterId::Pointillize => FilterGroup::Pixelate,
            FilterId::Clouds
            | FilterId::DifferenceClouds
            | FilterId::Fibers
            | FilterId::GradientFill
            | FilterId::LensFlare
            | FilterId::LightingEffects => FilterGroup::Render,
            FilterId::Diffuse
            | FilterId::Emboss
            | FilterId::Extrude
            | FilterId::FindEdges
            | FilterId::OilPaint
            | FilterId::Solarize
            | FilterId::Tiles
            | FilterId::TraceContour
            | FilterId::Wind => FilterGroup::Stylize,
            FilterId::Custom
            | FilterId::HighPass
            | FilterId::Maximum
            | FilterId::Minimum
            | FilterId::Offset
            | FilterId::HsbHsl => FilterGroup::Other,
            // Top-level rows (`is_top_level`): catalogued under Other, drawn
            // directly in the Filter menu and in no submenu.
            FilterId::CameraRaw | FilterId::LensCorrection => FilterGroup::Other,
        }
    }

    /// Menu label. A trailing ellipsis means the filter opens a dialog; a
    /// filter with no parameters applies immediately and carries none.
    pub const fn label(self) -> &'static str {
        match self {
            FilterId::Average => "Average",
            FilterId::Blur => "Blur",
            FilterId::BlurMore => "Blur More",
            FilterId::BoxBlur => "Box Blur…",
            FilterId::GaussianBlur => "Gaussian Blur…",
            FilterId::LensBlur => "Lens Blur…",
            FilterId::MotionBlur => "Motion Blur…",
            FilterId::RadialBlur => "Radial Blur…",
            FilterId::SmartBlur => "Smart Blur…",
            FilterId::SurfaceBlur => "Surface Blur…",
            FilterId::Sharpen => "Sharpen",
            FilterId::SharpenEdges => "Sharpen Edges",
            FilterId::SharpenMore => "Sharpen More",
            FilterId::SmartSharpen => "Smart Sharpen…",
            FilterId::UnsharpMask => "Unsharp Mask…",
            FilterId::AddNoise => "Add Noise…",
            FilterId::Despeckle => "Despeckle",
            FilterId::DustAndScratches => "Dust & Scratches…",
            FilterId::Median => "Median…",
            FilterId::ReduceNoise => "Reduce Noise…",
            FilterId::Displace => "Displace…",
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
            FilterId::Facet => "Facet",
            FilterId::Fragment => "Fragment",
            FilterId::Mezzotint => "Mezzotint…",
            FilterId::Mosaic => "Mosaic…",
            FilterId::Pointillize => "Pointillize…",
            FilterId::Clouds => "Clouds",
            FilterId::DifferenceClouds => "Difference Clouds",
            FilterId::Fibers => "Fibers…",
            FilterId::GradientFill => "Gradient…",
            FilterId::LensFlare => "Lens Flare…",
            FilterId::Diffuse => "Diffuse…",
            FilterId::Emboss => "Emboss…",
            FilterId::Extrude => "Extrude…",
            FilterId::FindEdges => "Find Edges",
            FilterId::OilPaint => "Oil Paint…",
            FilterId::Solarize => "Solarize",
            FilterId::Tiles => "Tiles…",
            FilterId::TraceContour => "Trace Contour…",
            FilterId::Wind => "Wind…",
            FilterId::Custom => "Custom…",
            FilterId::HighPass => "High Pass…",
            FilterId::Maximum => "Maximum…",
            FilterId::Minimum => "Minimum…",
            FilterId::Offset => "Offset…",
            FilterId::CameraRaw => "Camera Raw…",
            FilterId::LensCorrection => "Lens Correction…",
            FilterId::LightingEffects => "Lighting Effects…",
            FilterId::HsbHsl => "HSB/HSL…",
        }
    }

    /// Whether the filter is a row of the Filter menu itself rather than of
    /// its group's submenu — Camera Raw and Lens Correction, where Photopea
    /// and Photoshop put them.
    pub const fn is_top_level(self) -> bool {
        matches!(self, FilterId::CameraRaw | FilterId::LensCorrection)
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

/// What Edit ▸ Purge drops.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum PurgeTarget {
    /// The application's own copied pixels.
    Clipboard,
    /// Every open document's undo and redo stacks. Cannot be undone, so the
    /// application asks for a confirmation first.
    Histories,
    /// Both of the above.
    All,
}

impl PurgeTarget {
    pub const ALL: &'static [PurgeTarget] = &[
        PurgeTarget::Clipboard,
        PurgeTarget::Histories,
        PurgeTarget::All,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            PurgeTarget::Clipboard => "Clipboard",
            PurgeTarget::Histories => "Histories",
            PurgeTarget::All => "All",
        }
    }

    /// Why this purge has nothing to drop in `ctx`, or `None` when it has.
    fn unavailable_reason(self, ctx: &MenuContext) -> Option<&'static str> {
        let clipboard = ctx.clipboard.has_internal_pixels();
        // Every open document's history, not the active one's: that is what
        // the purge drops.
        let history = ctx.has_document && ctx.any_history;
        match self {
            PurgeTarget::Clipboard => (!clipboard).then_some("The clipboard is empty"),
            PurgeTarget::Histories => ctx
                .need_document()
                .or((!history).then_some("There is no history to purge")),
            PurgeTarget::All => (!clipboard && !history).then_some("There is nothing to purge"),
        }
    }
}

/// W7-I: a step offered under Edit ▸ Content-Aware Scale — the active layer
/// retargeted by seam carving (`filters::content_aware_scale`) to a fixed
/// fraction of the canvas along one axis, centred, so the low-detail areas
/// give way and the high-contrast content keeps its size. W10-J: these are
/// quick presets; the interactive box with handles and an Amount is
/// [`MenuAction::ContentAwareScaleFree`] (Free Transform's Content-Aware
/// mode).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum ContentAwareScaleStep {
    Width80,
    Width90,
    Width110,
    Width125,
    Height80,
    Height90,
    Height110,
    Height125,
}

impl ContentAwareScaleStep {
    pub const ALL: &'static [ContentAwareScaleStep] = &[
        ContentAwareScaleStep::Width80,
        ContentAwareScaleStep::Width90,
        ContentAwareScaleStep::Width110,
        ContentAwareScaleStep::Width125,
        ContentAwareScaleStep::Height80,
        ContentAwareScaleStep::Height90,
        ContentAwareScaleStep::Height110,
        ContentAwareScaleStep::Height125,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            ContentAwareScaleStep::Width80 => "Width to 80%",
            ContentAwareScaleStep::Width90 => "Width to 90%",
            ContentAwareScaleStep::Width110 => "Width to 110%",
            ContentAwareScaleStep::Width125 => "Width to 125%",
            ContentAwareScaleStep::Height80 => "Height to 80%",
            ContentAwareScaleStep::Height90 => "Height to 90%",
            ContentAwareScaleStep::Height110 => "Height to 110%",
            ContentAwareScaleStep::Height125 => "Height to 125%",
        }
    }

    /// `(horizontal, vertical)` scale factors, in percent.
    pub const fn percent(self) -> (u32, u32) {
        match self {
            ContentAwareScaleStep::Width80 => (80, 100),
            ContentAwareScaleStep::Width90 => (90, 100),
            ContentAwareScaleStep::Width110 => (110, 100),
            ContentAwareScaleStep::Width125 => (125, 100),
            ContentAwareScaleStep::Height80 => (100, 80),
            ContentAwareScaleStep::Height90 => (100, 90),
            ContentAwareScaleStep::Height110 => (100, 110),
            ContentAwareScaleStep::Height125 => (100, 125),
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
    /// W10-H: black and white only, from Grayscale.
    Bitmap,
    /// W10-H: one to four inks over a grayscale image, from Grayscale.
    Duotone,
}

impl ColorMode {
    pub const ALL: &'static [ColorMode] = &[
        ColorMode::Rgb,
        ColorMode::Grayscale,
        ColorMode::Lab,
        ColorMode::Cmyk,
        ColorMode::Indexed,
        ColorMode::Bitmap,
        ColorMode::Duotone,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            ColorMode::Rgb => "RGB Color",
            ColorMode::Grayscale => "Grayscale",
            ColorMode::Lab => "Lab Color",
            ColorMode::Cmyk => "CMYK Color",
            ColorMode::Indexed => "Indexed Color…",
            ColorMode::Bitmap => "Bitmap…",
            ColorMode::Duotone => "Duotone…",
        }
    }

    /// W10-H: why a document in `current` cannot convert into `self`
    /// directly, or `None` when it can. Photoshop's rule: Bitmap and
    /// Duotone are reached from Grayscale only
    /// (`editor_core::color_mode::conversion_allowed`).
    pub const fn conversion_reason(self, current: ColorMode) -> Option<&'static str> {
        match (self, current) {
            (ColorMode::Bitmap | ColorMode::Duotone, ColorMode::Grayscale)
            | (ColorMode::Bitmap, ColorMode::Bitmap)
            | (ColorMode::Duotone, ColorMode::Duotone) => None,
            (ColorMode::Bitmap, _) => {
                Some("Bitmap is reached from Grayscale: convert to Grayscale first")
            }
            (ColorMode::Duotone, _) => {
                Some("Duotone is reached from Grayscale: convert to Grayscale first")
            }
            _ => None,
        }
    }

    /// W10-H: why a 32 Bits/Channel document cannot convert into `self`:
    /// its colour-mode conversions are 8-bit, so every mode change but the
    /// row already checked waits for a 16 or 8 Bits/Channel document.
    pub const fn depth_reason(
        self,
        current: ColorMode,
        depth: ChannelDepth,
    ) -> Option<&'static str> {
        match depth {
            ChannelDepth::ThirtyTwo
                if !matches!(
                    (self, current),
                    (ColorMode::Rgb, ColorMode::Rgb) | (ColorMode::Grayscale, ColorMode::Grayscale)
                ) =>
            {
                Some("Convert to 16 or 8 Bits/Channel before changing the colour mode")
            }
            _ => None,
        }
    }

    /// Whether this build can convert a document into the mode.
    ///
    /// W7-D: all five. Tiles stay RGBA (Photopea's approach): the mode is a
    /// document flag plus a constraint on the pixels — Grayscale collapses to
    /// Rec.601 luma, CMYK clamps every colour to what `color::cmyk`'s
    /// documented ink model can print (no ICC press profile), Indexed maps
    /// onto a palette of 2-256 colours (`color::quantize`), and RGB and Lab
    /// keep every 8-bit sRGB colour. Each conversion is one undo step
    /// (`editor_core::color_mode::convert_color_mode`).
    pub const fn is_supported(self) -> bool {
        self.unsupported_reason().is_none()
    }

    /// Why this build cannot convert into the mode, or `None` when it can.
    /// `None` for every mode since W7-D; kept so a future mode that cannot be
    /// honoured is greyed with a reason rather than hidden.
    pub const fn unsupported_reason(self) -> Option<&'static str> {
        match self {
            ColorMode::Rgb
            | ColorMode::Grayscale
            | ColorMode::Lab
            | ColorMode::Cmyk
            | ColorMode::Indexed
            | ColorMode::Bitmap
            | ColorMode::Duotone => None,
        }
    }

    /// The mode a document's `meta.color_mode` byte names (the discriminant
    /// order); an unknown byte reads as RGB, the mode its pixels are stored in.
    pub const fn from_meta(byte: u8) -> Self {
        match byte {
            1 => ColorMode::Grayscale,
            2 => ColorMode::Lab,
            3 => ColorMode::Cmyk,
            4 => ColorMode::Indexed,
            5 => ColorMode::Bitmap,
            6 => ColorMode::Duotone,
            _ => ColorMode::Rgb,
        }
    }
}

/// A document's bits per channel, as Image ▸ Mode lists them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Default)]
pub enum ChannelDepth {
    #[default]
    Eight,
    Sixteen,
    /// W10-H: 32 bits per channel, `f32` layer tiles (`raster::depth32`).
    ThirtyTwo,
}

impl ChannelDepth {
    pub const ALL: &'static [ChannelDepth] = &[
        ChannelDepth::Eight,
        ChannelDepth::Sixteen,
        ChannelDepth::ThirtyTwo,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            ChannelDepth::Eight => "8 Bits/Channel",
            ChannelDepth::Sixteen => "16 Bits/Channel",
            ChannelDepth::ThirtyTwo => "32 Bits/Channel",
        }
    }

    /// The document metadata's `bit_depth` as a menu depth: 16 is Sixteen,
    /// 32 is ThirtyTwo, anything else Eight.
    pub const fn of_bits(bits: u8) -> Self {
        match bits {
            16 => ChannelDepth::Sixteen,
            32 => ChannelDepth::ThirtyTwo,
            _ => ChannelDepth::Eight,
        }
    }

    /// The depth as `DocumentMeta::bit_depth` stores it.
    pub const fn bits(self) -> u8 {
        match self {
            ChannelDepth::Eight => 8,
            ChannelDepth::Sixteen => 16,
            ChannelDepth::ThirtyTwo => 32,
        }
    }

    /// W10-H: why a document in colour mode `mode` cannot go to this depth.
    /// As in Photoshop, 32 Bits/Channel is for RGB and Grayscale documents.
    pub const fn mode_reason(self, mode: ColorMode) -> Option<&'static str> {
        match (self, mode) {
            (ChannelDepth::ThirtyTwo, ColorMode::Rgb | ColorMode::Grayscale) => None,
            (ChannelDepth::ThirtyTwo, _) => {
                Some("32 Bits/Channel is for RGB and Grayscale documents")
            }
            _ => None,
        }
    }

    /// Why converting a document at `current` into `self` is unavailable,
    /// or `None` when it is.
    ///
    /// Both conversions exist (W4-F: `OpenDocument::depth_conversion`, one
    /// undoable step that widens or rounds every raster tile), so the only
    /// refusal is the row for the depth the document is already at — the
    /// checked row, as in Photoshop.
    pub const fn conversion_reason(self, current: ChannelDepth) -> Option<&'static str> {
        match (current, self) {
            (ChannelDepth::Eight, ChannelDepth::Eight)
            | (ChannelDepth::Sixteen, ChannelDepth::Sixteen)
            | (ChannelDepth::ThirtyTwo, ChannelDepth::ThirtyTwo) => {
                Some("The document is already at that depth")
            }
            _ => None,
        }
    }
}

/// A Layer ▸ Layer Mask operation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum MaskOp {
    RevealAll,
    HideAll,
    RevealSelection,
    HideSelection,
    /// Card 058: invert the layer's mask coverage (255 − v per pixel) —
    /// an undoable one-channel delta, not a content edit.
    Invert,
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
        MaskOp::Invert,
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
            MaskOp::Invert => "Invert",
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

/// W9-G: a Layer ▸ Vector Mask operation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum VectorMaskOp {
    /// An empty, inverted path: everything shows.
    RevealAll,
    /// An empty path: nothing shows.
    HideAll,
    /// The Paths panel's selected path (or the pen's Work Path).
    CurrentPath,
    Delete,
    /// Disable / enable the vector mask, keeping it.
    Toggle,
}

impl VectorMaskOp {
    pub const ALL: &'static [VectorMaskOp] = &[
        VectorMaskOp::RevealAll,
        VectorMaskOp::HideAll,
        VectorMaskOp::CurrentPath,
        VectorMaskOp::Delete,
        VectorMaskOp::Toggle,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            VectorMaskOp::RevealAll => "Reveal All",
            VectorMaskOp::HideAll => "Hide All",
            VectorMaskOp::CurrentPath => "Current Path",
            VectorMaskOp::Delete => "Delete Vector Mask",
            VectorMaskOp::Toggle => "Disable / Enable Vector Mask",
        }
    }

    /// `true` when the operation adds a vector mask.
    const fn creates(self) -> bool {
        matches!(
            self,
            VectorMaskOp::RevealAll | VectorMaskOp::HideAll | VectorMaskOp::CurrentPath
        )
    }
}

/// W9-K: one Layer ▸ Text row over the active text layer's live warp:
/// `Dialog` opens Warp Text… (style, bend and both distortions), a `Style`
/// row sets the style at once (None clears the warp).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum WarpTextItem {
    Dialog,
    Style(layer_model::text::WarpStyle),
}

impl WarpTextItem {
    /// The dialog, then None and the thirteen styles.
    pub const ALL: &'static [WarpTextItem] = &[
        WarpTextItem::Dialog,
        WarpTextItem::Style(layer_model::text::WarpStyle::None),
        WarpTextItem::Style(layer_model::text::WarpStyle::Arc),
        WarpTextItem::Style(layer_model::text::WarpStyle::ArcLower),
        WarpTextItem::Style(layer_model::text::WarpStyle::ArcUpper),
        WarpTextItem::Style(layer_model::text::WarpStyle::Arch),
        WarpTextItem::Style(layer_model::text::WarpStyle::Bulge),
        WarpTextItem::Style(layer_model::text::WarpStyle::Flag),
        WarpTextItem::Style(layer_model::text::WarpStyle::Wave),
        WarpTextItem::Style(layer_model::text::WarpStyle::Fish),
        WarpTextItem::Style(layer_model::text::WarpStyle::Rise),
        WarpTextItem::Style(layer_model::text::WarpStyle::Fisheye),
        WarpTextItem::Style(layer_model::text::WarpStyle::Inflate),
        WarpTextItem::Style(layer_model::text::WarpStyle::Squeeze),
        WarpTextItem::Style(layer_model::text::WarpStyle::Twist),
    ];

    pub fn label(self) -> String {
        match self {
            WarpTextItem::Dialog => "Warp Text…".to_string(),
            WarpTextItem::Style(style) => style.label().to_string(),
        }
    }

    /// `warp` with this row applied: a style keeps the numbers (None clears
    /// the whole warp). The dialog row changes nothing by itself - its
    /// confirmation carries the whole new warp.
    pub fn apply(self, warp: layer_model::text::TextWarp) -> layer_model::text::TextWarp {
        match self {
            WarpTextItem::Dialog => warp,
            WarpTextItem::Style(layer_model::text::WarpStyle::None) => {
                layer_model::text::TextWarp::default()
            }
            WarpTextItem::Style(style) => layer_model::text::TextWarp { style, ..warp },
        }
    }
}

/// W10-I: a Layer ▸ Smart Object row beyond Edit / Replace / Commit.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum SmartObjectOp {
    /// Write the object's source (its embedded bytes, or the linked file's)
    /// to a file the user picks.
    ExportContents,
    /// A copy of the object with its OWN source: editing or replacing one
    /// leaves the other alone (Duplicate Layer shares the source).
    NewViaCopy,
    /// Unpack the object into a group of ordinary layers in its place.
    ConvertToLayers,
    /// Point a linked object at another file.
    RelinkToFile,
}

impl SmartObjectOp {
    pub const ALL: &'static [SmartObjectOp] = &[
        SmartObjectOp::NewViaCopy,
        SmartObjectOp::ExportContents,
        SmartObjectOp::RelinkToFile,
        SmartObjectOp::ConvertToLayers,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            SmartObjectOp::ExportContents => "Export Contents…",
            SmartObjectOp::NewViaCopy => "New Smart Object via Copy",
            SmartObjectOp::ConvertToLayers => "Convert to Layers",
            SmartObjectOp::RelinkToFile => "Relink to File…",
        }
    }
}

/// W10-I: a Layer ▸ Smart Filter row — the smart filters' shared mask
/// (Photopea's filter mask). The Layers panel's filter-mask row raises the
/// same actions.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum SmartFilterOp {
    /// Give the active smart object's filters a mask, white (every filter
    /// shows everywhere), and aim painting at it.
    AddMask,
    /// Aim painting at the filter mask (the row's thumbnail click).
    EditMask,
    /// Switch the filter mask off (the filters show everywhere) or back on.
    ToggleMask,
    /// Remove the filter mask.
    DeleteMask,
}

impl SmartFilterOp {
    pub const ALL: &'static [SmartFilterOp] = &[
        SmartFilterOp::AddMask,
        SmartFilterOp::EditMask,
        SmartFilterOp::ToggleMask,
        SmartFilterOp::DeleteMask,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            SmartFilterOp::AddMask => "Add Filter Mask",
            SmartFilterOp::EditMask => "Edit Filter Mask",
            SmartFilterOp::ToggleMask => "Disable / Enable Filter Mask",
            SmartFilterOp::DeleteMask => "Delete Filter Mask",
        }
    }
}

/// W10-I: why a Layer ▸ Smart Filter row is greyed.
pub const NO_SMART_FILTERS: &str =
    "The smart object has no smart filters: a filter mask masks its smart filters";
pub const FILTER_MASK_EXISTS: &str = "The smart filters already have a filter mask";
pub const NO_FILTER_MASK: &str = "The smart filters have no filter mask";

/// W10-I: a Layer ▸ Matting row — undo a matte the layer's edge pixels were
/// composited against.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum MattingOp {
    RemoveBlackMatte,
    RemoveWhiteMatte,
}

impl MattingOp {
    pub const ALL: &'static [MattingOp] =
        &[MattingOp::RemoveBlackMatte, MattingOp::RemoveWhiteMatte];

    pub const fn label(self) -> &'static str {
        match self {
            MattingOp::RemoveBlackMatte => "Remove Black Matte",
            MattingOp::RemoveWhiteMatte => "Remove White Matte",
        }
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

/// W9-F: a Layer > Combine Shapes operation — merges the selected shape
/// layers into the bottom-most one with a vector boolean op, folded bottom
/// to top (so Subtract Front takes each upper shape out of those below).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum ShapeCombine {
    Unite,
    SubtractFront,
    Intersect,
    Exclude,
}

impl ShapeCombine {
    pub const ALL: &'static [ShapeCombine] = &[
        ShapeCombine::Unite,
        ShapeCombine::SubtractFront,
        ShapeCombine::Intersect,
        ShapeCombine::Exclude,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            ShapeCombine::Unite => "Unite Shapes",
            ShapeCombine::SubtractFront => "Subtract Front Shape",
            ShapeCombine::Intersect => "Intersect Shape Areas",
            ShapeCombine::Exclude => "Exclude Overlapping Shapes",
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
    /// Fill the viewport with the document — the other half of Fit on Screen,
    /// which leaves letterbox bars whenever the document and the viewport are
    /// not the same shape.
    FillScreen,
    ActualPixels,
    /// Frame the current selection. Disabled, with a reason, when there is
    /// nothing selected.
    ToSelection,
    PrintSize,
    /// View ▸ 200%: two document pixels per screen pixel, an absolute zoom
    /// like [`ZoomCommand::ActualPixels`] rather than a step.
    Double,
}

impl ZoomCommand {
    pub const ALL: &'static [ZoomCommand] = &[
        ZoomCommand::In,
        ZoomCommand::Out,
        ZoomCommand::FitOnScreen,
        ZoomCommand::FillScreen,
        ZoomCommand::ActualPixels,
        ZoomCommand::Double,
        ZoomCommand::ToSelection,
        ZoomCommand::PrintSize,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            ZoomCommand::In => "Zoom In",
            ZoomCommand::Out => "Zoom Out",
            ZoomCommand::FitOnScreen => "Fit on Screen",
            ZoomCommand::FillScreen => "Fill Screen",
            ZoomCommand::ActualPixels => "100%",
            ZoomCommand::Double => "200%",
            ZoomCommand::ToSelection => "Zoom to Selection",
            ZoomCommand::PrintSize => "Print Size",
        }
    }

    fn shortcut(self) -> Option<Shortcut> {
        Some(match self {
            ZoomCommand::In => Shortcut::ctrl_key(Key::Plus),
            ZoomCommand::Out => Shortcut::ctrl_key(Key::Minus),
            ZoomCommand::FitOnScreen => Shortcut::ctrl('0'),
            // The two framing commands share a key: fit, and fit-the-other-way.
            ZoomCommand::FillScreen => Shortcut::ctrl_shift('0'),
            ZoomCommand::ActualPixels => Shortcut::ctrl('1'),
            ZoomCommand::ToSelection => Shortcut::ctrl_alt('0'),
            ZoomCommand::PrintSize | ZoomCommand::Double => return None,
        })
    }
}

/// Which canvas (or selection) edge Layer ▸ Align moves the layers to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum AlignEdge {
    Left,
    HorizontalCenter,
    Right,
    Top,
    VerticalCenter,
    Bottom,
}

impl AlignEdge {
    pub const ALL: &'static [AlignEdge] = &[
        AlignEdge::Top,
        AlignEdge::VerticalCenter,
        AlignEdge::Bottom,
        AlignEdge::Left,
        AlignEdge::HorizontalCenter,
        AlignEdge::Right,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            AlignEdge::Left => "Left Edges",
            AlignEdge::HorizontalCenter => "Horizontal Centers",
            AlignEdge::Right => "Right Edges",
            AlignEdge::Top => "Top Edges",
            AlignEdge::VerticalCenter => "Vertical Centers",
            AlignEdge::Bottom => "Bottom Edges",
        }
    }

    /// Whether the edge moves layers along x (`true`) or y.
    pub const fn is_horizontal(self) -> bool {
        matches!(
            self,
            AlignEdge::Left | AlignEdge::HorizontalCenter | AlignEdge::Right
        )
    }
}

/// Which axis Layer ▸ Distribute spaces the selected layers along.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum DistributeAxis {
    Horizontal,
    Vertical,
}

impl DistributeAxis {
    pub const ALL: &'static [DistributeAxis] =
        &[DistributeAxis::Vertical, DistributeAxis::Horizontal];

    pub const fn label(self) -> &'static str {
        match self {
            DistributeAxis::Horizontal => "Horizontally",
            DistributeAxis::Vertical => "Vertically",
        }
    }
}

/// One of the four lock flags on a layer — Layer ▸ Lock ▸ ….
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum LayerLock {
    Pixels,
    Position,
    Transparency,
    All,
}

impl LayerLock {
    pub const ALL: &'static [LayerLock] = &[
        LayerLock::Transparency,
        LayerLock::Pixels,
        LayerLock::Position,
        LayerLock::All,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            LayerLock::Pixels => "Lock Image Pixels",
            LayerLock::Position => "Lock Position",
            LayerLock::Transparency => "Lock Transparent Pixels",
            LayerLock::All => "Lock All",
        }
    }

    /// Whether this flag is set in `state`.
    pub const fn is_set(self, state: LockState) -> bool {
        match self {
            LayerLock::Pixels => state.pixels,
            LayerLock::Position => state.position,
            LayerLock::Transparency => state.transparency,
            LayerLock::All => state.all,
        }
    }

    /// `state` with this flag flipped.
    pub const fn toggled(self, state: LockState) -> LockState {
        match self {
            LayerLock::Pixels => LockState {
                pixels: !state.pixels,
                ..state
            },
            LayerLock::Position => LockState {
                position: !state.position,
                ..state
            },
            LayerLock::Transparency => LockState {
                transparency: !state.transparency,
                ..state
            },
            LayerLock::All => LockState {
                all: !state.all,
                ..state
            },
        }
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
    CloseOthers,
    Save,
    SaveAs,
    /// File ▸ Save as PSD…: the layered PSD writer behind a `.psd` picker.
    /// Separate from [`MenuAction::Export`], whose formats are all flat.
    SaveAsPsd,
    Export(ExportFormat),
    ExportLayers,
    /// File ▸ Export ▸ Slices…: one file per Slice-tool region, written with
    /// the last confirmed Export As settings.
    ExportSlices,
    /// W8-C: File ▸ Export ▸ Artboards to Files…: one file per artboard, its
    /// own contents over its rect, written with the last confirmed Export As
    /// settings.
    ExportArtboards,
    PlaceEmbedded,
    PlaceLinked,
    FileInfo,
    Print,
    Quit,

    // ---- Edit ----------------------------------------------------------
    Undo,
    Redo,
    /// Edit ▸ Step Forward: Photoshop's alias for redo, drawn as its own row.
    StepForward,
    /// Edit ▸ Step Backward: Photoshop's alias for undo, drawn as its own row.
    StepBackward,
    Cut,
    Copy,
    CopyMerged,
    Paste,
    PasteInto,
    /// Edit ▸ Paste Special ▸ Paste in Place: the copied pixels land where
    /// they were copied from.
    PasteInPlace,
    /// Edit ▸ Paste Special ▸ Paste Outside: masked by the inverse of the
    /// selection.
    PasteOutside,
    /// Edit ▸ Purge ▸ …
    Purge(PurgeTarget),
    ClearPixels,
    FillDialog,
    StrokeDialog,
    FreeTransform,
    /// Edit ▸ Puppet Warp (W7-H): pins on a mesh over the layer's ink; the
    /// dialog's deformation lands on the active layer as one undo step.
    PuppetWarp,
    Transform(TransformOp),
    /// W7-I: Edit ▸ Content-Aware Scale ▸ one fixed step.
    ContentAwareScale(ContentAwareScaleStep),
    DefinePattern,
    DefineBrush,
    /// W10-A: Edit ▸ Define Custom Shape: the active shape layer's outline
    /// (else the Paths panel's current path) joins the Custom Shape library,
    /// persisted in the shape presets and listed in the Custom Shape tool's
    /// Shape picker.
    DefineCustomShape,
    KeyboardShortcuts,
    Preferences,

    // ---- Image ---------------------------------------------------------
    SetColorMode(ColorMode),
    SetBitDepth(ChannelDepth),
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
    /// W9-G: Layer ▸ Vector Mask.
    VectorMask(VectorMaskOp),
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
    /// Card 060: Layer ▸ Refine Mask… (a top-level Layer item beside the
    /// Layer Mask submenu) — the edge-refinement dialog over the active
    /// layer's mask.
    RefineMask,
    /// Card 062: Layer ▸ Remove Color Fringe… — the edge COLOUR cleanup
    /// dialog over the active layer's pixels near the mask boundary. A
    /// separate edit from [`MenuAction::RefineMask`]: it recolours, it never
    /// regrades coverage.
    RemoveColorFringe,
    ReleaseClippingMask,
    BlendingOptions,
    LayerStyle(EffectSlot),
    /// Card 067: capture the active layer's style block (style fields only).
    CopyLayerStyle,
    /// Card 067: paste the captured style onto the active layer — one
    /// undoable wholesale effects replace.
    PasteLayerStyle,
    /// Card 067: store the active layer's style as the next named preset
    /// (survives restart via the preset store).
    DefineStylePreset,
    /// Card 067: apply the most recently defined style preset — one
    /// undoable step.
    ApplyStylePreset,
    ClearLayerStyle,
    ConvertToSmartObject,
    /// Card 069: swap the active smart object's source file without redoing
    /// the layout — one undoable transaction covering tiles, transforms and
    /// the asset row for every layer sharing the asset.
    ReplaceContents,
    EditSmartObjectContents,
    CommitSmartObjectContents,
    Rasterize(RasterizeTarget),
    /// W9-F: Layer > Combine Shapes > ... over the selected shape layers.
    CombineShapes(ShapeCombine),
    /// W9-K: Layer ▸ Text ▸ Warp Text ▸ … over the active text layer.
    WarpText(WarpTextItem),
    /// W9-K: Layer ▸ Text ▸ Convert to Shape - the glyph outlines become a
    /// shape layer in the text layer's place.
    ConvertTextToShape,
    GroupLayers,
    UngroupLayers,
    ArrangeLayer(Arrange),
    MergeDown,
    MergeVisible,
    FlattenImage,
    ToggleLayerVisibility,
    /// Layer ▸ Align ▸ …: move the selected layers so the named edge meets the
    /// selection's (when there is one) or the canvas's. One undoable step.
    AlignLayers(AlignEdge),
    /// Layer ▸ Distribute ▸ …: space three or more selected layers evenly
    /// between the outermost two. One undoable step.
    DistributeLayers(DistributeAxis),
    /// Layer ▸ Lock ▸ …: flip one lock flag on the active layer. Resolves
    /// straight to a [`Command::SetLayerProperties`].
    LockLayer(LayerLock),
    /// Layer ▸ Rename Layer…: a name dialog over the active layer.
    RenameLayer,
    /// Layer ▸ Stamp Visible: composite every visible layer into a new layer
    /// above the active one. Ctrl+Alt+Shift+E.
    StampVisible,

    // ---- Select --------------------------------------------------------
    SelectAll,
    Deselect,
    Reselect,
    InverseSelection,
    SelectAllLayers,
    DeselectLayers,
    ColorRange,
    /// Select ▸ Subject (W10-K): the most salient region, found without a
    /// neural model — saliency seeds and a GrabCut graph cut
    /// (`selection::subject`), run on the job worker.
    SelectSubject,
    Modify(ModifySelection),
    GrowSelection,
    SimilarSelection,
    TransformSelection,
    SaveSelection,
    LoadSelection,
    /// Photoshop's "Edit in Quick Mask Mode": while on, pixel edits land in a
    /// scratch mask instead of the layer, and leaving converts that painted
    /// coverage into the document selection. `Q`.
    ToggleQuickMask,
    /// Select ▸ Refine Edge…: the Refine Mask pipeline run over the selection
    /// itself (selection → temporary coverage → dialog → selection), so it
    /// needs no layer mask. Lands as one [`Command::SetSelection`].
    RefineEdge,
    /// W9-A: Photopea's "Select Pixels" — a selection built from a layer's
    /// transparency (or, with `mask`, from its mask's coverage), combined
    /// with the live selection by `op`. A Ctrl+click on a layer thumbnail
    /// emits it for that row (Shift adds, Alt subtracts, Shift+Alt
    /// intersects); the layer-row context menu emits it with `layer: None`,
    /// which means the active layer. Lands as one [`Command::SetSelection`].
    SelectLayerPixels {
        layer: Option<LayerId>,
        mask: bool,
        op: crate::dialogs::LoadOperation,
    },

    // ---- Filter --------------------------------------------------------
    LastFilter,
    FilterGallery,
    /// Filter ▸ Liquify… (W7-H): brush warping through a displacement field;
    /// the dialog's warp lands on the active layer as one undo step.
    Liquify,
    /// Filter ▸ Blur Gallery ▸ <kind>… (W9-O): Field, Iris, Tilt-Shift, Path
    /// and Spin blur, edited with on-image handles over a bounded preview;
    /// the confirmed blur lands on the active layer as one undo step.
    BlurGallery(filters::blur_gallery::BlurGalleryKind),
    /// Filter ▸ Convert for Smart Filters (W7-E). Converts the active layer
    /// to a smart object, whose `layer_model::SmartObjectLayer::filters` stack
    /// then receives every filter applied to it instead of its pixels being
    /// rewritten. Greyed with [`SMART_FILTERS_ALREADY`] when the active layer
    /// already is one.
    ConvertForSmartFilters,
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
    /// View ▸ New Guide…: orientation and position, appended to the
    /// document's guide set as one [`Command::SetGuides`].
    NewGuide,
    /// View ▸ Clear Guides: empty the document's guide set. Disabled, with a
    /// reason, when there are none.
    ClearGuides,
    /// View ▸ Lock Guides: flip the document-level guide lock the canvas drag
    /// code honours (`ui::canvas` refuses to drag a guide while it is on).
    LockGuides,
    // W10-J: the rest of Photoshop's View menu and its keyboard chords.
    /// View > Snap To > All: every Snap To target on, as one step.
    SnapToAll,
    /// View > Snap To > None: every Snap To target off.
    SnapToNone,
    /// View > New Guide Layout...: columns, rows, gutters and margins as one
    /// [`Command::SetGuides`].
    NewGuideLayout,
    /// View > New Guides from Shape: guides at the active shape layer's
    /// bounds (edges and centres), one [`Command::SetGuides`].
    NewGuidesFromShape,
    /// Alt+Ctrl+T: duplicate the layer (or float a copy of the selection)
    /// and free-transform the copy. No menu row; the chord reaches it.
    DuplicateFreeTransform,
    /// Shift+[ / Shift+]: step the painting tool's hardness by 25%
    /// (`true` is harder). No menu row.
    BrushHardness(bool),
    /// The number keys: `1`..`9` set the painting tool's opacity to
    /// 10%..90%, `0` to 100%; two digits typed quickly are an exact value.
    /// Carries the digit. No menu row.
    ToolOpacity(u8),
    /// Edit > Content-Aware Scale > With Handles (Alt+Shift+Ctrl+C): the
    /// Free Transform box in its Content-Aware mode — drag the handles, set
    /// the Amount, Enter commits one seam-carved step.
    ContentAwareScaleFree,

    // ---- Window --------------------------------------------------------
    ApplyLayout(LayoutId),
    TogglePanel(PanelId),
    SetTheme(design::Theme),

    // ---- Help ----------------------------------------------------------
    Help,
    ReleaseNotes,
    /// Help ▸ Export Diagnostics…: write a local diagnostic bundle (app
    /// version, OS, the live GPU adapter, any panic lines).
    ExportDiagnostics,
    ReportIssue,
    About,

    // ---- W10-I: Layer additions ------------------------------------------
    /// Layer ▸ Smart Object ▸ Export Contents / New via Copy / Convert to
    /// Layers / Relink to File.
    SmartObject(SmartObjectOp),
    /// Layer ▸ Matting ▸ Remove Black / White Matte.
    Matting(MattingOp),
    /// Layer ▸ Hide Layers: every selected layer's eye off, one undo step.
    HideLayers,
    /// Layer ▸ Show Layers: every selected layer's eye on, one undo step.
    ShowLayers,
    /// Layer ▸ Link Layers: chain the selected layers (or unchain them when
    /// every one already is), one undo step.
    LinkLayers,
    /// Layer ▸ Smart Filter ▸ the filter-mask rows.
    SmartFilter(SmartFilterOp),

    // ---- W10-D: Filter > Vanishing Point ------------------------------------
    /// Filter ▸ Vanishing Point… (W10-D): define a perspective plane by its
    /// four corners, then clone-stamp or paste inside it with
    /// perspective-correct scaling; the confirmed edit lands on the active
    /// layer as one undo step.
    VanishingPoint,

    // ---- W10-H: Image > Apply Image / Calculations -------------------------
    /// Image ▸ Apply Image…: blend a source document/layer/channel into the
    /// active layer (mode, opacity, invert, mask), one undo step.
    ApplyImage,
    /// Image ▸ Calculations…: blend two sources' channels into a new channel
    /// (a saved selection), the selection, or a new document.
    Calculations,

    // ---- W10-G: the Edit gaps ---------------------------------------------
    /// Edit ▸ Preset Manager…: every stored brush, gradient, pattern, style
    /// and custom shape in one window — rename, delete, reorder, and import
    /// or export the whole library as JSON.
    PresetManager,
    /// Edit ▸ Fade…: opacity and blend mode for the last filter, adjustment,
    /// fill or stroke against the pixels it replaced, as one step replacing
    /// that one. Greyed, with the reason, when the last step is not one.
    Fade,
    /// Edit ▸ Auto-Align Layers…: lay every selected layer over the first by
    /// an estimated similarity (feature matching) or translation (phase
    /// correlation), as layer transforms in one undo step.
    AutoAlignLayers,
    /// Edit ▸ Auto-Blend Layers…: stitch (Panorama) or focus-stack (Stack)
    /// the selected layers into one new layer, one undo step.
    AutoBlendLayers,
    /// Edit ▸ Perspective Warp: draw quads over planes of the layer, drag
    /// their corners; one homography per quad, one undo step.
    PerspectiveWarp,

    // ---- W10-B: the Channels panel's alpha rows ------------------------------
    /// Open saved selection `index` as an editable alpha channel: its
    /// coverage becomes a hidden scratch layer's mask, targeted for painting
    /// and shown alone in grayscale. Emitted by the Channels panel's alpha
    /// row, and by no menu at all.
    EditAlphaChannel(usize),
    /// Close the alpha channel being edited, writing the painted coverage
    /// back into its saved selection. Emitted by the Channels panel.
    CloseAlphaChannel,

    // ---- W10-E: File automation, Variables, LUT / PDF export, Vectorize -----
    /// File ▸ Automate ▸ Batch…: play a recorded Action over every image in
    /// a folder and save each result to another folder, on the job worker.
    AutomateBatch,
    /// File ▸ Automate ▸ Convert Formats…: re-save every image in a folder
    /// in another format / quality / size, on the job worker.
    ConvertFormats,
    /// File ▸ Export ▸ Color Lookup Tables…: the visible adjustment-layer
    /// stack sampled on an identity lattice, written as a `.cube`.
    ExportColorLookup,
    /// File ▸ Export ▸ PDF…: the composite on a chosen page, as a raster PDF.
    ExportPdf,
    /// Image ▸ Variables ▸ Define…: bind text / visibility variables to
    /// layers.
    DefineVariables,
    /// Image ▸ Variables ▸ Data Sets…: import a CSV, preview a set, export
    /// one file per set.
    DataSets,
    /// Image ▸ Vectorize Bitmap…: the active layer's colours traced into
    /// shape layers, one undo step.
    VectorizeBitmap,

    // ---- W10-A ---------------------------------------------------------------
    /// File ▸ Export ▸ Slice Options…: the name, URL and alt text of the slice
    /// the Slice Select tool picked; the name is the file File ▸ Export ▸
    /// Slices writes it as, the URL and alt text go into the HTML page that
    /// export writes beside the images.
    SliceOptions,
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
    /// Every class, so a new one cannot ship without a thumbnail drawing — see
    /// the gate in [`crate::icons`].
    pub const ALL: [LayerClass; 7] = [
        LayerClass::Raster,
        LayerClass::Group,
        LayerClass::Adjustment,
        LayerClass::Text,
        LayerClass::Shape,
        LayerClass::SmartObject,
        LayerClass::Generator,
    ];

    pub fn of(kind: &LayerKind) -> Self {
        match kind {
            LayerKind::Raster(_) => LayerClass::Raster,
            LayerKind::Group(_) => LayerClass::Group,
            LayerKind::Adjustment(_) => LayerClass::Adjustment,
            LayerKind::Text(_) => LayerClass::Text,
            LayerKind::Shape(_) => LayerClass::Shape,
            LayerKind::SmartObject(_) => LayerClass::SmartObject,
            LayerKind::Generator(_) => LayerClass::Generator,
            // W9-B: a fill layer is edited the way an adjustment layer is -
            // through its dialog and Properties, never with a brush.
            LayerKind::Fill(_) => LayerClass::Adjustment,
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
    /// The layer has a PIXEL mask. W9-G: a vector-only mask (vector kind,
    /// no coverage tiles) is not one — Layer ▸ Layer Mask then adds a pixel
    /// mask beside it and never bakes coverage it does not have.
    pub has_mask: bool,
    pub mask_enabled: bool,
    /// W9-G: the layer carries a vector mask ([`layer_model::VectorMask`]).
    pub has_vector_mask: bool,
    pub vector_mask_enabled: bool,
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
            has_mask: layer.mask.as_ref().is_some_and(|m| {
                !(m.kind == layer_model::MaskKind::Vector
                    && m.vector.is_some()
                    && doc
                        .pixels
                        .tiles(editor_core::PixelKey::Mask(m.id))
                        .is_none_or(|t| t.is_empty()))
            }),
            mask_enabled: layer.mask.as_ref().is_some_and(|m| m.enabled),
            has_vector_mask: layer.mask.as_ref().is_some_and(|m| m.vector.is_some()),
            vector_mask_enabled: layer
                .mask
                .as_ref()
                .and_then(|m| m.vector.as_ref())
                .is_some_and(|v| v.enabled),
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
    /// W9-G: a vector path is current — the Paths panel's selected path or
    /// the pen's Work Path — so Layer ▸ Vector Mask ▸ Current Path has
    /// something to use. Filled by the application (the Paths panel state is
    /// the workspace's, not the document's).
    pub has_current_path: bool,
    /// The names of the recently opened files, most recent first, as the File
    /// menu should label them. The list's length *is* the recent-file count —
    /// there is deliberately no second counter to fall out of step with it.
    pub recent_files: Vec<String>,
    pub open_documents: usize,
    pub can_undo: bool,
    pub can_redo: bool,
    /// Some open document — not only the active one — has an undo or redo
    /// step, so Edit > Purge > Histories has something to drop.
    pub any_history: bool,
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
    /// The document's bits per channel, so Image ▸ Mode can tick it.
    pub bit_depth: ChannelDepth,
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
    /// The document's guide set, so View ▸ Clear Guides can say "there are
    /// none" and Lock Guides can both tick and flip the document-level lock
    /// without losing the guides themselves.
    pub guides: Guides,
    /// W10-G: the history label of the step Edit ▸ Fade would fade — the
    /// last step, when it was a filter, adjustment, fill or stroke on a pixel
    /// layer and nothing has happened since. `None` greys Fade out with
    /// [`FADE_NOTHING`]. Filled by the application, which keeps the pixels.
    pub fade_step: Option<String>,
    /// W10-B: a saved selection is open in the Channels panel as an editable
    /// alpha channel ([`layer_model::DocumentExtras::alpha_edit`]), so Close
    /// Alpha Channel has something to close.
    pub alpha_editing: bool,
    /// W10-I: the active layer is a smart object whose source is a LINKED
    /// file — the only kind Layer ▸ Smart Object ▸ Relink to File acts on.
    pub smart_object_linked: bool,
    /// W10-I: how many smart filters the active smart object carries, and
    /// its filter mask's switch (`None`: no filter mask) — what the Layer ▸
    /// Smart Filter rows are gated on.
    pub smart_filters: usize,
    pub filter_mask: Option<bool>,
}

/// W10-I: the active layer's smart object, if it is one.
fn active_smart_object(doc: &Document) -> Option<&layer_model::SmartObjectLayer> {
    match &doc.layers.get(doc.active_layer()?)?.kind {
        layer_model::LayerKind::SmartObject(so) => Some(so),
        _ => None,
    }
}

/// W10-I: why Relink to File is greyed over an embedded smart object.
pub const RELINK_EMBEDDED: &str =
    "The smart object is embedded, not linked: use Replace Contents to swap its source";

/// W10-B: why Close Alpha Channel is greyed.
pub const NO_ALPHA_CHANNEL_OPEN: &str = "No alpha channel is open for editing";

/// W10-G: why Edit ▸ Fade is greyed.
pub const FADE_NOTHING: &str =
    "Fade applies to the last filter, adjustment, fill or stroke, and the last step was not one";

impl Default for MenuContext {
    /// The state before a document is open: almost everything is disabled, and
    /// each disabled item still says why.
    fn default() -> Self {
        Self {
            has_document: false,
            is_dirty: false,
            has_path: false,
            has_current_path: false,
            recent_files: Vec::new(),
            open_documents: 0,
            can_undo: false,
            can_redo: false,
            any_history: false,
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
            bit_depth: ChannelDepth::Eight,
            view: ViewFlags::defaults(),
            view_rotated: false,
            ruler_unit: crate::dialogs::units::Unit::Pixels,
            dock: DockState::default(),
            theme: design::Theme::default(),
            guides: Guides::default(),
            fade_step: None,
            alpha_editing: false,
            smart_object_linked: false,
            smart_filters: 0,
            filter_mask: None,
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
            has_current_path: false,
            open_documents: 1,
            can_undo: history.can_undo(),
            can_redo: history.can_redo(),
            any_history: history.can_undo() || history.can_redo(),
            undo_label: history.undo_label().map(str::to_owned),
            redo_label: history.redo_label().map(str::to_owned),
            // Deliberately not `!selection.is_empty()`. `Selection::None`
            // answers `false` to `is_empty` — with nothing selected every pixel
            // is selected — so the naive spelling would enable Deselect on a
            // document that has never had a selection. See the documentation on
            // `Selection::is_empty`.
            has_selection: doc.selection.bounds().is_some(),
            layer_count: doc.layers.len(),
            // The document's multi-selection set, never smaller than the
            // active cursor: Distribute needs three, Align moves the set.
            selected_layers: doc
                .layer_selection()
                .len()
                .max(usize::from(active.is_some())),
            active,
            guides: doc.guides.clone(),
            // The Image ▸ Mode items read the document's own mode: the current
            // mode's item is checked, and the meta's u8 maps onto the ui
            // enum's discriminants (0 RGB, 1 Grayscale, 2 Lab, 3 CMYK,
            // 4 Indexed).
            color_mode: ColorMode::from_meta(doc.meta.color_mode),
            bit_depth: ChannelDepth::of_bits(doc.meta.bit_depth),
            alpha_editing: doc.extras.alpha_edit.is_some(),
            smart_object_linked: doc
                .active_layer()
                .and_then(|id| doc.layers.get(id))
                .is_some_and(|l| match &l.kind {
                    layer_model::LayerKind::SmartObject(so) => matches!(
                        doc.asset_origin(so.asset),
                        Some(layer_model::AssetOrigin::Linked { .. })
                    ),
                    _ => false,
                }),
            smart_filters: active_smart_object(doc).map_or(0, |so| so.filters.len()),
            filter_mask: active_smart_object(doc)
                .and_then(|so| so.filter_mask.as_ref())
                .map(|m| m.enabled),
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
        out.extend([
            MenuAction::CloseDocument,
            MenuAction::CloseOthers,
            MenuAction::CloseAll,
        ]);
        out.extend([MenuAction::Save, MenuAction::SaveAs, MenuAction::SaveAsPsd]);
        out.extend(ExportFormat::ALL.iter().copied().map(MenuAction::Export));
        out.extend([
            MenuAction::ExportLayers,
            MenuAction::ExportSlices,
            MenuAction::SliceOptions,
            MenuAction::ExportArtboards,
            MenuAction::PlaceEmbedded,
            MenuAction::PlaceLinked,
            MenuAction::FileInfo,
            MenuAction::Print,
            MenuAction::Quit,
            // ---- Edit ----
            MenuAction::Undo,
            MenuAction::Redo,
            MenuAction::StepForward,
            MenuAction::StepBackward,
            MenuAction::Cut,
            MenuAction::Copy,
            MenuAction::CopyMerged,
            MenuAction::Paste,
            MenuAction::PasteInto,
            MenuAction::PasteInPlace,
            MenuAction::PasteOutside,
            MenuAction::ClearPixels,
            MenuAction::FillDialog,
            MenuAction::StrokeDialog,
            MenuAction::FreeTransform,
            MenuAction::PuppetWarp,
        ]);
        out.extend(PurgeTarget::ALL.iter().copied().map(MenuAction::Purge));
        out.extend(TransformOp::ALL.iter().copied().map(MenuAction::Transform));
        out.extend(
            ContentAwareScaleStep::ALL
                .iter()
                .copied()
                .map(MenuAction::ContentAwareScale),
        );
        out.extend([
            MenuAction::DefinePattern,
            MenuAction::DefineBrush,
            MenuAction::DefineCustomShape,
            MenuAction::KeyboardShortcuts,
            MenuAction::Preferences,
        ]);
        // ---- Image ----
        out.extend(ColorMode::ALL.iter().copied().map(MenuAction::SetColorMode));
        out.extend(
            ChannelDepth::ALL
                .iter()
                .copied()
                .map(MenuAction::SetBitDepth),
        );
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
            // W10-H: Image ▸ Apply Image / Calculations.
            MenuAction::ApplyImage,
            MenuAction::Calculations,
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
            AdjustmentId::LAYERS
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
        out.extend(
            VectorMaskOp::ALL
                .iter()
                .copied()
                .map(MenuAction::VectorMask),
        );
        out.extend([
            MenuAction::EditAdjustmentLayer,
            MenuAction::CreateClippingMask,
            MenuAction::RefineMask,
            MenuAction::RemoveColorFringe,
            MenuAction::ReleaseClippingMask,
            MenuAction::BlendingOptions,
        ]);
        out.extend(EffectSlot::ALL.iter().copied().map(MenuAction::LayerStyle));
        out.push(MenuAction::CopyLayerStyle);
        out.push(MenuAction::PasteLayerStyle);
        out.push(MenuAction::DefineStylePreset);
        out.push(MenuAction::ApplyStylePreset);
        out.push(MenuAction::ClearLayerStyle);
        out.push(MenuAction::ConvertToSmartObject);
        out.push(MenuAction::ReplaceContents);
        out.push(MenuAction::EditSmartObjectContents);
        out.push(MenuAction::CommitSmartObjectContents);
        out.extend(
            RasterizeTarget::ALL
                .iter()
                .copied()
                .map(MenuAction::Rasterize),
        );
        out.extend(
            ShapeCombine::ALL
                .iter()
                .copied()
                .map(MenuAction::CombineShapes),
        );
        out.extend(WarpTextItem::ALL.iter().copied().map(MenuAction::WarpText));
        out.push(MenuAction::ConvertTextToShape);
        out.extend([MenuAction::GroupLayers, MenuAction::UngroupLayers]);
        out.extend(Arrange::ALL.iter().copied().map(MenuAction::ArrangeLayer));
        out.extend([
            MenuAction::MergeDown,
            MenuAction::MergeVisible,
            MenuAction::FlattenImage,
            // Emitted by the Layers panel's eye, and by no menu at all.
            MenuAction::ToggleLayerVisibility,
        ]);
        out.extend(AlignEdge::ALL.iter().copied().map(MenuAction::AlignLayers));
        out.extend(
            DistributeAxis::ALL
                .iter()
                .copied()
                .map(MenuAction::DistributeLayers),
        );
        out.extend(LayerLock::ALL.iter().copied().map(MenuAction::LockLayer));
        out.extend([
            MenuAction::RenameLayer,
            MenuAction::StampVisible,
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
            MenuAction::ToggleQuickMask,
            MenuAction::RefineEdge,
            // ---- Filter ----
            MenuAction::LastFilter,
            MenuAction::FilterGallery,
            MenuAction::Liquify,
            MenuAction::VanishingPoint,
            MenuAction::ConvertForSmartFilters,
        ]);
        out.extend(
            filters::blur_gallery::BlurGalleryKind::ALL
                .iter()
                .copied()
                .map(MenuAction::BlurGallery),
        );
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
        out.extend([
            MenuAction::NewGuide,
            MenuAction::ClearGuides,
            MenuAction::LockGuides,
            MenuAction::SnapToAll,
            MenuAction::SnapToNone,
            MenuAction::NewGuideLayout,
            MenuAction::NewGuidesFromShape,
            MenuAction::DuplicateFreeTransform,
            MenuAction::BrushHardness(false),
            MenuAction::BrushHardness(true),
        ]);
        out.extend((0..=9u8).map(MenuAction::ToolOpacity));
        out.push(MenuAction::ContentAwareScaleFree);
        // ---- Window ----
        out.extend(LayoutId::ALL.iter().copied().map(MenuAction::ApplyLayout));
        out.extend(PanelId::ALL.iter().copied().map(MenuAction::TogglePanel));
        out.extend(design::Theme::ALL.iter().copied().map(MenuAction::SetTheme));
        // ---- Help ----
        out.extend([
            MenuAction::Help,
            MenuAction::ReleaseNotes,
            MenuAction::ExportDiagnostics,
            MenuAction::ReportIssue,
            MenuAction::About,
        ]);
        // ---- W10-I: Layer additions ----
        out.extend(
            SmartObjectOp::ALL
                .iter()
                .copied()
                .map(MenuAction::SmartObject),
        );
        out.extend(MattingOp::ALL.iter().copied().map(MenuAction::Matting));
        out.extend([
            MenuAction::HideLayers,
            MenuAction::ShowLayers,
            MenuAction::LinkLayers,
        ]);
        out.extend(
            SmartFilterOp::ALL
                .iter()
                .copied()
                .map(MenuAction::SmartFilter),
        );
        // ---- W10-G: the Edit gaps ----
        out.extend([
            MenuAction::PresetManager,
            MenuAction::Fade,
            MenuAction::AutoAlignLayers,
            MenuAction::AutoBlendLayers,
            MenuAction::PerspectiveWarp,
        ]);
        // ---- W10-B: emitted by the Channels panel's alpha rows only ----
        out.extend([
            MenuAction::EditAlphaChannel(0),
            MenuAction::CloseAlphaChannel,
        ]);
        // ---- W10-E: File automation / export, Image > Variables, Vectorize ----
        out.extend([
            MenuAction::AutomateBatch,
            MenuAction::ConvertFormats,
            MenuAction::ExportColorLookup,
            MenuAction::ExportPdf,
            MenuAction::DefineVariables,
            MenuAction::DataSets,
            MenuAction::VectorizeBitmap,
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
            MenuAction::CloseOthers => "Close Others".into(),
            MenuAction::Save => "Save".into(),
            MenuAction::SaveAs => "Save As…".into(),
            MenuAction::SaveAsPsd => "Save as PSD…".into(),
            MenuAction::Export(f) => format!("{}…", f.extension().to_uppercase()),
            MenuAction::ExportLayers => "Export Layers…".into(),
            MenuAction::ExportSlices => "Slices…".into(),
            MenuAction::SliceOptions => "Slice Options…".into(),
            MenuAction::ExportArtboards => "Artboards to Files…".into(),
            // W10-E.
            MenuAction::AutomateBatch => "Batch…".into(),
            MenuAction::ConvertFormats => "Convert Formats…".into(),
            MenuAction::ExportColorLookup => "Color Lookup Tables…".into(),
            MenuAction::ExportPdf => "PDF…".into(),
            MenuAction::DefineVariables => "Define…".into(),
            MenuAction::DataSets => "Data Sets…".into(),
            MenuAction::VectorizeBitmap => "Vectorize Bitmap…".into(),
            MenuAction::PlaceEmbedded => "Place Embedded…".into(),
            MenuAction::PlaceLinked => "Place Linked…".into(),
            MenuAction::FileInfo => "File Info…".into(),
            MenuAction::Print => "Print…".into(),
            MenuAction::Quit => "Quit".into(),

            MenuAction::Undo => "Undo".into(),
            MenuAction::Redo => "Redo".into(),
            MenuAction::StepForward => "Step Forward".into(),
            MenuAction::StepBackward => "Step Backward".into(),
            MenuAction::Cut => "Cut".into(),
            MenuAction::Copy => "Copy".into(),
            MenuAction::CopyMerged => "Copy Merged".into(),
            MenuAction::Paste => "Paste".into(),
            MenuAction::PasteInto => "Paste Into".into(),
            MenuAction::PasteInPlace => "Paste in Place".into(),
            MenuAction::PasteOutside => "Paste Outside".into(),
            MenuAction::Purge(target) => target.label().into(),
            MenuAction::ClearPixels => "Clear".into(),
            MenuAction::FillDialog => "Fill…".into(),
            MenuAction::StrokeDialog => "Stroke…".into(),
            MenuAction::FreeTransform => "Free Transform".into(),
            MenuAction::Transform(t) => t.label().into(),
            MenuAction::ContentAwareScale(step) => step.label().into(),
            MenuAction::DefinePattern => "Define Pattern…".into(),
            MenuAction::DefineBrush => "Define Brush Preset…".into(),
            MenuAction::DefineCustomShape => "Define Custom Shape…".into(),
            MenuAction::KeyboardShortcuts => "Keyboard Shortcuts…".into(),
            MenuAction::Preferences => "Preferences…".into(),

            MenuAction::SetColorMode(m) => m.label().into(),
            MenuAction::SetBitDepth(d) => d.label().into(),
            // The ellipsis promises a dialog: Desaturate and Equalize ask
            // nothing, so their rows read bare, as Photopea's do (W4-E).
            MenuAction::ApplyAdjustment(a) if !a.has_dialog() => a.label().to_string(),
            MenuAction::ApplyAdjustment(a) => format!("{}…", a.label()),
            MenuAction::AutoTone => "Auto Tone".into(),
            MenuAction::AutoContrast => "Auto Contrast".into(),
            MenuAction::AutoColor => "Auto Color".into(),
            MenuAction::ApplyImage => "Apply Image…".into(),
            MenuAction::Calculations => "Calculations…".into(),
            MenuAction::ImageSize => "Image Size…".into(),
            MenuAction::CanvasSize => "Canvas Size…".into(),
            MenuAction::RotateCanvas(r) => r.label().into(),
            MenuAction::CropToSelection => "Crop".into(),
            MenuAction::Trim => "Trim…".into(),
            MenuAction::RevealAll => "Reveal All".into(),
            // No ellipsis: the duplicate is made at once, nothing is asked.
            MenuAction::DuplicateDocument => "Duplicate".into(),

            MenuAction::NewLayer => "Layer".into(),
            MenuAction::NewGroup => "Group".into(),
            MenuAction::NewFillLayer(k) => k.label().into(),
            MenuAction::NewAdjustmentLayer(a) => a.label().into(),
            MenuAction::LayerViaCopy => "Layer via Copy".into(),
            MenuAction::LayerViaCut => "Layer via Cut".into(),
            // W2-F: the ellipsis is earned — the row asks for the copy's name
            // (Photoshop's dialog, opening at "<name> copy") before it copies.
            MenuAction::DuplicateLayer => "Duplicate Layer…".into(),
            MenuAction::DeleteLayer => "Delete Layer".into(),
            MenuAction::Mask(m) => m.label().into(),
            MenuAction::VectorMask(m) => m.label().into(),
            MenuAction::EditAdjustmentLayer => "Edit Adjustment…".into(),
            MenuAction::CreateClippingMask => "Create Clipping Mask".into(),
            MenuAction::ReleaseClippingMask => "Release Clipping Mask".into(),
            MenuAction::BlendingOptions => "Blending Options…".into(),
            MenuAction::LayerStyle(s) => s.label().into(),
            MenuAction::ClearLayerStyle => "Clear Layer Style".into(),
            MenuAction::ConvertToSmartObject => "Convert to Smart Object".into(),
            MenuAction::ReplaceContents => "Replace Contents…".into(),
            MenuAction::EditSmartObjectContents => "Edit Contents…".into(),
            MenuAction::CommitSmartObjectContents => "Commit Contents".into(),
            MenuAction::Rasterize(t) => t.label().into(),
            MenuAction::CombineShapes(c) => c.label().into(),
            MenuAction::WarpText(item) => item.label(),
            MenuAction::ConvertTextToShape => "Convert to Shape".into(),
            MenuAction::GroupLayers => "Group Layers".into(),
            MenuAction::UngroupLayers => "Ungroup Layers".into(),
            MenuAction::ArrangeLayer(a) => a.label().into(),
            MenuAction::MergeDown => "Merge Down".into(),
            MenuAction::MergeVisible => "Merge Visible".into(),
            MenuAction::FlattenImage => "Flatten Image".into(),
            MenuAction::ToggleLayerVisibility => "Show / Hide Layer".into(),
            MenuAction::AlignLayers(edge) => edge.label().into(),
            MenuAction::DistributeLayers(axis) => axis.label().into(),
            MenuAction::LockLayer(lock) => lock.label().into(),
            MenuAction::RenameLayer => "Rename Layer…".into(),
            MenuAction::StampVisible => "Stamp Visible".into(),

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
            MenuAction::ToggleQuickMask => "Edit in Quick Mask Mode".into(),
            MenuAction::RefineEdge => "Refine Edge…".into(),
            // W9-A: Photopea's layer-row / thumbnail "Select Pixels".
            MenuAction::SelectLayerPixels { mask: false, .. } => "Select Pixels".into(),
            MenuAction::SelectLayerPixels { mask: true, .. } => "Select Mask Pixels".into(),

            MenuAction::LastFilter => "Last Filter".into(),
            MenuAction::FilterGallery => "Filter Gallery…".into(),
            MenuAction::Liquify => "Liquify…".into(),
            MenuAction::VanishingPoint => "Vanishing Point…".into(),
            MenuAction::BlurGallery(kind) => kind.label().into(),
            MenuAction::PuppetWarp => "Puppet Warp".into(),
            // W10-G: the Edit gaps.
            MenuAction::PresetManager => "Preset Manager…".into(),
            MenuAction::Fade => "Fade…".into(),
            MenuAction::AutoAlignLayers => "Auto-Align Layers…".into(),
            MenuAction::AutoBlendLayers => "Auto-Blend Layers…".into(),
            MenuAction::PerspectiveWarp => "Perspective Warp".into(),
            MenuAction::ConvertForSmartFilters => "Convert for Smart Filters".into(),
            MenuAction::RefineMask => "Refine Mask…".into(),
            MenuAction::RemoveColorFringe => "Remove Color Fringe…".into(),
            MenuAction::CopyLayerStyle => "Copy Layer Style".into(),
            MenuAction::PasteLayerStyle => "Paste Layer Style".into(),
            MenuAction::DefineStylePreset => "New Style Preset".into(),
            MenuAction::ApplyStylePreset => "Apply Style Preset…".into(),
            MenuAction::Filter(f) => f.label().into(),

            MenuAction::Zoom(z) => z.label().into(),
            MenuAction::ToggleView(f) => f.label().into(),
            MenuAction::ResetViewRotation => "Reset View Rotation".into(),
            MenuAction::SetRulerUnit(unit) => unit.label().into(),
            MenuAction::NewGuide => "New Guide…".into(),
            MenuAction::ClearGuides => "Clear Guides".into(),
            MenuAction::LockGuides => "Lock Guides".into(),
            MenuAction::SnapToAll => "All".into(),
            MenuAction::SnapToNone => "None".into(),
            MenuAction::NewGuideLayout => "New Guide Layout…".into(),
            MenuAction::NewGuidesFromShape => "New Guides from Shape".into(),
            MenuAction::DuplicateFreeTransform => "Free Transform a Copy".into(),
            MenuAction::BrushHardness(false) => "Softer Brush".into(),
            MenuAction::BrushHardness(true) => "Harder Brush".into(),
            MenuAction::ToolOpacity(digit) => {
                format!("Opacity {}%", opacity_of_digit(digit))
            }
            MenuAction::ContentAwareScaleFree => "With Handles".into(),

            MenuAction::ApplyLayout(l) => l.title().into(),
            MenuAction::TogglePanel(p) => p.title().into(),
            MenuAction::SetTheme(t) => t.name().into(),

            MenuAction::Help => "Raster Studio Help".into(),
            MenuAction::ReleaseNotes => "Release Notes".into(),
            MenuAction::ExportDiagnostics => "Export Diagnostics…".into(),
            MenuAction::ReportIssue => "Report an Issue".into(),
            MenuAction::About => "About Raster Studio".into(),
            // W10-B
            MenuAction::EditAlphaChannel(_) => "Edit Alpha Channel".into(),
            MenuAction::CloseAlphaChannel => "Close Alpha Channel".into(),
            // W10-I
            MenuAction::SmartObject(op) => op.label().into(),
            MenuAction::Matting(op) => op.label().into(),
            MenuAction::HideLayers => "Hide Layers".into(),
            MenuAction::ShowLayers => "Show Layers".into(),
            MenuAction::LinkLayers => "Link Layers".into(),
            MenuAction::SmartFilter(op) => op.label().into(),
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
            // W10-G: Edit > Fade names the step it fades ("Fade Apply
            // Invert..."), as the dialog's title does.
            MenuAction::Fade => match ctx.fade_step.as_deref() {
                Some(step) => {
                    let fade = self.label();
                    let word = fade.trim_end_matches('…');
                    format!("{word} {step}{}", &fade[word.len()..])
                }
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
            MenuAction::Liquify => Shortcut::ctrl_shift('x'),
            MenuAction::KeyboardShortcuts => Shortcut::ctrl_alt_shift('k'),
            MenuAction::Preferences => Shortcut::ctrl('k'),

            // W5-E: Photoshop's (and Photopea's) core adjustment chords.
            MenuAction::ApplyAdjustment(AdjustmentId::Levels) => Shortcut::ctrl('l'),
            MenuAction::ApplyAdjustment(AdjustmentId::Curves) => Shortcut::ctrl('m'),
            MenuAction::ApplyAdjustment(AdjustmentId::HueSaturation) => Shortcut::ctrl('u'),
            MenuAction::ApplyAdjustment(AdjustmentId::ColorBalance) => Shortcut::ctrl('b'),
            MenuAction::ApplyAdjustment(AdjustmentId::Invert) => Shortcut::ctrl('i'),
            MenuAction::ApplyAdjustment(AdjustmentId::Desaturate) => Shortcut::ctrl_shift('u'),
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
            MenuAction::StampVisible => Shortcut::ctrl_alt_shift('e'),
            // Step Forward/Backward paint no chord: the application keymap
            // already binds Ctrl+Shift+Z and Ctrl+Alt+Z to Redo/Undo, whose
            // menu twins are the Undo/Redo rows, and a chord painted beside
            // two rows is what `no_two_painted_menu_chords_disagree` refuses.
            MenuAction::SelectAll => Shortcut::ctrl('a'),
            MenuAction::Deselect => Shortcut::ctrl('d'),
            MenuAction::Reselect => Shortcut::ctrl_shift('d'),
            MenuAction::InverseSelection => Shortcut::ctrl_shift('i'),
            MenuAction::SelectAllLayers => Shortcut::ctrl_alt('a'),
            // Photopea's (and Photoshop's) Feather… chord: Shift+F6.
            MenuAction::Modify(ModifySelection::Feather) => Shortcut {
                shift: true,
                ..Shortcut::bare(Key::F(6))
            },
            MenuAction::ToggleQuickMask => Shortcut::bare(Key::character('q')),

            MenuAction::LastFilter => Shortcut::ctrl('f'),

            MenuAction::Zoom(z) => return z.shortcut(),
            MenuAction::ToggleView(ViewFlag::Rulers) => Shortcut::ctrl('r'),
            MenuAction::ToggleView(ViewFlag::Grid) => Shortcut::ctrl_key(Key::Quote),
            MenuAction::ToggleView(ViewFlag::Guides) => Shortcut::ctrl_key(Key::Semicolon),
            MenuAction::ToggleView(ViewFlag::Snap) => Shortcut::ctrl_shift_key(Key::Semicolon),
            // W10-J: Photoshop's (and Photopea's) remaining View / tool chords.
            MenuAction::ToggleView(ViewFlag::Extras) => Shortcut::ctrl('h'),
            MenuAction::DuplicateFreeTransform => Shortcut::ctrl_alt('t'),
            MenuAction::BrushHardness(harder) => Shortcut {
                shift: true,
                ..Shortcut::bare(if harder {
                    Key::RightBracket
                } else {
                    Key::LeftBracket
                })
            },
            MenuAction::ToolOpacity(digit) if digit <= 9 => {
                Shortcut::bare(Key::character(char::from(b'0' + digit)))
            }
            // Photoshop's Content-Aware Scale chord.
            MenuAction::ContentAwareScaleFree => Shortcut::ctrl_alt_shift('c'),

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
            MenuAction::SetBitDepth(depth) => ctx.bit_depth == depth,
            MenuAction::SetRulerUnit(unit) => ctx.ruler_unit == unit,
            MenuAction::ToggleLayerVisibility => ctx.active.map(|l| l.visible)?,
            MenuAction::LockLayer(lock) => lock.is_set(ctx.active?.locked),
            MenuAction::LockGuides => ctx.guides.locked,
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
            | MenuAction::ExportSlices
            // W10-A: the pick itself is checked where the dialog opens,
            // which says why when there is none.
            | MenuAction::SliceOptions
            | MenuAction::ExportArtboards
            | MenuAction::PlaceEmbedded
            | MenuAction::PlaceLinked
            | MenuAction::FileInfo
            | MenuAction::Print
            | MenuAction::SaveAsPsd
            | MenuAction::DuplicateDocument => gate(ctx.need_document(), act(self)),
            // W10-E: Batch / Convert Formats work on folders, not on the open
            // document; the exports, Variables and Vectorize need one.
            MenuAction::AutomateBatch | MenuAction::ConvertFormats => act(self),
            MenuAction::ExportColorLookup
            | MenuAction::ExportPdf
            | MenuAction::DefineVariables
            | MenuAction::DataSets => gate(ctx.need_document(), act(self)),
            MenuAction::VectorizeBitmap => match ctx.need_pixel_layer() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::CloseAll => gate(
                (ctx.open_documents == 0).then_some("No document is open"),
                act(self),
            ),
            MenuAction::CloseOthers => gate(
                (ctx.open_documents < 2).then_some("No other document is open"),
                act(self),
            ),
            MenuAction::Export(_) => gate(ctx.need_document(), act(self)),

            // ---- Edit ------------------------------------------------------
            MenuAction::Undo | MenuAction::StepBackward => {
                gate((!ctx.can_undo).then_some("Nothing to undo"), act(self))
            }
            MenuAction::Redo | MenuAction::StepForward => {
                gate((!ctx.can_redo).then_some("Nothing to redo"), act(self))
            }
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
            // Card 052: Paste Into is gated on the APPLICATION's own store —
            // an OS-clipboard image has no in-document origin yet, and masking
            // one by the selection is card 053's job. Enabling it on the
            // external payload alone would let the menu promise something the
            // perform cannot do.
            MenuAction::PasteInto => gate(
                ctx.need_document()
                    .or((!ctx.clipboard.has_internal_pixels()).then_some("The clipboard is empty"))
                    .or((!ctx.has_selection).then_some("Paste Into needs a selection")),
                act(self),
            ),
            // Paste in Place needs the copy's origin, which only the
            // application's own store records.
            MenuAction::PasteInPlace => gate(
                ctx.need_document()
                    .or((!ctx.clipboard.has_internal_pixels()).then_some("The clipboard is empty")),
                act(self),
            ),
            MenuAction::PasteOutside => gate(
                ctx.need_document()
                    .or((!ctx.clipboard.has_internal_pixels()).then_some("The clipboard is empty"))
                    .or((!ctx.has_selection).then_some("Paste Outside needs a selection")),
                act(self),
            ),
            MenuAction::Purge(target) => gate(target.unavailable_reason(ctx), act(self)),
            MenuAction::FillDialog
            | MenuAction::StrokeDialog
            | MenuAction::ContentAwareScale(_) => match ctx.need_editable_pixels() {
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
            // Defining a brush preset needs only a document: it captures the
            // live brush, which exists with no selection at all.
            MenuAction::DefineBrush => gate(ctx.need_document(), act(self)),
            // W10-A: an outline to define from - the active shape layer's,
            // else the Paths panel's current path.
            MenuAction::DefineCustomShape => gate(
                ctx.need_document().or_else(|| {
                    let shape = ctx.active.is_some_and(|l| l.class == LayerClass::Shape);
                    (!shape && !ctx.has_current_path)
                        .then_some("Define Custom Shape needs a shape layer or a path")
                }),
                act(self),
            ),
            MenuAction::KeyboardShortcuts | MenuAction::Preferences => act(self),

            // ---- Image -----------------------------------------------------
            MenuAction::SetColorMode(mode) => gate(
                ctx.need_document()
                    .or(mode.unsupported_reason())
                    .or(mode.conversion_reason(ctx.color_mode))
                    .or(mode.depth_reason(ctx.color_mode, ctx.bit_depth)),
                act(self),
            ),
            // W10-H: Apply Image writes the active layer's pixels;
            // Calculations only reads, so it needs a document.
            MenuAction::ApplyImage => match ctx.need_editable_pixels() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::Calculations => gate(ctx.need_document(), act(self)),
            MenuAction::SetBitDepth(depth) => gate(
                ctx.need_document()
                    .or(depth.conversion_reason(ctx.bit_depth))
                    .or(depth.mode_reason(ctx.color_mode)),
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
            // W10-B: the Channels panel's alpha rows.
            MenuAction::EditAlphaChannel(index) => match ctx.need_document() {
                Some(r) => Resolution::Disabled(r),
                None if index >= ctx.saved_selections => {
                    Resolution::Disabled("That alpha channel is no longer saved")
                }
                None => act(self),
            },
            MenuAction::CloseAlphaChannel => match ctx.need_document() {
                Some(r) => Resolution::Disabled(r),
                None if !ctx.alpha_editing => Resolution::Disabled(NO_ALPHA_CHANNEL_OPEN),
                None => act(self),
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
            MenuAction::VectorMask(op) => resolve_vector_mask(op, ctx),
            // Both of these *are* the Properties panel: an adjustment layer's
            // parameters and a layer's blending mode, opacity and effects are
            // all edited there, through `Intent::EditLayerKind` and
            // `Command::SetLayerProperties`. So the item's whole job is to put
            // that panel in front of the user — which is a `SetPanelOpen`, not
            // a separate window somebody still has to write. They resolved to
            // `Intent::Action` and nothing performed them for a whole release.
            //
            // Disabled when the panel is already open, because opening an open
            // panel is exactly the "menu item that does nothing" this module
            // exists to refuse. `EditAdjustmentLayer` is the exception: it is
            // the Properties panel's "Open editor…" surfacing as a menu item,
            // and the panel showing the adjustment editor is the point, not a
            // thing to refuse — so it stays enabled whenever the active layer
            // is an adjustment, and the bridge reveals the panel.
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
            // Photopea's Blending Options… opens the Layer Style dialog; the
            // chrome's dialog host opens it for this action, the same way it
            // does for every `LayerStyle(_)` row.
            MenuAction::BlendingOptions => match ctx.need_layer() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::LayerStyle(_) => match ctx.need_layer() {
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
            MenuAction::ReplaceContents => match ctx.need_layer() {
                Ok(l) if l.class == LayerClass::SmartObject => act(self),
                Ok(_) => Resolution::Disabled("The active layer is not a smart object"),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::EditSmartObjectContents => match ctx.need_layer() {
                Ok(l) if l.class == LayerClass::SmartObject => act(self),
                Ok(_) => Resolution::Disabled("The active layer is not a smart object"),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::CommitSmartObjectContents => gate(ctx.need_document(), act(self)),
            MenuAction::Rasterize(target) => resolve_rasterize(target, ctx),
            // W9-F: needs a shape layer active and at least one more layer
            // selected; the bridge checks every selected layer is a shape.
            MenuAction::CombineShapes(_) => match ctx.need_layer() {
                Ok(l) if l.class != LayerClass::Shape => {
                    Resolution::Disabled("The active layer is not a shape layer")
                }
                Ok(_) if ctx.selected_layers < 2 => {
                    Resolution::Disabled("Select two or more shape layers to combine")
                }
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::WarpText(_) | MenuAction::ConvertTextToShape => resolve_text_op(self, ctx),
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
            // Align moves the selected layers, so a position lock on the
            // active one refuses it the way Free Transform is refused.
            MenuAction::AlignLayers(_) => match ctx.need_layer() {
                Ok(l) if l.locked.blocks_transform() => {
                    Resolution::Disabled("The layer's position is locked")
                }
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::DistributeLayers(_) => gate(
                ctx.need_document()
                    .or((ctx.selected_layers < 3).then_some("Select three or more layers")),
                act(self),
            ),
            // A lock flag is one property patch; `LockState::all` refuses
            // every other patch, but a patch that touches only `locked` is
            // the one it allows, which is how a lock is released.
            MenuAction::LockLayer(lock) => match ctx.need_layer() {
                Ok(l) => cmd(Command::SetLayerProperties {
                    layer_id: l.id,
                    patch: LayerPatch {
                        locked: Some(lock.toggled(l.locked)),
                        ..Default::default()
                    },
                }),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::RenameLayer => match ctx.need_layer() {
                Ok(l) if l.locked.all => Resolution::Disabled("The layer is locked"),
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::StampVisible => gate(
                ctx.need_document()
                    .or((ctx.layer_count == 0).then_some("The document has no layers")),
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
            MenuAction::ToggleQuickMask => gate(ctx.need_document(), act(self)),
            MenuAction::RefineEdge => gate(ctx.need_selection(), act(self)),
            // W9-A: an explicit layer needs only a document (the bridge
            // refuses a layer that has gone); `None` means the active layer,
            // which must exist (and carry a mask for the mask variant).
            // Subtract and Intersect need a live selection to act on.
            MenuAction::SelectLayerPixels { layer, mask, op } => {
                let missing = match layer {
                    None => match ctx.need_layer() {
                        Ok(l) if mask && !l.has_mask => Some("The active layer has no mask"),
                        Ok(_) => None,
                        Err(r) => Some(r),
                    },
                    Some(_) => ctx.need_document(),
                };
                let missing = missing.or(match op {
                    crate::dialogs::LoadOperation::Subtract
                    | crate::dialogs::LoadOperation::Intersect => ctx.need_selection(),
                    _ => None,
                });
                gate(missing, act(self))
            }
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
            MenuAction::RefineMask => {
                // Card 060: the dialog refines the ACTIVE layer's mask — it
                // needs a layer with one, and says so when the gate holds it
                // back.
                let layer = match ctx.need_layer() {
                    Ok(l) => l,
                    Err(r) => return Resolution::Disabled(r),
                };
                gate(
                    (!layer.has_mask).then_some("The layer has no mask"),
                    act(MenuAction::RefineMask),
                )
            }
            MenuAction::CopyLayerStyle | MenuAction::DefineStylePreset => {
                // Card 067: both capture the ACTIVE layer's style block.
                match ctx.need_layer() {
                    Ok(_) => act(self),
                    Err(r) => Resolution::Disabled(r),
                }
            }
            MenuAction::PasteLayerStyle | MenuAction::ApplyStylePreset => {
                // Card 067: both replace the ACTIVE layer's style (one
                // undoable step); a missing capture/preset is a runtime
                // error the status bar reports, not a disabled item — the
                // menu cannot see the session clipboard.
                match ctx.need_layer() {
                    Ok(_) => act(self),
                    Err(r) => Resolution::Disabled(r),
                }
            }
            MenuAction::RemoveColorFringe => {
                // Card 062: the fringe cleanup samples the ACTIVE layer's
                // mask boundary — same gate as RefineMask, a different edit.
                let layer = match ctx.need_layer() {
                    Ok(l) => l,
                    Err(r) => return Resolution::Disabled(r),
                };
                gate(
                    (!layer.has_mask).then_some("The layer has no mask"),
                    act(MenuAction::RemoveColorFringe),
                )
            }
            MenuAction::ConvertForSmartFilters => match ctx.need_layer() {
                Ok(l) if l.class == LayerClass::SmartObject => {
                    Resolution::Disabled(SMART_FILTERS_ALREADY)
                }
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            // W7-E: a filter on a smart object is appended to its smart-filter
            // stack rather than written into pixels, so the pixel gate does
            // not apply; the blanket lock (which refuses the stack edit) does.
            MenuAction::Filter(_)
                if ctx
                    .need_layer()
                    .is_ok_and(|l| l.class == LayerClass::SmartObject) =>
            {
                gate(
                    ctx.active
                        .is_some_and(|l| l.locked.all)
                        .then_some("The layer is locked"),
                    act(self),
                )
            }
            MenuAction::FilterGallery
            | MenuAction::Filter(_)
            | MenuAction::Liquify
            | MenuAction::VanishingPoint
            | MenuAction::BlurGallery(_)
            | MenuAction::PuppetWarp => match ctx.need_editable_pixels() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },

            // ---- W10-G: the Edit gaps -----------------------------------------
            // The preset store is the application's, not a document's.
            MenuAction::PresetManager => act(self),
            MenuAction::Fade => gate(
                ctx.need_document()
                    .or(ctx.fade_step.is_none().then_some(FADE_NOTHING)),
                act(self),
            ),
            MenuAction::AutoAlignLayers | MenuAction::AutoBlendLayers => gate(
                ctx.need_document().or((ctx.selected_layers < 2)
                    .then_some("Select two or more layers in the Layers panel first")),
                act(self),
            ),
            MenuAction::PerspectiveWarp => match ctx.need_editable_pixels() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },

            // ---- View ------------------------------------------------------
            // Zoom to Selection with nothing selected is disabled and says so,
            // rather than being an item that looks live and then no-ops.
            MenuAction::Zoom(ZoomCommand::ToSelection) => gate(
                ctx.need_document()
                    .or((!ctx.has_selection).then_some("Nothing is selected")),
                act(self),
            ),
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
            MenuAction::NewGuide => gate(ctx.need_document(), act(self)),
            MenuAction::ClearGuides => gate(
                ctx.need_document().or(ctx
                    .guides
                    .list
                    .is_empty()
                    .then_some("There are no guides to clear")),
                cmd(Command::SetGuides {
                    guides: Guides {
                        list: Vec::new(),
                        ..ctx.guides.clone()
                    },
                }),
            ),
            MenuAction::LockGuides => gate(
                ctx.need_document(),
                cmd(Command::SetGuides {
                    guides: Guides {
                        locked: !ctx.guides.locked,
                        ..ctx.guides.clone()
                    },
                }),
            ),
            // W10-J: Snap To > All / None set the five targets at once; a
            // click that would change nothing is greyed with the reason.
            MenuAction::SnapToAll => gate(
                ctx.need_document().or(ViewFlag::SNAP_TO
                    .iter()
                    .all(|f| ctx.view.get(*f))
                    .then_some("Every snap target is already on")),
                act(self),
            ),
            MenuAction::SnapToNone => gate(
                ctx.need_document()
                    .or((!ViewFlag::SNAP_TO.iter().any(|f| ctx.view.get(*f)))
                        .then_some("No snap target is on")),
                act(self),
            ),
            MenuAction::NewGuideLayout => gate(ctx.need_document(), act(self)),
            MenuAction::NewGuidesFromShape => match ctx.need_layer() {
                Ok(l) if l.class == LayerClass::Shape => act(self),
                Ok(_) => Resolution::Disabled("The active layer is not a shape layer"),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::DuplicateFreeTransform => match ctx.need_layer() {
                Ok(l) if l.locked.blocks_transform() => {
                    Resolution::Disabled("The layer's position is locked")
                }
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            // The painting tool's own settings: no document needed, and the
            // application refuses (with the reason) a tool that has no brush.
            MenuAction::BrushHardness(_) => act(self),
            MenuAction::ToolOpacity(digit) => gate(
                (digit > 9).then_some("Opacity keys are the digits 0 to 9"),
                act(self),
            ),
            MenuAction::ContentAwareScaleFree => match ctx.need_editable_pixels() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },

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
            | MenuAction::ExportDiagnostics
            | MenuAction::ReportIssue
            | MenuAction::About => act(self),

            // ---- W10-I: Layer additions ------------------------------------
            MenuAction::SmartObject(op) => match ctx.need_layer() {
                Ok(l) if l.class == LayerClass::SmartObject => gate(
                    (op == SmartObjectOp::RelinkToFile && !ctx.smart_object_linked)
                        .then_some(RELINK_EMBEDDED),
                    act(self),
                ),
                Ok(_) => Resolution::Disabled("The active layer is not a smart object"),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::SmartFilter(op) => match ctx.need_layer() {
                Ok(l) if l.class != LayerClass::SmartObject => {
                    Resolution::Disabled("The active layer is not a smart object")
                }
                Ok(_) => match op {
                    SmartFilterOp::AddMask if ctx.smart_filters == 0 => {
                        Resolution::Disabled(NO_SMART_FILTERS)
                    }
                    SmartFilterOp::AddMask if ctx.filter_mask.is_some() => {
                        Resolution::Disabled(FILTER_MASK_EXISTS)
                    }
                    SmartFilterOp::AddMask => act(self),
                    _ if ctx.filter_mask.is_none() => Resolution::Disabled(NO_FILTER_MASK),
                    _ => act(self),
                },
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::Matting(_) => match ctx.need_editable_pixels() {
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::HideLayers => match ctx.need_layer() {
                Ok(l) if !l.visible && ctx.selected_layers <= 1 => {
                    Resolution::Disabled("The layer is already hidden")
                }
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::ShowLayers => match ctx.need_layer() {
                Ok(l) if l.visible && ctx.selected_layers <= 1 => {
                    Resolution::Disabled("The layer is already showing")
                }
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
            MenuAction::LinkLayers => match ctx.need_layer() {
                Ok(_) if ctx.selected_layers < 2 => {
                    Resolution::Disabled("Select two or more layers to link")
                }
                Ok(_) => act(self),
                Err(r) => Resolution::Disabled(r),
            },
        }
    }
}

/// W9-G: Layer ▸ Vector Mask. The menu gates; the application performs —
/// a vector mask is attached, edited or removed by one undoable
/// `SetLayerProperties` the application builds from the layer's current mask
/// (and, for Current Path, from the Paths panel it owns).
fn resolve_vector_mask(op: VectorMaskOp, ctx: &MenuContext) -> Resolution {
    let layer = match ctx.need_layer() {
        Ok(l) => l,
        Err(r) => return Resolution::Disabled(r),
    };
    if layer.locked.all {
        return Resolution::Disabled("The layer is locked");
    }
    if op.creates() && layer.has_vector_mask {
        return Resolution::Disabled("The layer already has a vector mask");
    }
    if !op.creates() && !layer.has_vector_mask {
        return Resolution::Disabled("The layer has no vector mask");
    }
    if op == VectorMaskOp::CurrentPath && !ctx.has_current_path {
        return Resolution::Disabled("There is no path; draw one or select it in the Paths panel");
    }
    act(MenuAction::VectorMask(op))
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
        // Card 057: the four creation ops carry REAL coverage, and coverage
        // is pixels the application rasterises (the tile store lives there).
        // The menu gates (layer, existing mask, selection); the app attaches
        // mask + coverage atomically in one undoable transaction.
        MaskOp::RevealAll | MaskOp::HideAll | MaskOp::RevealSelection | MaskOp::HideSelection => {
            act(MenuAction::Mask(op))
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
        // Card 058: inverting coverage is a pixel edit on the mask's tile
        // map — back to the application like the other coverage ops.
        MaskOp::Invert => act(MenuAction::Mask(op)),
        MaskOp::Toggle | MaskOp::ToggleLink => act(MenuAction::Mask(op)),
    }
}

/// W9-K: the Layer ▸ Text rows act on an unlocked text layer; the edit
/// itself needs the whole payload, so it runs in the shell.
fn resolve_text_op(action: MenuAction, ctx: &MenuContext) -> Resolution {
    match ctx.need_layer() {
        Ok(l) if l.class != LayerClass::Text => {
            Resolution::Disabled("The active layer is not a text layer")
        }
        Ok(l) if l.locked.all => Resolution::Disabled("The layer is locked"),
        Ok(_) => act(action),
        Err(r) => Resolution::Disabled(r),
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

/// W10-J: the opacity percent one number key means on its own — `1`..`9`
/// are 10%..90% and `0` is 100%, Photoshop's table.
pub const fn opacity_of_digit(digit: u8) -> u8 {
    match digit {
        0 => 100,
        d if d <= 9 => d * 10,
        _ => 100,
    }
}

/// W10-J: the opacity two number keys typed quickly mean together — the
/// exact percent they spell (`5` `5` is 55%, `0` `5` is 5%, `0` `0` is 0%),
/// Photoshop's second-digit rule.
pub const fn opacity_of_two_digits(first: u8, second: u8) -> u8 {
    let v = (first as u16) * 10 + second as u16;
    if v > 100 {
        100
    } else {
        v as u8
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
            item(MenuAction::CloseOthers),
            item(MenuAction::CloseAll),
            Entry::Separator,
            item(MenuAction::Save),
            item(MenuAction::SaveAs),
            item(MenuAction::SaveAsPsd),
            Entry::Separator,
            Entry::submenu(
                "Export As",
                ExportFormat::ALL
                    .iter()
                    .map(|f| item(MenuAction::Export(*f)))
                    .collect(),
            ),
            item(MenuAction::ExportLayers),
            Entry::submenu(
                "Export",
                vec![
                    item(MenuAction::ExportSlices),
                    // W10-A.
                    item(MenuAction::SliceOptions),
                    item(MenuAction::ExportArtboards),
                    // W10-E.
                    item(MenuAction::ExportColorLookup),
                    item(MenuAction::ExportPdf),
                ],
            ),
            // W10-E: File > Automate.
            Entry::submenu(
                "Automate",
                vec![
                    item(MenuAction::AutomateBatch),
                    item(MenuAction::ConvertFormats),
                ],
            ),
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
            item(MenuAction::StepForward),
            item(MenuAction::StepBackward),
            // W10-G: Fade sits under the steps it fades, as in Photoshop.
            item(MenuAction::Fade),
            Entry::Separator,
            item(MenuAction::Cut),
            item(MenuAction::Copy),
            item(MenuAction::CopyMerged),
            item(MenuAction::Paste),
            Entry::submenu(
                "Paste Special",
                vec![
                    item(MenuAction::PasteInPlace),
                    item(MenuAction::PasteInto),
                    item(MenuAction::PasteOutside),
                ],
            ),
            item(MenuAction::ClearPixels),
            Entry::Separator,
            item(MenuAction::FillDialog),
            item(MenuAction::StrokeDialog),
            Entry::Separator,
            item(MenuAction::FreeTransform),
            item(MenuAction::PuppetWarp),
            // W10-G
            item(MenuAction::PerspectiveWarp),
            Entry::submenu("Transform", items(TransformOp::ALL, MenuAction::Transform)),
            Entry::submenu("Content-Aware Scale", {
                // W10-J: the interactive box first, the fixed steps after.
                let mut rows = vec![item(MenuAction::ContentAwareScaleFree), Entry::Separator];
                rows.extend(items(
                    ContentAwareScaleStep::ALL,
                    MenuAction::ContentAwareScale,
                ));
                rows
            }),
            // W10-G
            item(MenuAction::AutoAlignLayers),
            item(MenuAction::AutoBlendLayers),
            Entry::Separator,
            item(MenuAction::DefinePattern),
            item(MenuAction::DefineBrush),
            item(MenuAction::DefineCustomShape),
            // W10-G
            item(MenuAction::PresetManager),
            Entry::Separator,
            Entry::submenu("Purge", items(PurgeTarget::ALL, MenuAction::Purge)),
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
            Entry::submenu("Mode", {
                let mut e = items(ColorMode::ALL, MenuAction::SetColorMode);
                e.push(Entry::Separator);
                e.extend(items(ChannelDepth::ALL, MenuAction::SetBitDepth));
                e
            }),
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
            // W10-H
            item(MenuAction::ApplyImage),
            item(MenuAction::Calculations),
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
            // W10-E: Image > Variables and Image > Vectorize Bitmap.
            Entry::Separator,
            Entry::submenu(
                "Variables",
                vec![
                    item(MenuAction::DefineVariables),
                    item(MenuAction::DataSets),
                ],
            ),
            item(MenuAction::VectorizeBitmap),
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
                items(AdjustmentId::LAYERS, MenuAction::NewAdjustmentLayer),
            ),
            item(MenuAction::EditAdjustmentLayer),
            item(MenuAction::DuplicateLayer),
            item(MenuAction::DeleteLayer),
            item(MenuAction::RenameLayer),
            Entry::submenu("Lock", items(LayerLock::ALL, MenuAction::LockLayer)),
            // W10-I: Photopea's Hide Layers / Show Layers.
            item(MenuAction::HideLayers),
            item(MenuAction::ShowLayers),
            Entry::Separator,
            Entry::submenu("Layer Mask", items(MaskOp::ALL, MenuAction::Mask)),
            Entry::submenu(
                "Vector Mask",
                items(VectorMaskOp::ALL, MenuAction::VectorMask),
            ),
            item(MenuAction::RefineMask),
            item(MenuAction::RemoveColorFringe),
            // W10-I: Layer ▸ Matting.
            Entry::submenu("Matting", items(MattingOp::ALL, MenuAction::Matting)),
            item(MenuAction::CreateClippingMask),
            item(MenuAction::ReleaseClippingMask),
            Entry::Separator,
            Entry::submenu("Layer Style", {
                let mut e = vec![item(MenuAction::BlendingOptions), Entry::Separator];
                e.extend(items(EffectSlot::ALL, MenuAction::LayerStyle));
                e.push(Entry::Separator);
                e.push(item(MenuAction::CopyLayerStyle));
                e.push(item(MenuAction::PasteLayerStyle));
                e.push(item(MenuAction::DefineStylePreset));
                e.push(item(MenuAction::ApplyStylePreset));
                e.push(Entry::Separator);
                e.push(item(MenuAction::ClearLayerStyle));
                e
            }),
            item(MenuAction::ConvertToSmartObject),
            Entry::submenu(
                "Smart Object",
                vec![
                    item(MenuAction::EditSmartObjectContents),
                    item(MenuAction::ReplaceContents),
                    item(MenuAction::CommitSmartObjectContents),
                    // W10-I: the rest of Photopea's Smart Object submenu.
                    Entry::Separator,
                    item(MenuAction::SmartObject(SmartObjectOp::NewViaCopy)),
                    item(MenuAction::SmartObject(SmartObjectOp::ExportContents)),
                    item(MenuAction::SmartObject(SmartObjectOp::RelinkToFile)),
                    item(MenuAction::SmartObject(SmartObjectOp::ConvertToLayers)),
                ],
            ),
            // W10-I: Layer ▸ Smart Filter — the filters' shared mask.
            Entry::submenu(
                "Smart Filter",
                items(SmartFilterOp::ALL, MenuAction::SmartFilter),
            ),
            Entry::submenu(
                "Rasterize",
                items(RasterizeTarget::ALL, MenuAction::Rasterize),
            ),
            Entry::submenu(
                "Combine Shapes",
                items(ShapeCombine::ALL, MenuAction::CombineShapes),
            ),
            // W9-K: Layer ▸ Text.
            Entry::submenu(
                "Text",
                vec![
                    item(MenuAction::WarpText(WarpTextItem::Dialog)),
                    Entry::submenu("Warp Style", {
                        let mut e = vec![item(MenuAction::WarpText(WarpTextItem::ALL[1]))];
                        e.push(Entry::Separator);
                        e.extend(items(&WarpTextItem::ALL[2..], MenuAction::WarpText));
                        e
                    }),
                    item(MenuAction::ConvertTextToShape),
                ],
            ),
            Entry::Separator,
            item(MenuAction::GroupLayers),
            item(MenuAction::UngroupLayers),
            item(MenuAction::LinkLayers),
            Entry::submenu("Arrange", items(Arrange::ALL, MenuAction::ArrangeLayer)),
            Entry::submenu("Align", items(AlignEdge::ALL, MenuAction::AlignLayers)),
            Entry::submenu(
                "Distribute",
                items(DistributeAxis::ALL, MenuAction::DistributeLayers),
            ),
            Entry::Separator,
            item(MenuAction::MergeDown),
            item(MenuAction::MergeVisible),
            item(MenuAction::StampVisible),
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
            // W10-K: back as a classical pipeline (saliency + GrabCut, no
            // model); see `selection::subject` for what it can and cannot see.
            item(MenuAction::SelectSubject),
            Entry::Separator,
            Entry::submenu("Modify", items(ModifySelection::ALL, MenuAction::Modify)),
            item(MenuAction::GrowSelection),
            item(MenuAction::SimilarSelection),
            item(MenuAction::RefineEdge),
            Entry::Separator,
            item(MenuAction::TransformSelection),
            Entry::Separator,
            item(MenuAction::SaveSelection),
            item(MenuAction::LoadSelection),
            item(MenuAction::ToggleQuickMask),
        ],
    }
}

/// Why Filter ▸ Convert for Smart Filters is greyed out over a smart object:
/// there is nothing to convert, and every filter applied to it already lands
/// in its smart-filter stack.
pub const SMART_FILTERS_ALREADY: &str =
    "The layer is already a smart object: filters applied to it are smart filters";

/// W10-D: Filter ▸ Vanishing Point's name, shared by the menu row and the
/// dialog title.
pub const VANISHING_POINT_TITLE: &str = "Vanishing Point";

fn filter_menu() -> Menu {
    let mut entries = vec![
        item(MenuAction::LastFilter),
        Entry::Separator,
        item(MenuAction::FilterGallery),
        item(MenuAction::Filter(FilterId::CameraRaw)),
        item(MenuAction::Filter(FilterId::LensCorrection)),
        item(MenuAction::Liquify),
        item(MenuAction::VanishingPoint),
        Entry::submenu(
            "Blur Gallery",
            items(
                &filters::blur_gallery::BlurGalleryKind::ALL,
                MenuAction::BlurGallery,
            ),
        ),
        Entry::Separator,
        item(MenuAction::ConvertForSmartFilters),
        Entry::Separator,
    ];
    for group in FilterGroup::ALL {
        entries.push(Entry::submenu(
            group.label(),
            FilterId::ALL
                .iter()
                .filter(|f| f.group() == *group && !f.is_top_level())
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
    // W10-J: the Show and Snap To flags have submenus of their own below.
    entries.extend(
        ViewFlag::ALL
            .iter()
            .filter(|f| !f.in_submenu())
            .map(|f| item(MenuAction::ToggleView(*f))),
    );
    entries.push(Entry::submenu(
        "Show",
        vec![item(MenuAction::ToggleView(ViewFlag::Slices))],
    ));
    let mut snap_to = items(ViewFlag::SNAP_TO, MenuAction::ToggleView);
    snap_to.push(Entry::Separator);
    snap_to.push(item(MenuAction::SnapToAll));
    snap_to.push(item(MenuAction::SnapToNone));
    entries.push(Entry::submenu("Snap To", snap_to));
    // Guides are a document feature (persisted, undoable through
    // `Command::SetGuides`), so their rows sit beside the Guides overlay
    // toggle rather than in a document menu.
    entries.push(Entry::Separator);
    entries.push(item(MenuAction::NewGuide));
    entries.push(item(MenuAction::NewGuideLayout));
    entries.push(item(MenuAction::NewGuidesFromShape));
    entries.push(item(MenuAction::ClearGuides));
    entries.push(item(MenuAction::LockGuides));
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
            item(MenuAction::ExportDiagnostics),
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

    /// `view::ids::menu_item` keys a drawn row by its action alone, and two
    /// rows sharing an egui id is a real fault — egui paints an "id clash"
    /// warning over them. One menu is open at a time, so the rule that has to
    /// hold is *per menu*, not across the bar: `Keyboard Shortcuts…` is
    /// deliberately reachable from both Edit and Help, and those two rows are
    /// never on screen together.
    #[test]
    fn no_action_is_listed_twice_within_one_menu() {
        for recent in 0..=MAX_RECENT_FILES {
            for menu in menu_bar(recent) {
                let listed = menu.actions();
                let unique: HashSet<MenuAction> = listed.iter().copied().collect();
                assert_eq!(
                    unique.len(),
                    listed.len(),
                    "the {} menu draws a row twice with {recent} recent files",
                    menu.title
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
                    external_pixels: false,
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
                external_pixels: false,
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

    /// W7-E: Filter ▸ Convert for Smart Filters is in the Filter menu, live
    /// over any layer, greyed with its reason over a smart object (which is
    /// already converted) and without a layer. Over a smart object every
    /// Filter row stays live, because the filter lands in its stack.
    #[test]
    fn convert_for_smart_filters_is_live_over_a_layer_and_greyed_over_a_smart_object() {
        assert!(filter_menu()
            .actions()
            .contains(&MenuAction::ConvertForSmartFilters));
        assert!(MenuAction::all().contains(&MenuAction::ConvertForSmartFilters));
        assert_eq!(
            MenuAction::ConvertForSmartFilters.label(),
            "Convert for Smart Filters"
        );
        let (mut doc, group, inside, _b) = stacked_document();
        for ctx in [ctx_with_layer(&doc, inside), ctx_with_layer(&doc, group)] {
            assert!(MenuAction::ConvertForSmartFilters
                .resolve(&ctx)
                .is_enabled());
        }
        assert!(!MenuAction::ConvertForSmartFilters
            .resolve(&MenuContext::default())
            .is_enabled());

        let smart = doc
            .layers
            .insert_at(
                Layer::with_kind(
                    "Smart",
                    layer_model::LayerKind::SmartObject(layer_model::SmartObjectLayer {
                        asset: layer_model::AssetId::new(),
                        linked: false,
                        filters: Vec::new(),
                        filter_mask: None,
                    }),
                ),
                None,
                0,
            )
            .unwrap();
        let ctx = ctx_with_layer(&doc, smart);
        assert_eq!(
            MenuAction::ConvertForSmartFilters.resolve(&ctx).reason(),
            Some(SMART_FILTERS_ALREADY)
        );
        for id in [FilterId::GaussianBlur, FilterId::Mosaic] {
            assert!(
                MenuAction::Filter(id).resolve(&ctx).is_enabled(),
                "{id:?} must be live over a smart object"
            );
        }
        // The Filter Gallery still edits pixels, so it stays greyed there.
        assert!(!MenuAction::FilterGallery.resolve(&ctx).is_enabled());
        // The blanket lock refuses the stack edit, so it greys the rows.
        doc.layers.get_mut(smart).unwrap().locked.all = true;
        assert!(!MenuAction::Filter(FilterId::GaussianBlur)
            .resolve(&ctx_with_layer(&doc, smart))
            .is_enabled());
    }

    /// W10-C: Camera Raw and Lens Correction are rows of the Filter menu
    /// itself (and of no submenu); Lighting Effects is under Render and
    /// HSB/HSL under Other, each once.
    #[test]
    fn w10c_filter_rows_sit_where_photopea_puts_them() {
        let menu = filter_menu();
        let top: Vec<MenuAction> = menu
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::Item(a) => Some(*a),
                _ => None,
            })
            .collect();
        for id in [FilterId::CameraRaw, FilterId::LensCorrection] {
            assert!(id.is_top_level());
            assert!(
                top.contains(&MenuAction::Filter(id)),
                "{id:?} is not a top-level row"
            );
        }
        let submenu = |label: &str| -> Vec<MenuAction> {
            menu.entries
                .iter()
                .find_map(|e| match e {
                    Entry::Submenu { label: l, entries } if *l == label => Some(
                        entries
                            .iter()
                            .filter_map(|e| match e {
                                Entry::Item(a) => Some(*a),
                                _ => None,
                            })
                            .collect(),
                    ),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no {label} submenu"))
        };
        assert!(submenu("Render").contains(&MenuAction::Filter(FilterId::LightingEffects)));
        assert!(submenu("Other").contains(&MenuAction::Filter(FilterId::HsbHsl)));
        assert!(!submenu("Other").contains(&MenuAction::Filter(FilterId::CameraRaw)));
        assert_eq!(FilterId::CameraRaw.label(), "Camera Raw…");
        assert_eq!(FilterId::LensCorrection.label(), "Lens Correction…");
        assert_eq!(FilterId::LightingEffects.label(), "Lighting Effects…");
        assert_eq!(FilterId::HsbHsl.label(), "HSB/HSL…");
    }

    /// The Photopea rows the parity audit found missing are in the submenu
    /// Photopea files them under, and parameterless ones apply without the
    /// ellipsis that promises a dialog.
    #[test]
    fn the_photopea_parity_filters_sit_in_their_submenus() {
        let expect: &[(FilterId, FilterGroup, &str)] = &[
            (FilterId::Average, FilterGroup::Blur, "Average"),
            (FilterId::Blur, FilterGroup::Blur, "Blur"),
            (FilterId::BlurMore, FilterGroup::Blur, "Blur More"),
            (FilterId::SmartBlur, FilterGroup::Blur, "Smart Blur…"),
            (FilterId::Sharpen, FilterGroup::Sharpen, "Sharpen"),
            (FilterId::SharpenMore, FilterGroup::Sharpen, "Sharpen More"),
            (
                FilterId::SharpenEdges,
                FilterGroup::Sharpen,
                "Sharpen Edges",
            ),
            (FilterId::Displace, FilterGroup::Distort, "Displace…"),
            (FilterId::Facet, FilterGroup::Pixelate, "Facet"),
            (FilterId::Fragment, FilterGroup::Pixelate, "Fragment"),
            (FilterId::Mezzotint, FilterGroup::Pixelate, "Mezzotint…"),
            (FilterId::Extrude, FilterGroup::Stylize, "Extrude…"),
            (FilterId::Tiles, FilterGroup::Stylize, "Tiles…"),
            (
                FilterId::TraceContour,
                FilterGroup::Stylize,
                "Trace Contour…",
            ),
        ];
        let listed = filter_menu().actions();
        for (id, group, label) in expect {
            assert_eq!(id.group(), *group, "{id:?}");
            assert_eq!(id.label(), *label, "{id:?}");
            assert!(
                listed.contains(&MenuAction::Filter(*id)),
                "{id:?} is not in the menu"
            );
        }
    }

    #[test]
    fn image_mode_lists_both_depths_ticks_the_documents_and_greys_only_that_one() {
        let mode = image_menu()
            .entries
            .into_iter()
            .find_map(|e| match e {
                Entry::Submenu {
                    label: "Mode",
                    entries,
                } => Some(entries),
                _ => None,
            })
            .expect("Image has a Mode submenu");
        let actions: Vec<MenuAction> = mode
            .iter()
            .filter_map(|e| match e {
                Entry::Item(a) => Some(*a),
                _ => None,
            })
            .collect();
        for depth in ChannelDepth::ALL {
            assert!(
                actions.contains(&MenuAction::SetBitDepth(*depth)),
                "{depth:?} is missing from Image > Mode"
            );
            assert!(menu_actions().contains(&MenuAction::SetBitDepth(*depth)));
        }
        let mut doc = Document::new(8, 8, "deep");
        doc.meta.bit_depth = 16;
        let ctx = MenuContext::from_document(&doc, &History::default());
        assert_eq!(ctx.bit_depth, ChannelDepth::Sixteen);
        assert_eq!(
            MenuAction::SetBitDepth(ChannelDepth::Sixteen).checked(&ctx),
            Some(true)
        );
        assert_eq!(
            MenuAction::SetBitDepth(ChannelDepth::Eight).checked(&ctx),
            Some(false)
        );
        // W4-F: the row for the other depth is live; the checked row is the
        // one greyed, with the reason.
        assert_eq!(
            MenuAction::SetBitDepth(ChannelDepth::Sixteen)
                .resolve(&ctx)
                .reason(),
            Some("The document is already at that depth")
        );
        assert_eq!(
            MenuAction::SetBitDepth(ChannelDepth::Eight)
                .resolve(&ctx)
                .reason(),
            None,
            "16 -> 8 is enabled on a 16-bit document"
        );
        let shallow = MenuContext::from_document(&Document::new(8, 8, "s"), &History::default());
        assert_eq!(
            MenuAction::SetBitDepth(ChannelDepth::Sixteen)
                .resolve(&shallow)
                .reason(),
            None,
            "8 -> 16 is enabled on an 8-bit document"
        );
        assert_eq!(
            MenuAction::SetBitDepth(ChannelDepth::Eight).checked(&shallow),
            Some(true)
        );
        assert!(MenuAction::SetBitDepth(ChannelDepth::Eight)
            .resolve(&shallow)
            .reason()
            .is_some());
        // Every reason is one clean sentence: a lost `\` continuation
        // once left a run of indentation mid-sentence in the greyed row.
        for from in ChannelDepth::ALL {
            for to in ChannelDepth::ALL {
                let reason = to.conversion_reason(*from).unwrap_or_default();
                assert!(
                    !reason.contains("  ") && reason == reason.trim(),
                    "{from:?} -> {to:?} reason has a whitespace run: {reason:?}"
                );
            }
        }
    }

    #[test]
    fn thirty_two_bits_is_an_image_mode_row_for_rgb_and_grayscale() {
        // W10-H: Image > Mode lists 32 Bits/Channel and a 32-bit document
        // checks it; RGB and Grayscale reach it, the ink modes do not.
        let rows = MenuAction::SetBitDepth(ChannelDepth::ThirtyTwo);
        assert!(menu_actions().contains(&rows));
        assert_eq!(rows.label(), "32 Bits/Channel");
        let mut doc = Document::new(8, 8, "hdr");
        doc.meta.bit_depth = 32;
        let ctx = MenuContext::from_document(&doc, &History::default());
        assert_eq!(ctx.bit_depth, ChannelDepth::ThirtyTwo);
        assert_eq!(rows.checked(&ctx), Some(true));
        assert!(rows.resolve(&ctx).reason().is_some());
        for down in [ChannelDepth::Eight, ChannelDepth::Sixteen] {
            assert_eq!(MenuAction::SetBitDepth(down).resolve(&ctx).reason(), None);
        }
        // A 32-bit document changes colour mode only after going down.
        assert_eq!(
            MenuAction::SetColorMode(ColorMode::Cmyk)
                .resolve(&ctx)
                .reason(),
            Some("Convert to 16 or 8 Bits/Channel before changing the colour mode")
        );
        let rgb = MenuContext::from_document(&Document::new(8, 8, "s"), &History::default());
        assert_eq!(rows.resolve(&rgb).reason(), None, "8 -> 32 on RGB");
        let cmyk = MenuContext {
            color_mode: ColorMode::Cmyk,
            ..rgb.clone()
        };
        assert_eq!(
            rows.resolve(&cmyk).reason(),
            Some("32 Bits/Channel is for RGB and Grayscale documents")
        );
        let gray = MenuContext {
            color_mode: ColorMode::Grayscale,
            ..rgb
        };
        assert_eq!(rows.resolve(&gray).reason(), None);
    }

    #[test]
    fn feather_is_shift_f6() {
        assert_eq!(
            MenuAction::Modify(ModifySelection::Feather).shortcut(),
            Some(Shortcut {
                ctrl: false,
                alt: false,
                shift: true,
                key: Key::F(6),
            })
        );
    }

    #[test]
    fn every_colour_mode_is_enabled_with_a_document() {
        // W7-D: Lab, CMYK and Indexed were greyed with a reason; Photopea
        // converts into all five, and so does this build now.
        let ctx = MenuContext {
            has_document: true,
            ..Default::default()
        };
        // W10-H: Bitmap and Duotone are reached from Grayscale only; on a
        // Grayscale document every mode is enabled.
        let gray = MenuContext {
            has_document: true,
            color_mode: ColorMode::Grayscale,
            ..Default::default()
        };
        for mode in [ColorMode::Bitmap, ColorMode::Duotone] {
            let reason = MenuAction::SetColorMode(mode).resolve(&ctx).reason();
            assert!(
                reason.is_some_and(|r| r.contains("Grayscale")),
                "{mode:?} on RGB: {reason:?}"
            );
        }
        for &mode in ColorMode::ALL {
            assert!(
                MenuAction::SetColorMode(mode).resolve(&gray).is_enabled(),
                "{mode:?} is greyed"
            );
            assert!(mode.is_supported(), "{mode:?}");
            assert!(menu_actions().contains(&MenuAction::SetColorMode(mode)));
            assert_eq!(ColorMode::from_meta(mode as u8), mode);
        }
        // No document still greys them, for that reason alone.
        assert_eq!(
            MenuAction::SetColorMode(ColorMode::Cmyk)
                .resolve(&MenuContext::default())
                .reason(),
            Some("No document is open")
        );
    }

    #[test]
    fn only_the_mode_that_opens_a_dialog_has_an_ellipsis() {
        // W7-D: Indexed Color asks for a palette first (like Trim…); the
        // other four convert at once.
        for &mode in ColorMode::ALL {
            let label = MenuAction::SetColorMode(mode).label();
            assert_eq!(
                label.ends_with('…'),
                matches!(
                    mode,
                    ColorMode::Indexed | ColorMode::Bitmap | ColorMode::Duotone
                ),
                "{mode:?}: {label}"
            );
        }
        assert_eq!(
            MenuAction::SetColorMode(ColorMode::Indexed).label(),
            "Indexed Color…"
        );
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
    fn vector_mask_rows_gate_on_the_layer_its_vector_mask_and_a_path() {
        let (doc, _g, inside, _b) = stacked_document();
        let ctx = ctx_with_layer(&doc, inside);
        let row = |op| MenuAction::VectorMask(op);
        assert_eq!(
            row(VectorMaskOp::RevealAll).resolve(&ctx).intent(),
            Some(&Intent::Action(row(VectorMaskOp::RevealAll)))
        );
        assert_eq!(
            row(VectorMaskOp::CurrentPath).resolve(&ctx).reason(),
            Some("There is no path; draw one or select it in the Paths panel")
        );
        let pathed = MenuContext {
            has_current_path: true,
            ..ctx.clone()
        };
        assert_eq!(
            row(VectorMaskOp::CurrentPath).resolve(&pathed).intent(),
            Some(&Intent::Action(row(VectorMaskOp::CurrentPath)))
        );
        assert_eq!(
            row(VectorMaskOp::Delete).resolve(&ctx).reason(),
            Some("The layer has no vector mask")
        );
        let masked = MenuContext {
            active: Some(ActiveLayer {
                has_vector_mask: true,
                vector_mask_enabled: true,
                ..ctx.active.unwrap()
            }),
            ..pathed
        };
        assert_eq!(
            row(VectorMaskOp::HideAll).resolve(&masked).reason(),
            Some("The layer already has a vector mask")
        );
        for op in [VectorMaskOp::Delete, VectorMaskOp::Toggle] {
            assert_eq!(
                row(op).resolve(&masked).intent(),
                Some(&Intent::Action(row(op)))
            );
        }
        // Every row is in the action vocabulary the menu tree is checked against.
        for op in VectorMaskOp::ALL {
            assert!(MenuAction::all().contains(&row(*op)), "{op:?}");
        }
    }

    #[test]
    fn adding_a_mask_routes_to_the_app_and_a_second_one_is_refused() {
        let (doc, _g, inside, _b) = stacked_document();
        let ctx = ctx_with_layer(&doc, inside);
        // Card 057: the creation ops route to the APPLICATION (the coverage
        // is pixels the app rasterises and attaches atomically); the menu's
        // job is the gating below.
        match MenuAction::Mask(MaskOp::RevealAll).resolve(&ctx).intent() {
            Some(Intent::Action(MenuAction::Mask(MaskOp::RevealAll))) => {}
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
        // ...and deleting is the other way round. Card 058's Invert is a
        // mask op, not a creation op: it needs a mask and routes to the app.
        assert_eq!(
            MenuAction::Mask(MaskOp::Delete).resolve(&ctx).reason(),
            Some("The layer has no mask")
        );
        assert_eq!(
            MenuAction::Mask(MaskOp::Invert).resolve(&ctx).reason(),
            Some("The layer has no mask")
        );
        match MenuAction::Mask(MaskOp::Invert).resolve(&masked).intent() {
            Some(Intent::Action(MenuAction::Mask(MaskOp::Invert))) => {}
            other => panic!("unexpected resolution: {other:?}"),
        }
        // Card 060: the Refine Mask dialog needs a mask to refine — the
        // same gate, routing to the dialog host.
        assert_eq!(
            MenuAction::RefineMask.resolve(&ctx).reason(),
            Some("The layer has no mask")
        );
        match MenuAction::RefineMask.resolve(&masked).intent() {
            Some(Intent::Action(MenuAction::RefineMask)) => {}
            other => panic!("unexpected resolution: {other:?}"),
        }
        // Card 062: the fringe cleanup shares the mask gate, routes as its
        // own action.
        assert_eq!(
            MenuAction::RemoveColorFringe.resolve(&ctx).reason(),
            Some("The layer has no mask")
        );
        match MenuAction::RemoveColorFringe.resolve(&masked).intent() {
            Some(Intent::Action(MenuAction::RemoveColorFringe)) => {}
            other => panic!("unexpected resolution: {other:?}"),
        }
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
        for id in AdjustmentId::LAYERS {
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
        // Actions is the one panel Essentials leaves closed (W2-D opened
        // Paths, tabbed with Layers), so it is the closed side of the proof.
        assert!(!ctx.dock.is_open(PanelId::Actions));
        assert_eq!(
            MenuAction::TogglePanel(PanelId::Actions)
                .resolve(&ctx)
                .intent(),
            Some(&Intent::SetPanelOpen {
                panel: PanelId::Actions,
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
        // W10-G: Fade names the step it would fade, and only then.
        let mut fading = MenuContext::default();
        assert_eq!(MenuAction::Fade.label_in(&fading), MenuAction::Fade.label());
        fading.fade_step = Some("Apply Invert".into());
        assert_eq!(MenuAction::Fade.label_in(&fading), "Fade Apply Invert…");
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
        let layers: HashSet<AdjustmentId> = AdjustmentId::LAYERS.iter().copied().collect();
        assert_eq!(image, all);
        assert_eq!(layer, layers);
        // Photopea's split: six are destructive-only, Color Lookup is a
        // layer too, and nothing else is missing from the Layer menu.
        let destructive_only: HashSet<AdjustmentId> = all.difference(&layers).copied().collect();
        assert_eq!(
            destructive_only,
            HashSet::from([
                AdjustmentId::Desaturate,
                AdjustmentId::Equalize,
                AdjustmentId::ShadowsHighlights,
                AdjustmentId::HdrToning,
                AdjustmentId::MatchColor,
                AdjustmentId::ReplaceColor,
            ])
        );
        assert!(layers.contains(&AdjustmentId::ColorLookup));
    }

    #[test]
    fn desaturate_wears_shift_ctrl_u_and_only_three_adjustments_skip_the_dialog() {
        assert_eq!(
            MenuAction::ApplyAdjustment(AdjustmentId::Desaturate).shortcut(),
            Some(Shortcut::ctrl_shift('u'))
        );
        let no_dialog: Vec<AdjustmentId> = AdjustmentId::ALL
            .iter()
            .copied()
            .filter(|id| !id.has_dialog())
            .collect();
        // W5-E: Invert asks nothing either, so Ctrl+I inverts at once.
        assert_eq!(
            no_dialog,
            vec![
                AdjustmentId::Invert,
                AdjustmentId::Desaturate,
                AdjustmentId::Equalize
            ]
        );
    }

    #[test]
    fn an_adjustment_row_wears_the_ellipsis_only_when_it_opens_a_dialog() {
        // W4-E round 2: the ellipsis promises a dialog, so Desaturate and
        // Equalize (applied on the click) read bare, and every other row keeps
        // it — in the menu model the bar actually paints.
        let rows: Vec<MenuAction> = image_menu()
            .actions()
            .into_iter()
            .filter(|a| matches!(a, MenuAction::ApplyAdjustment(_)))
            .collect();
        assert_eq!(rows.len(), AdjustmentId::ALL.len());
        for action in rows {
            let MenuAction::ApplyAdjustment(id) = action else {
                unreachable!()
            };
            let label = action.label();
            assert_eq!(
                label.ends_with('…'),
                id.has_dialog(),
                "{id:?} is labelled {label:?}"
            );
            assert_eq!(label.trim_end_matches('…'), id.label());
        }
        assert_eq!(
            MenuAction::ApplyAdjustment(AdjustmentId::Desaturate).label(),
            "Desaturate"
        );
        assert_eq!(
            MenuAction::ApplyAdjustment(AdjustmentId::ShadowsHighlights).label(),
            "Shadows/Highlights…"
        );
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

    // ---- W2-F: the rows the audit found missing ----------------------------

    #[test]
    fn the_w2f_rows_are_in_their_menus_with_their_labels() {
        let by_title = |title: &str| -> Vec<MenuAction> {
            menu_bar(0)
                .into_iter()
                .find(|m| m.title == title)
                .unwrap_or_else(|| panic!("no {title} menu"))
                .actions()
        };
        let file = by_title("File");
        assert!(file.contains(&MenuAction::SaveAsPsd));
        assert_eq!(MenuAction::SaveAsPsd.label(), "Save as PSD…");

        let edit = by_title("Edit");
        assert!(edit.contains(&MenuAction::StepForward));
        assert!(edit.contains(&MenuAction::StepBackward));

        let layer = by_title("Layer");
        for edge in AlignEdge::ALL {
            assert!(layer.contains(&MenuAction::AlignLayers(*edge)), "{edge:?}");
        }
        for axis in DistributeAxis::ALL {
            assert!(
                layer.contains(&MenuAction::DistributeLayers(*axis)),
                "{axis:?}"
            );
        }
        for lock in LayerLock::ALL {
            assert!(layer.contains(&MenuAction::LockLayer(*lock)), "{lock:?}");
        }
        assert!(layer.contains(&MenuAction::RenameLayer));
        assert!(layer.contains(&MenuAction::StampVisible));
        assert_eq!(
            MenuAction::StampVisible.shortcut(),
            Some(Shortcut::ctrl_alt_shift('e'))
        );
        assert_eq!(
            action_for_shortcut(Shortcut::ctrl_alt_shift('e'), 0),
            Some(MenuAction::StampVisible)
        );

        let select = by_title("Select");
        assert!(select.contains(&MenuAction::RefineEdge));

        let view = by_title("View");
        assert!(view.contains(&MenuAction::Zoom(ZoomCommand::Double)));
        assert_eq!(MenuAction::Zoom(ZoomCommand::Double).label(), "200%");
        assert!(view.contains(&MenuAction::NewGuide));
        assert!(view.contains(&MenuAction::ClearGuides));
        assert!(view.contains(&MenuAction::LockGuides));

        // Trim keeps its ellipsis (it opens a dialog now); Image ▸ Duplicate
        // loses its false one because nothing is asked. Duplicate Layer keeps
        // its own: the application's dialog host opens `DuplicateLayerDialog`
        // for it (the copy's name is asked), so the ellipsis is earned.
        assert_eq!(MenuAction::Trim.label(), "Trim…");
        assert_eq!(MenuAction::DuplicateDocument.label(), "Duplicate");
        assert_eq!(MenuAction::DuplicateLayer.label(), "Duplicate Layer…");
    }

    #[test]
    fn blending_options_routes_to_the_layer_style_dialog_not_the_panel() {
        let (doc, _g, inside, _b) = stacked_document();
        let ctx = ctx_with_layer(&doc, inside);
        assert_eq!(
            MenuAction::BlendingOptions.resolve(&ctx).intent(),
            Some(&Intent::Action(MenuAction::BlendingOptions))
        );
        assert_eq!(
            MenuAction::BlendingOptions
                .resolve(&MenuContext {
                    has_document: true,
                    ..Default::default()
                })
                .reason(),
            Some("Select a layer first")
        );
    }

    #[test]
    fn a_lock_row_resolves_to_the_flipped_flag_and_ticks_when_set() {
        let (doc, _g, inside, _b) = stacked_document();
        let ctx = ctx_with_layer(&doc, inside);
        assert_eq!(
            MenuAction::LockLayer(LayerLock::Position).checked(&ctx),
            Some(false)
        );
        match MenuAction::LockLayer(LayerLock::Position)
            .resolve(&ctx)
            .intent()
        {
            Some(Intent::Document(Command::SetLayerProperties { layer_id, patch })) => {
                assert_eq!(*layer_id, inside);
                assert_eq!(
                    patch.locked,
                    Some(LockState {
                        position: true,
                        ..LockState::default()
                    })
                );
            }
            other => panic!("unexpected resolution: {other:?}"),
        }
        let locked = MenuContext {
            active: Some(ActiveLayer {
                locked: LockState {
                    all: true,
                    ..LockState::default()
                },
                ..ctx.active.unwrap()
            }),
            ..ctx
        };
        assert_eq!(
            MenuAction::LockLayer(LayerLock::All).checked(&locked),
            Some(true)
        );
        // Releasing the blanket lock is the one patch it allows.
        match MenuAction::LockLayer(LayerLock::All)
            .resolve(&locked)
            .intent()
        {
            Some(Intent::Document(Command::SetLayerProperties { patch, .. })) => {
                assert_eq!(patch.locked, Some(LockState::default()));
            }
            other => panic!("unexpected resolution: {other:?}"),
        }
        assert_eq!(
            MenuAction::RenameLayer.resolve(&locked).reason(),
            Some("The layer is locked")
        );
        assert_eq!(
            MenuAction::AlignLayers(AlignEdge::Left)
                .resolve(&locked)
                .reason(),
            Some("The layer's position is locked")
        );
    }

    #[test]
    fn distribute_needs_three_layers_and_refine_edge_needs_a_selection() {
        let (doc, _g, inside, _b) = stacked_document();
        let ctx = ctx_with_layer(&doc, inside);
        assert_eq!(
            MenuAction::DistributeLayers(DistributeAxis::Horizontal)
                .resolve(&ctx)
                .reason(),
            Some("Select three or more layers")
        );
        assert!(MenuAction::DistributeLayers(DistributeAxis::Horizontal)
            .resolve(&MenuContext {
                selected_layers: 3,
                ..ctx.clone()
            })
            .is_enabled());
        assert_eq!(
            MenuAction::RefineEdge.resolve(&ctx).reason(),
            Some("There is no selection")
        );
        assert!(MenuAction::RefineEdge
            .resolve(&MenuContext {
                has_selection: true,
                ..ctx
            })
            .is_enabled());
    }

    #[test]
    fn the_guide_rows_resolve_to_set_guides_commands_over_the_documents_set() {
        let mut ctx = MenuContext {
            has_document: true,
            ..Default::default()
        };
        assert_eq!(
            MenuAction::ClearGuides.resolve(&ctx).reason(),
            Some("There are no guides to clear")
        );
        assert_eq!(MenuAction::LockGuides.checked(&ctx), Some(false));
        ctx.guides = Guides {
            list: vec![editor_core::Guide {
                axis: editor_core::GuideAxis::Vertical,
                doc: 12.0,
                locked: false,
            }],
            visible: true,
            locked: false,
        };
        match MenuAction::ClearGuides.resolve(&ctx).intent() {
            Some(Intent::Document(Command::SetGuides { guides })) => {
                assert!(guides.list.is_empty());
                assert!(guides.visible, "clearing keeps the visibility flag");
            }
            other => panic!("unexpected resolution: {other:?}"),
        }
        match MenuAction::LockGuides.resolve(&ctx).intent() {
            Some(Intent::Document(Command::SetGuides { guides })) => {
                assert!(guides.locked, "the lock flips on");
                assert_eq!(guides.list.len(), 1, "locking keeps the guides");
            }
            other => panic!("unexpected resolution: {other:?}"),
        }
        // The context reads the document's own set.
        let mut doc = Document::new(16, 16, "G");
        doc.guides = ctx.guides.clone();
        let read = MenuContext::from_document(&doc, &History::new());
        assert_eq!(read.guides, ctx.guides);
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

/// W10-J: the View menu's Show and Snap To submenus, its guide rows, and
/// Snap To > All / None as the workspace absorbs them.
#[cfg(test)]
mod w10j_view_tests {
    use super::*;

    fn actions(entries: &[Entry]) -> Vec<MenuAction> {
        entries
            .iter()
            .filter_map(|e| match e {
                Entry::Item(a) => Some(*a),
                _ => None,
            })
            .collect()
    }

    fn submenu<'a>(menu: &'a Menu, label: &str) -> &'a [Entry] {
        menu.entries
            .iter()
            .find_map(|e| match e {
                Entry::Submenu { label: l, entries } if *l == label => Some(entries.as_slice()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no {label} submenu"))
    }

    #[test]
    fn the_view_menu_has_snap_to_show_extras_and_the_guide_rows() {
        let view = view_menu();
        let top = actions(&view.entries);
        assert!(top.contains(&MenuAction::ToggleView(ViewFlag::Extras)));
        for row in [MenuAction::NewGuideLayout, MenuAction::NewGuidesFromShape] {
            assert!(top.contains(&row), "View has no {row:?} row");
        }
        // The submenu flags are not also flat rows.
        for flag in ViewFlag::ALL.iter().filter(|f| f.in_submenu()) {
            assert!(!top.contains(&MenuAction::ToggleView(*flag)), "{flag:?}");
        }
        let snap = actions(submenu(&view, "Snap To"));
        for flag in ViewFlag::SNAP_TO {
            assert!(snap.contains(&MenuAction::ToggleView(*flag)), "{flag:?}");
        }
        assert!(snap.contains(&MenuAction::SnapToAll) && snap.contains(&MenuAction::SnapToNone));
        assert_eq!(
            actions(submenu(&view, "Show")),
            vec![MenuAction::ToggleView(ViewFlag::Slices)]
        );
        assert_eq!(
            MenuAction::ToggleView(ViewFlag::Extras).shortcut(),
            Some(Shortcut::ctrl('h'))
        );
    }

    #[test]
    fn extras_hides_what_it_names_and_keeps_every_tick() {
        let mut flags = ViewFlags::defaults();
        flags.set(ViewFlag::Grid, true);
        assert!(flags.shows(ViewFlag::Grid) && flags.shows(ViewFlag::Guides));
        flags.set(ViewFlag::Extras, false);
        for flag in [
            ViewFlag::Grid,
            ViewFlag::Guides,
            ViewFlag::SmartGuides,
            ViewFlag::SelectionEdges,
            ViewFlag::LayerEdges,
            ViewFlag::Slices,
        ] {
            assert!(flags.get(flag), "{flag:?} keeps its tick");
            assert!(!flags.shows(flag), "{flag:?} is hidden by Extras");
        }
        assert!(flags.shows(ViewFlag::Rulers), "rulers are not an extra");
    }

    #[test]
    fn snap_to_all_and_none_set_the_five_targets_and_grey_when_moot() {
        let mut w = crate::Workspace::new();
        assert!(w.absorb_action(MenuAction::SnapToNone));
        assert!(ViewFlag::SNAP_TO.iter().all(|f| !w.view_flags.get(*f)));
        assert!(
            !w.absorb_action(MenuAction::SnapToNone),
            "a second None changes nothing"
        );
        let ctx = MenuContext {
            has_document: true,
            view: w.view_flags,
            ..MenuContext::default()
        };
        assert!(!MenuAction::SnapToNone.resolve(&ctx).is_enabled());
        assert!(MenuAction::SnapToAll.resolve(&ctx).is_enabled());
        assert!(w.absorb_action(MenuAction::SnapToAll));
        assert!(ViewFlag::SNAP_TO.iter().all(|f| w.view_flags.get(*f)));
    }

    #[test]
    fn the_content_aware_scale_submenu_leads_with_the_interactive_box() {
        let edit = edit_menu();
        let rows = actions(submenu(&edit, "Content-Aware Scale"));
        assert_eq!(rows.first(), Some(&MenuAction::ContentAwareScaleFree));
        assert_eq!(
            MenuAction::ContentAwareScaleFree.shortcut(),
            Some(Shortcut::ctrl_alt_shift('c'))
        );
        for step in ContentAwareScaleStep::ALL {
            assert!(rows.contains(&MenuAction::ContentAwareScale(*step)));
        }
    }
}
