//! Workspace chrome: the menu bar, tool palette, tool options, and the docked
//! panels the user works in.
//!
//! Everything visual here comes from `design` tokens. No colour, font size,
//! radius or spacing is written literally in this module tree — a re-skin has
//! to be possible by editing one crate.
//!
//! Panels are views. They read the document and push [`crate::Intent`]s into an
//! outbox; they never mutate it. That is what keeps undo/redo uniform, and it
//! is also what makes this code testable without a window: the logic that
//! decides *which* command a click produces is separable from the drawing, and
//! it lives in the `*Model` / `*State` types each module leads with.

pub mod actions;
// W10-I: the frame-animation timeline.
pub mod animation;
pub mod brushes;
pub mod channels;
pub mod color;
pub mod histogram;
pub mod history;
pub mod layers;
pub mod navigator;
pub mod paths;
pub mod properties;
pub mod text;
// W10-B: the document-record panels and the tool presets.
pub mod glyphs;
pub mod layer_comps;
pub mod notes;
pub mod text_styles;
pub mod tool_presets;
// W11-I: Window > CSS.
pub mod css;
