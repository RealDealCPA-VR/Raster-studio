//! Parsing a `.psd` into a [`PsdFile`].
//!
//! # Shape of the file
//!
//! ```text
//! header (26 bytes)
//! u32 len  colour mode data
//! u32 len  image resources
//! u32 len  layer and mask information
//!            u32 len  layer info: i16 count, records, then channel data
//!            u32 len  global layer mask info
//!            tagged blocks to the end of the section
//! u16 compression, merged composite
//! ```
//!
//! Three things in that layout are easy to get wrong and each has a test:
//!
//! * **Records first, pixels second.** Every layer record is read before any
//!   channel data, and the channel *lengths* in the records are what say where
//!   one layer's pixels stop and the next layer's start.
//! * **Deep-bit-depth layers hide.** In a 16- or 32-bit document Photoshop
//!   leaves the layer-info length at zero and puts the whole layer section in
//!   an `Lr16` or `Lr32` tagged block instead. A reader that only looks at the
//!   first place finds no layers at all in a perfectly good file.
//! * **Groups are punctuation.** Nesting is expressed by `lsct` dividers, with
//!   the closing divider *below* the group's contents and the group's own
//!   record above them, because the file runs bottom-to-top.
//!
//! # Untrusted input
//!
//! Every section is parsed through a sub-cursor carved to the length the file
//! declared, so a section that lies can only damage itself. Every count is
//! checked against [`ReadOptions`] before a `Vec` is reserved, and every
//! decoded channel is drawn from one shared [`Budget`]. Nothing in this module
//! indexes a slice directly. Group nesting is depth-limited
//! ([`ReadOptions::max_group_depth`]) for the same reason descriptor nesting is:
//! the tree outlives the parse, and an unbounded one turns a later walk, write,
//! flatten or drop into a stack overflow.

use layer_model::BlendMode;

use crate::blend::blend_from_key;
use crate::bytes::Cursor;
use crate::codec::{decode_channel, decode_merged, ChannelShape, Compression};
use crate::error::{tag_name, PsdError, PsdResult};
use crate::header::PsdHeader;
use crate::limits::{check_limit, Budget, ReadOptions};
use crate::model::{
    Adjustment, AdjustmentKey, Channel, Effects, GroupData, ImageResource, LayerKind, MergedImage,
    Protection, PsdFile, PsdLayer, PsdMask, RealMask, Rect, TaggedBlock, CHANNEL_ALPHA,
    CHANNEL_REAL_USER_MASK, CHANNEL_USER_MASK,
};
use crate::resource::read_resources;
use crate::text;

/// The two signatures a tagged block may carry.
const SIG_8BIM: [u8; 4] = *b"8BIM";
const SIG_8B64: [u8; 4] = *b"8B64";

/// Tagged-block keys that hold a whole layer-info section for a deep-bit-depth
/// document, in the order they are looked for.
const NESTED_LAYER_KEYS: [[u8; 4]; 3] = [*b"Lr16", *b"Lr32", *b"Layr"];

/// The name Photoshop gives the hidden record that closes a group.
pub const GROUP_DIVIDER_NAME: &str = "</Layer group>";

/// Parse a `.psd` with default limits.
pub fn read(bytes: &[u8]) -> PsdResult<PsdFile> {
    read_with(bytes, &ReadOptions::default())
}

