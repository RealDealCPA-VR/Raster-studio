//! Append-only command journal for crash recovery and deterministic replay.
//!
//! One record per line, JSON. Two record kinds:
//!
//! * a **command** — one accepted [`Command`];
//! * a **save marker** — "the document was written to disk here, and it hashed
//!   to *this*".
//!
//! # Why a torn line is not a corrupt journal
//!
//! The journal's entire job is to survive a crash, and the signature of a crash
//! is a **half-written last line**. Refusing the whole file over it — which is
//! what `for line in reader.lines() { cmds.push(from_str(&line)?) }` did —
//! throws away every command the user did complete in order to punish the one
//! the power cut interrupted. Reading now stops at the first record it cannot
//! parse and keeps the valid prefix, reporting [`JournalRecovery::truncated`] so
//! a caller can say so.
//!
//! A record is written as **one buffer containing its payload and its newline**,
//! so an interrupted append leaves a partial line at the end and never a
//! payload with a missing terminator in the middle.
//!
//! # Why a save marker is not optional
//!
//! Without one, a journal is a list of commands with no idea which of them the
//! document on disk already contains. Replaying it onto a loaded snapshot
//! reapplies work that is already there: two "create layer" records become two
//! layers. The old test suite hid this by replaying onto a *fresh* document,
//! which validates a recovery model the application cannot use — the whole
//! point of a snapshot is not to rebuild from record zero.
//!
//! Recovery is therefore **snapshot + the suffix recorded after the last save
//! marker**, and the marker carries the [`DocumentDigest`] of the snapshot it
//! describes so a journal paired with the wrong document is refused rather than
//! replayed ([`JournalRecovery::replay_onto`]).
//!
//! # When a record may be appended, and what a save does with that window
//!
//! A record is appended **after its command has been accepted** into the
//! document: this is a log of what happened, not of what is about to.
//! [`crate::save_project_with`] holds `&Document` for the whole save, so no
//! command can be applied while one is running, and a record that reaches the
//! journal *during* a save therefore belongs to a command the snapshot being
//! written already contains.
//!
//! That is what makes the save's handling of the window correct rather than
//! lossy, and the window is real: the save copies the valid prefix of the
//! journal it read into the new package, and the directory swap then deletes the
//! old package — so a record appended between the read and the swap is deleted
//! with it. Dropping it is the right answer, because the command is in the
//! snapshot; carrying it across *after* the save marker would replay it onto a
//! document that already has it, which is exactly the duplicate-apply this
//! marker model exists to prevent. There is no third option: a record that
//! arrives after the read carries nothing that says which side of the snapshot
//! it belongs on, so the ordering rule above is what decides it.
//!
//! The limit that follows is stated rather than implied. An application that
//! journals a command **before** applying it (write-ahead), or that saves one
//! document while another thread mutates a copy of it, breaks that ordering, and
//! a record appended inside the window is then lost rather than redundant. This
//! crate states that contract; it cannot enforce it. See the crate-level "Known
//! limits".
//!
//! # Why the writers check for a symlink too
//!
//! `commands.journal` is the one file in a package that is neither
//! content-addressed nor digest-verified, and it is the one file this crate
//! *writes into an existing package* — repeatedly, for the whole session. That
//! made it, before this check existed, an arbitrary-file-write primitive: a
//! package whose `commands.journal` is a symlink to `~/.bashrc` loaded fine,
//! [`CommandJournal::append`] appended attacker-chosen JSON to that file, and
//! [`CommandJournal::clear`] truncated it to zero bytes.
//!
//! [`crate::open_project`] now refuses such a package, but a loader check alone
//! is not enough: the journal is appended to for the whole session and the link
//! can be planted *after* the open. So every writer here re-checks the target
//! immediately before opening it.
//!
//! **The residual race is stated rather than implied.** `std` has no portable
//! no-follow open (`O_NOFOLLOW`/`FILE_FLAG_OPEN_REPARSE_POINT`), so between the
//! `symlink_metadata` and the `open` there is a window an attacker with write
//! access to the package directory could still use. Closing it needs a
//! platform-specific open, which is future work; what is closed here is the
//! case that needs no race at all — a link that is simply *already there*.

use std::io::Write;
use std::path::Path;

use editor_core::{Command, Document};
use serde::{Deserialize, Serialize};

use crate::error::ProjectError;
use crate::hexid;

/// Largest journal this reader will load.
pub const MAX_JOURNAL_BYTES: u64 = 512 << 20;

