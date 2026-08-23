//! Photoshop `.psd` reading and writing.
//!
//! An original implementation written from the published format
//! documentation. Nothing here is derived from another project's source.
//!
//! ```
//! use psd::{PsdFile, PsdHeader, PsdLayer, Rect};
//!
//! let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
//! let mut layer = PsdLayer::raster("Background", Rect::sized(2, 2));
//! layer.set_rgba8(&[255, 0, 0, 255].repeat(4))?;
//! file.layers.push(layer);
//!
//! let bytes = psd::write(&file)?;
//! let reopened = psd::read(&bytes)?;
//! assert_eq!(reopened.layers[0].name, "Background");
//! # Ok::<(), psd::PsdError>(())
//! ```
//!
//! # What is covered
//!
//! * The file header, colour mode data, image resources, and the layer and mask
//!   information section.
//! * Layer records: bounds, per-channel data, blend mode, opacity and fill
//!   opacity, clipping, the visibility and lock flags, and names in both the
//!   legacy Pascal form and the `luni` Unicode form.
//! * Groups, via the `lsct` section dividers, restored to a real tree — see
//!   [`model`] for why that is the part most worth testing.
//! * All four channel encodings: raw, RLE (PackBits), ZIP, and ZIP with
//!   per-row delta prediction, at 8, 16 and 32 bits per sample.
//! * Layer masks, including the mask's own rectangle, its default colour
//!   outside that rectangle, its flags, and the second "real" mask Photoshop
//!   keeps when a vector mask and a raster mask coexist.
//! * The merged composite, read and written; [`flatten`] synthesises one from
//!   the layer tree when the caller has none.
//! * Adjustment layers, layer effects and type layers, recognised by key and
//!   preserved byte for byte, with a full [`descriptor`] parser for reading
//!   their contents and [`text`] for pulling the string out of a type layer.
//! * Greyscale and RGB at 8, 16 and 32 bits. CMYK, Lab, Indexed, Duotone,
//!   Multichannel and Bitmap are **refused by name** rather than approximated,
//!   because reading their samples as RGB produces pixels that are silently
//!   wrong.
//!
//! # Known gaps
//!
//! These are deliberate, and none of them silently corrupts a file:
//!
//! * **PSB** (`.psb`, version 2) is refused with
//!   [`PsdError::UnsupportedVersion`]; its 64-bit section lengths are a
//!   different parse.
//! * **Mask parameters** (per-mask density and feather, flag bit 4) are skipped
//!   on read, and the writer never claims to have them. A file that had them
//!   loses them on a save; nothing is misread.
//! * **Adjustment payloads** are preserved but not decoded into
//!   `layer_model::AdjustmentKind`; [`model::Adjustment::descriptor`] gives a
//!   caller the parsed tree to do it from.
//! * **Type layers are read, not synthesised.** [`text`] extracts the string
//!   and transform, and a type layer round-trips because its `TySh` block is
//!   written back verbatim, but this crate will not build one from scratch:
//!   Photoshop discards a type layer whose engine data does not describe every
//!   character run, so a half-built block is worse than none.
//! * **The fallback compositor in [`flatten`] ignores clipping groups, layer
//!   effects and adjustment layers.** Callers with a real renderer put its
//!   output in [`model::PsdFile::merged`] instead.
//!
//! # Untrusted input
//!
//! A `.psd` arrives from someone else, and every length, count and offset in
//! it is attacker-controlled. Parsing therefore validates before it allocates,
//! uses checked arithmetic, and returns typed errors — a malformed file must
//! never panic, hang, or exhaust memory. Concretely:
//!
//! * Every section is parsed through a **sub-cursor** carved to the length the
//!   file declared ([`bytes::Cursor::sub`]), so a section that lies about its
//!   contents can only damage itself.
//! * Every count is checked against [`limits::ReadOptions`] **before** the
//!   `Vec` that would hold it is reserved, and counts are additionally checked
//!   against the bytes actually remaining — four billion layers do not reserve
//!   four billion records.
//! * Every decoded channel is drawn from one shared [`limits::Budget`], because
//!   per-field ceilings cannot stop eight thousand individually-reasonable
//!   layers from adding up to a hostile total.
//! * ZIP channels inflate through a `take` capped one byte past the size the
//!   channel's geometry requires, so a decompression bomb is refused after one
//!   extra byte.
//! * Descriptor parsing **and group nesting** are both depth-limited
//!   ([`limits::ReadOptions::max_descriptor_depth`],
//!   [`limits::ReadOptions::max_group_depth`]), because a stack overflow is an
//!   abort rather than an error and would take the host application with it.
//!   A group costs only two layer records, so the layer-count ceiling alone
//!   allows a tree thousands of levels deep, and the tree outlives the parse:
//!   walking, writing, flattening and even *dropping* it would each have to be
//!   stack-safe forever. They are — every one of those is written with an
//!   explicit stack, including [`model::GroupData`]'s `Drop` — and the depth is
//!   capped as well, because defence in one layer is not defence.
//! * [`flatten`] takes its canvas size from a [`header::PsdHeader`], which a
//!   caller can hand over without any file behind it, so it draws every canvas
//!   from a byte budget and refuses before it reserves. See [`flatten_with`].
//! * **No index depends on an invariant held somewhere else.** Every field the
//!   parser reads comes through [`bytes::Cursor`], which returns
//!   [`PsdError::Truncated`] rather than panicking when a read would run past
//!   the end of its section. Where a byte-for-byte inner loop does index
//!   directly — PackBits, the ZIP row predictor, the channel interleavers, the
//!   fixed-size resource payloads — the bound is established in that same
//!   function, from a length it is holding: the 32-bit row predictor, for one,
//!   clamps the width it was passed to the row it was handed rather than trust
//!   that its caller checked. Nothing is indexed on the strength
//!   of a count some *other* function promised to produce, because the release
//!   profile sets `panic = "abort"`: a panic in this crate is not an error the
//!   host application can catch. See [`codec`] for the one place that rule cost
//!   something, and what it bought.
//! * The rule is a crate rule, not a parser rule, because [`flatten`] runs on
//!   the same untrusted sizes. Its canvas is two planes — colour and alpha —
//!   allocated to one length in one place, and the compositor never indexes
//!   either on the strength of the other's length or of the canvas's declared
//!   width and height: it takes both planes together from one iterator, or
//!   fetches a pixel and gets `None` past the end. A canvas that somehow came
//!   back the wrong length would compose fewer pixels; it would not abort the
//!   process.
//!
//! The one recursion left is the *derived* `Clone`, `PartialEq` and `Debug` on
//! [`model::PsdLayer`]. A tree that came from a file is depth-limited, so those
//! are safe on anything this crate parses; a caller that assembles a
//! thousand-level tree by hand and then clones it is on its own.

pub mod blend;
pub mod bytes;
pub mod codec;
pub mod descriptor;
pub mod error;
pub mod flatten;
pub mod header;
pub mod limits;
pub mod model;
pub mod packbits;
pub mod read;
pub mod resource;
pub mod text;
pub mod write;
pub mod zip;

pub use blend::{blend_from_key, key_from_blend, PASS_THROUGH};
pub use codec::{ChannelShape, Compression};
pub use descriptor::{Descriptor, RefItem, Value};
pub use error::{PsdError, PsdResult};
pub use flatten::{empty_merged, empty_merged_with, flatten, flatten_with};
pub use header::{ColorMode, Depth, PsdHeader};
pub use limits::{ReadOptions, WriteOptions};
pub use model::{
    Adjustment, AdjustmentKey, Channel, Effects, GroupData, ImageResource, LayerKind, MergedImage,
    Protection, PsdFile, PsdLayer, PsdMask, RealMask, Rect, TaggedBlock, TextData, CHANNEL_ALPHA,
    CHANNEL_REAL_USER_MASK, CHANNEL_USER_MASK,
};
pub use read::{read, read_with};
pub use write::{from_rgba8, write, write_with};

#[cfg(test)]
mod probe;
#[cfg(test)]
mod tests;
