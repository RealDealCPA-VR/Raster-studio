//! W16-M: video layers decode in the decode worker, driven through the real
//! `studio-desktop` binary (`CARGO_BIN_EXE_studio-desktop`) run as
//! `--decode-worker video`.
//!
//! What is proved here, each by a real child process:
//! - an H.264 MP4 the exporter wrote decodes to its frame count, size,
//!   durations and colours;
//! - a malformed MP4 (truncated, bit-flipped, garbage) comes back as an
//!   error or a picture, never as this process ending;
//! - the editor's video route (`Editor::open_video_path`, `add_media_path`),
//!   with `install` run as `main` runs it, opens the good file as a video
//!   layer and reports the malformed one as an error, the editor and its
//!   document still there.

use std::path::Path;
use std::time::Duration;

use app_shell::dialogs::decode_worker::{
    decode_video_in_worker, encode_mp4, install, set_worker_executable, ImportLimits, Mp4Frame,
};

fn exe() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_studio-desktop"))
}

const TIMEOUT: Duration = Duration::from_secs(60);

/// Frame `i`: a per-frame colour on the left half, grey on the right.
fn frame(w: u32, h: u32, i: usize) -> Vec<u8> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..h {
        for x in 0..w {
            if x < w / 2 {
                px.extend_from_slice(&[(30 + i * 70) as u8, 210 - (i * 60) as u8, 50, 255]);
            } else {
                px.extend_from_slice(&[128, 128, 128, 255]);
            }
        }
    }
    px
}

fn clip(w: u32, h: u32, durations: &[u32]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let px: Vec<Vec<u8>> = (0..durations.len()).map(|i| frame(w, h, i)).collect();
    let frames: Vec<Mp4Frame<'_>> = px
        .iter()
        .zip(durations)
        .map(|(p, d)| Mp4Frame {
            rgba8: p,
            duration_ms: *d,
        })
        .collect();
    (encode_mp4(w, h, &frames, 90).unwrap(), px)
}

fn close(got: &[u8], want: &[u8], tolerance: i32) -> bool {
    got.iter()
        .zip(want)
        .all(|(g, w)| (i32::from(*g) - i32::from(*w)).abs() <= tolerance)
}

#[test]
fn an_h264_mp4_decodes_in_the_worker_process() {
    let (w, h) = (48, 32);
    let (bytes, px) = clip(w, h, &[100, 300, 200]);
    let video = decode_video_in_worker(exe(), &bytes, ImportLimits::default(), TIMEOUT).unwrap();
    assert_eq!((video.width, video.height), (w, h));
    assert_eq!(video.frames.len(), 3);
    assert_eq!(
        video
            .frames
            .iter()
            .map(|f| f.duration_ms)
            .collect::<Vec<_>>(),
        vec![100, 300, 200]
    );
    for (i, f) in video.frames.iter().enumerate() {
        let at = ((16 * w + 6) * 4) as usize;
        assert!(
            close(&f.rgba8[at..at + 4], &px[i][at..at + 4], 8),
            "frame {i}: {:?}",
            &f.rgba8[at..at + 4]
        );
    }
}

#[test]
fn malformed_mp4s_are_errors_and_never_end_this_process() {
    let (bytes, _) = clip(32, 32, &[100, 100, 100]);
    let mut refused = 0;
    let err = decode_video_in_worker(exe(), b"not a video", ImportLimits::default(), TIMEOUT)
        .unwrap_err();
    assert!(err.to_string().contains("MP4"), "{err}");
    for cut in [0, 12, bytes.len() / 3, bytes.len() / 2, bytes.len() - 1] {
        let r = decode_video_in_worker(exe(), &bytes[..cut], ImportLimits::default(), TIMEOUT);
        assert!(r.is_err(), "a file cut at {cut} is refused");
        refused += 1;
    }
    // Bit flips past the `ftyp` head: each comes back as a result.
    for n in 0..40usize {
        let mut copy = bytes.clone();
        let at = 16 + (n.wrapping_mul(2_654_435_761) % (copy.len() - 16));
        copy[at] ^= 1 << (n % 8);
        if decode_video_in_worker(exe(), &copy, ImportLimits::default(), TIMEOUT).is_err() {
            refused += 1;
        }
    }
    assert!(refused >= 5);
    // Still here, and the worker still answers a good file.
    assert_eq!(
        decode_video_in_worker(exe(), &bytes, ImportLimits::default(), TIMEOUT)
            .unwrap()
            .frames
            .len(),
        3
    );
}

#[test]
fn the_editor_video_route_reaches_the_worker() {
    set_worker_executable(exe());
    install();
    let dir = tempfile::tempdir().unwrap();
    let (bytes, _) = clip(32, 32, &[250, 250]);
    let good = dir.path().join("clip.mp4");
    std::fs::write(&good, &bytes).unwrap();
    let broken = dir.path().join("broken.mp4");
    std::fs::write(&broken, &bytes[..bytes.len() / 2]).unwrap();

    let mut ed = app_shell::editor::Editor::with_state(
        app_shell::AppPaths::rooted(dir.path().join("config")),
        app_shell::Preferences::default(),
        app_shell::RecentFiles::new(),
        Box::new(app_shell::ScriptedDialogs::new()),
    );
    ed.open_video_path(&good).unwrap();
    let doc = &ed.active().unwrap().document;
    assert_eq!(doc.timeline.videos.len(), 1);
    assert_eq!(doc.timeline.videos[0].frames.len(), 2);

    let err = ed.open_video_path(&broken).unwrap_err();
    assert!(err.contains("MP4"), "{err}");
    let err = ed.add_media_path(&broken).unwrap_err();
    assert!(err.contains("MP4"), "{err}");
    assert_eq!(
        ed.documents().len(),
        1,
        "the editor and its document remain"
    );
    // And the good file still adds to the open document.
    ed.add_media_path(&good).unwrap();
    assert_eq!(ed.active().unwrap().document.timeline.videos.len(), 2);
}
