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
use crate::pen::{Combine, PenMode};
use crate::select::{LassoKind, LassoTool, MarqueeShape, MarqueeTool, WandKind, WandTool};
use crate::shape::{PaintSource, ShapeKind, ShapeMode, ShapeTool, DEFAULT_STROKE_WIDTH};
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

const fn col(key: &'static str, label: &'static str, default: [f32; 4]) -> OptionSpec {
    OptionSpec {
        key,
        label,
        kind: OptionKind::Color { default },
    }
}

/// The five paint controls every shape tool and the pen share, after the
/// tool's own leading controls — the keys [`crate::shape::PAINT_KEYS`] names
/// and [`crate::shape::ShapePaint::set`] answers. The defaults are
/// [`crate::shape::ShapePaint::default`]'s: filled with the foreground,
/// unstroked. A colour swatch is read only while its source is `Custom`
/// ([`crate::shape::ShapePaint::fill_rgba`]); the labels say so, so a user who
/// picks a swatch with the source on `Foreground` is told why the shape still
/// draws in the foreground colour.
macro_rules! with_paint {
    ($($lead:expr),* $(,)?) => {
        &[
            $($lead,)*
            c("fill", "Fill", PaintSource::CHOICES, 1),
            col("fill_color", "Custom Fill Colour", [0.0, 0.0, 0.0, 1.0]),
            c("stroke", "Stroke", PaintSource::CHOICES, 0),
            col("stroke_color", "Custom Stroke Colour", [0.0, 0.0, 0.0, 1.0]),
            f("stroke_width", "Stroke Width", 0.0, 500.0, DEFAULT_STROKE_WIDTH),
            // W9-F: stroke alignment, cap, join and dash / gap (in widths).
            c("stroke_align", "Align", crate::shape::STROKE_ALIGN_CHOICES, 1),
            c("stroke_cap", "Caps", crate::shape::STROKE_CAP_CHOICES, 0),
            c("stroke_join", "Corners", crate::shape::STROKE_JOIN_CHOICES, 0),
            f("stroke_dash", "Dash (0 = solid)", 0.0, 100.0, 0.0),
            f("stroke_gap", "Gap", 0.0, 100.0, 0.0),
        ]
    };
}

/// A shape tool's options: the mode, From Centre, the kind's own geometry
/// keys, then the paint. W9-F: the mode offers Path (the outline becomes the
/// Work Path) and the fill can be a colour, the gradient or the pattern.
macro_rules! shape_opts {
    ($($extra:expr),* $(,)?) => {
        with_paint!(
            c("mode", "Mode", ShapeMode::CHOICES, 0),
            b("from_center", "From Centre", false),
            $($extra,)*
            c("fill_type", "Fill Type", crate::shape::FillType::CHOICES, 0),
        )
    };
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
    // Spelled out rather than written with an arrow. These are checkbox
    // captions painted straight into the options bar by
    // `ui::view::toolbar`, and egui 0.29's font stack has no glyph for
    // U+2192, so "Pressure -> Size" written with the arrow character came out
    // as two empty squares whenever the Brush or the Eraser was selected.
    b("size_pressure", "Size from Pressure", true),
    b("flow_pressure", "Flow from Pressure", false),
    b("opacity_pressure", "Opacity from Pressure", false),
];

/// W9-L: the selection Mode every selection tool offers — New, Add,
/// Subtract, Intersect and Exclude (XOR), index for index with
/// [`crate::select::SELECTION_MODES`].
const SELECTION_MODE: OptionSpec = c("mode", "Mode", crate::select::SELECTION_MODE_LABELS, 0);

const SELECTION_OPTS: &[OptionSpec] = &[
    SELECTION_MODE,
    f("feather", "Feather", 0.0, 250.0, 0.0),
    b("antialias", "Anti-alias", true),
];

/// W9-L: the rectangular and elliptical marquees add Photopea's Style —
/// Normal, Fixed Ratio (W : H) or Fixed Size (W x H px) — to the shared
/// selection controls.
const MARQUEE_OPTS: &[OptionSpec] = &[
    SELECTION_MODE,
    f("feather", "Feather", 0.0, 250.0, 0.0),
    b("antialias", "Anti-alias", true),
    c("style", "Style", crate::select::MARQUEE_STYLE_LABELS, 0),
    f(
        "style_width",
        "W",
        0.01,
        crate::select::MARQUEE_STYLE_MAX_PX,
        crate::select::MARQUEE_STYLE_DEFAULT_PX,
    ),
    f(
        "style_height",
        "H",
        0.01,
        crate::select::MARQUEE_STYLE_MAX_PX,
        crate::select::MARQUEE_STYLE_DEFAULT_PX,
    ),
];

const WAND_OPTS: &[OptionSpec] = &[
    SELECTION_MODE,
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
    // W9-D: where the stamp reads (Current Layer / Current & Below / All
    // Layers); it always writes the active layer.
    SAMPLE_OPT,
];

/// W9-D: the Sample choice the Clone Stamp, the healing brushes, Blur,
/// Sharpen and Smudge share ([`crate::tool::SampleLayers`]).
const SAMPLE_OPT: OptionSpec = c(
    crate::tool::SAMPLE_LAYERS_KEY,
    "Sample",
    crate::tool::SampleLayers::CHOICES,
    0,
);

const TONE_OPTS: &[OptionSpec] = &[
    f("size", "Size", 1.0, 5000.0, 60.0),
    f("hardness", "Hardness", 0.0, 1.0, 0.0),
    f("exposure", "Exposure", 0.0, 1.0, 0.25),
    c("range", "Range", &["Shadows", "Midtones", "Highlights"], 1),
    // W11-I: Photoshop/Photopea's Protect Tones, on by default.
    b(
        crate::stroke::PROTECT_TONES_KEY,
        "Protect Tones",
        TONE_PROTECT_DEFAULT,
    ),
];

/// W11-I: Protect Tones and Vibrance start on, as in Photoshop; the tools
/// [`make`] builds start in the same state.
const TONE_PROTECT_DEFAULT: bool = true;
const SPONGE_VIBRANCE_DEFAULT: bool = true;

const SHAPE_OPTS: &[OptionSpec] = shape_opts!();

/// W9-K: the generic families every Font list starts with, in index order.
pub const GENERIC_FONT_FAMILIES: &[&str] = &["sans-serif", "serif", "monospace"];

/// W9-K: the live Type-tool Font list: [`GENERIC_FONT_FAMILIES`], then every
/// installed family registered so far, in registration order.
static FONT_CHOICES: std::sync::RwLock<&'static [&'static str]> =
    std::sync::RwLock::new(GENERIC_FONT_FAMILIES);

/// W9-K: the Type tools' Font choices right now. The options bar draws this
/// list and [`crate::text::TypeTool`] maps a held index through it, so the
/// two agree by construction.
pub fn type_font_choices() -> &'static [&'static str] {
    *FONT_CHOICES
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// W9-K: add installed families to the Font list and return the list.
///
/// Append-only - a family already listed keeps its index, so an index the
/// options bar already holds never starts naming a different family when a
/// font is loaded later. The list only grows when a name is new; each growth
/// leaks one small slice (fonts are added a handful of times a session).
pub fn register_font_families<S: AsRef<str>>(
    names: impl IntoIterator<Item = S>,
) -> &'static [&'static str] {
    let mut guard = FONT_CHOICES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut list: Vec<&'static str> = guard.to_vec();
    let before = list.len();
    for name in names {
        let name = name.as_ref();
        if !name.is_empty() && !list.contains(&name) {
            list.push(Box::leak(name.to_owned().into_boxed_str()));
        }
    }
    if list.len() != before {
        *guard = Box::leak(list.into_boxed_slice());
    }
    *guard
}

