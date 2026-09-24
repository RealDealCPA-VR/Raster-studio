//! W10-H: Image ▸ Apply Image… and Image ▸ Calculations….
//!
//! * **Apply Image** blends a source (an open document of the same size, its
//!   merged image or one layer, the composite colour or one channel,
//!   optionally inverted) into the active layer: a blending mode
//!   ([`layer_model::BlendMode::blend_rgb`], the compositor's reference
//!   arithmetic), an opacity, an optional mask channel and the live
//!   selection weight the blend per pixel. With Preserve Transparency the
//!   layer's alpha is kept; without it the blended area becomes opaque in
//!   proportion to the weight. One undo step. A 16-bit document is read and
//!   written at 16 bits.
//! * **Calculations** blends one channel of Source 1 onto one channel of
//!   Source 2 (the base) the same way, and the grey result becomes a new
//!   alpha channel (a saved selection "Alpha N", as Select ▸ Save Selection
//!   stores one), the live selection (one undoable `SetSelection`), or a new
//!   grayscale document.
//!
//! Sources are read at 16 bits whatever the document's depth, through the
//! compositor (a layer alone is composited with every other layer hidden,
//! the way `OpenDocument::layer_pixels` reads it).

use editor_core::color_mode::mode;
use editor_core::{Command, Selection};
use layer_model::BlendMode;
use ui::dialogs::{
    ApplyImageSpec, CalculationResult, CalculationsSpec, ImageSource, SourceChannel, SourceDocument,
};

use crate::doc::OpenDocument;
use crate::editor::Editor;

/// The same-size open documents a source can come from, the active one
/// first, each with its layers top first.
pub(crate) fn source_documents(editor: &Editor) -> Vec<SourceDocument> {
    let Some(active) = editor.active() else {
        return Vec::new();
    };
    let size = (active.document.width(), active.document.height());
    let mut docs: Vec<&OpenDocument> = vec![active];
    docs.extend(
        editor
            .documents()
            .iter()
            .filter(|d| d.id() != active.id() && (d.document.width(), d.document.height()) == size),
    );
    docs.into_iter()
        .map(|d| SourceDocument {
            key: d.id().0,
            name: d.title().to_string(),
            layers: d
                .document
                .layers
                .iter_depth_first()
                .into_iter()
                .rev()
                .filter_map(|id| d.document.layers.get(id).map(|l| (id, l.name.clone())))
                .collect(),
        })
        .collect()
}

/// A source's pixels over its canvas as straight RGBA in `0..=1`.
fn source_rgba(
    editor: &Editor,
    source: &ImageSource,
    size: (u32, u32),
) -> Result<Vec<[f32; 4]>, String> {
    let doc = editor
        .documents()
        .iter()
        .find(|d| d.id().0 == source.document)
        .ok_or("The source document is no longer open")?;
    if (doc.document.width(), doc.document.height()) != size {
        return Err("The source must be the same size as the target".into());
    }
    let mut staged = doc.document.clone();
    if let Some(layer) = source.layer {
        if staged.layers.get(layer).is_none() {
            return Err("The source layer no longer exists".into());
        }
        for other in staged.layers.iter_depth_first() {
            if other != layer {
                if let Some(l) = staged.layers.get_mut(other) {
                    l.visible = false;
                }
            }
        }
    }
    let rgba16 = compositor::composite_region(
        &staged,
        &doc.tiles,
        doc.canvas_rect(),
        0,
        compositor::CompositeOptions::default(),
    )
    .map_err(|e| e.to_string())?
    .to_rgba16(&doc.document.meta.color_space);
    Ok(rgba16
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p.map(|c| f32::from(c) / 65535.0))
        .collect())
}

