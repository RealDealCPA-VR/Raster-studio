//! Crash-safe package read/write.
//!
//! See the crate docs for the layout and [`crate::atomic`] for the swap. This
//! module is the orchestration: what gets written, in what order, and what is
//! checked on the way back in.
//!
//! # Order of checks on load, and why it is this order
//!
//! 1. **Complete an interrupted save** ([`crate::atomic::recover`]). Before
//!    anything else, because "the package is missing" is a state a crash
//!    produces and a state this reader can fix.
//! 2. **Parse the manifest** under a byte cap.
//! 3. **Refuse an unsafe `document_path`.** *Before* integrity, because a
//!    hostile package can produce a manifest whose digest verifies — the digest
//!    detects damage, not malice — and because this check is the one that stops
//!    the reader touching a file outside the package.
//! 4. **Verify integrity**: the manifest's seal, then each listed file.
//! 5. **Gate the document format version** before decoding the document.
//! 6. **Load pixels and assets**, each blob verified against the hash that
//!    names it.

use std::collections::BTreeMap;
use std::path::Path;

use asset_store::{AssetRecord, AssetStore};
use editor_core::Document;

use crate::assets::{self, AssetInput, AssetReport, ASSETS_INDEX};
use crate::atomic;
use crate::journal::{CommandJournal, DocumentDigest, MAX_JOURNAL_BYTES};
use crate::manifest::{FileDigest, Manifest, MANIFEST_VERSION, MIN_SUPPORTED_MANIFEST_VERSION};
use crate::migrate;
use crate::preview::{self, Preview, DEFAULT_PREVIEW_MAX_EDGE, PREVIEWS_DIR, PREVIEW_FILE};
use crate::safepath;
use crate::tiles::{self, NoTiles, TileBytes, TileReport, TILES_DIR};

pub use crate::error::ProjectError;

/// Fixed name of the manifest inside a package.
pub const MANIFEST_FILE: &str = "manifest.json";
/// Fixed name of the serialized document inside a package.
///
/// **Fixed on purpose.** The loader reads this name, not
/// [`Manifest::document_path`]; the manifest field exists so a package that
/// disagrees can be refused, never so a package can choose what gets read.
pub const DOCUMENT_FILE: &str = "document.msgpack";
/// Fixed name of the in-package command journal.
pub const JOURNAL_FILE: &str = "commands.journal";
/// Directory reserved for generation metadata.
pub const AI_DIR: &str = "ai";

/// Recorded as [`Manifest::app_version`] when the caller does not supply one.
pub const UNKNOWN_APP_VERSION: &str = "unknown";

/// Largest manifest this format writes or parses.
pub const MAX_MANIFEST_BYTES: u64 = 64 << 20;
/// Largest serialized document this format writes or loads.
pub const MAX_DOCUMENT_BYTES: u64 = 1 << 30;
/// Largest preview image this format writes or loads.
pub const MAX_PREVIEW_BYTES: u64 = 64 << 20;

/// The size bounds on the package files that are **not** content-addressed —
/// applied in both directions.
///
/// `document.msgpack`, `manifest.json` and `previews/preview.png` each have a
/// cap the loader refuses to read past, and [`build_package`] refuses to write
/// past exactly the same number, out of this one struct. That is the rule
/// [`crate::tiles::write_tiles`] has always stated and the rule the asset path
/// was fixed to keep: *a package with more in it than this reader will load is a
/// package this writer must not produce.* None of these three is as reachable as
/// the asset bounds were — but `app_version` and `preview_max_edge` are
/// caller-supplied and unbounded, so "unreachable" is a property of today's
/// callers rather than of this format, and a bound that only one side applies is
/// a bound that eventually writes a project nobody can open.
///
/// `assets/index.json` obeys the same rule from [`crate::assets::caps`], where
/// the rest of the asset bounds live.
///
/// `commands.journal` is deliberately absent, and it is the only absence. It is
/// the one file the *application* grows on its own, between saves, so no save
/// could bound it anyway — and it does not need one: [`open_project`] never
/// reads it, so no size it reaches can cost the user the project. A save that
/// meets an over-size journal starts a fresh one rather than failing
/// ([`carry_journal_forward`]); what that costs is the crash-recovery suffix,
/// which [`CommandJournal::read`] reports as
/// [`ProjectError::FileTooLarge`] rather than replaying half of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileCaps {
    /// [`MAX_DOCUMENT_BYTES`].
    pub document: u64,
    /// [`MAX_MANIFEST_BYTES`].
    pub manifest: u64,
    /// [`MAX_PREVIEW_BYTES`].
    pub preview: u64,
}