/// W9-K: the Type tools' Font option with the live choices in place of the
/// static table's three.
pub fn type_font_spec() -> OptionSpec {
    OptionSpec {
        key: "font_family",
        label: "Font",
        kind: OptionKind::Choice {
            choices: type_font_choices(),
            default: 0,
        },
    }
}

/// W9-N: the Custom Shape tool's Shape option key.
pub const CUSTOM_SHAPE_KEY: &str = "preset";

/// W9-N: the built-in library's labels, the start of the live Shape list.
const BUILTIN_SHAPE_NAMES: &[&str] = &vector::CUSTOM_SHAPE_NAMES;

/// W9-N: the live Custom Shape list - the built-in library's labels, then
/// every custom shape a `.csh` import registered, in registration order -
/// and the imported shapes' paths, in that list's order past the built-in
/// entries. One lock, so a reader never sees a name without its path.
static CUSTOM_SHAPES: std::sync::RwLock<(&'static [&'static str], Vec<vector::Path>)> =
    std::sync::RwLock::new((BUILTIN_SHAPE_NAMES, Vec::new()));

/// W9-N: the Custom Shape tool's Shape choices right now. The options bar
/// draws this list and [`crate::shape::ShapeTool`] maps a held index through
/// [`custom_shape_at`], so the two agree by construction.
pub fn custom_shape_choices() -> &'static [&'static str] {
    CUSTOM_SHAPES
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .0
}

/// W9-N: add imported custom shapes to the Shape list and return the list.
///
/// Append-only, like [`register_font_families`]: a name already listed keeps
/// its index (its path is replaced, so a re-import of an edited library draws
/// the new outline), so an index the options bar holds never starts naming a
/// different shape. A path that is empty, non-finite or has zero width or
/// height is skipped (the tool fits the path's bounds into the drag box), and
/// so is a name that is a built-in entry's.
pub fn register_custom_shapes<S: AsRef<str>>(
    shapes: impl IntoIterator<Item = (S, vector::Path)>,
) -> &'static [&'static str] {
    let mut guard = CUSTOM_SHAPES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (names, paths) = &mut *guard;
    let mut list: Vec<&'static str> = names.to_vec();
    let before = list.len();
    for (name, path) in shapes {
        let name = name.as_ref();
        let b = path.bounds();
        let usable = !name.is_empty()
            && !path.is_empty()
            && path.is_finite()
            && b.max.x - b.min.x > 0.0
            && b.max.y - b.min.y > 0.0;
        if !usable || BUILTIN_SHAPE_NAMES.contains(&name) {
            continue;
        }
        match list.iter().position(|n| *n == name) {
            Some(at) => {
                if let Some(slot) = paths.get_mut(at - BUILTIN_SHAPE_NAMES.len()) {
                    *slot = path;
                }
            }
            None => {
                list.push(Box::leak(name.to_owned().into_boxed_str()));
                paths.push(path);
            }
        }
    }
    if list.len() != before {
        *names = Box::leak(list.into_boxed_slice());
    }
    names
}

/// W9-N: [`register_custom_shapes`] from SVG path data (an imported shape's
/// unit-square outline); an outline that does not parse is skipped.
pub fn register_custom_shape_outlines<S: AsRef<str>, D: AsRef<str>>(
    outlines: impl IntoIterator<Item = (S, D)>,
) -> &'static [&'static str] {
    register_custom_shapes(
        outlines
            .into_iter()
            .filter_map(|(name, d)| Some((name, vector::parse_svg(d.as_ref()).ok()?))),
    )
}

/// W9-N: the shape a Shape choice `index` names - a built-in entry, or an
/// imported one past them. An index past the end clamps to the last entry,
/// the options bar's own `conform` rule.
pub fn custom_shape_at(index: usize) -> (String, vector::Path) {
    let builtin = vector::CustomShape::ALL.len();
    if index >= builtin {
        let guard = CUSTOM_SHAPES
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (names, paths) = &*guard;
        if let Some(last) = paths.len().checked_sub(1) {
            let at = (index - builtin).min(last);
            if let Some(name) = names.get(builtin + at) {
                return ((*name).to_owned(), paths[at].clone());
            }
        }
    }
    let last = vector::CustomShape::ALL[builtin - 1];
    let shape = vector::CustomShape::from_index(index).unwrap_or(last);
    (shape.name().to_owned(), shape.path())
}

/// W9-N: the Custom Shape tool's Shape option with the live choices in place
/// of the static table's built-in ten.
pub fn custom_shape_spec() -> OptionSpec {
    OptionSpec {
        key: CUSTOM_SHAPE_KEY,
        label: "Shape",
        kind: OptionKind::Choice {
            choices: custom_shape_choices(),
            default: 0,
        },
    }
}

/// W8-C: the Type tools' options (face, size and default style), with
/// `$extra` in front — every Type tool shares this list, and the two Type
/// Mask tools add the selection Mode their confirm combines with
/// ([`TYPE_MASK_OPTS`]).
macro_rules! type_opts {
    ($($extra:expr),* $(,)?) => { &[
    $($extra,)*
    f("size_px", "Size", 4.0, 512.0, 24.0),
    // The three CSS generic families, which `text_engine` resolves to
    // installed fonts (W1-B2). W9-K: this static table is only the
    // start of the list - the live Font choice is [`type_font_choices`]
    // (these three, then every installed family the UI registered with
    // [`register_font_families`]), which the options bar draws and
    // `text::TypeTool::set_setting` indexes.
    c("font_family", "Font", GENERIC_FONT_FAMILIES, 0),
    // W3-J: the Type tool's DEFAULT style - what the Character and
    // Paragraph panels edit with no text layer selected, held on the
    // workspace like every option and seeded into the next layer the
    // tool creates (`text::TypeTool::seed`). The keys and their
    // mapping live in `text::TYPE_STYLE_KEYS`.
    c("weight", "Weight", crate::text::TYPE_WEIGHTS, 3),
    b("italic", "Italic", false),
    b("underline", "Underline", false),
    b("strikethrough", "Strike", false),
    col("color", "Color", [0.0, 0.0, 0.0, 1.0]),
    f("tracking", "Tracking", -100.0, 400.0, 0.0),
    f("leading", "Leading (0 = auto)", 0.0, 400.0, 0.0),
    f("horizontal_scale", "H scale %", 1.0, 1000.0, 100.0),
    f("vertical_scale", "V scale %", 1.0, 1000.0, 100.0),
    f("baseline_shift", "Baseline", -200.0, 200.0, 0.0),
    c("script", "Position", crate::text::TYPE_SCRIPTS, 0),
    c("caps", "Caps", crate::text::TYPE_CAPS, 0),
    b("kerning", "Metrics kerning", true),
    b("ligatures", "Ligatures", true),
    c("anti_alias", "Edges", crate::text::TYPE_ANTI_ALIAS, 0),
    c("alignment", "Align", crate::text::TYPE_ALIGNMENTS, 0),
    f("left_indent", "Indent left", -200.0, 1000.0, 0.0),
    f("right_indent", "Indent right", -200.0, 1000.0, 0.0),
    f("first_line_indent", "First line", -200.0, 1000.0, 0.0),
    f("space_before", "Space before", 0.0, 1000.0, 0.0),
    f("space_after", "Space after", 0.0, 1000.0, 0.0),
] };
}

