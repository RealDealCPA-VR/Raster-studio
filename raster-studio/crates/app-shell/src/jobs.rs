//! Off-thread long operations (card 087).
//!
//! Importing a file is the one long operation the interaction thread
//! performs on every open: the bytes are read from disk and the pixels are
//! decoded before a document exists. Both are pure data work with no UI
//! types involved, so they run on a worker thread; the interaction thread
//! polls the outcome once per frame and builds the document when it arrives.
//!
//! # Stale completions cannot apply
//!
//! Every job carries the [`Editor`]'s import generation at spawn time. The
//! generation is bumped whenever pending imports are cancelled, and a
//! completion whose generation no longer matches is dropped unread at poll
//! time — an out-of-order or cancelled completion can never mutate the
//! document set. The document a completed job becomes is minted fresh at
//! apply time, so a completed import can never land inside the wrong
//! document either.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

/// What a finished import job hands back. Pure data: a worker thread must
/// never touch UI types, so everything here is `Send` by construction.
pub enum ImportOutcome {
    /// A flat image, decoded off-thread.
    Image {
        path: PathBuf,
        generation: u64,
        decoded: Result<crate::import::DecodedImage, String>,
    },
    /// A `.psd`: the bytes (bounded by the same ceiling the synchronous read
    /// applies) travel; the layered parse stays on the interaction thread,
    /// where the tile store it fills lives.
    Psd {
        path: PathBuf,
        generation: u64,
        bytes: Result<Vec<u8>, String>,
    },
}

impl ImportOutcome {
    /// The job's spawn-time generation.
    pub fn generation(&self) -> u64 {
        match self {
            Self::Image { generation, .. } | Self::Psd { generation, .. } => *generation,
        }
    }

    /// The file the job came from.
    pub fn path(&self) -> &PathBuf {
        match self {
            Self::Image { path, .. } | Self::Psd { path, .. } => path,
        }
    }

    /// `true` when the outcome belongs to a cancelled generation and must be
    /// dropped unread.
    pub fn is_stale(&self, current: u64) -> bool {
        self.generation() != current
    }
}

/// Largest flat image the worker will decode, matching the synchronous
/// route's own ceiling philosophy: finite, far past any real document.
const MAX_IMPORT_BYTES: u64 = 2 << 30;

/// Spawn a worker that reads (and, for a flat image, decodes) `path`,
/// reporting one [`ImportOutcome`] tagged with `generation`. The receiver is
/// the only channel back; dropping it simply discards the result.
pub fn spawn_import(path: PathBuf, generation: u64) -> Receiver<ImportOutcome> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name(format!("import:{}", path.display()))
        .spawn(move || {
            let outcome = run(path, generation);
            // A send fails only when the receiver is gone — the user quit or
            // the job was superseded. That is not an error; the result is
            // simply not needed any more.
            let _ = tx.send(outcome);
        })
        .expect("spawning the import worker cannot fail on a live process");
    rx
}

/// The worker's body: read, and decode what a flat image decodes.
fn run(path: PathBuf, generation: u64) -> ImportOutcome {
    if crate::import::looks_like_psd(&path) {
        let bytes = read_bounded(&path).map_err(|e| e.to_string());
        ImportOutcome::Psd {
            path,
            generation,
            bytes,
        }
    } else {
        let decoded = match read_bounded(&path) {
            Ok(bytes) => {
                crate::import::DecodedImage::decode_bytes(&bytes).map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        };
        ImportOutcome::Image {
            path,
            generation,
            decoded,
        }
    }
}

/// Read a whole file, refusing anything past [`MAX_IMPORT_BYTES`] before it
/// is read — the same rule the synchronous PSD path applies.
fn read_bounded(path: &std::path::Path) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let declared = file.metadata()?.len();
    if declared > MAX_IMPORT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "this file is {declared} bytes, more than the {MAX_IMPORT_BYTES} this build will read"
            ),
        ));
    }
    let mut bytes = Vec::new();
    // Not `with_capacity(declared)`: that reserves whatever the file claims
    // (the metadata can lie). `take` bounds what is actually read.
    file.take(MAX_IMPORT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_IMPORT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "this file is {} bytes, more than the {MAX_IMPORT_BYTES} this build will read",
                bytes.len()
            ),
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(dir: &std::path::Path, w: u32, h: u32, v: u8) -> PathBuf {
        let path = dir.join(format!("img-{w}x{h}-{v}.png"));
        std::fs::write(
            &path,
            raster::encode(
                raster::ExportFormat::Png,
                w,
                h,
                &vec![v; (w * h * 4) as usize],
            )
            .unwrap(),
        )
        .unwrap();
        path
    }

    #[test]
    fn an_import_job_decodes_off_thread_and_reports_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), 8, 6, 77);
        let rx = spawn_import(path.clone(), 3);
        match rx.recv().expect("the job completes") {
            ImportOutcome::Image {
                path: p,
                generation,
                decoded,
            } => {
                assert_eq!(p, path);
                assert_eq!(generation, 3);
                let img = decoded.expect("the png decodes");
                assert_eq!((img.width, img.height), (8, 6));
                assert!(img.rgba8.iter().all(|&b| b == 77));
            }
            _ => panic!("expected an image outcome"),
        }
    }

    #[test]
    fn a_psd_job_hands_back_bounded_bytes_for_the_layered_parse() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(8, 8));
        let canvas = psd::Rect::sized(8, 8);
        let mut layer = psd::PsdLayer::raster("L", canvas);
        layer.set_rgba8(&vec![9u8; 8 * 8 * 4]).unwrap();
        file.layers = vec![layer];
        let path = dir.path().join("job.psd");
        std::fs::write(&path, psd::write(&file).unwrap()).unwrap();

        let rx = spawn_import(path.clone(), 1);
        match rx.recv().unwrap() {
            ImportOutcome::Psd { path: p, bytes, .. } => {
                assert_eq!(p, path);
                assert_eq!(&bytes.unwrap()[..4], b"8BPS");
            }
            _ => panic!("expected a psd outcome"),
        }
    }

    #[test]
    fn a_missing_file_is_a_reported_failure_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let rx = spawn_import(dir.path().join("gone.png"), 0);
        match rx.recv().unwrap() {
            ImportOutcome::Image { decoded, .. } => assert!(decoded.is_err()),
            _ => panic!("expected an image outcome"),
        }
    }

    #[test]
    fn a_oversized_file_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.png");
        // A sparse-looking lie: the header says PNG, the metadata says huge.
        // The worker must refuse on the metadata before reserving anything.
        let mut f = std::fs::File::create(&path).unwrap();
        f.set_len(MAX_IMPORT_BYTES + 2).unwrap();
        std::io::Write::write_all(&mut f, b"RIFF-not-a-real-image").unwrap();
        drop(f);
        let rx = spawn_import(path, 0);
        match rx.recv().unwrap() {
            ImportOutcome::Image { decoded, .. } => {
                let e = match decoded {
                    Err(e) => e,
                    Ok(_) => panic!("expected a refusal"),
                };
                assert!(e.contains("more than the"), "{e}");
            }
            _ => panic!("expected an image outcome"),
        }
    }

    #[test]
    fn staleness_is_a_generation_comparison() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), 4, 4, 5);
        let rx = spawn_import(path, 7);
        let outcome = rx.recv().unwrap();
        assert!(!outcome.is_stale(7));
        assert!(outcome.is_stale(8), "a cancelled generation is stale");
    }
}
