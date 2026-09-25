//! Raster Studio desktop entry point.
//!
//! Initialize diagnostics, collect the files named on the command line, and
//! hand them to the shell, which restores the previous session's window,
//! preferences and keymap, offers to recover anything a crash left behind, and
//! opens each file as a document.
//!
//! ```text
//! studio-desktop [--shot OUT.png] [FILE ...]
//! ```
//!
//! `FILE` is an image (`png`, `jpg`, `webp`, `tif`, `gif`, `bmp`, …) or a
//! `.rstudio` project package. With no arguments the editor starts with no
//! document open — File ▸ New, File ▸ Open, or a drag-and-drop fills it.
//!
//! # Windows: a GUI program that still talks to its terminal
//!
//! On Windows the executable is linked with the `windows` subsystem, so
//! double-clicking it (or launching it from the Start menu) opens the editor
//! and nothing else — no console window behind it. A GUI-subsystem process
//! starts with no console at all, which would also silence the log a
//! terminal user expects from `studio-desktop --shot out.png`. So the first
//! thing `main` does on Windows is attach to the parent's console when one
//! exists ([`console::attach_to_parent`]): launched from a terminal, tracing
//! and the panic message print there as before; double-clicked, the attach
//! finds no console and the process carries on silently. The test harness is
//! a console program (`cfg(test)`), so `cargo test` output is unaffected.
//!
//! # Exit code
//!
//! A start-up failure is reported to the user in a native dialog by the shell
//! *and* returned here, so a run from a terminal or a CI script still sees a
//! non-zero exit rather than a silent one.

#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

use std::path::PathBuf;

use anyhow::Result;

fn main() -> Result<()> {
    // W15-A: `studio-desktop --decode-worker avif|heic` (hidden) is the
    // decode worker the editor starts for each AVIF / HEIC it opens: it
    // decodes the file on stdin to stdout and exits, before any console,
    // logging, crash hook or window exists, so a decoder panic ends only it.
    if let Some(code) = app_shell::dialogs::decode_worker::run_if_worker() {
        std::process::exit(code);
    }
    // W15-A: every AVIF / HEIC decode in the editor goes to such a worker.
    app_shell::dialogs::decode_worker::install();

    #[cfg(windows)]
    console::attach_to_parent();

    // The shell's Help ▸ About and the startup line report *this* build's
    // stamp: the package version plus the short commit `build.rs` recorded.
    app_shell::set_version_stamp(build_version());
    telemetry::init_tracing();
    tracing::info!("{}", app_shell::about_line());

    // A panic writes a crash bundle into the scratch dir (next to whatever
    // the periodic autosave already saved) before the process dies, so the
    // next launch's recovery scan finds both the work and the why.
    telemetry::install_panic_hook(
        app_shell::AppPaths::discover().default_scratch_dir(),
        env!("RASTER_VERSION_STAMP"),
    );

    // `--shot <path>` captures a literal GUI screenshot of the first frame to a
    // PNG and then exits (S2.3); everything else is a file to open.
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let (shot, path_args) = take_shot_flag(args);
    if let Some(p) = &shot {
        tracing::info!("will capture a screenshot to {}", p.display());
    }

    let files = collect_files(path_args.into_iter());
    if files.is_empty() {
        tracing::info!("no files given; starting with an empty workspace");
    } else {
        tracing::info!("opening {} file(s) from the command line", files.len());
    }

    app_shell::launch(files, shot)?;
    Ok(())
}

/// What `build.rs` recorded about this build: the package version, and the
/// short git commit when the build machine had one (`None` otherwise — a
/// tarball or offline build). This is the one place the two compile-time
/// stamps are read; everything downstream goes through `app_shell::version`.
fn build_version() -> app_shell::Version {
    let git = env!("RASTER_GIT_COMMIT");
    app_shell::Version {
        semver: env!("CARGO_PKG_VERSION"),
        git: (!git.is_empty()).then_some(git),
    }
}

