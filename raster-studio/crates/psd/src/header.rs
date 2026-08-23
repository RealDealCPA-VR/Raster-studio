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

/// Version 1 is `.psd`. Version 2 is `.psb`, whose section lengths are 64-bit;
/// this build refuses it by name rather than misreading it.
pub const VERSION_PSD: u16 = 1;
pub const VERSION_PSB: u16 = 2;

/// The colour models this crate handles.
///
/// The rejected modes are rejected *by name* rather than approximated, because
/// reading CMYK or Lab samples as if they were RGB produces pixels that are
/// silently, confidently wrong — the worst possible failure for an image tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorMode {
    Grayscale,
    Rgb,
}

impl ColorMode {
    /// The code stored in the header.
    pub const fn code(self) -> u16 {
        match self {
            ColorMode::Grayscale => 1,
            ColorMode::Rgb => 3,
        }
    }

    /// How many colour channels a layer or the composite carries, before any
    /// alpha channel.
    pub const fn color_channels(self) -> u16 {
        match self {
            ColorMode::Grayscale => 1,
            ColorMode::Rgb => 3,
        }
    }

    /// Channel ids for the colour channels, in the order Photoshop stores them.
    pub const fn channel_ids(self) -> &'static [i16] {
        match self {
            ColorMode::Grayscale => &[0],
            ColorMode::Rgb => &[0, 1, 2],
        }
    }

    pub fn from_code(code: u16) -> PsdResult<Self> {
        match code {
            1 => Ok(ColorMode::Grayscale),
            3 => Ok(ColorMode::Rgb),
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
    pub fn has_alpha(&self) -> bool {
        self.channels > self.color_mode.color_channels()
    }

    /// Samples in one channel of the composite.
    pub fn canvas_pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    pub fn read(cur: &mut Cursor<'_>, opts: &ReadOptions) -> PsdResult<Self> {
        cur.expect_tag(&SIGNATURE, "8BPS")?;
        let version = cur.u16()?;
        if version != VERSION_PSD {
            return Err(PsdError::UnsupportedVersion(version));
        }
        cur.skip(6)?; // reserved, must be zero; tolerated if not
        let channels = cur.u16()?;
        check_limit("header channel count", u64::from(channels), 56)?;
        let height = cur.u32()?;
        let width = cur.u32()?;
        check_limit(
            "canvas height",
            u64::from(height),
            u64::from(opts.max_dimension),
        )?;
        check_limit(
            "canvas width",
            u64::from(width),
            u64::from(opts.max_dimension),
        )?;
        let depth = Depth::from_bits(cur.u16()?)?;
        let color_mode = ColorMode::from_code(cur.u16()?)?;
        let min = color_mode.color_channels();
        if channels < min {
            return Err(PsdError::ChannelCountTooSmall {
                declared: channels,
                min,
            });
        }
        Ok(PsdHeader {
            channels,
            width,
            height,
            depth,
            color_mode,
        })
    }

    pub fn write(&self, sink: &mut Sink) {
        sink.tag(&SIGNATURE);
        sink.u16(VERSION_PSD);
        sink.zeros(6);
        sink.u16(self.channels);
        sink.u32(self.height);
        sink.u32(self.width);
        sink.u16(self.depth.bits());
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

    #[test]
    fn cmyk_lab_indexed_and_bitmap_are_refused_by_name() {
        for (code, name) in [(0, "Bitmap"), (2, "Indexed"), (4, "CMYK"), (9, "Lab")] {
            let err = ColorMode::from_code(code).unwrap_err();
            match err {
                PsdError::UnsupportedColorMode { code: c, name: n } => {
                    assert_eq!((c, n), (code, name));
                }
                other => panic!("wrong error for {name}: {other}"),
            }
        }
    }

    #[test]
    fn one_bit_depth_is_refused() {
        assert!(matches!(
            Depth::from_bits(1).unwrap_err(),
            PsdError::UnsupportedDepth(1)
        ));
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