/// Parse a `.psd`, bounding every allocation with `opts`.
pub fn read_with(bytes: &[u8], opts: &ReadOptions) -> PsdResult<PsdFile> {
    let mut cur = Cursor::new(bytes);
    let header = PsdHeader::read(&mut cur, opts)?;
    let mut budget = Budget::new(opts.max_decoded_bytes);
    let mut warnings = Vec::new();

    let color_mode_data = read_sized_section(&mut cur, "colour mode data", opts)?;
    let resources = {
        let len = cur.u32()? as usize;
        let mut section = cur.sub(len)?;
        read_resources(&mut section, opts, &mut warnings)?
    };

    let mut file = PsdFile {
        header,
        color_mode_data,
        resources,
        layers: Vec::new(),
        global_mask: Vec::new(),
        extra: Vec::new(),
        merged: None,
        warnings,
    };

    let lmi_len = cur.u32()? as usize;
    if lmi_len > 0 {
        let mut lmi = cur.sub(lmi_len)?;
        read_layer_and_mask(&mut lmi, &mut file, opts, &mut budget)?;
    }

    if cur.remaining() >= 2 {
        let code = cur.u16()?;
        let compression = Compression::from_code(code)?;
        let shape = ChannelShape::new(file.header.width, file.header.height, file.header.depth);
        let channels = decode_merged(
            &mut cur,
            compression,
            shape,
            file.header.channels as usize,
            &mut budget,
        )?;
        file.merged = Some(MergedImage { channels });
    }

    Ok(file)
}

/// A `u32` length followed by that many bytes, kept verbatim.
fn read_sized_section(
    cur: &mut Cursor<'_>,
    what: &'static str,
    opts: &ReadOptions,
) -> PsdResult<Vec<u8>> {
    let len = cur.u32()? as usize;
    check_limit(what, len as u64, opts.max_resource_bytes as u64)?;
    Ok(cur.take(len)?.to_vec())
}

fn read_layer_and_mask(
    lmi: &mut Cursor<'_>,
    file: &mut PsdFile,
    opts: &ReadOptions,
    budget: &mut Budget,
) -> PsdResult<()> {
    if lmi.remaining() >= 4 {
        let layer_info_len = lmi.u32()? as usize;
        let mut layer_info = lmi.sub(layer_info_len)?;
        if layer_info.remaining() >= 2 {
            file.layers = read_layer_info(
                &mut layer_info,
                file.header,
                opts,
                budget,
                &mut file.warnings,
            )?;
        }
    }
    if lmi.remaining() >= 4 {
        let len = lmi.u32()? as usize;
        file.global_mask = lmi.take(len)?.to_vec();
    }
    file.extra = read_tagged_blocks(lmi, opts, &mut file.warnings)?;

    // A 16- or 32-bit document parks its layers in a tagged block instead.
    if file.layers.is_empty() {
        if let Some(idx) = file
            .extra
            .iter()
            .position(|b| NESTED_LAYER_KEYS.contains(&b.key))
        {
            let block = file.extra.remove(idx);
            let mut nested = Cursor::new(&block.data);
            if nested.remaining() >= 2 {
                file.layers =
                    read_layer_info(&mut nested, file.header, opts, budget, &mut file.warnings)?;
            }
        }
    }
    Ok(())
}

/// One layer record before its channel data has been attached.
struct Pending {
    layer: PsdLayer,
    channel_ids: Vec<i16>,
    channel_lens: Vec<usize>,
    /// `lsct` type: 0 "any other type of layer" (an ordinary layer), 1 open
    /// group, 2 closed group, 3 the hidden closing divider.
    divider: Option<u32>,
}

fn read_layer_info(
    cur: &mut Cursor<'_>,
    header: PsdHeader,
    opts: &ReadOptions,
    budget: &mut Budget,
    warnings: &mut Vec<String>,
) -> PsdResult<Vec<PsdLayer>> {
    let raw_count = cur.i16()?;
    let count = raw_count.unsigned_abs() as usize;
    check_limit("layer count", count as u64, opts.max_layers as u64)?;
    // A layer record is at least 34 bytes before its extra data, so a count
    // that cannot possibly fit is refused before the `Vec` is reserved.
    if count.saturating_mul(34) > cur.remaining() {
        return Err(PsdError::Truncated {
            needed: count * 34,
            available: cur.remaining(),
            at: cur.offset(),
        });
    }

    let mut pendings = Vec::with_capacity(count);
    for _ in 0..count {
        pendings.push(read_layer_record(cur, opts, warnings)?);
    }
    for pending in pendings.iter_mut() {
        read_layer_channels(cur, pending, header, budget, warnings)?;
    }
    build_tree(pendings, opts, warnings)
}

