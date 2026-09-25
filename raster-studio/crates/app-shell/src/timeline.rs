//! W13-L: the video timeline, rendered.
//!
//! The record and its commands live in `editor_core::timeline` (saved with
//! the `.rstudio` document); the Animation panel in Timeline mode edits it.
//! This module is the half that needs pixels: [`render_at`] composites the
//! document as it stands at time `t` through the same compositor the canvas
//! and every export use, and [`export_frames`] is what an animated Export As
//! row writes — the timeline's frames (one per `1/fps` second) in Timeline
//! mode, the `_a_` frame layers otherwise.

use compositor::MemoryTileSource;
use editor_core::timeline::document_at;

// W16-M: video layers (File > Open of an MP4, the timeline's Add Media).
#[path = "video_layers.rs"]
pub mod video_layers;
use editor_core::Document;
use raster::animation::{AnimationFrame, MAX_ANIMATION_BYTES, MAX_ANIMATION_FRAMES};

impl crate::editor::Editor {
    /// Move the active document's timeline playhead to `t_ms` and put the
    /// tracked layers' values at that time on its layers
    /// ([`editor_core::timeline::seek`]), then redraw the whole canvas: what
    /// the Animation panel's ruler scrub, each playback frame and Stop ask
    /// for ([`ui::Intent::SeekTimeline`]).
    ///
    /// No history step and no dirty flag: the playhead is not an edit. Not
    /// journaled either, for the same reason. Answers whether anything moved.
    pub fn seek_timeline(&mut self, t_ms: u32) -> bool {
        // W16-M: a reopened document's video layers read their frames again
        // (once per source file a session).
        video_layers::reload_once(self);
        let Some(open) = self.active_mut() else {
            return false;
        };
        if !editor_core::timeline::seek(&mut open.document, t_ms) {
            return false;
        }
        open.invalidate_all();
        self.touch();
        true
    }
}

/// Whether an animated export of `doc` writes its timeline (Timeline mode is
/// on) rather than its `_a_` frame layers.
pub fn exports_timeline(doc: &Document) -> bool {
    doc.timeline.enabled
}

/// The whole canvas at `t_ms`, as straight-alpha RGBA8 in the document's
/// colour space.
pub fn render_at(doc: &Document, tiles: &MemoryTileSource, t_ms: u32) -> Result<Vec<u8>, String> {
    let at = document_at(doc, t_ms);
    let canvas = compositor::composite_region(
        &at,
        tiles,
        raster::PixelRect::new(0, 0, at.width(), at.height()),
        0,
        compositor::CompositeOptions::default(),
    )
    .map_err(|e| e.to_string())?;
    Ok(canvas.to_rgba8(&at.meta.color_space))
}

