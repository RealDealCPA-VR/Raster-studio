//! Selection algorithms: how a selection is *made* and *modified*.
//!
//! The selection **type** — a per-pixel coverage mask — lives in
//! [`editor_core::Selection`], because the document owns the current
//! selection and commands have to be able to change it. This crate sits above
//! that and provides the operations: marquee and lasso shapes, the magic wand
//! and colour range, the morphological modifiers (feather, expand, contract,
//! smooth, border), and outline extraction for the marching-ants overlay.
//!
//! Coverage is partial, never binary. Anti-aliased and feathered edges are the
//! normal case, so every operation here is defined on fractional coverage.
//!
//! # The four rules everything here follows
//!
//! * **Nothing is proportional to the canvas.** Every result is trimmed to the
//!   tight box of its own coverage before it becomes a [`SelectionMask`], and
//!   every shape tool allocates only its bounding box. A ten-pixel ellipse on a
//!   billion-pixel document costs a hundred bytes — pinned by
//!   `a_small_shape_on_a_huge_canvas_allocates_only_its_own_box`.
//! * **Coverage is linear, colour is not.** A coverage byte is a fraction of a
//!   pixel, exactly like alpha, so feathering, smoothing and resampling average
//!   the bytes directly with no transfer curve in the way (see [`buf`]).
//!   Colour *similarity* is the opposite case: it is a question about
//!   appearance, so its default metric is deliberately defined on gamma-encoded
//!   sRGB, with linear-light and CIELAB available as alternatives (see
//!   [`metric`]). Luminance selection, being a question about light rather than
//!   appearance, is linear again.
//! * **Every fractional operation degenerates to the exact binary one.** The
//!   boolean ops, morphology and outline extraction all reduce to classical set
//!   algebra when the inputs happen to be 0 or 255.
//! * **Caller input never panics — and never aborts.** A gesture across a
//!   two-billion-pixel rectangle, a NaN radius, a seed outside the image and a
//!   corrupt mask all come back as [`SelectionOpError`]. That extends to
//!   allocation: every working buffer whose size grows with the image or mask
//!   area is reserved through `try_reserve`, because `handle_alloc_error` is an
//!   abort no editor can catch, let alone report. [`buf`] lists which buffers
//!   that covers and which small ones it deliberately does not.
//!
//! # A tour
//!
//! ```
//! use editor_core::Selection;
//! use glam::IVec2;
//! use selection::{boolean::BooleanOp, marquee, modify, outline, Rect};
//!
//! let canvas = Rect::from_xywh(0, 0, 256, 256);
//!
//! // An anti-aliased ellipse, minus a rectangle bitten out of it.
//! let disc = marquee::ellipse(Rect::from_xywh(32, 32, 96, 96))?;
//! let bite = marquee::rectangle(Rect::from_xywh(96, 96, 64, 64))?;
//! let shape = selection::boolean::combine(&disc, &bite, BooleanOp::Subtract)?;
//!
//! // Soften it, then hand the UI the ants.
//! let soft = modify::feather(&shape, 4.0)?;
//! let loops = outline::outline(&soft, 128)?;
//! assert!(!loops.is_empty());
//!
//! // And it is a document selection like any other.
//! let sel = Selection::Mask(soft);
//! assert!(sel.coverage_at(IVec2::new(64, 64)) > 0.9);
//! assert_eq!(sel.coverage_at(IVec2::new(120, 120)), 0.0);
//! # let _ = canvas;
//! # Ok::<(), selection::SelectionOpError>(())
//! ```

#![forbid(unsafe_code)]

pub mod boolean;
pub mod buf;
pub mod channel;
pub mod error;
pub mod image;
pub mod lasso;
pub mod marquee;
pub mod metric;
pub mod modify;
pub mod outline;
pub mod rect;
mod scan;
pub mod transform;
pub mod wand;