impl Default for FileCaps {
    /// The constants — what every non-test call runs at.
    fn default() -> Self {
        Self {
            document: MAX_DOCUMENT_BYTES,
            manifest: MAX_MANIFEST_BYTES,
            preview: MAX_PREVIEW_BYTES,
        }
    }
}

// Test seam, in the shape of `assets::CAP_OVERRIDE`: one knob per bound, moving
// the writing side and the reading side of that bound together, so no test can
// pass while the two sides disagree.
#[cfg(test)]
thread_local! {
    static FILE_CAP_OVERRIDE: std::cell::Cell<Option<FileCaps>> =
        const { std::cell::Cell::new(None) };
}

/// The file-size caps in force.
///
/// [`FileCaps::default`], except under the test seam above.
pub(crate) fn file_caps() -> FileCaps {
    #[cfg(test)]
    {
        if let Some(overridden) = FILE_CAP_OVERRIDE.with(|c| c.get()) {
            return overridden;
        }
    }
    FileCaps::default()
}

/// Run `f` with the file caps replaced, restoring them afterwards even on a
/// panic, so a test can reach a bound without writing a gigabyte.
#[cfg(test)]
pub(crate) fn with_file_caps<T>(caps: FileCaps, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<FileCaps>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FILE_CAP_OVERRIDE.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(FILE_CAP_OVERRIDE.with(|c| c.replace(Some(caps))));
    f()
}

/// [`atomic::write_and_sync`], refusing a size this crate's own loader will not
/// read back.
///
/// The check is before the write, not after: the point is that these bytes never
/// reach a package. See [`FileCaps`].
fn write_reopenable(path: &Path, name: &str, bytes: &[u8], max: u64) -> Result<(), ProjectError> {
    if bytes.len() as u64 > max {
        return Err(ProjectError::PackageFileTooLarge {
            path: name.to_string(),
            size: bytes.len() as u64,
            max,
        });
    }
    atomic::write_and_sync(path, bytes)
}

/// Knobs for a save.
#[derive(Debug, Clone)]
pub struct SaveOptions {
    /// Version string of the **application**, recorded in the manifest.
    ///
    /// This is a parameter and not `env!("CARGO_PKG_VERSION")` because this
    /// crate's version is the version of a serialization library, not of the
    /// program the user is running — every build would have reported `0.1.0`.
    pub app_version: String,
    /// Write a composite preview into `previews/`.
    pub write_preview: bool,
    /// Longest edge of that preview, in pixels.
    pub preview_max_edge: u32,
    /// Read every [`AssetInput::Linked`] asset and embed its bytes, making the
    /// package portable.
    pub collect_assets: bool,
    /// Assets the document refers to.
    pub assets: Vec<AssetInput>,
}

impl SaveOptions {
    /// Options recording `app_version`, with a preview and no assets.
    pub fn new(app_version: impl Into<String>) -> Self {
        Self {
            app_version: app_version.into(),
            write_preview: true,
            preview_max_edge: DEFAULT_PREVIEW_MAX_EDGE,
            collect_assets: false,
            assets: Vec::new(),
        }
    }

    /// Embed every linked asset ("collect assets for a portable project").
    pub fn collecting_assets(mut self, assets: Vec<AssetInput>) -> Self {
        self.collect_assets = true;
        self.assets = assets;
        self
    }

    /// Record assets, leaving links as links.
    pub fn with_assets(mut self, assets: Vec<AssetInput>) -> Self {
        self.assets = assets;
        self
    }
}

impl Default for SaveOptions {
    fn default() -> Self {
        Self::new(UNKNOWN_APP_VERSION)
    }
}

