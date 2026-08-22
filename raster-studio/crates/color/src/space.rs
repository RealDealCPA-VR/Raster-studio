//! [`ColorSpace`] and the encode/decode dispatch between an encoded source
//! space and the linear sRGB working space.
//!
//! "Linear" everywhere in this crate means **linear sRGB primaries, D65 white,
//! unclamped `f32`**. That is the single working space the compositor blends
//! in, so [`to_linear`] always lands there and [`from_linear`] always starts
//! there, whatever the source space is.

use serde::{Deserialize, Serialize};

use crate::transfer::{linear_to_srgb3, srgb_to_linear3};

/// A row-major 3x3 matrix of `f32` coefficients.
pub type Mat3 = [[f32; 3]; 3];

/// Color space metadata attached to sources and the document working space.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ColorSpace {
    /// sRGB with the standard IEC 61966-2-1 transfer function.
    #[default]
    Srgb,
    /// Linear (scene-referred) sRGB primaries. The internal working space.
    LinearSrgb,
    /// Display P3: DCI-P3 primaries, D65 white, sRGB transfer function.
    DisplayP3,
    /// An ICC profile referenced by content hash in the asset store.
    ///
    /// **Not implemented.** No ICC engine is linked, so no correct transform
    /// exists for this variant; see [`ColorSpace::is_transform_supported`].
    IccProfile { asset_hash: String },
}

impl ColorSpace {
    /// Whether [`to_linear`]/[`from_linear`] can transform this space correctly.
    ///
    /// `false` only for [`ColorSpace::IccProfile`], for which the infallible
    /// entry points fall back to identity. Call [`try_to_linear`] /
    /// [`try_from_linear`] to turn that fallback into an error instead.
    pub fn is_transform_supported(&self) -> bool {
        !matches!(self, ColorSpace::IccProfile { .. })
    }

    /// Short stable identifier, useful in logs and UI.
    pub fn name(&self) -> &'static str {
        match self {
            ColorSpace::Srgb => "sRGB",
            ColorSpace::LinearSrgb => "Linear sRGB",
            ColorSpace::DisplayP3 => "Display P3",
            ColorSpace::IccProfile { .. } => "ICC profile",
        }
    }
}

/// Returned when a transform is requested for a space this build cannot handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedColorSpace {
    /// The space that has no implemented transform.
    pub space: ColorSpace,
}

impl std::fmt::Display for UnsupportedColorSpace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.space {
            ColorSpace::IccProfile { asset_hash } => write!(
                f,
                "no ICC engine is linked; cannot transform profile {asset_hash}"
            ),
            other => write!(f, "unsupported color space: {}", other.name()),
        }
    }
}

impl std::error::Error for UnsupportedColorSpace {}

/// Linear sRGB (D65) to CIE XYZ, derived from the sRGB primaries and the D65
/// chromaticity `(0.3127, 0.3290)`.
pub const LINEAR_SRGB_TO_XYZ_D65: Mat3 = [
    [0.412_390_8, 0.357_584_33, 0.180_480_8],
    [0.212_639, 0.715_168_65, 0.072_192_32],
    [0.019_330_818, 0.119_194_78, 0.950_532_14],
];

/// Inverse of [`LINEAR_SRGB_TO_XYZ_D65`].
pub const XYZ_D65_TO_LINEAR_SRGB: Mat3 = [
    [3.240_97, -1.537_383_2, -0.498_610_76],
    [-0.969_243_65, 1.875_967_5, 0.041_555_06],
    [0.055_630_08, -0.203_976_96, 1.056_971_5],
];

/// The D65 white point as this crate's matrices actually realise it, i.e. the
/// row sums of [`LINEAR_SRGB_TO_XYZ_D65`].
///
/// Deliberately *not* the rounded literature value `(0.95047, 1.0, 1.08883)`:
/// using the matrix's own white point is what makes a neutral RGB triple map to
/// exactly `a = b = 0` in CIELAB, so greys never acquire a chroma cast when a
/// user edits in Lab.
pub const D65_WHITE_XYZ: [f32; 3] = [0.950_455_9, 1.0, 1.089_057_8];

