//! W18-C: per-channel editing.
//!
//! Photopea (learn/channels): "Click any channel to select it. All editing
//! (e.g. painting, adjustments, filters ...) will be applied only to that
//! channel." The Channels panel's selection
//! ([`ui::panels::channels::ChannelsState::edit_mask`]) is mirrored into the
//! shell every frame by [`sync`] (called from `menu_bridge::context`, which
//! holds both the workspace and the editor), and the pixel routes pass their
//! command through [`masked`] before [`Editor::apply_command`]:
//!
//! * the pointer route's stroke commit (`tool_input`: brush, pencil, eraser,
//!   gradient, paint bucket and every other tool that paints tiles);
//! * `menu_bridge::edit_active_pixels` (every filter and every destructive
//!   adjustment), the Fill dialog, Edit > Clear and the layer flips.
//!
//! # What masking does to a tile
//!
//! For every tile a layer `PaintTiles` / `FillRegion` / `ClearRegion` writes,
//! the tile the layer held before is kept on every component that is not
//! selected, and the edit's own tile is taken on every one that is. Alpha is
//! never taken from the edit: a colour channel is not transparency. The
//! masked command is what reaches history and the journal, so an edit is one
//! undo step and undo restores the prior tile exactly.
//!
//! * **RGB** components are copied sample by sample, so an unselected
//!   component is byte-identical afterwards.
//! * **CMYK** and **Lab** components: both tiles are separated into the
//!   document's model (`color::cmyk`'s documented ink model, `color::model`'s
//!   CIELAB D65), the selected inks / components are taken from the edit,
//!   and the pixel is composed back into the RGBA tile. CMYK is separated at
//!   8 bits (the ink model's only API is 8-bit), Lab in `f32`.
//! * **16-bit documents**: a tile is merged at 16 bits. A tool's RGBA8 output
//!   keeps a component's exact 16-bit value wherever its 8-bit value equals
//!   the rounded prior value (the `widen_rgba8_over` rule, per component), so
//!   an unselected component never passes through 8 bits.
//! * A tile the edit *removes* (the eraser clearing a whole tile) reads as
//!   the all-zero tile on the selected components, instead of wiping the
//!   others.
//!
//! Not masked: a 32-bit document (its `f32` tiles pass through), mask and
//! filter-mask targets (a single channel already), and a layer the same
//! command creates (a paste as a new layer has no prior pixels to keep).

use std::collections::HashMap;

use compositor::TileSource;
use editor_core::pixels::{PixelTarget, TileDelta, TileEdit};
use editor_core::Command;
use layer_model::LayerId;
use raster::depth::{narrow_sample, widen_sample, RGBA16_TILE_BYTES, RGBA8_TILE_BYTES};
use ui::panels::channels::{ChannelModel, ChannelsState};

use crate::doc::OpenDocument;
use crate::editor::Editor;

/// The colour components a pixel edit writes: bit `i` of `mask` is
/// component `i` of `model` (R, G, B; C, M, Y, K; L, a, b).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ChannelWrite {
    pub mask: u8,
    pub model: ChannelModel,
}

impl ChannelWrite {
    /// Whether the mask leaves any component out (otherwise nothing is
    /// masked: every component is written, as with the composite).
    pub fn is_partial(self) -> bool {
        let all = self.model.all_mask();
        self.mask & all != 0 && self.mask & all != all
    }

    fn takes(self, component: usize) -> bool {
        self.mask & (1 << component) != 0
    }
}

impl Editor {
    /// W18-C: the colour components pixel edits write, `None` for all.
    pub fn edit_channels(&self) -> Option<ChannelWrite> {
        self.edit_targets.channels()
    }

    /// W18-C: set the colour components pixel edits write (`None`: all).
    /// The Channels panel's selection sets this every frame through [`sync`].
    pub fn set_edit_channels(&mut self, channels: Option<ChannelWrite>) {
        self.edit_targets
            .set_channels(channels.filter(|c| c.is_partial()));
    }
}

