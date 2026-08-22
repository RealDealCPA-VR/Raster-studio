//! Optional standalone AI-runtime manager.
//!
//! Separating this from `studio-desktop` reinforces the GPL boundary: the AI
//! runtime is a distinct, process-isolated component. This binary can
//! install / repair / start / stop the pinned ComfyUI sidecar and report
//! status independently of the editor. Phase-0 scaffold: it validates config,
//! starts the (stubbed) sidecar, prints status, and exits.

use anyhow::Result;

use ai_runtime::{RuntimeConfig, SidecarHandle};

#[tokio::main]
async fn main() -> Result<()> {
    telemetry::init_tracing();
    tracing::info!("runtime-manager {}", env!("CARGO_PKG_VERSION"));

    let config = RuntimeConfig::default();
    tracing::info!(
        "runtime bound to {}:{} (loopback only)",
        config.bind_host,
        config.port
    );

    let mut handle = SidecarHandle::new(config)?;
    tracing::info!("capability token generated (per-launch)");
    handle.start().await?;
    tracing::info!("runtime status: {:?}", handle.status());

    // A real manager would now serve status / accept control commands. The
    // scaffold stops cleanly to demonstrate lifecycle tie-in.
    handle.stop().await;
    tracing::info!("runtime stopped");
    Ok(())
}