/// One pixel of `channel` as a colour (a single channel is grey), inverted
/// when asked.
fn channel_of(px: [f32; 4], channel: SourceChannel, invert: bool) -> [f32; 3] {
    let luma = 0.299 * px[0] + 0.587 * px[1] + 0.114 * px[2];
    let v = match channel {
        SourceChannel::Rgb => [px[0], px[1], px[2]],
        SourceChannel::Red => [px[0]; 3],
        SourceChannel::Green => [px[1]; 3],
        SourceChannel::Blue => [px[2]; 3],
        SourceChannel::Gray => [luma; 3],
        SourceChannel::Transparency => [px[3]; 3],
    };
    if invert {
        v.map(|c| 1.0 - c)
    } else {
        v
    }
}

/// A source read as one channel (or the composite colour) per pixel.
fn source_plane(
    editor: &Editor,
    source: &ImageSource,
    size: (u32, u32),
) -> Result<Vec<[f32; 3]>, String> {
    Ok(source_rgba(editor, source, size)?
        .into_iter()
        .map(|px| channel_of(px, source.channel, source.invert))
        .collect())
}

/// Blend `src` onto `base` by `blend`, then mix the result in by `weight`.
pub(crate) fn blend_weighted(
    base: [f32; 3],
    src: [f32; 3],
    blend: BlendMode,
    weight: f32,
) -> [f32; 3] {
    let blended = blend.blend_rgb(base, src);
    let w = weight.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| base[i] + (blended[i] - base[i]) * w)
}

/// The per-pixel weight: opacity x mask channel x selection coverage.
fn weights(
    editor: &Editor,
    opacity: f32,
    mask: Option<&ImageSource>,
    selection: &Selection,
    (w, h): (u32, u32),
) -> Result<Vec<f32>, String> {
    let mask = match mask {
        Some(m) => Some(source_plane(editor, m, (w, h))?),
        None => None,
    };
    Ok((0..w as usize * h as usize)
        .map(|i| {
            let p = glam::IVec2::new((i % w as usize) as i32, (i / w as usize) as i32);
            let m = mask.as_ref().map_or(1.0, |m| m[i][0]);
            opacity.clamp(0.0, 1.0) * m * selection.coverage_at(p)
        })
        .collect())
}

/// Image ▸ Apply Image… into the active layer, one undo step.
pub(crate) fn apply_image(editor: &mut Editor, spec: ApplyImageSpec) -> Result<String, String> {
    let doc = editor.active().ok_or("No document is open")?;
    let size = (doc.document.width(), doc.document.height());
    let layer = doc
        .document
        .active_layer()
        .ok_or("Apply Image needs an active layer")?;
    let sixteen = doc.document.meta.bit_depth == 16;
    let selection = doc.document.selection.clone();
    let src = source_plane(editor, &spec.source, size)?;
    let weight = weights(editor, spec.opacity, spec.mask.as_ref(), &selection, size)?;
    let doc = editor.active_mut().ok_or("No document is open")?;
    let target: Vec<[f32; 4]> = if sixteen {
        doc.layer_rgba16(layer)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| p.map(|c| f32::from(c) / 65535.0))
            .collect()
    } else {
        super::pixels::read_layer(doc, layer)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| p.map(|c| f32::from(c) / 255.0))
            .collect()
    };
    let out: Vec<[f32; 4]> = target
        .iter()
        .zip(&src)
        .zip(&weight)
        .map(|((base, s), &w)| {
            let rgb = blend_weighted([base[0], base[1], base[2]], *s, spec.blend, w);
            let a = if spec.preserve_transparency {
                base[3]
            } else {
                base[3] + (1.0 - base[3]) * w
            };
            [rgb[0], rgb[1], rgb[2], a]
        })
        .collect();
    let label = "Apply Image";
    let command = if sixteen {
        let rgba16: Vec<u16> = out
            .iter()
            .flat_map(|p| p.map(|c| (c.clamp(0.0, 1.0) * 65535.0).round() as u16))
            .collect();
        doc.layer_rgba16_command(layer, &rgba16, label)?
    } else {
        let rgba8: Vec<u8> = out
            .iter()
            .flat_map(|p| p.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8))
            .collect();
        super::pixels::write_layer(doc, layer, &rgba8, label)?
    };
    let revision = editor.revision();
    editor.apply_command(command);
    if editor.revision() == revision {
        return Err("Apply Image changed nothing".into());
    }
    Ok("Applied the image".into())
}