/// The Horizontal and Vertical Type tools' options (W7-F): the same face,
/// size and default style, with no selection Mode.
const TYPE_OPTS: &[OptionSpec] = type_opts!();

/// W8-C: a Type Mask confirm combines its glyphs with the selection by this
/// Mode, the one every selection tool offers.
const TYPE_MASK_OPTS: &[OptionSpec] = type_opts!(c(
    "mode",
    "Mode",
    &["New", "Add", "Subtract", "Intersect"],
    0
));

/// Everything the UI needs to know about one tool.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToolInfo {
    pub id: ToolId,
    pub name: &'static str,
    pub group: ToolGroup,
    /// The palette slot this tool shares: tools with the same id are one
    /// button with a fly-out, in the order they appear in [`all`], whatever
    /// their shortcuts say (Blur/Sharpen/Smudge have none and still share a
    /// slot; Rotate View sits under Hand on `R`). `None` keeps a tool off the
    /// palette altogether — Free Transform is a menu item and `Ctrl+T`, as in
    /// Photopea, not a button.
    pub slot: Option<&'static str>,
    /// Icon key; the UI resolves it against its own icon set.
    pub icon: &'static str,
    pub cursor: Cursor,
    /// The key that selects this tool. Tools sharing a key form a cycle group,
    /// in the order they appear in [`all`].
    pub shortcut: Option<char>,
    pub options: &'static [OptionSpec],
}

// Eight positional arguments, one per `ToolInfo` field, for a const table
// that is read top to bottom: a builder or a struct literal per entry would
// be fifty more `ToolInfo {` blocks for no extra safety.
#[allow(clippy::too_many_arguments)]
const fn t(
    id: ToolId,
    name: &'static str,
    group: ToolGroup,
    slot: Option<&'static str>,
    icon: &'static str,
    cursor: Cursor,
    shortcut: Option<char>,
    options: &'static [OptionSpec],
) -> ToolInfo {
    ToolInfo {
        id,
        name,
        group,
        slot,
        icon,
        cursor,
        shortcut,
        options,
    }
}

/// The option keys every BRUSH-driven tool shares; they travel through
/// `Tool::set_brush` (the options bar writes them, the shell folds them into
/// the tool's [`BrushSettings`]) and never through `set_setting`. The shell's
/// boundary filter mirrors this list.
pub const BRUSH_OPTION_KEYS: &[&str] = &[
    "size",
    "hardness",
    "spacing",
    "angle",
    "roundness",
    "opacity",
    "flow",
    "smoothing",
    "size_pressure",
    "flow_pressure",
    "opacity_pressure",
];

