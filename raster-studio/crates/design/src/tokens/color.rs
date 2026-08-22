//! Device-independent sRGB color plus the WCAG 2.1 relative-luminance math the
//! contrast gate is built on.
//!
//! Nothing in this module depends on `egui`; it is plain data so that the
//! palette can be checked in a unit test without a graphics context.

/// A non-premultiplied 8-bit sRGB color.
///
/// Channel bytes are sRGB-encoded (gamma), *not* linear. `a` is straight
/// (non-premultiplied) alpha.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Srgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Srgba {
    /// Fully transparent black.
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);

    /// Opaque color from channel bytes.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Color from channel bytes with straight alpha.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Opaque color from a `0xRRGGBB` literal. The top byte is ignored.
    pub const fn hex(rgb: u32) -> Self {
        Self::rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }

    /// Color from a `0xRRGGBBAA` literal.
    pub const fn hexa(rgba: u32) -> Self {
        Self::rgba(
            (rgba >> 24) as u8,
            (rgba >> 16) as u8,
            (rgba >> 8) as u8,
            rgba as u8,
        )
    }

    /// The same color at a different straight alpha.
    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// `true` when the color is fully opaque.
    pub const fn is_opaque(self) -> bool {
        self.a == 255
    }

    /// Straight alpha as a 0..=1 factor.
    pub fn alpha_f32(self) -> f32 {
        f32::from(self.a) / 255.0
    }

    /// Source-over composite of `self` onto an **opaque** `background`.
    ///
    /// Blending is done on gamma-encoded bytes, which is what a GPU blender
    /// without an sRGB framebuffer does and therefore what the user sees.
    /// The result is always opaque, so it is safe to feed to
    /// [`Srgba::relative_luminance`].
    pub fn composite_over(self, background: Self) -> Self {
        debug_assert!(
            background.is_opaque(),
            "composite_over requires an opaque background"
        );
        if self.is_opaque() {
            return self;
        }
        let a = self.alpha_f32();
        let mix = |fg: u8, bg: u8| -> u8 {
            (f32::from(fg) * a + f32::from(bg) * (1.0 - a))
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Self::rgb(
            mix(self.r, background.r),
            mix(self.g, background.g),
            mix(self.b, background.b),
        )
    }

    /// WCAG 2.1 relative luminance in 0..=1.
    ///
    /// Alpha is **ignored** — composite with [`Srgba::composite_over`] first if
    /// the color is translucent, otherwise the number is meaningless.
    pub fn relative_luminance(self) -> f32 {
        0.2126 * linearize(self.r) + 0.7152 * linearize(self.g) + 0.0722 * linearize(self.b)
    }
}

/// sRGB transfer function, byte in, linear 0..=1 out (WCAG 2.1 definition).
fn linearize(channel: u8) -> f32 {
    let c = f32::from(channel) / 255.0;
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG 2.1 contrast ratio between two **opaque** colors, in 1.0..=21.0.
///
/// Order does not matter; the lighter color is always the numerator.
pub fn contrast_ratio(a: Srgba, b: Srgba) -> f32 {
    let la = a.relative_luminance();
    let lb = b.relative_luminance();
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Contrast of a possibly translucent `foreground` drawn on an opaque
/// `background`. The foreground is composited first.
pub fn contrast_ratio_over(foreground: Srgba, background: Srgba) -> f32 {
    contrast_ratio(foreground.composite_over(background), background)
}

/// The WCAG AA floor for a piece of text.
///
/// "Large" is >= 18pt regular or >= 14pt bold, per WCAG 2.1 SC 1.4.3.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextSize {
    /// Body-sized text: needs 4.5:1.
    Normal,
    /// Large text: needs 3:1.
    Large,
}

impl TextSize {
    /// Minimum contrast ratio required to pass WCAG AA at this size.
    pub const fn min_contrast_aa(self) -> f32 {
        match self {
            Self::Normal => 4.5,
            Self::Large => 3.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_literals_split_into_channels() {
        assert_eq!(Srgba::hex(0x1B2C3D), Srgba::rgba(0x1B, 0x2C, 0x3D, 0xFF));
        assert_eq!(Srgba::hexa(0x1B2C3D40), Srgba::rgba(0x1B, 0x2C, 0x3D, 0x40));
    }

    #[test]
    fn known_luminances_match_wcag_reference() {
        assert!((Srgba::hex(0xFFFFFF).relative_luminance() - 1.0).abs() < 1e-4);
        assert!(Srgba::hex(0x000000).relative_luminance().abs() < 1e-6);
        // sRGB mid grey #808080 has a documented relative luminance of ~0.2159.
        assert!((Srgba::hex(0x808080).relative_luminance() - 0.2159).abs() < 1e-3);
    }

    #[test]
    fn black_on_white_is_the_maximum_ratio() {
        let r = contrast_ratio(Srgba::hex(0x000000), Srgba::hex(0xFFFFFF));
        assert!((r - 21.0).abs() < 1e-3, "ratio was {r}");
    }

    #[test]
    fn contrast_ratio_is_symmetric_and_bottoms_out_at_one() {
        let a = Srgba::hex(0x123456);
        let b = Srgba::hex(0xEEDDCC);
        assert!((contrast_ratio(a, b) - contrast_ratio(b, a)).abs() < 1e-6);
        assert!((contrast_ratio(a, a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn compositing_resolves_alpha_before_luminance() {
        // 50% black over white is mid grey, whose contrast on white is far
        // below the 21:1 you would get if alpha were ignored.
        let half_black = Srgba::rgba(0, 0, 0, 128);
        let white = Srgba::hex(0xFFFFFF);
        let composited = half_black.composite_over(white);
        assert_eq!(composited.r, 127);
        assert!(composited.is_opaque());

        let naive = contrast_ratio(half_black, white);
        let correct = contrast_ratio_over(half_black, white);
        assert!(naive > 20.0);
        assert!(correct < 6.0, "correct ratio was {correct}");
    }

    #[test]
    fn opaque_foreground_composites_to_itself() {
        let fg = Srgba::hex(0x336699);
        assert_eq!(fg.composite_over(Srgba::hex(0xFFFFFF)), fg);
    }

    #[test]
    fn aa_thresholds() {
        assert_eq!(TextSize::Normal.min_contrast_aa(), 4.5);
        assert_eq!(TextSize::Large.min_contrast_aa(), 3.0);
    }
}