/// The grey plane Calculations produces, `0..=1` per pixel.
pub(crate) fn calculate(editor: &Editor, spec: &CalculationsSpec) -> Result<Vec<f32>, String> {
    let doc = editor.active().ok_or("No document is open")?;
    let size = (doc.document.width(), doc.document.height());
    let one = source_plane(editor, &spec.source1, size)?;
    let two = source_plane(editor, &spec.source2, size)?;
    // The live selection does not limit Calculations: its result is a new
    // channel, selection or document, not an edit of the selected pixels.
    let weight = weights(
        editor,
        spec.opacity,
        spec.mask.as_ref(),
        &Selection::None,
        size,
    )?;
    Ok(one
        .iter()
        .zip(&two)
        .zip(&weight)
        .map(|((s1, s2), &w)| blend_weighted(*s2, *s1, spec.blend, w)[0])
        .collect())
}

/// Image ▸ Calculations…: the result as a new channel, the selection or a
/// new document.
pub(crate) fn calculations(editor: &mut Editor, spec: CalculationsSpec) -> Result<String, String> {
    let plane = calculate(editor, &spec)?;
    let doc = editor.active().ok_or("No document is open")?;
    let (w, h) = (doc.document.width(), doc.document.height());
    let bytes: Vec<u8> = plane
        .iter()
        .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();
    match spec.result {
        CalculationResult::Selection | CalculationResult::NewChannel => {
            let mask = editor_core::SelectionMask::new(glam::IVec2::ZERO, w, h, bytes)
                .map_err(|e| e.to_string())?;
            let selection = Selection::Mask(mask);
            if spec.result == CalculationResult::Selection {
                let revision = editor.revision();
                editor.apply_command(Command::SetSelection { selection });
                if editor.revision() == revision {
                    return Err("Could not set the selection".into());
                }
                return Ok("Calculations made the selection".into());
            }
            let doc = editor.active_mut().ok_or("No document is open")?;
            let names = crate::dialog_host::saved_selection_names(&doc.document);
            let name = ui::dialogs::selection_name::next_alpha_name(&names);
            doc.document
                .saved_selections
                .push((name.clone(), selection));
            doc.document.mark_dirty();
            Ok(format!("Calculations made the channel \"{name}\""))
        }
        CalculationResult::NewDocument => {
            let rgba: Vec<u8> = bytes.iter().flat_map(|&v| [v, v, v, 255]).collect();
            editor
                .new_document_with(
                    w,
                    h,
                    "Calculations",
                    crate::import::BlankBackground::Transparent,
                )
                .map_err(|e| e.to_string())?;
            let doc = editor.active_mut().ok_or("The new document did not open")?;
            let layer = doc
                .document
                .layers
                .iter_depth_first()
                .into_iter()
                .next()
                .ok_or("The new document has no layer")?;
            let paint = super::pixels::write_layer(doc, layer, &rgba, "Calculations")?;
            let command = Command::Transaction {
                label: "Calculations".into(),
                commands: vec![
                    Command::SetMetaColorMode {
                        from: doc.document.meta.color_mode,
                        to: mode::GRAYSCALE,
                    },
                    paint,
                ],
            };
            editor.apply_command(command);
            Ok("Calculations made a new document".into())
        }
    }
}

