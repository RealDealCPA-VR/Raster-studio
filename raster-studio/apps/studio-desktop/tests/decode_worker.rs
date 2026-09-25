//! W15-A: the decode worker, driven through the real `studio-desktop`
//! binary (`CARGO_BIN_EXE_studio-desktop`, which cargo builds for these
//! tests): the parent is this test process, the child is the editor's own
//! executable run as `--decode-worker`.
//!
//! What is proved here, each by a real child process:
//! - an AVIF and a HEIC decode to the right size, depth and colours;
//! - 1000 bit-flipped copies each come back as a result (a picture or an
//!   error) and this process is still here afterwards;
//! - a worker that panics is reported as a crash, one that hangs is killed
//!   at the deadline, and an answer whose header declares a size past the
//!   limits is refused before a pixel is read;
//! - the open route (`Editor::open_any`, and File > Open's job) reaches the
//!   worker once `install` has run, as `main` runs it.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use app_shell::dialogs::decode_worker::{
    decode_in_worker, install, run_worker, set_worker_executable, CodecError, DecodedSurface,
    HeifKind, ImportLimits, SurfacePixels,
};

const AVIF_10BIT: &[u8] = include_bytes!(
    "../../../crates/raster/src/formats/testdata/w15a_quadrants_16x12_10bit_alpha.avif"
);
const AVIF_LIBAOM: &[u8] =
    include_bytes!("../../../crates/raster/src/formats/testdata/quadrants_16x12_libaom_420.avif");
const HEIC: &[u8] = include_bytes!(
    "../../../crates/raster/src/formats/testdata/w15a_grey_64x128_irot90_alpha.heic"
);

fn exe() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_studio-desktop"))
}

const TIMEOUT: Duration = Duration::from_secs(60);

fn px8(s: &DecodedSurface, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * s.width + x) * 4) as usize;
    match &s.pixels {
        SurfacePixels::Rgba8(v) => [v[i], v[i + 1], v[i + 2], v[i + 3]],
        SurfacePixels::Rgba16(v) => [0, 1, 2, 3].map(|c| (u32::from(v[i + c]) * 255 / 65535) as u8),
    }
}

fn close(got: [u8; 4], want: [u8; 4], tolerance: i32) -> bool {
    got.iter()
        .zip(want)
        .all(|(g, w)| (i32::from(*g) - i32::from(w)).abs() <= tolerance)
}

#[test]
fn an_avif_and_a_heic_decode_in_the_worker_process() {
    // The 10-bit AVIF fixture: red, green / blue, clear quadrants, 16x12.
    let s = decode_in_worker(
        exe(),
        HeifKind::Avif,
        AVIF_10BIT,
        ImportLimits::default(),
        TIMEOUT,
    )
    .unwrap();
    assert_eq!((s.width, s.height), (16, 12));
    assert!(
        matches!(s.pixels, SurfacePixels::Rgba16(_)),
        "10-bit opens 16-bit"
    );
    assert!(
        close(px8(&s, 4, 3), [220, 30, 40, 255], 6),
        "{:?}",
        px8(&s, 4, 3)
    );
    assert!(
        close(px8(&s, 11, 3), [30, 200, 60, 255], 6),
        "{:?}",
        px8(&s, 11, 3)
    );
    assert!(
        close(px8(&s, 4, 8), [40, 50, 210, 255], 6),
        "{:?}",
        px8(&s, 4, 8)
    );
    assert_eq!(px8(&s, 11, 8)[3], 0, "the clear quadrant");
    // An independent (libaom) 4:2:0 AVIF.
    let s = decode_in_worker(
        exe(),
        HeifKind::Avif,
        AVIF_LIBAOM,
        ImportLimits::default(),
        TIMEOUT,
    )
    .unwrap();
    assert_eq!((s.width, s.height), (16, 12));
    // The HEIC fixture: 64x128 coded, `irot` 90, grey 128 with alpha 128.
    let s = decode_in_worker(
        exe(),
        HeifKind::Heic,
        HEIC,
        ImportLimits::default(),
        TIMEOUT,
    )
    .unwrap();
    assert_eq!((s.width, s.height), (128, 64));
    assert!(
        close(px8(&s, 10, 10), [128, 128, 128, 128], 1),
        "{:?}",
        px8(&s, 10, 10)
    );
}

