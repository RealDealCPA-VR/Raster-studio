//! W16-M: video layers (photopea.com/learn/video, "Video Layers"): an MP4
//! opened as a document, or added to the timeline (Add Media), becomes a
//! video layer whose picture is the video's frame at the playhead.
//!
//! Declared from [`crate::timeline`] (with `#[path]`) and reached as
//! `app_shell::timeline::video_layers`.
//!
//! # The route
//!
//! 1. The file is read (at most [`MAX_VIDEO_FILE_BYTES`]) and decoded **in
//!    the decode worker** ([`crate::dialogs::decode_worker`], kind
//!    `video-window`): OpenH264 and the AV1 decoder are native code, so a
//!    crash on a damaged file is an error here, not the editor closing. The
//!    worker is what `studio-desktop`'s `main` installs
//!    ([`install_video_decoder`]); with none installed a video is refused by
//!    name.
//! 2. Frames are decoded **on demand**, [`DECODE_WINDOW_FRAMES`] at a time:
//!    opening or adding a video decodes the first window only (and learns
//!    every frame's timing); a playhead move to a frame not decoded yet
//!    ([`Editor::seek_timeline`] asks [`Editor::load_video_frames_at`])
//!    decodes the window holding it; an export decodes what it renders
//!    ([`fill_video_frames`], on its own copy of the document). Each decoded
//!    frame is cut into tiles and filed in the document's tile store
//!    (content-addressed, so a frame that repeats costs nothing), and stays
//!    there, cached, for the session.
//! 3. One undoable step adds a raster layer named after the file, a
//!    [`editor_core::timeline::VideoClip`] naming it (start at the playhead,
//!    every frame's duration, the decoded frames' tiles), a bar covering the
//!    video, and the frame at the playhead on the layer. Timeline mode is
//!    turned on, and the timeline is made long enough to hold the video.
//!
//! From then on the timeline shows the frame at `t` ([`editor_core::timeline::seek`]),
//! an export renders it ([`crate::timeline::render_at`] composites
//! [`editor_core::timeline::document_at`]), and the bar's ends trim it.
//!
//! # Bounds
//!
//! The decoder's own (`raster::codec::formats::mp4::video`): at most 1000
//! frames in the track, and 1 GiB of decoded RGBA8 in one window (an export
//! decodes the whole track as one window), checked before a frame is decoded
//! and again on the worker's answer before a frame is read. A window after
//! the first is decoded from the stream's start (a frame depends on the ones
//! before it), so a late window costs the decode time of the frames before it.
//!
//! # Not here
//!
//! Audio (no permissively licensed pure-Rust AAC decoder; MP4 export writes
//! no audio track).

use std::io::Read;
use std::path::Path;
use std::sync::RwLock;

use editor_core::pixels::{TileDelta, TileEdit, TileMap};
use editor_core::timeline::{self, VideoClip, DURATION_RANGE_MS, FPS_RANGE};
use editor_core::{Command, Document};
use layer_model::{Layer, LayerId};
use raster::codec::formats::mp4::{self, video::VideoWindow};
use raster::{CodecError, ImportLimits};

use crate::editor::Editor;
use crate::DocumentId;

/// The largest video file this route reads (1 GiB).
pub const MAX_VIDEO_FILE_BYTES: u64 = 1 << 30;

/// How many frames one on-demand decode brings in: the window holding the
/// frame the playhead needs.
pub const DECODE_WINDOW_FRAMES: usize = 32;

/// What decodes the frames `first..first + count` of a video's bytes for a
/// video layer.
pub type VideoDecoder = fn(&[u8], ImportLimits, usize, usize) -> Result<VideoWindow, CodecError>;

static DECODER: RwLock<Option<VideoDecoder>> = RwLock::new(None);

/// Decode every video layer's media with `decoder`: the decode worker, which
/// [`crate::dialogs::decode_worker::install`] installs.
pub fn install_video_decoder(decoder: VideoDecoder) {
    *DECODER.write().unwrap_or_else(|e| e.into_inner()) = Some(decoder);
}

