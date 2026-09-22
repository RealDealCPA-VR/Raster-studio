//! Build-time provenance for the desktop binary, and its Windows resource.
//!
//! # Version stamp
//!
//! Exposes two compile-time environment variables to the crate:
//!
//! * `RASTER_GIT_COMMIT` — the short hash of `HEAD` when a `git` binary and a
//!   checkout are available at build time, otherwise empty.
//! * `RASTER_VERSION_STAMP` — `0.1.0` alone, or `0.1.0+git<short-hash>`.
//!
//! The `+git…` suffix never fails a build — a release tarball, an offline box
//! or a CI checkout without git falls back to the plain version — so the
//! stamp is best-effort provenance, not a hard dependency. `main` hands the
//! stamp to `app_shell::set_version_stamp` so Help ▸ About and the startup
//! log name the exact build a bug report comes from.
//!
//! The script re-runs when `HEAD` moves: it registers `.git/HEAD`, the branch
//! ref `HEAD` points at (when that ref is a loose file) and `packed-refs` as
//! `rerun-if-changed` inputs, so a new commit re-stamps the next build
//! without a `cargo clean`.
//!
//! # Windows resource
//!
//! When the *target* is Windows, the script writes a `.rc` into `OUT_DIR`
//! carrying the application icon (`assets/raster-studio.ico`, so Explorer and
//! the taskbar show it) and a `VERSIONINFO` block filled from
//! `CARGO_PKG_VERSION` (so the file's Properties ▸ Details tab and installer
//! tooling read the real version), then compiles and links it through
//! `embed-resource`. On a Windows host without `rc.exe` a debug build warns
//! and continues; a release build fails, because an installer built from it
//! would ship an executable with no icon and no version.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let commit = git_short_commit();
    watch_git_head();

    let version = env_var("CARGO_PKG_VERSION");
    let stamp = match &commit {
        Some(c) => format!("{version}+git{c}"),
        None => version.clone(),
    };
    println!("cargo:rustc-env=RASTER_VERSION_STAMP={stamp}");
    println!(
        "cargo:rustc-env=RASTER_GIT_COMMIT={}",
        commit.as_deref().unwrap_or("")
    );

    windows_resource(&version, &stamp);
}

fn env_var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("cargo sets {name} for build scripts"))
}

/// `git <args>` from the crate directory, trimmed stdout, or `None` when git
/// is missing, this is not a checkout, or the command failed.
fn git<const N: usize>(args: [&str; N]) -> Option<String> {
    Command::new("git")
        .args(args)
        .current_dir(env_var("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn git_short_commit() -> Option<String> {
    git(["rev-parse", "--short", "HEAD"])
}

/// Register the files whose change means "HEAD moved" as build inputs.
///
/// Only files that exist are registered: cargo treats a registered path that
/// is missing as always-dirty, which would re-run this script on every build.
fn watch_git_head() {
    let Some(git_dir) = git(["rev-parse", "--absolute-git-dir"]) else {
        return;
    };
    let git_dir = PathBuf::from(git_dir);
    let head = git_dir.join("HEAD");
    if !head.is_file() {
        return;
    }
    println!("cargo:rerun-if-changed={}", head.display());
    if let Ok(contents) = std::fs::read_to_string(&head) {
        if let Some(reference) = contents.trim().strip_prefix("ref: ") {
            watch_if_present(&git_dir.join(reference.trim()));
        }
    }
    // A branch ref can live in packed-refs instead of (or as well as) a loose
    // file; a `git gc` or a fetch moves it there.
    watch_if_present(&git_dir.join("packed-refs"));
}

fn watch_if_present(path: &Path) {
    if path.is_file() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// Write, compile and link the Windows resource. A no-op for other targets.
fn windows_resource(version: &str, stamp: &str) {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out_dir = PathBuf::from(env_var("OUT_DIR"));
    let manifest_dir = PathBuf::from(env_var("CARGO_MANIFEST_DIR"));

    // The icon lives at the workspace root so the installer script and the
    // Linux packaging can reach the same file.
    let icon = manifest_dir.join("../../assets/raster-studio.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let icon_copy = out_dir.join("raster-studio.ico");
    std::fs::copy(&icon, &icon_copy)
        .unwrap_or_else(|e| panic!("copy {} into OUT_DIR: {e}", icon.display()));

    let major = env_var("CARGO_PKG_VERSION_MAJOR");
    let minor = env_var("CARGO_PKG_VERSION_MINOR");
    let patch = env_var("CARGO_PKG_VERSION_PATCH");
    let release = env_var("PROFILE") == "release";
    // VS_FF_DEBUG marks a debug build in the Details tab; a release carries no flags.
    let file_flags = if release { "0x0L" } else { "0x1L" };

    // Numeric constants are written out so the script needs no <windows.h>:
    // FILEOS 0x40004 = VOS_NT_WINDOWS32, FILETYPE 0x1 = VFT_APP,
    // "040904B0" = U.S. English, Unicode; Translation 0x409, 1200 the same.
    let rc = format!(
        r#"1 ICON "raster-studio.ico"

1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3fL
FILEFLAGS {file_flags}
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Raster Studio\0"
      VALUE "FileDescription", "Raster Studio\0"
      VALUE "FileVersion", "{version}\0"
      VALUE "InternalName", "studio-desktop\0"
      VALUE "LegalCopyright", "Copyright Raster Studio. Proprietary.\0"
      VALUE "OriginalFilename", "studio-desktop.exe\0"
      VALUE "ProductName", "Raster Studio\0"
      VALUE "ProductVersion", "{stamp}\0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let rc_path = out_dir.join("studio-desktop.rc");
    std::fs::write(&rc_path, rc).expect("write the generated .rc into OUT_DIR");

    let result = embed_resource::compile(
        &rc_path,
        embed_resource::ParamsIncludeDirs([out_dir.as_os_str()]),
    );
    let outcome = if release {
        result.manifest_required()
    } else {
        result.manifest_optional()
    };
    if let Err(e) = outcome {
        panic!("the Windows resource (icon + VERSIONINFO) did not compile: {e:?}");
    }
}
