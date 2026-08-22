//! Layer masks.
//!
//! A mask is a *reference* plus the parameters that turn the referenced
//! coverage data into an actual alpha multiplier. The coverage pixels
//! themselves live in the tile store under [`MaskId`]; this crate only
//! describes how to interpret them, which is what makes `Layer::mask`
//! resolvable by the compositor instead of an inert id.

use serde::{Deserialize, Serialize};

// Coverage is an alpha multiplier, so it obeys the same "map any f32 into
// 0.0..=1.0, non-finite becomes 0.0" rule as the blend math. Shared rather than
// re-implemented so the two cannot drift.
use crate::blend::unit;
use crate::ids::MaskId;

/// What the [`MaskId`] resolves to in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum MaskKind {
    /// An 8-bit grayscale tile set, same tile grid as raster layers.
    #[default]
    Raster,
    /// A vector path rasterized on demand at the current zoom.
    Vector,
}

/// Rejection of a mask parameter that cannot be repaired by clamping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MaskError {
    /// NaN or ±infinity. Unlike an out-of-range finite value there is no
    /// defensible value to clamp it to, so the write (or the document load) is
    /// refused instead of guessed at.
    #[error("mask field `{field}` must be finite")]
    NonFinite { field: &'static str },
}

/// A mask attached to a layer.
///
/// # Enforced numeric invariants
///
/// `density` is always finite and within `0.0..=1.0`, and `feather_px` is
/// always finite and `>= 0.0`. The compositor may rely on this: `feather_px`
/// is safe to use directly as a blur radius.
///
/// The enforcement is structural, not a convention — the two fields are
/// private and reachable only through [`LayerMask::set_density`] /
/// [`LayerMask::set_feather_px`], which clamp finite values and reject
/// non-finite ones, and deserialization is routed through the same setters by
/// `#[serde(try_from)]` so a hand-edited or corrupt document cannot smuggle a
/// bad value in either.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "LayerMaskRepr")]
pub struct LayerMask {
    /// Key into the mask/tile store holding the coverage data.
    pub id: MaskId,
    pub kind: MaskKind,
    /// When `true` the mask moves with the layer's transform (the chain-link
    /// state in the layers panel). When `false` the mask stays put in document
    /// space while the layer content moves under it.
    pub linked: bool,
    /// A disabled mask is retained on the layer but contributes nothing; the
    /// compositor must treat coverage as 1.0 everywhere.
    pub enabled: bool,
    /// See [`LayerMask::density`].
    density: f32,
    /// See [`LayerMask::feather_px`].
    feather_px: f32,
    /// Coverage is read as `1 - sample` when `true`.
    pub inverted: bool,
}

/// Deserialization shadow of [`LayerMask`]. Exists so `TryFrom` can push every
/// numeric field through the validating setters before the value escapes into
/// the document. Field names and defaults mirror [`LayerMask`] exactly, so the
/// wire format is unchanged.
#[derive(Deserialize)]
struct LayerMaskRepr {
    id: MaskId,
    #[serde(default)]
    kind: MaskKind,
    #[serde(default = "yes")]
    linked: bool,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default = "one")]
    density: f32,
    #[serde(default)]
    feather_px: f32,
    #[serde(default)]
    inverted: bool,
}

fn yes() -> bool {
    true
}

fn one() -> f32 {
    1.0
}

impl TryFrom<LayerMaskRepr> for LayerMask {
    type Error = MaskError;

    fn try_from(r: LayerMaskRepr) -> Result<Self, Self::Error> {
        let mut m = LayerMask::new(r.id);
        m.kind = r.kind;
        m.linked = r.linked;
        m.enabled = r.enabled;
        m.inverted = r.inverted;
        m.set_density(r.density)?;
        m.set_feather_px(r.feather_px)?;
        Ok(m)
    }
}

impl LayerMask {
    /// A fully-enabled, linked raster mask at full density with a hard edge.
    pub fn new(id: MaskId) -> Self {
        Self {
            id,
            kind: MaskKind::Raster,
            linked: true,
            enabled: true,
            density: 1.0,
            feather_px: 0.0,
            inverted: false,
        }
    }

    /// A vector mask with the same defaults.
    pub fn vector(id: MaskId) -> Self {
        Self {
            kind: MaskKind::Vector,
            ..Self::new(id)
        }
    }

    /// Strength of the mask, always finite and in `0.0..=1.0`.
    ///
    /// This is a *fade*, not an amount of hiding. At 1.0 the mask is applied at
    /// full strength — [`LayerMask::coverage`] passes the sample through
    /// unchanged, so a black sample still hides the layer completely. At 0.0
    /// the mask is fully faded out and hides nothing, which is *not* the same
    /// as disabling it: the data is still there and still feathered, and
    /// raising the density brings it back exactly.
    pub fn density(&self) -> f32 {
        self.density
    }