fn read_layer_record(
    cur: &mut Cursor<'_>,
    opts: &ReadOptions,
    warnings: &mut Vec<String>,
) -> PsdResult<Pending> {
    let bounds = Rect {
        top: cur.i32()?,
        left: cur.i32()?,
        bottom: cur.i32()?,
        right: cur.i32()?,
    };
    bounds.validate(opts.max_dimension)?;

    let nchannels = cur.u16()? as usize;
    check_limit(
        "layer channel count",
        nchannels as u64,
        opts.max_channels_per_layer as u64,
    )?;
    let mut channel_ids = Vec::with_capacity(nchannels);
    let mut channel_lens = Vec::with_capacity(nchannels);
    for _ in 0..nchannels {
        channel_ids.push(cur.i16()?);
        channel_lens.push(cur.u32()? as usize);
    }

    cur.expect_tag(&SIG_8BIM, "8BIM (layer blend signature)")?;
    let blend_key = cur.tag()?;
    let opacity = cur.u8()?;
    let clipping = cur.u8()? != 0;
    let flags = cur.u8()?;
    let _filler = cur.u8()?;

    let (blend_mode, pass_through) = match blend_from_key(blend_key) {
        Ok(Some(mode)) => (mode, false),
        Ok(None) => (BlendMode::Normal, true),
        Err(e) => {
            warnings.push(format!("{e}; treating the layer as Normal"));
            (BlendMode::Normal, false)
        }
    };

    let mut layer = PsdLayer {
        bounds,
        blend_mode,
        opacity,
        clipping,
        // Bit 1 is the *hidden* flag: set means not visible.
        visible: flags & 0b10 == 0,
        transparency_protected: flags & 0b1 != 0,
        pixel_data_irrelevant: flags & 0b1_0000 != 0,
        ..Default::default()
    };

    let extra_len = cur.u32()? as usize;
    let mut extra = cur.sub(extra_len)?;
    let mut divider = None;
    let mut group_pass_through = pass_through;

    if extra.remaining() >= 4 {
        let mask_len = extra.u32()? as usize;
        let mut mask_cur = extra.sub(mask_len)?;
        layer.mask = read_mask(&mut mask_cur, opts)?;
    }
    if extra.remaining() >= 4 {
        let ranges_len = extra.u32()? as usize;
        layer.blending_ranges = extra.take(ranges_len)?.to_vec();
    }
    if !extra.is_empty() {
        layer.name = extra.pascal_string(4)?;
    }

    let blocks = read_tagged_blocks(&mut extra, opts, warnings)?;
    for block in blocks {
        match &block.key {
            b"luni" => {
                let mut c = Cursor::new(&block.data);
                match c.unicode_string(opts.max_name_units) {
                    Ok(name) => layer.name = name,
                    Err(e) => warnings.push(format!("unreadable `luni` layer name: {e}")),
                }
            }
            b"lyid" => {
                let mut c = Cursor::new(&block.data);
                layer.layer_id = c.u32().ok();
            }
            b"lclr" => {
                let mut c = Cursor::new(&block.data);
                layer.sheet_color = c.u16().ok();
            }
            b"iOpa" => {
                let mut c = Cursor::new(&block.data);
                layer.fill_opacity = c.u8().ok();
            }
            b"lspf" => {
                let mut c = Cursor::new(&block.data);
                if let Ok(bits) = c.u32() {
                    layer.protection = Protection::from_bits(bits);
                }
            }
            b"lsct" | b"lsdk" => {
                let mut c = Cursor::new(&block.data);
                if let Ok(kind) = c.u32() {
                    divider = Some(kind);
                }
                // A divider may restate the group's blend mode; `pass` there
                // marks a pass-through group just as it does in the record.
                //
                // Read through the same cursor, which is already positioned
                // past the type, rather than re-slicing `block.data[4..]`
                // behind an `if block.data.len() >= 12` several lines above.
                // That older form could not panic either — the guard and the
                // offset were in step, and both spellings need twelve bytes to
                // reach the signature and the key — so this is a structural
                // change and not a bug fix: it removes the guard/offset pairing
                // rather than an out-of-bounds index. The cursor refuses to
                // read past the end of the block by construction, so a short
                // block yields `Err` however this code is edited later.
                if let (Ok(sig), Ok(key)) = (c.tag(), c.tag()) {
                    if sig == SIG_8BIM && key == crate::blend::PASS_THROUGH {
                        group_pass_through = true;
                    }
                }
            }
            b"lfx2" | b"lrFX" => {
                layer.effects = Some(Effects {
                    key: block.key,
                    data: block.data,
                });
            }
            b"TySh" => {
                layer.text = Some(text::parse(&block.data, opts));
            }
            key if AdjustmentKey::from_code(*key).is_some() => {
                layer.adjustment = Some(Adjustment {
                    key: block.key,
                    data: block.data,
                });
            }
            _ => layer.extra.push(block),
        }
    }

    // Only three of the four defined `lsct` types describe a group: 1 (open),
    // 2 (closed) and 3 (the hidden bounding divider). Type 0 is the format's
    // "any other type of layer" — an ordinary layer that happens to carry the
    // block, with real pixels in its channels. Turning it into a group here
    // would disagree with [`build_tree`], which treats `Some(0)` exactly like
    // `None`, and the disagreement is invisible: the layer arrives as a group
    // with no children, and saving it writes a group record plus a bounding
    // divider where an ordinary layer used to be.
    if matches!(divider, Some(1) | Some(2) | Some(3)) {
        layer.kind = LayerKind::Group(GroupData {
            children: Vec::new(),
            open: divider == Some(1),
            pass_through: group_pass_through,
        });
    }

    Ok(Pending {
        layer,
        channel_ids,
        channel_lens,
        divider,
    })
}

