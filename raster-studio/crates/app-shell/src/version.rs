//! The build the user is running, as Help ▸ About and the startup log report it.
//!
//! The shell is a library: its own `CARGO_PKG_VERSION` is the version of
//! `app-shell`, not of the product, and it has no build script that could ask
//! git anything. The executable does — `apps/studio-desktop/build.rs` stamps
//! the package version and the short commit hash — so the executable hands
//! that stamp in once, before the window opens, and the About arm reads it
//! from here. Until it does, [`version()`] falls back to the shell's own
//! version with no commit, so a test binary or an embedding that never calls
//! [`set_version_stamp`] still gets a truthful (if less precise) line.

use std::sync::OnceLock;

/// What a build knows about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    /// The Cargo package version of the executable, e.g. `0.1.0`.
    pub semver: &'static str,
    /// The short git commit the executable was built from, when the build
    /// machine had git and a checkout; `None` for a tarball or offline build.
    pub git: Option<&'static str>,
}

static STAMP: OnceLock<Version> = OnceLock::new();

/// Record the executable's version once. A second call is ignored and returns
/// the stamp already in place — the first caller (the desktop `main`) wins.
pub fn set_version_stamp(version: Version) -> Version {
    let _ = STAMP.set(version);
    *STAMP.get().expect("the stamp was just set")
}

/// The recorded stamp, or the shell's own package version with no commit when
/// nothing was recorded.
pub fn version() -> Version {
    resolve(STAMP.get().copied())
}

/// [`version`] as a pure function of the stamp slot: the recorded stamp when
/// there is one, otherwise the shell's own package version with no commit.
/// Split out so the unstamped branch can be tested in a binary whose slot
/// another test may already have filled.
fn resolve(stamp: Option<Version>) -> Version {
    stamp.unwrap_or(Version {
        semver: env!("CARGO_PKG_VERSION"),
        git: None,
    })
}

/// The product name and version as one line: `Raster Studio 0.1.0 (53dd398)`,
/// or `Raster Studio 0.1.0` when no commit is known.
pub fn about_line() -> String {
    about_line_for(version())
}

/// [`about_line`] for an explicit stamp, so the format can be pinned without
/// touching the process-wide `OnceLock`.
pub fn about_line_for(version: Version) -> String {
    match version.git {
        Some(git) if !git.is_empty() => format!("Raster Studio {} ({git})", version.semver),
        _ => format!("Raster Studio {}", version.semver),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_about_line_carries_the_semver_and_the_short_commit() {
        assert_eq!(
            about_line_for(Version {
                semver: "0.1.0",
                git: Some("53dd398"),
            }),
            "Raster Studio 0.1.0 (53dd398)"
        );
    }

    #[test]
    fn without_a_commit_the_about_line_has_no_empty_parentheses() {
        assert_eq!(
            about_line_for(Version {
                semver: "0.1.0",
                git: None,
            }),
            "Raster Studio 0.1.0"
        );
        assert_eq!(
            about_line_for(Version {
                semver: "0.1.0",
                git: Some(""),
            }),
            "Raster Studio 0.1.0"
        );
    }

    #[test]
    fn the_unstamped_fallback_is_the_shell_version_with_no_commit() {
        // The process-wide slot may already be filled by another test in this
        // binary, so the unstamped branch is driven through `resolve`, the
        // function `version()` is: an empty slot yields the shell's own
        // package version and no commit.
        let fallback = resolve(None);
        assert_eq!(fallback.semver, env!("CARGO_PKG_VERSION"));
        assert_eq!(fallback.git, None);
        assert_eq!(
            about_line_for(fallback),
            format!("Raster Studio {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn a_recorded_stamp_wins_over_the_fallback() {
        let stamped = Version {
            semver: "9.8.7",
            git: Some("abcdef0"),
        };
        assert_eq!(resolve(Some(stamped)), stamped);
    }

    #[test]
    fn version_reports_whatever_the_first_stamp_was() {
        // First caller wins; whichever test stamped first, `version()` must
        // agree with what `set_version_stamp` reports as the stamp in place.
        let in_place = set_version_stamp(Version {
            semver: "1.2.3",
            git: Some("0123abc"),
        });
        assert_eq!(version(), in_place);
        assert_eq!(about_line(), about_line_for(in_place));
    }
}