    /// Gaussian feather radius in document pixels, applied to the coverage
    /// before `density`. Always finite and `>= 0.0`; `0.0` means a hard edge.
    pub fn feather_px(&self) -> f32 {
        self.feather_px
    }

    /// Set [`LayerMask::density`].
    ///
    /// Finite values are clamped into `0.0..=1.0`. A non-finite value is
    /// refused with [`MaskError::NonFinite`] and the mask is left unchanged.
    pub fn set_density(&mut self, density: f32) -> Result<(), MaskError> {
        if !density.is_finite() {
            return Err(MaskError::NonFinite { field: "density" });
        }
        self.density = density.clamp(0.0, 1.0);
        Ok(())
    }

    /// Set [`LayerMask::feather_px`].
    ///
    /// Finite values are clamped to `>= 0.0` — a negative blur radius has no
    /// meaning and would either panic or produce garbage in a blur kernel. A
    /// non-finite value is refused with [`MaskError::NonFinite`] and the mask
    /// is left unchanged.
    pub fn set_feather_px(&mut self, feather_px: f32) -> Result<(), MaskError> {
        if !feather_px.is_finite() {
            return Err(MaskError::NonFinite {
                field: "feather_px",
            });
        }
        self.feather_px = feather_px.max(0.0);
        Ok(())
    }

    /// `true` when this mask can change the composite at all.
    ///
    /// A disabled mask, or one at zero density, is a no-op and the compositor
    /// may skip binding its texture entirely.
    pub fn affects_composite(&self) -> bool {
        self.enabled && self.density > 0.0
    }