fn read_mask(cur: &mut Cursor<'_>, opts: &ReadOptions) -> PsdResult<Option<PsdMask>> {
    if cur.remaining() < 18 {
        // 0 is the ordinary "no mask" case; anything shorter than a rectangle
        // plus its two flag bytes cannot describe one.
        cur.skip_rest();
        return Ok(None);
    }
    let bounds = Rect {
        top: cur.i32()?,
        left: cur.i32()?,
        bottom: cur.i32()?,
        right: cur.i32()?,
    };
    bounds.validate(opts.max_dimension)?;
    let default_color = cur.u8()?;
    let flags = cur.u8()?;

    if flags & 0b1_0000 != 0 {
        // Mask parameters, present only when bit 4 says so.
        let params = cur.u8()?;
        if params & 0b1 != 0 {
            cur.skip(1)?; // user mask density
        }
        if params & 0b10 != 0 {
            cur.skip(8)?; // user mask feather
        }
        if params & 0b100 != 0 {
            cur.skip(1)?; // vector mask density
        }
        if params & 0b1000 != 0 {
            cur.skip(8)?; // vector mask feather
        }
    }

    let mut mask = PsdMask {
        bounds,
        default_color,
        relative_to_layer: flags & 0b1 != 0,
        disabled: flags & 0b10 != 0,
        invert: flags & 0b100 != 0,
        from_render: flags & 0b1000 != 0,
        data: Vec::new(),
        real: None,
    };

    if cur.remaining() >= 18 {
        let real_flags = cur.u8()?;
        let real_background = cur.u8()?;
        let real_bounds = Rect {
            top: cur.i32()?,
            left: cur.i32()?,
            bottom: cur.i32()?,
            right: cur.i32()?,
        };
        real_bounds.validate(opts.max_dimension)?;
        mask.real = Some(RealMask {
            bounds: real_bounds,
            default_color: real_background,
            relative_to_layer: real_flags & 0b1 != 0,
            disabled: real_flags & 0b10 != 0,
            invert: real_flags & 0b100 != 0,
            data: Vec::new(),
        });
    }
    cur.skip_rest();
    Ok(Some(mask))
}