fn decode(bytes: &[u8], first: usize, count: usize) -> Result<VideoWindow, CodecError> {
    let decoder = *DECODER.read().unwrap_or_else(|e| e.into_inner());
    match decoder {
        Some(decode) => decode(bytes, ImportLimits::default(), first, count),
        None => Err(CodecError::Unsupported(
            "video layers are decoded in the decode worker process, which this process has not \
             started"
                .into(),
        )),
    }
}

/// Whether `path` holds a video file (by its content: an MP4 / MOV / M4V
/// `ftyp`, Matroska / WebM, AVI).
pub fn is_video_path(path: &Path) -> bool {
    let mut head = [0u8; 64];
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => filled += n,
        }
    }
    mp4::looks_like_video(&head[..filled])
}

/// Read `path` (bounded by [`MAX_VIDEO_FILE_BYTES`]) and decode its frames
/// `first..first + count`.
fn read_video(path: &Path, first: usize, count: usize) -> Result<VideoWindow, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_VIDEO_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() as u64 > MAX_VIDEO_FILE_BYTES {
        return Err(format!(
            "{} is larger than the {} MiB a video layer reads",
            path.display(),
            MAX_VIDEO_FILE_BYTES >> 20
        ));
    }
    decode(&bytes, first, count).map_err(|e| format!("{}: {e}", path.display()))
}

/// One tile map per frame of the track: the window's frames' tiles, filed
/// in `tiles`; an empty map for every frame outside the window (not decoded
/// yet).
fn frame_tiles(
    window: &VideoWindow,
    tiles: &mut compositor::MemoryTileSource,
) -> Result<Vec<TileMap>, String> {
    let mut maps = vec![TileMap::default(); window.durations_ms.len()];
    for (k, f) in window.frames.iter().enumerate() {
        let grid = raster::TileGrid::from_rgba8(window.width, window.height, &f.rgba8)
            .map_err(|e| e.to_string())?;
        let edits: Vec<TileEdit> = grid
            .iter()
            .map(|(coord, tile)| TileEdit::set(coord, tiles.insert_bytes(tile.data().to_vec())))
            .collect();
        let mut map = TileMap::default();
        map.apply_delta(&TileDelta::new(edits).map_err(|e| e.to_string())?);
        let slot = maps
            .get_mut(window.first + k)
            .ok_or("the decoded window lies past the video's frames")?;
        *slot = map;
    }
    Ok(maps)
}

/// File `window`'s frames into `clip` (keeping the frames it has), when the
/// window is of the clip's media (same size, frame count and timing).
/// Answers how many frames were filed.
fn file_window(
    clip: &mut VideoClip,
    window: &VideoWindow,
    tiles: &mut compositor::MemoryTileSource,
) -> Result<usize, String> {
    if (clip.width, clip.height) != (window.width, window.height)
        || clip.durations_ms != window.durations_ms
    {
        return Err(format!(
            "{} changed since it was added (its size or length differs)",
            clip.source
        ));
    }
    let maps = frame_tiles(window, tiles)?;
    clip.frames
        .resize(clip.durations_ms.len(), TileMap::default());
    let mut filed = 0;
    for (i, map) in maps.into_iter().enumerate() {
        if !map.is_empty() {
            clip.frames[i] = map;
            filed += 1;
        }
    }
    Ok(filed)
}

/// The first frame of the decode window holding frame `index`.
fn window_start(index: usize) -> usize {
    index / DECODE_WINDOW_FRAMES * DECODE_WINDOW_FRAMES
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Video".to_string())
}

/// The frame rate a video suggests: its shortest frame's, in the range the
/// timeline accepts.
fn fps_of(window: &VideoWindow) -> u32 {
    let shortest = window
        .durations_ms
        .iter()
        .map(|d| (*d).max(1))
        .min()
        .unwrap_or(1000);
    (1000 / shortest).clamp(*FPS_RANGE.start(), *FPS_RANGE.end())
}

fn media_ms(window: &VideoWindow) -> u64 {
    window.durations_ms.iter().map(|d| u64::from(*d)).sum()
}

