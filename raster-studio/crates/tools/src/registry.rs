//! The tool registry.
//!
//! The UI must not contain a `match` over [`ToolId`]. If it does, every new
//! tool means editing the palette, the options bar, the cursor logic and the
//! shortcut table separately, and one of those four always gets forgotten.
//! Instead the registry answers all four questions in one place — what a tool
//! is called, which icon and cursor it uses, which key selects it, and what
//! options it exposes — and [`make`] hands back a live instance.
//!
//! Two tests keep it honest: every [`ToolId`] appears exactly once, and every
//! entry can be constructed and cancelled.

use crate::brush::BrushSettings;
use crate::bucket::{FillContent, FillSettings, PaintBucketTool, PatternFillTool};
use crate::edit::{
    CropTool, EyedropperTool, MagicEraserTool, MoveTool, PatchTool, RedEyeTool, SliceTool,
};
use crate::gradient::GradientTool;
use crate::select::{LassoKind, LassoTool, MarqueeShape, MarqueeTool, WandKind, WandTool};
use crate::shape::{ShapeKind, ShapeMode, ShapeTool};
use crate::stroke::{SpongeMode, StrokeOp, StrokeTool, ToneRange};
use crate::tool::{Tool, ToolId};
use crate::transform::TransformTool;
use crate::view::{ViewGesture, ViewTool};

/// Palette grouping — the dividers in the tool bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolGroup {
    Select,
    Crop,
    Retouch,
    Paint,
    Draw,
    Navigate,
    Transform,
}

/// The pointer shape a tool asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    Arrow,
    Move,
    Crosshair,
    /// A ring the size of the brush.
    BrushRing,
    Eyedropper,
    Bucket,
    OpenHand,
    ZoomIn,
    Rotate,
    CropMarks,
    Slice,
}

/// One control in the options bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OptionSpec {
    /// Stable key the UI reads and writes; also what a tool preset stores.
    pub key: &'static str,
    pub label: &'static str,
    pub kind: OptionKind,
}

/// What kind of control an option needs, and its range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OptionKind {
    Float {
        min: f32,
        max: f32,
        default: f32,
    },
    Int {
        min: i32,
        max: i32,
        default: i32,
    },
    Bool {
        default: bool,
    },
    Choice {
        choices: &'static [&'static str],
        default: usize,
    },
    Color {
        default: [f32; 4],
    },
}

const fn f(key: &'static str, label: &'static str, min: f32, max: f32, default: f32) -> OptionSpec {
    OptionSpec {
        key,
        label,
        kind: OptionKind::Float { min, max, default },
    }
}

const fn i(key: &'static str, label: &'static str, min: i32, max: i32, default: i32) -> OptionSpec {
    OptionSpec {
        key,
        label,
        kind: OptionKind::Int { min, max, default },
    }
}

const fn b(key: &'static str, label: &'static str, default: bool) -> OptionSpec {
    OptionSpec {
        key,
        label,
        kind: OptionKind::Bool { default },
    }
}

const fn c(
    key: &'static str,
    label: &'static str,
    choices: &'static [&'static str],
    default: usize,
) -> OptionSpec {
    OptionSpec {
        key,
        label,
        kind: OptionKind::Choice { choices, default },
    }
}

/// The controls every stamping tool shares.
const BRUSH_OPTS: &[OptionSpec] = &[
    f("size", "Size", 1.0, 5000.0, 24.0),
    f("hardness", "Hardness", 0.0, 1.0, 0.8),
    f("spacing", "Spacing", 0.01, 10.0, 0.25),
    f("opacity", "Opacity", 0.0, 1.0, 1.0),
    f("flow", "Flow", 0.0, 1.0, 1.0),
    f("smoothing", "Smoothing", 0.0, 0.99, 0.0),
    f("angle", "Angle", -3.15, 3.15, 0.0),
    f("roundness", "Roundness", 0.01, 1.0, 1.0),
    b("size_pressure", "Pressure → Size", true),
    b("flow_pressure", "Pressure → Flow", false),
];