fn read_layer_channels(
    cur: &mut Cursor<'_>,
    pending: &mut Pending,
    header: PsdHeader,
    budget: &mut Budget,
    warnings: &mut Vec<String>,
) -> PsdResult<()> {
    let ids = std::mem::take(&mut pending.channel_ids);
    let lens = std::mem::take(&mut pending.channel_lens);
    for (id, len) in ids.into_iter().zip(lens) {
        let mut section = cur.sub(len)?;
        if section.remaining() < 2 {
            // A zero-length channel: Photoshop writes these for group records.
            continue;
        }
        let compression = Compression::from_code(section.u16()?)?;
        let bounds = match id {
            CHANNEL_USER_MASK => match &pending.layer.mask {
                Some(m) => m.bounds,
                None => {
                    warnings.push(format!(
                        "layer {:?} has a mask channel but no mask record; \
                         reading it against the layer bounds",
                        pending.layer.name
                    ));
                    pending.layer.bounds
                }
            },
            CHANNEL_REAL_USER_MASK => pending
                .layer
                .mask
                .as_ref()
                .and_then(|m| m.real.as_ref())
                .map_or(pending.layer.bounds, |r| r.bounds),
            _ => pending.layer.bounds,
        };
        let shape = ChannelShape::new(bounds.width(), bounds.height(), header.depth);
        let data = decode_channel(&mut section, compression, shape, budget)?;
        if shape.is_empty() && id >= CHANNEL_ALPHA {
            // Photoshop gives every group record and every adjustment layer a
            // full set of channels holding nothing but a compression code. They
            // carry no information, and keeping them would mean a document read
            // from a file did not compare equal to the same document built in
            // memory. The writer regenerates them, so a round-trip is stable.
            continue;
        }
        match id {
            CHANNEL_USER_MASK if pending.layer.mask.is_some() => {
                if let Some(m) = pending.layer.mask.as_mut() {
                    m.data = data;
                }
            }
            CHANNEL_REAL_USER_MASK
                if pending
                    .layer
                    .mask
                    .as_ref()
                    .is_some_and(|m| m.real.is_some()) =>
            {
                if let Some(r) = pending.layer.mask.as_mut().and_then(|m| m.real.as_mut()) {
                    r.data = data;
                }
            }
            _ => pending.layer.channels.push(Channel::new(id, data)),
        }
    }
    drop_masks_without_pixels(pending, header, warnings)?;
    empty_the_rectangle_without_pixels(pending, warnings);
    Ok(())
}

/// Collapse the rectangle of a layer whose colour channels never arrived.
///
/// This is [`drop_masks_without_pixels`] again, one field over. A record may
/// declare a non-empty rectangle and then list **no channels at all** — or list
/// only channels whose declared length is too short to hold even a compression
/// code, which [`read_layer_channels`] skips. Either way the layer arrives with
/// an attacker-chosen rectangle and nothing to put in it, and the writer refuses
/// the whole document with a [`PsdError::InvalidDocument`] whose `is_file_fault`
/// is `false` — blaming the caller for a defect in someone else's file. Read and
/// write have to agree, so the rectangle is emptied here, with a warning, and
/// the document stays writable.
///
/// Emptying rather than synthesising a transparent fill is deliberate, for the
/// same reason the mask case records: the rectangle is attacker-controlled and
/// bounded only by [`ReadOptions::max_dimension`], so filling it would let a
/// file carrying no pixel data at all charge hundreds of megabytes to the
/// [`Budget`] — the one ceiling that is supposed to make that impossible.
///
/// A record that already has an empty rectangle is left alone: that is what
/// Photoshop writes for every group record and every adjustment layer, and the
/// writer regenerates the placeholder channels for it.
fn empty_the_rectangle_without_pixels(pending: &mut Pending, warnings: &mut Vec<String>) {
    let layer = &mut pending.layer;
    if !layer.channels.is_empty() || layer.bounds.is_empty() {
        return;
    }
    warnings.push(format!(
        "layer {:?} declares a {}x{} rectangle but the file carries no channel \
         data to fill it; reading it as an empty layer",
        layer.name,
        layer.bounds.width(),
        layer.bounds.height()
    ));
    layer.bounds = Rect::default();
}