/// The frames an animated Export As row writes: in Timeline mode one frame
/// per `1/fps` second over the timeline's length, each rendered at its time
/// and lasting until the next; otherwise one per `_a_` frame layer
/// ([`crate::import::composite_animation_frames`]). The frame count and the
/// bytes held are bounded like an opened animation's
/// ([`MAX_ANIMATION_FRAMES`], [`MAX_ANIMATION_BYTES`]) before any frame is
/// rendered.
pub fn export_frames(
    doc: &Document,
    tiles: &MemoryTileSource,
) -> Result<Vec<AnimationFrame>, String> {
    if !exports_timeline(doc) {
        return crate::import::composite_animation_frames(doc, tiles).map_err(|e| e.to_string());
    }
    let times = doc.timeline.frame_times();
    let bytes = u64::from(doc.width()) * u64::from(doc.height()) * 4 * times.len() as u64;
    if times.len() > MAX_ANIMATION_FRAMES || bytes > MAX_ANIMATION_BYTES {
        return Err(format!(
            "the timeline is {} frames of {}x{}: more than this build renders at once \
             ({MAX_ANIMATION_FRAMES} frames, {} MiB); shorten it or lower its frame rate",
            times.len(),
            doc.width(),
            doc.height(),
            MAX_ANIMATION_BYTES >> 20
        ));
    }
    times
        .into_iter()
        .map(|(t, duration)| {
            Ok(AnimationFrame {
                rgba8: render_at(doc, tiles, t)?,
                delay_ms: duration,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    //! Driven through the editor's real routes: a PNG opened with File >
    //! Open's synchronous path, timeline edits applied as the shell applies
    //! the Animation panel's commands, the document saved as a `.rstudio`
    //! package and reopened, and `Editor::request_export` (what the shell
    //! calls with the Export As job) writing an MP4.

    use std::path::Path;

    use editor_core::timeline::{self, KeyProperty};
    use raster::codec::formats::mp4;
    use raster::ExportFormat;

    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::editor::Editor;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;

    const W: u32 = 32;
    const H: u32 = 24;

    fn editor(dir: &Path) -> Editor {
        Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        )
    }

    /// An editor with a solid red `W` x `H` PNG open as one raster layer.
    fn red_document(dir: &Path) -> Editor {
        let mut ed = editor(dir);
        let png = dir.join("red.png");
        let rgba: Vec<u8> = (0..W * H).flat_map(|_| [255u8, 0, 0, 255]).collect();
        std::fs::write(
            &png,
            raster::encode(ExportFormat::Png, W, H, &rgba).unwrap(),
        )
        .unwrap();
        ed.open_path(&png).unwrap();
        ed
    }

    fn only_layer(ed: &Editor) -> layer_model::LayerId {
        let doc = &ed.active().unwrap().document;
        *doc.layers.root().first().expect("one layer")
    }

    /// Timeline mode on, the layer's bar from 0 to 1 s, fading in from
    /// opacity 0 at 0 ms to 1 at 1000 ms — each edit one command, as the
    /// panel emits them.
    fn fade_in(ed: &mut Editor) -> layer_model::LayerId {
        let id = only_layer(ed);
        let doc = &ed.active().unwrap().document;
        let c = timeline::set_mode(doc, true).unwrap();
        ed.apply_command(c);
        let mut t = ed.active().unwrap().document.timeline.clone();
        t.duration_ms = 1000;
        t.fps = 4;
        let c = timeline::set_timeline(&ed.active().unwrap().document, "Timeline", t).unwrap();
        ed.apply_command(c);
        let c = timeline::add_key(
            &ed.active().unwrap().document,
            id,
            KeyProperty::Opacity,
            1000,
        )
        .unwrap();
        ed.apply_command(c);
        ed.active_mut()
            .unwrap()
            .document
            .layers
            .get_mut(id)
            .unwrap()
            .opacity = 0.0;
        let c =
            timeline::add_key(&ed.active().unwrap().document, id, KeyProperty::Opacity, 0).unwrap();
        ed.apply_command(c);
        id
    }

    #[test]
    fn the_timeline_survives_a_save_and_open_of_the_rstudio_package() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = red_document(dir.path());
        let id = fade_in(&mut ed);
        let c = timeline::set_in_out(&ed.active().unwrap().document, id, 0, 750).unwrap();
        ed.apply_command(c);
        let saved = ed.active().unwrap().document.timeline.clone();
        assert!(saved.enabled);
        assert_eq!(saved.track(id).unwrap().out_ms, 750);
        assert_eq!(saved.track(id).unwrap().opacity.len(), 2);
        assert!(
            ed.active().unwrap().document.is_dirty(),
            "an unsaved change"
        );

        let project = dir.path().join("fade.rstudio");
        ed.active_mut().unwrap().save_to(&project, "test").unwrap();
        let mut again = editor(dir.path());
        again.open_path(&project).unwrap();
        assert_eq!(
            again.active().unwrap().document.timeline,
            saved,
            "the package carries the timeline"
        );
    }

    #[test]
    fn render_at_composites_the_keyframed_opacity_at_that_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = red_document(dir.path());
        fade_in(&mut ed);
        let open = ed.active().unwrap();
        let alpha_at = |t| render_at(&open.document, &open.tiles, t).unwrap()[3];
        assert_eq!(alpha_at(0), 0);
        let half = alpha_at(500);
        assert!(
            (120..=135).contains(&half),
            "half faded in at 500 ms: {half}"
        );
        assert!(alpha_at(990) >= 250, "faded in by the end");
        // Past the end of the layer's bar it is gone.
        let c = timeline::set_in_out(&open.document, only_layer(&ed), 0, 600).unwrap();
        ed.apply_command(c);
        let open = ed.active().unwrap();
        assert_eq!(render_at(&open.document, &open.tiles, 700).unwrap()[3], 0);
    }

    /// W13X-9: scale and rotation keys (about the layer centre) change the
    /// composited frame: the solid red layer covers the canvas corner at
    /// 0 ms, and at 1000 ms (half size, a quarter turn) the corner is
    /// empty while the centre is still red. With the first rotation and
    /// scale keys on Hold, the corner stays covered until the next key.
    #[test]
    fn scale_and_rotation_keys_change_the_rendered_frame() {
        use editor_core::timeline::{Interpolation, RotationKey, ScaleKey};
        let dir = tempfile::tempdir().unwrap();
        let mut ed = red_document(dir.path());
        let id = only_layer(&ed);
        let c = timeline::set_mode(&ed.active().unwrap().document, true).unwrap();
        ed.apply_command(c);
        let mut t = ed.active().unwrap().document.timeline.clone();
        // Two seconds, so the layer's bar (0..2000) still shows at 1000 ms.
        t.duration_ms = 2000;
        let track = t.track_mut(id);
        let key = |t_ms, s| ScaleKey {
            t_ms,
            sx: s,
            sy: s,
            interp: Interpolation::Linear,
        };
        track.scale = vec![key(0, 1.0), key(1000, 0.5)];
        let turn = |t_ms, degrees| RotationKey {
            t_ms,
            degrees,
            interp: Interpolation::Linear,
        };
        track.rotation = vec![turn(0, 0.0), turn(1000, 90.0)];
        let c = timeline::set_timeline(&ed.active().unwrap().document, "Keys", t).unwrap();
        ed.apply_command(c);

        let pixel = |ed: &Editor, t_ms, x: u32, y: u32| {
            let open = ed.active().unwrap();
            let rgba = render_at(&open.document, &open.tiles, t_ms).unwrap();
            let i = ((y * W + x) * 4) as usize;
            [rgba[i], rgba[i + 3]]
        };
        assert_eq!(pixel(&ed, 0, 0, 0), [255, 255], "unturned at 0 ms");
        assert_eq!(pixel(&ed, 1000, 0, 0)[1], 0, "the corner is empty");
        assert_eq!(pixel(&ed, 1000, 16, 12), [255, 255], "the centre stays");
        // Half size and turned 90 degrees about (16, 12): the 32 x 24 box
        // becomes 12 wide by 16 tall, so x 10..22 and y 4..20.
        assert_eq!(pixel(&ed, 1000, 11, 12)[1], 255);
        assert_eq!(pixel(&ed, 1000, 8, 12)[1], 0);
        assert_eq!(pixel(&ed, 1000, 16, 5)[1], 255);
        assert_eq!(pixel(&ed, 1000, 16, 2)[1], 0);
        assert_eq!(pixel(&ed, 500, 0, 0)[1], 0, "moving by 500 ms");

        // Hold on the first keys: the frame at 500 ms is the frame at 0.
        for property in [KeyProperty::Scale, KeyProperty::Rotation] {
            let c = timeline::set_interpolation(
                &ed.active().unwrap().document,
                id,
                property,
                0,
                Interpolation::Hold,
            )
            .unwrap();
            ed.apply_command(c);
        }
        assert_eq!(pixel(&ed, 500, 0, 0), [255, 255], "held");
        assert_eq!(pixel(&ed, 1000, 0, 0)[1], 0, "then the next key");
    }

    fn export_mp4(ed: &mut Editor, out: &Path) -> mp4::Mp4Info {
        export_mp4_as(ed, out, ExportFormat::Mp4(60))
    }

    /// W15-B: the same export in a chosen MP4 codec (`Mp4` is H.264,
    /// `Mp4Av1` AV1).
    fn export_mp4_as(ed: &mut Editor, out: &Path, format: ExportFormat) -> mp4::Mp4Info {
        std::fs::create_dir_all(out).unwrap();
        let job = ui::dialogs::ExportJob {
            base_name: "clip".to_string(),
            entries: vec![ui::dialogs::ExportEntry::new("", format, 1.0)],
        };
        ed.request_export(job, out.to_path_buf());
        ed.poll_exports();
        let path = out.join("clip.mp4");
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{} was not written: {e}", path.display()));
        mp4::probe(&bytes).unwrap()
    }

    /// Export As ▸ MP4 of a Timeline-mode document writes the timeline: one
    /// frame per 1/fps second at the canvas size; the frames differ over
    /// time, so it is the timeline that was rendered, not one still.
    #[test]
    fn export_as_mp4_writes_the_timelines_frames_at_its_frame_rate() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = red_document(dir.path());
        fade_in(&mut ed);
        let frames = {
            let open = ed.active().unwrap();
            export_frames(&open.document, &open.tiles).unwrap()
        };
        assert_eq!(frames.len(), 4, "1 s at 4 fps");
        assert_eq!(
            frames.iter().map(|f| f.delay_ms).collect::<Vec<_>>(),
            vec![250, 250, 250, 250]
        );
        assert!(frames[0].rgba8[3] < frames[3].rgba8[3], "it fades in");

        // W15-B: H.264 is the default MP4 codec.
        let info = export_mp4(&mut ed, &dir.path().join("out"));
        assert_eq!((info.width, info.height), (W, H));
        assert_eq!(&info.codec, b"avc1");
        assert_eq!(info.frame_count, 4);
        assert_eq!(info.durations, vec![250, 250, 250, 250]);

        // W15-B: the AV1 option still writes the whole timeline.
        let info = export_mp4_as(&mut ed, &dir.path().join("av1"), ExportFormat::Mp4Av1(60));
        assert_eq!((info.width, info.height), (W, H));
        assert_eq!(&info.codec, b"av01");
        assert_eq!(info.frame_count, 4);
        assert_eq!(info.durations, vec![250, 250, 250, 250]);
    }

    /// In Frames mode the same Export As row writes the `_a_` frame layers,
    /// each with its own delay.
    #[test]
    fn export_as_mp4_in_frames_mode_writes_the_frame_layers() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = red_document(dir.path());
        let id = only_layer(&ed);
        ed.active_mut()
            .unwrap()
            .document
            .layers
            .get_mut(id)
            .unwrap()
            .name = raster::animation::frame_layer_name("Frame 1", 120);
        let copy = {
            let doc = &ed.active().unwrap().document;
            let mut copy = doc.layers.get(id).unwrap().clone();
            copy.id = layer_model::LayerId::new();
            copy.name = raster::animation::frame_layer_name("Frame 2", 300);
            copy
        };
        ed.apply_command(editor_core::Command::create_layer(copy));
        assert!(!exports_timeline(&ed.active().unwrap().document));
        let info = export_mp4(&mut ed, &dir.path().join("frames"));
        assert_eq!(info.frame_count, 2);
        assert_eq!(info.durations, vec![120, 300]);
        assert_eq!((info.width, info.height), (W, H));
    }

    /// File > Export As > MP4 on a Timeline-mode document, the way a click
    /// on the row goes: the row is found in the File menu's Export As
    /// submenu the menu bar draws, resolved as the bar resolves it, and
    /// clicked through `Chrome::menu_click`; the chrome's dialog host then
    /// draws in an egui frame. The MP4 row has a quality field and offers
    /// Animated with a caption naming the timeline's frames.
    #[test]
    fn file_export_as_mp4_opens_the_dialog_with_quality_and_the_timeline_caption() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = red_document(dir.path());
        fade_in(&mut ed);
        let file = crate::menu_bridge::menus(&ed)
            .into_iter()
            .find(|m| m.title == "File")
            .expect("a File menu");
        let export_as = file
            .entries
            .iter()
            .find_map(|e| match e {
                ui::menu::Entry::Submenu { label, entries } if *label == "Export As" => {
                    Some(entries.clone())
                }
                _ => None,
            })
            .expect("File > Export As");
        let row = export_as
            .iter()
            .flat_map(ui::menu::Entry::actions)
            .find(|a| matches!(a, ui::menu::MenuAction::Export(ExportFormat::Mp4(_))))
            .expect("an MP4 row under File > Export As");
        let mut chrome = crate::chrome::Chrome::new();
        let menu_ctx = crate::menu_bridge::context(&mut ed, chrome.workspace());
        let intent = crate::menu_bridge::resolve_intent(row, &menu_ctx, &ed)
            .unwrap_or_else(|why| panic!("the MP4 row is disabled: {why}"));
        let mut clicked = crate::ChromeOutput::default();
        chrome.menu_click(intent, &ed, &mut clicked);
        assert!(
            chrome.dialog_open(),
            "the click opened the Export As dialog"
        );
        let host = chrome.dialogs_for_test();
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut out = crate::ChromeOutput::default();
        let mut texts = Vec::new();
        for _ in 0..3 {
            let full = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1600.0, 1400.0),
                    )),
                    ..Default::default()
                },
                |ctx| host.ui(ctx, None, &mut out),
            );
            texts = full
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                    _ => None,
                })
                .collect();
        }
        assert!(texts.iter().any(|t| t == "MP4"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "Quality"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "Animated"), "{texts:?}");
        assert!(
            texts
                .iter()
                .any(|t| t.contains("MP4: the timeline, 4 frames at 4 fps over 1000 ms")),
            "{texts:?}"
        );
        assert!(!texts.iter().any(|t| t.contains("_a_ layer")), "{texts:?}");
    }

    /// Opening a video file is refused by name, not as "unknown format".
    #[test]
    fn opening_a_video_file_is_a_clear_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = red_document(dir.path());
        fade_in(&mut ed);
        let info = export_mp4(&mut ed, &dir.path().join("v"));
        assert_eq!(info.frame_count, 4);
        let mut fresh = editor(&dir.path().join("fresh"));
        let err = fresh
            .open_path(&dir.path().join("v").join("clip.mp4"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("video"), "{err}");
    }
}