/// Decode, into `doc`'s clips and `tiles`, every frame an export of `doc`
/// renders that is not decoded yet: each clip whose frames are not all
/// here is read again from its source, whole, in one window. For an
/// export's own copy of the document (the export runs off the interaction
/// thread). A clip whose source is gone or changed keeps what it has.
pub fn fill_video_frames(doc: &mut Document, tiles: &mut compositor::MemoryTileSource) {
    for clip in doc.timeline.videos.iter_mut() {
        if clip.frames_loaded() || clip.source.is_empty() {
            continue;
        }
        if let Ok(window) = read_video(Path::new(&clip.source), 0, clip.durations_ms.len()) {
            let _ = file_window(clip, &window, tiles);
        }
    }
}

/// Sources whose on-demand decode failed this session, so a missing or
/// damaged source is not read again on every playhead move.
static FAILED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

impl Editor {
    /// File > Open of a video: a new document the video's size holding one
    /// video layer, Timeline mode on, as long as the video and at its frame
    /// rate. Opening is not an edit: the document is clean with nothing to
    /// undo. The file is decoded before anything changes, so a damaged or
    /// refused file opens nothing. Only the first [`DECODE_WINDOW_FRAMES`]
    /// frames are decoded now; the rest as the playhead reaches them.
    pub fn open_video_path(&mut self, path: &Path) -> Result<DocumentId, String> {
        let window = read_video(path, 0, DECODE_WINDOW_FRAMES)?;
        let title = file_name(path);
        self.new_document_with(
            window.width,
            window.height,
            &title,
            crate::import::BlankBackground::Transparent,
        )
        .map_err(|e| e.to_string())?;
        let blank = self
            .active()
            .and_then(|o| o.document.layers.root().first().copied());
        let fps = fps_of(&window);
        let layer = self.add_video_layer(path, &window, Some(fps), blank)?;
        let open = self.active_mut().ok_or("No document is open")?;
        let _ = open.document.set_active_layer(Some(layer));
        open.history.clear();
        open.document.mark_saved();
        open.invalidate_all();
        let id = open.id();
        self.set_status(format!(
            "Opened {} as a video layer ({} frames)",
            path.display(),
            window.durations_ms.len()
        ));
        Ok(id)
    }

    /// The timeline's Add Media: `path`'s video as a new video layer in the
    /// active document, starting at the playhead. One undo step. Only the
    /// first [`DECODE_WINDOW_FRAMES`] frames are decoded now.
    pub fn add_media_path(&mut self, path: &Path) -> Result<String, String> {
        if self.active().is_none() {
            return Err("No document is open".into());
        }
        let window = read_video(path, 0, DECODE_WINDOW_FRAMES)?;
        self.add_video_layer(path, &window, None, None)?;
        let status = format!(
            "Added {} ({} frames, {} ms)",
            file_name(path),
            window.durations_ms.len(),
            media_ms(&window)
        );
        self.set_status(status.clone());
        Ok(status)
    }

    /// W16-M: the open routes' branch for a video file (File > Open, a
    /// drag-and-drop, Open Recent, the command line): `None` when `path` is
    /// not a video, otherwise [`Self::open_video_path`]'s outcome as an
    /// action result. Meant for `Editor::open_resource_file`'s routing
    /// table, ahead of the image decode that refuses a video by name.
    pub fn open_video_file(
        &mut self,
        path: &Path,
    ) -> Option<Result<crate::editor::Effect, crate::editor::ActionError>> {
        if !is_video_path(path) {
            return None;
        }
        Some(
            self.open_video_path(path)
                .map(|_| crate::editor::Effect::DocumentSet)
                .map_err(|reason| crate::editor::ActionError::Failed {
                    action: crate::action::Action::Open,
                    reason,
                }),
        )
    }

    /// W16-M: File > Open and Place (Place Embedded / Linked) of a video
    /// file, which the timeline's Add Media asks for too (Photopea: "File -
    /// Open and Place" of a media file makes a video layer): `None` when
    /// `path` is not a video, otherwise [`Self::add_media_path`]'s outcome.
    /// Meant for the top of `Editor::place_path`.
    pub fn place_video_file(&mut self, path: &Path) -> Option<Result<String, String>> {
        is_video_path(path).then(|| self.add_media_path(path))
    }

