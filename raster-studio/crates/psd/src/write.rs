//! Serialising a [`PsdFile`] back to bytes.
//!
//! The writer aims at two readers at once, because they disagree about what is
//! optional:
//!
//! * **Photoshop** needs the layer section in the right *place*. For an 8-bit
//!   document that is the layer-info section; for 16- and 32-bit documents the
//!   layer-info length must be zero and the whole section moves into an `Lr16`
//!   or `Lr32` tagged block. Photoshop opens a deep-bit-depth file with layers
//!   in the 8-bit position as if it had no layers at all.
//! * **Photopea, thumbnailers and previewers** need the merged composite. It is
//!   nominally optional; in practice a file without one shows as blank almost
//!   everywhere. [`write`] therefore always emits one, flattening the layer
//!   tree itself when the caller has not supplied a composite.
//!
//! Both also want a `ResolutionInfo` resource: without it Photoshop reports the
//! document as 1 pixel per inch, which makes every physical measurement in the
//! UI absurd. One is synthesised at 72 dpi when the document has none.
//!
//! Everything this crate does not model — unknown tagged blocks, blending
//! ranges, effects, type data, adjustment payloads — is written back byte for
//! byte, so a load/save cycle does not quietly strip what another application
//! put there.

use crate::blend::{key_from_blend, PASS_THROUGH};
use crate::bytes::Sink;
use crate::codec::{encode_channel_as, encode_merged_as, ChannelShape};
use crate::error::{PsdError, PsdResult};
use crate::flatten::flatten_with;
use crate::header::{ColorMode, Depth, PsdHeader};
use crate::limits::WriteOptions;
use crate::model::{
    GroupData, LayerKind, MergedImage, PsdFile, PsdLayer, PsdMask, Rect, CHANNEL_ALPHA,
    CHANNEL_REAL_USER_MASK, CHANNEL_USER_MASK,
};
use crate::read::{GROUP_DIVIDER_NAME, PSB_LONG_KEYS};
use crate::resource::{resolution_info, write_resources, ID_RESOLUTION_INFO};

/// The largest layer count the `i16` in the layer-info section can carry.
const MAX_RECORDS: usize = i16::MAX as usize;

/// The largest canvas edge a `.psd` can describe. Beyond this the document is a
/// `.psb` (W11-H: which [`write`] then writes, see [`write_psb`]).
pub const MAX_DIMENSION: u32 = 30_000;

/// W11-H: the largest canvas edge a `.psb` can describe (Adobe's limit, and
/// what [`crate::ReadOptions::max_psb_dimension`] reads back by default).
pub const MAX_PSB_DIMENSION: u32 = 300_000;

/// Refuse a header that cannot be expressed, *before* anything is built from it.
///
/// The reader enforces the same three rules on the way in
/// ([`PsdHeader::read`]); this is the matching gate on the way out, because a
/// `PsdFile` can also be assembled in memory. Without it a hand-built 4-billion
/// pixel header reaches [`flatten`], which would try to allocate the canvas and
/// abort the process rather than return an error.
fn check_header(header: PsdHeader) -> PsdResult<()> {
    if header.width == 0 || header.height == 0 {
        return Err(PsdError::InvalidDocument(format!(
            "a .psd canvas must be at least 1x1, not {}x{}",
            header.width, header.height
        )));
    }
    if header.width > MAX_PSB_DIMENSION || header.height > MAX_PSB_DIMENSION {
        return Err(PsdError::InvalidDocument(format!(
            "a .psb canvas may be at most {MAX_PSB_DIMENSION}x{MAX_PSB_DIMENSION}, not {}x{}",
            header.width, header.height
        )));
    }
    let min = header.color_mode.color_channels();
    if header.channels < min || header.channels > 56 {
        return Err(PsdError::InvalidDocument(format!(
            "a {:?} document needs between {min} and 56 channels, not {}",
            header.color_mode, header.channels
        )));
    }
    // W16-B: the colour modes past RGB and Greyscale, each with what it needs.
    match header.color_mode {
        ColorMode::Bitmap => {
            return Err(PsdError::InvalidDocument(
                "a Bitmap document is held at 8 bits here and is written as Greyscale; this \
                 writer does not pack 1-bit samples"
                    .into(),
            ))
        }
        ColorMode::Indexed if header.depth != Depth::Eight => {
            return Err(PsdError::InvalidDocument(format!(
                "an Indexed document is 8-bit, not {}-bit",
                header.depth.bits()
            )))
        }
        _ => {}
    }
    Ok(())
}