/// BLAKE3 of a serialized document — the identity a save marker records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocumentDigest([u8; 32]);

impl DocumentDigest {
    pub fn of(document_bytes: &[u8]) -> Self {
        Self(*blake3::hash(document_bytes).as_bytes())
    }

    pub fn to_hex(&self) -> String {
        hexid::to_hex(&self.0)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        hexid::from_hex(s).map(Self)
    }
}

impl Serialize for DocumentDigest {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for DocumentDigest {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_hex(&s).ok_or_else(|| serde::de::Error::custom("not 64 hex digits"))
    }
}

/// One line of the journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Record {
    /// An accepted command.
    Command(Box<Command>),
    /// The document was saved here, and this is what it hashed to.
    Save { document: DocumentDigest },
}

/// A save marker and where it sits in the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveMark {
    /// Zero-based index of the marker record among all parsed records.
    pub seq: u64,
    /// Digest of the document snapshot the marker describes.
    pub document: DocumentDigest,
}

/// What a journal held, and which part of it recovery should replay.
#[derive(Debug, Clone, Default)]
pub struct JournalRecovery {
    records: u64,
    truncated: bool,
    last_save: Option<SaveMark>,
    commands: Vec<Command>,
    /// Index into `commands` of the first command recorded after the last save
    /// marker.
    first_unsaved: usize,
    /// Byte length of the valid prefix — the offset just past the last record
    /// that parsed. Copying only this much of a journal drops a torn tail
    /// without disturbing anything before it.
    valid_bytes: u64,
}

impl JournalRecovery {
    /// Every command in the valid prefix, oldest first.
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// The commands recorded after the last save marker — the ones a loaded
    /// snapshot is missing. Equal to [`JournalRecovery::commands`] when nothing
    /// has been saved yet.
    pub fn since_last_save(&self) -> &[Command] {
        &self.commands[self.first_unsaved..]
    }

    pub fn last_save(&self) -> Option<SaveMark> {
        self.last_save
    }

    /// `true` when parsing stopped early — the expected shape of a crash.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Records successfully parsed.
    pub fn records_read(&self) -> u64 {
        self.records
    }

    /// Byte length of the valid prefix. A save copies exactly this much of the
    /// old journal into the new package, so a torn tail is dropped and every
    /// intact record — including the previous save marker — is kept.
    pub fn valid_bytes(&self) -> u64 {
        self.valid_bytes
    }

    /// Replay the unsaved suffix onto a document loaded from the snapshot whose
    /// serialized bytes hashed to `snapshot`.
    ///
    /// Refuses a journal whose marker names a different snapshot: replaying it
    /// would apply commands to a document they were never recorded against.
    ///
    /// When the journal holds no marker, the whole journal is the suffix — the
    /// document has never been saved, so the caller is expected to pass a fresh
    /// one.
    pub fn replay_onto(
        &self,
        doc: &mut Document,
        snapshot: DocumentDigest,
    ) -> Result<usize, ProjectError> {
        if let Some(mark) = self.last_save {
            if mark.document != snapshot {
                return Err(ProjectError::SnapshotMismatch {
                    journal: mark.document.to_hex(),
                    snapshot: snapshot.to_hex(),
                });
            }
        }
        let mut applied = 0;
        for cmd in self.since_last_save() {
            cmd.apply(doc)
                .map_err(|e| ProjectError::Replay(e.to_string()))?;
            applied += 1;
        }
        Ok(applied)
    }
}

/// Reads/writes the newline-delimited command journal.
pub struct CommandJournal;

impl CommandJournal {
    /// Append a command to the journal file (creating it if needed) and fsync.
    pub fn append(path: &Path, cmd: &Command) -> Result<(), ProjectError> {
        Self::append_record(path, &Record::Command(Box::new(cmd.clone())))
    }

    /// Append a save marker naming the snapshot just written to disk.
    ///
    /// Everything after this record is what a crash would cost; everything
    /// before it is already in the document on disk.
    pub fn mark_saved(path: &Path, document: DocumentDigest) -> Result<(), ProjectError> {
        Self::append_record(path, &Record::Save { document })
    }