/// The active layer's id, for tests.
#[cfg(test)]
fn active_layer(editor: &Editor) -> layer_model::LayerId {
    editor.active().unwrap().document.active_layer().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;

    /// A 16x8 document opened from a PNG of `rgba`.
    fn open(dir: &std::path::Path, name: &str, rgba: &[u8]) -> Editor {
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        add(&mut ed, dir, name, rgba);
        ed
    }

    fn add(ed: &mut Editor, dir: &std::path::Path, name: &str, rgba: &[u8]) {
        let path = dir.join(name);
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, 16, 8, rgba).unwrap(),
        )
        .unwrap();
        ed.open_path(&path).unwrap();
    }

    fn flat(rgba: [u8; 4]) -> Vec<u8> {
        std::iter::repeat_n(rgba, 16 * 8).flatten().collect()
    }

    /// A left-to-right grey ramp, opaque.
    fn ramp() -> Vec<u8> {
        (0..16 * 8)
            .flat_map(|i| {
                let v = ((i % 16) * 17) as u8;
                [v, v, v, 255]
            })
            .collect()
    }

    fn layer_rgba(ed: &Editor) -> Vec<u8> {
        let doc = ed.active().unwrap();
        super::super::pixels::read_layer(doc, active_layer(ed))
    }

    #[test]
    fn the_source_list_is_the_same_size_documents_active_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = open(dir.path(), "a.png", &flat([10, 20, 30, 255]));
        add(&mut ed, dir.path(), "b.png", &ramp());
        let docs = source_documents(&ed);
        assert_eq!(docs.len(), 2);
        assert_eq!(
            docs[0].key,
            ed.active().unwrap().id().0,
            "the active one first"
        );
        assert_eq!(docs[0].layers.len(), 1);
    }

    /// Apply Image of another document's red channel in Multiply at 100%:
    /// every pixel of the target is multiplied by that channel — one undo
    /// step, and undo gives the pixels back.
    #[test]
    fn apply_image_multiplies_the_layer_by_another_documents_channel() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = open(dir.path(), "src.png", &ramp());
        let src_key = ed.active().unwrap().id().0;
        add(&mut ed, dir.path(), "dst.png", &flat([200, 100, 50, 255]));
        let before = layer_rgba(&ed);
        let steps = ed.active().unwrap().history.journal().count();
        let spec = ApplyImageSpec {
            source: ImageSource {
                document: src_key,
                layer: None,
                channel: SourceChannel::Red,
                invert: false,
            },
            blend: BlendMode::Multiply,
            opacity: 1.0,
            preserve_transparency: true,
            mask: None,
        };
        apply_image(&mut ed, spec).unwrap();
        let after = layer_rgba(&ed);
        for x in [0usize, 5, 15] {
            let s = (x * 17) as f32 / 255.0;
            let px = &after[x * 4..x * 4 + 4];
            for (c, base) in [200.0f32, 100.0, 50.0].iter().enumerate() {
                let want = (base * s).round();
                assert!(
                    (f32::from(px[c]) - want).abs() <= 1.0,
                    "x {x} channel {c}: {} vs {want}",
                    px[c]
                );
            }
            assert_eq!(px[3], 255);
        }
        assert_eq!(ed.active().unwrap().history.journal().count(), steps + 1);
        ed.active_mut().unwrap().undo().unwrap();
        assert_eq!(layer_rgba(&ed), before);
    }

    /// Opacity, invert, a mask channel and the selection all weight it.
    #[test]
    fn opacity_invert_mask_and_selection_weight_the_blend() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = open(dir.path(), "src.png", &ramp());
        let src_key = ed.active().unwrap().id().0;
        add(&mut ed, dir.path(), "dst.png", &flat([200, 200, 200, 255]));
        // Select the left half only.
        ed.apply_command(Command::SetSelection {
            selection: Selection::Rect {
                min: glam::IVec2::new(0, 0),
                max: glam::IVec2::new(8, 8),
            },
        });
        let spec = ApplyImageSpec {
            source: ImageSource {
                document: src_key,
                layer: None,
                channel: SourceChannel::Gray,
                invert: true,
            },
            blend: BlendMode::Normal,
            opacity: 0.5,
            preserve_transparency: true,
            // The mask is the source's own grey: 0 at x = 0.
            mask: Some(ImageSource::merged(src_key, SourceChannel::Gray)),
        };
        apply_image(&mut ed, spec).unwrap();
        let after = layer_rgba(&ed);
        let at = |x: usize| after[x * 4];
        assert_eq!(at(0), 200, "the mask is black at x 0");
        assert_eq!(at(12), 200, "outside the selection");
        // x 4: src grey 68, inverted 187; mask 68/255; opacity .5.
        let w: f32 = 0.5 * 68.0 / 255.0;
        let want = (200.0 + (187.0 - 200.0) * w).round() as u8;
        assert!(
            (i32::from(at(4)) - i32::from(want)).abs() <= 1,
            "{} vs {want}",
            at(4)
        );
    }

    /// Without Preserve Transparency a transparent layer takes the blend
    /// in; with it, the layer's alpha is kept.
    #[test]
    fn preserve_transparency_keeps_the_alpha() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = open(dir.path(), "src.png", &flat([90, 90, 90, 255]));
        let key = ed.active().unwrap().id().0;
        add(&mut ed, dir.path(), "dst.png", &flat([0, 0, 0, 0]));
        let mut spec = ApplyImageSpec {
            source: ImageSource::merged(key, SourceChannel::Rgb),
            blend: BlendMode::Normal,
            opacity: 1.0,
            preserve_transparency: true,
            mask: None,
        };
        // Nothing to change with the alpha kept at 0 and colour moved: the
        // colour moves but the pixel stays invisible.
        let _ = apply_image(&mut ed, spec);
        assert_eq!(layer_rgba(&ed)[3], 0);
        spec.preserve_transparency = false;
        apply_image(&mut ed, spec).unwrap();
        let px = &layer_rgba(&ed)[..4];
        assert_eq!(px, &[90, 90, 90, 255]);
    }

    /// Calculations: Source 1 multiplied onto Source 2 into the selection,
    /// a new channel and a new document.
    #[test]
    fn calculations_writes_a_selection_a_channel_and_a_document() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = open(dir.path(), "a.png", &ramp());
        let key = ed.active().unwrap().id().0;
        let spec = CalculationsSpec {
            source1: ImageSource::merged(key, SourceChannel::Gray),
            source2: ImageSource::merged(key, SourceChannel::Gray),
            blend: BlendMode::Multiply,
            opacity: 1.0,
            mask: None,
            result: CalculationResult::Selection,
        };
        let plane = calculate(&ed, &spec).unwrap();
        let v: f32 = (10.0 * 17.0) / 255.0;
        assert!(
            (plane[10] - v * v).abs() < 1e-3,
            "{} vs {}",
            plane[10],
            v * v
        );

        calculations(&mut ed, spec).unwrap();
        let doc = ed.active().unwrap();
        let cov = doc.document.selection.coverage_at(glam::IVec2::new(10, 3));
        assert!((cov - v * v).abs() < 0.01, "{cov}");
        assert!(doc.document.selection.coverage_at(glam::IVec2::new(0, 0)) < 0.01);

        calculations(
            &mut ed,
            CalculationsSpec {
                result: CalculationResult::NewChannel,
                ..spec
            },
        )
        .unwrap();
        let doc = ed.active().unwrap();
        assert_eq!(doc.document.saved_selections.len(), 1);
        assert!(doc.document.saved_selections[0].0.starts_with("Alpha"));

        let docs = ed.documents().len();
        calculations(
            &mut ed,
            CalculationsSpec {
                result: CalculationResult::NewDocument,
                ..spec
            },
        )
        .unwrap();
        assert_eq!(ed.documents().len(), docs + 1);
        let doc = ed.active().unwrap();
        assert_eq!(doc.document.meta.color_mode, mode::GRAYSCALE);
        let px = super::super::pixels::read_layer(doc, doc.document.layers.iter_depth_first()[0]);
        let want = (v * v * 255.0).round() as u8;
        assert!((i32::from(px[10 * 4]) - i32::from(want)).abs() <= 1);
    }
}
