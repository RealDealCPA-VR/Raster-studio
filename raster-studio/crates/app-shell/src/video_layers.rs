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
//!    `video`): OpenH264 and the AV1 decoder are native code, so a crash on
//!    a damaged file is an error here, not the editor closing. The worker is
//!    what `studio-desktop`'s `main` installs ([`install_video_decoder`]);
//!    with none installed a video is refused by name.
//! 2. Every decoded frame is cut into tiles and filed in the document's
//!    tile store (content-addressed, so a frame that repeats costs nothing).
//! 3. One undoable step adds a raster layer named after the file, a
//!    [`editor_core::timeline::VideoClip`] naming it (start at the playhead,
//!    every frame's duration and tiles), a bar covering the video, and the
//!    frame at the playhead on the layer. Timeline mode is turned on, and
//!    the timeline is made long enough to hold the video.
//!
//! From then on the timeline shows the frame at `t` ([`editor_core::timeline::seek`]),
//! an export renders it ([`crate::timeline::render_at`] composites
//! [`editor_core::timeline::document_at`]), and the bar's ends trim it.
//!
//! # Bounds
//!
//! The decoder's own (`raster::codec::formats::mp4::video`): at most 1000
//! frames and 1 GiB of decoded RGBA8, checked before a frame is decoded and
//! again on the worker's answer before a frame is read.
//!
//! # Not here
//!
//! Audio (no permissively licensed pure-Rust AAC decoder; MP4 export writes
//! no audio track), New Video Group, and frames decoded lazily per playhead
//! position (a video is decoded once, whole, within the bounds above).

use std::io::Read;
use std::path::Path;
use std::sync::RwLock;

use editor_core::pixels::{TileDelta, TileEdit, TileMap};
use editor_core::timeline::{self, VideoClip, DURATION_RANGE_MS, FPS_RANGE};
use editor_core::Command;
use layer_model::{Layer, LayerId};
use raster::codec::formats::mp4::{self, video::DecodedVideo};
use raster::{CodecError, ImportLimits};

use crate::editor::Editor;
use crate::DocumentId;

/// The largest video file this route reads (1 GiB).
pub const MAX_VIDEO_FILE_BYTES: u64 = 1 << 30;

/// What decodes a video's bytes for a video layer.
pub type VideoDecoder = fn(&[u8], ImportLimits) -> Result<DecodedVideo, CodecError>;

static DECODER: RwLock<Option<VideoDecoder>> = RwLock::new(None);

/// Decode every video layer's media with `decoder`: the decode worker, which
/// [`crate::dialogs::decode_worker::install`] installs.
pub fn install_video_decoder(decoder: VideoDecoder) {
    *DECODER.write().unwrap_or_else(|e| e.into_inner()) = Some(decoder);
}

