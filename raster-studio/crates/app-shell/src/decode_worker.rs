//! W15-A: the decode worker - AVIF and HEIC decoded in a child process, so a
//! decoder panic cannot close the editor. W16-M: and the video track of an
//! MP4 for a video layer (the `video` and `video-window:<first>:<count>`
//! kinds, below the picture kinds in this file), with the same spawn,
//! deadline and crash handling.
//!
//! Declared from [`crate::dialogs`] (with `#[path]`), the open route's
//! module, and reached as `app_shell::dialogs::decode_worker`.
//!
//! # Why
//!
//! The AV1 decoder (`rusty_av1d`) and the HEVC decoder (`heic-rs`) both
//! panic on some damaged files, and the release profile is
//! `panic = "abort"` (see `raster::codec::formats::heif`). So the editor
//! never runs them itself. [`install`] (called by `studio-desktop`'s `main`
//! before any window exists) routes every AVIF / HEIC decode the codec
//! facade makes - File > Open, Open Recent, drag-and-drop, the command
//! line - to [`decode_in_worker`], which:
//!
//! 1. re-executes the editor's own binary ([`std::env::current_exe`]) as
//!    `<exe> --decode-worker avif|heic`, with stdin, stdout piped and
//!    stderr discarded;
//! 2. writes a request (the import limits, then the file's bytes) to its
//!    stdin from a thread of its own, and reads the answer from its stdout
//!    on another;
//! 3. checks the answer's header - width, height, depth, ICC length -
//!    against the limits **before** reading or allocating a pixel, then
//!    reads exactly the pixels that header implies;
//! 4. waits for the child at most [`DEFAULT_TIMEOUT`] in all (a wall-clock
//!    deadline from the spawn): past it the child is killed and the open
//!    fails with "did not finish";
//! 5. treats any exit other than a clean `0` - a panic, an abort, a kill, a
//!    truncated or malformed answer - as "the decoder crashed on this file;
//!    it may be damaged".
//!
//! The child side is [`run_if_worker`]: `main` calls it first, so a worker
//! never initialises logging, the panic crash-bundle hook, the console or a
//! window; it decodes with `raster::codec::formats::heif::decode_in_this_process`
//! and exits.
//!
//! # The protocol
//!
//! All integers little-endian. Request (parent to child): `RSDW`, version
//! `1`, `max_width` u32, `max_height` u32, `max_pixels` u64,
//! `max_alloc_bytes` u64, `max_icc_bytes` u64, `len` u64, then `len` file
//! bytes. Answer (child to parent): `RSDW`, version `1`, then status `0`
//! (decoded): `width` u32, `height` u32, `depth` u8 (8 or 16), `space` u8
//! (0 sRGB, 1 Display P3, 2 the ICC profile that follows), `icc_len` u32,
//! the profile, and `width * height * 4` samples of `depth` bits; or status
//! `1` (refused): `kind` u8 (0 unsupported, 1 over a limit), `len` u32 (at
//! most [`MAX_MESSAGE`]), a UTF-8 message.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{mpsc, RwLock};
use std::time::{Duration, Instant};

use raster::codec::formats::heif;
use raster::PixelFormat;

/// The types this module's functions take and return, for callers outside
/// the shell (the worker's own tests in `studio-desktop`).
pub use raster::codec::formats::heif::HeifKind;
pub use raster::{CodecError, DecodedSurface, ImportLimits, SurfacePixels};

/// The hidden flag that turns the editor's binary into a decode worker.
pub const WORKER_FLAG: &str = "--decode-worker";

/// How long a worker may take, from spawn to exit, before it is killed.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// The longest refusal message a worker may send.
pub const MAX_MESSAGE: usize = 4096;

const MAGIC: &[u8; 4] = b"RSDW";
const VERSION: u8 = 1;

/// The executable [`install`]'s decoder spawns, when not the current one.
static WORKER_EXE: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Spawn `exe` as the worker instead of [`std::env::current_exe`]: for a
/// process that is not the editor (a test harness) but can reach it.
pub fn set_worker_executable(exe: impl Into<PathBuf>) {
    *WORKER_EXE.write().unwrap_or_else(|e| e.into_inner()) = Some(exe.into());
}

/// Route every AVIF / HEIC decode in this process through the worker.
/// W16-M: and every video-layer decode
/// ([`decode_video_window_with_installed_worker`]).
pub fn install() {
    heif::install_isolated_decoder(decode_with_installed_worker);
    crate::timeline::video_layers::install_video_decoder(decode_video_window_with_installed_worker);
}

