//! Tool icons, drawn rather than typed.
//!
//! # Why not a font
//!
//! The registry hands the UI an icon *key* (`"marquee-rect"`, `"eyedropper"`)
//! and leaves the resolving to whoever draws. Resolving it to an emoji is a
//! gamble on the glyph existing in whatever face egui loaded, and a missing
//! glyph is a tofu box in the middle of the tool palette. These are paths in a
//! unit square instead: they scale to any size, take their colour from the
//! design tokens like everything else, and cannot be missing.
//!
//! # The gate
//!
//! [`icon_for`] is total over the registry: `every_registry_icon_key_has_a_drawing`
//! walks `tools::registry::all()` and fails on the first key that falls through
//! to [`Icon::UNKNOWN`]. A new tool cannot ship with a blank button.

use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};

/// One stroke of an icon, in a unit square with `y` pointing down.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Prim {
    /// An open or closed polyline.
    Poly(&'static [[f32; 2]], bool),
    /// A stroked circle: centre, radius.
    Circle([f32; 2], f32),
    /// A filled dot: centre, radius.
    Dot([f32; 2], f32),
    /// A single segment.
    Line([f32; 2], [f32; 2]),
}

/// A drawable icon.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Icon(pub &'static [Prim]);

impl Icon {
    /// The fallback: a hollow square, used for a key nothing recognises.
    pub const UNKNOWN: Icon = Icon(&[Prim::Poly(
        &[[0.2, 0.2], [0.8, 0.2], [0.8, 0.8], [0.2, 0.8]],
        true,
    )]);

    /// `true` when this is the fallback rather than a real drawing.
    pub fn is_unknown(self) -> bool {
        std::ptr::eq(self.0.as_ptr(), Self::UNKNOWN.0.as_ptr())
    }