/// Discard a mask whose pixel channel never arrived.
///
/// A layer record may declare a mask rectangle in its extra data and then list
/// no `-2` (or `-3`) channel to fill it. Left alone that reads perfectly
/// cleanly and then makes the writer refuse the whole document with an
/// [`PsdError::InvalidDocument`] — an error whose `is_file_fault` is `false`,
/// blaming the caller for a defect in someone else's file. Read and write have
/// to agree, so the mask is dropped here, with a warning, and the document
/// stays writable.
///
/// Dropping rather than synthesising a `default_color` fill is deliberate: the
/// rectangle is attacker-controlled and only bounded by
/// [`ReadOptions::max_dimension`], so filling it would let a file with no pixel
/// data in it at all charge hundreds of megabytes to the budget.
fn drop_masks_without_pixels(
    pending: &mut Pending,
    header: PsdHeader,
    warnings: &mut Vec<String>,
) -> PsdResult<()> {
    // Disjoint field borrows: the name is read while the mask is written.
    let name = &pending.layer.name;
    let Some(mask) = pending.layer.mask.as_mut() else {
        return Ok(());
    };

    let mut drop_real = false;
    if let Some(real) = mask.real.as_ref() {
        let shape = ChannelShape::new(real.bounds.width(), real.bounds.height(), header.depth);
        if real.data.len() != shape.byte_len()? {
            warnings.push(format!(
                "layer {name:?} declares a real user mask of {}x{} but the file carries no \
                 channel {CHANNEL_REAL_USER_MASK} to fill it; dropping the real mask",
                real.bounds.width(),
                real.bounds.height()
            ));
            drop_real = true;
        }
    }
    if drop_real {
        mask.real = None;
    }

    let shape = ChannelShape::new(mask.bounds.width(), mask.bounds.height(), header.depth);
    if mask.data.len() != shape.byte_len()? {
        warnings.push(format!(
            "layer {name:?} declares a layer mask of {}x{} but the file carries no \
             channel {CHANNEL_USER_MASK} to fill it; dropping the mask",
            mask.bounds.width(),
            mask.bounds.height()
        ));
        pending.layer.mask = None;
    }
    Ok(())
}