    fn append_record(path: &Path, record: &Record) -> Result<(), ProjectError> {
        // Before the open, every time. See the module header: this is the one
        // file the application writes into a package it did not build, so a
        // link planted here is a write to wherever the link points.
        reject_unsafe_target(path)?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        // Payload and terminator in ONE buffer and ONE `write_all`. Written as
        // two calls, a crash between them leaves a complete record with no
        // newline, which the next append then concatenates with — turning one
        // torn record into two unreadable ones.
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        f.write_all(&line)?;
        f.flush()?;
        f.sync_all()?; // durability: survive a crash right after append
        Ok(())
    }

    /// Read a journal, keeping the valid prefix and stopping at the first
    /// record that does not parse.
    ///
    /// An absent journal reads as an empty one. A journal that is a symlink, or
    /// is not a regular file, is refused — see the module header.
    pub fn read(path: &Path) -> Result<JournalRecovery, ProjectError> {
        match crate::safepath::read_capped(path, &label(path), MAX_JOURNAL_BYTES) {
            Ok(bytes) => Ok(Self::parse(&bytes)),
            Err(ProjectError::MissingFile { .. }) => Ok(JournalRecovery::default()),
            Err(e) => Err(e),
        }
    }

    /// Parse a journal that is already in memory.
    ///
    /// [`CommandJournal::read`] is this plus a capped read. It is public so a
    /// caller that has the bytes — the save path, copying a journal forward —
    /// can get the valid prefix **of the buffer it holds** rather than of a
    /// second, independent read of the same file. Those two disagree whenever
    /// the file grows in between, which is exactly what the application does to
    /// an open package, and the disagreement used to truncate the copy
    /// mid-record.
    pub fn parse(bytes: &[u8]) -> JournalRecovery {
        let mut out = JournalRecovery::default();
        let mut start = 0usize;
        loop {
            let Some(rel) = bytes[start..].iter().position(|b| *b == b'\n') else {
                // No terminator left. Anything still unread is a record whose
                // newline never reached the disk: the crash artifact.
                if start < bytes.len() {
                    out.truncated = true;
                }
                break;
            };
            let line = &bytes[start..start + rel];
            let next = start + rel + 1;
            if !line.iter().all(|b| b.is_ascii_whitespace()) {
                let Ok(record) = serde_json::from_slice::<Record>(line) else {
                    out.truncated = true;
                    break;
                };
                match record {
                    Record::Command(cmd) => out.commands.push(*cmd),
                    Record::Save { document } => {
                        out.last_save = Some(SaveMark {
                            seq: out.records,
                            document,
                        });
                        // Everything up to here is in the snapshot.
                        out.first_unsaved = out.commands.len();
                    }
                }
                out.records += 1;
            }
            start = next;
            out.valid_bytes = next as u64;
        }
        debug_assert!(out.valid_bytes <= bytes.len() as u64);
        out
    }

    /// Read all commands from a journal file (empty if the file is absent).
    ///
    /// Ignores save markers and returns the whole valid prefix. Replaying this
    /// onto a *loaded* document duplicates the work the snapshot already holds
    /// — use [`CommandJournal::read`] and
    /// [`JournalRecovery::replay_onto`] for recovery, and this only when
    /// rebuilding from an empty document on purpose.
    pub fn read_all(path: &Path) -> Result<Vec<Command>, ProjectError> {
        Ok(Self::read(path)?.commands)
    }

    /// Truncate the journal (called after a successful full save).
    ///
    /// Refuses a symlink. `File::create` follows one, so this call used to be a
    /// "truncate any file the user can write" primitive handed to whoever wrote
    /// the package.
    pub fn clear(path: &Path) -> Result<(), ProjectError> {
        reject_unsafe_target(path)?;
        if std::fs::symlink_metadata(path).is_ok() {
            std::fs::File::create(path)?; // truncates
        }
        Ok(())
    }
}

