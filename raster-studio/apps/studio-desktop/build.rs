//! Build-time version stamping for the desktop binary.
//!
//! Exposes `RASTER_VERSION_STAMP` to the crate: `0.1.0` alone, or
//! `0.1.0+git<short-hash>` when a `git` binary and a repository are available
//! at build time. The `+git…` suffix never fails a build — a release tarball,
//! an offline box or a CI checkout without git falls back to the plain version
//! — so the stamp is best-effort provenance, not a hard dependency. The About
//! menu and the startup log report the stamp so a user (or a bug report) can
//! say exactly which build they are on.

fn main() {
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    let stamp = match commit {
        Some(c) => format!("{}+git{c}", env!("CARGO_PKG_VERSION")),
        None => env!("CARGO_PKG_VERSION").to_string(),
    };
    println!("cargo:rustc-env=RASTER_VERSION_STAMP={stamp}");
    // Re-run when the working tree's HEAD moves, so the stamp tracks commits.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RASTER_VERSION_STAMP");
}