/// Turn the flat, bottom-to-top record list back into a tree.
///
/// The file's punctuation is: divider type 3 opens a group (it sits *below* the
/// contents), types 1 and 2 close it and carry the group's own name and blend
/// mode. Unbalanced punctuation is repaired rather than rejected — a damaged
/// file should still show its layers — and every repair is reported.
///
/// # Why the depth is capped
///
/// This loop is iterative, so building the tree cannot overflow the stack
/// however deep the punctuation goes. What *can* overflow is everything that
/// happens to the tree afterwards, including the implicit `Drop`. A group costs
/// two records, so [`ReadOptions::max_layers`] on its own permits a tree four
/// thousand levels deep, and a file of half a megabyte is enough to build one.
/// The depth is therefore refused here, by name, before the tree exists —
/// exactly as descriptor nesting is.
fn build_tree(
    pendings: Vec<Pending>,
    opts: &ReadOptions,
    warnings: &mut Vec<String>,
) -> PsdResult<Vec<PsdLayer>> {
    let mut stack: Vec<Vec<PsdLayer>> = vec![Vec::new()];
    for pending in pendings {
        let Pending {
            mut layer, divider, ..
        } = pending;
        match divider {
            // `stack` holds the root level plus one level per open group, so
            // its length before the push is the depth this divider would take
            // the tree to.
            Some(3) if stack.len() > opts.max_group_depth => {
                return Err(PsdError::GroupTooDeep {
                    max: opts.max_group_depth,
                });
            }
            Some(3) => stack.push(Vec::new()),
            Some(1) | Some(2) => {
                let children = if stack.len() > 1 {
                    stack.pop().expect("checked above")
                } else {
                    warnings.push(format!(
                        "group {:?} closes a group that was never opened; \
                         reading it as an empty group",
                        layer.name
                    ));
                    Vec::new()
                };
                match &mut layer.kind {
                    LayerKind::Group(g) => g.children = children,
                    LayerKind::Raster => {
                        layer.kind = LayerKind::Group(GroupData {
                            children,
                            open: divider == Some(1),
                            pass_through: false,
                        });
                    }
                }
                push_to_top(&mut stack, layer);
            }
            // Type 0 is "any other type of layer": ordinary, and it keeps
            // whatever pixels its record carried. `read_layer_record` leaves it
            // a raster for the same reason.
            Some(0) | None => push_to_top(&mut stack, layer),
            Some(other) => {
                warnings.push(format!(
                    "layer {:?} has unknown section divider type {other}; \
                     reading it as an ordinary layer",
                    layer.name
                ));
                layer.kind = LayerKind::Raster;
                push_to_top(&mut stack, layer);
            }
        }
    }
    while stack.len() > 1 {
        let orphans = stack.pop().expect("checked above");
        warnings.push(format!(
            "{} layer(s) were inside a group that is never closed; \
             they were lifted into the enclosing level",
            orphans.len()
        ));
        if let Some(top) = stack.last_mut() {
            top.extend(orphans);
        }
    }
    Ok(stack.pop().unwrap_or_default())
}

fn push_to_top(stack: &mut [Vec<PsdLayer>], layer: PsdLayer) {
    if let Some(top) = stack.last_mut() {
        top.push(layer);
    }
}

/// Read tagged blocks until the cursor runs out.
///
/// Real files disagree about padding: the specification says a block is padded
/// to an even length, and Photoshop sometimes pads to four. Rather than guess,
/// the scan resynchronises by looking ahead up to three bytes for the next
/// signature — bounded, and only accepted when it lands on a real one.
pub fn read_tagged_blocks(
    cur: &mut Cursor<'_>,
    opts: &ReadOptions,
    warnings: &mut Vec<String>,
) -> PsdResult<Vec<TaggedBlock>> {
    let mut out = Vec::new();
    loop {
        if cur.remaining() < 12 {
            cur.skip_rest();
            break;
        }
        let sig = match cur.peek_tag() {
            Some(s) => s,
            None => break,
        };
        if sig != SIG_8BIM && sig != SIG_8B64 {
            let resync = (1..=3usize).find(|ahead| {
                cur.peek_tag_at(*ahead)
                    .is_some_and(|t| t == SIG_8BIM || t == SIG_8B64)
            });
            match resync {
                Some(ahead) => {
                    cur.skip(ahead)?;
                    continue;
                }
                None => {
                    warnings.push(format!(
                        "expected a tagged block signature at offset {}, found {:?}; \
                         stopped reading blocks there",
                        cur.offset(),
                        tag_name(sig)
                    ));
                    cur.skip_rest();
                    break;
                }
            }
        }
        let signature = cur.tag()?;
        let key = cur.tag()?;
        let len = cur.u32()? as usize;
        check_limit(
            "tagged block length",
            len as u64,
            opts.max_tagged_block_bytes as u64,
        )?;
        let data = cur.take(len)?.to_vec();
        if len % 2 == 1 {
            // The pad byte is outside the declared length and may be absent at
            // the very end of a section.
            let _ = cur.skip(1);
        }
        out.push(TaggedBlock {
            signature,
            key,
            data,
        });
    }
    Ok(out)
}

/// Convenience: every resource with a given id.
pub fn resources_with_id(file: &PsdFile, id: u16) -> impl Iterator<Item = &ImageResource> {
    file.resources.iter().filter(move |r| r.id == id)
}