fn decode_with_installed_worker(
    kind: HeifKind,
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    let configured = WORKER_EXE.read().unwrap_or_else(|e| e.into_inner()).clone();
    let exe = match configured {
        Some(exe) => exe,
        None => std::env::current_exe().map_err(|e| {
            CodecError::Unsupported(format!(
                "could not find the editor's executable to start the {} decode worker: {e}",
                kind.name()
            ))
        })?,
    };
    decode_in_worker(&exe, kind, bytes, limits, DEFAULT_TIMEOUT)
}

/// Decode `bytes` (a `kind` file) in a worker process spawned from `exe`.
pub fn decode_in_worker(
    exe: &Path,
    kind: HeifKind,
    bytes: &[u8],
    limits: ImportLimits,
    timeout: Duration,
) -> Result<DecodedSurface, CodecError> {
    run_worker(exe, kind.worker_arg(), kind.name(), bytes, limits, timeout)
}

/// [`decode_in_worker`] with the worker argument spelled out, so a test can
/// name the debug-build-only kinds ([`run_if_worker`] lists them).
#[doc(hidden)]
pub fn run_worker(
    exe: &Path,
    arg: &str,
    name: &str,
    bytes: &[u8],
    limits: ImportLimits,
    timeout: Duration,
) -> Result<DecodedSurface, CodecError> {
    let request = encode_request(bytes, limits);
    spawn_worker(exe, arg, name, request, timeout, move |stdout| {
        read_answer(stdout, limits)
    })
}

/// W16-M: spawn `exe` as a worker of `arg`, send `request`, and read its
/// answer with `read`: the spawn, deadline and crash handling every kind
/// shares.
fn spawn_worker<T: Send + 'static>(
    exe: &Path,
    arg: &str,
    name: &str,
    request: Vec<u8>,
    timeout: Duration,
    read: impl FnOnce(std::process::ChildStdout) -> Result<Answer<T>, AnswerError> + Send + 'static,
) -> Result<T, CodecError> {
    let deadline = Instant::now() + timeout;
    let mut child = Command::new(exe)
        .arg(WORKER_FLAG)
        .arg(arg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            CodecError::Unsupported(format!(
                "could not start the {name} decode worker ({}): {e}",
                exe.display()
            ))
        })?;
    let crashed = || {
        CodecError::Unsupported(format!(
            "the {name} decoder crashed on this file; it may be damaged"
        ))
    };
    let timed_out = || {
        CodecError::Unsupported(format!(
            "the {name} decoder did not finish within {} s and was stopped; the file may be \
             damaged",
            timeout.as_secs()
        ))
    };

    // The request goes in from a thread of its own: a worker that stops
    // reading (it crashed, or it is answering first) must not block us.
    let stdin = child.stdin.take();
    let writer = std::thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            // A broken pipe here means the child is gone; its exit says why.
            let _ = stdin.write_all(&request);
        }
    });
    let Some(stdout) = child.stdout.take() else {
        kill(&mut child);
        return Err(crashed());
    };
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let _ = tx.send(read(stdout));
    });

    let remaining = deadline.saturating_duration_since(Instant::now());
    let answer = match rx.recv_timeout(remaining) {
        Ok(answer) => answer,
        Err(stopped) => {
            kill(&mut child);
            let _ = writer.join();
            let _ = reader.join();
            return Err(match stopped {
                mpsc::RecvTimeoutError::Timeout => timed_out(),
                // The reader ended without an answer: nothing it read can
                // be trusted.
                mpsc::RecvTimeoutError::Disconnected => crashed(),
            });
        }
    };
    let status = match wait_until(&mut child, deadline) {
        Some(status) => status,
        None => {
            kill(&mut child);
            let _ = writer.join();
            let _ = reader.join();
            return Err(timed_out());
        }
    };
    let _ = writer.join();
    let _ = reader.join();
    if !status.success() {
        return Err(crashed());
    }
    match answer {
        Ok(Answer::Decoded(surface)) => Ok(surface),
        Ok(Answer::Refused(err)) => Err(err),
        // A clean exit with a malformed answer is a worker that went wrong
        // all the same: the header check's own refusal (a limit) is kept.
        Err(AnswerError::Limit(err)) => Err(err),
        Err(AnswerError::Malformed) => Err(crashed()),
    }
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// The child's exit status, or `None` once `deadline` passes first.
fn wait_until(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) | Err(_) => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// The wire format.
// ---------------------------------------------------------------------------

fn encode_request(bytes: &[u8], limits: ImportLimits) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 48);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&limits.max_width.to_le_bytes());
    out.extend_from_slice(&limits.max_height.to_le_bytes());
    out.extend_from_slice(&limits.max_pixels.to_le_bytes());
    out.extend_from_slice(&limits.max_alloc_bytes.to_le_bytes());
    out.extend_from_slice(&(limits.max_icc_bytes as u64).to_le_bytes());
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
    out
}

fn read_array<const N: usize>(r: &mut impl Read) -> std::io::Result<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)?;
    Ok(b)
}