/// Mirror the Channels panel's selected colour components into the editor,
/// against the active document's colour model.
pub fn sync(editor: &mut Editor, channels: &ChannelsState) {
    let write = editor.active().and_then(|doc| {
        let model = ChannelModel::of(&doc.document);
        channels
            .edit_mask(model)
            .map(|mask| ChannelWrite { mask, model })
    });
    editor.set_edit_channels(write);
}

/// `command` restricted to the selected colour components, or unchanged
/// when every component is written (or the document's model or depth does
/// not match what was selected).
pub fn masked(editor: &mut Editor, command: Command) -> Command {
    let Some(write) = editor.edit_channels() else {
        return command;
    };
    let Some(doc) = editor.active_mut() else {
        return command;
    };
    if ChannelModel::of(&doc.document) != write.model || doc.document.meta.bit_depth == 32 {
        return command;
    }
    mask_command(doc, command, write)
}

/// The layer's samples before a channel-limited edit, kept so Edit > Fade
/// can be re-recorded with what the edit really wrote ([`refresh_fade`]).
/// `None` when nothing is channel-masked (the route's own record is right).
pub enum FadeBefore {
    Eight(Vec<u8>),
    Sixteen(Vec<u16>),
}

/// Read [`FadeBefore`] for `layer` when the next edit will be masked.
pub fn fade_before(editor: &Editor, layer: LayerId) -> Option<FadeBefore> {
    let write = editor.edit_channels()?;
    let doc = editor.active()?;
    if ChannelModel::of(&doc.document) != write.model || doc.document.meta.bit_depth == 32 {
        return None;
    }
    Some(if doc.is_sixteen_bit() {
        FadeBefore::Sixteen(doc.layer_rgba16(layer))
    } else {
        FadeBefore::Eight(crate::menu_bridge::pixels::read_layer(doc, layer))
    })
}

/// After a channel-limited edit landed, record Edit > Fade's step again
/// with the pixels the layer now holds, so a Fade blends toward the masked
/// result instead of rewriting the unselected components.
pub fn refresh_fade(editor: &Editor, layer: LayerId, label: &str, before: Option<FadeBefore>) {
    let (Some(before), Some(doc)) = (before, editor.active()) else {
        return;
    };
    match before {
        FadeBefore::Eight(b) => {
            let after = crate::menu_bridge::pixels::read_layer(doc, layer);
            crate::fade::remember(doc.id(), layer, label, &b, &after);
        }
        FadeBefore::Sixteen(b) => {
            let after = doc.layer_rgba16(layer);
            crate::fade::remember(doc.id(), layer, label, &b, &after);
        }
    }
}

/// Rewrite every layer tile edit in `command` (recursing into transactions)
/// so only the components `write` names change.
pub fn mask_command(doc: &mut OpenDocument, command: Command, write: ChannelWrite) -> Command {
    if !write.is_partial() {
        return command;
    }
    match command {
        Command::PaintTiles {
            target: PixelTarget::Layer(layer),
            delta,
        } => Command::PaintTiles {
            target: PixelTarget::Layer(layer),
            delta: mask_delta(doc, layer, delta, write),
        },
        Command::FillRegion {
            target: PixelTarget::Layer(layer),
            rect,
            value,
            delta,
        } => Command::FillRegion {
            target: PixelTarget::Layer(layer),
            rect,
            value,
            delta: mask_delta(doc, layer, delta, write),
        },
        // A clear removes tiles; masked, the selected components go to zero
        // and the others stay, which is a paint, not a removal.
        Command::ClearRegion {
            target: PixelTarget::Layer(layer),
            rect,
            delta,
        } => {
            if editor_core::resolve_target(&doc.document, PixelTarget::Layer(layer)).is_err() {
                return Command::ClearRegion {
                    target: PixelTarget::Layer(layer),
                    rect,
                    delta,
                };
            }
            Command::PaintTiles {
                target: PixelTarget::Layer(layer),
                delta: mask_delta(doc, layer, delta, write),
            }
        }
        Command::Transaction { label, commands } => Command::Transaction {
            label,
            commands: commands
                .into_iter()
                .map(|c| mask_command(doc, c, write))
                .collect(),
        },
        other => other,
    }
}