pub use boolean::{combine, combine_selection, to_mask, BooleanOp};
pub use buf::CoverageBuf;
pub use channel::{
    channel_to_selection, mask_tiles_to_selection, selection_to_channel, selection_to_mask_tiles,
    MaskTile,
};
pub use error::SelectionOpError;
pub use image::{ImageBuffer, ImageView};
pub use lasso::{
    lasso_freehand, lasso_magnetic, lasso_polygonal, magnetic_path, polygon, FillRule,
    MagneticOptions,
};
pub use marquee::{
    ellipse, ellipse_subpixel, rectangle, rectangle_subpixel, single_column, single_row,
};
pub use metric::{distance, tolerance_coverage, ColorCoords, ColorMetric};
pub use modify::{border, contract, expand, feather, invert, invert_selection, smooth, MAX_RADIUS};
pub use outline::{outline, outline_selection, Polyline};
pub use rect::{Rect, COORD_LIMIT};
pub use transform::{transform, transform_selection, ResampleFilter};
pub use wand::{
    color_range, grow, luminance_range, magic_wand, quick_select, similar, ColorRangeOptions,
    QuickSelectOptions, WandOptions,
};

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::{Selection, SelectionMask};
    use glam::IVec2;

    /// The end-to-end shape of a real editing session, and the property the
    /// whole crate exists to keep: a selection survives being made, refined,
    /// stored, and reloaded without ever being materialised at canvas size.
    #[test]
    fn a_selection_survives_a_full_round_of_editing() {
        let canvas = Rect::from_xywh(0, 0, 4096, 4096);

        // Wand-free path: an ellipse, feathered, unioned with a lasso.
        let disc = ellipse(Rect::from_xywh(100, 100, 60, 40)).unwrap();
        let soft = feather(&disc, 3.0).unwrap();
        let tri = lasso_freehand(&[
            glam::Vec2::new(200.0, 100.0),
            glam::Vec2::new(240.0, 100.0),
            glam::Vec2::new(200.0, 140.0),
        ])
        .unwrap();
        let both = combine(&soft, &tri, BooleanOp::Add).unwrap();

        // Still tiny relative to the canvas.
        assert!(
            (both.coverage().len() as u64) < canvas.area() / 100,
            "the selection grew to canvas scale: {} samples",
            both.coverage().len()
        );

        // Save as a channel, load it back, and the coverage is identical.
        let tiles = selection_to_mask_tiles(&both).unwrap();
        assert_eq!(mask_tiles_to_selection(&tiles).unwrap(), both);

        // Invert inside the canvas and back again.
        let inv = invert(&both, canvas).unwrap();
        let back = invert(&inv, canvas).unwrap();
        for p in [
            IVec2::new(130, 120),
            IVec2::new(210, 110),
            IVec2::new(0, 0),
            IVec2::new(4095, 4095),
        ] {
            assert_eq!(back.coverage_at(p), both.coverage_at(p), "at {p:?}");
        }

        // And it is a document selection the rest of the editor understands.
        let sel = Selection::Mask(both);
        assert!(!sel.is_none() && !sel.is_empty());
    }

    /// Every fractional rule collapses onto the classical binary one when the
    /// inputs are binary. If it did not, a hard-edged selection would drift
    /// through operations that are supposed to be exact.
    #[test]
    fn binary_inputs_give_binary_set_algebra() {
        let a = rectangle(Rect::from_xywh(0, 0, 8, 8)).unwrap();
        let b = rectangle(Rect::from_xywh(4, 4, 8, 8)).unwrap();
        for op in [
            BooleanOp::Add,
            BooleanOp::Subtract,
            BooleanOp::Intersect,
            BooleanOp::Exclude,
        ] {
            let r = combine(&a, &b, op).unwrap();
            assert!(
                r.coverage().iter().all(|&v| v == 0 || v == 255),
                "{op:?} invented partial coverage from binary inputs"
            );
        }
        for m in [
            expand(&a, 2).unwrap(),
            contract(&a, 2).unwrap(),
            border(&a, 3).unwrap(),
            invert(&a, Rect::from_xywh(0, 0, 16, 16)).unwrap(),
        ] {
            assert!(m.coverage().iter().all(|&v| v == 0 || v == 255));
        }
        // `smooth` is the documented exception: it anti-aliases the corners it
        // rounds rather than stair-stepping them.
        assert!(smooth(&a, 2)
            .unwrap()
            .coverage()
            .iter()
            .any(|&v| v > 0 && v < 255));
    }

    /// The layering rule this crate is built on: it adds algorithms, it does
    /// not add a second selection type. Everything public produces or consumes
    /// `editor_core`'s mask.
    #[test]
    fn results_are_editor_core_masks_not_a_parallel_type() {
        fn assert_mask(_: &SelectionMask) {}
        assert_mask(&rectangle(Rect::from_xywh(0, 0, 1, 1)).unwrap());
        assert_mask(&ellipse(Rect::from_xywh(0, 0, 4, 4)).unwrap());
        assert_mask(&single_row(0, 0, 4).unwrap());
        assert_mask(&single_column(0, 0, 4).unwrap());
        assert_mask(&channel_to_selection(IVec2::ZERO, 2, 2, &[255; 4]).unwrap());
        // And a mask made here is accepted by the document type unchanged.
        let s = Selection::Mask(ellipse(Rect::from_xywh(0, 0, 8, 8)).unwrap());
        assert_eq!(Rect::of_selection_bounds(&s), Rect::from_xywh(0, 0, 8, 8));
    }
}