/// A deterministic bit flip: bit `n % 8` of byte `(n * 2654435761) % len`
/// past the first 16 bytes (the `ftyp` head decides the kind, and a flip
/// there only tests the sniff).
fn flipped(file: &[u8], n: usize) -> Vec<u8> {
    let mut copy = file.to_vec();
    let span = copy.len() - 16;
    let at = 16 + (n.wrapping_mul(2_654_435_761) % span);
    copy[at] ^= 1 << (n % 8);
    copy
}

#[derive(Default, Debug)]
struct Tally {
    decoded: usize,
    refused: usize,
    crashed: usize,
    timed_out: usize,
}

#[test]
fn a_thousand_bit_flipped_files_come_back_as_results_and_never_abort_this_process() {
    const COPIES: usize = 1000;
    let jobs: Vec<(HeifKind, Vec<u8>)> = (0..COPIES)
        .map(|n| match n % 3 {
            0 => (HeifKind::Avif, flipped(AVIF_10BIT, n)),
            1 => (HeifKind::Avif, flipped(AVIF_LIBAOM, n)),
            _ => (HeifKind::Heic, flipped(HEIC, n)),
        })
        .collect();
    let (tx, rx) = mpsc::channel();
    let chunks: Vec<Vec<(HeifKind, Vec<u8>)>> = {
        let mut chunks: Vec<Vec<_>> = (0..4).map(|_| Vec::new()).collect();
        for (i, job) in jobs.into_iter().enumerate() {
            chunks[i % 4].push(job);
        }
        chunks
    };
    std::thread::scope(|scope| {
        for chunk in chunks {
            let tx = tx.clone();
            scope.spawn(move || {
                for (kind, bytes) in chunk {
                    let result =
                        decode_in_worker(exe(), kind, &bytes, ImportLimits::default(), TIMEOUT);
                    tx.send(result.map(|_| ()).map_err(|e| e.to_string()))
                        .unwrap();
                }
            });
        }
    });
    drop(tx);
    let mut tally = Tally::default();
    let mut returned = 0;
    for result in rx {
        returned += 1;
        match result {
            Ok(()) => tally.decoded += 1,
            Err(text) if text.contains("crashed") => tally.crashed += 1,
            Err(text) if text.contains("did not finish") => tally.timed_out += 1,
            Err(text) => {
                assert!(
                    text.contains("AVIF") || text.contains("HEIC") || text.contains("limit"),
                    "{text}"
                );
                tally.refused += 1;
            }
        }
    }
    eprintln!("bit-flipped copies through the worker: {tally:?}");
    // Every copy came back, and this process is still running to count
    // them: no damaged file reached a decoder in this process.
    assert_eq!(returned, COPIES);
    assert!(
        tally.refused + tally.crashed + tally.timed_out > 0,
        "{tally:?}"
    );
}

#[test]
fn a_worker_that_panics_is_reported_as_a_crash() {
    let err = run_worker(
        exe(),
        "test-panic",
        "AVIF",
        AVIF_10BIT,
        ImportLimits::default(),
        TIMEOUT,
    )
    .unwrap_err();
    let text = err.to_string();
    assert!(
        text.ends_with("the AVIF decoder crashed on this file; it may be damaged"),
        "{text}"
    );
}

#[test]
fn a_hung_worker_is_killed_at_the_deadline() {
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let result = run_worker(
            exe(),
            "test-hang",
            "HEIC",
            HEIC,
            ImportLimits::default(),
            Duration::from_secs(2),
        );
        let _ = tx.send(result.map(|_| ()).map_err(|e| e.to_string()));
    });
    // A watchdog far past the 2 s deadline, so a worker that is never killed
    // fails this test instead of hanging the suite.
    let result = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the hung worker was never stopped");
    let text = result.unwrap_err();
    assert!(text.contains("did not finish within 2 s"), "{text}");
    assert!(started.elapsed() >= Duration::from_secs(2));
}

