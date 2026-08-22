//! Editor core: the [`Document`], the [`Command`] system, and the undo/redo
//! [`History`].
//!
//! The central invariant of the whole product — *the document remains editable
//! after every operation* — is enforced here. Every user-visible edit is a
//! deterministic [`Command`] that can `apply`, produce its `inverse`, be
//! serialized, and be replayed from a journal.

pub mod command;
pub mod document;
pub mod history;
pub mod selection;

pub use command::{Command, CommandError, LayerPatch};
pub use document::{Document, DocumentMeta};
pub use history::History;
pub use selection::Selection;
