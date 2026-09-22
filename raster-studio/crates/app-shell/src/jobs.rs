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
///
/// When the OS refuses the thread (out of handles, out of address space —
/// rare, but a live process can hit it), no worker runs and the *failure*
/// arrives on the receiver instead, shaped like the import it would have
/// been: the caller's poll reports it through the same route as a decode
/// error, and the interaction thread never panics over it.
pub fn spawn_import(path: PathBuf, generation: u64) -> Receiver<ImportOutcome> {
    spawn_import_with(path, generation, |name, body| {
        std::thread::Builder::new()
            .name(name)
            .spawn(body)
            .map(|_handle| ())
    })
}

/// A way to start a worker: given a thread name and its body, either run the
/// body on another thread or say why not. Injected so the refusal route can
/// be exercised without exhausting the machine's threads.
pub type Spawner = fn(String, Box<dyn FnOnce() + Send>) -> std::io::Result<()>;

/// [`spawn_import`] with the thread spawner injected.
pub fn spawn_import_with(
    path: PathBuf,
    generation: u64,
    spawn: Spawner,
) -> Receiver<ImportOutcome> {
    let (tx, rx) = channel();
    let name = format!("import:{}", path.display());
    let worker_path = path.clone();
    let worker_tx = tx.clone();
    let body: Box<dyn FnOnce() + Send> = Box::new(move || {
        let outcome = run(worker_path, generation);
        // A send fails only when the receiver is gone — the user quit or
        // the job was superseded. That is not an error; the result is
        // simply not needed any more.
        let _ = worker_tx.send(outcome);
    });
    if let Err(e) = spawn(name, body) {
        let reason = format!("could not start the import worker: {e}");
        let _ = tx.send(failed_outcome(path, generation, reason));
    }
    rx
}

/// The outcome an import that never ran reports: the same variant a real
/// worker would have produced for `path`, carrying `reason` as its error.
fn failed_outcome(path: PathBuf, generation: u64, reason: String) -> ImportOutcome {
    if crate::import::looks_like_psd(&path) {
        ImportOutcome::Psd {
            path,
            generation,
            bytes: Err(reason),
        }
    } else {
        ImportOutcome::Image {
            path,
            generation,
            decoded: Err(reason),
        }
    }
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
    fn a_refused_thread_is_reported_on_the_receiver_not_panicked() {
        fn refuse(_name: String, _body: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "no more threads",
            ))
        }
        let rx = spawn_import_with(PathBuf::from("photo.png"), 4, refuse);
        match rx.recv().expect("the refusal arrives on the receiver") {
            ImportOutcome::Image {
                path,
                generation,
                decoded,
            } => {
                assert_eq!(path, PathBuf::from("photo.png"));
                assert_eq!(generation, 4);
                let e = decoded.expect_err("an import that never ran is a failure");
                assert!(e.contains("import worker"), "{e}");
                assert!(e.contains("no more threads"), "{e}");
            }
            _ => panic!("expected an image outcome"),
        }
        // A .psd that never ran fails in the .psd shape, so the poller's
        // existing failure route applies unchanged. (`looks_like_psd` goes by
        // content, so the file has to exist and open with the signature.)
        let dir = tempfile::tempdir().unwrap();
        let psd = dir.path().join("layers.psd");
        std::fs::write(&psd, b"8BPS\0\x01 not a whole document").unwrap();
        let rx = spawn_import_with(psd, 4, refuse);
        match rx.recv().unwrap() {
            ImportOutcome::Psd { bytes, .. } => assert!(bytes.is_err()),
            _ => panic!("expected a psd outcome"),
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
