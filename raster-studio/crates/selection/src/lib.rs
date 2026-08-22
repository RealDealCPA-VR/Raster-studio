//! Selection algorithms: how a selection is *made* and *modified*.
//!
//! The selection **type** — a per-pixel coverage mask — lives in
//! [`editor_core::Selection`], because the document owns the current
//! selection and commands have to be able to change it. This crate sits above
//! that and provides the operations: marquee and lasso shapes, the magic wand
//! and colour range, the morphological modifiers (feather, expand, contract,
//! smooth, border), and outline extraction for the marching-ants overlay.
//!
//! Coverage is partial, never binary. Anti-aliased and feathered edges are the
//! normal case, so every operation here is defined on fractional coverage.
