//! The canvas view: the viewport the document is drawn into, and everything
//! drawn on top of it.
//!
//! Two responsibilities:
//!
//! - **Geometry.** Converting between screen and document space, accounting for
//!   panel insets, DPI scale, zoom and view rotation. Every overlay and every
//!   tool depends on this being exact, so it is pure, separable, and tested
//!   without a window.
//! - **Overlays.** Marching ants, transform handles, crop framing, path anchors,
//!   the text caret and the brush cursor — drawn above the composited image.
//!
//! Input is routed from here to the active tool in *document* coordinates. When
//! the pointer is over a panel, neither the tool nor the camera sees the event.