    /// Turn one sampled coverage value (already feathered) into the alpha
    /// multiplier the compositor applies to the layer.
    ///
    /// Applies, in order: `enabled`, `inverted`, then `density` as a lerp from
    /// full coverage — so `density == 0.0` yields 1.0 (nothing hidden) and
    /// `density == 1.0` yields the sample unchanged.
    ///
    /// Total over all `f32`: the return value is always finite and within
    /// `0.0..=1.0`. A finite sample outside `0.0..=1.0` is clamped; a
    /// **non-finite sample counts as zero coverage**, so a corrupt tile or an
    /// uninitialised texture read hides the layer rather than turning the
    /// composite into NaN (`f32::clamp` propagates NaN, it does not substitute
    /// a bound).
    pub fn coverage(&self, sample: f32) -> f32 {
        if !self.enabled {
            return 1.0;
        }
        let s = unit(sample);
        let m = if self.inverted { 1.0 - s } else { s };
        // `density` is enforced finite by the setters; `unit` is applied anyway
        // so this function's totality does not depend on that enforcement.
        unit(1.0 - unit(self.density) * (1.0 - m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask() -> LayerMask {
        LayerMask::new(MaskId::new())
    }

    #[test]
    fn defaults_are_a_full_strength_linked_raster_mask() {
        let m = mask();
        assert!(m.linked && m.enabled && !m.inverted);
        assert_eq!(m.kind, MaskKind::Raster);
        assert_eq!(m.density(), 1.0);
        assert_eq!(m.feather_px(), 0.0);
        assert!(m.affects_composite());
    }

    #[test]
    fn disabled_mask_passes_everything_through() {
        let mut m = mask();
        m.enabled = false;
        assert_eq!(m.coverage(0.0), 1.0);
        assert_eq!(m.coverage(0.5), 1.0);
        assert!(!m.affects_composite());
    }

    #[test]
    fn full_density_returns_the_sample_unchanged() {
        let m = mask();
        assert_eq!(m.coverage(0.0), 0.0);
        assert_eq!(m.coverage(0.25), 0.25);
        assert_eq!(m.coverage(1.0), 1.0);
    }

    #[test]
    fn zero_density_hides_nothing_but_still_counts_as_present() {
        let mut m = mask();
        m.set_density(0.0).unwrap();
        assert_eq!(m.coverage(0.0), 1.0);
        assert!(!m.affects_composite());
    }

    #[test]
    fn half_density_lerps_toward_full_coverage() {
        let mut m = mask();
        m.set_density(0.5).unwrap();
        // 1 - 0.5 * (1 - 0) = 0.5
        assert_eq!(m.coverage(0.0), 0.5);
        // 1 - 0.5 * (1 - 0.4) = 0.7
        assert!((m.coverage(0.4) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn inversion_flips_the_sample_before_density() {
        let mut m = mask();
        m.inverted = true;
        assert_eq!(m.coverage(0.0), 1.0);
        assert_eq!(m.coverage(1.0), 0.0);
        m.set_density(0.5).unwrap();
        // sample 1.0 -> inverted 0.0 -> 1 - 0.5*(1-0) = 0.5
        assert_eq!(m.coverage(1.0), 0.5);
    }

    #[test]
    fn out_of_range_samples_are_clamped() {
        let m = mask();
        assert_eq!(m.coverage(-3.0), 0.0);
        assert_eq!(m.coverage(9.0), 1.0);
    }

    #[test]
    fn non_finite_samples_never_leak_into_the_composite() {
        for m in [mask(), {
            let mut inv = mask();
            inv.inverted = true;
            inv
        }] {
            for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                let c = m.coverage(sample);
                assert!(
                    c.is_finite() && (0.0..=1.0).contains(&c),
                    "coverage({sample}) = {c}"
                );
            }
            // NaN is the case a bare `clamp` gets wrong: it returns NaN.
            assert_eq!(m.coverage(f32::NAN), m.coverage(0.0));
        }
    }

    #[test]
    fn setters_clamp_finite_values_into_range() {
        let mut m = mask();
        m.set_density(5.0).unwrap();
        assert_eq!(m.density(), 1.0);
        m.set_density(-2.0).unwrap();
        assert_eq!(m.density(), 0.0);
        m.set_feather_px(-3.0).unwrap();
        assert_eq!(m.feather_px(), 0.0, "a negative blur radius is meaningless");
        m.set_feather_px(4.5).unwrap();
        assert_eq!(m.feather_px(), 4.5);
    }

    #[test]
    fn setters_reject_non_finite_values_and_leave_the_mask_untouched() {
        let mut m = mask();
        m.set_density(0.5).unwrap();
        m.set_feather_px(2.0).unwrap();
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                m.set_density(bad).unwrap_err(),
                MaskError::NonFinite { field: "density" }
            );
            assert_eq!(
                m.set_feather_px(bad).unwrap_err(),
                MaskError::NonFinite {
                    field: "feather_px"
                }
            );
        }
        assert_eq!(m.density(), 0.5);
        assert_eq!(m.feather_px(), 2.0);
    }

    #[test]
    fn a_document_with_out_of_range_parameters_loads_clamped() {
        let id = MaskId::new();
        let json = format!(
            r#"{{"id":"{}","kind":"Raster","linked":true,"enabled":true,
                 "density":5.0,"feather_px":-3.0,"inverted":false}}"#,
            id.0
        );
        let m: LayerMask = serde_json::from_str(&json).unwrap();
        assert_eq!(m.density(), 1.0, "density must not survive at 5.0");
        assert_eq!(m.feather_px(), 0.0, "feather must not survive at -3.0");
        // And a compositor reading it back gets a usable blur radius.
        assert!(m.feather_px().is_finite() && m.feather_px() >= 0.0);
    }

    #[test]
    fn a_document_with_a_non_finite_parameter_fails_to_load() {
        // JSON has no NaN literal, so the hostile value arrives the way a
        // binary/bincode document or a custom format would deliver it: through
        // the same deserialization shadow.
        let err = LayerMask::try_from(LayerMaskRepr {
            id: MaskId::new(),
            kind: MaskKind::Raster,
            linked: true,
            enabled: true,
            density: f32::NAN,
            feather_px: 0.0,
            inverted: false,
        })
        .unwrap_err();
        assert_eq!(err, MaskError::NonFinite { field: "density" });

        let err = LayerMask::try_from(LayerMaskRepr {
            id: MaskId::new(),
            kind: MaskKind::Raster,
            linked: true,
            enabled: true,
            density: 1.0,
            feather_px: f32::INFINITY,
            inverted: false,
        })
        .unwrap_err();
        assert_eq!(
            err,
            MaskError::NonFinite {
                field: "feather_px"
            }
        );
    }

    #[test]
    fn missing_fields_take_the_constructor_defaults() {
        let id = MaskId::new();
        let m: LayerMask = serde_json::from_str(&format!(r#"{{"id":"{}"}}"#, id.0)).unwrap();
        assert_eq!(m, LayerMask::new(id));
    }

    #[test]
    fn serde_roundtrip() {
        let mut m = LayerMask::vector(MaskId::new());
        m.set_feather_px(4.5).unwrap();
        m.set_density(0.75).unwrap();
        let json = serde_json::to_string(&m).unwrap();
        let back: LayerMask = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        assert_eq!(back.density(), 0.75);
        assert_eq!(back.feather_px(), 4.5);
    }
}