const SELECTION_OPTS: &[OptionSpec] = &[
    c("mode", "Mode", &["New", "Add", "Subtract", "Intersect"], 0),
    f("feather", "Feather", 0.0, 250.0, 0.0),
    b("antialias", "Anti-alias", true),
];

const WAND_OPTS: &[OptionSpec] = &[
    c("mode", "Mode", &["New", "Add", "Subtract", "Intersect"], 0),
    f("tolerance", "Tolerance", 0.0, 1.0, 32.0 / 255.0),
    b("contiguous", "Contiguous", true),
    b("antialias", "Anti-alias", true),
    b("sample_merged", "Sample All Layers", false),
];

const FILL_OPTS: &[OptionSpec] = &[
    f("tolerance", "Tolerance", 0.0, 1.0, 32.0 / 255.0),
    b("contiguous", "Contiguous", true),
    b("antialias", "Anti-alias", true),
    f("opacity", "Opacity", 0.0, 1.0, 1.0),
    b("sample_merged", "Sample All Layers", false),
];

const CLONE_OPTS: &[OptionSpec] = &[
    f("size", "Size", 1.0, 5000.0, 40.0),
    f("hardness", "Hardness", 0.0, 1.0, 0.5),
    f("spacing", "Spacing", 0.01, 10.0, 0.05),
    f("opacity", "Opacity", 0.0, 1.0, 1.0),
    b("aligned", "Aligned", true),
];

const TONE_OPTS: &[OptionSpec] = &[
    f("size", "Size", 1.0, 5000.0, 60.0),
    f("hardness", "Hardness", 0.0, 1.0, 0.0),
    f("exposure", "Exposure", 0.0, 1.0, 0.25),
    c("range", "Range", &["Shadows", "Midtones", "Highlights"], 1),
];

const SHAPE_OPTS: &[OptionSpec] = &[
    c("mode", "Mode", &["Shape Layer", "Rasterize"], 0),
    b("from_center", "From Centre", false),
];

/// Everything the UI needs to know about one tool.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToolInfo {
    pub id: ToolId,
    pub name: &'static str,
    pub group: ToolGroup,
    /// Icon key; the UI resolves it against its own icon set.
    pub icon: &'static str,
    pub cursor: Cursor,
    /// The key that selects this tool. Tools sharing a key form a cycle group,
    /// in the order they appear in [`all`].
    pub shortcut: Option<char>,
    pub options: &'static [OptionSpec],
}

const fn t(
    id: ToolId,
    name: &'static str,
    group: ToolGroup,
    icon: &'static str,
    cursor: Cursor,
    shortcut: Option<char>,
    options: &'static [OptionSpec],
) -> ToolInfo {
    ToolInfo {
        id,
        name,
        group,
        icon,
        cursor,
        shortcut,
        options,
    }
}