/// Name a journal path by its filename, for an error message that does not
/// leak the whole absolute path of the user's machine.
fn label(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Refuse to open a journal path that is a symlink or is not a regular file.
///
/// A path that does not exist yet is fine — that is the first append creating
/// the journal.
pub(crate) fn reject_unsafe_target(path: &Path) -> Result<(), ProjectError> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Err(ProjectError::Symlink { path: label(path) }),
        Ok(m) if !m.is_file() => Err(ProjectError::NotAFile { path: label(path) }),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::{Command, Document};
    use layer_model::Layer;

    fn create(name: &str) -> (Command, layer_model::LayerId) {
        let layer = Layer::raster(name);
        let id = layer.id;
        (Command::create_layer(layer), id)
    }

    #[test]
    fn append_read_replay() {
        let dir = tempfile::tempdir().unwrap();
        let jpath = dir.path().join("commands.journal");

        let mut doc = Document::new(64, 64, "t");
        let (cmd, id) = create("L1");
        cmd.apply(&mut doc).unwrap();
        CommandJournal::append(&jpath, &cmd).unwrap();

        // Fresh document + replay journal == same state.
        let mut recovered = Document::new(64, 64, "t");
        for c in CommandJournal::read_all(&jpath).unwrap() {
            c.apply(&mut recovered).unwrap();
        }
        assert!(recovered.layers.get(id).is_some());

        CommandJournal::clear(&jpath).unwrap();
        assert!(CommandJournal::read_all(&jpath).unwrap().is_empty());
    }

    #[test]
    fn a_torn_last_line_costs_only_that_line() {
        let dir = tempfile::tempdir().unwrap();
        let jpath = dir.path().join("j");
        let (a, a_id) = create("A");
        let (b, b_id) = create("B");
        CommandJournal::append(&jpath, &a).unwrap();
        CommandJournal::append(&jpath, &b).unwrap();

        // Chop the file mid-way through the second record, exactly as a power
        // cut during the append would.
        let bytes = std::fs::read(&jpath).unwrap();
        let first_newline = bytes.iter().position(|b| *b == b'\n').unwrap();
        let cut = first_newline + 1 + (bytes.len() - first_newline) / 2;
        std::fs::write(&jpath, &bytes[..cut]).unwrap();

        let rec = CommandJournal::read(&jpath).unwrap();
        assert!(rec.truncated(), "a torn tail must be reported");
        assert_eq!(rec.commands().len(), 1, "the valid prefix survives");

        let mut doc = Document::new(8, 8, "t");
        for c in rec.commands() {
            c.apply(&mut doc).unwrap();
        }
        assert!(doc.layers.get(a_id).is_some());
        assert!(doc.layers.get(b_id).is_none());
    }

    #[test]
    fn a_torn_line_in_the_middle_stops_the_read_there() {
        let dir = tempfile::tempdir().unwrap();
        let jpath = dir.path().join("j");
        let (a, _) = create("A");
        CommandJournal::append(&jpath, &a).unwrap();
        let good = std::fs::read(&jpath).unwrap();
        // valid record, garbage record, valid record
        let mut file = good.clone();
        file.extend_from_slice(b"{ not json at all\n");
        file.extend_from_slice(&good);
        std::fs::write(&jpath, &file).unwrap();

        let rec = CommandJournal::read(&jpath).unwrap();
        assert!(rec.truncated());
        assert_eq!(rec.commands().len(), 1);
        assert_eq!(rec.records_read(), 1);
    }

    #[test]
    fn a_record_is_written_as_one_buffer() {
        // The payload and its newline must reach the file together, so a
        // journal never contains a complete record with no terminator followed
        // by another record.
        let dir = tempfile::tempdir().unwrap();
        let jpath = dir.path().join("j");
        let (a, _) = create("A");
        CommandJournal::append(&jpath, &a).unwrap();
        let bytes = std::fs::read(&jpath).unwrap();
        assert_eq!(*bytes.last().unwrap(), b'\n');
        assert_eq!(bytes.iter().filter(|b| **b == b'\n').count(), 1);
    }

    #[test]
    fn an_empty_or_absent_journal_reads_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        let rec = CommandJournal::read(&missing).unwrap();
        assert!(rec.commands().is_empty() && !rec.truncated());

        let empty = dir.path().join("empty");
        std::fs::write(&empty, b"").unwrap();
        let rec = CommandJournal::read(&empty).unwrap();
        assert!(rec.commands().is_empty() && !rec.truncated());
    }

    #[test]
    fn recovery_replays_only_the_suffix_after_the_save_marker() {
        let dir = tempfile::tempdir().unwrap();
        let jpath = dir.path().join("j");

        // Two commands, then a save, then one more command.
        let mut doc = Document::new(64, 64, "t");
        let (a, a_id) = create("A");
        let (b, b_id) = create("B");
        for c in [&a, &b] {
            c.apply(&mut doc).unwrap();
            CommandJournal::append(&jpath, c).unwrap();
        }
        let snapshot_bytes = rmp_serde::to_vec_named(&doc).unwrap();
        let digest = DocumentDigest::of(&snapshot_bytes);
        CommandJournal::mark_saved(&jpath, digest).unwrap();

        let (c, c_id) = create("C");
        c.apply(&mut doc).unwrap();
        CommandJournal::append(&jpath, &c).unwrap();

        // Now: crash, reopen. The snapshot on disk holds A and B.
        let mut loaded: Document = rmp_serde::from_slice(&snapshot_bytes).unwrap();
        assert_eq!(loaded.layers.len(), 2);

        let rec = CommandJournal::read(&jpath).unwrap();
        assert_eq!(rec.commands().len(), 3);
        assert_eq!(rec.since_last_save().len(), 1);
        assert_eq!(rec.last_save().unwrap().seq, 2);

        assert_eq!(rec.replay_onto(&mut loaded, digest).unwrap(), 1);
        assert_eq!(
            loaded.layers.len(),
            3,
            "replaying the whole journal would have made five layers"
        );
        for id in [a_id, b_id, c_id] {
            assert!(loaded.layers.get(id).is_some());
        }
    }

    #[test]
    fn replaying_the_whole_journal_onto_a_snapshot_is_what_the_marker_prevents() {
        // The failure mode, demonstrated, so the test above is measuring
        // something real. Replaying every record onto a document that already
        // contains them reapplies work the snapshot holds; `CreateLayer` is
        // guarded well enough to refuse outright, which is the *lucky* case —
        // a paint or a property change would simply be applied twice.
        let dir = tempfile::tempdir().unwrap();
        let jpath = dir.path().join("j");
        let mut doc = Document::new(64, 64, "t");
        for name in ["A", "B"] {
            let (c, _) = create(name);
            c.apply(&mut doc).unwrap();
            CommandJournal::append(&jpath, &c).unwrap();
        }
        let bytes = rmp_serde::to_vec_named(&doc).unwrap();
        CommandJournal::mark_saved(&jpath, DocumentDigest::of(&bytes)).unwrap();

        let mut loaded: Document = rmp_serde::from_slice(&bytes).unwrap();
        let outcome: Result<(), _> = CommandJournal::read_all(&jpath)
            .unwrap()
            .iter()
            .try_for_each(|c| c.apply(&mut loaded).map(|_| ()));
        assert!(
            outcome.is_err(),
            "replaying the whole journal onto the snapshot has to go wrong; \
             it is only this loud because CreateLayer happens to notice"
        );

        // The anchored path, on the same journal, is a no-op: everything in it
        // predates the marker.
        let mut loaded: Document = rmp_serde::from_slice(&bytes).unwrap();
        let rec = CommandJournal::read(&jpath).unwrap();
        assert_eq!(
            rec.replay_onto(&mut loaded, DocumentDigest::of(&bytes))
                .unwrap(),
            0
        );
        assert_eq!(loaded.layers.len(), 2);
    }

    #[test]
    fn a_journal_recorded_against_another_document_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let jpath = dir.path().join("j");
        let (a, _) = create("A");
        CommandJournal::append(&jpath, &a).unwrap();
        CommandJournal::mark_saved(&jpath, DocumentDigest::of(b"one document")).unwrap();

        let rec = CommandJournal::read(&jpath).unwrap();
        let mut doc = Document::new(8, 8, "t");
        let err = rec
            .replay_onto(&mut doc, DocumentDigest::of(b"a different document"))
            .unwrap_err();
        assert!(
            matches!(err, ProjectError::SnapshotMismatch { .. }),
            "{err}"
        );
        assert_eq!(doc.layers.len(), 0, "the refusal changed nothing");
    }

    #[test]
    fn with_no_marker_the_whole_journal_is_the_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let jpath = dir.path().join("j");
        for name in ["A", "B"] {
            let (c, _) = create(name);
            CommandJournal::append(&jpath, &c).unwrap();
        }
        let rec = CommandJournal::read(&jpath).unwrap();
        assert!(rec.last_save().is_none());
        assert_eq!(rec.since_last_save().len(), 2);

        let mut fresh = Document::new(8, 8, "t");
        assert_eq!(
            rec.replay_onto(&mut fresh, DocumentDigest::of(b"anything"))
                .unwrap(),
            2,
            "with no marker there is nothing to disagree with"
        );
    }

    #[test]
    fn a_digest_serializes_as_hex_and_refuses_anything_else() {
        let d = DocumentDigest::of(b"x");
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json.len(), 66, "64 hex digits plus quotes: {json}");
        assert_eq!(serde_json::from_str::<DocumentDigest>(&json).unwrap(), d);
        assert!(serde_json::from_str::<DocumentDigest>("\"beef\"").is_err());
    }
}
