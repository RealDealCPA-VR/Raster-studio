//! The 26-byte file header, and the colour mode / bit depth vocabulary.
//!
//! ```text
//! '8BPS'  u16 version  6 zero bytes  u16 channels
//! u32 height  u32 width  u16 depth  u16 colour-mode
//! ```
//!
//! Note the order: **height before width**. Getting that backwards produces a
//! file that opens, looks plausible on a square canvas, and is wrong on every
//! other one, so [`PsdHeader::read`] and [`PsdHeader::write`] are covered by a
//! round-trip test on a deliberately non-square document.

use serde::{Deserialize, Serialize};

use crate::bytes::{Cursor, Sink};
use crate::error::{PsdError, PsdResult};
use crate::limits::{check_limit, ReadOptions};

/// The four-byte magic every `.psd` starts with.
pub const SIGNATURE: [u8; 4] = *b"8BPS";

/// Version 1 is `.psd`. Version 2 is `.psb`, whose section lengths are 64-bit.
/// [`PsdHeader::read`] refuses version 2 by name rather than misreading it;
/// W10-F: [`PsdHeader::read_any`] accepts both and says which it read, and
/// [`crate::read::read_with`] reads a `.psb` through it.
pub const VERSION_PSD: u16 = 1;
pub const VERSION_PSB: u16 = 2;

/// The colour models this crate handles.
///
/// W16-B: every mode Photoshop defines is read. Greyscale and RGB samples are
/// what they say; the others are *not* RGB and must never be interpreted as
/// such — [`crate::colour_modes`] decodes them (CMYK inverted per Adobe, Lab
/// offset-encoded, Indexed through the palette in the colour mode data,
/// Bitmap unpacked from 1 bit, Duotone as its greyscale base). Variants are
/// appended, never reordered (serde). Codes Photoshop never assigned are
/// refused by name.
///
/// A [`ColorMode::Bitmap`] header is held **expanded**: its [`PsdHeader::depth`]
/// is [`Depth::Eight`] in memory and its one channel holds `0` (black) or
/// `255` (white); on disk the depth is 1 and the rows are packed bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorMode {
    Grayscale,
    Rgb,
    Bitmap,
    Indexed,
    Cmyk,
    Multichannel,
    Duotone,
    Lab,
}

impl ColorMode {
    /// The code stored in the header.
    pub const fn code(self) -> u16 {
        match self {
            ColorMode::Bitmap => 0,
            ColorMode::Grayscale => 1,
            ColorMode::Indexed => 2,
            ColorMode::Rgb => 3,
            ColorMode::Cmyk => 4,
            ColorMode::Multichannel => 7,
            ColorMode::Duotone => 8,
            ColorMode::Lab => 9,
        }
    }

    /// How many colour channels a layer or the composite carries, before any
    /// alpha channel.
    ///
    /// A Multichannel document's channels are all colour (inks); `1` is the
    /// least one can hold.
    pub const fn color_channels(self) -> u16 {
        match self {
            ColorMode::Grayscale
            | ColorMode::Bitmap
            | ColorMode::Indexed
            | ColorMode::Multichannel
            | ColorMode::Duotone => 1,
            ColorMode::Rgb | ColorMode::Lab => 3,
            ColorMode::Cmyk => 4,
        }
    }

    /// Channel ids for the colour channels, in the order Photoshop stores them.
    pub const fn channel_ids(self) -> &'static [i16] {
        match self {
            ColorMode::Grayscale
            | ColorMode::Bitmap
            | ColorMode::Indexed
            | ColorMode::Multichannel
            | ColorMode::Duotone => &[0],
            ColorMode::Rgb | ColorMode::Lab => &[0, 1, 2],
            ColorMode::Cmyk => &[0, 1, 2, 3],
        }
    }

    pub fn from_code(code: u16) -> PsdResult<Self> {
        match code {
            0 => Ok(ColorMode::Bitmap),
            1 => Ok(ColorMode::Grayscale),
            2 => Ok(ColorMode::Indexed),
            3 => Ok(ColorMode::Rgb),
            4 => Ok(ColorMode::Cmyk),
            7 => Ok(ColorMode::Multichannel),
            8 => Ok(ColorMode::Duotone),
            9 => Ok(ColorMode::Lab),
            other => Err(PsdError::UnsupportedColorMode {
                code: other,
                name: mode_name(other),
            }),
        }
    }
}

/// The spelling Adobe uses for a colour-mode code, for error messages.
pub const fn mode_name(code: u16) -> &'static str {
    match code {
        0 => "Bitmap",
        1 => "Greyscale",
        2 => "Indexed",
        3 => "RGB",
        4 => "CMYK",
        7 => "Multichannel",
        8 => "Duotone",
        9 => "Lab",
        _ => "unknown",
    }
}

