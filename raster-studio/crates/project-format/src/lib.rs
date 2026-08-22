//! The `.rstudio` project package: read, write, migrate.
//!
//! A project is a **directory package**, not a monolithic file:
//!
//! ```text
//! project.rstudio/
//! ├── manifest.json          format version + integrity
//! ├── document.msgpack       serialized Document
//! ├── commands.journal       accepted commands (crash recovery / replay)
//! ├── previews/              thumbnail + composite preview
//! ├── tiles/                 content-addressed tile blobs
//! ├── assets/                embedded/linked asset records
//! └── ai/                    generation metadata
//! ```
//!
//! Save is **atomic**: write a fresh package to a temp path, fsync, then
//! rename over the target. A command journal is appended on every accepted
//! command so an interrupted session can be recovered by replay.

pub mod journal;
pub mod manifest;
pub mod package;

pub use journal::CommandJournal;
pub use manifest::{Manifest, MANIFEST_VERSION};
pub use package::{load_project, save_project, ProjectError};