    /// Paint into `rect`, insetting so strokes stay inside it.
    pub fn paint(self, painter: &Painter, rect: Rect, color: Color32, width: f32) {
        let side = rect.width().min(rect.height());
        if side <= 0.0 {
            return;
        }
        // Inset by half the stroke so a path at the edge of the unit square is
        // not clipped in half by the button's own bounds.
        let inset = width * 0.5;
        let box_side = (side - inset * 2.0).max(1.0);
        let origin = rect.center() - Vec2::splat(box_side * 0.5);
        let at = |p: [f32; 2]| Pos2::new(origin.x + p[0] * box_side, origin.y + p[1] * box_side);
        let stroke = Stroke::new(width, color);
        for prim in self.0 {
            match prim {
                Prim::Poly(points, closed) => {
                    let mut pts: Vec<Pos2> = points.iter().map(|p| at(*p)).collect();
                    if *closed {
                        if let Some(first) = pts.first().copied() {
                            pts.push(first);
                        }
                    }
                    painter.add(egui::Shape::line(pts, stroke));
                }
                Prim::Circle(c, r) => {
                    painter.circle_stroke(at(*c), r * box_side, stroke);
                }
                Prim::Dot(c, r) => {
                    painter.circle_filled(at(*c), r * box_side, color);
                }
                Prim::Line(a, b) => {
                    painter.line_segment([at(*a), at(*b)], stroke);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The drawings
// ---------------------------------------------------------------------------

const ARROW: &[Prim] = &[Prim::Poly(
    &[
        [0.28, 0.14],
        [0.28, 0.80],
        [0.44, 0.64],
        [0.55, 0.88],
        [0.68, 0.82],
        [0.57, 0.59],
        [0.76, 0.56],
    ],
    true,
)];

const CROSS_ARROWS: &[Prim] = &[
    Prim::Line([0.5, 0.12], [0.5, 0.88]),
    Prim::Line([0.12, 0.5], [0.88, 0.5]),
    Prim::Poly(&[[0.38, 0.24], [0.5, 0.12], [0.62, 0.24]], false),
    Prim::Poly(&[[0.38, 0.76], [0.5, 0.88], [0.62, 0.76]], false),
    Prim::Poly(&[[0.24, 0.38], [0.12, 0.5], [0.24, 0.62]], false),
    Prim::Poly(&[[0.76, 0.38], [0.88, 0.5], [0.76, 0.62]], false),
];

const MARQUEE_RECT: &[Prim] = &[Prim::Poly(
    &[[0.16, 0.24], [0.84, 0.24], [0.84, 0.76], [0.16, 0.76]],
    true,
)];

const MARQUEE_ELLIPSE: &[Prim] = &[Prim::Circle([0.5, 0.5], 0.33)];

const MARQUEE_ROW: &[Prim] = &[
    Prim::Poly(&[[0.1, 0.42], [0.9, 0.42], [0.9, 0.58], [0.1, 0.58]], true),
    Prim::Line([0.1, 0.2], [0.9, 0.2]),
];

const MARQUEE_COLUMN: &[Prim] = &[
    Prim::Poly(&[[0.42, 0.1], [0.58, 0.1], [0.58, 0.9], [0.42, 0.9]], true),
    Prim::Line([0.2, 0.1], [0.2, 0.9]),
];

const LASSO: &[Prim] = &[
    Prim::Poly(
        &[
            [0.5, 0.16],
            [0.78, 0.30],
            [0.80, 0.55],
            [0.52, 0.66],
            [0.24, 0.55],
            [0.22, 0.30],
        ],
        true,
    ),
    Prim::Poly(&[[0.36, 0.63], [0.32, 0.86], [0.46, 0.90]], false),
];

const LASSO_POLY: &[Prim] = &[
    Prim::Poly(
        &[[0.18, 0.30], [0.55, 0.14], [0.86, 0.44], [0.62, 0.70]],
        false,
    ),
    Prim::Dot([0.18, 0.30], 0.07),
    Prim::Dot([0.86, 0.44], 0.07),
    Prim::Line([0.62, 0.70], [0.32, 0.88]),
];

const LASSO_MAGNETIC: &[Prim] = &[
    Prim::Poly(
        &[
            [0.20, 0.72],
            [0.20, 0.42],
            [0.50, 0.20],
            [0.80, 0.42],
            [0.80, 0.72],
        ],
        false,
    ),
    Prim::Line([0.20, 0.72], [0.36, 0.72]),
    Prim::Line([0.80, 0.72], [0.64, 0.72]),
    Prim::Line([0.36, 0.72], [0.36, 0.52]),
    Prim::Line([0.64, 0.72], [0.64, 0.52]),
];

const WAND: &[Prim] = &[
    Prim::Line([0.20, 0.84], [0.66, 0.34]),
    Prim::Poly(&[[0.58, 0.24], [0.76, 0.24], [0.76, 0.42]], false),
    Prim::Line([0.80, 0.16], [0.88, 0.10]),
    Prim::Dot([0.30, 0.24], 0.05),
    Prim::Dot([0.84, 0.62], 0.05),
];

const QUICK_SELECT: &[Prim] = &[
    Prim::Circle([0.38, 0.46], 0.26),
    Prim::Line([0.62, 0.66], [0.86, 0.88]),
    Prim::Line([0.66, 0.18], [0.90, 0.18]),
    Prim::Line([0.78, 0.06], [0.78, 0.30]),
];

const CROP: &[Prim] = &[
    Prim::Poly(&[[0.28, 0.08], [0.28, 0.72], [0.92, 0.72]], false),
    Prim::Poly(&[[0.08, 0.28], [0.72, 0.28], [0.72, 0.92]], false),
];

const SLICE: &[Prim] = &[
    Prim::Poly(&[[0.12, 0.2], [0.88, 0.2], [0.88, 0.8], [0.12, 0.8]], true),
    Prim::Line([0.5, 0.2], [0.5, 0.8]),
    Prim::Line([0.12, 0.5], [0.5, 0.5]),
];

const EYEDROPPER: &[Prim] = &[
    Prim::Line([0.16, 0.86], [0.60, 0.42]),
    Prim::Poly(
        &[[0.52, 0.34], [0.74, 0.12], [0.90, 0.28], [0.68, 0.50]],
        true,
    ),
    Prim::Poly(&[[0.16, 0.86], [0.14, 0.94], [0.24, 0.90]], true),
];

const SPOT_HEAL: &[Prim] = &[
    Prim::Circle([0.5, 0.5], 0.3),
    Prim::Line([0.34, 0.5], [0.66, 0.5]),
    Prim::Line([0.5, 0.34], [0.5, 0.66]),
];

const HEAL: &[Prim] = &[
    Prim::Poly(
        &[[0.18, 0.52], [0.50, 0.20], [0.82, 0.52], [0.50, 0.84]],
        true,
    ),
    Prim::Line([0.36, 0.52], [0.64, 0.52]),
    Prim::Line([0.50, 0.38], [0.50, 0.66]),
];

const PATCH: &[Prim] = &[
    Prim::Poly(
        &[
            [0.16, 0.34],
            [0.50, 0.14],
            [0.84, 0.34],
            [0.72, 0.80],
            [0.28, 0.80],
        ],
        true,
    ),
    Prim::Line([0.30, 0.52], [0.70, 0.52]),
];

const RED_EYE: &[Prim] = &[
    Prim::Poly(
        &[
            [0.10, 0.5],
            [0.32, 0.28],
            [0.68, 0.28],
            [0.90, 0.5],
            [0.68, 0.72],
            [0.32, 0.72],
        ],
        true,
    ),
    Prim::Dot([0.5, 0.5], 0.13),
];

const BRUSH: &[Prim] = &[
    Prim::Poly(
        &[
            [0.20, 0.88],
            [0.24, 0.62],
            [0.62, 0.24],
            [0.80, 0.42],
            [0.42, 0.80],
        ],
        true,
    ),
    Prim::Line([0.62, 0.24], [0.86, 0.12]),
];

const PENCIL: &[Prim] = &[
    Prim::Poly(
        &[
            [0.14, 0.86],
            [0.22, 0.62],
            [0.70, 0.14],
            [0.86, 0.30],
            [0.38, 0.78],
        ],
        true,
    ),
    Prim::Line([0.22, 0.62], [0.38, 0.78]),
];

const COLOR_REPLACE: &[Prim] = &[
    Prim::Poly(
        &[
            [0.20, 0.86],
            [0.26, 0.60],
            [0.64, 0.22],
            [0.80, 0.38],
            [0.42, 0.76],
        ],
        true,
    ),
    Prim::Circle([0.74, 0.74], 0.16),
];

const CLONE: &[Prim] = &[
    Prim::Poly(
        &[[0.28, 0.36], [0.72, 0.36], [0.72, 0.48], [0.28, 0.48]],
        true,
    ),
    Prim::Poly(
        &[[0.40, 0.16], [0.60, 0.16], [0.60, 0.36], [0.40, 0.36]],
        true,
    ),
    Prim::Poly(
        &[[0.40, 0.48], [0.60, 0.48], [0.60, 0.88], [0.40, 0.88]],
        true,
    ),
];

const PATTERN_STAMP: &[Prim] = &[
    Prim::Poly(
        &[[0.28, 0.36], [0.72, 0.36], [0.72, 0.48], [0.28, 0.48]],
        true,
    ),
    Prim::Poly(
        &[[0.40, 0.48], [0.60, 0.48], [0.60, 0.88], [0.40, 0.88]],
        true,
    ),
    Prim::Line([0.32, 0.16], [0.68, 0.16]),
    Prim::Line([0.32, 0.26], [0.68, 0.26]),
];

const ERASER: &[Prim] = &[
    Prim::Poly(
        &[[0.14, 0.68], [0.52, 0.20], [0.86, 0.44], [0.48, 0.88]],
        true,
    ),
    Prim::Line([0.48, 0.88], [0.90, 0.88]),
    Prim::Line([0.32, 0.44], [0.68, 0.68]),
];

const ERASER_BG: &[Prim] = &[
    Prim::Poly(
        &[[0.14, 0.62], [0.48, 0.18], [0.80, 0.42], [0.46, 0.84]],
        true,
    ),
    Prim::Line([0.30, 0.40], [0.62, 0.62]),
    Prim::Dot([0.82, 0.80], 0.09),
];

const ERASER_MAGIC: &[Prim] = &[
    Prim::Poly(
        &[[0.10, 0.62], [0.44, 0.20], [0.74, 0.42], [0.40, 0.84]],
        true,
    ),
    Prim::Line([0.78, 0.14], [0.92, 0.28]),
    Prim::Dot([0.86, 0.62], 0.05),
    Prim::Dot([0.66, 0.10], 0.05),
];

const GRADIENT: &[Prim] = &[
    Prim::Poly(
        &[[0.12, 0.24], [0.88, 0.24], [0.88, 0.76], [0.12, 0.76]],
        true,
    ),
    Prim::Line([0.32, 0.24], [0.32, 0.76]),
    Prim::Line([0.50, 0.24], [0.50, 0.76]),
    Prim::Line([0.70, 0.24], [0.70, 0.76]),
];

const BUCKET: &[Prim] = &[
    Prim::Poly(
        &[[0.18, 0.44], [0.52, 0.12], [0.86, 0.46], [0.52, 0.78]],
        true,
    ),
    Prim::Line([0.30, 0.30], [0.30, 0.72]),
    Prim::Poly(&[[0.86, 0.60], [0.94, 0.76], [0.78, 0.76]], true),
];

const PATTERN_FILL: &[Prim] = &[
    Prim::Poly(
        &[[0.14, 0.14], [0.86, 0.14], [0.86, 0.86], [0.14, 0.86]],
        true,
    ),
    Prim::Line([0.14, 0.38], [0.86, 0.38]),
    Prim::Line([0.14, 0.62], [0.86, 0.62]),
    Prim::Line([0.38, 0.14], [0.38, 0.86]),
    Prim::Line([0.62, 0.14], [0.62, 0.86]),
];

const BLUR: &[Prim] = &[
    Prim::Poly(
        &[
            [0.5, 0.14],
            [0.80, 0.50],
            [0.66, 0.80],
            [0.34, 0.80],
            [0.20, 0.50],
        ],
        true,
    ),
    Prim::Line([0.34, 0.62], [0.66, 0.62]),
];

const SHARPEN: &[Prim] = &[
    Prim::Poly(&[[0.5, 0.12], [0.84, 0.86], [0.16, 0.86]], true),
    Prim::Line([0.34, 0.60], [0.66, 0.60]),
];

const SMUDGE: &[Prim] = &[
    Prim::Poly(
        &[[0.16, 0.82], [0.34, 0.44], [0.56, 0.66], [0.86, 0.20]],
        false,
    ),
    Prim::Dot([0.86, 0.20], 0.07),
];

const DODGE: &[Prim] = &[
    Prim::Circle([0.42, 0.42], 0.22),
    Prim::Line([0.60, 0.62], [0.86, 0.88]),
];

const BURN: &[Prim] = &[
    Prim::Poly(
        &[
            [0.5, 0.12],
            [0.72, 0.42],
            [0.72, 0.68],
            [0.5, 0.88],
            [0.28, 0.68],
            [0.28, 0.42],
        ],
        true,
    ),
    Prim::Circle([0.5, 0.66], 0.12),
];

const SPONGE: &[Prim] = &[
    Prim::Poly(
        &[
            [0.16, 0.40],
            [0.32, 0.22],
            [0.68, 0.22],
            [0.84, 0.40],
            [0.72, 0.80],
            [0.28, 0.80],
        ],
        true,
    ),
    Prim::Dot([0.40, 0.46], 0.05),
    Prim::Dot([0.62, 0.56], 0.05),
    Prim::Dot([0.46, 0.68], 0.05),
];

const SHAPE_RECT: &[Prim] = &[Prim::Poly(
    &[[0.14, 0.22], [0.86, 0.22], [0.86, 0.78], [0.14, 0.78]],
    true,
)];

const SHAPE_RRECT: &[Prim] = &[Prim::Poly(
    &[
        [0.22, 0.22],
        [0.78, 0.22],
        [0.86, 0.32],
        [0.86, 0.68],
        [0.78, 0.78],
        [0.22, 0.78],
        [0.14, 0.68],
        [0.14, 0.32],
    ],
    true,
)];

const SHAPE_ELLIPSE: &[Prim] = &[Prim::Circle([0.5, 0.5], 0.34)];

const SHAPE_POLYGON: &[Prim] = &[Prim::Poly(
    &[
        [0.5, 0.12],
        [0.85, 0.32],
        [0.85, 0.68],
        [0.5, 0.88],
        [0.15, 0.68],
        [0.15, 0.32],
    ],
    true,
)];

const SHAPE_STAR: &[Prim] = &[Prim::Poly(
    &[
        [0.50, 0.10],
        [0.60, 0.38],
        [0.90, 0.38],
        [0.66, 0.56],
        [0.75, 0.86],
        [0.50, 0.68],
        [0.25, 0.86],
        [0.34, 0.56],
        [0.10, 0.38],
        [0.40, 0.38],
    ],
    true,
)];

const SHAPE_LINE: &[Prim] = &[
    Prim::Line([0.14, 0.86], [0.86, 0.14]),
    Prim::Dot([0.14, 0.86], 0.07),
    Prim::Dot([0.86, 0.14], 0.07),
];

const SHAPE_CUSTOM: &[Prim] = &[Prim::Poly(
    &[
        [0.50, 0.86],
        [0.18, 0.54],
        [0.18, 0.34],
        [0.34, 0.20],
        [0.50, 0.34],
        [0.66, 0.20],
        [0.82, 0.34],
        [0.82, 0.54],
    ],
    true,
)];

const HAND: &[Prim] = &[Prim::Poly(
    &[
        [0.28, 0.86],
        [0.20, 0.58],
        [0.24, 0.46],
        [0.32, 0.54],
        [0.32, 0.22],
        [0.42, 0.16],
        [0.46, 0.24],
        [0.46, 0.18],
        [0.56, 0.14],
        [0.60, 0.24],
        [0.70, 0.24],
        [0.76, 0.34],
        [0.76, 0.70],
        [0.68, 0.86],
    ],
    true,
)];

const ZOOM: &[Prim] = &[
    Prim::Circle([0.44, 0.44], 0.28),
    Prim::Line([0.64, 0.64], [0.88, 0.88]),
    Prim::Line([0.30, 0.44], [0.58, 0.44]),
    Prim::Line([0.44, 0.30], [0.44, 0.58]),
];

const ROTATE_VIEW: &[Prim] = &[
    Prim::Poly(
        &[
            [0.22, 0.62],
            [0.24, 0.40],
            [0.40, 0.24],
            [0.62, 0.22],
            [0.78, 0.34],
        ],
        false,
    ),
    Prim::Poly(&[[0.62, 0.16], [0.82, 0.30], [0.62, 0.42]], false),
    Prim::Poly(&[[0.32, 0.76], [0.50, 0.86], [0.70, 0.74]], false),
];

const TRANSFORM: &[Prim] = &[
    Prim::Poly(
        &[[0.22, 0.22], [0.78, 0.22], [0.78, 0.78], [0.22, 0.78]],
        true,
    ),
    Prim::Dot([0.22, 0.22], 0.07),
    Prim::Dot([0.78, 0.22], 0.07),
    Prim::Dot([0.22, 0.78], 0.07),
    Prim::Dot([0.78, 0.78], 0.07),
];

/// The drawing for a registry icon key.
///
/// Total over `tools::registry` — see the module note and the gate that keeps
/// it that way.
pub fn icon_for(key: &str) -> Icon {
    Icon(match key {
        "move" => CROSS_ARROWS,
        "marquee-rect" => MARQUEE_RECT,
        "marquee-ellipse" => MARQUEE_ELLIPSE,
        "marquee-row" => MARQUEE_ROW,
        "marquee-column" => MARQUEE_COLUMN,
        "lasso" => LASSO,
        "lasso-poly" => LASSO_POLY,
        "lasso-magnetic" => LASSO_MAGNETIC,
        "wand" => WAND,
        "quick-select" => QUICK_SELECT,
        "crop" => CROP,
        "slice" => SLICE,
        "eyedropper" => EYEDROPPER,
        "spot-heal" => SPOT_HEAL,
        "heal" => HEAL,
        "patch" => PATCH,
        "red-eye" => RED_EYE,
        "brush" => BRUSH,
        "pencil" => PENCIL,
        "color-replace" => COLOR_REPLACE,
        "clone" => CLONE,
        "pattern-stamp" => PATTERN_STAMP,
        "eraser" => ERASER,
        "eraser-bg" => ERASER_BG,
        "eraser-magic" => ERASER_MAGIC,
        "gradient" => GRADIENT,
        "bucket" => BUCKET,
        "pattern-fill" => PATTERN_FILL,
        "blur" => BLUR,
        "sharpen" => SHARPEN,
        "smudge" => SMUDGE,
        "dodge" => DODGE,
        "burn" => BURN,
        "sponge" => SPONGE,
        "shape-rect" => SHAPE_RECT,
        "shape-rrect" => SHAPE_RRECT,
        "shape-ellipse" => SHAPE_ELLIPSE,
        "shape-polygon" => SHAPE_POLYGON,
        "shape-star" => SHAPE_STAR,
        "shape-line" => SHAPE_LINE,
        "shape-custom" => SHAPE_CUSTOM,
        "hand" => HAND,
        "zoom" => ZOOM,
        "rotate-view" => ROTATE_VIEW,
        "transform" => TRANSFORM,
        "arrow" => ARROW,
        _ => return Icon::UNKNOWN,
    })
}

/// A toolbar button showing the drawing for `key`.
///
/// The registry stores an icon *key* (`"marquee-rect"`), not a glyph, so
/// handing that key to a text button paints the literal words "marquee-rect"
/// wrapped across the strip. That is what the tool palette did, and it is the
/// first thing anyone sees on opening the app.
///
/// This keeps `design`'s button for the framing — size, radius, fill, selected
/// ring, hover — and paints the vector icon into the rect it reports, so the
/// palette follows the theme like every other control.
pub fn icon_button(ui: &mut egui::Ui, key: &str, tooltip: &str, selected: bool) -> egui::Response {
    use design::{color32, ColorRole, Space, TextRole};

    let response = design::toolbar_icon_button(ui, "", tooltip, selected);
    let t = design::current_tokens(ui);
    let color = color32(t.palette.text(if selected {
        TextRole::Primary
    } else {
        TextRole::Secondary
    }));
    // Keep the fallback visible rather than silently blank, so a tool that
    // ships without a drawing looks wrong instead of looking disabled.
    let icon = icon_for(key);
    let color = if icon.is_unknown() {
        color32(t.palette.color(ColorRole::Danger))
    } else {
        color
    };
    icon.paint(
        &ui.painter_at(response.rect),
        response.rect.shrink(Space::Small.pt()),
        color,
        t.borders.hairline * 1.5,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registry_icon_key_has_a_drawing() {
        for info in tools::registry::all() {
            let icon = icon_for(info.icon);
            assert!(
                !icon.is_unknown(),
                "{:?} has icon key {:?} with no drawing; the palette would show a blank button",
                info.id,
                info.icon
            );
        }
    }

    #[test]
    fn an_unrecognised_key_falls_back_visibly_rather_than_to_nothing() {
        let icon = icon_for("no-such-icon");
        assert!(icon.is_unknown());
        assert!(!icon.0.is_empty(), "the fallback must still draw something");
    }

    #[test]
    fn no_icon_is_empty() {
        for info in tools::registry::all() {
            assert!(!icon_for(info.icon).0.is_empty(), "{:?}", info.id);
        }
    }

    #[test]
    fn every_point_of_every_icon_is_inside_the_unit_square() {
        // A path outside 0..1 would be clipped by the button, so the icon would
        // silently lose a limb at small sizes.
        let mut checked = 0usize;
        for info in tools::registry::all() {
            for prim in icon_for(info.icon).0 {
                let points: Vec<[f32; 2]> = match prim {
                    Prim::Poly(p, _) => p.to_vec(),
                    Prim::Circle(c, r) | Prim::Dot(c, r) => {
                        vec![[c[0] - r, c[1] - r], [c[0] + r, c[1] + r]]
                    }
                    Prim::Line(a, b) => vec![*a, *b],
                };
                for p in points {
                    assert!(
                        (0.0..=1.0).contains(&p[0]) && (0.0..=1.0).contains(&p[1]),
                        "{:?} ({}) has a point outside the unit square: {p:?}",
                        info.id,
                        info.icon
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 100, "the icon set lost its geometry");
    }

    #[test]
    fn a_polyline_has_at_least_two_points() {
        for info in tools::registry::all() {
            for prim in icon_for(info.icon).0 {
                if let Prim::Poly(p, _) = prim {
                    assert!(p.len() >= 2, "{:?} has a one-point polyline", info.id);
                }
            }
        }
    }

    #[test]
    fn no_two_tools_share_a_drawing() {
        // Two identical buttons in one column is the same bug as a blank one.
        let mut seen: Vec<(&'static str, &'static [Prim])> = Vec::new();
        for info in tools::registry::all() {
            let icon = icon_for(info.icon);
            if let Some((other, _)) = seen
                .iter()
                .find(|(_, prims)| std::ptr::eq(prims.as_ptr(), icon.0.as_ptr()))
            {
                panic!("{:?} draws the same icon as {other}", info.id);
            }
            seen.push((info.icon, icon.0));
        }
    }

    #[test]
    fn painting_into_a_zero_sized_rect_is_a_no_op_rather_than_a_panic() {
        let ctx = egui::Context::default();
        let mut done = false;
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let painter = ui.painter();
                icon_for("brush").paint(
                    &painter.clone(),
                    Rect::from_min_size(Pos2::ZERO, Vec2::ZERO),
                    Color32::WHITE,
                    1.5,
                );
                icon_for("brush").paint(
                    &painter.clone(),
                    Rect::from_min_size(Pos2::ZERO, Vec2::splat(24.0)),
                    Color32::WHITE,
                    1.5,
                );
                done = true;
            });
        });
        assert!(done);
    }

    /// Count the shapes one headless frame emits for `body`.
    fn shapes_in_frame(mut body: impl FnMut(&mut egui::Ui)) -> usize {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Light);
        let out = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, &mut body);
        });
        ctx.tessellate(out.shapes, out.pixels_per_point)
            .iter()
            .map(|p| match &p.primitive {
                egui::epaint::Primitive::Mesh(m) => m.indices.len(),
                egui::epaint::Primitive::Callback(_) => 0,
            })
            .sum()
    }

    #[test]
    fn the_icon_button_actually_draws_its_icon() {
        // The palette bug this exists to prevent: the registry stores an icon
        // KEY, and handing that key to a text button paints the literal words
        // "marquee-rect" instead of a drawing. An icon button must therefore
        // emit strictly more geometry than the same button with nothing in it.
        let empty = shapes_in_frame(|ui| {
            design::toolbar_icon_button(ui, "", "", false);
        });
        let drawn = shapes_in_frame(|ui| {
            icon_button(ui, "marquee-rect", "", false);
        });
        assert!(
            drawn > empty,
            "icon_button emitted no more geometry than an empty button              ({drawn} vs {empty}) — the icon is not being painted"
        );
    }

    #[test]
    fn the_icon_button_does_not_render_the_key_as_text() {
        // Two keys nothing recognises. Both fall back to the same square, so
        // the geometry must be IDENTICAL — the drawing does not depend on the
        // key, only on what the key resolved to. If the key were being typed
        // instead, a 40-character key and a 1-character key could not match.
        let long = shapes_in_frame(|ui| {
            icon_button(ui, "a-key-nothing-recognises-at-all-whatsoever", "", false);
        });
        let short = shapes_in_frame(|ui| {
            icon_button(ui, "q", "", false);
        });
        assert_eq!(
            long, short,
            "geometry changed with the key's length, so the key is being typed"
        );

        // And prove the comparison can tell the two apart: the text button,
        // which really does type its argument, differs on the same pair.
        let text_long = shapes_in_frame(|ui| {
            design::toolbar_icon_button(
                ui,
                "a-key-nothing-recognises-at-all-whatsoever",
                "",
                false,
            );
        });
        let text_short = shapes_in_frame(|ui| {
            design::toolbar_icon_button(ui, "q", "", false);
        });
        assert_ne!(
            text_long, text_short,
            "the text button was expected to scale with its label"
        );
    }
}