    /// Add `window` (the track's timing and its first decoded frames) as a
    /// video layer to the active document in one step. For a document made
    /// for the video, `fps` sets the frame rate (and the length becomes the
    /// video's) and `replace` is its blank layer, deleted.
    fn add_video_layer(
        &mut self,
        path: &Path,
        window: &VideoWindow,
        fps: Option<u32>,
        replace: Option<LayerId>,
    ) -> Result<LayerId, String> {
        let name = file_name(path);
        let command = {
            let open = self.active_mut().ok_or("No document is open")?;
            let frames = frame_tiles(window, &mut open.tiles)?;
            let layer = Layer::raster(&name);
            let id = layer.id;
            let create = Command::create_layer(layer);
            // The document with the layer in it, for the timeline edit to
            // evaluate against (it paints the frame at the playhead).
            let mut with_layer = open.document.clone();
            create
                .apply(&mut with_layer)
                .map_err(|e| format!("Add Media: {e}"))?;
            let mut t = with_layer.timeline.clone();
            t.enabled = true;
            if let Some(fps) = fps {
                t.fps = fps;
            }
            let start = t.current_ms;
            let media = u32::try_from(media_ms(window)).unwrap_or(u32::MAX);
            let end = start.saturating_add(media);
            // A document made for the video is as long as it; an existing
            // timeline only grows to hold it.
            let length = if fps.is_some() {
                end
            } else {
                t.duration_ms.max(end)
            };
            t.duration_ms = length.clamp(*DURATION_RANGE_MS.start(), *DURATION_RANGE_MS.end());
            let track = t.track_mut(id);
            track.in_ms = start;
            track.out_ms = end;
            t.videos.push(VideoClip {
                layer: id,
                name: name.clone(),
                source: path.display().to_string(),
                width: window.width,
                height: window.height,
                start_ms: start,
                durations_ms: window.durations_ms.clone(),
                frames,
            });
            let edit = timeline::set_timeline(&with_layer, "Add Media", t)
                .ok_or("Add Media: the timeline did not change")?;
            let mut commands = vec![create, edit];
            if let Some(blank) = replace {
                commands.push(Command::DeleteLayer { layer_id: blank });
            }
            (
                Command::Transaction {
                    label: "Add Media".to_string(),
                    commands,
                },
                id,
            )
        };
        let (command, id) = command;
        self.apply_command(command);
        let open = self.active_mut().ok_or("No document is open")?;
        if !open.document.timeline.videos.iter().any(|c| c.layer == id) {
            return Err(format!("Add Media: {name} could not be added"));
        }
        open.invalidate_all();
        Ok(id)
    }