fn read_u32(r: &mut impl Read) -> std::io::Result<u32> {
    read_array::<4>(r).map(u32::from_le_bytes)
}

fn read_u64(r: &mut impl Read) -> std::io::Result<u64> {
    read_array::<8>(r).map(u64::from_le_bytes)
}

fn read_header(r: &mut impl Read) -> std::io::Result<bool> {
    let magic = read_array::<4>(r)?;
    let version = read_array::<1>(r)?[0];
    Ok(&magic == MAGIC && version == VERSION)
}

/// Read a request: the limits, then the file (bounded as the codec facade
/// bounds a file read: four times the decode allocation ceiling).
fn read_request(r: &mut impl Read) -> Result<(ImportLimits, Vec<u8>), String> {
    let io = |e: std::io::Error| format!("reading the request: {e}");
    if !read_header(r).map_err(io)? {
        return Err("not a decode worker request".into());
    }
    let limits = ImportLimits {
        max_width: read_u32(r).map_err(io)?,
        max_height: read_u32(r).map_err(io)?,
        max_pixels: read_u64(r).map_err(io)?,
        max_alloc_bytes: read_u64(r).map_err(io)?,
        max_icc_bytes: usize::try_from(read_u64(r).map_err(io)?).unwrap_or(usize::MAX),
    };
    let len = read_u64(r).map_err(io)?;
    let cap = limits.max_alloc_bytes.saturating_mul(4);
    if len > cap {
        return Err(format!("the file is larger than {cap} bytes"));
    }
    let mut data = Vec::new();
    r.take(len).read_to_end(&mut data).map_err(io)?;
    if data.len() as u64 != len {
        return Err("the request ended early".into());
    }
    Ok((limits, data))
}

fn encode_answer(result: &Result<DecodedSurface, CodecError>) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.push(VERSION);
    match result {
        Ok(s) => {
            out.push(0);
            out.extend_from_slice(&s.width.to_le_bytes());
            out.extend_from_slice(&s.height.to_le_bytes());
            let sixteen = s.format() == PixelFormat::Rgba16;
            out.push(if sixteen { 16 } else { 8 });
            let (space, icc): (u8, &[u8]) = match (&s.color_space, &s.icc_profile) {
                (_, Some(icc)) => (2, icc),
                (color::ColorSpace::DisplayP3, None) => (1, &[]),
                _ => (0, &[]),
            };
            out.push(space);
            out.extend_from_slice(&(icc.len() as u32).to_le_bytes());
            out.extend_from_slice(icc);
            match &s.pixels {
                SurfacePixels::Rgba8(v) => out.extend_from_slice(v),
                SurfacePixels::Rgba16(v) => {
                    out.reserve(v.len() * 2);
                    for x in v {
                        out.extend_from_slice(&x.to_le_bytes());
                    }
                }
            }
        }
        Err(e) => {
            out.push(1);
            out.push(u8::from(matches!(e, CodecError::LimitExceeded(_))));
            let text = e.to_string();
            let mut end = text.len().min(MAX_MESSAGE);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            out.extend_from_slice(&(end as u32).to_le_bytes());
            out.extend_from_slice(&text.as_bytes()[..end]);
        }
    }
    out
}

/// What a worker answered (W16-M: a picture, or a video's frames).
enum Answer<T = DecodedSurface> {
    Decoded(T),
    Refused(CodecError),
}

enum AnswerError {
    /// The header declared more than the limits allow; nothing was read
    /// past it.
    Limit(CodecError),
    /// Truncated, mis-tagged or oversized in some other way.
    Malformed,
}