/// What a save wrote.
#[derive(Debug, Clone)]
pub struct SaveReport {
    /// Digest of the serialized document, for
    /// [`CommandJournal::mark_saved`].
    pub document: DocumentDigest,
    pub document_bytes: u64,
    pub tiles: TileReport,
    pub assets: AssetReport,
    /// Preview dimensions, when one was written.
    pub preview: Option<(u32, u32)>,
}

/// A package, opened.
#[derive(Debug)]
pub struct LoadedProject {
    pub document: Document,
    /// Every tile and embedded asset blob the package holds, content-addressed.
    pub tiles: AssetStore,
    pub manifest: Manifest,
    pub assets: Vec<AssetRecord>,
    /// PNG bytes of the composite preview, when the package carries one.
    pub preview: Option<Vec<u8>>,
    /// Digest of the document as stored, for pairing with the journal.
    pub document_digest: DocumentDigest,
    /// The package was found mid-swap and the interrupted save was completed.
    pub recovered_from_interrupted_save: bool,
    /// Format version the file declared, when it was older than the current one.
    pub migrated_from: Option<u32>,
}

impl LoadedProject {
    /// A compositor-ready view of this project's tiles.
    ///
    /// [`compositor::TileSource`] hands out `&[u8]` borrowed from the source,
    /// while [`asset_store::AssetStore`] hands out `Arc<[u8]>` — it is an LRU
    /// over a disk backend, so a blob may have to be faulted in before it can
    /// be returned, and there is nothing to borrow from. The two cannot be
    /// bridged without copying, so this **copies**: it materializes every tile
    /// the document references into a [`compositor::MemoryTileSource`].
    ///
    /// Callers that only need the bytes should read them from
    /// [`LoadedProject::tiles`] directly. This exists for the one caller that
    /// wants to composite immediately after a load.
    pub fn tile_source(&self) -> Result<compositor::MemoryTileSource, ProjectError> {
        let mut out = compositor::MemoryTileSource::new();
        for key in self.document.pixels.keys() {
            let Some(map) = self.document.pixels.tiles(key) else {
                continue;
            };
            for (_, hash) in map.iter() {
                let bytes = self.tiles.get(asset_store::BlobHash(hash.0))?;
                out.insert_bytes(bytes.to_vec());
            }
        }
        Ok(out)
    }
}

/// Save `doc` to the package directory at `path`, atomically, with no pixel
/// data available.
///
/// Kept for callers that hold a document and nothing else. It **fails** rather
/// than writing a package with no pixels if the document references any tiles —
/// see [`crate::tiles::NoTiles`]. Real saves go through
/// [`save_project_with`].
pub fn save_project(path: &Path, doc: &Document) -> Result<(), ProjectError> {
    save_project_with(path, doc, &NoTiles, &SaveOptions::default())?;
    Ok(())
}