fn mask_delta(
    doc: &mut OpenDocument,
    layer: LayerId,
    delta: TileDelta,
    write: ChannelWrite,
) -> TileDelta {
    // A layer the same command creates is not in the document yet: it has
    // no prior pixels to keep, so its tiles land as they are.
    let Ok(key) = editor_core::resolve_target(&doc.document, PixelTarget::Layer(layer)) else {
        return delta;
    };
    let mut cmyk = CmykCache::default();
    let mut edits = Vec::with_capacity(delta.len());
    for edit in delta.edits() {
        let prior = doc
            .document
            .pixels
            .tile(key, edit.coord)
            .and_then(|h| doc.tiles.tile(h))
            .map(<[u8]>::to_vec);
        let new = match edit.hash {
            Some(hash) => match doc.tiles.tile(hash) {
                Some(bytes) => bytes.to_vec(),
                None => {
                    edits.push(*edit);
                    continue;
                }
            },
            // A removal of a tile that was never there changes nothing.
            None => match &prior {
                Some(p) => vec![0u8; p.len()],
                None => {
                    edits.push(*edit);
                    continue;
                }
            },
        };
        match merge_tile(prior.as_deref(), &new, write, &mut cmyk) {
            Some(bytes) => {
                let hash = doc.tiles.insert_bytes(bytes);
                edits.push(TileEdit::set(edit.coord, hash));
            }
            None => edits.push(*edit),
        }
    }
    TileDelta::new(edits).unwrap_or(delta)
}

/// Separations already computed in this command (the ink model is an
/// iterative solve; a stroke repeats few colours).
#[derive(Default)]
struct CmykCache(HashMap<[u8; 3], color::cmyk::Cmyk>);

impl CmykCache {
    fn of(&mut self, rgb: [u8; 3]) -> color::cmyk::Cmyk {
        *self
            .0
            .entry(rgb)
            .or_insert_with(|| color::cmyk::rgb8_to_cmyk(rgb))
    }
}

/// One tile merged: `prior`'s unselected components and alpha, `new`'s
/// selected components. `None` for bytes that are not an RGBA8 / RGBA16
/// colour tile (the edit then lands as it is).
fn merge_tile(
    prior: Option<&[u8]>,
    new: &[u8],
    write: ChannelWrite,
    cmyk: &mut CmykCache,
) -> Option<Vec<u8>> {
    let known = |len: usize| len == RGBA8_TILE_BYTES || len == RGBA16_TILE_BYTES;
    if !known(new.len()) || prior.is_some_and(|p| !known(p.len())) {
        return None;
    }
    let deep =
        new.len() == RGBA16_TILE_BYTES || prior.is_some_and(|p| p.len() == RGBA16_TILE_BYTES);
    if !deep {
        let zero = vec![0u8; RGBA8_TILE_BYTES];
        let prior = prior.unwrap_or(&zero);
        let mut out = prior.to_vec();
        for (i, (p, n)) in prior
            .as_chunks::<4>()
            .0
            .iter()
            .zip(new.as_chunks::<4>().0)
            .enumerate()
        {
            if p != n {
                let merged = merge_pixel8(*p, *n, write, cmyk);
                out[i * 4..i * 4 + 4].copy_from_slice(&merged);
            }
        }
        return Some(out);
    }
    let prior16: Vec<u16> = match prior {
        Some(p) => raster::depth::rgba16_samples(p)?,
        None => vec![0u16; RGBA16_TILE_BYTES / 2],
    };
    let new16: Vec<u16> = if new.len() == RGBA16_TILE_BYTES {
        raster::depth::rgba16_samples(new)?
    } else {
        // A tool's RGBA8 output: a component whose 8-bit value is the
        // rounded prior keeps its exact 16-bit prior value.
        new.iter()
            .zip(&prior16)
            .map(|(n, p)| {
                if narrow_sample(*p) == *n {
                    *p
                } else {
                    widen_sample(*n)
                }
            })
            .collect()
    };
    let mut out = prior16.clone();
    for (i, (p, n)) in prior16
        .as_chunks::<4>()
        .0
        .iter()
        .zip(new16.as_chunks::<4>().0)
        .enumerate()
    {
        if p != n {
            let merged = merge_pixel16(*p, *n, write, cmyk);
            out[i * 4..i * 4 + 4].copy_from_slice(&merged);
        }
    }
    Some(raster::rgba16_to_tile_bytes(&out))
}

