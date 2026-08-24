//! Raster Studio desktop entry point.
//!
//! Initialize diagnostics, collect the files named on the command line, and
//! hand them to the shell, which restores the previous session's window,
//! preferences and keymap, offers to recover anything a crash left behind, and
//! opens each file as a document.
//!
//! ```text
//! studio-desktop [FILE ...]
//! ```
//!
//! `FILE` is an image (`png`, `jpg`, `webp`, `tif`, `gif`, `bmp`, …) or a
//! `.rstudio` project package. With no arguments the editor starts with no
//! document open — File ▸ New, File ▸ Open, or a drag-and-drop fills it.
//!
//! # Exit code
//!
//! A start-up failure is reported to the user in a native dialog by the shell
//! *and* returned here, so a run from a terminal or a CI script still sees a
//! non-zero exit rather than a silent one.

use std::path::PathBuf;

use anyhow::Result;

fn main() -> Result<()> {
    telemetry::init_tracing();
    tracing::info!("Raster Studio {}", env!("CARGO_PKG_VERSION"));

    let files = collect_files(std::env::args_os().skip(1));
    if files.is_empty() {
        tracing::info!("no files given; starting with an empty workspace");
    } else {
        tracing::info!("opening {} file(s) from the command line", files.len());
    }

    app_shell::launch(files)?;
    Ok(())
}

/// The full product name and the build version stamp.
///
/// The stamp is [`env!("RASTER_VERSION_STAMP")`] set by `build.rs` — the plain
/// package version, or `version+git<short-hash>` when git was available at
/// build time — so a user or a bug report can say exactly which build they are
/// on. The `+git` suffix must not leak into a filename or a path; it is a
/// rendezvous between the About text and the log only.
pub fn about() -> String {
    format!("Raster Studio {}", env!("RASTER_VERSION_STAMP"))
}

/// Turn the command line into a list of paths to open.
///
/// Arguments that are not paths of an existing file or directory are dropped
/// with a warning rather than opened: a mistyped flag would otherwise reach the
/// decoder and come back as "not a recognised image format", which names the
/// wrong problem.
fn collect_files(args: impl Iterator<Item = std::ffi::OsString>) -> Vec<PathBuf> {
    args.map(PathBuf::from)
        .filter(|p| {
            if p.exists() {
                true
            } else {
                tracing::warn!("ignoring `{}`: no such file", p.display());
                false
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn the_about_line_names_the_product_and_the_version() {
        let line = about();
        assert!(line.starts_with("Raster Studio "), "{line}");
        // A stamp is either the bare version or version+git<hash> — never empty.
        let rest = line.trim_start_matches("Raster Studio ");
        assert!(!rest.is_empty(), "{line}");
        assert!(rest.starts_with("0.1.0"), "unexpected version: {line}");
        // The +git suffix, when present, is `git` + 7 hex chars.
        if let Some(suffix) = rest.strip_prefix("0.1.0+") {
            assert!(suffix.starts_with("git"), "{line}");
            let hash = suffix.trim_start_matches("git");
            assert!(
                !hash.is_empty() && hash.chars().all(|c| c.is_ascii_hexdigit()),
                "{line}"
            );
        }
    }

    #[test]
    fn only_paths_that_exist_are_opened() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("photo.png");
        std::fs::write(&real, b"not really a png, but it exists").unwrap();

        let args = [
            OsString::from(&real),
            OsString::from("--not-a-flag-we-have"),
            OsString::from(dir.path().join("missing.png")),
            OsString::from(dir.path()),
        ];
        let files = collect_files(args.into_iter());
        assert_eq!(
            files,
            vec![real, dir.path().to_path_buf()],
            "a directory is a valid target (a .rstudio package is one)"
        );
    }

    #[test]
    fn no_arguments_means_no_files() {
        assert!(collect_files(std::iter::empty()).is_empty());
    }
}
