//! Building the merged composite a `.psd` has to carry.
//!
//! Photoshop stores a flattened copy of the document at the end of the file,
//! and a great many readers — file browsers, thumbnailers, "quick look"
//! previews, Photopea's fallback path — show *only* that. A layered file with
//! no composite looks blank everywhere except in an application that
//! understands layers, so [`crate::write`] never ships one without it.
//!
//! When the caller has a real composite (their own renderer's output) they put
//! it in [`PsdFile::merged`] and this module is not used. [`flatten`] is the
//! fallback for when they do not.
//!
//! # What this flattener does and does not do
//!
//! It honours visibility, opacity, fill opacity, all 27 blend modes (through
//! [`layer_model::BlendMode::blend_rgb`], so the maths is the same one the real
//! compositor uses), layer masks including their default colour outside the
//! mask rectangle, and groups — both isolated and pass-through.
//!
//! It deliberately does **not** model clipping groups, layer effects, or
//! adjustment layers: those need the full compositor in `compositor`, and a
//! half-implementation of them would be wrong in ways that are hard to see.
//! Adjustment layers are skipped, and clipped layers composite as if they were
//! not clipped. A caller who needs those supplies [`PsdFile::merged`] itself.

//! # Untrusted input
//!
//! [`flatten`] is public, is reached from [`crate::write`] whenever a document
//! has no composite of its own, and takes all of its sizes from
//! [`PsdHeader`] — a struct a caller can fill in by hand and which the reader
//! will happily produce from a thirty-eight byte file that declares a
//! 30 000 × 30 000 canvas. Sixteen bytes per pixel of that is fourteen
//! gigabytes, and an isolated group asks for a second canvas on top.
//!
//! So every canvas is drawn from a [`Budget`] and refused *before* it is
//! reserved, and the compositor walks the group tree with an explicit stack
//! rather than recursion. Both are the same rule: a header, on its own, must
//! never be able to abort the process.

use layer_model::blend::unit;

use crate::error::{PsdError, PsdResult};
use crate::header::{Depth, PsdHeader};
use crate::limits::{Budget, WriteOptions};
use crate::model::{LayerKind, MergedImage, PsdFile, PsdLayer, Rect, CHANNEL_ALPHA};

/// Bytes one canvas pixel costs: three `f32` of colour plus one of alpha.
const CANVAS_BYTES_PER_PIXEL: usize = std::mem::size_of::<[f32; 3]>() + std::mem::size_of::<f32>();

/// One canvas of straight-alpha `f32` colour, `channels + 1` planes wide.
struct Canvas {
    width: usize,
    height: usize,
    /// Colour, one plane per colour channel, straight (un-premultiplied).
    color: Vec<[f32; 3]>,
    alpha: Vec<f32>,
    /// What this canvas drew from the budget, so it can be handed back when the
    /// canvas is finished with.
    cost: u64,
}

/// `width * height * per_pixel`, or a typed refusal.
///
/// Both multiplications are checked: `u32::MAX * u32::MAX` fits a 64-bit
/// `usize` but multiplying it by sixteen does not, and `vec![_; n]` with an
/// overflowed `n` panics with "capacity overflow" rather than returning — a
/// panic I reproduced by handing `flatten` a hand-built `u32::MAX` header.
fn checked_bytes(
    width: usize,
    height: usize,
    per_pixel: usize,
    what: &'static str,
) -> PsdResult<usize> {
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(per_pixel))
        .ok_or(PsdError::Overflow { what })
}

impl Canvas {
    /// A cleared canvas, or a refusal if it does not fit the budget.
    fn new(width: usize, height: usize, budget: &mut Budget) -> PsdResult<Self> {
        let cost =
            checked_bytes(width, height, CANVAS_BYTES_PER_PIXEL, "flatten canvas size")? as u64;
        budget.take(cost)?;
        // Safe: `checked_bytes` proved the product fits.
        let pixels = width * height;
        Ok(Canvas {
            width,
            height,
            color: vec![[0.0; 3]; pixels],
            alpha: vec![0.0; pixels],
            cost,
        })
    }

    /// A canvas that owns nothing, used as a placeholder while a pass-through
    /// group borrows its parent's canvas.
    fn placeholder() -> Self {
        Canvas {
            width: 0,
            height: 0,
            color: Vec::new(),
            alpha: Vec::new(),
            cost: 0,
        }
    }

