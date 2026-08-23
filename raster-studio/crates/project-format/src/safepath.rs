//! Turning untrusted names from a package into paths, or refusing to.
//!
//! # The bug this module exists to make impossible
//!
//! ```text
//! std::fs::read(package_dir.join(&manifest.document_path))
//! ```
//!
//! `Path::join` is not a "stay inside this directory" operation. Handed an
//! absolute path it **discards the base entirely**, so a manifest saying
//! `"/etc/shadow"` (or `"C:\\Users\\me\\.ssh\\id_ed25519"`, or
//! `"\\\\attacker\\share\\x"`) reads that file instead of the document. Handed
//! `"../../../etc/shadow"` it walks out of the package. Both are one line of
//! JSON in a file anyone can mail you.
//!
//! So: nothing from a package is ever joined onto a base without going through
//! [`check`] first, and the loader does not depend on the manifest for the
//! document's location at all — it reads a fixed filename and only uses the
//! manifest field to *reject* a package that disagrees.
//!
//! # What counts as unsafe
//!
//! * empty, or containing a NUL
//! * absolute, rooted (`/x`), or carrying a Windows prefix (`C:x`, `\\?\`,
//!   `\\server\share`)
//! * any `..` or `.` component
//! * any `\` anywhere. On Windows it is a separator; on Linux it is an ordinary
//!   filename character. A name that means two different things on two
//!   platforms is not a name we accept.
//! * a component that is not plain (verified twice: once by splitting on `/`
//!   ourselves, once through [`std::path::Path::components`], because the two
//!   disagree across platforms and we want the intersection).
//!
//! Symlinks get their own check ([`reject_symlink`]) because a name can be
//! perfectly well-formed and still be a door out of the directory — and the
//! door can be *any* component, not just the last one. `tiles/ab` being a link
//! to `/etc` makes `tiles/ab/<64 hex>.tile` an open outside the package while
//! every component of the name is a plain word, so [`safe_join`] walks the
//! whole path and refuses a link at any depth.

use std::path::{Component, Path, PathBuf};

use crate::error::ProjectError;

/// Reject anything that is not a plain relative path made of ordinary
/// components.
pub(crate) fn check(field: &'static str, value: &str) -> Result<(), ProjectError> {
    let bad = || ProjectError::UnsafePath {
        field,
        value: value.to_string(),
    };

    if value.is_empty() || value.contains('\0') || value.contains('\\') {
        return Err(bad());
    }
    // A Windows drive or UNC prefix, spelled with forward slashes so it would
    // survive the backslash check above: "C:/x", "//server/share".
    let b = value.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return Err(bad());
    }
    if value.starts_with('/') {
        return Err(bad());
    }
    for segment in value.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(bad());
        }
    }

    // Second opinion from the platform's own parser. On Windows this catches
    // prefixes and separators the manual scan above does not know about; on
    // Unix it is a cheap restatement. Anything other than a plain `Normal`
    // component is refused.
    let path = Path::new(value);
    if path.is_absolute() || path.has_root() {
        return Err(bad());
    }
    for c in path.components() {
        match c {
            Component::Normal(s) if !s.is_empty() => {}
            _ => return Err(bad()),
        }
    }
    Ok(())
}

/// `base.join(rel)` for a `rel` that has passed [`check`], with a final
/// containment assertion so a future change to `check` cannot silently reopen
/// the hole, and a symlink check on **every** component.
///
/// The per-component walk is what makes the containment lexical *and* real:
/// `check` proves the name stays inside the package, and the walk proves no
/// directory on the way down redirects out of it. A component that does not
/// exist yet is fine — that is the save side building the package — so
/// [`reject_symlink`] treats `NotFound` as "not a link".
pub(crate) fn safe_join(
    base: &Path,
    rel: &str,
    field: &'static str,
) -> Result<PathBuf, ProjectError> {
    check(field, rel)?;
    let joined = base.join(rel);
    if !joined.starts_with(base) {
        return Err(ProjectError::UnsafePath {
            field,
            value: rel.to_string(),
        });
    }
    let mut walked = base.to_path_buf();
    let mut label = String::new();
    for segment in rel.split('/') {
        walked.push(segment);
        if !label.is_empty() {
            label.push('/');
        }
        label.push_str(segment);
        reject_symlink(&walked, &label)?;
    }
    Ok(joined)
}

/// Refuse a symbolic link. A well-formed name can still point outside the
/// package, and a package has no legitimate use for one.
pub(crate) fn reject_symlink(path: &Path, label: &str) -> Result<(), ProjectError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(ProjectError::Symlink {
            path: label.to_string(),
        }),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Read a file from a package with a hard ceiling on how much is allocated.
///
/// The ceiling is applied to the *metadata* first, so an absurd length never
/// becomes an allocation, and re-checked after the read so a file that grew
/// between the two calls is still refused.
pub(crate) fn read_capped(path: &Path, label: &str, max: u64) -> Result<Vec<u8>, ProjectError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ProjectError::MissingFile {
                path: label.to_string(),
            })
        }
        Err(e) => return Err(e.into()),
    };
    if meta.file_type().is_symlink() {
        return Err(ProjectError::Symlink {
            path: label.to_string(),
        });
    }
    if !meta.is_file() {
        return Err(ProjectError::NotAFile {
            path: label.to_string(),
        });
    }
    let too_large = |size: u64| ProjectError::FileTooLarge {
        path: label.to_string(),
        size,
        max,
    };
    if meta.len() > max {
        return Err(too_large(meta.len()));
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() as u64 > max {
        return Err(too_large(bytes.len() as u64));
    }
    Ok(bytes)
}