const TOOLS: &[ToolInfo] = &[
    t(
        ToolId::Move,
        "Move",
        ToolGroup::Select,
        "move",
        Cursor::Move,
        Some('v'),
        &[
            b("auto_select", "Auto-Select", false),
            b("show_transform", "Show Transform Controls", false),
        ],
    ),
    t(
        ToolId::RectMarquee,
        "Rectangular Marquee",
        ToolGroup::Select,
        "marquee-rect",
        Cursor::Crosshair,
        Some('m'),
        SELECTION_OPTS,
    ),
    t(
        ToolId::EllipseMarquee,
        "Elliptical Marquee",
        ToolGroup::Select,
        "marquee-ellipse",
        Cursor::Crosshair,
        Some('m'),
        SELECTION_OPTS,
    ),
    t(
        ToolId::SingleRowMarquee,
        "Single Row Marquee",
        ToolGroup::Select,
        "marquee-row",
        Cursor::Crosshair,
        Some('m'),
        SELECTION_OPTS,
    ),
    t(
        ToolId::SingleColumnMarquee,
        "Single Column Marquee",
        ToolGroup::Select,
        "marquee-column",
        Cursor::Crosshair,
        Some('m'),
        SELECTION_OPTS,
    ),
    t(
        ToolId::Lasso,
        "Lasso",
        ToolGroup::Select,
        "lasso",
        Cursor::Crosshair,
        Some('l'),
        SELECTION_OPTS,
    ),
    t(
        ToolId::PolygonalLasso,
        "Polygonal Lasso",
        ToolGroup::Select,
        "lasso-poly",
        Cursor::Crosshair,
        Some('l'),
        SELECTION_OPTS,
    ),
    t(
        ToolId::MagneticLasso,
        "Magnetic Lasso",
        ToolGroup::Select,
        "lasso-magnetic",
        Cursor::Crosshair,
        Some('l'),
        &[
            c("mode", "Mode", &["New", "Add", "Subtract", "Intersect"], 0),
            i("search_radius", "Width", 1, 256, 24),
            f("edge_weight", "Contrast", 0.0, 4.0, 1.0),
        ],
    ),
    t(
        ToolId::MagicWand,
        "Magic Wand",
        ToolGroup::Select,
        "wand",
        Cursor::Crosshair,
        Some('w'),
        WAND_OPTS,
    ),
    t(
        ToolId::QuickSelect,
        "Quick Selection",
        ToolGroup::Select,
        "quick-select",
        Cursor::BrushRing,
        Some('w'),
        &[
            c("mode", "Mode", &["New", "Add", "Subtract", "Intersect"], 0),
            f("radius", "Size", 1.0, 500.0, 8.0),
            f("tolerance", "Tolerance", 0.0, 1.0, 16.0 / 255.0),
        ],
    ),
    t(
        ToolId::Crop,
        "Crop",
        ToolGroup::Crop,
        "crop",
        Cursor::CropMarks,
        Some('c'),
        &[
            f("aspect", "Aspect Ratio", 0.0, 100.0, 0.0),
            f("straighten", "Straighten", -3.15, 3.15, 0.0),
            b("delete_cropped", "Delete Cropped Pixels", false),
        ],
    ),
    t(
        ToolId::Slice,
        "Slice",
        ToolGroup::Crop,
        "slice",
        Cursor::Slice,
        Some('c'),
        &[],
    ),
    t(
        ToolId::Eyedropper,
        "Eyedropper",
        ToolGroup::Crop,
        "eyedropper",
        Cursor::Eyedropper,
        Some('i'),
        &[
            i("sample_radius", "Sample Size", 0, 64, 0),
            b("sample_all_layers", "Sample All Layers", true),
        ],
    ),
    t(
        ToolId::SpotHealing,
        "Spot Healing Brush",
        ToolGroup::Retouch,
        "spot-heal",
        Cursor::BrushRing,
        Some('j'),
        &[
            f("size", "Size", 1.0, 5000.0, 30.0),
            f("hardness", "Hardness", 0.0, 1.0, 0.6),
        ],
    ),
    t(
        ToolId::HealingBrush,
        "Healing Brush",
        ToolGroup::Retouch,
        "heal",
        Cursor::BrushRing,
        Some('j'),
        &[
            f("size", "Size", 1.0, 5000.0, 40.0),
            f("softness", "Softness", 0.5, 64.0, 4.0),
            b("aligned", "Aligned", true),
        ],
    ),
    t(
        ToolId::Patch,
        "Patch",
        ToolGroup::Retouch,
        "patch",
        Cursor::Crosshair,
        Some('j'),
        &[f("softness", "Softness", 0.5, 64.0, 4.0)],
    ),
    t(
        ToolId::RedEye,
        "Red Eye",
        ToolGroup::Retouch,
        "red-eye",
        Cursor::Crosshair,
        Some('j'),
        &[
            f("threshold", "Pupil Threshold", 1.0, 4.0, 1.6),
            f("darken", "Darken Amount", 0.0, 1.0, 0.5),
        ],
    ),
    t(
        ToolId::Brush,
        "Brush",
        ToolGroup::Paint,
        "brush",
        Cursor::BrushRing,
        Some('b'),
        BRUSH_OPTS,
    ),
    t(
        ToolId::Pencil,
        "Pencil",
        ToolGroup::Paint,
        "pencil",
        Cursor::BrushRing,
        Some('b'),
        &[
            f("size", "Size", 1.0, 1000.0, 1.0),
            f("opacity", "Opacity", 0.0, 1.0, 1.0),
            f("spacing", "Spacing", 0.01, 10.0, 0.1),
        ],
    ),
    t(
        ToolId::ColorReplacement,
        "Colour Replacement",
        ToolGroup::Paint,
        "color-replace",
        Cursor::BrushRing,
        Some('b'),
        &[
            f("size", "Size", 1.0, 5000.0, 30.0),
            f("tolerance", "Tolerance", 0.0, 1.0, 30.0 / 255.0),
            f("opacity", "Opacity", 0.0, 1.0, 1.0),
        ],
    ),
    t(
        ToolId::CloneStamp,
        "Clone Stamp",
        ToolGroup::Paint,
        "clone",
        Cursor::BrushRing,
        Some('s'),
        CLONE_OPTS,
    ),
    t(
        ToolId::PatternStamp,
        "Pattern Stamp",
        ToolGroup::Paint,
        "pattern-stamp",
        Cursor::BrushRing,
        Some('s'),
        &[
            f("size", "Size", 1.0, 5000.0, 40.0),
            f("opacity", "Opacity", 0.0, 1.0, 1.0),
            f("spacing", "Spacing", 0.01, 10.0, 0.1),
        ],
    ),
    t(
        ToolId::Eraser,
        "Eraser",
        ToolGroup::Paint,
        "eraser",
        Cursor::BrushRing,
        Some('e'),
        BRUSH_OPTS,
    ),
    t(
        ToolId::BackgroundEraser,
        "Background Eraser",
        ToolGroup::Paint,
        "eraser-bg",
        Cursor::BrushRing,
        Some('e'),
        &[
            f("size", "Size", 1.0, 5000.0, 40.0),
            f("tolerance", "Tolerance", 0.0, 1.0, 30.0 / 255.0),
        ],
    ),
    t(
        ToolId::MagicEraser,
        "Magic Eraser",
        ToolGroup::Paint,
        "eraser-magic",
        Cursor::Crosshair,
        Some('e'),
        FILL_OPTS,
    ),
    t(
        ToolId::Gradient,
        "Gradient",
        ToolGroup::Paint,
        "gradient",
        Cursor::Crosshair,
        Some('g'),
        &[
            c(
                "shape",
                "Style",
                &["Linear", "Radial", "Angle", "Reflected", "Diamond"],
                0,
            ),
            b("dither", "Dither", true),
            b("reverse", "Reverse", false),
            f("opacity", "Opacity", 0.0, 1.0, 1.0),
        ],
    ),
    t(
        ToolId::PaintBucket,
        "Paint Bucket",
        ToolGroup::Paint,
        "bucket",
        Cursor::Bucket,
        Some('g'),
        FILL_OPTS,
    ),
    t(
        ToolId::PatternFill,
        "Pattern Fill",
        ToolGroup::Paint,
        "pattern-fill",
        Cursor::Bucket,
        Some('g'),
        &[f("opacity", "Opacity", 0.0, 1.0, 1.0)],
    ),
    t(
        ToolId::Blur,
        "Blur",
        ToolGroup::Retouch,
        "blur",
        Cursor::BrushRing,
        None,
        &[
            f("size", "Size", 1.0, 5000.0, 40.0),
            f("radius", "Strength", 0.1, 64.0, 3.0),
            f("opacity", "Opacity", 0.0, 1.0, 1.0),
        ],
    ),
    t(
        ToolId::Sharpen,
        "Sharpen",
        ToolGroup::Retouch,
        "sharpen",
        Cursor::BrushRing,
        None,
        &[
            f("size", "Size", 1.0, 5000.0, 40.0),
            f("amount", "Strength", 0.0, 4.0, 1.0),
            f("opacity", "Opacity", 0.0, 1.0, 1.0),
        ],
    ),
    t(
        ToolId::Smudge,
        "Smudge",
        ToolGroup::Retouch,
        "smudge",
        Cursor::BrushRing,
        None,
        &[
            f("size", "Size", 1.0, 5000.0, 40.0),
            f("strength", "Strength", 0.0, 1.0, 0.5),
        ],
    ),
    t(
        ToolId::Dodge,
        "Dodge",
        ToolGroup::Retouch,
        "dodge",
        Cursor::BrushRing,
        Some('o'),
        TONE_OPTS,
    ),
    t(
        ToolId::Burn,
        "Burn",
        ToolGroup::Retouch,
        "burn",
        Cursor::BrushRing,
        Some('o'),
        TONE_OPTS,
    ),
    t(
        ToolId::Sponge,
        "Sponge",
        ToolGroup::Retouch,
        "sponge",
        Cursor::BrushRing,
        Some('o'),
        &[
            f("size", "Size", 1.0, 5000.0, 60.0),
            f("amount", "Flow", 0.0, 1.0, 0.3),
            c("mode", "Mode", &["Desaturate", "Saturate"], 0),
        ],
    ),
    t(
        ToolId::Rectangle,
        "Rectangle",
        ToolGroup::Draw,
        "shape-rect",
        Cursor::Crosshair,
        Some('u'),
        SHAPE_OPTS,
    ),
    t(
        ToolId::RoundedRectangle,
        "Rounded Rectangle",
        ToolGroup::Draw,
        "shape-rrect",
        Cursor::Crosshair,
        Some('u'),
        &[
            c("mode", "Mode", &["Shape Layer", "Rasterize"], 0),
            f("radius", "Radius", 0.0, 500.0, 8.0),
        ],
    ),
    t(
        ToolId::Ellipse,
        "Ellipse",
        ToolGroup::Draw,
        "shape-ellipse",
        Cursor::Crosshair,
        Some('u'),
        SHAPE_OPTS,
    ),
    t(
        ToolId::Polygon,
        "Polygon",
        ToolGroup::Draw,
        "shape-polygon",
        Cursor::Crosshair,
        Some('u'),
        &[
            c("mode", "Mode", &["Shape Layer", "Rasterize"], 0),
            i("sides", "Sides", 3, 100, 6),
        ],
    ),
    t(
        ToolId::Star,
        "Star",
        ToolGroup::Draw,
        "shape-star",
        Cursor::Crosshair,
        Some('u'),
        &[
            c("mode", "Mode", &["Shape Layer", "Rasterize"], 0),
            i("points", "Points", 3, 100, 5),
            f("inner_ratio", "Indent", 0.05, 1.0, 0.4),
        ],
    ),
    t(
        ToolId::Line,
        "Line",
        ToolGroup::Draw,
        "shape-line",
        Cursor::Crosshair,
        Some('u'),
        &[
            c("mode", "Mode", &["Shape Layer", "Rasterize"], 0),
            f("width", "Weight", 0.1, 500.0, 2.0),
        ],
    ),
    t(
        ToolId::CustomShape,
        "Custom Shape",
        ToolGroup::Draw,
        "shape-custom",
        Cursor::Crosshair,
        Some('u'),
        SHAPE_OPTS,
    ),
    t(
        ToolId::Hand,
        "Hand",
        ToolGroup::Navigate,
        "hand",
        Cursor::OpenHand,
        Some('h'),
        &[],
    ),
    t(
        ToolId::Zoom,
        "Zoom",
        ToolGroup::Navigate,
        "zoom",
        Cursor::ZoomIn,
        Some('z'),
        &[],
    ),
    t(
        ToolId::RotateView,
        "Rotate View",
        ToolGroup::Navigate,
        "rotate-view",
        Cursor::Rotate,
        Some('r'),
        &[],
    ),
    t(
        ToolId::FreeTransform,
        "Free Transform",
        ToolGroup::Transform,
        "transform",
        Cursor::Arrow,
        Some('t'),
        &[c(
            "mode",
            "Mode",
            &["Scale", "Rotate", "Skew", "Distort", "Perspective", "Warp"],
            0,
        )],
    ),
];