#[test]
fn an_answer_declaring_a_size_past_the_limits_is_refused_before_its_pixels() {
    let err = run_worker(
        exe(),
        "test-oversize",
        "AVIF",
        AVIF_10BIT,
        ImportLimits::default(),
        TIMEOUT,
    )
    .unwrap_err();
    assert!(matches!(err, CodecError::LimitExceeded(_)), "{err}");
}

/// The open route: `install` (what `main` calls) routes the codec facade to
/// the worker, and `Editor::open_any` (Open Recent, drops, the command
/// line) and File > Open's job both open an AVIF and a HEIC as documents.
#[test]
fn the_editor_open_routes_reach_the_worker() {
    set_worker_executable(exe());
    install();
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, bytes: &[u8]| -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    };
    let avif = write("quadrants.avif", AVIF_10BIT);
    let heic = write("grey.heic", HEIC);
    let damaged = write("damaged.avif", &AVIF_10BIT[..AVIF_10BIT.len() / 2]);
    // A bit-flipped copy the real decoder panics on (the first of the
    // deterministic flips above that does), opened through the editor.
    let (crash_kind, crashing) = (0..600)
        .map(|n| match n % 3 {
            0 => (HeifKind::Avif, flipped(AVIF_10BIT, n)),
            1 => (HeifKind::Avif, flipped(AVIF_LIBAOM, n)),
            _ => (HeifKind::Heic, flipped(HEIC, n)),
        })
        .find(|(kind, bytes)| {
            decode_in_worker(exe(), *kind, bytes, ImportLimits::default(), TIMEOUT)
                .is_err_and(|e| e.to_string().contains("crashed"))
        })
        .expect("some flipped copy makes a decoder panic");
    let crashing = write(
        if crash_kind == HeifKind::Avif {
            "crashing.avif"
        } else {
            "crashing.heic"
        },
        &crashing,
    );

    let mut ed = app_shell::editor::Editor::with_state(
        app_shell::AppPaths::rooted(dir.path().join("config")),
        app_shell::Preferences::default(),
        app_shell::RecentFiles::new(),
        Box::new(app_shell::ScriptedDialogs::new().opening(&heic)),
    );
    ed.set_image_clipboard(Box::new(app_shell::clipboard::FakeClipboard::default()));

    // Open Recent / drop / command line.
    ed.open_any(&avif).unwrap().expect("a document");
    let open = ed.active_mut().unwrap();
    assert_eq!((open.document.width(), open.document.height()), (16, 12));
    assert_eq!(
        open.document.meta.bit_depth, 16,
        "a 10-bit AVIF opens 16-bit"
    );
    let rect = open.canvas_rect();
    let px = open.composite(rect).unwrap();
    let at = |x: usize, y: usize| {
        let i = (y * 16 + x) * 4;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };
    assert!(close(at(4, 3), [220, 30, 40, 255], 8), "{:?}", at(4, 3));

    // File > Open: the picker answers the HEIC, the job decodes it.
    ed.dispatch(app_shell::Action::Open).unwrap();
    ed.poll_imports();
    let open = ed.active_mut().unwrap();
    assert_eq!((open.document.width(), open.document.height()), (128, 64));

    // A damaged file fails to open with the decoder's reason, and the editor
    // carries on with both documents.
    let err = ed.open_any(&damaged).unwrap_err();
    assert!(err.contains("AVIF"), "{err}");
    // A file whose decoder panics: the worker dies, this process does not,
    // and the open fails saying so.
    let err = ed.open_any(&crashing).unwrap_err();
    assert!(err.contains("crashed on this file"), "{err}");
    assert_eq!(ed.documents().len(), 2);
    // ...and the editor still opens the next file.
    ed.open_any(&avif).unwrap().expect("a document");
    assert_eq!(ed.documents().len(), 3);
}
