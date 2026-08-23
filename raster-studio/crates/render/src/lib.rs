//! GPU renderer and compositing graph (wgpu).
//!
//! Phase 0 delivers the [`Canvas`] renderer: it uploads a single decoded image
//! as a GPU texture and draws it as a fullscreen quad with a pannable/zoomable
//! [`Camera`]. This is the "engine proof" milestone — smooth 4K pan/zoom.
//!
//! The compositing graph (tiles, per-layer blend passes, display transform)
//! grows on top of this in Phases 1–3; the [`GpuContext`] and shader plumbing
//! here are shared by both.
//!
//! # Color and orientation conventions
//!
//! * Shading happens in LINEAR light. What lands in the framebuffer is always
//!   sRGB-encoded: for an `*-Srgb` target the hardware encodes and this crate
//!   writes linear values (including clear values); for a plain 8-bit unorm
//!   target [`Canvas`] applies the encode in the shader instead. Non-8-bit
//!   targets are rejected — see [`Canvas::supports_target`].
//! * Clip-space `y = +1` is the top of the target and maps to `v = 0`, the
//!   first texel row of a source texture. See `render_shaders`.
//! * Source textures carry STRAIGHT (non-premultiplied) alpha at every mip
//!   level; [`CompositePass`] is the one premultiplied stage.
//!
//! # Rendering without a window
//!
//! [`OffscreenTarget`] renders into a texture and copies the result back to CPU
//! RGBA8 via [`OffscreenTarget::read_rgba8`]. Combined with
//! [`GpuContext::headless`] that gives the crate a windowless path for
//! golden-image tests, thumbnails and export.

pub mod camera;
pub mod canvas;
pub mod composite;
pub mod context;
pub mod offscreen;
pub mod texture;

pub use camera::{Camera, MAX_ZOOM, MIN_ZOOM};
pub use canvas::{backdrop_clear_color, srgb_to_linear, Canvas, DEFAULT_BACKDROP_SRGB};
pub use composite::{CompositeParams, CompositePass};
pub use context::GpuContext;
pub use offscreen::{read_texture_rgba8, OffscreenTarget, Readback};
pub use texture::{GpuTexture, MipGenerator};