/// Save `doc` and the pixels it references to the package directory at `path`.
///
/// Strategy: build the whole package in a uniquely named sibling directory,
/// fsync it, then swap it into place with the previous package parked under a
/// recoverable name. A partially written package can never replace a good one,
/// and a crash mid-swap leaves something [`open_project`] can put back.
pub fn save_project_with(
    path: &Path,
    doc: &Document,
    tiles: &dyn TileBytes,
    opts: &SaveOptions,
) -> Result<SaveReport, ProjectError> {
    let tmp = atomic::unique_sibling(path, atomic::TEMP_PREFIX);
    let report = match build_package(&tmp, path, doc, tiles, opts) {
        Ok(r) => r,
        Err(e) => {
            // Only ever our own temp: the name was minted for this call.
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };
    atomic::swap_into_place(path, &tmp)?;
    Ok(report)
}

/// Build a complete package at `tmp`. `dest` is consulted only for the journal
/// it may already hold.
fn build_package(
    tmp: &Path,
    dest: &Path,
    doc: &Document,
    tiles: &dyn TileBytes,
    opts: &SaveOptions,
) -> Result<SaveReport, ProjectError> {
    std::fs::create_dir_all(tmp)?;
    for sub in [PREVIEWS_DIR, TILES_DIR, assets::ASSETS_DIR, AI_DIR] {
        std::fs::create_dir_all(tmp.join(sub))?;
    }

    // The bounds `open_project` will apply to this package, applied on the way
    // out so it cannot be written unopenable. See [`FileCaps`].
    let caps = file_caps();
    let mut contents: BTreeMap<String, FileDigest> = BTreeMap::new();

    // Document. MessagePack, and `to_vec_named` specifically: `Document`'s
    // `Serialize` omits empty fields, so its field *count* depends on its
    // content and a positional encoder would read the wrong field back. See
    // `editor_core::document`.
    let doc_bytes = rmp_serde::to_vec_named(doc)?;
    write_reopenable(
        &tmp.join(DOCUMENT_FILE),
        DOCUMENT_FILE,
        &doc_bytes,
        caps.document,
    )?;
    contents.insert(DOCUMENT_FILE.to_string(), FileDigest::of(&doc_bytes));

    // Pixels. Content-addressed, so not listed in `contents`.
    let tile_report = tiles::write_tiles(tmp, doc, tiles)?;

    // Assets.
    let (asset_report, index) = assets::write_assets(tmp, &opts.assets, opts.collect_assets)?;
    if let Some(index) = &index {
        contents.insert(ASSETS_INDEX.to_string(), FileDigest::of(index));
    }

    // Composite preview.
    let mut preview_size = None;
    if opts.write_preview {
        if let Some(Preview { width, height, png }) =
            preview::render(doc, tiles, opts.preview_max_edge)?
        {
            write_reopenable(&tmp.join(PREVIEW_FILE), PREVIEW_FILE, &png, caps.preview)?;
            contents.insert(PREVIEW_FILE.to_string(), FileDigest::of(&png));
            preview_size = Some((width, height));
        }
    }

    // Journal: carry the previous package's valid prefix across, then record
    // the save marker that anchors recovery to *this* snapshot.
    //
    // Deliberately not listed in `contents`: the application appends to it
    // while the package is open, so a digest of it would be stale the moment
    // the user drew something.
    let document = DocumentDigest::of(&doc_bytes);
    let journal = tmp.join(JOURNAL_FILE);
    carry_journal_forward(&dest.join(JOURNAL_FILE), &journal)?;
    CommandJournal::mark_saved(&journal, document)?;

    // Manifest last: it describes everything above.
    let mut manifest = Manifest {
        manifest_version: MANIFEST_VERSION,
        app_version: opts.app_version.clone(),
        document_path: DOCUMENT_FILE.to_string(),
        assets_collected: asset_report.collected,
        contents,
        integrity: String::new(),
    };
    manifest.seal();
    write_reopenable(
        &tmp.join(MANIFEST_FILE),
        MANIFEST_FILE,
        &serde_json::to_vec_pretty(&manifest)?,
        caps.manifest,
    )?;

    // Files were fsynced as they were written; the directories holding them
    // were not, and a file fsync says nothing about the durability of the
    // directory entry pointing at it.
    atomic::sync_tree(tmp)?;

    Ok(SaveReport {
        document,
        document_bytes: doc_bytes.len() as u64,
        tiles: tile_report,
        assets: asset_report,
        preview: preview_size,
    })
}

/// Copy the valid prefix of an existing journal into the new package.
///
/// The *valid prefix*, not the file: a torn tail copied verbatim would sit in
/// front of the save marker written next, and the reader stops at the first
/// unparseable record — so the marker would never be seen and recovery would
/// replay from the previous one.
///
/// **One read, not two.** The prefix length is computed from the buffer this
/// function already holds ([`CommandJournal::parse`]), never from a second
/// `read` of the same file. The application appends to the in-package journal
/// while the package is open, so the file can grow between two reads; a length
/// measured over the longer buffer and applied to the shorter one cuts the copy
/// mid-record, which is precisely the state this function exists to avoid.
///
/// **What that one read does not carry.** Whatever is appended to `from` after
/// it is not in the copy, and [`atomic::swap_into_place`] then deletes the
/// directory `from` lives in — so a command journalled between this read and the
/// swap does not reach the new package. That is the intended outcome rather than
/// an oversight: under the ordering rule in the [`crate::journal`] module header
/// such a record belongs to a command the snapshot already holds, and carrying
/// it past the save marker written next would replay it twice. The one
/// application shape it does not hold for is named there and in the crate-level
/// "Known limits".
fn carry_journal_forward(from: &Path, to: &Path) -> Result<(), ProjectError> {
    if !from.exists() {
        atomic::write_and_sync(to, b"")?;
        return Ok(());
    }
    let bytes = match safepath::read_capped(from, JOURNAL_FILE, MAX_JOURNAL_BYTES) {
        Ok(b) => b,
        // An oversized or non-regular journal is not a reason to refuse the
        // save; the snapshot being written is what the user asked for, and the
        // marker appended next makes the fresh journal self-consistent.
        Err(ProjectError::FileTooLarge { .. }) | Err(ProjectError::NotAFile { .. }) => {
            atomic::write_and_sync(to, b"")?;
            return Ok(());
        }
        // A *symlink* is refused by name rather than papered over: `read_capped`
        // returns `ProjectError::Symlink { path: "commands.journal" }` and it
        // travels out of here unchanged. It cannot happen by accident — the
        // loader rejects a package carrying one — so reaching this means the
        // link was planted while the package was open, and quietly starting a
        // fresh journal would hide that from the one person who can act on it.
        Err(e) => return Err(e),
    };
    #[cfg(test)]
    grow_source_journal(from)?;
    let valid = CommandJournal::parse(&bytes).valid_bytes() as usize;
    debug_assert!(valid <= bytes.len(), "the prefix came from another read");
    atomic::write_and_sync(to, &bytes[..valid])?;
    Ok(())
}

// Test seam: append to the *source* journal after this function has read it,
// which is what the running application does to an open package's journal all
// session long. The seam's whole point is that arming it must change nothing —
// the prefix is measured on the buffer read above, so a later state of the file
// is not consulted. Arm it against the old two-read code and the second read
// sees the longer file, the length no longer belongs to the buffer being
// sliced, and the copy is cut mid-record.
#[cfg(test)]
thread_local! {
    pub(crate) static GROW_JOURNAL_AFTER_READ: std::cell::RefCell<Option<Vec<u8>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn grow_source_journal(from: &Path) -> Result<(), ProjectError> {
    use std::io::Write;
    let Some(extra) = GROW_JOURNAL_AFTER_READ.with(|c| c.borrow_mut().take()) else {
        return Ok(());
    };
    let mut f = std::fs::OpenOptions::new().append(true).open(from)?;
    f.write_all(&extra)?;
    f.flush()?;
    Ok(())
}

/// Load a project package, verifying it and running document migrations.
pub fn load_project(path: &Path) -> Result<Document, ProjectError> {
    Ok(open_project(path)?.document)
}

/// Load a package with everything in it: pixels, assets, preview, manifest.
pub fn open_project(path: &Path) -> Result<LoadedProject, ProjectError> {
    // A crash between the two renames of a previous save leaves nothing at
    // `path` and the previous package under a sibling name. Put it back before
    // concluding the project does not exist.
    let recovered = atomic::recover(path)?;

    let manifest_path = path.join(MANIFEST_FILE);
    if !manifest_path.is_file() {
        return Err(ProjectError::NotAPackage(path.display().to_string()));
    }
    // Every path this loader builds is lexically contained in the package, but a
    // *directory* on the way down can still be a link out of it. Blob contents
    // are verified against their own hashes, so a redirected read cannot hand
    // back data the document did not name — but it would still be an open of a
    // file outside the package, and that is not something a package gets to ask
    // for.
    safepath::reject_symlink(&manifest_path, MANIFEST_FILE)?;
    // `commands.journal` is checked here even though this function never reads
    // it, because it is the one file the *application* writes into a package it
    // did not build. A symlinked journal that survived this check would let a
    // mailed package choose a file for `CommandJournal::append` to write into
    // and for `CommandJournal::clear` to truncate. The writers re-check too —
    // the link can be planted after the open — but a package that arrives with
    // one already in it does not get opened at all.
    safepath::reject_symlink(&path.join(JOURNAL_FILE), JOURNAL_FILE)?;
    for dir in [TILES_DIR, assets::ASSETS_DIR, PREVIEWS_DIR, AI_DIR] {
        safepath::reject_symlink(&path.join(dir), dir)?;
    }
    // The same bounds the save applied on the way out — one accessor, so the two
    // sides of each cannot drift apart. See [`FileCaps`].
    let caps = file_caps();
    let manifest_bytes = safepath::read_capped(&manifest_path, MANIFEST_FILE, caps.manifest)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;

    if !(MIN_SUPPORTED_MANIFEST_VERSION..=MANIFEST_VERSION).contains(&manifest.manifest_version) {
        return Err(ProjectError::UnsupportedVersion {
            found: manifest.manifest_version,
            min: MIN_SUPPORTED_MANIFEST_VERSION,
            max: MANIFEST_VERSION,
        });
    }

    // *** The path check, before anything reads a file the manifest named. ***
    //
    // `document_path` is never joined onto `path` — the document is read from
    // the fixed `DOCUMENT_FILE` — but a package whose manifest points somewhere
    // else is refused rather than quietly ignored, because a manifest saying
    // `/etc/shadow` is not a package with a harmless typo in it.
    safepath::check("document_path", &manifest.document_path)?;
    if manifest.document_path != DOCUMENT_FILE {
        return Err(ProjectError::UnexpectedPath {
            field: "document_path",
            expected: DOCUMENT_FILE,
            value: manifest.document_path.clone(),
        });
    }
    for name in manifest.contents.keys() {
        safepath::check("contents", name)?;
    }

    if !manifest.verify_seal() {
        return Err(ProjectError::ManifestIntegrityMismatch);
    }

    // Document.
    let doc_path = safepath::safe_join(path, DOCUMENT_FILE, "document_path")?;
    let doc_bytes = safepath::read_capped(&doc_path, DOCUMENT_FILE, caps.document)?;
    verify_listed(&manifest, DOCUMENT_FILE, &doc_bytes)?;

    // Version first, document second: a file from a newer build must produce a
    // sentence about versions, not a decode error about an unknown field.
    let found = migrate::document_version(&doc_bytes)?;
    migrate::check_document_version(found)?;
    let document: Document = rmp_serde::from_slice(&doc_bytes)?;
    let mut document = migrate::migrate(document, found)?;
    document.set_path(Some(path.to_path_buf()));

    // Pixels and assets. The store is built from this crate's own asset cap
    // rather than from `AssetStore::new()`'s default, so the limit the saver
    // enforced and the limit this store applies are one number — see
    // [`assets::MAX_ASSET_BYTES`].
    let mut store = assets::new_store();
    tiles::read_tiles(path, &document, &mut store)?;

    let index_path = safepath::safe_join(path, ASSETS_INDEX, "assets")?;
    let index_bytes = if manifest.contents.contains_key(ASSETS_INDEX) || index_path.is_file() {
        let bytes = safepath::read_capped(&index_path, ASSETS_INDEX, assets::caps().index)?;
        verify_listed(&manifest, ASSETS_INDEX, &bytes)?;
        Some(bytes)
    } else {
        None
    };
    let asset_records = assets::read_assets(path, index_bytes.as_deref(), &mut store)?;

    // Preview.
    let preview_path = safepath::safe_join(path, PREVIEW_FILE, "preview")?;
    let preview = if manifest.contents.contains_key(PREVIEW_FILE) || preview_path.is_file() {
        let bytes = safepath::read_capped(&preview_path, PREVIEW_FILE, caps.preview)?;
        verify_listed(&manifest, PREVIEW_FILE, &bytes)?;
        Some(bytes)
    } else {
        None
    };

    Ok(LoadedProject {
        document,
        tiles: store,
        assets: asset_records,
        preview,
        document_digest: DocumentDigest::of(&doc_bytes),
        recovered_from_interrupted_save: recovered,
        migrated_from: (found < migrate::MAX_DOCUMENT_VERSION).then_some(found),
        manifest,
    })
}

/// Check `bytes` against the digest the manifest lists for `name`.
///
/// A file that is *present* but *unlisted* is refused: the manifest is the
/// package's inventory, and an extra file that nothing vouches for is exactly
/// what a substituted payload looks like.
fn verify_listed(manifest: &Manifest, name: &str, bytes: &[u8]) -> Result<(), ProjectError> {
    match manifest.contents.get(name) {
        Some(d) if d.matches(bytes) => Ok(()),
        _ => Err(ProjectError::IntegrityMismatch {
            path: name.to_string(),
        }),
    }
}
