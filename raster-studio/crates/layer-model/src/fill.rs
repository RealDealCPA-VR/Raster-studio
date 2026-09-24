//! W9-B: live fill layers — Photopea's Layer ▸ New Fill Layer ▸ Solid Color,
//! Gradient and Pattern.
//!
//! A fill layer owns no pixels. It is a *source*: the compositor evaluates it
//! over whatever region it is asked for, so it covers the whole canvas at any
//! size, follows a canvas resize without being re-baked, and stays editable —
//! re-opening its dialog (or its Properties page) changes the parameters, not
//! a copy of the pixels. Its layer mask shapes it, exactly as Photopea's fill
//! layers are shaped by theirs. Rasterizing one is what turns it into pixels.
//!
//! The payload is [`LayerKind::Fill`](crate::LayerKind::Fill). Every struct
//! here is `#[serde(default)]`, so a field added later is append-only: a
//! document written before it still opens.

use serde::{Deserialize, Serialize};

use crate::effects::{Gradient, GradientStyle, PatternFill, Rgba};

/// What a fill layer paints. Photopea's three fill-layer kinds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FillSource {
    /// One colour everywhere. Straight-alpha RGBA in the document's colour
    /// space; each component expected in `0.0..=1.0` (the compositor clamps).
    Solid { color: Rgba },
    /// A gradient laid across the document.
    Gradient(GradientFill),
    /// A pattern tiled across the document.
    Pattern(PatternFill),
}

impl Default for FillSource {
    fn default() -> Self {
        FillSource::Solid {
            color: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

impl FillSource {
    /// The kind's name, for a status line or a history label.
    pub const fn kind_name(&self) -> &'static str {
        match self {
            FillSource::Solid { .. } => "Color Fill",
            FillSource::Gradient(_) => "Gradient Fill",
            FillSource::Pattern(_) => "Pattern Fill",
        }
    }
}

/// A gradient fill layer's parameters: Photopea's Gradient Fill dialog.
///
/// The ramp is fitted to the **document** (a fill layer has no bounds of its
/// own — it covers everything), centred there, and `scale` times the longer
/// half-side long, the same geometry the Gradient Overlay style uses with
/// "Align with layer" off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GradientFill {
    pub gradient: Gradient,
    pub style: GradientStyle,
    /// Direction in degrees, counter-clockwise from +x: 90 runs bottom to
    /// top, Photopea's default; 0 runs left to right.
    pub angle_deg: f32,
    /// Ramp length as a fraction of the fitted extent; 1.0 = 100%. Expected
    /// `> 0.0`; the compositor treats anything else as 1.0.
    pub scale: f32,
    pub reverse: bool,
    /// Break up banding with a sub-code-value dither of the ramp position.
    pub dither: bool,
    /// Ramp origin offset from the document centre, in document pixels.
    pub offset_px: [f32; 2],
}

impl Default for GradientFill {
    fn default() -> Self {
        Self {
            gradient: Gradient::default(),
            style: GradientStyle::Linear,
            angle_deg: 90.0,
            scale: 1.0,
            reverse: false,
            dither: false,
            offset_px: [0.0, 0.0],
        }
    }
}

/// The payload of [`LayerKind::Fill`](crate::LayerKind::Fill).
///
/// A struct around the [`FillSource`] rather than the enum itself so a
/// property that applies to every fill kind can be appended later without
/// changing the wire shape of the three sources.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FillLayer {
    pub source: FillSource,
}

impl FillLayer {
    pub fn new(source: FillSource) -> Self {
        Self { source }
    }

    /// A solid-colour fill.
    pub fn solid(color: Rgba) -> Self {
        Self::new(FillSource::Solid { color })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Layer, LayerKind, PatternTile};

    #[test]
    fn a_fill_layer_round_trips_through_serde_for_every_source() {
        let tile = PatternTile::new("Checks", 2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        for source in [
            FillSource::Solid {
                color: [0.25, 0.5, 0.75, 1.0],
            },
            FillSource::Gradient(GradientFill {
                angle_deg: 30.0,
                scale: 0.5,
                reverse: true,
                dither: true,
                style: GradientStyle::Radial,
                ..GradientFill::default()
            }),
            FillSource::Pattern(PatternFill {
                tile: Some(tile.clone()),
                scale: 2.0,
                ..PatternFill::default()
            }),
        ] {
            let layer = Layer::with_kind("Fill", LayerKind::Fill(FillLayer::new(source)));
            let json = serde_json::to_string(&layer).unwrap();
            let back: Layer = serde_json::from_str(&json).unwrap();
            assert_eq!(back, layer, "{json}");
        }
    }

    #[test]
    fn a_gradient_fill_missing_its_later_fields_still_loads() {
        // Append-only: an older writer that knew only the ramp and the angle.
        let json = r#"{"source":{"Gradient":{"angle_deg":45.0}}}"#;
        let fill: FillLayer = serde_json::from_str(json).unwrap();
        match fill.source {
            FillSource::Gradient(g) => {
                assert_eq!(g.angle_deg, 45.0);
                assert_eq!(g.scale, 1.0);
                assert!(!g.reverse && !g.dither);
            }
            other => panic!("loaded as {other:?}"),
        }
        let empty: FillLayer = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, FillLayer::default());
    }
}