const TOOLS: &[ToolInfo] = &[
    t(
        ToolId::Move,
        "Move",
        ToolGroup::Select,
        Some("move"),
        "move",
        Cursor::Move,
        Some('v'),
        &[
            b("auto_select", "Auto-Select", false),
            b("select_groups", "Select Groups", false),
            b("show_transform", "Show Transform Controls", false),
        ],
    ),
    // W7-F: Photoshop's Artboard tool shares the Move slot and `V`.
    t(
        ToolId::Artboard,
        "Artboard",
        ToolGroup::Select,
        Some("move"),
        "artboard",
        Cursor::Crosshair,
        Some('v'),
        &[c(
            "background",
            "Background",
            crate::artboard::BACKGROUND_CHOICES,
            0,
        )],
    ),
    t(
        ToolId::RectMarquee,
        "Rectangular Marquee",
        ToolGroup::Select,
        Some("marquee"),
        "marquee-rect",
        Cursor::Crosshair,
        Some('m'),
        MARQUEE_OPTS,
    ),
    t(
        ToolId::EllipseMarquee,
        "Elliptical Marquee",
        ToolGroup::Select,
        Some("marquee"),
        "marquee-ellipse",
        Cursor::Crosshair,
        Some('m'),
        MARQUEE_OPTS,
    ),
    t(
        ToolId::SingleRowMarquee,
        "Single Row Marquee",
        ToolGroup::Select,
        Some("marquee"),
        "marquee-row",
        Cursor::Crosshair,
        Some('m'),
        SELECTION_OPTS,
    ),
    t(
        ToolId::SingleColumnMarquee,
        "Single Column Marquee",
        ToolGroup::Select,
        Some("marquee"),
        "marquee-column",
        Cursor::Crosshair,
        Some('m'),
        SELECTION_OPTS,
    ),
    t(
        ToolId::Lasso,
        "Lasso",
        ToolGroup::Select,
        Some("lasso"),
        "lasso",
        Cursor::Crosshair,
        Some('l'),
        SELECTION_OPTS,
    ),
    t(
        ToolId::PolygonalLasso,
        "Polygonal Lasso",
        ToolGroup::Select,
        Some("lasso"),
        "lasso-poly",
        Cursor::Crosshair,
        Some('l'),
        SELECTION_OPTS,
    ),
    t(
        ToolId::MagneticLasso,
        "Magnetic Lasso",
        ToolGroup::Select,
        Some("lasso"),
        "lasso-magnetic",
        Cursor::Crosshair,
        Some('l'),
        &[
            SELECTION_MODE,
            i("search_radius", "Width", 1, 256, 24),
            f("edge_weight", "Contrast", 0.0, 4.0, 1.0),
        ],
    ),
    t(
        ToolId::MagicWand,
        "Magic Wand",
        ToolGroup::Select,
        Some("wand"),
        "wand",
        Cursor::Crosshair,
        Some('w'),
        WAND_OPTS,
    ),
    t(
        ToolId::QuickSelect,
        "Quick Selection",
        ToolGroup::Select,
        Some("wand"),
        "quick-select",
        Cursor::BrushRing,
        Some('w'),
        &[
            SELECTION_MODE,
            f("radius", "Size", 1.0, 500.0, 8.0),
            f("tolerance", "Tolerance", 0.0, 1.0, 16.0 / 255.0),
        ],
    ),
    // W11-G: third in the wand slot, as in Photopea / Photoshop.
    t(
        ToolId::ObjectSelection,
        "Object Selection",
        ToolGroup::Select,
        Some("wand"),
        "object-select",
        Cursor::Crosshair,
        Some('w'),
        &[SELECTION_MODE],
    ),
    t(
        ToolId::Crop,
        "Crop",
        ToolGroup::Crop,
        Some("crop"),
        "crop",
        Cursor::CropMarks,
        Some('c'),
        &[
            // W4-D: Photopea's crop bar — a ratio preset, the W x H x
            // Resolution fields the last preset reads, the overlay, the
            // Straighten line mode and Delete Cropped Pixels.
            c("ratio", "Ratio", &crate::edit::CROP_RATIO_LABELS, 0),
            f("width", "W", 0.001, crate::edit::CROP_MAX_OUTPUT_PX, 1920.0),
            f(
                "height",
                "H",
                0.001,
                crate::edit::CROP_MAX_OUTPUT_PX,
                1080.0,
            ),
            c("units", "Units", &["px", "in"], 0),
            f("resolution", "Resolution (px/in)", 1.0, 10_000.0, 72.0),
            c("overlay", "Overlay", &crate::edit::CROP_OVERLAY_LABELS, 1),
            b("straighten_line", "Straighten", false),
            b("delete_cropped", "Delete Cropped Pixels", false),
        ],
    ),
    // W7-F: second in the Crop slot, as in Photopea.
    t(
        ToolId::PerspectiveCrop,
        "Perspective Crop",
        ToolGroup::Crop,
        Some("crop"),
        "crop-perspective",
        Cursor::CropMarks,
        Some('c'),
        &[
            // 0 = from the quad's own edge lengths.
            i(
                "width",
                "W (0 = auto)",
                0,
                crate::perspective_crop::MAX_OUTPUT_PX,
                0,
            ),
            i(
                "height",
                "H (0 = auto)",
                0,
                crate::perspective_crop::MAX_OUTPUT_PX,
                0,
            ),
        ],
    ),
    t(
        ToolId::Slice,
        "Slice",
        ToolGroup::Crop,
        Some("crop"),
        "slice",
        Cursor::Slice,
        Some('c'),
        &[],
    ),
    // W10-A: after the Slice tool in the Crop slot, as in Photoshop.
    t(
        ToolId::SliceSelect,
        "Slice Select",
        ToolGroup::Crop,
        Some("crop"),
        "slice-select",
        Cursor::Arrow,
        Some('c'),
        &[],
    ),
    t(
        ToolId::Eyedropper,
        "Eyedropper",
        ToolGroup::Crop,
        Some("eyedropper"),
        "eyedropper",
        Cursor::Eyedropper,
        Some('i'),
        &[
            i("sample_radius", "Sample Size", 0, 64, 0),
            b("sample_all_layers", "Sample All Layers", true),
        ],
    ),
    // W4-G: Photoshop's order inside the Eyedropper slot, on the same `I`.
    t(
        ToolId::ColorSampler,
        "Colour Sampler",
        ToolGroup::Crop,
        Some("eyedropper"),
        "color-sampler",
        Cursor::Eyedropper,
        Some('i'),
        &[],
    ),
    t(
        ToolId::Ruler,
        "Ruler",
        ToolGroup::Crop,
        Some("eyedropper"),
        "ruler",
        Cursor::Crosshair,
        Some('i'),
        &[],
    ),
    // W10-B: after the Ruler in the Eyedropper slot, as in Photoshop.
    t(
        ToolId::Note,
        "Note",
        ToolGroup::Crop,
        Some("eyedropper"),
        "note",
        Cursor::Crosshair,
        Some('i'),
        &[],
    ),
    t(
        ToolId::SpotHealing,
        "Spot Healing Brush",
        ToolGroup::Retouch,
        Some("heal"),
        "spot-heal",
        Cursor::BrushRing,
        Some('j'),
        &[
            f("size", "Size", 1.0, 5000.0, 30.0),
            f("hardness", "Hardness", 0.0, 1.0, 0.6),
            // W7-I: Proximity Match diffuses the surroundings inward;
            // Content-Aware synthesises the brushed area by PatchMatch.
            c("type", "Type", &["Proximity Match", "Content-Aware"], 0),
            SAMPLE_OPT,
        ],
    ),
    t(
        ToolId::HealingBrush,
        "Healing Brush",
        ToolGroup::Retouch,
        Some("heal"),
        "heal",
        Cursor::BrushRing,
        Some('j'),
        &[
            f("size", "Size", 1.0, 5000.0, 40.0),
            f("softness", "Softness", 0.5, 64.0, 4.0),
            b("aligned", "Aligned", true),
            SAMPLE_OPT,
        ],
    ),
    t(
        ToolId::Patch,
        "Patch",
        ToolGroup::Retouch,
        Some("heal"),
        "patch",
        Cursor::Crosshair,
        Some('j'),
        &[f("softness", "Softness", 0.5, 64.0, 4.0)],
    ),
    // W10-A: after the Patch tool in the Healing slot, as in Photoshop.
    t(
        ToolId::ContentAwareMove,
        "Content-Aware Move",
        ToolGroup::Retouch,
        Some("heal"),
        "content-aware-move",
        Cursor::Crosshair,
        Some('j'),
        &[
            c(
                crate::content_aware_move::MODE_KEY,
                "Mode",
                crate::content_aware_move::CamMode::CHOICES,
                0,
            ),
            f(
                crate::content_aware_move::ADAPTATION_KEY,
                "Adaptation",
                0.0,
                crate::content_aware_move::MAX_ADAPTATION,
                2.0,
            ),
        ],
    ),
    t(
        ToolId::RedEye,
        "Red Eye",
        ToolGroup::Retouch,
        Some("heal"),
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
        Some("brush"),
        "brush",
        Cursor::BrushRing,
        Some('b'),
        BRUSH_OPTS,
    ),
    t(
        ToolId::Pencil,
        "Pencil",
        ToolGroup::Paint,
        Some("brush"),
        "pencil",
        Cursor::BrushRing,
        Some('b'),
        &[
            f("size", "Size", 1.0, 1000.0, 1.0),
            f("opacity", "Opacity", 0.0, 1.0, 1.0),
            f("spacing", "Spacing", 0.01, 10.0, 0.1),
            // W4-G: a stroke that starts on the foreground paints the
            // background (`crate::pencil::PencilTool`).
            b(crate::pencil::AUTO_ERASE_KEY, "Auto Erase", false),
        ],
    ),
    t(
        ToolId::ColorReplacement,
        "Colour Replacement",
        ToolGroup::Paint,
        Some("brush"),
        "color-replace",
        Cursor::BrushRing,
        Some('b'),
        &[
            f("size", "Size", 1.0, 5000.0, 30.0),
            f("tolerance", "Tolerance", 0.0, 1.0, 30.0 / 255.0),
            f("opacity", "Opacity", 0.0, 1.0, 1.0),
        ],
    ),
    // W7-F: last in the Brush slot, as in Photoshop.
    t(
        ToolId::MixerBrush,
        "Mixer Brush",
        ToolGroup::Paint,
        Some("brush"),
        "mixer-brush",
        Cursor::BrushRing,
        Some('b'),
        &[
            f("size", "Size", 1.0, 5000.0, 30.0),
            f("hardness", "Hardness", 0.0, 1.0, 0.6),
            f("spacing", "Spacing", 0.01, 10.0, 0.1),
            f("opacity", "Opacity", 0.0, 1.0, 1.0),
            f("flow", "Flow", 0.0, 1.0, 1.0),
            f("wet", "Wet", 0.0, 1.0, 0.5),
            f("load", "Load", 0.0, 1.0, 0.5),
            f("mix", "Mix", 0.0, 1.0, 0.5),
            b("load_after", "Load Brush After Each Stroke", true),
            b("clean_after", "Clean Brush After Each Stroke", false),
        ],
    ),
    t(
        ToolId::CloneStamp,
        "Clone Stamp",
        ToolGroup::Paint,
        Some("clone"),
        "clone",
        Cursor::BrushRing,
        Some('s'),
        CLONE_OPTS,
    ),
    t(
        ToolId::PatternStamp,
        "Pattern Stamp",
        ToolGroup::Paint,
        Some("clone"),
        "pattern-stamp",
        Cursor::BrushRing,
        Some('s'),
        &[
            f("size", "Size", 1.0, 5000.0, 40.0),
            f("opacity", "Opacity", 0.0, 1.0, 1.0),
            f("spacing", "Spacing", 0.01, 10.0, 0.1),
        ],
    ),
    // W4-G: Photoshop's own slot, straight after the Clone slot, on `Y`.
    t(
        ToolId::HistoryBrush,
        "History Brush",
        ToolGroup::Paint,
        Some("history"),
        "history-brush",
        Cursor::BrushRing,
        Some('y'),
        &[
            f("size", "Size", 1.0, 5000.0, 24.0),
            f("hardness", "Hardness", 0.0, 1.0, 0.5),
            f("spacing", "Spacing", 0.01, 10.0, 0.1),
            f("opacity", "Opacity", 0.0, 1.0, 1.0),
            // The History panel row to paint from; 0 is the document as
            // opened. The panel's source column writes it too.
            i(
                crate::history_brush::SOURCE_KEY,
                "Source state",
                0,
                10_000,
                0,
            ),
        ],
    ),
    t(
        ToolId::Eraser,
        "Eraser",
        ToolGroup::Paint,
        Some("eraser"),
        "eraser",
        Cursor::BrushRing,
        Some('e'),
        BRUSH_OPTS,
    ),
    t(
        ToolId::BackgroundEraser,
        "Background Eraser",
        ToolGroup::Paint,
        Some("eraser"),
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
        Some("eraser"),
        "eraser-magic",
        Cursor::Crosshair,
        Some('e'),
        FILL_OPTS,
    ),
    t(
        ToolId::Gradient,
        "Gradient",
        ToolGroup::Paint,
        Some("gradient"),
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
            b(
                crate::gradient::GRADIENT_TRANSPARENCY_KEY,
                "Transparency",
                true,
            ),
        ],
    ),
    t(
        ToolId::PaintBucket,
        "Paint Bucket",
        ToolGroup::Paint,
        Some("gradient"),
        "bucket",
        Cursor::Bucket,
        Some('g'),
        FILL_OPTS,
    ),
    t(
        ToolId::PatternFill,
        "Pattern Fill",
        ToolGroup::Paint,
        Some("gradient"),
        "pattern-fill",
        Cursor::Bucket,
        Some('g'),
        &[f("opacity", "Opacity", 0.0, 1.0, 1.0)],
    ),
    t(
        ToolId::Blur,
        "Blur",
        ToolGroup::Retouch,
        Some("blur"),
        "blur",
        Cursor::BrushRing,
        None,
        &[
            f("size", "Size", 1.0, 5000.0, 40.0),
            f("radius", "Strength", 0.1, 64.0, 3.0),
            f("opacity", "Opacity", 0.0, 1.0, 1.0),
            SAMPLE_OPT,
        ],
    ),
    t(
        ToolId::Sharpen,
        "Sharpen",
        ToolGroup::Retouch,
        Some("blur"),
        "sharpen",
        Cursor::BrushRing,
        None,
        &[
            f("size", "Size", 1.0, 5000.0, 40.0),
            f("amount", "Strength", 0.0, 4.0, 1.0),
            f("opacity", "Opacity", 0.0, 1.0, 1.0),
            SAMPLE_OPT,
        ],
    ),
    t(
        ToolId::Smudge,
        "Smudge",
        ToolGroup::Retouch,
        Some("blur"),
        "smudge",
        Cursor::BrushRing,
        None,
        &[
            f("size", "Size", 1.0, 5000.0, 40.0),
            f("strength", "Strength", 0.0, 1.0, 0.5),
            SAMPLE_OPT,
        ],
    ),
    t(
        ToolId::RefineBoundary,
        "Refine Boundary",
        ToolGroup::Retouch,
        Some("blur"),
        "refine-boundary",
        Cursor::BrushRing,
        None,
        &[
            f("size", "Size", 1.0, 5000.0, 40.0),
            f("strength", "Strength", 0.0, 1.0, 0.5),
            f("opacity", "Opacity", 0.0, 1.0, 1.0),
        ],
    ),
    t(
        ToolId::Dodge,
        "Dodge",
        ToolGroup::Retouch,
        Some("tone"),
        "dodge",
        Cursor::BrushRing,
        Some('o'),
        TONE_OPTS,
    ),
    t(
        ToolId::Burn,
        "Burn",
        ToolGroup::Retouch,
        Some("tone"),
        "burn",
        Cursor::BrushRing,
        Some('o'),
        TONE_OPTS,
    ),
    t(
        ToolId::Sponge,
        "Sponge",
        ToolGroup::Retouch,
        Some("tone"),
        "sponge",
        Cursor::BrushRing,
        Some('o'),
        &[
            f("size", "Size", 1.0, 5000.0, 60.0),
            f("amount", "Flow", 0.0, 1.0, 0.3),
            c("mode", "Mode", &["Desaturate", "Saturate"], 0),
            b(
                crate::stroke::VIBRANCE_KEY,
                "Vibrance",
                SPONGE_VIBRANCE_DEFAULT,
            ),
        ],
    ),
    t(
        ToolId::Pen,
        "Pen",
        ToolGroup::Draw,
        Some("pen"),
        "pen",
        Cursor::Crosshair,
        // `P`, the one letter of the brief no tool answered to.
        Some('p'),
        // Mode (Path / Shape / Pixels), the shared paint, and how a closed
        // path combines with the active shape layer. `vector::boolean` does
        // union, difference and intersection, so all four are live.
        with_paint!(
            c("mode", "Mode", PenMode::CHOICES, 1),
            c("combine", "Combine", Combine::CHOICES, 0),
        ),
    ),
    // W7-F: Photoshop's order in the Pen slot, on `P`.
    t(
        ToolId::FreeformPen,
        "Freeform Pen",
        ToolGroup::Draw,
        Some("pen"),
        "pen-freeform",
        Cursor::Crosshair,
        Some('p'),
        with_paint!(
            c("mode", "Mode", PenMode::CHOICES, 1),
            c("combine", "Combine", Combine::CHOICES, 0),
            f(
                "curve_fit",
                "Curve Fit",
                0.5,
                10.0,
                crate::pen::DEFAULT_CURVE_FIT_PX
            ),
        ),
    ),
    t(
        ToolId::CurvaturePen,
        "Curvature Pen",
        ToolGroup::Draw,
        Some("pen"),
        "pen-curvature",
        Cursor::Crosshair,
        Some('p'),
        with_paint!(
            c("mode", "Mode", PenMode::CHOICES, 1),
            c("combine", "Combine", Combine::CHOICES, 0),
        ),
    ),
    // W4-G: the Pen slot's path-editing tools, letterless as in Photoshop.
    t(
        ToolId::AddAnchor,
        "Add Anchor Point",
        ToolGroup::Draw,
        Some("pen"),
        "anchor-add",
        Cursor::Crosshair,
        None,
        &[],
    ),
    t(
        ToolId::DeleteAnchor,
        "Delete Anchor Point",
        ToolGroup::Draw,
        Some("pen"),
        "anchor-delete",
        Cursor::Crosshair,
        None,
        &[],
    ),
    t(
        ToolId::ConvertAnchor,
        "Convert Point",
        ToolGroup::Draw,
        Some("pen"),
        "anchor-convert",
        Cursor::Crosshair,
        None,
        &[],
    ),
    t(
        ToolId::Type,
        "Type",
        ToolGroup::Draw,
        Some("type"),
        "type",
        Cursor::Crosshair,
        // `T` is the Type tool's alone. It used to be shared with Free
        // Transform, so a second press while typing left the text for a
        // transform box; Free Transform is `Ctrl+T` and the Edit menu now.
        Some('t'),
        TYPE_OPTS,
    ),
    // W7-F: the other three type tools, in Photoshop's slot order.
    t(
        ToolId::VerticalType,
        "Vertical Type",
        ToolGroup::Draw,
        Some("type"),
        "type-vertical",
        Cursor::Crosshair,
        Some('t'),
        TYPE_OPTS,
    ),
    t(
        ToolId::VerticalTypeMask,
        "Vertical Type Mask",
        ToolGroup::Draw,
        Some("type"),
        "type-mask-vertical",
        Cursor::Crosshair,
        Some('t'),
        TYPE_MASK_OPTS,
    ),
    t(
        ToolId::HorizontalTypeMask,
        "Horizontal Type Mask",
        ToolGroup::Draw,
        Some("type"),
        "type-mask",
        Cursor::Crosshair,
        Some('t'),
        TYPE_MASK_OPTS,
    ),
    t(
        ToolId::PathSelect,
        "Path Select",
        ToolGroup::Draw,
        Some("path"),
        "path-select",
        Cursor::Crosshair,
        // `A`, Photopea's path-tool letter; Direct Selection shares it and
        // the letter cycles between them.
        Some('a'),
        // W9-F: merge or align the clicked path's components.
        &[
            c(
                "combine",
                "Combine",
                crate::path_select::PathCombine::CHOICES,
                0,
            ),
            c(
                "align",
                "Align Components",
                crate::path_select::PathAlign::CHOICES,
                0,
            ),
        ],
    ),
    t(
        ToolId::DirectSelection,
        "Direct Selection",
        ToolGroup::Draw,
        Some("path"),
        "anchor-block",
        Cursor::Crosshair,
        Some('a'),
        &[],
    ),
    t(
        ToolId::Rectangle,
        "Rectangle",
        ToolGroup::Draw,
        Some("shape"),
        "shape-rect",
        Cursor::Crosshair,
        Some('u'),
        SHAPE_OPTS,
    ),
    t(
        ToolId::RoundedRectangle,
        "Rounded Rectangle",
        ToolGroup::Draw,
        Some("shape"),
        "shape-rrect",
        Cursor::Crosshair,
        Some('u'),
        shape_opts!(f("radius", "Radius", 0.0, 500.0, 8.0)),
    ),
    t(
        ToolId::Ellipse,
        "Ellipse",
        ToolGroup::Draw,
        Some("shape"),
        "shape-ellipse",
        Cursor::Crosshair,
        Some('u'),
        SHAPE_OPTS,
    ),
    t(
        ToolId::Polygon,
        "Polygon",
        ToolGroup::Draw,
        Some("shape"),
        "shape-polygon",
        Cursor::Crosshair,
        Some('u'),
        shape_opts!(i("sides", "Sides", 3, 100, 6)),
    ),
    t(
        ToolId::Star,
        "Star",
        ToolGroup::Draw,
        Some("shape"),
        "shape-star",
        Cursor::Crosshair,
        Some('u'),
        shape_opts!(
            i("points", "Points", 3, 100, 5),
            f("inner_ratio", "Indent", 0.05, 1.0, 0.4),
        ),
    ),
    t(
        ToolId::Line,
        "Line",
        ToolGroup::Draw,
        Some("shape"),
        "shape-line",
        Cursor::Crosshair,
        Some('u'),
        shape_opts!(f("width", "Weight", 0.1, 500.0, 2.0)),
    ),
    t(
        ToolId::CustomShape,
        "Custom Shape",
        ToolGroup::Draw,
        Some("shape"),
        "shape-custom",
        Cursor::Crosshair,
        Some('u'),
        // The built-in library (`vector::custom`), picked by index; the
        // labels are built from the enum so the two cannot disagree.
        shape_opts!(c("preset", "Shape", &vector::CUSTOM_SHAPE_NAMES, 0)),
    ),
    // W10-A: the parametric spiral, last in the shape slot.
    t(
        ToolId::Spiral,
        "Spiral",
        ToolGroup::Draw,
        Some("shape"),
        "shape-spiral",
        Cursor::Crosshair,
        Some('u'),
        shape_opts!(
            f("turns", "Turns", 0.25, 50.0, 3.0),
            f("inner_radius", "Inner Radius", 0.0, 0.95, 0.1),
            c(
                "direction",
                "Direction",
                crate::shape::SPIRAL_DIRECTION_CHOICES,
                0
            ),
        ),
    ),
    t(
        ToolId::Hand,
        "Hand",
        ToolGroup::Navigate,
        Some("hand"),
        "hand",
        Cursor::OpenHand,
        Some('h'),
        &[],
    ),
    t(
        ToolId::RotateView,
        "Rotate View",
        ToolGroup::Navigate,
        Some("hand"),
        "rotate-view",
        Cursor::Rotate,
        Some('r'),
        &[],
    ),
    t(
        ToolId::Zoom,
        "Zoom",
        ToolGroup::Navigate,
        Some("zoom"),
        "zoom",
        Cursor::ZoomIn,
        Some('z'),
        &[],
    ),
    t(
        ToolId::FreeTransform,
        "Free Transform",
        ToolGroup::Transform,
        // Off the palette and off the letter keys: Photopea reaches Free
        // Transform from Edit ▸ Free Transform and `Ctrl+T` only, and sharing
        // `T` with Type meant a second `T` while typing swapped the text for a
        // transform box.
        None,
        "transform",
        Cursor::Arrow,
        None,
        // W9-L: Photopea's numeric bar — reference point, X/Y, W/H %, Link,
        // Angle, H/V Skew, Interpolation — and the Warp presets. The
        // geometry keys apply together on an edit-counter change
        // (`crate::transform::TransformTool::apply_pending_numeric`).
        &[
            // W10-J: Content-Aware is the seventh mode (Edit > Content-Aware
            // Scale), labelled from `TransformMode::LABELS`.
            c("mode", "Mode", crate::transform::TransformMode::LABELS, 0),
            c(
                crate::transform::keys::REFERENCE,
                "Reference",
                crate::transform::REFERENCE_LABELS,
                crate::transform::REFERENCE_CENTRE,
            ),
            f(crate::transform::keys::X, "X", -100_000.0, 100_000.0, 0.0),
            f(crate::transform::keys::Y, "Y", -100_000.0, 100_000.0, 0.0),
            f(crate::transform::keys::W, "W %", -10_000.0, 10_000.0, 100.0),
            f(crate::transform::keys::H, "H %", -10_000.0, 10_000.0, 100.0),
            b(crate::transform::keys::LINK, "Link", false),
            f(crate::transform::keys::ANGLE, "Angle", -180.0, 180.0, 0.0),
            f(
                crate::transform::keys::SKEW_H,
                "H Skew",
                -crate::transform::MAX_SKEW_DEG,
                crate::transform::MAX_SKEW_DEG,
                0.0,
            ),
            f(
                crate::transform::keys::SKEW_V,
                "V Skew",
                -crate::transform::MAX_SKEW_DEG,
                crate::transform::MAX_SKEW_DEG,
                0.0,
            ),
            c(
                crate::transform::keys::INTERPOLATION,
                "Interpolation",
                crate::transform::INTERPOLATION_LABELS,
                2,
            ),
            c(
                crate::transform::keys::WARP,
                "Warp",
                crate::transform::WARP_PRESET_LABELS,
                0,
            ),
            f(crate::transform::keys::BEND, "Bend", -100.0, 100.0, 50.0),
            // W10-J: Content-Aware Scale's Amount (the Content-Aware mode).
            f(
                crate::transform::keys::CA_AMOUNT,
                "Amount",
                0.0,
                100.0,
                100.0,
            ),
            i(crate::transform::keys::NUMERIC_SEQ, "Edit", 0, i32::MAX, 0),
        ],
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
        ToolId::ObjectSelection => Box::new(crate::select::ObjectSelectionTool::default()),
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
        ToolId::Pencil => Box::new(crate::pencil::PencilTool::default()),
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
        ToolId::RefineBoundary => Box::new(StrokeTool::new(
            id,
            brush(40.0, 0.0, 0.05),
            StrokeOp::RefineBoundary { strength: 0.5 },
        )),
        ToolId::Smudge => Box::new(StrokeTool::new(
            id,
            brush(40.0, 0.0, 0.05),
            StrokeOp::Smudge { strength: 0.5 },
        )),
        ToolId::Dodge => {
            let mut tool = StrokeTool::new(
                id,
                brush(60.0, 0.0, 0.05),
                StrokeOp::Dodge {
                    exposure: 0.25,
                    range: ToneRange::Midtones,
                },
            );
            tool.protect_tones = TONE_PROTECT_DEFAULT;
            Box::new(tool)
        }
        ToolId::Burn => {
            let mut tool = StrokeTool::new(
                id,
                brush(60.0, 0.0, 0.05),
                StrokeOp::Burn {
                    exposure: 0.25,
                    range: ToneRange::Midtones,
                },
            );
            tool.protect_tones = TONE_PROTECT_DEFAULT;
            Box::new(tool)
        }
        ToolId::Sponge => {
            let mut tool = StrokeTool::new(
                id,
                brush(60.0, 0.0, 0.05),
                StrokeOp::Sponge {
                    amount: 0.3,
                    mode: SpongeMode::Desaturate,
                },
            );
            tool.vibrance = SPONGE_VIBRANCE_DEFAULT;
            Box::new(tool)
        }
        ToolId::Pen => Box::new(crate::pen::PenTool::default()),
        ToolId::PathSelect => Box::new(crate::path_select::PathSelectTool::default()),
        ToolId::DirectSelection => Box::new(crate::path_select::DirectSelectionTool::default()),
        ToolId::Type => Box::new(crate::text::TypeTool::default()),
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
            // The library's first entry, matching the `preset` default (0).
            ShapeKind::custom(vector::CustomShape::ALL[0]),
            ShapeMode::VectorLayer,
        )),
        ToolId::Hand => Box::new(ViewTool::new(ViewGesture::Pan)),
        ToolId::Zoom => Box::new(ViewTool::new(ViewGesture::Zoom)),
        ToolId::RotateView => Box::new(ViewTool::new(ViewGesture::Rotate)),
        ToolId::FreeTransform => Box::new(TransformTool::default()),
        ToolId::Ruler => Box::new(crate::measure::RulerTool::default()),
        ToolId::ColorSampler => Box::new(crate::measure::ColorSamplerTool::default()),
        ToolId::HistoryBrush => Box::new(crate::history_brush::HistoryBrushTool::default()),
        ToolId::AddAnchor => Box::new(crate::path_select::AnchorTool::new(
            crate::path_select::AnchorEdit::Add,
        )),
        ToolId::DeleteAnchor => Box::new(crate::path_select::AnchorTool::new(
            crate::path_select::AnchorEdit::Delete,
        )),
        ToolId::ConvertAnchor => Box::new(crate::path_select::AnchorTool::new(
            crate::path_select::AnchorEdit::Convert,
        )),
        // W7-F.
        ToolId::PerspectiveCrop => {
            Box::new(crate::perspective_crop::PerspectiveCropTool::default())
        }
        ToolId::VerticalType => Box::new(crate::text::TypeTool::with_mode(
            crate::text::TypeMode::Vertical,
        )),
        ToolId::HorizontalTypeMask => Box::new(crate::text::TypeTool::with_mode(
            crate::text::TypeMode::HorizontalMask,
        )),
        ToolId::VerticalTypeMask => Box::new(crate::text::TypeTool::with_mode(
            crate::text::TypeMode::VerticalMask,
        )),
        ToolId::MixerBrush => Box::new(crate::mixer_brush::MixerBrushTool::default()),
        ToolId::Artboard => Box::new(crate::artboard::ArtboardTool::default()),
        ToolId::CurvaturePen => Box::new(crate::curvature_pen::CurvaturePenTool::default()),
        ToolId::FreeformPen => Box::new(crate::pen::FreeformPenTool::default()),
        // W10-A.
        ToolId::ContentAwareMove => {
            Box::new(crate::content_aware_move::ContentAwareMoveTool::default())
        }
        ToolId::SliceSelect => Box::new(crate::slice_select::SliceSelectTool::default()),
        // W10-B.
        ToolId::Note => Box::new(crate::note::NoteTool::default()),
        ToolId::Spiral => Box::new(ShapeTool::new(
            ShapeKind::Spiral {
                turns: 3.0,
                inner_ratio: 0.1,
                clockwise: true,
            },
            ShapeMode::VectorLayer,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use crate::tool::ToolSetting;
    use crate::tool::{PointerEvent, ToolContext};
    use raster::PixelRect;

    /// Tools whose option refusals are tolerated by
    /// [`every_declared_option_reaches_its_tool`].
    ///
    /// **Empty, by design.** It held the W1-B2 tools (stroke, shape, type,
    /// transform) while their `set_setting` wiring was another doer's; they
    /// all landed, and W2-C emptied it — so the test now covers every tool
    /// in [`all`] and every key it declares. `the_option_allow_list_is_empty`
    /// pins it shut: a tool that wants back in has to change that test,
    /// which is the review conversation this constant exists to force.
    pub(super) const W1_B2_PENDING: &[ToolId] = &[];

    /// A value of the spec's kind that is NOT its default, inside its range —
    /// so a `set_setting` that quietly drops the value cannot pass by
    /// coincidence, and a kind-mismatch refusal cannot hide behind a wrong
    /// variant.
    fn non_default_setting(kind: OptionKind) -> ToolSetting {
        match kind {
            OptionKind::Float { min, max, default } => {
                ToolSetting::Float(if default < max { max } else { min })
            }
            OptionKind::Int { min, max, default } => {
                ToolSetting::Int(if default < max { max } else { min })
            }
            OptionKind::Bool { default } => ToolSetting::Bool(!default),
            OptionKind::Choice { choices, default } => {
                ToolSetting::Choice((default + 1) % choices.len().max(1))
            }
            OptionKind::Color { default } => {
                ToolSetting::Color([1.0 - default[0], default[1], default[2], default[3]])
            }
        }
    }

    /// W1-B1: the registry-to-tool contract, for EVERY option kind. For
    /// every tool in [`all`], for every [`OptionSpec`] it declares, the
    /// tool [`make`] builds must accept a non-default value of the spec's
    /// kind through `Tool::set_setting`. An option the bar draws that its
    /// tool refuses is a control that does nothing while looking like it
    /// does — the defect this test exists to prevent. The brush-shared keys
    /// travel through `set_brush` by design and are skipped; the tools in
    /// [`W1_B2_PENDING`] would be tolerated, and that list is empty.
    #[test]
    fn every_declared_option_reaches_its_tool() {
        let mut refused = Vec::new();
        let mut pending_still_refusing = Vec::new();
        for info in all() {
            let mut tool = make(info.id);
            for spec in info.options {
                if BRUSH_OPTION_KEYS.contains(&spec.key) {
                    continue;
                }
                let value = non_default_setting(spec.kind);
                if let Err(e) = tool.set_setting(spec.key, value) {
                    if W1_B2_PENDING.contains(&info.id) {
                        pending_still_refusing.push((info.name, spec.key, e.to_string()));
                    } else {
                        refused.push((info.name, spec.key, e.to_string()));
                    }
                }
            }
        }
        // Informational only: what the allow-list is still covering.
        for (name, key, why) in &pending_still_refusing {
            eprintln!("W1-B2 pending: {name}.{key}: {why}");
        }
        assert!(
            refused.is_empty(),
            "a declared option was refused by its own tool — wire it in set_setting: {refused:#?}"
        );
    }

    /// W2-C: the allow-list above is shut. With it empty, the test above is
    /// the whole contract — every tool, every key — and a tool cannot be
    /// quietly excused by being listed.
    #[test]
    fn the_option_allow_list_is_empty() {
        assert!(
            W1_B2_PENDING.is_empty(),
            "a tool is being excused from answering its own options: {W1_B2_PENDING:?}"
        );
    }

    /// The palette column Photopea draws, as slot ids, top to bottom. The
    /// `ui` crate's palette derives its buttons from [`ToolInfo::slot`] in
    /// registry order, so this list is the order of the buttons.
    const PHOTOPEA_SLOTS: &[&str] = &[
        "move",
        "marquee",
        "lasso",
        "wand",
        "crop",
        "eyedropper",
        "heal",
        "brush",
        "clone",
        "history",
        "eraser",
        "gradient",
        "blur",
        "tone",
        "pen",
        "type",
        "path",
        "shape",
        "hand",
        "zoom",
    ];

    #[test]
    fn the_slots_read_in_photopeas_order_and_each_is_one_contiguous_run() {
        // A slot's tools sit together in `TOOLS`: a palette that groups by
        // id in registry order would otherwise show a slot twice.
        let mut runs: Vec<&str> = Vec::new();
        for t in TOOLS {
            let Some(slot) = t.slot else { continue };
            if runs.last() != Some(&slot) {
                assert!(
                    !runs.contains(&slot),
                    "{slot:?} appears in two separate runs; {:?} is out of place",
                    t.id
                );
                runs.push(slot);
            }
        }
        assert_eq!(runs, PHOTOPEA_SLOTS);
    }

    #[test]
    fn the_slot_mates_are_the_ones_photopea_groups() {
        let mates = |slot: &str| -> Vec<ToolId> {
            TOOLS
                .iter()
                .filter(|t| t.slot == Some(slot))
                .map(|t| t.id)
                .collect()
        };
        assert_eq!(
            mates("blur"),
            vec![
                ToolId::Blur,
                ToolId::Sharpen,
                ToolId::Smudge,
                ToolId::RefineBoundary
            ],
            "the keyless retouch brushes share one slot"
        );
        assert_eq!(mates("hand"), vec![ToolId::Hand, ToolId::RotateView]);
        assert_eq!(
            mates("heal"),
            vec![
                ToolId::SpotHealing,
                ToolId::HealingBrush,
                ToolId::Patch,
                // W10-A: after the Patch tool, as in Photoshop.
                ToolId::ContentAwareMove,
                ToolId::RedEye
            ]
        );
        // W7-F: Photoshop's four type tools, the Artboard under Move and
        // the Perspective Crop inside the Crop slot.
        assert_eq!(
            mates("type"),
            vec![
                ToolId::Type,
                ToolId::VerticalType,
                ToolId::VerticalTypeMask,
                ToolId::HorizontalTypeMask
            ]
        );
        assert_eq!(mates("move"), vec![ToolId::Move, ToolId::Artboard]);
        assert_eq!(
            mates("crop"),
            vec![
                ToolId::Crop,
                ToolId::PerspectiveCrop,
                ToolId::Slice,
                // W10-A.
                ToolId::SliceSelect
            ]
        );
        // W4-G: Photoshop's order inside the slots that grew, and the
        // History Brush in a slot of its own.
        assert_eq!(
            mates("eyedropper"),
            vec![
                ToolId::Eyedropper,
                ToolId::ColorSampler,
                ToolId::Ruler,
                ToolId::Note
            ]
        );
        assert_eq!(
            mates("clone"),
            vec![ToolId::CloneStamp, ToolId::PatternStamp]
        );
        assert_eq!(mates("history"), vec![ToolId::HistoryBrush]);
        assert_eq!(
            mates("pen"),
            vec![
                ToolId::Pen,
                ToolId::FreeformPen,
                ToolId::CurvaturePen,
                ToolId::AddAnchor,
                ToolId::DeleteAnchor,
                ToolId::ConvertAnchor
            ]
        );
        assert_eq!(
            mates("path"),
            vec![ToolId::PathSelect, ToolId::DirectSelection]
        );
        // W10-A: the Spiral closes the shape slot.
        assert_eq!(mates("shape").last(), Some(&ToolId::Spiral));
    }

    #[test]
    fn free_transform_is_the_only_tool_off_the_palette_and_has_no_letter() {
        let off: Vec<ToolId> = TOOLS
            .iter()
            .filter(|t| t.slot.is_none())
            .map(|t| t.id)
            .collect();
        assert_eq!(off, vec![ToolId::FreeTransform]);
        let ft = info(ToolId::FreeTransform).unwrap();
        assert_eq!(ft.shortcut, None, "Free Transform is Ctrl+T, not a letter");
        // And `T` is the type tools' alone (W7-F: the four of them, as in
        // Photoshop), never Free Transform's.
        assert_eq!(
            by_shortcut('t'),
            vec![
                ToolId::Type,
                ToolId::VerticalType,
                ToolId::VerticalTypeMask,
                ToolId::HorizontalTypeMask
            ]
        );
        assert_eq!(cycle('t', None), Some(ToolId::Type));
    }

    #[test]
    fn the_registry_covers_every_tool_id_exactly_once() {
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
            vec![
                ToolId::Brush,
                ToolId::Pencil,
                ToolId::ColorReplacement,
                ToolId::MixerBrush
            ]
        );
        // Case-insensitive.
        assert_eq!(by_shortcut('B'), brushes);
        assert_eq!(cycle('b', None), Some(ToolId::Brush));
        assert_eq!(cycle('b', Some(ToolId::Brush)), Some(ToolId::Pencil));
        assert_eq!(cycle('b', Some(ToolId::MixerBrush)), Some(ToolId::Brush));
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

    /// The tuning a tool is built with is readable back, so a shell keeping a
    /// brush per tool can seed each slot from here instead of from a second
    /// copy of this table.
    #[test]
    fn a_tools_own_brush_reads_back_as_the_one_it_was_built_with() {
        assert_eq!(
            make(ToolId::Pencil).brush(),
            Some(BrushSettings::pencil(1.0))
        );
        assert_eq!(make(ToolId::Brush).brush(), Some(BrushSettings::default()));
        assert_eq!(
            make(ToolId::CloneStamp).brush(),
            Some(brush_of(40.0, 0.5, 0.05))
        );
        assert_eq!(make(ToolId::Dodge).brush(), Some(brush_of(60.0, 0.0, 0.05)));
        // The Pencil and the Brush share `StrokeOp::Paint`, so these settings
        // are the *only* thing that tells the two tools apart.
        assert_ne!(make(ToolId::Pencil).brush(), make(ToolId::Brush).brush());
        // A tool that stamps no dabs has no brush to hand back.
        assert_eq!(make(ToolId::RectMarquee).brush(), None);
        assert_eq!(make(ToolId::Hand).brush(), None);
        assert_eq!(make(ToolId::Gradient).brush(), None);
    }

    /// The same shape [`make`]'s local helper builds, for the test above.
    fn brush_of(size: f32, hardness: f32, spacing: f32) -> BrushSettings {
        BrushSettings {
            size,
            hardness,
            spacing,
            ..BrushSettings::default()
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
