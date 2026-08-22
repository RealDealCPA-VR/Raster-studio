//! Append-only command journal for crash recovery and deterministic replay.
//!
//! Each accepted command is appended as one JSON line (JSONL). On recovery we
//! start from the last saved `document.msgpack` and replay any journal entries
//! recorded after it, restoring unsaved work.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use editor_core::Command;

use crate::package::ProjectError;

/// Reads/writes the newline-delimited command journal.
pub struct CommandJournal;

impl CommandJournal {
    /// Append a command to the journal file (creating it if needed) and flush.
    pub fn append(path: &Path, cmd: &Command) -> Result<(), ProjectError> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let line = serde_json::to_string(cmd)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()?;
        f.sync_all()?; // durability: survive a crash right after append
        Ok(())
    }

    /// Read all commands from a journal file (empty if the file is absent).
    pub fn read_all(path: &Path) -> Result<Vec<Command>, ProjectError> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let f = std::fs::File::open(path)?;
        let reader = BufReader::new(f);
        let mut cmds = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            cmds.push(serde_json::from_str::<Command>(&line)?);
        }
        Ok(cmds)
    }

    /// Truncate the journal (called after a successful full save).
    pub fn clear(path: &Path) -> Result<(), ProjectError> {
        if path.exists() {
            std::fs::File::create(path)?; // truncates
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::{Command, Document};
    use layer_model::Layer;

    #[test]
    fn append_read_replay() {
        let dir = tempfile::tempdir().unwrap();
        let jpath = dir.path().join("commands.journal");

        let mut doc = Document::new(64, 64, "t");
        let layer = Layer::raster("L1");
        let id = layer.id;
        let cmd = Command::CreateLayer { layer };
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
}