/// Bits per sample. Photoshop also defines 1 (bitmap mode), which this crate
/// refuses along with the bitmap colour mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Depth {
    Eight,
    Sixteen,
    ThirtyTwo,
}

impl Depth {
    pub const fn bits(self) -> u16 {
        match self {
            Depth::Eight => 8,
            Depth::Sixteen => 16,
            Depth::ThirtyTwo => 32,
        }
    }

    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Depth::Eight => 1,
            Depth::Sixteen => 2,
            Depth::ThirtyTwo => 4,
        }
    }

    pub fn from_bits(bits: u16) -> PsdResult<Self> {
        match bits {
            8 => Ok(Depth::Eight),
            16 => Ok(Depth::Sixteen),
            32 => Ok(Depth::ThirtyTwo),
            other => Err(PsdError::UnsupportedDepth(other)),
        }
    }
}

/// The file header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsdHeader {
    /// Total channels in the *merged composite*, including alpha and any spot
    /// channels. Layers declare their own channel counts separately.
    pub channels: u16,
    pub width: u32,
    pub height: u32,
    pub depth: Depth,
    pub color_mode: ColorMode,
}

impl PsdHeader {
    /// An 8-bit RGB header with alpha.
    pub fn rgba8(width: u32, height: u32) -> Self {
        PsdHeader {
            channels: 4,
            width,
            height,
            depth: Depth::Eight,
            color_mode: ColorMode::Rgb,
        }
    }

    /// `true` when the merged composite carries an alpha channel.
    ///
    /// W16-B: never for Multichannel, whose every channel is an ink.
    pub fn has_alpha(&self) -> bool {
        self.color_mode != ColorMode::Multichannel
            && self.channels > self.color_mode.color_channels()
    }

    /// Samples in one channel of the composite.
    pub fn canvas_pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// Read a version-1 (`.psd`) header; a `.psb` is refused by name.
    pub fn read(cur: &mut Cursor<'_>, opts: &ReadOptions) -> PsdResult<Self> {
        let start = cur.clone();
        match Self::read_any(cur, opts)? {
            (header, false) => Ok(header),
            (_, true) => {
                *cur = start;
                Err(PsdError::UnsupportedVersion(VERSION_PSB))
            }
        }
    }

    /// W10-F: read a `.psd` or a `.psb` header, returning `true` for a
    /// `.psb`. A `.psb` canvas is bounded by
    /// [`ReadOptions::max_psb_dimension`] instead of
    /// [`ReadOptions::max_dimension`].
    pub fn read_any(cur: &mut Cursor<'_>, opts: &ReadOptions) -> PsdResult<(Self, bool)> {
        cur.expect_tag(&SIGNATURE, "8BPS")?;
        let version = cur.u16()?;
        let psb = match version {
            VERSION_PSD => false,
            VERSION_PSB => true,
            other => return Err(PsdError::UnsupportedVersion(other)),
        };
        let max_dimension = if psb {
            opts.max_psb_dimension
        } else {
            opts.max_dimension
        };
        cur.skip(6)?; // reserved, must be zero; tolerated if not
        let channels = cur.u16()?;
        check_limit("header channel count", u64::from(channels), 56)?;
        let height = cur.u32()?;
        let width = cur.u32()?;
        check_limit("canvas height", u64::from(height), u64::from(max_dimension))?;
        check_limit("canvas width", u64::from(width), u64::from(max_dimension))?;
        // Bounded from below as well as from above. `check_limit` only refuses
        // an absurdly *large* canvas; a 0 x N one parsed cleanly all the way
        // through and then made `write` refuse the document with an
        // `InvalidDocument` — an error whose `is_file_fault` is false, blaming
        // the caller for a file they did not write. Photoshop cannot produce a
        // zero-area canvas, so the reader is the lenient side and this is where
        // the two are brought back into agreement.
        if width == 0 || height == 0 {
            return Err(PsdError::EmptyCanvas { width, height });
        }
        let bits = cur.u16()?;
        let color_mode = ColorMode::from_code(cur.u16()?)?;
        // W16-B: a Bitmap file is 1 bit per sample on disk and is held
        // expanded to 8 in memory (see [`ColorMode`]); 1 bit is legal for no
        // other mode, and Bitmap for no other depth. Indexed is 8-bit only.
        let depth = match (color_mode, bits) {
            (ColorMode::Bitmap, 1) => Depth::Eight,
            (ColorMode::Bitmap, other) => return Err(PsdError::UnsupportedDepth(other)),
            (ColorMode::Indexed, 8) => Depth::Eight,
            (ColorMode::Indexed, other) => return Err(PsdError::UnsupportedDepth(other)),
            (_, bits) => Depth::from_bits(bits)?,
        };
        let min = color_mode.color_channels();
        if channels < min {
            return Err(PsdError::ChannelCountTooSmall {
                declared: channels,
                min,
            });
        }
        Ok((
            PsdHeader {
                channels,
                width,
                height,
                depth,
                color_mode,
            },
            psb,
        ))
    }

