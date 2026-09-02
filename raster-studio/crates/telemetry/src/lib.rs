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

pub fn panic_bundle(
    scratch_dir: &std::path::Path,
    app_version: &str,
    message: &str,
) -> std::path::PathBuf {
    let mut bundle = DiagnosticBundle::new(app_version);
    bundle.recent_log_lines = vec![format!("panic: {message}")];
    bundle.upload_consented = false;
    let file = scratch_dir.join("crash-report.json");
    let _ = std::fs::create_dir_all(scratch_dir);
    let _ = std::fs::write(&file, bundle.to_json());
    file
}

/// Install the global panic hook: on panic, a crash bundle lands in
/// `scratch_dir` (next to whatever the periodic autosave already saved) and
/// the default hook still prints. The editor's own scratch autosave + the
/// startup recovery scan are what carry the unsaved work back — the hook
/// records the *why*, the autosaves carry the *what*.
pub fn install_panic_hook(scratch_dir: std::path::PathBuf, app_version: &'static str) {
    std::panic::set_hook(Box::new(move |info| {
        let _ = panic_bundle(&scratch_dir, app_version, &info.to_string());
        // The default hook is not called from inside a custom one, so the
        // message is printed here — the user sees the panic on stderr exactly
        // as they would have.
        eprintln!("{info}");
    }));
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_bundle_lands_in_the_scratch_dir_with_no_consent() {
        let dir = tempfile::tempdir().unwrap();
        let file = panic_bundle(dir.path(), "0.1.0", "injected test panic");
        assert!(file.exists(), "the bundle was written");
        let json = std::fs::read_to_string(&file).unwrap();
        let bundle: DiagnosticBundle = serde_json::from_str(&json).unwrap();
        assert!(!bundle.upload_consented, "a crash never consents");
        assert_eq!(
            bundle.recent_log_lines,
            vec!["panic: injected test panic".to_string()],
            "the panic message is recorded"
        );
        assert_eq!(bundle.app_version, "0.1.0");
    }

    #[test]
    fn bundle_defaults_to_no_upload() {
        let b = DiagnosticBundle::new("0.1.0");
        assert!(!b.upload_consented, "must be opt-in");
        assert!(b.to_json().contains("0.1.0"));
    }
}
