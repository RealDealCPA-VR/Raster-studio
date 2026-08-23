//! Blend-mode keys, and the bijection with [`layer_model::BlendMode`].
//!
//! A layer record stores its blend mode as a four-character key. Several of
//! them are traps for anyone matching on the name rather than the key:
//! `'idiv'` is **Color Burn**, `'div '` is **Color Dodge**, `'smud'` is
//! **Exclusion**, and `'fsub'`/`'fdiv'` are Subtract and Divide. Keys are also
//! space-padded to four bytes, so `'mul '` and `'lum '` carry a trailing space
//! that must be written back exactly.
//!
//! `'pass'` — Pass Through — is not a blend mode at all. It is a property of a
//! *group*: its children blend against what is beneath the group rather than
//! into an isolated buffer. It therefore maps to
//! [`layer_model::GroupBlending::PassThrough`] and not to a `BlendMode`, which
//! is why [`blend_from_key`] returns an `Option`.

use layer_model::BlendMode;

use crate::error::{tag_name, PsdError, PsdResult};

/// The key a group uses to say "pass through".
pub const PASS_THROUGH: [u8; 4] = *b"pass";

/// Every `(key, mode)` pair, in [`BlendMode::ALL`] order.
///
/// This table is the single source of truth for both directions of the
/// mapping, so the two cannot drift apart, and
/// `mapping_is_bijective_over_every_supported_mode` proves it covers every
/// variant `layer-model` defines.
const KEYS: [([u8; 4], BlendMode); 27] = [
    (*b"norm", BlendMode::Normal),
    (*b"diss", BlendMode::Dissolve),
    (*b"dark", BlendMode::Darken),
    (*b"mul ", BlendMode::Multiply),
    (*b"idiv", BlendMode::ColorBurn),
    (*b"lbrn", BlendMode::LinearBurn),
    (*b"dkCl", BlendMode::DarkerColor),
    (*b"lite", BlendMode::Lighten),
    (*b"scrn", BlendMode::Screen),
    (*b"div ", BlendMode::ColorDodge),
    (*b"lddg", BlendMode::LinearDodge),
    (*b"lgCl", BlendMode::LighterColor),
    (*b"over", BlendMode::Overlay),
    (*b"sLit", BlendMode::SoftLight),
    (*b"hLit", BlendMode::HardLight),
    (*b"vLit", BlendMode::VividLight),
    (*b"lLit", BlendMode::LinearLight),
    (*b"pLit", BlendMode::PinLight),
    (*b"hMix", BlendMode::HardMix),
    (*b"diff", BlendMode::Difference),
    (*b"smud", BlendMode::Exclusion),
    (*b"fsub", BlendMode::Subtract),
    (*b"fdiv", BlendMode::Divide),
    (*b"hue ", BlendMode::Hue),
    (*b"sat ", BlendMode::Saturation),
    (*b"colr", BlendMode::Color),
    (*b"lum ", BlendMode::Luminosity),
];

/// Map a file key to a blend mode.
///
/// `Ok(None)` means `'pass'`: the record is a pass-through group, which has no
/// blend mode of its own.
pub fn blend_from_key(key: [u8; 4]) -> PsdResult<Option<BlendMode>> {
    if key == PASS_THROUGH {
        return Ok(None);
    }
    KEYS.iter()
        .find(|(k, _)| *k == key)
        .map(|(_, m)| Some(*m))
        .ok_or_else(|| PsdError::UnknownBlendMode(tag_name(key)))
}