/// Create a directory symlink, or say why not.
///
/// Unix always can. Windows can only with the `SeCreateSymbolicLink` privilege
/// (Developer Mode, or an elevated shell), so a test staging a hostile package
/// has to be able to tell "there is no bug" from "this machine cannot stage the
/// bug".
#[cfg(test)]
pub(crate) fn try_symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        Err(std::io::Error::other("no symlinks on this platform"))
    }
}

/// Create a *file* symlink, or say why not. Same privilege caveat as
/// [`try_symlink_dir`]; Windows keeps the two kinds apart.
#[cfg(test)]
pub(crate) fn try_symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        Err(std::io::Error::other("no symlinks on this platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_join_that_started_all_this_is_refused() {
        // Each of these, joined onto a package directory, reads something that
        // is not in the package.
        let hostile = [
            "/etc/shadow",
            "/etc/passwd",
            "../../../etc/passwd",
            "..",
            "../document.msgpack",
            "a/../../b",
            "C:/Windows/System32/config/SAM",
            "C:\\Windows\\win.ini",
            "\\\\attacker\\share\\payload",
            "//attacker/share/payload",
            "\\\\?\\C:\\x",
            "sub\\dir\\doc.msgpack",
            "",
            ".",
            "./document.msgpack",
            "a//b",
            "doc\0.msgpack",
        ];
        for v in hostile {
            assert!(
                check("document_path", v).is_err(),
                "accepted hostile path {v:?}"
            );
        }
    }

    #[test]
    fn ordinary_relative_names_are_accepted() {
        for v in [
            "document.msgpack",
            "previews/preview.png",
            "tiles/ab/abcdef.tile",
            "assets/index.json",
        ] {
            check("document_path", v).unwrap_or_else(|e| panic!("rejected {v:?}: {e}"));
        }
    }

    #[test]
    fn an_absolute_join_would_have_replaced_the_base() {
        // The property that makes `join` the wrong tool, pinned so nobody has
        // to take it on faith.
        let base = Path::new("/packages/p.rstudio");
        let escaped = base.join(Path::new("/etc/passwd"));
        assert!(
            !escaped.starts_with(base),
            "join kept the base, so this test needs rewriting: {escaped:?}"
        );
        assert!(safe_join(base, "/etc/passwd", "document_path").is_err());
    }

    #[test]
    fn safe_join_stays_under_the_base() {
        let base = Path::new("pkg");
        let p = safe_join(base, "previews/preview.png", "f").unwrap();
        assert!(p.starts_with(base));
        assert!(p.ends_with("preview.png"));
    }

    #[test]
    fn safe_join_accepts_components_that_do_not_exist_yet() {
        // The save side joins before it creates. A missing component is not a
        // link.
        let dir = tempfile::tempdir().unwrap();
        let p = safe_join(dir.path(), "tiles/ab/cd.tile", "tile").unwrap();
        assert!(p.ends_with("cd.tile"));
    }

    #[test]
    fn a_symlinked_directory_component_is_refused_even_though_the_name_is_plain() {
        // `tiles/ab` is a perfectly ordinary name. If it is a link, reading
        // `tiles/ab/<hex>.tile` is an open outside the package, and only a
        // per-component check sees it: the leaf's own metadata says "regular
        // file".
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("pkg");
        std::fs::create_dir_all(pkg.join("tiles")).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("cd.tile"), b"not ours").unwrap();
        if let Err(e) = try_symlink_dir(&outside, &pkg.join("tiles").join("ab")) {
            if cfg!(unix) {
                panic!("could not stage the symlink: {e}");
            }
            eprintln!("skipped: this machine cannot create a directory symlink ({e})");
            return;
        }

        // The leaf really does resolve to a readable regular file...
        assert!(pkg.join("tiles/ab/cd.tile").is_file());
        // ...and is refused anyway.
        let err = safe_join(&pkg, "tiles/ab/cd.tile", "tile").unwrap_err();
        assert!(
            matches!(err, ProjectError::Symlink { ref path } if path == "tiles/ab"),
            "{err}"
        );
    }

    #[test]
    fn read_capped_refuses_an_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("big");
        std::fs::write(&f, vec![0u8; 4096]).unwrap();
        let err = read_capped(&f, "big", 1024).unwrap_err();
        assert!(matches!(err, ProjectError::FileTooLarge { size: 4096, .. }));
        assert_eq!(read_capped(&f, "big", 8192).unwrap().len(), 4096);
    }

    #[test]
    fn read_capped_reports_a_missing_file_as_missing_not_as_io() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_capped(&dir.path().join("nope"), "nope", 16).unwrap_err();
        assert!(matches!(err, ProjectError::MissingFile { .. }), "{err}");
    }

    #[test]
    fn read_capped_refuses_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_capped(dir.path(), "dir", 16).unwrap_err();
        assert!(matches!(err, ProjectError::NotAFile { .. }), "{err}");
    }
}
