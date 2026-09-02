//! The visual language of Raster Studio: Photopea’s — dark-first neutral
//! greys, compact density, restrained chrome.
//!
//! Three layers, in dependency order:
//!
//! 1. [`tokens`] — plain data. Colors, type scale, 4pt grid, radii, elevation,
//!    motion. No `egui`, so the design system is unit-testable: WCAG contrast,
//!    scale monotonicity and grid alignment are asserted without a window.
//! 2. [`egui_theme`] — the one module that maps tokens onto [`egui::Style`].
//! 3. [`widgets`] — themed primitives that read the installed theme back out of
//!    the [`egui::Context`], so no call site ever names a color.
//!
//! ```no_run
//! # let ctx = egui::Context::default();
//! design::apply_theme(&ctx, design::Theme::Dark);
//! ```
//!
//! Design intent: content-first and restrained. Panels recede, the image is the
//! hero, hierarchy comes from spacing and weight rather than from boxes and
//! rules. Nothing grows on hover; depth is carried by blur, not by darkness.

#![forbid(unsafe_code)]

pub mod egui_theme;
pub mod theme;
pub mod tokens;
pub mod widgets;

pub use egui_theme::{apply_theme, color32, current_theme, current_tokens, style_for};
pub use theme::{Theme, Tokens};
pub use tokens::{
    contrast_ratio, contrast_ratio_over, BorderWidths, ColorRole, Easing, Elevation, Metrics,
    Motion, Palette, Radii, Radius, ShadowSpec, Space, Srgba, SurfaceRole, TextRole, TextSize,
    TypeRole, TypeScale, TypeStyle, UNIT_PT,
};
pub use widgets::{
    ghost_button, inspector_field, list_row, primary_button, secondary_button, section_header,
    segmented_control, slider_row, toolbar_icon_button, TextPairing,
};