/// Read an answer, checking its header against `limits` before reading a
/// single pixel.
fn read_answer(mut r: impl Read, limits: ImportLimits) -> Result<Answer, AnswerError> {
    let m = |_| AnswerError::Malformed;
    if !read_header(&mut r).map_err(m)? {
        return Err(AnswerError::Malformed);
    }
    match read_array::<1>(&mut r).map_err(m)?[0] {
        0 => {}
        1 => {
            let limit = read_array::<1>(&mut r).map_err(m)?[0] == 1;
            let len = read_u32(&mut r).map_err(m)? as usize;
            if len > MAX_MESSAGE {
                return Err(AnswerError::Malformed);
            }
            let mut text = vec![0; len];
            r.read_exact(&mut text).map_err(m)?;
            let text = String::from_utf8_lossy(&text).into_owned();
            return Ok(Answer::Refused(if limit {
                CodecError::LimitExceeded(text)
            } else {
                CodecError::Unsupported(text)
            }));
        }
        _ => return Err(AnswerError::Malformed),
    }
    let width = read_u32(&mut r).map_err(m)?;
    let height = read_u32(&mut r).map_err(m)?;
    let sixteen = match read_array::<1>(&mut r).map_err(m)?[0] {
        8 => false,
        16 => true,
        _ => return Err(AnswerError::Malformed),
    };
    let space = read_array::<1>(&mut r).map_err(m)?[0];
    let icc_len = read_u32(&mut r).map_err(m)? as usize;
    // The size cap, on the header, before any pixel is read or allocated.
    heif::check_worker_header(limits, width, height, sixteen).map_err(AnswerError::Limit)?;
    if icc_len > limits.max_icc_bytes || (space == 2) != (icc_len > 0) || space > 2 {
        return Err(AnswerError::Malformed);
    }
    let mut icc = vec![0; icc_len];
    r.read_exact(&mut icc).map_err(m)?;
    let samples = (width as usize) * (height as usize) * 4;
    let pixels = if sixteen {
        let mut raw = vec![0u8; samples * 2];
        r.read_exact(&mut raw).map_err(m)?;
        SurfacePixels::Rgba16(
            raw.as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes(*b))
                .collect(),
        )
    } else {
        let mut raw = vec![0u8; samples];
        r.read_exact(&mut raw).map_err(m)?;
        SurfacePixels::Rgba8(raw)
    };
    // Nothing may follow the pixels.
    if r.read(&mut [0u8; 1]).map_err(m)? != 0 {
        return Err(AnswerError::Malformed);
    }
    let (color_space, icc_profile) = match space {
        2 => (raster::icc_profile_space(&icc), Some(icc)),
        1 => (color::ColorSpace::DisplayP3, None),
        _ => (color::ColorSpace::Srgb, None),
    };
    Ok(Answer::Decoded(DecodedSurface {
        width,
        height,
        pixels,
        color_space,
        icc_profile,
        source_format: raster::ImportFormat::Avif,
    }))
}

// ---------------------------------------------------------------------------
// The child.
// ---------------------------------------------------------------------------

/// When this process was started as a decode worker (`argv[1]` is
/// [`WORKER_FLAG`]), do that job on stdin / stdout and return the exit code;
/// otherwise `None`, having touched nothing.
///
/// Debug builds also accept three test kinds, which the release build does
/// not have: `test-hang` (never answers), `test-panic` (reads the request,
/// then panics) and `test-oversize` (answers a header past any limit).
pub fn run_if_worker() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != WORKER_FLAG {
        return None;
    }
    let kind = args.next();
    Some(worker_main(
        kind,
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    ))
}

/// The worker's body, over any request and answer streams.
pub fn worker_main(kind: Option<OsString>, mut input: impl Read, mut output: impl Write) -> i32 {
    let kind = kind.and_then(|k| k.into_string().ok()).unwrap_or_default();
    #[cfg(debug_assertions)]
    match kind.as_str() {
        "test-hang" => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
        "test-panic" => {
            let _ = read_request(&mut input);
            panic!("test-panic: a decoder panic, on purpose");
        }
        "test-oversize" => {
            let _ = read_request(&mut input);
            let mut out = MAGIC.to_vec();
            out.push(VERSION);
            out.push(0);
            out.extend_from_slice(&u32::MAX.to_le_bytes());
            out.extend_from_slice(&u32::MAX.to_le_bytes());
            out.extend_from_slice(&[16, 0, 0, 0, 0, 0]);
            let _ = output.write_all(&out);
            return 0;
        }
        _ => {}
    }
    // W16-M: a video's frames (all, or a window of them), for a video layer.
    if let Some((first, count)) = video_window_of_arg(&kind) {
        let result = match read_request(&mut input) {
            Ok((limits, bytes)) => {
                video::decode_window_in_this_process(&bytes, limits, first, count)
            }
            Err(e) => Err(CodecError::Unsupported(e)),
        };
        let answer = encode_video_answer(&result);
        return match output.write_all(&answer).and_then(|()| output.flush()) {
            Ok(()) => 0,
            Err(_) => 3,
        };
    }
    let Some(kind) = HeifKind::from_worker_arg(&kind) else {
        return 2;
    };
    let result = match read_request(&mut input) {
        Ok((limits, bytes)) => heif::decode_in_this_process(kind, &bytes, limits),
        Err(e) => Err(CodecError::Unsupported(e)),
    };
    let answer = encode_answer(&result);
    if output
        .write_all(&answer)
        .and_then(|()| output.flush())
        .is_err()
    {
        return 3;
    }
    0
}

