//! The `.rstudio` project package: read, write, migrate.
//!
//! A project is a **directory package**, not a monolithic file:
//!
//! ```text
//! project.rstudio/
//! ├── manifest.json          layout version, app version, integrity digests
//! ├── document.msgpack       serialized Document (fixed name — see below)
//! ├── commands.journal       accepted commands + save markers (crash recovery)
//! ├── previews/preview.png   composite thumbnail
//! ├── tiles/                 content-addressed tile blobs (the pixels)
//! ├── assets/                asset index + content-addressed asset blobs
//! └── ai/                    generation metadata
//! ```
//!
//! # Untrusted input
//!
//! A package arrives from other people. Every length, offset, count and path in
//! one is chosen by whoever wrote the file, so:
//!
//! * **No path from a package is ever joined onto a directory.** The document
//!   is read from a fixed filename; [`Manifest::document_path`] exists so a
//!   package that disagrees can be *refused*. `Path::join` with an absolute
//!   path discards the base, which turned one line of JSON into "read any file
//!   on this machine" — see [`safepath`].
//! * **Every allocation is bounded** by a named constant before the read
//!   happens, not after. That includes the aggregates, not only the per-item
//!   caps: [`tiles::MAX_TILE_DATA_BYTES`] and [`assets::MAX_ASSET_DATA_BYTES`]
//!   are the ceilings on what a package can make resident, and the tile
//!   reference set stops being collected the moment it passes
//!   [`tiles::MAX_PACKAGE_TILES`].
//! * **Every one of those bounds is applied on the way out as well.** A package
//!   this crate writes is a package this crate reads: the save refuses an asset,
//!   an asset total, a tile, a tile total, an asset index, a document, a
//!   manifest or a preview that would exceed what the load will accept, from the
//!   same accessor the load reads — `assets::caps`, `package::file_caps`, and
//!   the constants in [`tiles`] — rather than from a second number that can
//!   drift. A save can fail loudly; it cannot succeed into a file that will not
//!   reopen. It could once: an embedded asset over the store's blob limit, and
//!   an asset index over 16 MiB, each saved `Ok` and then failed every
//!   subsequent open with the user's only copy inside. The one file with no
//!   write-side bound is `commands.journal`, deliberately: the application grows
//!   it between saves rather than a save writing it whole, and nothing in
//!   [`open_project`] reads it, so no size it reaches can cost the project — a
//!   save that meets an over-size journal starts a fresh one. See [`journal`].
//! * **Every blob is verified** against the content hash that names it. Three
//!   files are not content-addressed — `document.msgpack`, `assets/index.json`
//!   and `previews/preview.png` — and each of those, whenever the package
//!   carries it, is verified against the digest [`Manifest::contents`] records
//!   for it, with a *missing* entry treated as a failure rather than as
//!   permission to skip the check. Precisely those three names, and no others:
//!   a `contents` entry naming anything else is checked for path safety, sealed
//!   with the rest of the manifest, and then **never read or verified**. The
//!   loader opens a fixed set of files and takes no instruction from the
//!   inventory about what else to look at.
//! * **`commands.journal` is verified by nothing at all**: the application
//!   appends to it while the package is open, so a digest of it would be stale
//!   the moment the user drew something. A hostile package can put whatever it
//!   likes in it — [`journal`] parses it defensively and stops at the first
//!   record it cannot read — but it may not be a *symlink*, because this is the
//!   one file the application writes back into a package it did not build.
//! * **Symlinks are refused** anywhere in a package, at *every* component of a
//!   path rather than only the last: a plain-looking `tiles/ab` that is a link
//!   is still a read outside the package. The journal is checked on open **and
//!   again by every writer**, since a link can be planted mid-session; see
//!   [`journal`] for the residual open-after-check race that `std` cannot close
//!   portably.
//!
//! # Durability
//!
//! Save builds the whole package in a uniquely named sibling directory, fsyncs
//! its files *and its directories*, then swaps it into place leaving the
//! previous package under a recoverable name. [`open_project`] completes an
//! interrupted swap before doing anything else.
//!
//! What that costs, precisely: **the previous package always survives, the
//! interrupted save does not.** [`atomic::recover`] renames the backup back
//! into place and never adopts the `.new-` temp, even though the temp is a
//! complete, fsynced package sealed by a manifest written last. So a crash
//! between the two renames returns the user to their last successful save, and
//! the work in the interrupted one is discarded rather than silently
//! half-applied. What can never happen is the state the old code left: no
//! project at the path the user saved to. See [`atomic`].
//!
//! # Recovery model
//!
//! Snapshot **plus the journal suffix recorded after the save marker**. The
//! journal is not a rebuild-from-zero log: replaying all of it onto a loaded
//! document duplicates everything the snapshot already holds. See [`journal`].
//!
//! # Known limits
//!
//! Stated rather than implied:
//!
//! * **Loading is eager.** Every tile blob a document references is read into
//!   memory ([`LoadedProject::tiles`]). A disk-backed, lazily faulted tile store
//!   is future work; the caps in [`tiles`] are what stand in for it today.
//! * **Integrity is not authenticity.** The digests detect corruption and
//!   tampering after the fact. There is no signature, so anyone who can rewrite
//!   a file can recompute the digest — which is why path handling never depends
//!   on a package having verified.
//! * **Directory fsync is a no-op off Unix.** `std` cannot open a directory
//!   handle on Windows; see [`atomic`]. Interrupted-save recovery is what covers
//!   the gap, and it runs on every platform.
//! * **Package layout version 1 is refused, not migrated.** It had no integrity
//!   data and stored no pixels; no such package ever shipped.
//! * **An interrupted save leaves a `.new-` sibling that nothing reclaims.** A
//!   process that dies between the two renames cleans nothing up, and
//!   [`atomic::recover`] deliberately never touches a `.new-` directory — one
//!   may belong to a save still running in another process. So the disk keeps a
//!   full-size copy of the interrupted save next to the project until someone
//!   deletes it by hand. Disk usage, not data loss; an age-gated sweep would
//!   have to guess how long a legitimate save may take.
//! * **A command journalled while a save is running does not reach the new
//!   package.** The save copies the valid prefix of the journal it read into the
//!   package it is building, and the swap then deletes the directory the old
//!   journal is in, so a record appended between that read and the swap is
//!   dropped. Under this crate's ordering rule — a record is appended *after*
//!   its command is accepted, and [`save_project_with`] holds `&Document` for
//!   the whole save, so nothing can be applied during one — that record names a
//!   command the snapshot already contains, and dropping it is correct: carrying
//!   it past the save marker would replay it onto a document that already has
//!   it. An application that journals a command *before* applying it, or that
//!   saves one document while another thread mutates a copy, loses that record
//!   instead. That ordering is a contract this crate states and cannot enforce.
//!   See [`journal`].
//! * **A symlink check is not a no-follow open.** The journal writers check
//!   `symlink_metadata` immediately before opening, which stops a link that is
//!   already in the package, but `std` offers no portable `O_NOFOLLOW`, so a
//!   link planted in the microseconds between the two calls would still be
//!   followed. That needs platform-specific code and has not been written.

#![forbid(unsafe_code)]

pub mod assets;
pub mod atomic;
pub mod error;
mod hexid;
pub mod journal;
pub mod manifest;
pub mod migrate;
pub mod package;
pub mod preview;
pub mod safepath;
pub mod tiles;

pub use assets::{AssetInput, AssetReport, MAX_ASSET_DATA_BYTES};
pub use error::ProjectError;
pub use journal::{CommandJournal, DocumentDigest, JournalRecovery, SaveMark};
pub use manifest::{FileDigest, Manifest, MANIFEST_VERSION, MIN_SUPPORTED_MANIFEST_VERSION};
pub use migrate::{document_version, migrate, MAX_DOCUMENT_VERSION, MIN_DOCUMENT_VERSION};
pub use package::{
    load_project, open_project, save_project, save_project_with, LoadedProject, SaveOptions,
    SaveReport, AI_DIR, DOCUMENT_FILE, JOURNAL_FILE, MANIFEST_FILE, UNKNOWN_APP_VERSION,
};
pub use preview::{Preview, PREVIEWS_DIR, PREVIEW_FILE};
pub use tiles::{NoTiles, TileBytes, TileReport, TILES_DIR};

#[cfg(test)]
mod tests;
