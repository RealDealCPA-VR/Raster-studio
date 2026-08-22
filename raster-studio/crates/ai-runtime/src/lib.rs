//! Sidecar lifecycle + local IPC client — the **only** component that knows
//! ComfyUI exists.
//!
//! Responsibilities:
//! - Launch a pinned ComfyUI build as a child process **on demand**.
//! - Bind it to `127.0.0.1` only, with a random **per-launch capability token**.
//! - Translate a typed [`ai_contracts::AiOperation`] into a workflow submission
//!   (the adapter), and translate results back into [`ai_contracts::AiResult`].
//! - Enforce safety controls: job/dimension limits, VRAM preflight,
//!   cancellation, capability checks. **No** arbitrary URLs, shell, or
//!   node-install in the normal product path.
//!
//! The actual HTTP/graph translation is intentionally stubbed here; the
//! contract is what the rest of the app depends on.

pub mod manifest;
pub mod sidecar;
pub mod token;

pub use manifest::{WorkflowManifest, WorkflowRegistry};
pub use sidecar::{RuntimeConfig, RuntimeStatus, SidecarError, SidecarHandle};
pub use token::CapabilityToken;