// ---------------------------------------------------------------------------
// W16-M: the video kind.
// ---------------------------------------------------------------------------
//
// `<exe> --decode-worker video` decodes an MP4's video track
// (`raster::codec::formats::mp4::video`) for a video layer;
// `<exe> --decode-worker video-window:<first>:<count>` decodes only the
// frames `first..first + count` (a video layer decodes on demand, a window
// around the timeline time at a time). The request is the one above.
// Answer: `RSDW`, version `1`, then status `0` (decoded): `width` u32,
// `height` u32, `codec` u8 (0 H.264, 1 AV1), `total` u32 (the track's
// frames), `first` u32, `count` u32 (the frames that follow), `total`
// durations (u32 ms), then `count * width * height * 4` RGBA8 bytes; or
// status `1`, a refusal exactly as above. The header (size, frame counts
// and the bytes they imply) is checked against the limits and the video
// bounds before a frame is read or allocated.

use raster::codec::formats::mp4::video;
pub use raster::codec::formats::mp4::video::{DecodedVideo, VideoCodec, VideoFrame, VideoWindow};
/// The MP4 writer File > Export As uses, for the worker's tests in
/// `studio-desktop` (which do not link `raster` themselves).
#[doc(hidden)]
pub use raster::codec::formats::mp4::{encode as encode_mp4, Mp4Frame};

/// The worker argument of the video kind (every frame).
pub const VIDEO_ARG: &str = "video";

/// The worker argument of a window of a video's frames:
/// `video-window:<first>:<count>`.
pub const VIDEO_WINDOW_ARG_PREFIX: &str = "video-window:";

/// The `(first, count)` a video worker argument names: every frame for
/// [`VIDEO_ARG`], the window for a [`VIDEO_WINDOW_ARG_PREFIX`] one; `None`
/// for any other kind.
fn video_window_of_arg(kind: &str) -> Option<(usize, usize)> {
    if kind == VIDEO_ARG {
        return Some((0, usize::MAX));
    }
    let (first, count) = kind
        .strip_prefix(VIDEO_WINDOW_ARG_PREFIX)?
        .split_once(':')?;
    Some((first.parse().ok()?, count.parse().ok()?))
}

/// Decode an MP4's video track in a worker process spawned from `exe`.
pub fn decode_video_in_worker(
    exe: &Path,
    bytes: &[u8],
    limits: ImportLimits,
    timeout: Duration,
) -> Result<DecodedVideo, CodecError> {
    let request = encode_request(bytes, limits);
    let window = spawn_worker(exe, VIDEO_ARG, "video", request, timeout, move |stdout| {
        read_video_answer(stdout, limits)
    })?;
    if window.first != 0 || window.frames.len() != window.durations_ms.len() {
        return Err(CodecError::Unsupported(
            "the video decode worker answered part of the video".into(),
        ));
    }
    Ok(DecodedVideo {
        width: window.width,
        height: window.height,
        codec: window.codec,
        frames: window.frames,
    })
}

/// W16-M: decode the frames `first..first + count` of an MP4's video track
/// (clamped to its end) in a worker process spawned from `exe`.
pub fn decode_video_window_in_worker(
    exe: &Path,
    bytes: &[u8],
    limits: ImportLimits,
    first: usize,
    count: usize,
    timeout: Duration,
) -> Result<VideoWindow, CodecError> {
    let request = encode_request(bytes, limits);
    let arg = format!("{VIDEO_WINDOW_ARG_PREFIX}{first}:{count}");
    let window = spawn_worker(exe, &arg, "video", request, timeout, move |stdout| {
        read_video_answer(stdout, limits)
    })?;
    if window.first != first {
        return Err(CodecError::Unsupported(
            "the video decode worker answered another window".into(),
        ));
    }
    Ok(window)
}

/// The executable [`install`] spawns: the one [`set_worker_executable`]
/// named, or this process's own.
fn video_worker_exe() -> Result<PathBuf, CodecError> {
    let configured = WORKER_EXE.read().unwrap_or_else(|e| e.into_inner()).clone();
    match configured {
        Some(exe) => Ok(exe),
        None => std::env::current_exe().map_err(|e| {
            CodecError::Unsupported(format!(
                "could not find the editor's executable to start the video decode worker: {e}"
            ))
        }),
    }
}