    /// Release this canvas and return its bytes to the budget.
    fn release(self, budget: &mut Budget) {
        budget.give(self.cost);
    }
}

/// Read one sample as `0.0..=1.0`.
///
/// A short or missing plane reads as zero rather than failing: the flattener is
/// a convenience over data a caller may have assembled by hand, and refusing to
/// produce a composite because one plane is the wrong length would be worse
/// than producing one with a black pixel in it. The index arithmetic is checked
/// because `index` derives from a public [`crate::Rect`], which can be built
/// with corners that multiply out past `usize`.
fn sample(data: &[u8], index: usize, depth: Depth) -> f32 {
    let Some(start) = index.checked_mul(depth.bytes_per_sample()) else {
        return 0.0;
    };
    let Some(end) = start.checked_add(depth.bytes_per_sample()) else {
        return 0.0;
    };
    let Some(bytes) = data.get(start..end) else {
        return 0.0;
    };
    match depth {
        Depth::Eight => f32::from(bytes[0]) / 255.0,
        Depth::Sixteen => f32::from(u16::from_be_bytes([bytes[0], bytes[1]])) / 65535.0,
        Depth::ThirtyTwo => unit(f32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
    }
}

/// Write one sample from `0.0..=1.0`.
fn store(out: &mut Vec<u8>, value: f32, depth: Depth) {
    let v = unit(value);
    match depth {
        Depth::Eight => out.push((v * 255.0).round() as u8),
        Depth::Sixteen => out.extend_from_slice(&((v * 65535.0).round() as u16).to_be_bytes()),
        Depth::ThirtyTwo => out.extend_from_slice(&v.to_be_bytes()),
    }
}

/// The mask value at a canvas pixel: the mask's own pixels inside its
/// rectangle, its default colour outside it.
fn mask_at(layer: &PsdLayer, x: i64, y: i64, depth: Depth) -> f32 {
    let Some(mask) = &layer.mask else {
        return 1.0;
    };
    if mask.disabled {
        return 1.0;
    }
    let w = i64::from(mask.bounds.width());
    let h = i64::from(mask.bounds.height());
    let mx = x - i64::from(mask.bounds.left);
    let my = y - i64::from(mask.bounds.top);
    let raw = if mx < 0 || my < 0 || mx >= w || my >= h || w == 0 || h == 0 {
        f32::from(mask.default_color) / 255.0
    } else {
        sample(&mask.data, (my * w + mx) as usize, depth)
    };
    if mask.invert {
        1.0 - raw
    } else {
        raw
    }
}

/// Composite one leaf layer over `canvas`.
fn composite_layer(canvas: &mut Canvas, layer: &PsdLayer, header: PsdHeader, group_alpha: f32) {
    let bounds = layer.bounds;
    let lw = bounds.width() as i64;
    let lh = bounds.height() as i64;
    if lw == 0 || lh == 0 {
        return;
    }
    let nc = header.color_mode.color_channels() as usize;
    let planes: Vec<Option<&Vec<u8>>> = (0..nc)
        .map(|c| layer.channel(c as i16).map(|ch| &ch.data))
        .collect();
    let alpha_plane = layer.channel(CHANNEL_ALPHA).map(|ch| &ch.data);

    let layer_alpha = f32::from(layer.opacity) / 255.0
        * layer.fill_opacity.map_or(1.0, |f| f32::from(f) / 255.0)
        * group_alpha;

    let x0 = bounds.left.max(0) as i64;
    let y0 = bounds.top.max(0) as i64;
    let x1 = (i64::from(bounds.left) + lw).min(canvas.width as i64);
    let y1 = (i64::from(bounds.top) + lh).min(canvas.height as i64);

    for y in y0..y1 {
        for x in x0..x1 {
            let li = ((y - i64::from(bounds.top)) * lw + (x - i64::from(bounds.left))) as usize;
            let ci = (y * canvas.width as i64 + x) as usize;

            let a_s = layer_alpha
                * alpha_plane.map_or(1.0, |p| sample(p, li, header.depth))
                * mask_at(layer, x, y, header.depth);
            if a_s <= 0.0 {
                continue;
            }
            let mut src = [0.0f32; 3];
            for c in 0..nc {
                src[c] = planes[c].map_or(0.0, |p| sample(p, li, header.depth));
            }
            if nc == 1 {
                src[1] = src[0];
                src[2] = src[0];
            }
            let base = canvas.color[ci];
            let a_b = canvas.alpha[ci];
            let blended = layer.blend_mode.blend_rgb(base, src);

            // W3C source-over with a blend function, in straight alpha.
            let a_o = a_s + a_b * (1.0 - a_s);
            if a_o <= 0.0 {
                canvas.color[ci] = [0.0; 3];
                canvas.alpha[ci] = 0.0;
                continue;
            }
            let mut out = [0.0f32; 3];
            for c in 0..3 {
                let premul = a_s * (1.0 - a_b) * src[c]
                    + a_s * a_b * blended[c]
                    + (1.0 - a_s) * a_b * base[c];
                out[c] = unit(premul / a_o);
            }
            canvas.color[ci] = out;
            canvas.alpha[ci] = unit(a_o);
        }
    }
}

/// Composite a canvas over another, as if it were a layer.
fn composite_canvas(dst: &mut Canvas, src: &Canvas, layer: &PsdLayer, alpha_scale: f32) {
    for i in 0..dst.color.len().min(src.color.len()) {
        let a_s = src.alpha[i] * alpha_scale;
        if a_s <= 0.0 {
            continue;
        }
        let base = dst.color[i];
        let a_b = dst.alpha[i];
        let blended = layer.blend_mode.blend_rgb(base, src.color[i]);
        let a_o = a_s + a_b * (1.0 - a_s);
        if a_o <= 0.0 {
            continue;
        }
        let mut out = [0.0f32; 3];
        for c in 0..3 {
            let premul = a_s * (1.0 - a_b) * src.color[i][c]
                + a_s * a_b * blended[c]
                + (1.0 - a_s) * a_b * base[c];
            out[c] = unit(premul / a_o);
        }
        dst.color[i] = out;
        dst.alpha[i] = unit(a_o);
    }
}

/// One level of the group walk: the canvas being drawn into, where in the list
/// the walk has got to, and what to do with the canvas when the level ends.
struct Frame<'a> {
    canvas: Canvas,
    layers: &'a [PsdLayer],
    next: usize,
    /// The group opacity every layer at this level is scaled by.
    alpha: f32,
    /// `Some((group, alpha))` for an isolated group, whose canvas is composited
    /// back into its parent's as if it were one layer. `None` when the canvas
    /// *is* the parent's — the root, and every pass-through group.
    merge: Option<(&'a PsdLayer, f32)>,
}

/// What the current frame asked for on this step.
enum Step<'a> {
    /// Nothing to push or pop; the layer was drawn, skipped, or invisible.
    Continue,
    /// This level is finished.
    Pop,
    /// Draw these children straight onto the current canvas.
    PassThrough(&'a [PsdLayer], f32),
    /// Draw these children onto a fresh canvas, then merge it back.
    Isolated(&'a [PsdLayer], &'a PsdLayer, f32),
}

/// Composite a whole layer tree onto one canvas, without recursing.
///
/// The stack is explicit for the reason given in the module docs: nesting comes
/// from the file, and a native-stack walk over four thousand levels is an
/// uncatchable abort. Every isolated group's scratch canvas is drawn from
/// `budget` and handed back when the group closes, so the ceiling measures
/// bytes held *at once*: a hundred groups side by side are fine, a hundred
/// nested inside one another are refused.
fn composite_tree(
    layers: &[PsdLayer],
    header: PsdHeader,
    width: usize,
    height: usize,
    budget: &mut Budget,
) -> PsdResult<Canvas> {
    let mut stack = vec![Frame {
        canvas: Canvas::new(width, height, budget)?,
        layers,
        next: 0,
        alpha: 1.0,
        merge: None,
    }];

    loop {
        let step = {
            let top = stack.last_mut().expect("the root frame is popped last");
            // Copied out so the borrow of `top` does not tie down `'a`.
            let list = top.layers;
            if top.next >= list.len() {
                Step::Pop
            } else {
                let layer = &list[top.next];
                top.next += 1;
                if !layer.visible || layer.adjustment.is_some() {
                    Step::Continue
                } else {
                    match &layer.kind {
                        LayerKind::Raster => {
                            composite_layer(&mut top.canvas, layer, header, top.alpha);
                            Step::Continue
                        }
                        LayerKind::Group(g) => {
                            let alpha = top.alpha * f32::from(layer.opacity) / 255.0;
                            if g.pass_through {
                                Step::PassThrough(&g.children, alpha)
                            } else {
                                Step::Isolated(&g.children, layer, alpha)
                            }
                        }
                    }
                }
            }
        };

        match step {
            Step::Continue => {}
            Step::PassThrough(children, alpha) => {
                // A pass-through group draws onto the backdrop it sits on, so
                // it borrows its parent's canvas rather than allocating one.
                let top = stack.last_mut().expect("a frame is current");
                let canvas = std::mem::replace(&mut top.canvas, Canvas::placeholder());
                stack.push(Frame {
                    canvas,
                    layers: children,
                    next: 0,
                    alpha,
                    merge: None,
                });
            }
            Step::Isolated(children, layer, alpha) => {
                let canvas = Canvas::new(width, height, budget)?;
                stack.push(Frame {
                    canvas,
                    layers: children,
                    next: 0,
                    // An isolated group's children see no backdrop, and the
                    // group's own opacity is applied once, on the merge.
                    alpha: 1.0,
                    merge: Some((layer, alpha)),
                });
            }
            Step::Pop => {
                let frame = stack.pop().expect("a frame is current");
                let Some(parent) = stack.last_mut() else {
                    return Ok(frame.canvas);
                };
                match frame.merge {
                    Some((layer, alpha)) => {
                        composite_canvas(&mut parent.canvas, &frame.canvas, layer, alpha);
                        frame.canvas.release(budget);
                    }
                    // The canvas was the parent's all along; give it back.
                    None => parent.canvas = frame.canvas,
                }
            }
        }
    }
}

/// Flatten a document into the channels the merged section stores, with the
/// default memory ceiling from [`WriteOptions`].
///
/// The result always has exactly [`PsdHeader::channels`] planes: the colour
/// channels, an alpha channel when the header declares one, and zero-filled
/// planes for any spot channels the header counts but the layer tree has no
/// opinion about.
///
/// Returns [`PsdError::BudgetExhausted`] rather than allocating when the
/// header's canvas is too large — see [`flatten_with`].
pub fn flatten(file: &PsdFile) -> PsdResult<MergedImage> {
    flatten_with(file, WriteOptions::default().max_flatten_bytes)
}

/// Flatten a document, holding at most `max_bytes` of working memory.
///
/// `max_bytes` covers the canvas, every isolated group's scratch canvas, and
/// the output planes. It is checked before each of them is reserved, so an
/// absurd header costs a comparison rather than an allocation — including
/// headers no file could produce, such as one that is `u32::MAX` on a side.
pub fn flatten_with(file: &PsdFile, max_bytes: u64) -> PsdResult<MergedImage> {
    let header = file.header;
    let (w, h) = (header.width as usize, header.height as usize);
    let mut budget = Budget::new(max_bytes);
    let canvas = composite_tree(&file.layers, header, w, h, &mut budget)?;

    let nc = header.color_mode.color_channels() as usize;
    let has_alpha = header.has_alpha();
    let n = w * h; // `Canvas::new` already proved this does not overflow.
    let bytes = header.depth.bytes_per_sample();
    let plane_bytes = checked_bytes(w, h, bytes, "merged plane size")?;
    let mut channels: Vec<Vec<u8>> = Vec::with_capacity(header.channels as usize);

    let reserve = |budget: &mut Budget| -> PsdResult<Vec<u8>> {
        budget.take(plane_bytes as u64)?;
        Ok(Vec::with_capacity(plane_bytes))
    };

    for c in 0..nc.min(header.channels as usize) {
        let mut plane = reserve(&mut budget)?;
        for i in 0..n {
            // Composite against white where nothing covers the canvas, which is
            // what Photoshop shows for a document with no background layer.
            let a = canvas.alpha[i];
            let v = canvas.color[i][if nc == 1 { 0 } else { c }] * a + (1.0 - a);
            store(&mut plane, v, header.depth);
        }
        channels.push(plane);
    }
    if has_alpha && channels.len() < header.channels as usize {
        let mut plane = reserve(&mut budget)?;
        for i in 0..n {
            store(&mut plane, canvas.alpha[i], header.depth);
        }
        channels.push(plane);
    }
    while channels.len() < header.channels as usize {
        budget.take(plane_bytes as u64)?;
        channels.push(vec![0u8; plane_bytes]);
    }
    // A hand-built header may declare fewer channels than its colour mode
    // implies. The merged section has to match the header, not the mode.
    channels.truncate(header.channels as usize);
    Ok(MergedImage { channels })
}

/// A fully transparent composite of the right shape, for a document with no
/// layers at all.
///
/// Fallible for the same reason [`flatten`] is: the sizes come from a header,
/// and `channels * width * height * depth` is not a product to compute
/// unchecked on a number somebody else chose.
pub fn empty_merged(header: PsdHeader) -> PsdResult<MergedImage> {
    empty_merged_with(header, WriteOptions::default().max_flatten_bytes)
}

/// [`empty_merged`] with an explicit ceiling on the bytes it may reserve.
pub fn empty_merged_with(header: PsdHeader, max_bytes: u64) -> PsdResult<MergedImage> {
    let n = checked_bytes(
        header.width as usize,
        header.height as usize,
        header.depth.bytes_per_sample(),
        "empty composite plane size",
    )?;
    let mut budget = Budget::new(max_bytes);
    let mut channels = Vec::with_capacity(header.channels as usize);
    for c in 0..header.channels as usize {
        budget.take(n as u64)?;
        // White paper, transparent alpha: what an empty Photoshop document
        // composites to.
        let colour = c < header.color_mode.color_channels() as usize;
        channels.push(vec![if colour { 0xFF } else { 0 }; n]);
    }
    Ok(MergedImage { channels })
}

/// The canvas rectangle, for callers building full-canvas layers.
pub fn canvas_rect(header: PsdHeader) -> Rect {
    Rect::sized(header.width, header.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::ColorMode;
    use crate::model::{Channel, PsdMask};
    use layer_model::BlendMode;

    fn solid(name: &str, w: u32, h: u32, rgba: [u8; 4]) -> PsdLayer {
        let mut l = PsdLayer::raster(name, Rect::sized(w, h));
        let n = (w * h) as usize;
        l.channels = vec![
            Channel::new(CHANNEL_ALPHA, vec![rgba[3]; n]),
            Channel::new(0, vec![rgba[0]; n]),
            Channel::new(1, vec![rgba[1]; n]),
            Channel::new(2, vec![rgba[2]; n]),
        ];
        l
    }

    fn rgba_of(file: &PsdFile) -> Vec<u8> {
        flatten(file)
            .unwrap()
            .to_rgba8(file.header.width, file.header.height)
            .unwrap()
    }

    #[test]
    fn an_opaque_layer_covering_the_canvas_becomes_the_composite() {
        let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
        file.layers.push(solid("red", 2, 2, [255, 0, 0, 255]));
        assert_eq!(rgba_of(&file), [255, 0, 0, 255].repeat(4));
    }

    #[test]
    fn an_invisible_layer_contributes_nothing() {
        let mut file = PsdFile::new(PsdHeader::rgba8(1, 1));
        let mut l = solid("red", 1, 1, [255, 0, 0, 255]);
        l.visible = false;
        file.layers.push(l);
        // Nothing covers the canvas: white paper, zero alpha.
        assert_eq!(rgba_of(&file), vec![255, 255, 255, 0]);
    }

    #[test]
    fn opacity_and_blend_mode_are_both_applied() {
        let mut file = PsdFile::new(PsdHeader::rgba8(1, 1));
        file.layers.push(solid("base", 1, 1, [255, 255, 255, 255]));
        let mut top = solid("half", 1, 1, [0, 0, 0, 255]);
        top.opacity = 128;
        top.blend_mode = BlendMode::Multiply;
        file.layers.push(top);
        let out = rgba_of(&file);
        // White multiplied with black at 50% is mid grey.
        assert!((out[0] as i32 - 127).abs() <= 1, "{out:?}");
        assert_eq!(out[3], 255);
    }

    #[test]
    fn a_mask_hides_the_pixels_it_covers_and_its_default_colour_rules_outside() {
        let mut file = PsdFile::new(PsdHeader::rgba8(2, 1));
        file.layers.push(solid("bg", 2, 1, [0, 0, 255, 255]));
        let mut top = solid("red", 2, 1, [255, 0, 0, 255]);
        // A mask that covers only the left pixel, opaque there; outside the
        // mask rectangle the default colour (0) hides the layer.
        let mut mask = PsdMask::new(Rect::new(0, 0, 1, 1), vec![255]);
        mask.default_color = 0;
        top.mask = Some(mask);
        file.layers.push(top);
        let out = rgba_of(&file);
        assert_eq!(&out[0..4], &[255, 0, 0, 255], "left pixel is masked in");
        assert_eq!(&out[4..8], &[0, 0, 255, 255], "right pixel is masked out");
    }

    #[test]
    fn a_mask_default_colour_of_255_shows_the_layer_outside_the_mask_rectangle() {
        let mut file = PsdFile::new(PsdHeader::rgba8(2, 1));
        file.layers.push(solid("bg", 2, 1, [0, 0, 255, 255]));
        let mut top = solid("red", 2, 1, [255, 0, 0, 255]);
        let mut mask = PsdMask::new(Rect::new(0, 0, 1, 1), vec![0]);
        mask.default_color = 255;
        top.mask = Some(mask);
        file.layers.push(top);
        let out = rgba_of(&file);
        assert_eq!(&out[0..4], &[0, 0, 255, 255], "left pixel is masked out");
        assert_eq!(&out[4..8], &[255, 0, 0, 255], "right pixel is masked in");
    }

    #[test]
    fn a_layer_smaller_than_the_canvas_only_covers_its_own_rectangle() {
        let mut file = PsdFile::new(PsdHeader::rgba8(3, 1));
        let mut l = PsdLayer::raster("dot", Rect::new(1, 0, 2, 1));
        l.channels = vec![
            Channel::new(CHANNEL_ALPHA, vec![255]),
            Channel::new(0, vec![0]),
            Channel::new(1, vec![255]),
            Channel::new(2, vec![0]),
        ];
        file.layers.push(l);
        let out = rgba_of(&file);
        assert_eq!(&out[0..4], &[255, 255, 255, 0]);
        assert_eq!(&out[4..8], &[0, 255, 0, 255]);
        assert_eq!(&out[8..12], &[255, 255, 255, 0]);
    }

    #[test]
    fn a_layer_partly_outside_the_canvas_is_clipped_rather_than_indexing_out() {
        let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
        let mut l = PsdLayer::raster("overhang", Rect::new(-3, -3, 5, 5));
        let n = 64;
        l.channels = vec![
            Channel::new(CHANNEL_ALPHA, vec![255; n]),
            Channel::new(0, vec![10; n]),
            Channel::new(1, vec![20; n]),
            Channel::new(2, vec![30; n]),
        ];
        file.layers.push(l);
        assert_eq!(rgba_of(&file), [10, 20, 30, 255].repeat(4));
    }

    #[test]
    fn an_isolated_group_applies_its_own_opacity_to_the_whole_group() {
        let mut file = PsdFile::new(PsdHeader::rgba8(1, 1));
        file.layers.push(solid("bg", 1, 1, [0, 0, 0, 255]));
        let mut group = PsdLayer::group("g");
        group.opacity = 128;
        group
            .push_child(solid("white", 1, 1, [255, 255, 255, 255]))
            .unwrap();
        file.layers.push(group);
        let out = rgba_of(&file);
        assert!((out[0] as i32 - 128).abs() <= 2, "{out:?}");
    }

    #[test]
    fn a_pass_through_group_sees_the_backdrop_and_an_isolated_one_does_not() {
        // Mid grey multiplied by mid grey is quarter grey — but only if the
        // multiply can see the backdrop. Inside an isolated group the child
        // multiplies against transparency, so it comes out unchanged.
        let mut isolated = PsdFile::new(PsdHeader::rgba8(1, 1));
        isolated
            .layers
            .push(solid("bg", 1, 1, [128, 128, 128, 255]));
        let mut g = PsdLayer::group("g");
        let mut child = solid("grey", 1, 1, [128, 128, 128, 255]);
        child.blend_mode = BlendMode::Multiply;
        g.push_child(child).unwrap();
        isolated.layers.push(g);

        let mut pass = isolated.clone();
        pass.layers[1]
            .group_data_mut()
            .expect("layer 1 is the group")
            .pass_through = true;

        let a = rgba_of(&isolated);
        let b = rgba_of(&pass);
        assert!(
            (a[0] as i32 - 128).abs() <= 1,
            "isolated group should stay mid grey, got {a:?}"
        );
        assert!(
            (b[0] as i32 - 64).abs() <= 2,
            "pass-through group should multiply into the backdrop, got {b:?}"
        );
        assert_eq!((a[3], b[3]), (255, 255));
    }

    #[test]
    fn adjustment_layers_are_skipped_rather_than_drawn_as_black() {
        let mut file = PsdFile::new(PsdHeader::rgba8(1, 1));
        file.layers.push(solid("bg", 1, 1, [12, 34, 56, 255]));
        let mut adj = PsdLayer::raster("levels", Rect::sized(1, 1));
        adj.adjustment = Some(crate::model::Adjustment {
            key: *b"levl",
            data: vec![0; 8],
        });
        file.layers.push(adj);
        assert_eq!(rgba_of(&file), vec![12, 34, 56, 255]);
    }

    #[test]
    fn a_sixteen_bit_document_flattens_to_sixteen_bit_planes() {
        let header = PsdHeader {
            channels: 4,
            width: 1,
            height: 1,
            depth: Depth::Sixteen,
            color_mode: ColorMode::Rgb,
        };
        let mut file = PsdFile::new(header);
        let mut l = PsdLayer::raster("l", Rect::sized(1, 1));
        l.channels = vec![
            Channel::new(CHANNEL_ALPHA, vec![0xFF, 0xFF]),
            Channel::new(0, vec![0x80, 0x00]),
            Channel::new(1, vec![0x00, 0x00]),
            Channel::new(2, vec![0xFF, 0xFF]),
        ];
        file.layers.push(l);
        let merged = flatten(&file).unwrap();
        assert_eq!(merged.channels.len(), 4);
        assert_eq!(merged.channels[0].len(), 2);
        let red = u16::from_be_bytes([merged.channels[0][0], merged.channels[0][1]]);
        assert!((i32::from(red) - 0x8000).abs() <= 2, "{red:#x}");
    }

    #[test]
    fn a_greyscale_document_flattens_to_the_channel_count_its_header_declares() {
        let header = PsdHeader {
            channels: 2,
            width: 2,
            height: 1,
            depth: Depth::Eight,
            color_mode: ColorMode::Grayscale,
        };
        let mut file = PsdFile::new(header);
        let mut l = PsdLayer::raster("g", Rect::sized(2, 1));
        l.channels = vec![
            Channel::new(CHANNEL_ALPHA, vec![255, 255]),
            Channel::new(0, vec![64, 192]),
        ];
        file.layers.push(l);
        let merged = flatten(&file).unwrap();
        assert_eq!(merged.channels.len(), 2);
        assert_eq!(merged.channels[0], vec![64, 192]);
        assert_eq!(merged.channels[1], vec![255, 255]);
    }

    #[test]
    fn spot_channels_the_header_counts_are_filled_rather_than_left_missing() {
        let header = PsdHeader {
            channels: 6,
            width: 2,
            height: 2,
            depth: Depth::Eight,
            color_mode: ColorMode::Rgb,
        };
        let file = PsdFile::new(header);
        let merged = flatten(&file).unwrap();
        assert_eq!(merged.channels.len(), 6);
        assert!(merged.channels.iter().all(|c| c.len() == 4));
    }

    #[test]
    fn an_empty_document_flattens_to_white_paper_with_no_alpha() {
        let file = PsdFile::new(PsdHeader::rgba8(2, 2));
        assert_eq!(rgba_of(&file), [255, 255, 255, 0].repeat(4));
        let e = empty_merged(file.header).unwrap();
        assert_eq!(e.channels.len(), 4);
        assert_eq!(e.channels[0], vec![255; 4]);
        assert_eq!(e.channels[3], vec![0; 4]);
    }

    #[test]
    fn a_layer_whose_channels_are_too_short_reads_as_zero_rather_than_panicking() {
        let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
        let mut l = PsdLayer::raster("short", Rect::sized(2, 2));
        l.channels = vec![
            Channel::new(CHANNEL_ALPHA, vec![255]),
            Channel::new(0, vec![9]),
        ];
        file.layers.push(l);
        let _ = rgba_of(&file);
    }

    #[test]
    fn canvas_rect_matches_the_header() {
        let h = PsdHeader::rgba8(9, 4);
        assert_eq!(canvas_rect(h), Rect::new(0, 0, 9, 4));
    }
}
