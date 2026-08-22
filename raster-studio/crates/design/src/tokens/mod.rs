//! Design tokens as plain data.
//!
//! **Constraint:** nothing under `tokens` may depend on `egui` or on any
//! rendering backend. The tokens are the checkable part of the design system —
//! contrast, scale monotonicity and grid alignment are all asserted against
//! these values without a window ever existing.

pub mod color;
pub mod elevation;
pub mod motion;
pub mod palette;
pub mod shape;
pub mod spacing;
pub mod typography;

pub use color::{contrast_ratio, contrast_ratio_over, Srgba, TextSize};
pub use elevation::{Elevation, ShadowSpec};
pub use motion::{Easing, Motion};
pub use palette::{ColorRole, Palette, SurfaceRole, TextRole};
pub use shape::{BorderWidths, Radii, Radius};
pub use spacing::{grid, Metrics, Space, UNIT_PT};
pub use typography::{FontWeight, TypeRole, TypeScale, TypeStyle};