/// Every tool, in palette order.
pub fn all() -> &'static [ToolInfo] {
    TOOLS
}

/// One tool's metadata.
pub fn info(id: ToolId) -> Option<&'static ToolInfo> {
    TOOLS.iter().find(|t| t.id == id)
}

/// The tools a shortcut key cycles through, in palette order.
pub fn by_shortcut(key: char) -> Vec<ToolId> {
    let key = key.to_ascii_lowercase();
    TOOLS
        .iter()
        .filter(|t| t.shortcut == Some(key))
        .map(|t| t.id)
        .collect()
}

/// The next tool that `key` selects, given what is active now.
///
/// Pressing the key repeatedly walks the group; pressing it when something
/// outside the group is active jumps to the group's first member.
pub fn cycle(key: char, current: Option<ToolId>) -> Option<ToolId> {
    let group = by_shortcut(key);
    if group.is_empty() {
        return None;
    }
    match current.and_then(|c| group.iter().position(|g| *g == c)) {
        Some(i) => Some(group[(i + 1) % group.len()]),
        None => Some(group[0]),
    }
}

/// Build a live instance of a tool with its default settings.
///
/// Matched exhaustively with no wildcard: a new [`ToolId`] fails to compile
/// here until it has an implementation, which is the whole reason the registry
/// exists.
pub fn make(id: ToolId) -> Box<dyn Tool> {
    fn brush(size: f32, hardness: f32, spacing: f32) -> BrushSettings {
        BrushSettings {
            size,
            hardness,
            spacing,
            ..BrushSettings::default()
        }
    }
    match id {
        ToolId::Move => Box::new(MoveTool::default()),
        ToolId::RectMarquee => Box::new(MarqueeTool::new(MarqueeShape::Rect)),
        ToolId::EllipseMarquee => Box::new(MarqueeTool::new(MarqueeShape::Ellipse)),
        ToolId::SingleRowMarquee => Box::new(MarqueeTool::new(MarqueeShape::SingleRow)),
        ToolId::SingleColumnMarquee => Box::new(MarqueeTool::new(MarqueeShape::SingleColumn)),
        ToolId::Lasso => Box::new(LassoTool::new(LassoKind::Freehand)),
        ToolId::PolygonalLasso => Box::new(LassoTool::new(LassoKind::Polygonal)),
        ToolId::MagneticLasso => Box::new(LassoTool::new(LassoKind::Magnetic)),
        ToolId::MagicWand => Box::new(WandTool::new(WandKind::Magic)),
        ToolId::QuickSelect => Box::new(WandTool::new(WandKind::Quick)),
        ToolId::Crop => Box::new(CropTool::default()),
        ToolId::Slice => Box::new(SliceTool::default()),
        ToolId::Eyedropper => Box::new(EyedropperTool::default()),
        ToolId::SpotHealing => Box::new(StrokeTool::new(
            id,
            brush(30.0, 0.6, 0.05),
            StrokeOp::SpotHealing,
        )),
        ToolId::HealingBrush => Box::new(StrokeTool::new(
            id,
            brush(40.0, 0.5, 0.05),
            StrokeOp::Healing { softness: 4.0 },
        )),
        ToolId::Patch => Box::new(PatchTool::default()),
        ToolId::RedEye => Box::new(RedEyeTool::default()),
        ToolId::Brush => Box::new(StrokeTool::new(
            id,
            BrushSettings::default(),
            StrokeOp::Paint {
                color: [0.0, 0.0, 0.0, 1.0],
            },
        )),
        ToolId::Pencil => Box::new(StrokeTool::new(
            id,
            BrushSettings::pencil(1.0),
            StrokeOp::Paint {
                color: [0.0, 0.0, 0.0, 1.0],
            },
        )),
        ToolId::ColorReplacement => Box::new(StrokeTool::new(
            id,
            brush(30.0, 0.8, 0.1),
            StrokeOp::ColorReplacement {
                color: [0.0, 0.0, 0.0, 1.0],
                tolerance: 30.0 / 255.0,
            },
        )),
        ToolId::CloneStamp => {
            let mut t = StrokeTool::new(id, brush(40.0, 0.5, 0.05), StrokeOp::CloneStamp);
            t.clone.aligned = true;
            Box::new(t)
        }
        ToolId::PatternStamp => Box::new(StrokeTool::new(
            id,
            brush(40.0, 0.5, 0.1),
            StrokeOp::PatternStamp,
        )),
        ToolId::Eraser => Box::new(StrokeTool::new(
            id,
            BrushSettings::default(),
            StrokeOp::Erase,
        )),
        ToolId::BackgroundEraser => Box::new(StrokeTool::new(
            id,
            brush(40.0, 1.0, 0.1),
            StrokeOp::BackgroundErase {
                tolerance: 30.0 / 255.0,
            },
        )),
        ToolId::MagicEraser => Box::new(MagicEraserTool::default()),
        ToolId::Gradient => Box::new(GradientTool::default()),
        ToolId::PaintBucket => Box::new(PaintBucketTool::new(
            FillSettings::default(),
            FillContent::Foreground,
        )),
        ToolId::PatternFill => Box::new(PatternFillTool::default()),
        ToolId::Blur => Box::new(StrokeTool::new(
            id,
            brush(40.0, 0.0, 0.05),
            StrokeOp::Blur { radius: 3.0 },
        )),
        ToolId::Sharpen => Box::new(StrokeTool::new(
            id,
            brush(40.0, 0.0, 0.05),
            StrokeOp::Sharpen {
                amount: 1.0,
                radius: 1.5,
            },
        )),
        ToolId::Smudge => Box::new(StrokeTool::new(
            id,
            brush(40.0, 0.0, 0.05),
            StrokeOp::Smudge { strength: 0.5 },
        )),
        ToolId::Dodge => Box::new(StrokeTool::new(
            id,
            brush(60.0, 0.0, 0.05),
            StrokeOp::Dodge {
                exposure: 0.25,
                range: ToneRange::Midtones,
            },
        )),
        ToolId::Burn => Box::new(StrokeTool::new(
            id,
            brush(60.0, 0.0, 0.05),
            StrokeOp::Burn {
                exposure: 0.25,
                range: ToneRange::Midtones,
            },
        )),
        ToolId::Sponge => Box::new(StrokeTool::new(
            id,
            brush(60.0, 0.0, 0.05),
            StrokeOp::Sponge {
                amount: 0.3,
                mode: SpongeMode::Desaturate,
            },
        )),
        ToolId::Rectangle => Box::new(ShapeTool::new(ShapeKind::Rectangle, ShapeMode::VectorLayer)),
        ToolId::RoundedRectangle => Box::new(ShapeTool::new(
            ShapeKind::RoundedRectangle { radius: 8.0 },
            ShapeMode::VectorLayer,
        )),
        ToolId::Ellipse => Box::new(ShapeTool::new(ShapeKind::Ellipse, ShapeMode::VectorLayer)),
        ToolId::Polygon => Box::new(ShapeTool::new(
            ShapeKind::Polygon { sides: 6 },
            ShapeMode::VectorLayer,
        )),
        ToolId::Star => Box::new(ShapeTool::new(
            ShapeKind::Star {
                points: 5,
                inner_ratio: 0.4,
            },
            ShapeMode::VectorLayer,
        )),
        ToolId::Line => Box::new(ShapeTool::new(
            ShapeKind::Line { width: 2.0 },
            ShapeMode::VectorLayer,
        )),
        ToolId::CustomShape => Box::new(ShapeTool::new(
            ShapeKind::Custom {
                path: vector::shapes::rect(vector::Bounds::new(
                    vector::point(0.0, 0.0),
                    vector::point(1.0, 1.0),
                )),
                name: "Custom Shape".into(),
            },
            ShapeMode::VectorLayer,
        )),
        ToolId::Hand => Box::new(ViewTool::new(ViewGesture::Pan)),
        ToolId::Zoom => Box::new(ViewTool::new(ViewGesture::Zoom)),
        ToolId::RotateView => Box::new(ViewTool::new(ViewGesture::Rotate)),
        ToolId::FreeTransform => Box::new(TransformTool::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use crate::tool::{PointerEvent, ToolContext};
    use raster::PixelRect;

    #[test]
    fn the_registry_covers_every_tool_id_exactly_once() {
        assert_eq!(TOOLS.len(), ToolId::ALL.len());
        for id in ToolId::ALL {
            let hits = TOOLS.iter().filter(|t| t.id == *id).count();
            assert_eq!(hits, 1, "{id:?} appears {hits} times in the registry");
            assert!(info(*id).is_some());
        }
    }

    #[test]
    fn names_and_icons_are_present_and_unique() {
        for t in TOOLS {
            assert!(!t.name.is_empty(), "{:?} has no name", t.id);
            assert!(!t.icon.is_empty(), "{:?} has no icon", t.id);
        }
        let mut icons: Vec<&str> = TOOLS.iter().map(|t| t.icon).collect();
        icons.sort_unstable();
        let before = icons.len();
        icons.dedup();
        assert_eq!(before, icons.len(), "two tools share an icon key");
    }

    #[test]
    fn a_shortcut_key_cycles_through_its_group_and_nothing_else() {
        let brushes = by_shortcut('b');
        assert_eq!(
            brushes,
            vec![ToolId::Brush, ToolId::Pencil, ToolId::ColorReplacement]
        );
        // Case-insensitive.
        assert_eq!(by_shortcut('B'), brushes);
        assert_eq!(cycle('b', None), Some(ToolId::Brush));
        assert_eq!(cycle('b', Some(ToolId::Brush)), Some(ToolId::Pencil));
        assert_eq!(
            cycle('b', Some(ToolId::ColorReplacement)),
            Some(ToolId::Brush)
        );
        // A key nothing claims selects nothing.
        assert_eq!(cycle('q', None), None);
        // A tool from another group jumps to the head of this one.
        assert_eq!(cycle('b', Some(ToolId::Hand)), Some(ToolId::Brush));
    }

    #[test]
    fn every_option_default_sits_inside_its_own_range() {
        for t in TOOLS {
            for o in t.options {
                match o.kind {
                    OptionKind::Float { min, max, default } => assert!(
                        min <= default && default <= max && min < max,
                        "{:?}.{} default {default} outside {min}..{max}",
                        t.id,
                        o.key
                    ),
                    OptionKind::Int { min, max, default } => assert!(
                        min <= default && default <= max && min < max,
                        "{:?}.{} default {default} outside {min}..{max}",
                        t.id,
                        o.key
                    ),
                    OptionKind::Choice { choices, default } => assert!(
                        default < choices.len() && !choices.is_empty(),
                        "{:?}.{} default {default} has no choice",
                        t.id,
                        o.key
                    ),
                    OptionKind::Bool { .. } | OptionKind::Color { .. } => {}
                }
                assert!(!o.key.is_empty() && !o.label.is_empty());
            }
            // Option keys are unique within a tool, or the UI cannot address them.
            let mut keys: Vec<&str> = t.options.iter().map(|o| o.key).collect();
            keys.sort_unstable();
            let before = keys.len();
            keys.dedup();
            assert_eq!(before, keys.len(), "{:?} repeats an option key", t.id);
        }
    }

    #[test]
    fn every_tool_can_be_constructed_and_cancelled_without_panicking() {
        for id in ToolId::ALL {
            let mut tiles = MemoryTiles::new();
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64));
            let mut tool = make(*id);
            assert_eq!(tool.id(), *id, "make({id:?}) built the wrong tool");
            assert!(!tool.is_active(), "{id:?} starts active");
            // A full gesture with no active layer: some tools will report an
            // error, none may panic, and none may leave a command behind.
            let _ = tool.on_pointer_down(&mut ctx, PointerEvent::at(8.0, 8.0));
            let _ = tool.on_pointer_move(&mut ctx, PointerEvent::at(24.0, 20.0));
            tool.cancel(&mut ctx);
            assert!(!tool.is_active(), "{id:?} still active after cancel");
            assert!(
                ctx.commands().is_empty(),
                "{id:?} emitted a command from a cancelled gesture"
            );
            assert!(
                ctx.selection_edits().is_empty(),
                "{id:?} emitted a selection edit from a cancelled gesture"
            );
            // And it is reusable afterwards.
            let _ = tool.on_pointer_down(&mut ctx, PointerEvent::at(4.0, 4.0));
            tool.cancel(&mut ctx);
        }
    }
}