/// Map a blend mode to the key Photoshop writes for it.
pub const fn key_from_blend(mode: BlendMode) -> [u8; 4] {
    // Written as a match rather than a table lookup so that adding a variant to
    // `layer-model` is a compile error here instead of a runtime `None`.
    match mode {
        BlendMode::Normal => *b"norm",
        BlendMode::Dissolve => *b"diss",
        BlendMode::Darken => *b"dark",
        BlendMode::Multiply => *b"mul ",
        BlendMode::ColorBurn => *b"idiv",
        BlendMode::LinearBurn => *b"lbrn",
        BlendMode::DarkerColor => *b"dkCl",
        BlendMode::Lighten => *b"lite",
        BlendMode::Screen => *b"scrn",
        BlendMode::ColorDodge => *b"div ",
        BlendMode::LinearDodge => *b"lddg",
        BlendMode::LighterColor => *b"lgCl",
        BlendMode::Overlay => *b"over",
        BlendMode::SoftLight => *b"sLit",
        BlendMode::HardLight => *b"hLit",
        BlendMode::VividLight => *b"vLit",
        BlendMode::LinearLight => *b"lLit",
        BlendMode::PinLight => *b"pLit",
        BlendMode::HardMix => *b"hMix",
        BlendMode::Difference => *b"diff",
        BlendMode::Exclusion => *b"smud",
        BlendMode::Subtract => *b"fsub",
        BlendMode::Divide => *b"fdiv",
        BlendMode::Hue => *b"hue ",
        BlendMode::Saturation => *b"sat ",
        BlendMode::Color => *b"colr",
        BlendMode::Luminosity => *b"lum ",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn mapping_is_bijective_over_every_supported_mode() {
        let mut seen_keys = HashSet::new();
        for mode in BlendMode::ALL {
            let key = key_from_blend(mode);
            assert!(
                seen_keys.insert(key),
                "{mode:?} shares key {:?} with another mode",
                tag_name(key)
            );
            assert_eq!(
                blend_from_key(key).unwrap(),
                Some(mode),
                "key {:?} did not map back to {mode:?}",
                tag_name(key)
            );
        }
        assert_eq!(seen_keys.len(), BlendMode::ALL.len());
        assert_eq!(KEYS.len(), BlendMode::ALL.len());
    }

    #[test]
    fn the_table_and_the_match_agree_in_both_directions() {
        for (key, mode) in KEYS {
            assert_eq!(key_from_blend(mode), key, "{mode:?}");
            assert_eq!(blend_from_key(key).unwrap(), Some(mode));
        }
    }

    #[test]
    fn every_key_is_exactly_four_bytes_and_space_padded_not_nul_padded() {
        for (key, mode) in KEYS {
            assert_eq!(key.len(), 4);
            assert!(
                key.iter().all(|b| b.is_ascii_graphic() || *b == b' '),
                "{mode:?} has a non-printable key"
            );
            assert!(!key.contains(&0), "{mode:?} is NUL padded");
        }
    }

    #[test]
    fn the_confusable_keys_map_the_way_photoshop_means_them() {
        assert_eq!(
            blend_from_key(*b"idiv").unwrap(),
            Some(BlendMode::ColorBurn)
        );
        assert_eq!(
            blend_from_key(*b"div ").unwrap(),
            Some(BlendMode::ColorDodge)
        );
        assert_eq!(
            blend_from_key(*b"smud").unwrap(),
            Some(BlendMode::Exclusion)
        );
        assert_eq!(blend_from_key(*b"fsub").unwrap(), Some(BlendMode::Subtract));
        assert_eq!(blend_from_key(*b"fdiv").unwrap(), Some(BlendMode::Divide));
        assert_eq!(
            blend_from_key(*b"lddg").unwrap(),
            Some(BlendMode::LinearDodge)
        );
    }

    #[test]
    fn pass_through_is_not_a_blend_mode() {
        assert_eq!(blend_from_key(PASS_THROUGH).unwrap(), None);
        // ...and nothing maps *to* it, so a pass-through group cannot be
        // reconstructed by accident from a blend mode.
        for mode in BlendMode::ALL {
            assert_ne!(key_from_blend(mode), PASS_THROUGH, "{mode:?}");
        }
    }

    #[test]
    fn an_unknown_key_is_a_typed_error_that_repeats_the_key_readably() {
        let err = blend_from_key(*b"zzzz").unwrap_err();
        match err {
            PsdError::UnknownBlendMode(k) => assert_eq!(k, "zzzz"),
            other => panic!("wrong error: {other}"),
        }
        // A key of control bytes is escaped rather than printed raw.
        let err = blend_from_key([0, 1, 2, 3]).unwrap_err();
        match err {
            PsdError::UnknownBlendMode(k) => assert_eq!(k, "\\x00\\x01\\x02\\x03"),
            other => panic!("wrong error: {other}"),
        }
    }
}