    /// Decode, on demand, the frame each video layer of the active document
    /// shows at `t_ms` when it is not decoded yet: the
    /// [`DECODE_WINDOW_FRAMES`]-frame window holding it, read from the
    /// clip's source through the decode worker, its tiles filed and kept.
    /// History free, like the playhead. A source whose decode fails is not
    /// read again this session (the layer keeps the frame it shows).
    /// Answers how many frames were decoded.
    pub fn load_video_frames_at(&mut self, t_ms: u32) -> usize {
        let wanted: Vec<(usize, String, usize)> = match self.active() {
            Some(open) => open
                .document
                .timeline
                .videos
                .iter()
                .enumerate()
                .filter(|(_, c)| !c.source.is_empty())
                .filter_map(|(i, c)| {
                    let frame = c.frame_at(t_ms)?;
                    (!c.frame_loaded(frame)).then(|| (i, c.source.clone(), window_start(frame)))
                })
                .collect(),
            None => return 0,
        };
        let mut filed = 0;
        for (index, source, first) in wanted {
            if FAILED
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&source)
            {
                continue;
            }
            let result =
                read_video(Path::new(&source), first, DECODE_WINDOW_FRAMES).and_then(|window| {
                    let open = self.active_mut().ok_or("No document is open")?;
                    let clip = open
                        .document
                        .timeline
                        .videos
                        .get_mut(index)
                        .ok_or("the video layer is gone")?;
                    file_window(clip, &window, &mut open.tiles)
                });
            match result {
                Ok(n) => filed += n,
                Err(_) => FAILED
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(source),
            }
        }
        filed
    }

    /// Read every frame again for each video layer of the active document
    /// whose frames are not all decoded (a document reopened from disk: the
    /// frames' tiles are not saved), from each clip's source file, whole.
    /// History free, like the playhead. Answers how many clips were loaded;
    /// a clip whose file is gone or changed size / length keeps its frames.
    pub fn reload_video_frames(&mut self) -> usize {
        let wanted: Vec<(usize, String, usize)> = match self.active() {
            Some(open) => open
                .document
                .timeline
                .videos
                .iter()
                .enumerate()
                .filter(|(_, c)| !c.frames_loaded() && !c.source.is_empty())
                .map(|(i, c)| (i, c.source.clone(), c.durations_ms.len()))
                .collect(),
            None => return 0,
        };
        let mut loaded = 0;
        for (index, source, count) in wanted {
            let Ok(window) = read_video(Path::new(&source), 0, count) else {
                continue;
            };
            let Some(open) = self.active_mut() else {
                break;
            };
            let Some(clip) = open.document.timeline.videos.get_mut(index) else {
                continue;
            };
            if file_window(clip, &window, &mut open.tiles).is_ok() {
                loaded += 1;
            }
        }
        loaded
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use raster::codec::formats::mp4::{video, Mp4Frame};

    /// The in-process decoder, for these tests (the application installs
    /// the worker; `studio-desktop`'s tests drive that one).
    pub(crate) fn use_in_process_decoder() {
        install_video_decoder(video::decode_window_in_this_process);
    }

    fn editor(dir: &Path) -> Editor {
        Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        )
    }

    /// Frame `i` of a `w x h` test clip: a flat colour on the left half
    /// that differs per frame, grey on the right.
    pub(crate) fn clip_frame(w: u32, h: u32, i: usize) -> Vec<u8> {
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

    /// An H.264 MP4 the exporter wrote: `durations.len()` frames.
    pub(crate) fn write_clip(path: &Path, w: u32, h: u32, durations: &[u32]) -> Vec<Vec<u8>> {
        let px: Vec<Vec<u8>> = (0..durations.len()).map(|i| clip_frame(w, h, i)).collect();
        let frames: Vec<Mp4Frame<'_>> = px
            .iter()
            .zip(durations)
            .map(|(p, d)| Mp4Frame {
                rgba8: p,
                duration_ms: *d,
            })
            .collect();
        std::fs::write(path, mp4::encode(w, h, &frames, 90).unwrap()).unwrap();
        px
    }

    fn close(a: [u8; 4], b: [u8; 4], tolerance: i32) -> bool {
        a.iter()
            .zip(b)
            .all(|(x, y)| (i32::from(*x) - i32::from(y)).abs() <= tolerance)
    }

    fn pixel(rgba: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    /// An H.264 MP4 the exporter wrote opens as a video layer: one layer,
    /// a clip with the file's frame count and durations, Timeline mode on,
    /// and the frame composited at `t` is the video's frame at `t`.
    #[test]
    fn an_exported_h264_mp4_opens_as_a_video_layer_and_shows_the_frame_at_t() {
        use_in_process_decoder();
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (48, 32);
        let path = dir.path().join("clip.mp4");
        let px = write_clip(&path, w, h, &[200, 200, 400]);
        assert!(is_video_path(&path));
        let mut ed = editor(dir.path());
        ed.open_video_path(&path).unwrap();
        let open = ed.active().unwrap();
        let doc = &open.document;
        assert_eq!((doc.width(), doc.height()), (w, h));
        assert_eq!(doc.layers.root().len(), 1, "one layer: the video");
        let layer = doc.layers.root()[0];
        assert_eq!(doc.layers.get(layer).unwrap().name, "clip.mp4");
        assert!(doc.timeline.enabled);
        let clip = &doc.timeline.videos[0];
        assert_eq!(clip.layer, layer);
        assert_eq!(clip.frames.len(), 3, "the file's frame count");
        assert_eq!(clip.durations_ms, vec![200, 200, 400]);
        assert_eq!(doc.timeline.duration_ms, 800);
        assert_eq!(doc.timeline.fps, 5);
        let track = doc.timeline.track(layer).unwrap();
        assert_eq!((track.in_ms, track.out_ms), (0, 800));
        assert!(!open.is_dirty(), "opening is not an edit");

        for (t, frame) in [(0, 0), (199, 0), (250, 1), (500, 2), (799, 2)] {
            let rgba = crate::timeline::render_at(doc, &open.tiles, t).unwrap();
            let got = pixel(&rgba, w, 6, 16);
            let want = pixel(&px[frame], w, 6, 16);
            assert!(
                close(got, want, 8),
                "t {t}: {got:?} vs frame {frame} {want:?}"
            );
            assert!(close(pixel(&rgba, w, 40, 16), [128, 128, 128, 255], 8));
        }
        // The playhead shows it on the layer itself (the canvas).
        assert!(ed.seek_timeline(500));
        let open = ed.active().unwrap();
        let at = editor_core::timeline::document_at(&open.document, 500);
        assert_eq!(
            open.document.layer_tiles(layer),
            at.layer_tiles(layer),
            "the seek put the frame at 500 ms on the layer"
        );
    }

    /// Add Media puts the video at the playhead of an open document as one
    /// undo step; trimming the bar hides it outside the trim; export writes
    /// the video's frames into the MP4.
    #[test]
    fn add_media_places_the_video_at_the_playhead_trims_and_exports() {
        use_in_process_decoder();
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (32, 32);
        let path = dir.path().join("media.mp4");
        let px = write_clip(&path, w, h, &[250, 250]);
        let mut ed = editor(dir.path());
        ed.new_document_with(
            w,
            h,
            "Scene",
            crate::import::BlankBackground::Solid {
                rgba8: [0, 0, 255, 255],
                depth: raster::BitDepth::Eight,
            },
        )
        .unwrap();
        // Timeline on, 2 s long, playhead at 1000 ms.
        let doc = &ed.active().unwrap().document;
        let mut t = doc.timeline.clone();
        t.enabled = true;
        t.duration_ms = 2000;
        t.fps = 4;
        let c = timeline::set_timeline(doc, "Timeline", t).unwrap();
        ed.apply_command(c);
        assert!(ed.seek_timeline(1000));
        ed.add_media_path(&path).unwrap();
        let open = ed.active().unwrap();
        let doc = &open.document;
        let clip = doc.timeline.videos[0].clone();
        assert_eq!(clip.start_ms, 1000, "added at the playhead");
        let track = doc.timeline.track(clip.layer).unwrap();
        assert_eq!((track.in_ms, track.out_ms), (1000, 1500));
        let px_at = |ed: &Editor, t| {
            let open = ed.active().unwrap();
            let rgba = crate::timeline::render_at(&open.document, &open.tiles, t).unwrap();
            pixel(&rgba, w, 4, 4)
        };
        assert_eq!(
            px_at(&ed, 500),
            [0, 0, 255, 255],
            "before the bar: the scene"
        );
        assert!(close(px_at(&ed, 1100), pixel(&px[0], w, 4, 4), 8));
        assert!(close(px_at(&ed, 1300), pixel(&px[1], w, 4, 4), 8));
        // Trim the start to 1250 ms: the first frame is cut away and the
        // second stays where it was.
        let c =
            timeline::set_in_out(&ed.active().unwrap().document, clip.layer, 1250, 1500).unwrap();
        ed.apply_command(c);
        assert_eq!(px_at(&ed, 1100), [0, 0, 255, 255], "trimmed away");
        assert!(close(px_at(&ed, 1300), pixel(&px[1], w, 4, 4), 8));
        // Export As MP4 renders the video layer in its frames.
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let job = ui::dialogs::ExportJob {
            base_name: "scene".to_string(),
            entries: vec![ui::dialogs::ExportEntry::new(
                "",
                raster::ExportFormat::Mp4(90),
                1.0,
            )],
        };
        ed.request_export(job, out.clone());
        ed.poll_exports();
        let bytes = std::fs::read(out.join("scene.mp4")).unwrap();
        let back = video::decode_in_this_process(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(back.frames.len(), 8, "2 s at 4 fps");
        // Frames at 0.. 250 ms steps: 1250 and 1500.. -> index 5 is 1250 ms.
        assert!(close(
            pixel(&back.frames[1].rgba8, w, 4, 4),
            [0, 0, 255, 255],
            10
        ));
        assert!(
            close(
                pixel(&back.frames[5].rgba8, w, 4, 4),
                pixel(&px[1], w, 4, 4),
                10
            ),
            "{:?}",
            pixel(&back.frames[5].rgba8, w, 4, 4)
        );
        // Undo takes the trim, then the whole Add Media, back.
        let open = ed.active_mut().unwrap();
        assert_eq!(open.document.layers.root().len(), 2);
        assert!(open.undo().unwrap(), "the trim");
        assert!(open.undo().unwrap(), "Add Media");
        assert_eq!(open.document.layers.root().len(), 1);
        assert!(open.document.timeline.videos.is_empty());
    }

    /// A malformed MP4 is an error and changes nothing; the editor and its
    /// open document are still there.
    #[test]
    fn a_malformed_mp4_is_an_error_and_opens_nothing() {
        use_in_process_decoder();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        write_clip(&path, 32, 32, &[100, 100]);
        let bytes = std::fs::read(&path).unwrap();
        let broken = dir.path().join("broken.mp4");
        std::fs::write(&broken, &bytes[..bytes.len() / 2]).unwrap();
        let mut ed = editor(dir.path());
        ed.open_video_path(&path).unwrap();
        let err = ed.open_video_path(&broken).unwrap_err();
        assert!(err.contains("MP4"), "{err}");
        assert_eq!(ed.documents().len(), 1, "nothing opened");
        let err = ed.add_media_path(&broken).unwrap_err();
        assert!(err.contains("MP4"), "{err}");
        assert_eq!(ed.active().unwrap().document.timeline.videos.len(), 1);
    }

    /// The routing branches answer `None` for a file that is not a video
    /// (it goes on to its own importer) and route a video to the video
    /// layer.
    #[test]
    fn the_routing_branches_take_videos_only() {
        use_in_process_decoder();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        write_clip(&path, 32, 32, &[100, 100]);
        let png = dir.path().join("still.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nnot really").unwrap();
        let mut ed = editor(dir.path());
        assert!(ed.open_video_file(&png).is_none());
        assert!(ed.place_video_file(&png).is_none());
        assert_eq!(
            ed.open_video_file(&path).unwrap().unwrap(),
            crate::editor::Effect::DocumentSet
        );
        assert!(ed.place_video_file(&path).unwrap().is_ok());
        assert_eq!(ed.active().unwrap().document.timeline.videos.len(), 2);
    }

    /// The real menu route: File > Open of an MP4 opens a video layer and
    /// Place Embedded (what the timeline's Add Media asks for) adds one.
    #[test]
    fn file_open_and_place_route_an_mp4_to_a_video_layer() {
        use_in_process_decoder();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        write_clip(&path, 32, 32, &[100, 100, 100]);
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new().opening(&path).placing(&path)),
        );
        use ui::menu::MenuAction;
        // File > Open is a shell action (the menu row maps to Action::Open).
        ed.dispatch(crate::action::Action::Open).unwrap();
        ed.poll_imports();
        let doc = &ed.active().expect("File > Open opened the MP4").document;
        assert_eq!(doc.timeline.videos.len(), 1, "a video layer");
        assert_eq!(doc.timeline.videos[0].frames.len(), 3);
        crate::menu_bridge::perform(MenuAction::PlaceEmbedded, &mut ed).unwrap();
        assert_eq!(ed.active().unwrap().document.timeline.videos.len(), 2);
    }

    /// A `w x h` H.264 clip of `n` frames of `ms` each whose left half is a
    /// different colour in every frame (any `n` up to 40).
    fn write_long_clip(path: &Path, w: u32, h: u32, n: usize, ms: u32) -> Vec<Vec<u8>> {
        let px: Vec<Vec<u8>> = (0..n)
            .map(|i| {
                let c = (i * 6) as u8;
                let mut p = Vec::with_capacity((w * h * 4) as usize);
                for _ in 0..h {
                    for x in 0..w {
                        if x < w / 2 {
                            p.extend_from_slice(&[c, 255 - c, 80, 255]);
                        } else {
                            p.extend_from_slice(&[128, 128, 128, 255]);
                        }
                    }
                }
                p
            })
            .collect();
        let frames: Vec<Mp4Frame<'_>> = px
            .iter()
            .map(|p| Mp4Frame {
                rgba8: p,
                duration_ms: ms,
            })
            .collect();
        std::fs::write(path, mp4::encode(w, h, &frames, 95).unwrap()).unwrap();
        px
    }

    /// Frames decode on demand: opening decodes the first window only; a
    /// playhead move to a frame past it decodes the window holding it, and
    /// the canvas and the render at that time show that frame.
    #[test]
    fn frames_decode_on_demand_for_the_timeline_time() {
        use_in_process_decoder();
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (32, 32);
        let path = dir.path().join("long.mp4");
        let px = write_long_clip(&path, w, h, 40, 50);
        let mut ed = editor(dir.path());
        ed.open_video_path(&path).unwrap();
        let clip = ed.active().unwrap().document.timeline.videos[0].clone();
        assert_eq!(clip.durations_ms.len(), 40, "every frame's timing");
        assert_eq!(clip.loaded_frames(), DECODE_WINDOW_FRAMES, "one window");
        assert!(!clip.frame_loaded(35));
        let t = 35 * 50 + 10;
        assert!(ed.seek_timeline(t));
        let open = ed.active().unwrap();
        let clip = &open.document.timeline.videos[0];
        assert!(clip.frame_loaded(35), "the seek decoded frame 35");
        assert_eq!(clip.loaded_frames(), 40, "the window 32..40");
        assert_eq!(
            open.document.layer_tiles(clip.layer),
            Some(&clip.frames[35]),
            "the canvas shows frame 35"
        );
        let rgba = crate::timeline::render_at(&open.document, &open.tiles, t).unwrap();
        let (got, want) = (pixel(&rgba, w, 6, 16), pixel(&px[35], w, 6, 16));
        assert!(close(got, want, 8), "{got:?} vs frame 35 {want:?}");
    }

    /// An export renders frames that were never decoded for the playhead:
    /// it decodes them into its own copy of the document.
    #[test]
    fn an_export_decodes_the_frames_it_renders() {
        use_in_process_decoder();
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (32, 32);
        let path = dir.path().join("long.mp4");
        let px = write_long_clip(&path, w, h, 40, 50);
        let mut ed = editor(dir.path());
        ed.open_video_path(&path).unwrap();
        let open = ed.active().unwrap();
        assert!(!open.document.timeline.videos[0].frame_loaded(36));
        let frames = crate::timeline::export_frames(&open.document, &open.tiles).unwrap();
        assert_eq!(frames.len(), 40, "2 s at 20 fps");
        let (got, want) = (pixel(&frames[36].rgba8, w, 6, 16), pixel(&px[36], w, 6, 16));
        assert!(close(got, want, 8), "{got:?} vs frame 36 {want:?}");
        assert_eq!(
            ed.active().unwrap().document.timeline.videos[0].loaded_frames(),
            DECODE_WINDOW_FRAMES,
            "the open document is left as it was"
        );
    }

    /// A reopened document (frames not saved) reads its frames again from
    /// the source on the next seek.
    #[test]
    fn a_reopened_document_reads_its_frames_again() {
        use_in_process_decoder();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        write_clip(&path, 32, 32, &[100, 100, 100]);
        let mut ed = editor(dir.path());
        ed.open_video_path(&path).unwrap();
        let project = dir.path().join("video.rstudio");
        ed.active_mut().unwrap().save_to(&project, "test").unwrap();
        let mut again = editor(dir.path());
        again.open_path(&project).unwrap();
        let clip = &again.active().unwrap().document.timeline.videos[0];
        assert_eq!(clip.durations_ms, vec![100, 100, 100]);
        assert!(!clip.frames_loaded());
        assert_eq!(again.reload_video_frames(), 1);
        assert!(again.active().unwrap().document.timeline.videos[0].frames_loaded());
    }
}