    pub fn write(&self, sink: &mut Sink) {
        self.write_as(sink, false);
    }

    /// W11-H: write a version-1 (`.psd`) header, or a version-2 (`.psb`) one
    /// when `psb`; the two differ only in the version field.
    pub fn write_as(&self, sink: &mut Sink, psb: bool) {
        sink.tag(&SIGNATURE);
        sink.u16(if psb { VERSION_PSB } else { VERSION_PSD });
        sink.zeros(6);
        sink.u16(self.channels);
        sink.u32(self.height);
        sink.u32(self.width);
        // W16-B: a Bitmap header is 1 bit on disk (see [`ColorMode`]).
        sink.u16(if self.color_mode == ColorMode::Bitmap {
            1
        } else {
            self.depth.bits()
        });
        sink.u16(self.color_mode.code());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(h: PsdHeader) -> PsdHeader {
        let mut s = Sink::new();
        h.write(&mut s);
        let buf = s.into_inner();
        assert_eq!(buf.len(), 26, "the header is a fixed 26 bytes");
        let mut c = Cursor::new(&buf);
        PsdHeader::read(&mut c, &ReadOptions::default()).unwrap()
    }

    #[test]
    fn width_and_height_do_not_swap_on_a_non_square_canvas() {
        let h = PsdHeader {
            channels: 4,
            width: 640,
            height: 128,
            depth: Depth::Sixteen,
            color_mode: ColorMode::Rgb,
        };
        assert_eq!(round_trip(h), h);
    }

    #[test]
    fn every_supported_depth_and_mode_round_trips() {
        for depth in [Depth::Eight, Depth::Sixteen, Depth::ThirtyTwo] {
            for mode in [ColorMode::Grayscale, ColorMode::Rgb] {
                let h = PsdHeader {
                    channels: mode.color_channels() + 1,
                    width: 7,
                    height: 3,
                    depth,
                    color_mode: mode,
                };
                assert_eq!(round_trip(h), h, "{depth:?} {mode:?}");
            }
        }
    }

    /// W16-B: every mode Photoshop defines is recognised; only codes it
    /// never assigned (5, 6, 10+) are refused by name.
    #[test]
    fn every_photoshop_mode_code_is_recognised_and_unknown_codes_are_refused() {
        for (code, mode) in [
            (0, ColorMode::Bitmap),
            (1, ColorMode::Grayscale),
            (2, ColorMode::Indexed),
            (3, ColorMode::Rgb),
            (4, ColorMode::Cmyk),
            (7, ColorMode::Multichannel),
            (8, ColorMode::Duotone),
            (9, ColorMode::Lab),
        ] {
            assert_eq!(ColorMode::from_code(code).unwrap(), mode);
            assert_eq!(mode.code(), code);
            assert_eq!(mode.channel_ids().len(), mode.color_channels() as usize);
        }
        for code in [5u16, 6, 10, 0xffff] {
            assert!(matches!(
                ColorMode::from_code(code).unwrap_err(),
                PsdError::UnsupportedColorMode { code: c, .. } if c == code
            ));
        }
    }

    /// W16-B: the CMYK, Lab, Indexed, Multichannel and Duotone headers
    /// round-trip; a Bitmap one is 1 bit on disk and 8 in memory.
    #[test]
    fn the_print_modes_round_trip_and_bitmap_is_one_bit_on_disk() {
        for (mode, depth) in [
            (ColorMode::Cmyk, Depth::Eight),
            (ColorMode::Cmyk, Depth::Sixteen),
            (ColorMode::Lab, Depth::Sixteen),
            (ColorMode::Indexed, Depth::Eight),
            (ColorMode::Multichannel, Depth::Eight),
            (ColorMode::Duotone, Depth::Sixteen),
            (ColorMode::Bitmap, Depth::Eight),
        ] {
            let h = PsdHeader {
                channels: mode.color_channels(),
                width: 9,
                height: 2,
                depth,
                color_mode: mode,
            };
            assert_eq!(round_trip(h), h, "{mode:?} {depth:?}");
        }
        let mut s = Sink::new();
        PsdHeader {
            channels: 1,
            width: 9,
            height: 2,
            depth: Depth::Eight,
            color_mode: ColorMode::Bitmap,
        }
        .write(&mut s);
        assert_eq!(&s.into_inner()[22..26], &[0, 1, 0, 0], "1 bit, mode 0");
    }

    #[test]
    fn one_bit_depth_is_refused_outside_bitmap_and_indexed_is_eight_bit_only() {
        assert!(matches!(
            Depth::from_bits(1).unwrap_err(),
            PsdError::UnsupportedDepth(1)
        ));
        let header = |bits: u16, mode: u16| {
            let mut s = Sink::new();
            s.tag(&SIGNATURE);
            s.u16(VERSION_PSD);
            s.zeros(6);
            s.u16(4);
            s.u32(2);
            s.u32(2);
            s.u16(bits);
            s.u16(mode);
            s.into_inner()
        };
        for (bits, mode) in [(1u16, 3u16), (1, 4), (8, 0), (16, 2)] {
            let bytes = header(bits, mode);
            let err =
                PsdHeader::read(&mut Cursor::new(&bytes), &ReadOptions::default()).unwrap_err();
            assert!(
                matches!(err, PsdError::UnsupportedDepth(b) if b == bits),
                "{bits}-bit mode {mode}: {err}"
            );
        }
    }

    #[test]
    fn psb_is_refused_by_version_rather_than_misread() {
        let mut s = Sink::new();
        s.tag(&SIGNATURE);
        s.u16(VERSION_PSB);
        s.zeros(6);
        s.u16(4);
        s.u32(10);
        s.u32(10);
        s.u16(8);
        s.u16(3);
        let buf = s.into_inner();
        let err = PsdHeader::read(&mut Cursor::new(&buf), &ReadOptions::default()).unwrap_err();
        assert!(matches!(err, PsdError::UnsupportedVersion(2)), "{err}");
    }

    #[test]
    fn an_absurd_canvas_size_is_refused_before_anything_is_allocated() {
        let mut s = Sink::new();
        s.tag(&SIGNATURE);
        s.u16(VERSION_PSD);
        s.zeros(6);
        s.u16(4);
        s.u32(u32::MAX);
        s.u32(u32::MAX);
        s.u16(8);
        s.u16(3);
        let buf = s.into_inner();
        let err = PsdHeader::read(&mut Cursor::new(&buf), &ReadOptions::default()).unwrap_err();
        assert!(matches!(err, PsdError::LimitExceeded { .. }), "{err}");
    }

    /// A header with a zero edge is refused on the way *in*.
    ///
    /// Without this the file read cleanly and then `write` refused the document
    /// it produced, which is the one asymmetry the crate promises not to have:
    /// the refusal has to blame the file, so `is_file_fault` must be true.
    #[test]
    fn a_zero_edge_canvas_is_refused_by_the_reader_and_blamed_on_the_file() {
        for (width, height) in [(0u32, 0u32), (0, 5), (5, 0)] {
            let mut s = Sink::new();
            s.tag(&SIGNATURE);
            s.u16(VERSION_PSD);
            s.zeros(6);
            s.u16(4);
            s.u32(height);
            s.u32(width);
            s.u16(8);
            s.u16(3);
            let buf = s.into_inner();
            let err = PsdHeader::read(&mut Cursor::new(&buf), &ReadOptions::default())
                .expect_err("a zero-area canvas must be refused");
            assert!(err.is_file_fault(), "{width}x{height}: {err}");
            match err {
                PsdError::EmptyCanvas {
                    width: w,
                    height: h,
                } => assert_eq!((w, h), (width, height)),
                other => panic!("wrong error for {width}x{height}: {other}"),
            }
        }
        // One pixel each way is still the smallest legal canvas, not a refusal.
        let h = PsdHeader::rgba8(1, 1);
        assert_eq!(round_trip(h), h);
    }

    #[test]
    fn an_rgb_header_declaring_two_channels_is_refused() {
        let mut s = Sink::new();
        s.tag(&SIGNATURE);
        s.u16(VERSION_PSD);
        s.zeros(6);
        s.u16(2);
        s.u32(4);
        s.u32(4);
        s.u16(8);
        s.u16(3);
        let buf = s.into_inner();
        let err = PsdHeader::read(&mut Cursor::new(&buf), &ReadOptions::default()).unwrap_err();
        assert!(
            matches!(
                err,
                PsdError::ChannelCountTooSmall {
                    declared: 2,
                    min: 3
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn a_wrong_magic_reports_the_signature_it_found() {
        let buf = *b"8BIM\0\x01other bytes here...........";
        let err = PsdHeader::read(&mut Cursor::new(&buf), &ReadOptions::default()).unwrap_err();
        match err {
            PsdError::BadSignature { found, at, .. } => {
                assert_eq!(found, "8BIM");
                assert_eq!(at, 0);
            }
            other => panic!("wrong error: {other}"),
        }
    }
}