/// Linear Display P3 to linear sRGB, derived from both sets of primaries under
/// a shared D65 white. Values outside `[0, 1]` are expected: P3 is the wider
/// gamut, so saturated P3 colours are legitimately out of sRGB gamut and are
/// **not** clamped here.
pub const DISPLAY_P3_TO_LINEAR_SRGB: Mat3 = [
    [1.224_940_2, -0.224_940_18, 0.0],
    [-0.042_056_955, 1.042_056_9, 0.0],
    [-0.019_637_555, -0.078_636_04, 1.098_273_6],
];

/// Inverse of [`DISPLAY_P3_TO_LINEAR_SRGB`].
pub const LINEAR_SRGB_TO_DISPLAY_P3: Mat3 = [
    [0.822_461_96, 0.177_538_04, 0.0],
    [0.033_194_2, 0.966_805_8, 0.0],
    [0.017_082_632, 0.072_397_44, 0.910_519_96],
];

/// Multiplies a row-major 3x3 matrix by a column vector.
#[inline]
pub fn mat3_mul_vec3(m: &Mat3, v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Linear sRGB (D65) to CIE XYZ.
#[inline]
pub fn linear_srgb_to_xyz(rgb: [f32; 3]) -> [f32; 3] {
    mat3_mul_vec3(&LINEAR_SRGB_TO_XYZ_D65, rgb)
}

/// CIE XYZ (D65) to linear sRGB. Unclamped; out-of-gamut XYZ yields negative
/// or greater-than-one components.
#[inline]
pub fn xyz_to_linear_srgb(xyz: [f32; 3]) -> [f32; 3] {
    mat3_mul_vec3(&XYZ_D65_TO_LINEAR_SRGB, xyz)
}

/// Decodes an encoded triple in `space` into the linear sRGB working space.
///
/// For [`ColorSpace::IccProfile`] this returns the input unchanged, because no
/// ICC engine is linked and inventing a transform would silently corrupt
/// colour. Use [`try_to_linear`] when the caller must know that happened.
pub fn to_linear(space: &ColorSpace, rgb: [f32; 3]) -> [f32; 3] {
    match space {
        ColorSpace::Srgb => srgb_to_linear3(rgb),
        ColorSpace::LinearSrgb => rgb,
        ColorSpace::DisplayP3 => {
            mat3_mul_vec3(&DISPLAY_P3_TO_LINEAR_SRGB, srgb_to_linear3(rgb))
        }
        // Identity, not a guess. Documented above.
        ColorSpace::IccProfile { .. } => rgb,
    }
}

/// Encodes a linear sRGB working-space triple into `space`.
///
/// Exact inverse of [`to_linear`] for every supported space; identity for
/// [`ColorSpace::IccProfile`].
pub fn from_linear(space: &ColorSpace, rgb: [f32; 3]) -> [f32; 3] {
    match space {
        ColorSpace::Srgb => linear_to_srgb3(rgb),
        ColorSpace::LinearSrgb => rgb,
        ColorSpace::DisplayP3 => {
            linear_to_srgb3(mat3_mul_vec3(&LINEAR_SRGB_TO_DISPLAY_P3, rgb))
        }
        ColorSpace::IccProfile { .. } => rgb,
    }
}

/// [`to_linear`] that reports the unsupported path instead of silently
/// falling back to identity.
pub fn try_to_linear(
    space: &ColorSpace,
    rgb: [f32; 3],
) -> Result<[f32; 3], UnsupportedColorSpace> {
    if space.is_transform_supported() {
        Ok(to_linear(space, rgb))
    } else {
        Err(UnsupportedColorSpace {
            space: space.clone(),
        })
    }
}

/// [`from_linear`] that reports the unsupported path instead of silently
/// falling back to identity.
pub fn try_from_linear(
    space: &ColorSpace,
    rgb: [f32; 3],
) -> Result<[f32; 3], UnsupportedColorSpace> {
    if space.is_transform_supported() {
        Ok(from_linear(space, rgb))
    } else {
        Err(UnsupportedColorSpace {
            space: space.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(got: [f32; 3], want: [f32; 3], tol: f32, what: &str) {
        for i in 0..3 {
            assert!(
                (got[i] - want[i]).abs() < tol,
                "{what}: channel {i} = {}, expected {} (got {got:?})",
                got[i],
                want[i]
            );
        }
    }

    fn mat_mul(a: &Mat3, b: &Mat3) -> Mat3 {
        let mut out = [[0.0f32; 3]; 3];
        for (i, row) in out.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
            }
        }
        out
    }

    #[test]
    fn srgb_dispatch_uses_the_transfer_function() {
        assert_close(
            to_linear(&ColorSpace::Srgb, [0.5, 0.5, 0.5]),
            [0.214_041_1; 3],
            1e-5,
            "srgb to_linear",
        );
        assert_close(
            from_linear(&ColorSpace::Srgb, [0.214_041_1; 3]),
            [0.5; 3],
            1e-5,
            "srgb from_linear",
        );
    }

    #[test]
    fn linear_srgb_dispatch_is_identity() {
        let v = [0.1, -0.4, 2.5];
        assert_eq!(to_linear(&ColorSpace::LinearSrgb, v), v);
        assert_eq!(from_linear(&ColorSpace::LinearSrgb, v), v);
    }

    #[test]
    fn display_p3_white_is_srgb_white() {
        // Equal-energy P3 must land on equal-energy sRGB: both are D65.
        assert_close(
            to_linear(&ColorSpace::DisplayP3, [1.0, 1.0, 1.0]),
            [1.0, 1.0, 1.0],
            1e-5,
            "P3 white",
        );
        assert_close(
            to_linear(&ColorSpace::DisplayP3, [0.5, 0.5, 0.5]),
            [0.214_041_1; 3],
            1e-5,
            "P3 mid grey",
        );
    }

    #[test]
    fn display_p3_red_is_outside_srgb_gamut_at_known_coordinates() {
        // Linear P3 (1,0,0) expressed in linear sRGB. Derived from the two
        // primary sets; the negative green/blue is the out-of-gamut signal.
        let got = mat3_mul_vec3(&DISPLAY_P3_TO_LINEAR_SRGB, [1.0, 0.0, 0.0]);
        assert_close(
            got,
            [1.224_940_2, -0.042_056_955, -0.019_637_555],
            1e-5,
            "P3 red",
        );
        assert!(got[0] > 1.0 && got[1] < 0.0 && got[2] < 0.0);
    }

    /// White and round-trip tests are blind to the two P3 matrices being
    /// swapped in the dispatch, because the swap is still self-inverse and
    /// still maps white to white. These asymmetric anchors are not.
    #[test]
    fn display_p3_dispatch_picks_the_correct_matrix_direction() {
        // Full-intensity P3 red, decoded, is outside the sRGB gamut.
        assert_close(
            to_linear(&ColorSpace::DisplayP3, [1.0, 0.0, 0.0]),
            [1.224_940_2, -0.042_056_955, -0.019_637_555],
            1e-4,
            "P3 red decoded",
        );
        // sRGB red, encoded for a P3 display, is the familiar #EA3323.
        assert_close(
            from_linear(&ColorSpace::DisplayP3, [1.0, 0.0, 0.0]),
            [0.917_5, 0.200_4, 0.138_6],
            1e-3,
            "sRGB red encoded for P3",
        );
    }

    #[test]
    fn srgb_primaries_fit_inside_the_p3_gamut() {
        for i in 0..3 {
            let mut v = [0.0f32; 3];
            v[i] = 1.0;
            let p3 = mat3_mul_vec3(&LINEAR_SRGB_TO_DISPLAY_P3, v);
            for c in p3 {
                assert!(
                    (-1e-6..=1.0 + 1e-6).contains(&c),
                    "sRGB primary {i} left the P3 gamut: {p3:?}"
                );
            }
        }
    }

    fn assert_inverse_pair(a: &Mat3, b: &Mat3, what: &str) {
        let product = mat_mul(a, b);
        for (i, row) in product.iter().enumerate() {
            for (j, cell) in row.iter().enumerate() {
                let want = if i == j { 1.0 } else { 0.0 };
                assert!(
                    (cell - want).abs() < 1e-5,
                    "{what} are not inverses at [{i}][{j}]: {cell}"
                );
            }
        }
    }

    #[test]
    fn p3_matrices_are_mutual_inverses() {
        assert_inverse_pair(
            &DISPLAY_P3_TO_LINEAR_SRGB,
            &LINEAR_SRGB_TO_DISPLAY_P3,
            "P3 matrices",
        );
        assert_inverse_pair(
            &LINEAR_SRGB_TO_DISPLAY_P3,
            &DISPLAY_P3_TO_LINEAR_SRGB,
            "P3 matrices (reversed)",
        );
    }

    #[test]
    fn xyz_matrices_are_mutual_inverses_and_hit_d65() {
        assert_inverse_pair(
            &LINEAR_SRGB_TO_XYZ_D65,
            &XYZ_D65_TO_LINEAR_SRGB,
            "XYZ matrices",
        );
        assert_close(
            linear_srgb_to_xyz([1.0, 1.0, 1.0]),
            D65_WHITE_XYZ,
            1e-6,
            "white point",
        );
        // And close to the rounded literature D65 value.
        assert_close(
            D65_WHITE_XYZ,
            [0.95047, 1.0, 1.08883],
            2e-3,
            "D65 vs literature",
        );
    }

    #[test]
    fn display_p3_round_trips_through_the_working_space() {
        for &v in &[
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.2, 0.7, 0.4],
            [1.0, 1.0, 1.0],
            [0.9, 0.1, 0.05],
        ] {
            let round = from_linear(&ColorSpace::DisplayP3, to_linear(&ColorSpace::DisplayP3, v));
            assert_close(round, v, 1e-4, "P3 round trip");
        }
    }

    #[test]
    fn every_supported_space_round_trips() {
        let spaces = [ColorSpace::Srgb, ColorSpace::LinearSrgb, ColorSpace::DisplayP3];
        let v = [0.13, 0.62, 0.87];
        for space in spaces {
            let round = from_linear(&space, to_linear(&space, v));
            assert_close(round, v, 1e-4, space.name());
        }
    }

    #[test]
    fn icc_profile_is_an_explicit_unsupported_identity() {
        let icc = ColorSpace::IccProfile {
            asset_hash: "deadbeef".to_string(),
        };
        assert!(!icc.is_transform_supported());
        let v = [0.25, 0.5, 0.75];
        assert_eq!(to_linear(&icc, v), v, "ICC must not fake a transform");
        assert_eq!(from_linear(&icc, v), v);

        let err = try_to_linear(&icc, v).unwrap_err();
        assert_eq!(err.space, icc);
        assert!(err.to_string().contains("deadbeef"));
        assert!(try_from_linear(&icc, v).is_err());
    }

    #[test]
    fn supported_spaces_never_error() {
        for space in [ColorSpace::Srgb, ColorSpace::LinearSrgb, ColorSpace::DisplayP3] {
            assert!(space.is_transform_supported());
            assert!(try_to_linear(&space, [0.5; 3]).is_ok());
            assert!(try_from_linear(&space, [0.5; 3]).is_ok());
        }
    }

    #[test]
    fn dispatch_handles_out_of_range_working_values() {
        // The working space is unclamped f32; dispatch must not produce NaN.
        for space in [ColorSpace::Srgb, ColorSpace::LinearSrgb, ColorSpace::DisplayP3] {
            for v in [[-0.5f32, 1.8, 0.2], [4.0, -2.0, 0.0]] {
                for c in to_linear(&space, v) {
                    assert!(c.is_finite(), "{} to_linear({v:?}) not finite", space.name());
                }
                for c in from_linear(&space, v) {
                    assert!(c.is_finite(), "{} from_linear({v:?}) not finite", space.name());
                }
            }
        }
    }

    #[test]
    fn default_space_is_srgb() {
        assert_eq!(ColorSpace::default(), ColorSpace::Srgb);
    }
}