/// [`decode_video_in_worker`] with the executable [`install`] uses.
pub fn decode_video_with_installed_worker(
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<DecodedVideo, CodecError> {
    decode_video_in_worker(&video_worker_exe()?, bytes, limits, DEFAULT_TIMEOUT)
}

/// [`decode_video_window_in_worker`] with the executable [`install`] uses:
/// what every video layer decodes through once [`install`] has run.
pub fn decode_video_window_with_installed_worker(
    bytes: &[u8],
    limits: ImportLimits,
    first: usize,
    count: usize,
) -> Result<VideoWindow, CodecError> {
    decode_video_window_in_worker(
        &video_worker_exe()?,
        bytes,
        limits,
        first,
        count,
        DEFAULT_TIMEOUT,
    )
}

fn encode_video_answer(result: &Result<VideoWindow, CodecError>) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.push(VERSION);
    let v = match result {
        Ok(v) => v,
        Err(e) => {
            // A refusal, exactly as a picture's.
            out.push(1);
            out.push(u8::from(matches!(e, CodecError::LimitExceeded(_))));
            let text = e.to_string();
            let mut end = text.len().min(MAX_MESSAGE);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            out.extend_from_slice(&(end as u32).to_le_bytes());
            out.extend_from_slice(&text.as_bytes()[..end]);
            return out;
        }
    };
    out.push(0);
    out.extend_from_slice(&v.width.to_le_bytes());
    out.extend_from_slice(&v.height.to_le_bytes());
    out.push(match v.codec {
        VideoCodec::H264 => 0,
        VideoCodec::Av1 => 1,
    });
    out.extend_from_slice(&(v.durations_ms.len() as u32).to_le_bytes());
    out.extend_from_slice(&(v.first as u32).to_le_bytes());
    out.extend_from_slice(&(v.frames.len() as u32).to_le_bytes());
    for d in &v.durations_ms {
        out.extend_from_slice(&d.to_le_bytes());
    }
    for f in &v.frames {
        out.extend_from_slice(&f.rgba8);
    }
    out
}