fn merge_pixel8(p: [u8; 4], n: [u8; 4], write: ChannelWrite, cmyk: &mut CmykCache) -> [u8; 4] {
    match write.model {
        ChannelModel::Rgb => {
            let mut out = p;
            for c in 0..3 {
                if write.takes(c) {
                    out[c] = n[c];
                }
            }
            out
        }
        ChannelModel::Cmyk => {
            let rgb = merge_cmyk([p[0], p[1], p[2]], [n[0], n[1], n[2]], write, cmyk);
            [rgb[0], rgb[1], rgb[2], p[3]]
        }
        ChannelModel::Lab => {
            let unit = |v: u8| f32::from(v) / 255.0;
            let rgb = merge_lab(
                [unit(p[0]), unit(p[1]), unit(p[2])],
                [unit(n[0]), unit(n[1]), unit(n[2])],
                write,
            )
            .map(|v| (v * 255.0).round() as u8);
            [rgb[0], rgb[1], rgb[2], p[3]]
        }
    }
}

fn merge_pixel16(p: [u16; 4], n: [u16; 4], write: ChannelWrite, cmyk: &mut CmykCache) -> [u16; 4] {
    match write.model {
        ChannelModel::Rgb => {
            let mut out = p;
            for c in 0..3 {
                if write.takes(c) {
                    out[c] = n[c];
                }
            }
            out
        }
        // The ink model separates 8-bit colours only.
        ChannelModel::Cmyk => {
            let rgb = merge_cmyk(
                [p[0], p[1], p[2]].map(narrow_sample),
                [n[0], n[1], n[2]].map(narrow_sample),
                write,
                cmyk,
            );
            [
                widen_sample(rgb[0]),
                widen_sample(rgb[1]),
                widen_sample(rgb[2]),
                p[3],
            ]
        }
        ChannelModel::Lab => {
            let unit = |v: u16| f32::from(v) / 65_535.0;
            let rgb = merge_lab(
                [unit(p[0]), unit(p[1]), unit(p[2])],
                [unit(n[0]), unit(n[1]), unit(n[2])],
                write,
            )
            .map(|v| (v * 65_535.0).round() as u16);
            [rgb[0], rgb[1], rgb[2], p[3]]
        }
    }
}

/// Separate both colours, take the selected inks from `new`, compose back.
fn merge_cmyk(prior: [u8; 3], new: [u8; 3], write: ChannelWrite, cache: &mut CmykCache) -> [u8; 3] {
    let p = cache.of(prior);
    let n = cache.of(new);
    let pick = |c: usize, pv: f32, nv: f32| if write.takes(c) { nv } else { pv };
    color::cmyk::cmyk_to_rgb8(color::cmyk::Cmyk {
        c: pick(0, p.c, n.c),
        m: pick(1, p.m, n.m),
        y: pick(2, p.y, n.y),
        k: pick(3, p.k, n.k),
    })
}

/// Take the selected CIELAB components from `new`, the rest from `prior`;
/// sRGB in and out, `0..=1`, clamped.
fn merge_lab(prior: [f32; 3], new: [f32; 3], write: ChannelWrite) -> [f32; 3] {
    let p = color::model::rgb_to_lab(prior);
    let n = color::model::rgb_to_lab(new);
    let mut lab = p;
    for c in 0..3 {
        if write.takes(c) {
            lab[c] = n[c];
        }
    }
    color::model::lab_to_rgb(lab).map(|v| v.clamp(0.0, 1.0))
}

#[cfg(test)]
#[path = "channel_edit_w18_tests.rs"]
mod tests;
