//! The authoritative CPU tile compositor.
//!
//! This crate answers one question: given a [`editor_core::Document`] and a
//! region, what pixels does the user see? Everything else in the editor —
//! the GPU presenter, export, thumbnails, golden-image tests — consumes this
//! answer rather than computing its own.
//!
//! Having exactly one implementation is deliberate. A GPU compositor running
//! alongside a CPU one is two sources of truth that drift; here the GPU only
//! presents tiles this crate produced.
//!
//! # Working space
//!
//! Compositing happens in **linear, premultiplied** `f32`. Inputs are
//! converted on the way in and results converted back on the way out, so the
//! blend, mask and adjustment maths never sees gamma-encoded values.