/// Read a video answer, checking its header against `limits` and the video
/// bounds before reading a single frame.
fn read_video_answer(
    mut r: impl Read,
    limits: ImportLimits,
) -> Result<Answer<VideoWindow>, AnswerError> {
    let m = |_| AnswerError::Malformed;
    if !read_header(&mut r).map_err(m)? {
        return Err(AnswerError::Malformed);
    }
    match read_array::<1>(&mut r).map_err(m)?[0] {
        0 => {}
        1 => {
            let limit = read_array::<1>(&mut r).map_err(m)?[0] == 1;
            let len = read_u32(&mut r).map_err(m)? as usize;
            if len > MAX_MESSAGE {
                return Err(AnswerError::Malformed);
            }
            let mut text = vec![0; len];
            r.read_exact(&mut text).map_err(m)?;
            let text = String::from_utf8_lossy(&text).into_owned();
            return Ok(Answer::Refused(if limit {
                CodecError::LimitExceeded(text)
            } else {
                CodecError::Unsupported(text)
            }));
        }
        _ => return Err(AnswerError::Malformed),
    }
    let width = read_u32(&mut r).map_err(m)?;
    let height = read_u32(&mut r).map_err(m)?;
    let codec = match read_array::<1>(&mut r).map_err(m)?[0] {
        0 => VideoCodec::H264,
        1 => VideoCodec::Av1,
        _ => return Err(AnswerError::Malformed),
    };
    let total = read_u32(&mut r).map_err(m)? as usize;
    let first = read_u32(&mut r).map_err(m)? as usize;
    let count = read_u32(&mut r).map_err(m)? as usize;
    // The bounds, on the header, before any frame is read or allocated.
    let limit = |text: String| AnswerError::Limit(CodecError::LimitExceeded(text));
    if total > video::MAX_VIDEO_FRAMES {
        return Err(limit(format!(
            "the video decode worker answered a {total}-frame video, more than a video layer holds"
        )));
    }
    if width == 0 || height == 0 || count == 0 || first.saturating_add(count) > total {
        return Err(AnswerError::Malformed);
    }
    if width > limits.max_width
        || height > limits.max_height
        || u64::from(width) * u64::from(height) > limits.max_pixels
    {
        return Err(limit(format!(
            "the video decode worker answered a {width}x{height} video, past the limits"
        )));
    }
    let frame_bytes = u64::from(width) * u64::from(height) * 4;
    if count > video::MAX_VIDEO_FRAMES
        || frame_bytes * count as u64 > video::max_video_bytes(limits)
    {
        return Err(limit(format!(
            "the video decode worker answered {count} frames of {width}x{height}, more than a \
             video layer holds"
        )));
    }
    let mut durations = Vec::with_capacity(total);
    for _ in 0..total {
        durations.push(read_u32(&mut r).map_err(m)?);
    }
    let mut frames = Vec::with_capacity(count);
    for &duration_ms in &durations[first..first + count] {
        let mut rgba8 = vec![0u8; frame_bytes as usize];
        r.read_exact(&mut rgba8).map_err(m)?;
        frames.push(VideoFrame { rgba8, duration_ms });
    }
    // Nothing may follow the frames.
    if r.read(&mut [0u8; 1]).map_err(m)? != 0 {
        return Err(AnswerError::Malformed);
    }
    Ok(Answer::Decoded(VideoWindow {
        width,
        height,
        codec,
        durations_ms: durations,
        first,
        frames,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const AVIF_10BIT: &[u8] =
        include_bytes!("../../raster/src/formats/testdata/w15a_quadrants_16x12_10bit_alpha.avif");
    const HEIC: &[u8] =
        include_bytes!("../../raster/src/formats/testdata/w15a_grey_64x128_irot90_alpha.heic");

    /// The worker body, over in-memory streams: what the parent reads back
    /// is what the in-process decoder produced, sample for sample.
    fn round_trip(kind: &str, bytes: &[u8], limits: ImportLimits) -> Result<Answer, AnswerError> {
        let mut out = Vec::new();
        let code = worker_main(
            Some(kind.into()),
            Cursor::new(encode_request(bytes, limits)),
            &mut out,
        );
        assert_eq!(code, 0);
        read_answer(Cursor::new(out), limits)
    }

    #[test]
    fn a_decoded_answer_carries_the_surface_through_the_protocol() {
        for (kind, file, heif_kind) in [
            ("avif", AVIF_10BIT, HeifKind::Avif),
            ("heic", HEIC, HeifKind::Heic),
        ] {
            let want =
                heif::decode_in_this_process(heif_kind, file, ImportLimits::default()).unwrap();
            let Ok(Answer::Decoded(got)) = round_trip(kind, file, ImportLimits::default()) else {
                panic!("{kind}: not decoded");
            };
            assert_eq!((got.width, got.height), (want.width, want.height), "{kind}");
            assert_eq!(got.pixels, want.pixels, "{kind}");
            assert_eq!(got.color_space, want.color_space, "{kind}");
        }
        // The 10-bit AVIF arrives as 16-bit samples, the HEIC turned by its
        // `irot` (64x128 coded, 128x64 shown).
        let Ok(Answer::Decoded(s)) = round_trip("avif", AVIF_10BIT, ImportLimits::default()) else {
            unreachable!()
        };
        assert_eq!(s.format(), PixelFormat::Rgba16);
        let Ok(Answer::Decoded(s)) = round_trip("heic", HEIC, ImportLimits::default()) else {
            unreachable!()
        };
        assert_eq!((s.width, s.height), (128, 64));
    }

    #[test]
    fn a_refusal_crosses_as_a_refusal_and_an_icc_profile_as_the_space() {
        let Ok(Answer::Refused(err)) = round_trip("avif", b"not an avif", ImportLimits::default())
        else {
            panic!("garbage is refused");
        };
        assert!(err.to_string().contains("AVIF"), "{err}");
        let tight = ImportLimits {
            max_pixels: 10,
            ..ImportLimits::default()
        };
        let Ok(Answer::Refused(err)) = round_trip("heic", HEIC, tight) else {
            panic!("over the limit is refused");
        };
        assert!(matches!(err, CodecError::LimitExceeded(_)), "{err}");
        let surface = DecodedSurface {
            width: 1,
            height: 1,
            pixels: SurfacePixels::Rgba8(vec![1, 2, 3, 4]),
            color_space: raster::icc_profile_space(b"icc"),
            icc_profile: Some(b"icc".to_vec()),
            source_format: raster::ImportFormat::Avif,
        };
        let bytes = encode_answer(&Ok(surface.clone()));
        let Ok(Answer::Decoded(back)) = read_answer(Cursor::new(bytes), ImportLimits::default())
        else {
            panic!("decoded");
        };
        assert_eq!(back.color_space, surface.color_space);
        assert_eq!(back.icc_profile, surface.icc_profile);
    }

    /// The header is checked against the limits before a pixel is read:
    /// an answer claiming 4 billion by 4 billion pixels, followed by no
    /// pixels at all, is a limit error, not an allocation.
    #[test]
    fn an_oversized_header_is_refused_before_reading_pixels() {
        let mut out = Vec::new();
        assert_eq!(
            worker_main(
                Some("test-oversize".into()),
                Cursor::new(encode_request(b"x", ImportLimits::default())),
                &mut out
            ),
            0
        );
        assert!(matches!(
            read_answer(Cursor::new(out), ImportLimits::default()),
            Err(AnswerError::Limit(CodecError::LimitExceeded(_)))
        ));
        // A truncated answer, and trailing bytes, are malformed.
        let good = encode_answer(&Ok(DecodedSurface {
            width: 2,
            height: 1,
            pixels: SurfacePixels::Rgba8(vec![9; 8]),
            color_space: color::ColorSpace::Srgb,
            icc_profile: None,
            source_format: raster::ImportFormat::Avif,
        }));
        for cut in 0..good.len() {
            assert!(matches!(
                read_answer(Cursor::new(&good[..cut]), ImportLimits::default()),
                Err(AnswerError::Malformed)
            ));
        }
        let mut long = good.clone();
        long.push(0);
        assert!(matches!(
            read_answer(Cursor::new(long), ImportLimits::default()),
            Err(AnswerError::Malformed)
        ));
    }

    #[test]
    fn an_unknown_kind_or_a_bad_request_is_not_a_decode() {
        assert_eq!(
            worker_main(Some("gif".into()), Cursor::new(Vec::new()), Vec::new()),
            2
        );
        assert_eq!(worker_main(None, Cursor::new(Vec::new()), Vec::new()), 2);
        // A request whose file is larger than the limits allow is refused
        // before it is read.
        let limits = ImportLimits {
            max_alloc_bytes: 1,
            ..ImportLimits::default()
        };
        let Ok(Answer::Refused(err)) = round_trip("avif", AVIF_10BIT, limits) else {
            panic!("refused");
        };
        assert!(err.to_string().contains("larger than"), "{err}");
    }

    /// Spawning something that is not a worker (here: a path that does not
    /// exist) fails with the reason, and nothing is installed by the test
    /// process on its own (the application's `main` calls [`install`]).
    #[test]
    fn a_missing_worker_executable_is_reported() {
        let err = decode_in_worker(
            Path::new("/no/such/raster-studio-worker"),
            HeifKind::Avif,
            AVIF_10BIT,
            ImportLimits::default(),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(err.to_string().contains("could not start"), "{err}");
    }

    /// W16-M: the video kind over in-memory streams: an exported MP4's
    /// frames cross the protocol; garbage crosses as a refusal; a header
    /// past the bounds is a limit error before any frame is read; a
    /// truncated answer is malformed.
    #[test]
    fn a_video_answer_carries_the_frames_and_its_header_is_bounded() {
        let px: Vec<Vec<u8>> = (0..2u8)
            .map(|i| [40 + i * 100, 90, 160, 255].repeat(32 * 32))
            .collect();
        let frames: Vec<Mp4Frame<'_>> = px
            .iter()
            .map(|p| Mp4Frame {
                rgba8: p,
                duration_ms: 120,
            })
            .collect();
        let mp4 = encode_mp4(32, 32, &frames, 90).unwrap();
        let limits = ImportLimits::default();
        let run = |bytes: &[u8]| {
            let mut out = Vec::new();
            let code = worker_main(
                Some(VIDEO_ARG.into()),
                Cursor::new(encode_request(bytes, limits)),
                &mut out,
            );
            assert_eq!(code, 0);
            out
        };
        let answer = run(&mp4);
        let Ok(Answer::Decoded(video)) = read_video_answer(Cursor::new(&answer), limits) else {
            panic!("decoded");
        };
        assert_eq!((video.width, video.height, video.frames.len()), (32, 32, 2));
        assert_eq!((video.first, video.durations_ms.len()), (0, 2));
        assert_eq!(video.frames[1].duration_ms, 120);
        // A window: the second frame only, with the whole track's timing.
        let mut out = Vec::new();
        let code = worker_main(
            Some(format!("{VIDEO_WINDOW_ARG_PREFIX}1:1").into()),
            Cursor::new(encode_request(&mp4, limits)),
            &mut out,
        );
        assert_eq!(code, 0);
        let Ok(Answer::Decoded(window)) = read_video_answer(Cursor::new(&out), limits) else {
            panic!("a window decoded");
        };
        assert_eq!((window.first, window.frames.len()), (1, 1));
        assert_eq!(window.durations_ms, vec![120, 120]);
        assert_eq!(window.frames[0].rgba8, video.frames[1].rgba8);
        assert_eq!(video_window_of_arg("video-window:3:x"), None);
        assert_eq!(video_window_of_arg("avif"), None);
        let Ok(Answer::Refused(err)) = read_video_answer(Cursor::new(run(b"garbage")), limits)
        else {
            panic!("refused");
        };
        assert!(err.to_string().contains("MP4"), "{err}");
        for cut in 0..answer.len().min(64) {
            assert!(matches!(
                read_video_answer(Cursor::new(&answer[..cut]), limits),
                Err(AnswerError::Malformed)
            ));
        }
        // Past the frame count, and (1000 frames of 1080p, 8 GB) past the
        // decoded-bytes budget: refused on the header alone.
        for count in [u32::MAX, 1000] {
            let mut header = MAGIC.to_vec();
            header.push(VERSION);
            header.push(0);
            header.extend_from_slice(&1920u32.to_le_bytes());
            header.extend_from_slice(&1080u32.to_le_bytes());
            header.push(0);
            header.extend_from_slice(&count.to_le_bytes());
            header.extend_from_slice(&0u32.to_le_bytes());
            header.extend_from_slice(&count.to_le_bytes());
            assert!(
                matches!(
                    read_video_answer(Cursor::new(header), limits),
                    Err(AnswerError::Limit(CodecError::LimitExceeded(_)))
                ),
                "{count} frames"
            );
        }
    }
}