/// W16-B: the colour mode data a mode needs, checked before anything is
/// written: an Indexed file's 768-byte palette. (A Duotone record is written
/// back as it was read; the reader treats one it cannot parse as greyscale.)
fn check_mode_data(file: &PsdFile) -> PsdResult<()> {
    let palette = crate::colour_modes::INDEXED_MODE_DATA_LEN;
    if file.header.color_mode == ColorMode::Indexed && file.color_mode_data.len() != palette {
        return Err(PsdError::InvalidDocument(format!(
            "an Indexed document needs a 768-byte palette, not {} bytes",
            file.color_mode_data.len()
        )));
    }
    Ok(())
}

/// W16-B: the composite of a print-mode document the caller supplied none
/// for. The fallback flattener composites RGB and grey only, so this is blank
/// paper in the mode's own encoding (no ink; Lab's `a`/`b` at their
/// midpoint), transparent where there is an alpha channel.
fn paper_composite(header: PsdHeader, max_bytes: u64) -> PsdResult<MergedImage> {
    let mut merged = crate::flatten::empty_merged_with(header, max_bytes)?;
    if header.color_mode == ColorMode::Lab {
        for plane in merged.channels.iter_mut().skip(1).take(2) {
            for (i, v) in plane.iter_mut().enumerate() {
                // The high byte of a 16-bit sample, or the one 8-bit byte.
                let high = header.depth.bytes_per_sample() == 1
                    || i % header.depth.bytes_per_sample() == 0;
                *v = if high { 0x80 } else { 0 };
            }
        }
    }
    Ok(merged)
}

/// Serialise with default options (RLE everywhere, 72 dpi if unset).
pub fn write(file: &PsdFile) -> PsdResult<Vec<u8>> {
    write_with(file, &WriteOptions::default())
}

/// Serialise with `opts`.
///
/// W11-H: a canvas wider or taller than [`MAX_DIMENSION`] (30 000 px) has
/// no `.psd` encoding, so it is written as a `.psb` (version 2): the one
/// file this document can be. Whether that suits the file *name* is the
/// caller's question ([`is_psb`] answers what was written).
pub fn write_with(file: &PsdFile, opts: &WriteOptions) -> PsdResult<Vec<u8>> {
    let psb = file.header.width > MAX_DIMENSION || file.header.height > MAX_DIMENSION;
    write_impl(file, opts, psb)
}

/// W11-H: serialise as a `.psb` (Photoshop's large document format,
/// version 2) with default options, whatever the canvas size.
///
/// A `.psb` differs from a `.psd` only in field widths: the version is 2;
/// the layer-and-mask section, the layer-info section and every layer
/// channel carry a 64-bit length; the long-key tagged blocks (`Lr16`,
/// `Lr32`, `Layr`, `lnk2`, `FMsk`... the list [`crate::read`] honours) do
/// too; and PackBits row counts are 32-bit. Canvas edges may reach
/// [`MAX_PSB_DIMENSION`].
pub fn write_psb(file: &PsdFile) -> PsdResult<Vec<u8>> {
    write_psb_with(file, &WriteOptions::default())
}

/// W11-H: [`write_psb`] with `opts`.
pub fn write_psb_with(file: &PsdFile, opts: &WriteOptions) -> PsdResult<Vec<u8>> {
    write_impl(file, opts, true)
}

