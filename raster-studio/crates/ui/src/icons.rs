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
//! The same argument applies to every other affordance in the chrome, and for a
//! while only the tool palette took it: the panel headers, the Adjustments
//! grid, the History markers and the Layers lock row all typed a symbol
//! (`"▸"`, `"✕"`, `"◐"`, `"▨"`) into a text widget. egui 0.29 ships Ubuntu-Light
//! plus two emoji faces and nothing else; asking those very fonts (via
//! `Fonts::has_glyph`) which of the crate's symbols they held answered *most of
//! them, no*, so most of the chrome was tofu. Those surfaces draw through
//! [`ui_icon`] now.
//!
//! # The gates
//!
//! [`icon_for`] is total over the registry: `every_registry_icon_key_has_a_drawing`
//! walks `tools::registry::all()` and fails on the first key that falls through
//! to [`Icon::UNKNOWN`]. A new tool cannot ship with a blank button.
//!
//! [`ui_icon`] is total over the chrome the same way, and over each *set* that
//! feeds it: the adjustments, the history step kinds, the layer classes, the
//! lock toggles. A new adjustment or a new lock kind cannot ship without a
//! drawing.
//!
//! `tests/no_typed_ui_glyphs.rs` closes the loop: it scans every crate's
//! shipping source for a non-ASCII character in a string literal and fails on
//! anything outside a small allowlist of real text (an em dash in a sentence, a
//! middle dot between two readouts), and it puts that allowlist to egui's
//! loaded fonts, so a tofu character cannot be allowlisted out of the problem.
//! Every crate rather than this one, because a label is a label wherever it is
//! written: `tools::registry`'s Brush options are painted by `view::toolbar`,
//! and a scan of `crates/ui` alone could not see them.
//!
//! `every_chrome_icon_key_is_claimed_by_a_control`, in the same file, asks the
//! opposite question — whether every drawing in [`CHROME_ICON_KEYS`] has a
//! caller — since a key nobody uses usually means a control that was never
//! converted.

use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};

