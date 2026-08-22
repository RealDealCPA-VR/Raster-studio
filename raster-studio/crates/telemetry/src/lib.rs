//! Local diagnostics. **Local-first and opt-in**: nothing leaves the machine
//! unless the user explicitly enables sending, honoring the "no required cloud"
//! principle.
//!
//! Provides tracing initialization and a structured diagnostic-bundle builder
//! (the "diagnostic export" reliability feature). Network upload is a separate,
//! explicitly-gated action not implemented in the scaffold.

use serde::{Deserialize, Serialize};

/// Initialize tracing with an env-filter (`RUST_LOG`). Safe to call once at
/// startup; ignores the error if a subscriber is already set.
pub fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,raster_studio=debug"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .try_init();
}

/// A structured diagnostic bundle assembled on demand (crash/report export).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiagnosticBundle {
    pub app_version: String,
    pub os: String,
    pub gpu_adapter: String,
    pub recent_log_lines: Vec<String>,
    /// User consented to sending this bundle off-device.
    pub upload_consented: bool,
}

impl DiagnosticBundle {
    pub fn new(app_version: impl Into<String>) -> Self {
        Self {
            app_version: app_version.into(),
            os: std::env::consts::OS.to_string(),
            ..Default::default()
        }
    }

    /// Serialize to pretty JSON for writing to a local file.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_defaults_to_no_upload() {
        let b = DiagnosticBundle::new("0.1.0");
        assert!(!b.upload_consented, "must be opt-in");
        assert!(b.to_json().contains("0.1.0"));
    }
}
