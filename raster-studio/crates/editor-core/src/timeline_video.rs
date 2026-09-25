//! W16-M: video layers on the timeline (photopea.com/learn/video, "Video
//! Layers").
//!
//! Declared from [`super`] (`timeline.rs`, with `#[path]`) and re-exported
//! there.
//!
//! # The record
//!
//! A video layer is an ordinary raster layer plus a [`VideoClip`] in
//! [`super::DocumentTimeline::videos`] naming it. The clip holds where the
//! media's first frame sits on the timeline ([`VideoClip::start_ms`]), every
//! frame's duration, and each decoded frame's tiles ([`VideoClip::frames`],
//! content hashes into the document's tile store, like every layer's
//! pixels). The layer's own pixels are the frame at the playhead:
//! [`super::seek`] and [`super::document_at`] put the frame at `t` on it,
//! and [`super::set_timeline`] does the same as part of an edit.
//!
//! Trimming is the layer's bar, as in Photopea: dragging the bar's ends
//! ([`super::set_in_out`]) cuts the video's ends away while the middle stays
//! where it is on the timeline, because the frame at `t` depends only on
//! `t - start_ms`. Before the first frame the first frame shows, past the
//! last the last (the bar normally ends there).
//!
//! # What is saved
//!
//! The clip's layer, name, source path, size, start and frame durations are
//! saved with the document. The decoded frames' tiles are **not**
//! (`#[serde(skip)]`): a document reopened from disk holds the frame that
//! was on the layer when it was saved, and the application reads the frames
//! again from [`VideoClip::source`] (Photopea likewise asks for a large
//! media file again on reopen).

use layer_model::LayerId;
use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::pixels::{PixelKey, TileDelta, TileEdit, TileMap};

/// One video layer's media on the timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoClip {
    /// The raster layer the video shows on.
    pub layer: LayerId,
    /// The media file's name, as the timeline row shows it.
    pub name: String,
    /// Where the media was read from, to read it again on reopen.
    #[serde(default)]
    pub source: String,
    /// The frame size, in pixels.
    pub width: u32,
    pub height: u32,
    /// Where the media's first frame sits on the timeline, in milliseconds.
    #[serde(default)]
    pub start_ms: u32,
    /// Every frame's duration, in milliseconds (the media's own timing).
    pub durations_ms: Vec<u32>,
    /// The tiles of every decoded frame, in the document's tile store; empty
    /// until the frames are decoded (not saved: see the module docs).
    #[serde(skip)]
    pub frames: Vec<TileMap>,
}

impl VideoClip {
    /// The media's whole length, in milliseconds.
    pub fn media_ms(&self) -> u32 {
        self.durations_ms
            .iter()
            .fold(0u32, |sum, d| sum.saturating_add(*d))
    }

    /// Whether the decoded frames are here (one tile map per frame).
    pub fn frames_loaded(&self) -> bool {
        !self.frames.is_empty() && self.frames.len() == self.durations_ms.len()
    }

    /// The index of the frame showing at timeline time `t_ms`: the media is
    /// at `t_ms - start_ms`; the first frame before it starts, the last past
    /// its end. `None` for a clip with no frames.
    pub fn frame_at(&self, t_ms: u32) -> Option<usize> {
        if self.durations_ms.is_empty() {
            return None;
        }
        let media = t_ms.saturating_sub(self.start_ms);
        let mut end = 0u32;
        for (i, d) in self.durations_ms.iter().enumerate() {
            end = end.saturating_add(*d);
            if media < end {
                return Some(i);
            }
        }
        Some(self.durations_ms.len() - 1)
    }
}

/// For every video layer with its frames loaded, the tile delta that turns
/// its pixels into the frame at `t_ms`; layers already showing it, and
/// layers that left the tree, are skipped.
pub(super) fn frame_deltas(doc: &Document, t_ms: u32) -> Vec<(LayerId, TileDelta)> {
    let mut out = Vec::new();
    for clip in &doc.timeline.videos {
        if !clip.frames_loaded() || doc.layers.get(clip.layer).is_none() {
            continue;
        }
        let Some(want) = clip.frame_at(t_ms).and_then(|i| clip.frames.get(i)) else {
            continue;
        };
        let empty = TileMap::default();
        let have = doc
            .pixels
            .tiles(PixelKey::Layer(clip.layer))
            .unwrap_or(&empty);
        let mut edits: Vec<TileEdit> = want
            .iter()
            .filter(|(c, h)| have.get(*c) != Some(*h))
            .map(|(c, h)| TileEdit::set(c, h))
            .collect();
        edits.extend(
            have.iter()
                .filter(|(c, _)| !want.contains(*c))
                .map(|(c, _)| TileEdit::clear(c)),
        );
        if edits.is_empty() {
            continue;
        }
        // Coordinates come from two maps with disjoint filters, so none
        // repeats and the delta always builds.
        if let Ok(delta) = TileDelta::new(edits) {
            out.push((clip.layer, delta));
        }
    }
    out
}