/// W11-H: whether `bytes` is a version-2 (`.psb`) file.
pub fn is_psb(bytes: &[u8]) -> bool {
    bytes.len() >= 6 && bytes[..4] == *b"8BPS" && bytes[4..6] == [0, 2]
}

/// W11-H: a length field, 32-bit in a `.psd` and 64-bit in a `.psb`.
fn put_len(sink: &mut Sink, len: usize, psb: bool) {
    if psb {
        sink.u32((len as u64 >> 32) as u32);
        sink.u32(len as u32);
    } else {
        sink.u32(len as u32);
    }
}

/// W11-H: `body` as a length-prefixed section padded to an even length (the
/// pad counted in the length), at the width `psb` asks for.
fn put_section_even(sink: &mut Sink, body: &[u8], psb: bool) {
    let pad = body.len() % 2;
    put_len(sink, body.len() + pad, psb);
    sink.bytes(body);
    sink.zeros(pad);
}

fn write_impl(file: &PsdFile, opts: &WriteOptions, psb: bool) -> PsdResult<Vec<u8>> {
    let header = file.header;
    check_header(header)?;
    check_mode_data(file)?;
    let mut sink = Sink::new();
    header.write_as(&mut sink, psb);

    // Colour mode data.
    sink.u32(file.color_mode_data.len() as u32);
    sink.bytes(&file.color_mode_data);

    // Image resources.
    let slot = sink.begin_len();
    let mut resources = file.resources.clone();
    if opts.synthesize_resolution && !resources.iter().any(|r| r.id == ID_RESOLUTION_INFO) {
        resources.insert(0, resolution_info(72.0));
    }
    write_resources(&resources, &mut sink);
    sink.end_len_even(slot);

    write_layer_and_mask(file, opts, &mut sink, psb)?;

    // Merged composite.
    let merged = match &file.merged {
        Some(m) => m.clone(),
        // A header alone decides how big this canvas is, and a header can come
        // from a thirty-eight byte file. `flatten_with` refuses before it
        // reserves; `check_header` above only bounds the *edge*, not the total.
        None if matches!(header.color_mode, ColorMode::Rgb | ColorMode::Grayscale) => {
            flatten_with(file, opts.max_flatten_bytes)?
        }
        None => paper_composite(header, opts.max_flatten_bytes)?,
    };
    if merged.channels.len() != header.channels as usize {
        return Err(PsdError::InvalidDocument(format!(
            "the merged composite has {} channels but the header declares {}",
            merged.channels.len(),
            header.channels
        )));
    }
    let shape = ChannelShape::new(header.width, header.height, header.depth);
    encode_merged_as(
        &merged.channels,
        opts.merged_compression,
        shape,
        &mut sink,
        psb,
    )?;

    Ok(sink.into_inner())
}

fn write_layer_and_mask(
    file: &PsdFile,
    opts: &WriteOptions,
    sink: &mut Sink,
    psb: bool,
) -> PsdResult<()> {
    let body = build_layer_info(file, opts, psb)?;
    let deep = file.header.depth != Depth::Eight;

    // W11-H: built apart so its length can be written at either width.
    let mut lmi = Sink::new();
    if deep {
        // A 16- or 32-bit document leaves this length at zero and carries its
        // layers in a tagged block further down the same section.
        put_len(&mut lmi, 0, psb);
    } else {
        put_section_even(&mut lmi, &body, psb);
    }

    lmi.u32(file.global_mask.len() as u32);
    lmi.bytes(&file.global_mask);

    if deep {
        let key = match file.header.depth {
            Depth::Sixteen => b"Lr16",
            _ => b"Lr32",
        };
        write_global_block(&mut lmi, b"8BIM", key, &body, psb);
    }
    for block in &file.extra {
        write_global_block(&mut lmi, &block.signature, &block.key, &block.data, psb);
    }
    put_section_even(sink, lmi.as_slice(), psb);
    Ok(())
}

