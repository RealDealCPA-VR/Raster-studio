//! Sidecar process lifecycle + local IPC client (stubbed transport).
//!
//! The real implementation spawns the pinned ComfyUI process, waits for its
//! localhost server to become ready, and submits jobs over authenticated HTTP.
//! The scaffold models the *state machine and safety controls* without a real
//! child process, so higher layers can be built and tested now.

use ai_contracts::{AiOperation, JobStatus};

use crate::token::CapabilityToken;

/// Configuration for launching the runtime.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Loopback address the sidecar binds to. **Must** be 127.0.0.1.
    pub bind_host: String,
    /// Port; 0 means "let the OS pick".
    pub port: u16,
    /// Path to the pinned ComfyUI distribution (see `runtime/comfyui`).
    pub dist_path: String,
    /// Hard cap on any output edge length in pixels (job-size limit).
    pub max_dimension: u32,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            bind_host: "127.0.0.1".to_string(),
            port: 0,
            dist_path: "runtime/comfyui/dist".to_string(),
            max_dimension: 8192,
        }
    }
}

/// Observable state of the sidecar, surfaced in the runtime status UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeStatus {
    Stopped,
    Starting,
    Ready,
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    #[error("runtime is not ready (status: {0:?})")]
    NotReady(RuntimeStatus),
    #[error("refusing to bind to non-loopback host '{0}'")]
    NonLoopbackBind(String),
    #[error("job exceeds size limit: {dim}px > {max}px")]
    JobTooLarge { dim: u32, max: u32 },
    #[error("job was cancelled")]
    Cancelled,
    #[error("runtime error: {0}")]
    Runtime(String),
}

/// A handle to the (optional) local AI runtime.
pub struct SidecarHandle {
    config: RuntimeConfig,
    token: CapabilityToken,
    status: RuntimeStatus,
}

impl SidecarHandle {
    /// Prepare a handle (does not launch yet). Validates loopback binding.
    pub fn new(config: RuntimeConfig) -> Result<Self, SidecarError> {
        if config.bind_host != "127.0.0.1" && config.bind_host != "localhost" {
            return Err(SidecarError::NonLoopbackBind(config.bind_host));
        }
        Ok(Self {
            config,
            token: CapabilityToken::generate(),
            status: RuntimeStatus::Stopped,
        })
    }

    pub fn status(&self) -> &RuntimeStatus {
        &self.status
    }

    pub fn token(&self) -> &CapabilityToken {
        &self.token
    }

    /// Launch the sidecar. In the scaffold this transitions the state machine;
    /// the real version spawns the child process and polls for readiness.
    pub async fn start(&mut self) -> Result<(), SidecarError> {
        self.status = RuntimeStatus::Starting;
        // TODO(phase-2): spawn pinned ComfyUI child bound to bind_host:port,
        // pass the capability token via env, wait for /system_stats to answer.
        self.status = RuntimeStatus::Ready;
        Ok(())
    }

    /// Terminate the sidecar. Child lifetime is always tied to the app.
    pub async fn stop(&mut self) {
        // TODO(phase-2): kill child, await exit.
        self.status = RuntimeStatus::Stopped;
    }

    /// Validate an operation against safety limits before it is ever submitted.
    pub fn preflight(&self, op: &AiOperation) -> Result<(), SidecarError> {
        if self.status != RuntimeStatus::Ready {
            return Err(SidecarError::NotReady(self.status.clone()));
        }
        if let Some(dim) = requested_dimension(op) {
            if dim > self.config.max_dimension {
                return Err(SidecarError::JobTooLarge {
                    dim,
                    max: self.config.max_dimension,
                });
            }
        }
        Ok(())
    }

    /// Submit an operation. Stubbed: real impl builds the workflow graph and
    /// streams [`JobStatus`] updates. Here we just preflight and report queued.
    pub async fn submit(&self, op: &AiOperation) -> Result<JobStatus, SidecarError> {
        self.preflight(op)?;
        // TODO(phase-2): adapter translates `op` -> ComfyUI graph, POST with
        // the capability token, stream progress, map outputs to AiResult.
        Ok(JobStatus::Queued)
    }
}

/// Largest output edge implied by an operation, for the size-limit check.
fn requested_dimension(op: &AiOperation) -> Option<u32> {
    match op {
        AiOperation::Generate(r) => Some(r.width.max(r.height)),
        AiOperation::Outpaint(r) => Some(r.extend.iter().copied().max().unwrap_or(0)),
        AiOperation::Upscale(r) => Some(r.scale.saturating_mul(1024)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_contracts::GenerateRequest;

    fn ready_handle(max: u32) -> SidecarHandle {
        let mut h = SidecarHandle::new(RuntimeConfig {
            max_dimension: max,
            ..Default::default()
        })
        .unwrap();
        h.status = RuntimeStatus::Ready;
        h
    }

    #[test]
    fn rejects_non_loopback() {
        let cfg = RuntimeConfig {
            bind_host: "0.0.0.0".into(),
            ..Default::default()
        };
        assert!(matches!(
            SidecarHandle::new(cfg),
            Err(SidecarError::NonLoopbackBind(_))
        ));
    }

    #[test]
    fn preflight_rejects_oversize() {
        let h = ready_handle(1024);
        let op = AiOperation::Generate(GenerateRequest {
            prompt: "x".into(),
            negative_prompt: String::new(),
            width: 4096,
            height: 4096,
            seed: None,
        });
        assert!(matches!(
            h.preflight(&op),
            Err(SidecarError::JobTooLarge { .. })
        ));
    }

    #[test]
    fn preflight_requires_ready() {
        let h = SidecarHandle::new(RuntimeConfig::default()).unwrap();
        let op = AiOperation::Generate(GenerateRequest {
            prompt: "x".into(),
            negative_prompt: String::new(),
            width: 64,
            height: 64,
            seed: None,
        });
        assert!(matches!(h.preflight(&op), Err(SidecarError::NotReady(_))));
    }
}