fn decode(bytes: &[u8]) -> Result<DecodedVideo, CodecError> {
    let decoder = *DECODER.read().unwrap_or_else(|e| e.into_inner());
    match decoder {
        Some(decode) => decode(bytes, ImportLimits::default()),
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

/// Read and decode `path` (the file bounded by [`MAX_VIDEO_FILE_BYTES`]).
fn read_video(path: &Path) -> Result<DecodedVideo, String> {
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
    decode(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// File every frame's tiles in `tiles`; one tile map per frame.
fn frame_tiles(
    video: &DecodedVideo,
    tiles: &mut compositor::MemoryTileSource,
) -> Result<Vec<TileMap>, String> {
    video
        .frames
        .iter()
        .map(|f| {
            let grid = raster::TileGrid::from_rgba8(video.width, video.height, &f.rgba8)
                .map_err(|e| e.to_string())?;
            let edits: Vec<TileEdit> = grid
                .iter()
                .map(|(coord, tile)| TileEdit::set(coord, tiles.insert_bytes(tile.data().to_vec())))
                .collect();
            let mut map = TileMap::default();
            map.apply_delta(&TileDelta::new(edits).map_err(|e| e.to_string())?);
            Ok(map)
        })
        .collect()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Video".to_string())
}

/// The frame rate a video suggests: its shortest frame's, in the range the
/// timeline accepts.
fn fps_of(video: &DecodedVideo) -> u32 {
    let shortest = video
        .frames
        .iter()
        .map(|f| f.duration_ms.max(1))
        .min()
        .unwrap_or(1000);
    (1000 / shortest).clamp(*FPS_RANGE.start(), *FPS_RANGE.end())
}

/// [`Editor::reload_video_frames`] for clips whose source this session has
/// not tried yet, so a missing or damaged source is not read again on every
/// playhead move.
pub(crate) fn reload_once(editor: &mut Editor) {
    static TRIED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let fresh: Vec<String> = match editor.active() {
        Some(open) => open
            .document
            .timeline
            .videos
            .iter()
            .filter(|c| !c.frames_loaded() && !c.source.is_empty())
            .map(|c| c.source.clone())
            .collect(),
        None => return,
    };
    if fresh.is_empty() {
        return;
    }
    {
        let mut tried = TRIED.lock().unwrap_or_else(|e| e.into_inner());
        if fresh.iter().all(|s| tried.contains(s)) {
            return;
        }
        for s in fresh {
            if !tried.contains(&s) {
                tried.push(s);
            }
        }
    }
    editor.reload_video_frames();
}

impl Editor {
    /// File > Open of a video: a new document the video's size holding one
    /// video layer, Timeline mode on, as long as the video and at its frame
    /// rate. Opening is not an edit: the document is clean with nothing to
    /// undo. The file is decoded before anything changes, so a damaged or
    /// refused file opens nothing.
    pub fn open_video_path(&mut self, path: &Path) -> Result<DocumentId, String> {
        let video = read_video(path)?;
        let title = file_name(path);
        self.new_document_with(
            video.width,
            video.height,
            &title,
            crate::import::BlankBackground::Transparent,
        )
        .map_err(|e| e.to_string())?;
        let blank = self
            .active()
            .and_then(|o| o.document.layers.root().first().copied());
        let fps = fps_of(&video);
        let layer = self.add_video_layer(path, &video, Some(fps), blank)?;
        let open = self.active_mut().ok_or("No document is open")?;
        let _ = open.document.set_active_layer(Some(layer));
        open.history.clear();
        open.document.mark_saved();
        open.invalidate_all();
        let id = open.id();
        self.set_status(format!(
            "Opened {} as a video layer ({} frames)",
            path.display(),
            video.frames.len()
        ));
        Ok(id)
    }

    /// The timeline's Add Media: `path`'s video as a new video layer in the
    /// active document, starting at the playhead. One undo step.
    pub fn add_media_path(&mut self, path: &Path) -> Result<String, String> {
        if self.active().is_none() {
            return Err("No document is open".into());
        }
        let video = read_video(path)?;
        self.add_video_layer(path, &video, None, None)?;
        let status = format!(
            "Added {} ({} frames, {} ms)",
            file_name(path),
            video.frames.len(),
            video.duration_ms()
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

    /// Add `video` as a video layer to the active document in one step. For
    /// a document made for the video, `fps` sets the frame rate (and the
    /// length becomes the video's) and `replace` is its blank layer, deleted.
    fn add_video_layer(
        &mut self,
        path: &Path,
        video: &DecodedVideo,
        fps: Option<u32>,
        replace: Option<LayerId>,
    ) -> Result<LayerId, String> {
        let name = file_name(path);
        let command = {
            let open = self.active_mut().ok_or("No document is open")?;
            let frames = frame_tiles(video, &mut open.tiles)?;
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
            let media = u32::try_from(video.duration_ms()).unwrap_or(u32::MAX);
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
                width: video.width,
                height: video.height,
                start_ms: start,
                durations_ms: video.frames.iter().map(|f| f.duration_ms).collect(),
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

    /// Read the frames again for every video layer of the active document
    /// whose frames are not loaded (a document reopened from disk: the
    /// frames' tiles are not saved), from each clip's source file. History
    /// free, like the playhead. Answers how many clips were loaded; a clip
    /// whose file is gone or changed size / length keeps its saved frame.
    pub fn reload_video_frames(&mut self) -> usize {
        let wanted: Vec<(usize, String)> = match self.active() {
            Some(open) => open
                .document
                .timeline
                .videos
                .iter()
                .enumerate()
                .filter(|(_, c)| !c.frames_loaded() && !c.source.is_empty())
                .map(|(i, c)| (i, c.source.clone()))
                .collect(),
            None => return 0,
        };
        let mut loaded = 0;
        for (index, source) in wanted {
            let Ok(video) = read_video(Path::new(&source)) else {
                continue;
            };
            let Some(open) = self.active_mut() else {
                break;
            };
            let fits = open.document.timeline.videos.get(index).is_some_and(|c| {
                (c.width, c.height) == (video.width, video.height)
                    && c.durations_ms.len() == video.frames.len()
            });
            if !fits {
                continue;
            }
            let Ok(frames) = frame_tiles(&video, &mut open.tiles) else {
                continue;
            };
            open.document.timeline.videos[index].frames = frames;
            loaded += 1;
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
        install_video_decoder(video::decode_in_this_process);
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
    /// Ignored until `Editor::open_resource_file` (editor_open_any.rs) calls
    /// [`Editor::open_video_file`] and `Editor::place_path` (editor.rs)
    /// calls [`Editor::place_video_file`]: both files are outside W16-M's
    /// file list. Run with `--ignored` to see the route's state.
    #[test]
    #[ignore = "W16-M: needs the two routing hooks outside this agent's files"]
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
        crate::menu_bridge::perform(MenuAction::Open, &mut ed).unwrap();
        ed.poll_imports();
        let doc = &ed.active().expect("File > Open opened the MP4").document;
        assert_eq!(doc.timeline.videos.len(), 1, "a video layer");
        assert_eq!(doc.timeline.videos[0].frames.len(), 3);
        crate::menu_bridge::perform(MenuAction::PlaceEmbedded, &mut ed).unwrap();
        assert_eq!(ed.active().unwrap().document.timeline.videos.len(), 2);
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