/// One flattened layer record, with its channel payloads already encoded.
struct Record {
    bounds: Rect,
    /// `(id, payload)` where the payload starts with the compression code.
    channels: Vec<(i16, Vec<u8>)>,
    blend_key: [u8; 4],
    opacity: u8,
    clipping: u8,
    flags: u8,
    /// The whole extra-data block, already serialised.
    extra: Vec<u8>,
}

/// The layer-info section body: count, records, then all channel data.
fn build_layer_info(file: &PsdFile, opts: &WriteOptions, psb: bool) -> PsdResult<Vec<u8>> {
    let mut records = Vec::new();
    build_records(&file.layers, file.header, opts, &mut records, psb)?;
    if records.len() > MAX_RECORDS {
        return Err(PsdError::InvalidDocument(format!(
            "{} layer records (groups need two each) exceeds the {MAX_RECORDS} \
             a .psd can address",
            records.len()
        )));
    }

    let mut sink = Sink::new();
    // A negative count is Photoshop's way of saying the merged image's first
    // alpha channel holds transparency rather than a spot channel.
    let count = records.len() as i16;
    sink.i16(if file.header.has_alpha() {
        -count
    } else {
        count
    });
    for r in &records {
        sink.i32(r.bounds.top);
        sink.i32(r.bounds.left);
        sink.i32(r.bounds.bottom);
        sink.i32(r.bounds.right);
        sink.u16(r.channels.len() as u16);
        for (id, payload) in &r.channels {
            sink.i16(*id);
            put_len(&mut sink, payload.len(), psb);
        }
        sink.tag(b"8BIM");
        sink.tag(&r.blend_key);
        sink.u8(r.opacity);
        sink.u8(r.clipping);
        sink.u8(r.flags);
        sink.u8(0); // filler
        sink.u32(r.extra.len() as u32);
        sink.bytes(&r.extra);
    }
    for r in &records {
        for (_, payload) in &r.channels {
            sink.bytes(payload);
        }
    }
    Ok(sink.into_inner())
}

/// One item of pending work for [`build_records`].
enum Task<'a> {
    /// Emit `layers[at..]`, one layer at a time.
    List(&'a [PsdLayer], usize),
    /// Emit the group's own record, now that its children are all out.
    Close(&'a PsdLayer, &'a GroupData),
}

/// Flatten the tree into file order: bottom-to-top, with a group's hidden
/// closing divider *below* its contents and its own record above them.
///
/// Iterative, because `layers` may have come from a file: nesting is
/// attacker-controlled, and a recursive walk over a deeply nested tree is a
/// stack overflow, which aborts the process instead of returning an error.
/// [`crate::read`] caps the depth it will build, and this covers a tree a
/// caller assembled itself.
fn build_records(
    layers: &[PsdLayer],
    header: PsdHeader,
    opts: &WriteOptions,
    out: &mut Vec<Record>,
    psb: bool,
) -> PsdResult<()> {
    let mut stack = vec![Task::List(layers, 0)];
    while let Some(task) = stack.pop() {
        match task {
            Task::List(list, at) => {
                let Some(layer) = list.get(at) else { continue };
                // The rest of this level resumes once the layer is done.
                stack.push(Task::List(list, at + 1));
                match &layer.kind {
                    LayerKind::Raster => out.push(build_record(layer, header, opts, None, psb)?),
                    LayerKind::Group(g) => {
                        out.push(divider_record(header)?);
                        stack.push(Task::Close(layer, g));
                        stack.push(Task::List(&g.children, 0));
                    }
                }
            }
            Task::Close(layer, g) => out.push(build_record(layer, header, opts, Some(g), psb)?),
        }
    }
    Ok(())
}

