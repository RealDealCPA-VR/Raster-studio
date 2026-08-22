//! Interactive tools. Each tool is isolated from persistence and rendering
//! internals: it receives pointer events and a [`ToolContext`], mutates an
//! in-progress interaction, and ultimately emits [`editor_core::Command`]s that
//! the app applies through history. Tools never touch the GPU or disk directly.
//!
//! Delivery order (per roadmap): hand/pan+zoom → move/transform → crop → brush/
//! eraser → rect/ellipse selection → lasso → wand → clone/heal → gradient →
//! pen/vector.

pub mod brush;
pub mod tool;

pub use brush::{BrushSettings, BrushStroke, BrushTool};
pub use tool::{PointerEvent, Tool, ToolContext, ToolId};
