//! GPU renderer and compositing graph (wgpu).
//!
//! Phase 0 delivers the [`Canvas`] renderer: it uploads a single decoded image
//! as a GPU texture and draws it as a fullscreen quad with a pannable/zoomable
//! [`Camera`]. This is the "engine proof" milestone — smooth 4K pan/zoom.
//!
//! The compositing graph (tiles, per-layer blend passes, display transform)
//! grows on top of this in Phases 1–3; the [`GpuContext`] and shader plumbing
//! here are shared by both.

pub mod camera;
pub mod canvas;
pub mod context;
pub mod texture;

pub use camera::Camera;
pub use canvas::Canvas;
pub use context::GpuContext;
pub use texture::GpuTexture;