/// The hidden `</Layer group>` record that closes a group.
fn divider_record(header: PsdHeader) -> PsdResult<Record> {
    let mut extra = Sink::new();
    extra.u32(0); // no mask
    extra.u32(0); // no blending ranges
    extra.pascal_string(GROUP_DIVIDER_NAME, 4);
    let mut lsct = Sink::new();
    lsct.u32(3);
    write_block(&mut extra, b"8BIM", b"lsct", lsct.as_slice(), false);
    let mut luni = Sink::new();
    luni.unicode_string(GROUP_DIVIDER_NAME);
    write_block(&mut extra, b"8BIM", b"luni", luni.as_slice(), false);

    Ok(Record {
        bounds: Rect::default(),
        channels: empty_channels(header),
        blend_key: *b"norm",
        opacity: 255,
        clipping: 0,
        flags: 0b1000,
        extra: extra.into_inner(),
    })
}

/// The channel set Photoshop gives a record that carries no pixels: one
/// transparency channel plus the colour channels, each holding nothing but a
/// compression code.
fn empty_channels(header: PsdHeader) -> Vec<(i16, Vec<u8>)> {
    let mut out = vec![(CHANNEL_ALPHA, vec![0u8, 0])];
    for id in header.color_mode.channel_ids() {
        out.push((*id, vec![0u8, 0]));
    }
    out
}

fn build_record(
    layer: &PsdLayer,
    header: PsdHeader,
    opts: &WriteOptions,
    group: Option<&GroupData>,
    psb: bool,
) -> PsdResult<Record> {
    let mut channels = Vec::with_capacity(layer.channels.len() + 2);
    if layer.channels.is_empty() {
        if !layer.bounds.is_empty() {
            return Err(PsdError::InvalidDocument(format!(
                "layer {:?} has a {}x{} rectangle but no channel data",
                layer.name,
                layer.bounds.width(),
                layer.bounds.height()
            )));
        }
        channels = empty_channels(header);
    } else {
        let shape = ChannelShape::new(layer.bounds.width(), layer.bounds.height(), header.depth);
        for channel in &layer.channels {
            channels.push((
                channel.id,
                channel_payload(&channel.data, shape, opts, &layer.name, psb)?,
            ));
        }
    }

    if let Some(mask) = &layer.mask {
        let shape = ChannelShape::new(mask.bounds.width(), mask.bounds.height(), header.depth);
        channels.push((
            CHANNEL_USER_MASK,
            channel_payload(&mask.data, shape, opts, &layer.name, psb)?,
        ));
        if let Some(real) = &mask.real {
            let shape = ChannelShape::new(real.bounds.width(), real.bounds.height(), header.depth);
            channels.push((
                CHANNEL_REAL_USER_MASK,
                channel_payload(&real.data, shape, opts, &layer.name, psb)?,
            ));
        }
    }

    let blend_key = match group {
        Some(g) if g.pass_through => PASS_THROUGH,
        _ => key_from_blend(layer.blend_mode),
    };

    let mut flags = 0b1000u8; // "bit 4 has useful information", set since 5.0
    if layer.transparency_protected {
        flags |= 0b1;
    }
    if !layer.visible {
        flags |= 0b10;
    }
    if layer.pixel_data_irrelevant {
        flags |= 0b1_0000;
    }

    Ok(Record {
        bounds: layer.bounds,
        channels,
        blend_key,
        opacity: layer.opacity,
        clipping: u8::from(layer.clipping),
        flags,
        extra: build_extra(layer, group, psb),
    })
}

fn channel_payload(
    data: &[u8],
    shape: ChannelShape,
    opts: &WriteOptions,
    layer_name: &str,
    psb: bool,
) -> PsdResult<Vec<u8>> {
    let expected = shape.byte_len()?;
    if data.len() != expected {
        return Err(PsdError::InvalidDocument(format!(
            "layer {layer_name:?} has a channel of {} bytes where its \
             {}x{} rectangle needs {expected}",
            data.len(),
            shape.width,
            shape.height
        )));
    }
    let mut sink = Sink::new();
    sink.u16(opts.layer_compression.code());
    sink.bytes(&encode_channel_as(
        data,
        opts.layer_compression,
        shape,
        psb,
    )?);
    Ok(sink.into_inner())
}

