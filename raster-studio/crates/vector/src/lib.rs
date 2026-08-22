//! Bezier path geometry and anti-aliased rasterisation.
//!
//! Shapes, the pen tool, vector masks and text outlines all reduce to the same
//! two operations: build a path, then turn it into coverage. This crate owns
//! both, and deliberately produces a **coverage mask** rather than colour, so
//! fills, strokes, masks and selections share one rasteriser instead of each
//! growing their own.
//!
//! It is a leaf crate: no document, no I/O, no GPU.