/// Split a leading `--shot <path>` pair off the argument list, if present.
fn take_shot_flag(mut args: Vec<std::ffi::OsString>) -> (Option<PathBuf>, Vec<std::ffi::OsString>) {
    if let Some(first) = args.first() {
        if first == "--shot" {
            let value = args.get(1).cloned();
            args.drain(..2.min(args.len()));
            let path = value.map(PathBuf::from);
            return (path, args);
        }
    }
    (None, args)
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

/// Re-attach a GUI-subsystem process to the console it was launched from.
#[cfg(windows)]
mod console {
    use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
        STD_OUTPUT_HANDLE,
    };

    /// Attach to the parent process's console, if it has one, and point the
    /// standard output and error handles at it when they are not already
    /// set — a parent that redirected them (`cargo run`, `2> log.txt`, a CI
    /// step) passed valid handles in, and those are left alone.
    ///
    /// Without a console parent (double-click, Start menu) this is a no-op:
    /// `AttachConsole` fails and the process keeps running with no console,
    /// which is the whole point of the `windows` subsystem. Rust's `stdout`
    /// and `stderr` treat a missing handle as a sink, so logging without a
    /// console neither panics nor blocks.
    pub fn attach_to_parent() {
        // SAFETY: plain Win32 calls with valid arguments. `AttachConsole` and
        // `SetStdHandle` change process-wide console state only; this runs
        // first thing in `main`, before any other thread exists, and the
        // CONOUT$ handle is intentionally leaked for the life of the process.
        unsafe {
            if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
                return;
            }
            for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                let current = GetStdHandle(which);
                if !current.is_null() && current != INVALID_HANDLE_VALUE {
                    continue;
                }
                let name: Vec<u16> = "CONOUT$".encode_utf16().chain(std::iter::once(0)).collect();
                let handle = CreateFileW(
                    name.as_ptr(),
                    GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                );
                if handle != INVALID_HANDLE_VALUE {
                    SetStdHandle(which, handle);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// The version stamp the executable hands the shell is the package
    /// version and, when git was available at build time, a short hex hash.
    #[test]
    fn the_build_version_is_the_package_version_and_a_hex_commit() {
        let v = build_version();
        assert_eq!(v.semver, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            v.semver, "0.1.0",
            "the version the installer scripts expect"
        );
        if let Some(git) = v.git {
            assert!(git.len() >= 7, "a short hash is at least 7 chars: {git}");
            assert!(git.chars().all(|c| c.is_ascii_hexdigit()), "{git}");
            assert_eq!(
                env!("RASTER_VERSION_STAMP"),
                format!("{}+git{git}", v.semver),
                "the two stamps build.rs writes agree"
            );
        } else {
            assert_eq!(env!("RASTER_VERSION_STAMP"), v.semver);
        }
    }

    /// Help ▸ About, driven through the real menu bridge, names the
    /// *executable's* build — `Raster Studio 0.1.0 (<short git>)` — not the
    /// shell library's own package version.
    #[test]
    fn help_about_names_the_executable_version_and_commit() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = app_shell::Editor::new(
            app_shell::AppPaths::rooted(dir.path()),
            Box::new(app_shell::ScriptedDialogs::new()),
        );
        let stamped = app_shell::set_version_stamp(build_version());
        assert_eq!(stamped, build_version(), "the stamp is this build's");

        let line = app_shell::menu_bridge::perform(ui::MenuAction::About, &mut editor)
            .expect("About is informational and never refuses");
        let expected_version = match build_version().git {
            Some(git) => format!("Raster Studio {} ({git})", env!("CARGO_PKG_VERSION")),
            None => format!("Raster Studio {}", env!("CARGO_PKG_VERSION")),
        };
        assert_eq!(
            line,
            format!("{expected_version} — a layered raster editor")
        );
        assert_eq!(
            editor.status(),
            Some(line.as_str()),
            "About lands on the status line, where the user reads it"
        );
        // Quoted in the report: the exact line the menu produced.
        println!("Help ▸ About: {line}");
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

    #[test]
    fn the_shot_flag_is_split_off_with_its_value() {
        let (shot, rest) = take_shot_flag(vec![
            OsString::from("--shot"),
            OsString::from("out.png"),
            OsString::from("a.png"),
        ]);
        assert_eq!(shot, Some(PathBuf::from("out.png")));
        assert_eq!(rest, vec![OsString::from("a.png")]);

        // Without `--shot`, nothing is consumed and nothing is a shot path.
        let (none, rest2) = take_shot_flag(vec![OsString::from("a.png")]);
        assert_eq!(none, None);
        assert_eq!(rest2, vec![OsString::from("a.png")]);
    }
}