fn build_extra(layer: &PsdLayer, group: Option<&GroupData>, psb: bool) -> Vec<u8> {
    let mut sink = Sink::new();
    write_mask(&mut sink, layer.mask.as_ref());
    sink.u32(layer.blending_ranges.len() as u32);
    sink.bytes(&layer.blending_ranges);
    sink.pascal_string(&layer.name, 4);

    if let Some(g) = group {
        let mut body = Sink::new();
        body.u32(if g.open { 1 } else { 2 });
        body.tag(b"8BIM");
        body.tag(&if g.pass_through {
            PASS_THROUGH
        } else {
            key_from_blend(layer.blend_mode)
        });
        write_block(&mut sink, b"8BIM", b"lsct", body.as_slice(), psb);
    }

    let mut luni = Sink::new();
    luni.unicode_string(&layer.name);
    write_block(&mut sink, b"8BIM", b"luni", luni.as_slice(), psb);

    if let Some(id) = layer.layer_id {
        let mut body = Sink::new();
        body.u32(id);
        write_block(&mut sink, b"8BIM", b"lyid", body.as_slice(), psb);
    }
    if let Some(color) = layer.sheet_color {
        let mut body = Sink::new();
        body.u16(color);
        body.zeros(6);
        write_block(&mut sink, b"8BIM", b"lclr", body.as_slice(), psb);
    }
    if let Some(fill) = layer.fill_opacity {
        let mut body = Sink::new();
        body.u8(fill);
        body.zeros(3);
        write_block(&mut sink, b"8BIM", b"iOpa", body.as_slice(), psb);
    }
    if !layer.protection.is_default() {
        let mut body = Sink::new();
        body.u32(layer.protection.to_bits());
        write_block(&mut sink, b"8BIM", b"lspf", body.as_slice(), psb);
    }
    if let Some(adj) = &layer.adjustment {
        write_block(&mut sink, b"8BIM", &adj.key, &adj.data, psb);
    }
    if let Some(fx) = &layer.effects {
        write_block(&mut sink, b"8BIM", &fx.key, &fx.data, psb);
    }
    if let Some(text) = &layer.text {
        write_block(&mut sink, b"8BIM", b"TySh", &text.raw, psb);
    }
    for block in &layer.extra {
        write_block(&mut sink, &block.signature, &block.key, &block.data, psb);
    }
    sink.into_inner()
}

/// The layer mask / adjustment layer data: 0, 20 or 36 bytes.
fn write_mask(sink: &mut Sink, mask: Option<&PsdMask>) {
    let Some(mask) = mask else {
        sink.u32(0);
        return;
    };
    let slot = sink.begin_len();
    sink.i32(mask.bounds.top);
    sink.i32(mask.bounds.left);
    sink.i32(mask.bounds.bottom);
    sink.i32(mask.bounds.right);
    sink.u8(mask.default_color);
    // A non-finite feather is treated as default on write: a hostile file
    // must not produce a parameter block the model then refuses.
    let feather = if mask.feather_px.is_finite() {
        mask.feather_px
    } else {
        0.0
    };
    // W9-G: the vector mask's pair, written only when set and meaningful.
    let vector_density = mask.vector_density.filter(|d| *d != 255);
    let vector_feather = mask
        .vector_feather_px
        .filter(|f| f.is_finite() && *f != 0.0);
    let has_params = mask.density != 255
        || feather != 0.0
        || vector_density.is_some()
        || vector_feather.is_some();
    let flags = mask_flags(
        mask.relative_to_layer,
        mask.disabled,
        mask.invert,
        mask.from_render,
    ) | if has_params { 0b1_0000 } else { 0 };
    sink.u8(flags);
    if has_params {
        // The user-mask pair, then (W9-G) the vector-mask pair — in the
        // order the flag bits name them.
        sink.u8(u8::from(mask.density != 255)
            | (u8::from(feather != 0.0) << 1)
            | (u8::from(vector_density.is_some()) << 2)
            | (u8::from(vector_feather.is_some()) << 3));
        if mask.density != 255 {
            sink.u8(mask.density);
        }
        if feather != 0.0 {
            sink.f64(feather);
        }
        if let Some(d) = vector_density {
            sink.u8(d);
        }
        if let Some(f) = vector_feather {
            sink.f64(f);
        }
    }
    match &mask.real {
        Some(real) => {
            sink.u8(mask_flags(
                real.relative_to_layer,
                real.disabled,
                real.invert,
                false,
            ));
            sink.u8(real.default_color);
            sink.i32(real.bounds.top);
            sink.i32(real.bounds.left);
            sink.i32(real.bounds.bottom);
            sink.i32(real.bounds.right);
        }
        // The 20-byte form ends with two bytes of padding.
        None => sink.zeros(2),
    }
    sink.end_len(slot);
}

