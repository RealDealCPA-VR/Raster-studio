//! Everything that can go wrong reading or writing a `.rstudio` package.
//!
//! A package is **untrusted input** — it arrives by email, from a shared drive,
//! from a download. Every variant below that mentions a path, a length or a
//! count exists because the file gets to choose that value, and the answer to a
//! value we do not like is a named refusal rather than an allocation or an
//! open.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("msgpack encode error: {0}")]
    MsgpackEncode(#[from] rmp_serde::encode::Error),
    #[error("msgpack decode error: {0}")]
    MsgpackDecode(#[from] rmp_serde::decode::Error),
    #[error("asset store error: {0}")]
    Store(#[from] asset_store::StoreError),

    #[error("unsupported package layout version {found} (this build reads {min}..={max})")]
    UnsupportedVersion { found: u32, min: u32, max: u32 },
    #[error(
        "document format version {found} is outside what this build reads ({min}..={max}); \
         it was written by a newer Raster Studio"
    )]
    UnsupportedDocumentVersion { found: u32, min: u32, max: u32 },
    #[error("not a .rstudio package: {0}")]
    NotAPackage(String),

    /// A path *from the package* that would escape the package directory.
    ///
    /// This is the one error in this enum that means "hostile file", not
    /// "damaged file": `..`, a leading `/`, a `C:` prefix and a UNC share all
    /// land here, because `Path::join` would happily follow every one of them
    /// out of the package and read an arbitrary file on the machine.
    #[error("package field `{field}` is not a safe relative path: {value:?}")]
    UnsafePath { field: &'static str, value: String },
    #[error("package field `{field}` must name {expected:?}, not {value:?}")]
    UnexpectedPath {
        field: &'static str,
        expected: &'static str,
        value: String,
    },
    #[error("{path} is a symbolic link; a package may not contain one")]
    Symlink { path: String },
    #[error("{path} is not a regular file")]
    NotAFile { path: String },
    #[error("{path} is missing from the package")]
    MissingFile { path: String },
    #[error("{path} is {size} bytes, more than the {max} this reader will load")]
    FileTooLarge { path: String, size: u64, max: u64 },

    #[error("manifest integrity digest does not match the manifest's own contents")]
    ManifestIntegrityMismatch,
    #[error("{path} does not match the digest recorded in the manifest")]
    IntegrityMismatch { path: String },

    #[error("tile {hash} is referenced by the document but its bytes are not available")]
    MissingTile { hash: String },
    #[error("tile blob {path} does not hash to the name it is filed under")]
    CorruptBlob { path: String },
    #[error("package references {count} tiles, more than the {max} this reader will load")]
    TooManyTiles { count: u64, max: u64 },
    #[error("package tile data totals more than the {max} bytes this reader will load")]
    TileDataTooLarge { max: u64 },
    #[error("package lists {count} assets, more than the {max} this reader will load")]
    TooManyAssets { count: u64, max: u64 },
    #[error("package asset data totals more than the {max} bytes this reader will load")]
    AssetDataTooLarge { max: u64 },

    #[error("could not render the composite preview: {0}")]
    Preview(String),

    /// The swap failed *and* so did putting the previous package back.
    ///
    /// Never silently discarded: nothing is at the save path, the previous
    /// package is on disk under `backup`, the save that was in flight is under
    /// `temp`, and this error is the only thing that knows either name.
    #[error(
        "save failed ({source}) and the rollback failed too ({rollback}); \
         the previous package is still at {backup} and the new one at {temp}"
    )]
    RollbackFailed {
        source: std::io::Error,
        rollback: std::io::Error,
        backup: PathBuf,
        temp: PathBuf,
    },

    #[error("journal entry could not be replayed: {0}")]
    Replay(String),
    #[error(
        "journal was recorded against a different snapshot \
         (journal names {journal}, the loaded document is {snapshot})"
    )]
    SnapshotMismatch { journal: String, snapshot: String },
}
