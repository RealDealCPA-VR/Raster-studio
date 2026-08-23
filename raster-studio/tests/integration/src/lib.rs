//! The harness the end-to-end tests share.
//!
//! # What these tests are for
//!
//! Every other crate in the workspace tests itself. What none of them can test
//! is the thing the user actually does: open a picture, paint on it, build a
//! stack of layers, save it, close it, open it again, and get the same pixels
//! back. That property lives in the *seams* — between the document and the tile
//! store, between the compositor and the package format, between a command and
//! its inverse — and a seam is exactly what a unit test cannot reach.
//!
//! # These tests drive the application's engine, not a copy of it
//!
//! [`app_shell::doc::OpenDocument`] is the whole state an open document is: the
//! [`editor_core::Document`], its [`editor_core::History`], the bytes its
//! content hashes name, the tile cache the canvas is painted from, and the
//! dirty set the presenter uploads. It carries no window and no GPU handle, so
//! the tests here open, edit, composite, save, reopen and export through the
//! very calls the shipping application makes:
//!
//! ```text
//!   OpenDocument::open_image     a file becomes a document with a raster layer
//!            |
//!   OpenDocument::apply          every edit is a command, through History
//!            v
//!   OpenDocument::composite      the document becomes the picture, via the
//!            |                   tile cache the canvas draws through
//!   OpenDocument::save_to        the picture survives being closed
//!   OpenDocument::open_project
//!            v
//!   OpenDocument::export_to      ...and is handed to another program
//!   session::recoverable/replay  ...and survives a process that crashed
//! ```
//!
//! A previous version of this suite carried its own `Engine` — a document, a
//! history and a tile store of its own, with an `apply` of its own. It passed
//! while exercising code the application never runs. What is left here is
//! [`app`]: command builders, fixture images, and one documented bridge for the
//! single seam `app-shell` has not wired yet.

#![forbid(unsafe_code)]

pub mod app;
pub mod fixture;

pub use app::{blank, linear, next_id, open_image, open_project, DocExt, DocTiles, APP_VERSION};