/// Bits 0–3 of the mask flags byte. Bit 4 — "mask parameters follow" — is set
/// by [`write_mask`] itself, only when there are parameters to write: a flag
/// promising bytes that are not there desynchronises every reader that
/// believes it.
fn mask_flags(relative: bool, disabled: bool, invert: bool, from_render: bool) -> u8 {
    u8::from(relative)
        | (u8::from(disabled) << 1)
        | (u8::from(invert) << 2)
        | (u8::from(from_render) << 3)
}

/// A document-level tagged block, padded to a four-byte boundary.
///
/// W9-M: readers disagree about the padding after a block (see
/// [`crate::read::read_tagged_blocks`]); psd-tools reads the document-level
/// ones as padded to four and so walks off the next signature — losing, for
/// one, the `lnk2` block a smart object's pixels live in — when a block's
/// length is two past a multiple of four (an `Lr16` section often is). The
/// padding sits outside the declared length, which every reader tolerates:
/// this crate's reader resynchronises on the next signature.
fn write_global_block(sink: &mut Sink, signature: &[u8; 4], key: &[u8; 4], data: &[u8], psb: bool) {
    write_block(sink, signature, key, data, psb);
    // `write_block` already padded an odd length by one byte; make the pad
    // after the data reach the next multiple of four of its LENGTH (which is
    // how psd-tools computes it), not of the file offset.
    let pad = (4 - data.len() % 4) % 4;
    sink.zeros(pad - data.len() % 2);
}

/// W11-H: in a `.psb` a long-key block ([`crate::read::PSB_LONG_KEYS`]) carries a
/// 64-bit length; every other block keeps its 32-bit one.
fn write_block(sink: &mut Sink, signature: &[u8; 4], key: &[u8; 4], data: &[u8], psb: bool) {
    sink.tag(signature);
    sink.tag(key);
    put_len(sink, data.len(), psb && PSB_LONG_KEYS.contains(key));
    sink.bytes(data);
    if data.len() % 2 == 1 {
        sink.u8(0);
    }
}

/// Build a document from a single full-canvas 8-bit RGBA image.
///
/// A convenience for the common "save this picture as a `.psd`" case, and the
/// shortest path to a file that is valid in both Photoshop and Photopea.
pub fn from_rgba8(width: u32, height: u32, rgba: &[u8]) -> PsdResult<PsdFile> {
    let header = PsdHeader::rgba8(width, height);
    let mut file = PsdFile::new(header);
    let mut layer = PsdLayer::raster("Layer 1", Rect::sized(width, height));
    layer.set_rgba8(rgba)?;
    file.layers.push(layer);
    file.merged = Some(MergedImage::from_rgba8(width, height, rgba)?);
    Ok(file)
}