/// One stroke of an icon, in a unit square with `y` pointing down.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Prim {
    /// An open or closed polyline.
    Poly(&'static [[f32; 2]], bool),
    /// A *filled* convex polygon.
    ///
    /// The one thing [`Prim::Poly`] cannot say. A closed polyline around a
    /// square is a hollow square, which is the exact shape of [`Icon::UNKNOWN`]
    /// and of the tofu box — so the marks that stood for something *solid*
    /// (U+25A0 BLACK SQUARE in the Canvas Size anchor grid) have to be filled,
    /// or they read as the missing glyph they replaced.
    Fill(&'static [[f32; 2]]),
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

/// Distance between two unit-square points.
fn dist(a: &[f32; 2], b: &[f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

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

    /// The ink this drawing lays down, in square points, painted into a
    /// `side_pt` square at `width` points of stroke.
    ///
    /// Geometric, not rasterised: strokes count as length times width with a
    /// small cap allowance, dots as discs, fills by the shoelace formula. It
    /// is the number the weight-pass gate asserts on — a drawing whose ink
    /// under-shoots the floor renders as a pale smudge at tool size.
    pub fn ink_area(self, side_pt: f32, width: f32) -> f32 {
        let side = side_pt.max(1.0);
        self.0
            .iter()
            .map(|prim| match prim {
                Prim::Line(a, b) => {
                    let d = dist(a, b) * side;
                    d * width + width * width
                }
                Prim::Poly(points, closed) => {
                    let mut total = 0.0;
                    let n = points.len();
                    if n < 2 {
                        return 0.0;
                    }
                    for i in 0..n - 1 {
                        total += dist(&points[i], &points[i + 1]) * side * width;
                    }
                    if *closed {
                        total += dist(&points[n - 1], &points[0]) * side * width;
                    }
                    // Joints: each vertex doubles up roughly half a stroke.
                    total + n as f32 * 0.5 * width * width
                }
                Prim::Fill(points) => {
                    // The shoelace formula, in unit-square terms scaled up.
                    let n = points.len();
                    if n < 3 {
                        return 0.0;
                    }
                    let mut twice = 0.0;
                    for i in 0..n {
                        let j = (i + 1) % n;
                        twice += points[i][0] * points[j][1] - points[j][0] * points[i][1];
                    }
                    (twice.abs() * 0.5) * side * side
                }
                Prim::Circle(_, r) => {
                    // The circumference times the stroke, plus the joint allowance.
                    2.0 * std::f32::consts::PI * r * side * width + width * width
                }
                Prim::Dot(_, r) => std::f32::consts::PI * (r * side).powi(2),
            })
            .sum()
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
                Prim::Fill(points) => {
                    let pts: Vec<Pos2> = points.iter().map(|p| at(*p)).collect();
                    painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
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

/// The pen: a nib with a slit, and the anchor it is placing.
///
/// Deliberately distinct from [`PENCIL`] (a plain barrel) — the two sit in
/// different palette groups and a user has to tell them apart at 16 px, which
/// is what `no_two_tools_share_a_drawing` exists to enforce.
const PEN: &[Prim] = &[
    Prim::Poly(
        &[[0.20, 0.84], [0.30, 0.46], [0.62, 0.14], [0.80, 0.32]],
        true,
    ),
    Prim::Line([0.30, 0.46], [0.52, 0.68]),
    Prim::Line([0.20, 0.84], [0.41, 0.57]),
    Prim::Dot([0.20, 0.84], 0.06),
];

/// Type: a serifed capital I, the mark every editor uses for a text tool.
const TYPE: &[Prim] = &[
    Prim::Line([0.26, 0.20], [0.74, 0.20]),
    Prim::Line([0.50, 0.20], [0.50, 0.80]),
    Prim::Line([0.32, 0.80], [0.68, 0.80]),
];

/// The drawing for a registry icon key.
///
/// Total over `tools::registry` — see the module note and the gate that keeps
/// it that way.
pub fn icon_for(key: &str) -> Icon {
    Icon(match key {
        "move" => CROSS_ARROWS,
        "pen" => PEN,
        "anchor-block" => ANCHOR_BLOCK,
        "path-select" => LAYER_SHAPE,
        "type" => TYPE,
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

// ---------------------------------------------------------------------------
// The chrome: headers, list rows, the Adjustments grid, History, the locks
// ---------------------------------------------------------------------------

const CHEVRON_RIGHT: &[Prim] = &[Prim::Poly(
    &[[0.38, 0.22], [0.66, 0.5], [0.38, 0.78]],
    false,
)];

const CHEVRON_DOWN: &[Prim] = &[Prim::Poly(
    &[[0.22, 0.38], [0.5, 0.66], [0.78, 0.38]],
    false,
)];

const CHEVRON_UP: &[Prim] = &[Prim::Poly(
    &[[0.22, 0.62], [0.5, 0.34], [0.78, 0.62]],
    false,
)];

const CHEVRON_LEFT: &[Prim] = &[Prim::Poly(
    &[[0.62, 0.22], [0.34, 0.5], [0.62, 0.78]],
    false,
)];

/// The block of existing content in the Canvas Size anchor grid.
///
/// It used to be `"\u{25A0}"` painted with `Painter::text`, next to four
/// triangles two of which — U+25B2 and U+25BC — egui's fonts do not have. The
/// escape spelling is what hid them from the source gate for a whole round.
///
/// [`Prim::Fill`] rather than a closed [`Prim::Poly`] on purpose: the original
/// was a BLACK square, and an outlined one sitting in the middle of the selected
/// cell is indistinguishable from [`Icon::UNKNOWN`] and from the tofu box. The
/// fix is not supposed to look like the bug.
const ANCHOR_BLOCK: &[Prim] = &[Prim::Fill(&[
    [0.34, 0.34],
    [0.66, 0.34],
    [0.66, 0.66],
    [0.34, 0.66],
])];

const CLOSE: &[Prim] = &[
    Prim::Line([0.26, 0.26], [0.74, 0.74]),
    Prim::Line([0.74, 0.26], [0.26, 0.74]),
];

const OVERFLOW: &[Prim] = &[
    Prim::Dot([0.22, 0.5], 0.095),
    Prim::Dot([0.5, 0.5], 0.095),
    Prim::Dot([0.78, 0.5], 0.095),
];

const CHECK: &[Prim] = &[Prim::Poly(
    &[[0.20, 0.52], [0.42, 0.74], [0.80, 0.26]],
    false,
)];

const PLUS: &[Prim] = &[
    Prim::Line([0.5, 0.20], [0.5, 0.80]),
    Prim::Line([0.20, 0.5], [0.80, 0.5]),
];

const MINUS: &[Prim] = &[Prim::Line([0.20, 0.5], [0.80, 0.5])];

const EYE: &[Prim] = &[
    Prim::Poly(
        &[
            [0.08, 0.50],
            [0.30, 0.28],
            [0.70, 0.28],
            [0.92, 0.50],
            [0.70, 0.72],
            [0.30, 0.72],
        ],
        true,
    ),
    Prim::Circle([0.50, 0.50], 0.14),
];

const TRASH: &[Prim] = &[
    Prim::Poly(
        &[[0.26, 0.28], [0.32, 0.86], [0.68, 0.86], [0.74, 0.28]],
        true,
    ),
    Prim::Line([0.16, 0.28], [0.84, 0.28]),
    Prim::Poly(
        &[[0.40, 0.28], [0.40, 0.14], [0.60, 0.14], [0.60, 0.28]],
        false,
    ),
];

const TARGET: &[Prim] = &[
    Prim::Circle([0.5, 0.5], 0.24),
    Prim::Line([0.5, 0.08], [0.5, 0.28]),
    Prim::Line([0.5, 0.72], [0.5, 0.92]),
    Prim::Line([0.08, 0.5], [0.28, 0.5]),
    Prim::Line([0.72, 0.5], [0.92, 0.5]),
];

const SWAP: &[Prim] = &[
    Prim::Line([0.16, 0.34], [0.84, 0.34]),
    Prim::Poly(&[[0.70, 0.20], [0.84, 0.34], [0.70, 0.48]], false),
    Prim::Line([0.16, 0.66], [0.84, 0.66]),
    Prim::Poly(&[[0.30, 0.52], [0.16, 0.66], [0.30, 0.80]], false),
];

const COLORS_DEFAULT: &[Prim] = &[
    Prim::Poly(
        &[[0.12, 0.12], [0.60, 0.12], [0.60, 0.60], [0.12, 0.60]],
        true,
    ),
    Prim::Poly(
        &[[0.40, 0.40], [0.88, 0.40], [0.88, 0.88], [0.40, 0.88]],
        true,
    ),
];

const CLIPPING: &[Prim] = &[
    Prim::Poly(&[[0.28, 0.16], [0.28, 0.66], [0.80, 0.66]], false),
    Prim::Poly(&[[0.64, 0.52], [0.80, 0.66], [0.64, 0.80]], false),
];

const NEW_GROUP: &[Prim] = &[
    Prim::Poly(
        &[
            [0.10, 0.76],
            [0.10, 0.24],
            [0.38, 0.24],
            [0.46, 0.36],
            [0.90, 0.36],
            [0.90, 0.76],
        ],
        true,
    ),
    Prim::Line([0.68, 0.46], [0.68, 0.66]),
    Prim::Line([0.58, 0.56], [0.78, 0.56]),
];

// --- layer classes ---------------------------------------------------------

const LAYER_RASTER: &[Prim] = &[
    Prim::Poly(
        &[[0.12, 0.20], [0.88, 0.20], [0.88, 0.80], [0.12, 0.80]],
        true,
    ),
    Prim::Poly(&[[0.12, 0.68], [0.36, 0.44], [0.58, 0.68]], false),
    Prim::Dot([0.68, 0.36], 0.07),
];

const LAYER_GROUP: &[Prim] = &[Prim::Poly(
    &[
        [0.10, 0.78],
        [0.10, 0.22],
        [0.38, 0.22],
        [0.46, 0.34],
        [0.90, 0.34],
        [0.90, 0.78],
    ],
    true,
)];

const LAYER_ADJUSTMENT: &[Prim] = &[
    Prim::Circle([0.5, 0.5], 0.34),
    Prim::Line([0.5, 0.16], [0.5, 0.84]),
    Prim::Line([0.24, 0.34], [0.50, 0.34]),
    Prim::Line([0.20, 0.50], [0.50, 0.50]),
    Prim::Line([0.24, 0.66], [0.50, 0.66]),
];

const LAYER_TEXT: &[Prim] = &[
    Prim::Line([0.18, 0.20], [0.82, 0.20]),
    Prim::Line([0.50, 0.20], [0.50, 0.80]),
    Prim::Line([0.34, 0.80], [0.66, 0.80]),
];

const LAYER_SHAPE: &[Prim] = &[Prim::Poly(
    &[[0.50, 0.12], [0.88, 0.50], [0.50, 0.88], [0.12, 0.50]],
    true,
)];

const LAYER_SMART_OBJECT: &[Prim] = &[
    Prim::Poly(
        &[[0.14, 0.18], [0.86, 0.18], [0.86, 0.82], [0.14, 0.82]],
        true,
    ),
    Prim::Poly(
        &[[0.14, 0.50], [0.50, 0.18], [0.86, 0.50], [0.50, 0.82]],
        true,
    ),
];

const LAYER_GENERATOR: &[Prim] = &[
    Prim::Poly(
        &[
            [0.50, 0.10],
            [0.60, 0.40],
            [0.90, 0.50],
            [0.60, 0.60],
            [0.50, 0.90],
            [0.40, 0.60],
            [0.10, 0.50],
            [0.40, 0.40],
        ],
        true,
    ),
    Prim::Dot([0.50, 0.50], 0.06),
];

// --- lock toggles ----------------------------------------------------------

const LOCK_TRANSPARENCY: &[Prim] = &[
    Prim::Poly(
        &[[0.16, 0.16], [0.84, 0.16], [0.84, 0.84], [0.16, 0.84]],
        true,
    ),
    Prim::Line([0.16, 0.50], [0.50, 0.16]),
    Prim::Line([0.16, 0.84], [0.84, 0.16]),
    Prim::Line([0.50, 0.84], [0.84, 0.50]),
];

const LOCK_PIXELS: &[Prim] = &[
    Prim::Poly(
        &[[0.16, 0.16], [0.84, 0.16], [0.84, 0.84], [0.16, 0.84]],
        true,
    ),
    Prim::Dot([0.34, 0.34], 0.07),
    Prim::Dot([0.66, 0.34], 0.07),
    Prim::Dot([0.34, 0.66], 0.07),
    Prim::Dot([0.66, 0.66], 0.07),
];

const LOCK_POSITION: &[Prim] = &[
    Prim::Line([0.5, 0.16], [0.5, 0.84]),
    Prim::Line([0.16, 0.5], [0.84, 0.5]),
    Prim::Poly(&[[0.38, 0.28], [0.5, 0.16], [0.62, 0.28]], false),
    Prim::Poly(&[[0.38, 0.72], [0.5, 0.84], [0.62, 0.72]], false),
    Prim::Poly(&[[0.28, 0.38], [0.16, 0.5], [0.28, 0.62]], false),
    Prim::Poly(&[[0.72, 0.38], [0.84, 0.5], [0.72, 0.62]], false),
];

const LOCK_ALL: &[Prim] = &[
    Prim::Poly(
        &[[0.22, 0.46], [0.78, 0.46], [0.78, 0.88], [0.22, 0.88]],
        true,
    ),
    Prim::Poly(
        &[
            [0.34, 0.46],
            [0.34, 0.26],
            [0.42, 0.14],
            [0.58, 0.14],
            [0.66, 0.26],
            [0.66, 0.46],
        ],
        false,
    ),
    Prim::Dot([0.50, 0.66], 0.07),
];

// --- adjustments -----------------------------------------------------------

const ADJ_BRIGHTNESS_CONTRAST: &[Prim] = &[
    Prim::Circle([0.5, 0.5], 0.26),
    Prim::Line([0.5, 0.06], [0.5, 0.20]),
    Prim::Line([0.5, 0.80], [0.5, 0.94]),
    Prim::Line([0.06, 0.5], [0.20, 0.5]),
    Prim::Line([0.80, 0.5], [0.94, 0.5]),
    Prim::Line([0.5, 0.28], [0.5, 0.72]),
];

const ADJ_LEVELS: &[Prim] = &[
    Prim::Poly(&[[0.14, 0.78], [0.86, 0.78], [0.86, 0.22]], true),
    Prim::Line([0.14, 0.90], [0.86, 0.90]),
    Prim::Dot([0.50, 0.90], 0.06),
];

const ADJ_CURVES: &[Prim] = &[
    Prim::Poly(&[[0.14, 0.14], [0.14, 0.86], [0.86, 0.86]], false),
    Prim::Poly(
        &[
            [0.18, 0.80],
            [0.34, 0.72],
            [0.46, 0.44],
            [0.62, 0.30],
            [0.84, 0.20],
        ],
        false,
    ),
];

const ADJ_EXPOSURE: &[Prim] = &[
    Prim::Circle([0.5, 0.5], 0.20),
    Prim::Line([0.5, 0.06], [0.5, 0.22]),
    Prim::Line([0.5, 0.78], [0.5, 0.94]),
    Prim::Line([0.06, 0.5], [0.22, 0.5]),
    Prim::Line([0.78, 0.5], [0.94, 0.5]),
    Prim::Line([0.20, 0.20], [0.32, 0.32]),
    Prim::Line([0.68, 0.68], [0.80, 0.80]),
    Prim::Line([0.80, 0.20], [0.68, 0.32]),
    Prim::Line([0.32, 0.68], [0.20, 0.80]),
];

const ADJ_VIBRANCE: &[Prim] = &[
    Prim::Poly(&[[0.5, 0.10], [0.86, 0.86], [0.14, 0.86]], true),
    Prim::Line([0.32, 0.58], [0.68, 0.58]),
];

const ADJ_HUE_SATURATION: &[Prim] = &[
    Prim::Circle([0.5, 0.5], 0.34),
    Prim::Circle([0.5, 0.5], 0.12),
    Prim::Line([0.5, 0.16], [0.5, 0.38]),
];

const ADJ_COLOR_BALANCE: &[Prim] = &[
    Prim::Line([0.10, 0.30], [0.90, 0.30]),
    Prim::Line([0.50, 0.30], [0.50, 0.86]),
    Prim::Line([0.30, 0.86], [0.70, 0.86]),
    Prim::Poly(&[[0.06, 0.44], [0.22, 0.44], [0.14, 0.30]], true),
    Prim::Poly(&[[0.78, 0.44], [0.94, 0.44], [0.86, 0.30]], true),
];

const ADJ_BLACK_AND_WHITE: &[Prim] = &[
    Prim::Circle([0.36, 0.50], 0.26),
    Prim::Circle([0.64, 0.50], 0.26),
];

const ADJ_PHOTO_FILTER: &[Prim] = &[
    Prim::Circle([0.5, 0.5], 0.36),
    Prim::Circle([0.5, 0.5], 0.20),
    Prim::Circle([0.5, 0.5], 0.06),
];

const ADJ_CHANNEL_MIXER: &[Prim] = &[
    Prim::Line([0.50, 0.90], [0.50, 0.50]),
    Prim::Line([0.50, 0.50], [0.16, 0.14]),
    Prim::Line([0.50, 0.50], [0.84, 0.14]),
    Prim::Dot([0.16, 0.14], 0.06),
    Prim::Dot([0.84, 0.14], 0.06),
];

const ADJ_INVERT: &[Prim] = &[
    Prim::Poly(
        &[[0.14, 0.14], [0.86, 0.14], [0.86, 0.86], [0.14, 0.86]],
        true,
    ),
    Prim::Line([0.50, 0.14], [0.50, 0.86]),
    Prim::Line([0.14, 0.30], [0.50, 0.30]),
    Prim::Line([0.14, 0.50], [0.50, 0.50]),
    Prim::Line([0.14, 0.70], [0.50, 0.70]),
];

const ADJ_POSTERIZE: &[Prim] = &[Prim::Poly(
    &[
        [0.12, 0.86],
        [0.12, 0.64],
        [0.38, 0.64],
        [0.38, 0.42],
        [0.64, 0.42],
        [0.64, 0.20],
        [0.88, 0.20],
    ],
    false,
)];

const ADJ_THRESHOLD: &[Prim] = &[
    Prim::Poly(
        &[[0.14, 0.14], [0.86, 0.14], [0.86, 0.86], [0.14, 0.86]],
        true,
    ),
    Prim::Line([0.14, 0.14], [0.86, 0.86]),
    Prim::Line([0.30, 0.70], [0.70, 0.70]),
];

const ADJ_GRADIENT_MAP: &[Prim] = &[
    Prim::Poly(
        &[[0.10, 0.28], [0.90, 0.28], [0.90, 0.72], [0.10, 0.72]],
        true,
    ),
    Prim::Line([0.30, 0.28], [0.30, 0.72]),
    Prim::Line([0.50, 0.28], [0.50, 0.72]),
    Prim::Line([0.70, 0.28], [0.70, 0.72]),
    Prim::Dot([0.90, 0.86], 0.06),
];

const ADJ_SELECTIVE_COLOR: &[Prim] = &[
    Prim::Poly(
        &[[0.50, 0.08], [0.92, 0.50], [0.50, 0.92], [0.08, 0.50]],
        true,
    ),
    Prim::Poly(
        &[[0.50, 0.30], [0.70, 0.50], [0.50, 0.70], [0.30, 0.50]],
        true,
    ),
];

// --- history step kinds ----------------------------------------------------

const STEP_OPEN: &[Prim] = &[
    Prim::Poly(
        &[[0.14, 0.20], [0.86, 0.20], [0.86, 0.80], [0.14, 0.80]],
        true,
    ),
    Prim::Line([0.14, 0.36], [0.86, 0.36]),
];

const STEP_LAYER_ADDED: &[Prim] = &[
    Prim::Poly(
        &[[0.10, 0.62], [0.50, 0.42], [0.90, 0.62], [0.50, 0.82]],
        true,
    ),
    Prim::Line([0.50, 0.10], [0.50, 0.34]),
    Prim::Line([0.38, 0.22], [0.62, 0.22]),
];

const STEP_LAYER_REMOVED: &[Prim] = &[
    Prim::Poly(
        &[[0.10, 0.62], [0.50, 0.42], [0.90, 0.62], [0.50, 0.82]],
        true,
    ),
    Prim::Line([0.38, 0.22], [0.62, 0.22]),
];

const STEP_LAYER_MOVED: &[Prim] = &[
    Prim::Line([0.5, 0.10], [0.5, 0.90]),
    Prim::Poly(&[[0.36, 0.24], [0.5, 0.10], [0.64, 0.24]], false),
    Prim::Poly(&[[0.36, 0.76], [0.5, 0.90], [0.64, 0.76]], false),
];

const STEP_LAYER_CHANGED: &[Prim] = &[
    Prim::Line([0.12, 0.32], [0.88, 0.32]),
    Prim::Dot([0.34, 0.32], 0.08),
    Prim::Line([0.12, 0.68], [0.88, 0.68]),
    Prim::Dot([0.66, 0.68], 0.08),
];

const STEP_TRANSFORMED: &[Prim] = &[
    Prim::Line([0.16, 0.84], [0.84, 0.16]),
    Prim::Poly(&[[0.16, 0.60], [0.16, 0.84], [0.40, 0.84]], false),
    Prim::Poly(&[[0.60, 0.16], [0.84, 0.16], [0.84, 0.40]], false),
];

const STEP_PAINTED: &[Prim] = &[
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
    Prim::Dot([0.20, 0.80], 0.05),
];

const STEP_FILLED: &[Prim] = &[
    Prim::Poly(
        &[[0.16, 0.16], [0.84, 0.16], [0.84, 0.84], [0.16, 0.84]],
        true,
    ),
    Prim::Line([0.16, 0.30], [0.84, 0.30]),
    Prim::Line([0.16, 0.44], [0.84, 0.44]),
    Prim::Line([0.16, 0.58], [0.84, 0.58]),
    Prim::Line([0.16, 0.72], [0.84, 0.72]),
];

const STEP_CLEARED: &[Prim] = &[
    Prim::Poly(&[[0.16, 0.34], [0.16, 0.16], [0.34, 0.16]], false),
    Prim::Poly(&[[0.66, 0.16], [0.84, 0.16], [0.84, 0.34]], false),
    Prim::Poly(&[[0.84, 0.66], [0.84, 0.84], [0.66, 0.84]], false),
    Prim::Poly(&[[0.34, 0.84], [0.16, 0.84], [0.16, 0.66]], false),
];

const STEP_BATCH: &[Prim] = &[
    Prim::Line([0.16, 0.28], [0.84, 0.28]),
    Prim::Line([0.16, 0.50], [0.84, 0.50]),
    Prim::Line([0.16, 0.72], [0.84, 0.72]),
];

const STEP_UNKNOWN: &[Prim] = &[Prim::Dot([0.5, 0.5], 0.16)];

/// Photopea's footer chain link: two rings joined by a bar.
const LINK: &[Prim] = &[
    Prim::Circle([0.26, 0.38], 0.14),
    Prim::Circle([0.74, 0.62], 0.14),
    Prim::Line([0.38, 0.47], [0.62, 0.53]),
];

/// The adjustment footer button: a half-moon — a circle whose right half is
/// filled (the arc approximated by a short chord).
const ADJUSTMENT: &[Prim] = &[
    Prim::Circle([0.5, 0.5], 0.34),
    Prim::Fill(&[
        [0.5, 0.16],
        [0.63, 0.185],
        [0.74, 0.26],
        [0.815, 0.37],
        [0.84, 0.5],
        [0.815, 0.63],
        [0.74, 0.74],
        [0.63, 0.815],
        [0.5, 0.84],
    ]),
];

/// Photopea's footer mask button: a rectangle with a dot, the mask-on-shape.
const MASK: &[Prim] = &[
    Prim::Poly(&[[0.18, 0.3], [0.82, 0.3], [0.82, 0.7], [0.18, 0.7]], true),
    Prim::Dot([0.5, 0.5], 0.11),
];

/// The drawing for a chrome icon key.
///
/// The companion to [`icon_for`], for everything that is not a tool: the panel
/// headers, the Layers list, the Adjustments grid, the History markers. Total
/// over every key this crate uses — see the gates in the module note.
pub fn ui_icon(key: &str) -> Icon {
    Icon(match key {
        // headers and list chrome
        "chevron-right" => CHEVRON_RIGHT,
        "chevron-down" => CHEVRON_DOWN,
        "chevron-up" => CHEVRON_UP,
        "chevron-left" => CHEVRON_LEFT,
        "anchor-block" => ANCHOR_BLOCK,
        "close" => CLOSE,
        "overflow" => OVERFLOW,
        "check" => CHECK,
        "plus" => PLUS,
        "minus" => MINUS,
        "eye" => EYE,
        "trash" => TRASH,
        "target" => TARGET,
        "swap" => SWAP,
        "colors-default" => COLORS_DEFAULT,
        "clipping" => CLIPPING,
        "new-group" => NEW_GROUP,
        "link" => LINK,
        "adjustment" => ADJUSTMENT,
        "mask" => MASK,
        // layer classes
        "layer-raster" => LAYER_RASTER,
        "layer-group" => LAYER_GROUP,
        "layer-adjustment" => LAYER_ADJUSTMENT,
        "layer-text" => LAYER_TEXT,
        "layer-shape" => LAYER_SHAPE,
        "layer-smart-object" => LAYER_SMART_OBJECT,
        "layer-generator" => LAYER_GENERATOR,
        // lock toggles
        "lock-transparency" => LOCK_TRANSPARENCY,
        "lock-pixels" => LOCK_PIXELS,
        "lock-position" => LOCK_POSITION,
        "lock-all" => LOCK_ALL,
        // adjustments
        "adj-brightness-contrast" => ADJ_BRIGHTNESS_CONTRAST,
        "adj-levels" => ADJ_LEVELS,
        "adj-curves" => ADJ_CURVES,
        "adj-exposure" => ADJ_EXPOSURE,
        "adj-vibrance" => ADJ_VIBRANCE,
        "adj-hue-saturation" => ADJ_HUE_SATURATION,
        "adj-color-balance" => ADJ_COLOR_BALANCE,
        "adj-black-and-white" => ADJ_BLACK_AND_WHITE,
        "adj-photo-filter" => ADJ_PHOTO_FILTER,
        "adj-channel-mixer" => ADJ_CHANNEL_MIXER,
        "adj-invert" => ADJ_INVERT,
        "adj-posterize" => ADJ_POSTERIZE,
        "adj-threshold" => ADJ_THRESHOLD,
        "adj-gradient-map" => ADJ_GRADIENT_MAP,
        "adj-selective-color" => ADJ_SELECTIVE_COLOR,
        // history step kinds
        "step-open" => STEP_OPEN,
        "step-layer-added" => STEP_LAYER_ADDED,
        "step-layer-removed" => STEP_LAYER_REMOVED,
        "step-layer-moved" => STEP_LAYER_MOVED,
        "step-layer-changed" => STEP_LAYER_CHANGED,
        "step-transformed" => STEP_TRANSFORMED,
        "step-painted" => STEP_PAINTED,
        "step-filled" => STEP_FILLED,
        "step-cleared" => STEP_CLEARED,
        "step-batch" => STEP_BATCH,
        "step-unknown" => STEP_UNKNOWN,
        _ => return Icon::UNKNOWN,
    })
}

/// Every fixed chrome key a call site in this crate names.
///
/// The sets that come from an enum — the adjustments, the step kinds, the layer
/// classes, the locks — are gated through that enum instead, so this is only
/// the one-off keys. It exists so a key can be renamed in one place and caught
/// in the other.
pub const CHROME_ICON_KEYS: &[&str] = &[
    "chevron-right",
    "chevron-down",
    "chevron-up",
    "chevron-left",
    "anchor-block",
    "close",
    "overflow",
    "check",
    "plus",
    "minus",
    "eye",
    "trash",
    "target",
    "swap",
    "colors-default",
    "clipping",
    "new-group",
    "link",
    "adjustment",
    "mask",
];

/// Draw the chrome icon `key` centred in `rect`, in the palette's colour for
/// `role`.
///
/// The one place a chrome icon key turns into pixels — the panel headers, the
/// menu tick, the Layers wells and the History markers all come through here.
/// A key with no drawing is painted in the danger colour rather than left
/// blank, for the same reason [`icon_button`] does it: a control that ships
/// without a drawing should look wrong, not look disabled.
/// The stroke every icon is painted at: Photopea's glyphs read at 16 pt, which
/// the hairline does not and the thick outline overshoots, so they have their
/// own rung on the width scale.
pub fn icon_stroke_width(t: &design::Tokens) -> f32 {
    t.borders.icon
}

pub fn paint_ui_icon(ui: &egui::Ui, rect: Rect, key: &str, role: design::TextRole) {
    let t = design::current_tokens(ui);
    let icon = ui_icon(key);
    let color = if icon.is_unknown() {
        design::color32(t.palette.color(design::ColorRole::Danger))
    } else {
        design::color32(t.palette.text(role))
    };
    icon.paint(
        &ui.painter_at(rect),
        rect.shrink(design::Space::XSmall.pt()),
        color,
        icon_stroke_width(t),
    );
}

/// A borderless square button the size of a hit target, showing the drawing
/// for the chrome icon `key`.
///
/// This is the one shape every chrome affordance is built from — the panel
/// header's collapse chevron, its close and its overflow, the Layers eye and
/// lock wells, and the shell's own per-document tab close. It is public
/// because `app-shell` draws chrome too, and a close button in the tab strip
/// built a second way is how the typed `"×"` survived the first sweep of this
/// bug.
///
/// `id` is for the controls a headless test has to be able to click by name;
/// pass `None` and egui derives one from the position.
pub fn ui_icon_button_id(
    ui: &mut egui::Ui,
    key: &str,
    tooltip: &str,
    role: design::TextRole,
    id: Option<egui::Id>,
) -> egui::Response {
    let t = design::current_tokens(ui);
    let side = t.metrics.min_hit_target;
    let (rect, auto) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::click());
    let response = match id {
        Some(id) => ui.interact(rect, id, egui::Sense::click()),
        None => auto,
    };
    if ui.is_rect_visible(rect) {
        if response.hovered() {
            let radius = design::Radius::Small.resolve(&t.radii, side);
            ui.painter().rect_filled(
                rect,
                design::egui_theme::rounding(radius),
                design::color32(t.palette.color(design::ColorRole::ControlFillHovered)),
            );
        }
        paint_ui_icon(ui, rect, key, role);
    }
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

/// [`ui_icon_button_id`] without an explicit id.
pub fn ui_icon_button(
    ui: &mut egui::Ui,
    key: &str,
    tooltip: &str,
    role: design::TextRole,
) -> egui::Response {
    ui_icon_button_id(ui, key, tooltip, role, None)
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

    /// Every chrome key any surface can ask for, from the fixed list and from
    /// each enum that names one.
    fn all_chrome_keys() -> Vec<&'static str> {
        let mut keys: Vec<&'static str> = CHROME_ICON_KEYS.to_vec();
        keys.extend(
            crate::menu::AdjustmentId::ALL
                .iter()
                .map(|id| crate::panels::properties::AdjustmentsPanel::icon(*id)),
        );
        keys.extend(
            crate::panels::history::StepKind::ALL
                .iter()
                .map(|k| k.icon()),
        );
        keys.extend(
            crate::menu::LayerClass::ALL
                .iter()
                .map(|c| crate::view::kind_icon(*c)),
        );
        keys.extend(
            crate::view::LockToggle::ALL
                .iter()
                .map(|t| t.icon_and_tooltip().0),
        );
        keys
    }

    #[test]
    fn every_chrome_icon_key_has_a_drawing() {
        // The same promise `every_registry_icon_key_has_a_drawing` makes for the
        // tool palette, for everything else: a new adjustment, a new history
        // step kind, a new layer class or a new lock toggle cannot ship without
        // one. Each set is walked through the enum that defines it, via that
        // enum's `ALL`.
        //
        // `ALL` is hand-written, though — `StepKind::ALL` is a `[StepKind; 11]`
        // — so a variant added without extending it compiles and drops out of
        // this walk silently. `a_new_variant_of_an_iconed_enum_cannot_compile_\
        // without_visiting_this_gate`, below, is what stands in the way of that.
        for key in all_chrome_keys() {
            assert!(
                !ui_icon(key).is_unknown(),
                "chrome icon key {key:?} has no drawing; the control would be blank"
            );
        }
    }

    #[test]
    fn a_new_variant_of_an_iconed_enum_cannot_compile_without_visiting_this_gate() {
        // Four enums name a chrome icon key, and each hands this gate its set
        // through a hand-written `ALL`. A hand-written list is the weak link:
        // add `StepKind::Masked` and forget to extend `StepKind::ALL` and
        // everything still compiles, `all_chrome_keys` never sees the new key,
        // and the History panel paints `Icon::UNKNOWN` — a hollow square, the
        // very shape this whole exercise was about removing.
        //
        // These matches have no wildcard arm, so a new variant of any of the
        // four is a *compile error in this file*. That is the guarantee, and it
        // is a guarantee about attention rather than about arithmetic: whoever
        // adds the variant is standing in the gate, next to the assertion above
        // and the counts below, when the compiler stops them. Extend the arm,
        // extend `ALL`, and add the drawing.
        use crate::menu::{AdjustmentId, LayerClass};
        use crate::panels::history::StepKind;
        use crate::view::LockToggle;

        match AdjustmentId::BrightnessContrast {
            AdjustmentId::BrightnessContrast
            | AdjustmentId::Levels
            | AdjustmentId::Curves
            | AdjustmentId::Exposure
            | AdjustmentId::Vibrance
            | AdjustmentId::HueSaturation
            | AdjustmentId::ColorBalance
            | AdjustmentId::BlackAndWhite
            | AdjustmentId::PhotoFilter
            | AdjustmentId::ChannelMixer
            | AdjustmentId::Invert
            | AdjustmentId::Posterize
            | AdjustmentId::Threshold
            | AdjustmentId::GradientMap
            | AdjustmentId::SelectiveColor => {}
        }
        match StepKind::Open {
            StepKind::Open
            | StepKind::LayerAdded
            | StepKind::LayerRemoved
            | StepKind::LayerMoved
            | StepKind::LayerChanged
            | StepKind::Transformed
            | StepKind::Painted
            | StepKind::Filled
            | StepKind::Cleared
            | StepKind::Batch
            | StepKind::Unknown => {}
        }
        match LayerClass::Raster {
            LayerClass::Raster
            | LayerClass::Group
            | LayerClass::Adjustment
            | LayerClass::Text
            | LayerClass::Shape
            | LayerClass::SmartObject
            | LayerClass::Generator => {}
        }
        match LockToggle::Transparency {
            LockToggle::Transparency
            | LockToggle::Pixels
            | LockToggle::Position
            | LockToggle::All => {}
        }

        // One per arm above. If the compiler sent you here, these are what tell
        // you whether the matching `ALL` was extended as well.
        assert_eq!(AdjustmentId::ALL.len(), 15, "extend the match above too");
        assert_eq!(StepKind::ALL.len(), 11, "extend the match above too");
        assert_eq!(LayerClass::ALL.len(), 7, "extend the match above too");
        assert_eq!(LockToggle::ALL.len(), 4, "extend the match above too");
    }

    #[test]
    fn no_chrome_icon_key_is_claimed_by_two_controls() {
        // A key named twice means two controls share a picture, which is the
        // same class of bug as a blank button: the user cannot tell them apart.
        //
        // This reads the *declared* set — `CHROME_ICON_KEYS` plus the keys the
        // enums derive — and says nothing about call sites. The matching
        // question, whether every declared key is actually asked for by some
        // control, needs the source and is answered by
        // `every_chrome_icon_key_is_claimed_by_a_control` in
        // `tests/no_typed_ui_glyphs.rs`.
        let declared = all_chrome_keys();
        let mut sorted = declared.clone();
        sorted.sort_unstable();
        let count = sorted.len();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            count,
            "an icon key is claimed by two controls"
        );
    }

    #[test]
    fn no_two_chrome_controls_share_a_drawing() {
        let mut seen: Vec<(&'static str, &'static [Prim])> = Vec::new();
        for key in all_chrome_keys() {
            let icon = ui_icon(key);
            if let Some((other, _)) = seen
                .iter()
                .find(|(_, prims)| std::ptr::eq(prims.as_ptr(), icon.0.as_ptr()))
            {
                panic!("{key:?} draws the same icon as {other:?}");
            }
            seen.push((key, icon.0));
        }
    }

    #[test]
    fn every_point_of_every_chrome_icon_is_inside_the_unit_square() {
        let mut checked = 0usize;
        for key in all_chrome_keys() {
            for prim in ui_icon(key).0 {
                let points: Vec<[f32; 2]> = match prim {
                    Prim::Poly(p, _) => {
                        assert!(p.len() >= 2, "{key:?} has a one-point polyline");
                        p.to_vec()
                    }
                    Prim::Fill(p) => {
                        assert!(p.len() >= 3, "{key:?} has a fill with no area");
                        p.to_vec()
                    }
                    Prim::Circle(c, r) | Prim::Dot(c, r) => {
                        vec![[c[0] - r, c[1] - r], [c[0] + r, c[1] + r]]
                    }
                    Prim::Line(a, b) => vec![*a, *b],
                };
                for p in points {
                    assert!(
                        (0.0..=1.0).contains(&p[0]) && (0.0..=1.0).contains(&p[1]),
                        "{key:?} has a point outside the unit square: {p:?}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 100, "the chrome icon set lost its geometry");
    }

    #[test]
    fn painting_a_chrome_icon_draws_more_than_painting_nothing() {
        // The point of the whole exercise: `paint_ui_icon` must emit geometry.
        // A key that resolved to an empty drawing would look exactly like the
        // tofu box it replaced, only quieter.
        let with = shapes_in_frame(|ui| {
            let rect = ui.available_rect_before_wrap();
            paint_ui_icon(ui, rect, "close", design::TextRole::Primary);
        });
        let without = shapes_in_frame(|_| {});
        assert!(
            with > without,
            "paint_ui_icon emitted no geometry ({with} vs {without})"
        );

        // And the same for a `Prim::Fill`, which goes down a different egui path
        // (`convex_polygon`) from every other primitive. A fill that emitted
        // nothing would leave the selected Canvas Size anchor cell empty.
        let filled = shapes_in_frame(|ui| {
            let rect = ui.available_rect_before_wrap();
            paint_ui_icon(ui, rect, "anchor-block", design::TextRole::Primary);
        });
        assert!(
            matches!(ui_icon("anchor-block").0, [Prim::Fill(_)]),
            "the anchor block is not a filled mark any more, so it is a hollow \
             square again — the same shape as the tofu box it replaced"
        );
        assert!(
            filled > without,
            "a filled icon emitted no geometry ({filled} vs {without})"
        );
    }

    #[test]
    fn an_unrecognised_key_falls_back_visibly_rather_than_to_nothing() {
        assert!(ui_icon("no-such-chrome-icon").is_unknown());
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
                    Prim::Poly(p, _) | Prim::Fill(p) => p.to_vec(),
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

    /// The weight-pass gate (P1.15): every icon carries a minimum share of
    /// ink at the tool-button size, where thin strokes read as nothing. The
    /// floor sits under the thinnest drawings in the set — the two-segment
    /// chevrons at about 0.245, shape-line at about 0.30 of their squares —
    /// and above anything a weight regression produces.
    #[test]
    fn every_icon_carries_enough_ink_at_the_tool_button_size() {
        let tokens = design::Theme::Dark.tokens();
        let side = tokens.metrics.toolbar_button - design::Space::Small.pt() * 2.0;
        let width = icon_stroke_width(tokens);
        // Stroked glyphs must lay down a real share of their square to read at
        // 16 pt. The thinnest legitimate marks set the floor: the one-segment
        // minus at about 0.18, the two-segment chevrons at about 0.245 — a
        // regression to the old hairline-weight strokes lands near 0.05 and
        // fails by miles. Solid marks (a filled square, dots) cover less area
        // but are denser where they sit, so they carry their own, lower floor.
        const STROKE_FLOOR: f32 = 0.17;
        const SOLID_FLOOR: f32 = 0.08;

        let floor_for = |icon: Icon| -> f32 {
            let solid_only = icon
                .0
                .iter()
                .all(|p| matches!(p, Prim::Fill(_) | Prim::Dot(_, _)));
            if solid_only {
                SOLID_FLOOR
            } else {
                STROKE_FLOOR
            }
        };

        for info in tools::registry::all() {
            let icon = icon_for(info.icon);
            let coverage = icon.ink_area(side, width) / (side * side);
            assert!(
                coverage >= floor_for(icon),
                "{:?} ({}) covers only {coverage:.4} of its {}pt square at {}pt stroke",
                info.name,
                info.icon,
                side,
                width
            );
        }

        for key in all_chrome_keys() {
            let icon = ui_icon(key);
            let coverage = icon.ink_area(side, width) / (side * side);
            assert!(
                coverage >= floor_for(icon),
                "chrome icon {key:?} covers only {coverage:.4} of its square"
            );
        }
    }

    /// The weight pass means something: the dedicated icon width is heavier
    /// than the hairline the icons used to borrow, and lighter than the
    /// emphasised outline.
    #[test]
    fn the_icon_stroke_weight_sits_between_the_hairline_and_the_thick_outline() {
        let tokens = design::Theme::Dark.tokens();
        let w = icon_stroke_width(tokens);
        assert!(
            w > tokens.borders.hairline,
            "icons must out-weigh {w}pt chrome borders"
        );
        assert!(
            w < tokens.borders.thick,
            "icons must stay under {w}pt emphasised outlines"
        );
    }
}