/// Put `deltas` on `doc`'s layer pixels directly (no command): the
/// playhead's frame, which is not an edit.
pub(super) fn put_frames(doc: &mut Document, deltas: Vec<(LayerId, TileDelta)>) {
    for (layer, delta) in deltas {
        doc.pixels.apply(PixelKey::Layer(layer), &delta);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{document_at, seek, set_in_out, set_timeline};
    use super::*;
    use layer_model::Layer;
    use raster::{TileCoord, TileHash};

    fn map(seed: u8) -> TileMap {
        let mut m = TileMap::default();
        m.apply_delta(&TileDelta::single(TileEdit::set(
            TileCoord::new(0, 0, 0),
            TileHash::of(&[seed; 16]),
        )));
        m
    }

    /// A document with one video layer: three frames of 100, 200 and 300 ms
    /// starting at 1000 ms, Timeline mode on.
    fn video_doc() -> (Document, LayerId) {
        let mut doc = Document::new(16, 16, "video");
        let id = doc.layers.push_root(Layer::raster("clip.mp4")).unwrap();
        doc.timeline.enabled = true;
        doc.timeline.videos.push(VideoClip {
            layer: id,
            name: "clip.mp4".into(),
            source: "clip.mp4".into(),
            width: 16,
            height: 16,
            start_ms: 1000,
            durations_ms: vec![100, 200, 300],
            frames: vec![map(1), map(2), map(3)],
        });
        (doc, id)
    }

    fn shown(doc: &Document, id: LayerId) -> Option<TileHash> {
        doc.layer_tiles(id)?.get(TileCoord::new(0, 0, 0))
    }

    #[test]
    fn the_frame_at_t_follows_the_media_time() {
        let (doc, _) = video_doc();
        let clip = &doc.timeline.videos[0];
        assert_eq!(clip.media_ms(), 600);
        assert_eq!(clip.frame_at(0), Some(0), "before the start: frame 1");
        assert_eq!(clip.frame_at(1099), Some(0));
        assert_eq!(clip.frame_at(1100), Some(1));
        assert_eq!(clip.frame_at(1299), Some(1));
        assert_eq!(clip.frame_at(1300), Some(2));
        assert_eq!(clip.frame_at(9000), Some(2), "past the end: the last");
    }

    /// `document_at` and `seek` put the frame at `t` on the layer; the seek
    /// is no edit (the document stays clean).
    #[test]
    fn document_at_and_seek_show_the_frame_at_t() {
        let (mut doc, id) = video_doc();
        let want = |i: usize, doc: &Document| {
            doc.timeline.videos[0].frames[i].get(TileCoord::new(0, 0, 0))
        };
        assert_eq!(shown(&document_at(&doc, 1150), id), want(1, &doc));
        assert_eq!(shown(&document_at(&doc, 1400), id), want(2, &doc));
        assert!(seek(&mut doc, 1150));
        assert_eq!(shown(&doc, id), want(1, &doc));
        assert!(!doc.is_dirty(), "a seek is not an edit");
        assert!(seek(&mut doc, 1000));
        assert_eq!(shown(&doc, id), want(0, &doc));
    }

    /// Trimming is the bar: the frame at `t` does not move when the bar's
    /// ends do, and an edit through `set_timeline` (here the bar, then the
    /// start) paints the frame at the playhead as part of its undo step.
    #[test]
    fn trimming_keeps_the_middle_still_and_moving_the_start_repaints() {
        let (mut doc, id) = video_doc();
        doc.timeline.current_ms = 1150;
        let c = set_in_out(&doc, id, 1100, 1300).unwrap();
        c.apply(&mut doc).unwrap();
        let track = doc.timeline.track(id).unwrap();
        assert_eq!((track.in_ms, track.out_ms), (1100, 1300));
        assert_eq!(doc.timeline.videos[0].frame_at(1150), Some(1));
        assert_eq!(
            shown(&doc, id),
            doc.timeline.videos[0].frames[1].get(TileCoord::new(0, 0, 0)),
            "the edit painted the frame at the playhead"
        );
        let mut t = doc.timeline.clone();
        t.videos[0].start_ms = 1150;
        let c = set_timeline(&doc, "Move Video", t).unwrap();
        let before = doc.clone();
        let inverse = c.apply(&mut doc).unwrap();
        assert_eq!(
            shown(&doc, id),
            doc.timeline.videos[0].frames[0].get(TileCoord::new(0, 0, 0))
        );
        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc.timeline, before.timeline);
        assert_eq!(shown(&doc, id), shown(&before, id), "undo puts it back");
    }

    /// The saved record keeps the clip but not the frames' tiles, and a
    /// document from before video layers (no `videos` key) still loads.
    #[test]
    fn the_clip_saves_without_its_frames() {
        let (doc, id) = video_doc();
        let json = serde_json::to_string(&doc.timeline).unwrap();
        assert!(json.contains("\"videos\""), "{json}");
        let back: super::super::DocumentTimeline = serde_json::from_str(&json).unwrap();
        assert_eq!(back.videos.len(), 1);
        assert_eq!(back.videos[0].layer, id);
        assert_eq!(back.videos[0].durations_ms, vec![100, 200, 300]);
        assert!(!back.videos[0].frames_loaded(), "frames are read again");
        let old: super::super::DocumentTimeline =
            serde_json::from_str(r#"{"enabled":true,"duration_ms":3000,"fps":30}"#).unwrap();
        assert!(old.videos.is_empty());
        // A clip with no frames loaded leaves the layer alone.
        let mut unloaded = doc.clone();
        unloaded.timeline.videos[0].frames.clear();
        assert!(frame_deltas(&unloaded, 1400).is_empty());
    }
}
