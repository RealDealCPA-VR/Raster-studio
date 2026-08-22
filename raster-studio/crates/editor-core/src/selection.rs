//! Selections: which pixels an operation is allowed to touch.
//!
//! Two shapes, one interface. A rectangle is the marquee tools' result; a
//! [`SelectionMask`] carries per-pixel coverage and is what lasso, wand,
//! feather, and refine-edge produce. Consumers read both through
//! [`Selection::coverage_at`] and [`Selection::bounds`] and never branch on the
//! variant.
//!
//! # Coordinates
//! Everything here is document pixel space. Bounds are **half-open**: `min` is
//! the first covered pixel, `max` is one past the last, so `max - min` is the
//! size and an empty box is `min == max`.

use glam::IVec2;
use serde::{Deserialize, Serialize};

/// Rejection of a selection mask that cannot describe a rectangle of samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SelectionError {
    #[error("selection mask is {width}x{height} but carries {got} coverage samples (expected {expected})")]
    CoverageLengthMismatch {
        width: u32,
        height: u32,
        expected: usize,
        got: usize,
    },
    #[error("selection mask dimensions {width}x{height} do not fit in memory")]
    DimensionOverflow { width: u32, height: u32 },
    #[error(
        "selection mask at ({x}, {y}) sized {width}x{height} reaches past the i32 pixel grid"
    )]
    OriginOutOfRange {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
}

/// Serialized form of [`SelectionMask`]. The cached bounds are derived, never
/// stored, so a hand-edited file cannot disagree with its own coverage data.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SelectionMaskRepr {
    origin: IVec2,
    width: u32,
    height: u32,
    coverage: Vec<u8>,
}

/// Per-pixel selection coverage over an axis-aligned rectangle.
///
/// `coverage[y * width + x]` is the alpha of the pixel at
/// `origin + (x, y)`: 0 excluded, 255 fully selected, in between partially
/// (anti-aliased or feathered edges). Pixels outside the rectangle are not
/// selected.
///
/// # Invariants
/// `coverage.len() == width * height`, and `origin + (width, height)` fits in
/// `i32` — both enforced by [`SelectionMask::new`] and re-checked on
/// deserialize, so no coordinate arithmetic in this module can overflow on a
/// corrupt or hand-edited document. The tight bounds of the non-zero samples
/// are computed once at construction, so [`SelectionMask::bounds`] is O(1) and
/// can never drift from the data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "SelectionMaskRepr", try_from = "SelectionMaskRepr")]
pub struct SelectionMask {
    origin: IVec2,
    width: u32,
    height: u32,
    coverage: Vec<u8>,
    /// Tight half-open box of the non-zero samples; `None` when nothing is
    /// selected. Derived from `coverage`, never deserialized.
    bounds: Option<(IVec2, IVec2)>,
}

impl TryFrom<SelectionMaskRepr> for SelectionMask {
    type Error = SelectionError;

    fn try_from(r: SelectionMaskRepr) -> Result<Self, Self::Error> {
        SelectionMask::new(r.origin, r.width, r.height, r.coverage)
    }
}

impl From<SelectionMask> for SelectionMaskRepr {
    fn from(m: SelectionMask) -> Self {
        Self {
            origin: m.origin,
            width: m.width,
            height: m.height,
            coverage: m.coverage,
        }
    }
}

impl SelectionMask {
    /// Build a mask from raw coverage samples, row-major, `width * height` of
    /// them.
    ///
    /// Refuses a sample count that does not match the rectangle, a rectangle
    /// too large to index, and an origin whose far edge leaves the `i32` pixel
    /// grid ([`SelectionError::OriginOutOfRange`]) — the last is what keeps
    /// [`SelectionMask::bounds`] from overflowing on a document that was
    /// hand-edited or corrupted.
    pub fn new(
        origin: IVec2,
        width: u32,
        height: u32,
        coverage: Vec<u8>,
    ) -> Result<Self, SelectionError> {
        let expected = sample_count(origin, width, height)?;
        if coverage.len() != expected {
            return Err(SelectionError::CoverageLengthMismatch {
                width,
                height,
                expected,
                got: coverage.len(),
            });
        }
        let bounds = tight_bounds(origin, width, height, &coverage);
        Ok(Self {
            origin,
            width,
            height,
            coverage,
            bounds,
        })
    }

    /// A fully-selected rectangle — the mask form of a marquee, useful as the
    /// starting point for feathering or a boolean combination.
    pub fn filled(origin: IVec2, width: u32, height: u32) -> Result<Self, SelectionError> {
        // Validated *before* the allocation: a rejected extent must not cost a
        // multi-gigabyte buffer on the way to its error.
        let expected = sample_count(origin, width, height)?;
        Self::new(origin, width, height, vec![255; expected])
    }

    pub fn origin(&self) -> IVec2 {
        self.origin
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Raw samples, row-major.
    pub fn coverage(&self) -> &[u8] {
        &self.coverage
    }

    /// Coverage of one document pixel; 0 outside the mask rectangle.
    ///
    /// Total over every `IVec2`: the local coordinates are computed in `i64`,
    /// so a query far from the origin answers 0 rather than wrapping (release)
    /// or panicking (debug) on the subtraction.
    pub fn coverage_at(&self, p: IVec2) -> u8 {
        let lx = p.x as i64 - self.origin.x as i64;
        let ly = p.y as i64 - self.origin.y as i64;
        if lx < 0 || ly < 0 || lx >= self.width as i64 || ly >= self.height as i64 {
            return 0;
        }
        self.coverage[ly as usize * self.width as usize + lx as usize]
    }

    /// Tight half-open bounds of the non-zero coverage, or `None` when nothing
    /// is selected.
    ///
    /// Tight, not the storage rectangle: a lasso stored in a padded buffer
    /// reports the box that actually contains the selection, so callers can use
    /// it to limit the work they do.
    pub fn bounds(&self) -> Option<(IVec2, IVec2)> {
        self.bounds
    }

    /// `true` when no pixel is selected at all.
    pub fn is_empty(&self) -> bool {
        self.bounds.is_none()
    }
}

/// Samples a `width * height` mask at `origin` must carry, or the reason the
/// extent is not representable.
///
/// Two separate limits: the sample count has to be indexable (`usize`), and the
/// far edge `origin + (width, height)` has to stay inside `i32` so bounds
/// arithmetic is exact. The second is the one a corrupt document reaches first
/// — `width` alone is bounded by the coverage length, but `origin` is not
/// bounded by anything.
fn sample_count(origin: IVec2, width: u32, height: u32) -> Result<usize, SelectionError> {
    if origin.x as i64 + width as i64 > i32::MAX as i64
        || origin.y as i64 + height as i64 > i32::MAX as i64
    {
        return Err(SelectionError::OriginOutOfRange {
            x: origin.x,
            y: origin.y,
            width,
            height,
        });
    }
    (width as usize)
        .checked_mul(height as usize)
        .ok_or(SelectionError::DimensionOverflow { width, height })
}

/// Half-open box of the non-zero samples.
///
/// The `origin + ...` additions cannot overflow: [`sample_count`] has already
/// rejected any extent whose far edge leaves the `i32` grid, and `max_x + 1`
/// is at most `width`.
fn tight_bounds(
    origin: IVec2,
    width: u32,
    height: u32,
    coverage: &[u8],
) -> Option<(IVec2, IVec2)> {
    let (mut min_x, mut min_y) = (u32::MAX, u32::MAX);
    let (mut max_x, mut max_y) = (0u32, 0u32);
    let mut any = false;
    for y in 0..height {
        let row = &coverage[y as usize * width as usize..(y as usize + 1) * width as usize];
        for (x, &v) in row.iter().enumerate() {
            if v != 0 {
                let x = x as u32;
                any = true;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }
    if !any {
        return None;
    }
    Some((
        origin + IVec2::new(min_x as i32, min_y as i32),
        origin + IVec2::new(max_x as i32 + 1, max_y as i32 + 1),
    ))
}

/// The active pixel selection.
///
/// Persisted with the document (see [`crate::Document::selection`]): a
/// selection is work — a wand click plus three feather passes — and losing it
/// on save is losing user work.
///
/// # Empty is not the same as absent
/// Only [`Selection::None`] is omitted from the serialized form (that is what
/// [`Selection::is_none`] is for). An *empty* selection — a zero-area rectangle
/// or an all-zero mask — is written out and comes back as itself, because the
/// two answer differently: with no selection every pixel is selected
/// ([`Selection::coverage_at`] returns 1.0), and with an empty selection no
/// pixel is ([`Selection::coverage_at`] returns 0.0). Collapsing one into the
/// other across a save would turn "the fill touches nothing" into "the fill
/// touches the whole layer".
///
/// The same split governs the two predicates, and mixing them up is the trap
/// this type is shaped to prevent:
///
/// | | [`Selection::is_none`] | [`Selection::is_empty`] | [`Selection::coverage_at`] |
/// |---|---|---|---|
/// | `None` | `true` | `false` | 1.0 — every pixel |
/// | zero-area `Rect` / all-zero `Mask` | `false` | `true` | 0.0 — no pixel |
/// | anything else | `false` | `false` | per pixel |
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Selection {
    /// Nothing selected — operations apply to the whole active layer.
    #[default]
    None,
    /// Axis-aligned rectangle in document pixel space, half-open.
    Rect { min: IVec2, max: IVec2 },
    /// Per-pixel coverage (lasso / wand / feather / refine-edge results).
    Mask(SelectionMask),
}

impl Selection {
    /// `true` when this selection selects **no pixel at all**: a zero-area
    /// [`Selection::Rect`], or a [`Selection::Mask`] whose every sample is 0.
    ///
    /// [`Selection::None`] answers `false`, and that is the whole point. With no
    /// selection every pixel is selected ([`Selection::coverage_at`] answers
    /// 1.0), so `None` is the *opposite* of "nothing to operate on". The obvious
    /// consumer guard —
    ///
    /// ```
    /// # use editor_core::Selection;
    /// # fn paint(_: &Selection) {}
    /// # let selection = Selection::None;
    /// if selection.is_empty() {
    ///     return; // nothing is selected, so there is nothing to do
    /// }
    /// paint(&selection);
    /// ```
    ///
    /// — must therefore *not* skip when there is no selection, or a brush with
    /// nothing selected would refuse to paint anywhere. Pinned by
    /// `is_empty_is_false_for_no_selection_because_no_selection_means_all_pixels`.
    ///
    /// This is not the serialization predicate either; that is
    /// [`Selection::is_none`].
    pub fn is_empty(&self) -> bool {
        match self {
            Selection::None => false,
            Selection::Rect { min, max } => max.x <= min.x || max.y <= min.y,
            Selection::Mask(m) => m.is_empty(),
        }
    }

    /// `true` only for [`Selection::None`] — no selection at all, as opposed to
    /// a selection that happens to be empty.
    ///
    /// This is the predicate serialization uses: `None` is the default and
    /// costs nothing on disk, everything else is written faithfully.
    pub fn is_none(&self) -> bool {
        matches!(self, Selection::None)
    }

    /// Bounding box in document pixel space, half-open, or `None` when nothing
    /// is selected.
    ///
    /// [`Selection::Mask`] reports the tight box of its non-zero coverage, not
    /// its storage rectangle.
    pub fn bounds(&self) -> Option<(IVec2, IVec2)> {
        match self {
            Selection::None => None,
            Selection::Rect { min, max } => {
                if max.x <= min.x || max.y <= min.y {
                    None
                } else {
                    Some((*min, *max))
                }
            }
            Selection::Mask(m) => m.bounds(),
        }
    }

    /// How much of one pixel is selected, in `0.0..=1.0`.
    ///
    /// [`Selection::None`] answers 1.0 everywhere: with no selection an
    /// operation applies to the whole layer, so every consumer can multiply by
    /// this without first asking whether a selection exists.
    pub fn coverage_at(&self, p: IVec2) -> f32 {
        match self {
            Selection::None => 1.0,
            Selection::Rect { min, max } => {
                if p.x >= min.x && p.y >= min.y && p.x < max.x && p.y < max.y {
                    1.0
                } else {
                    0.0
                }
            }
            Selection::Mask(m) => m.coverage_at(p) as f32 / 255.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_bounds() {
        let s = Selection::Rect {
            min: IVec2::new(1, 2),
            max: IVec2::new(10, 20),
        };
        assert_eq!(s.bounds(), Some((IVec2::new(1, 2), IVec2::new(10, 20))));
        assert!(!s.is_empty());
        assert_eq!(s.coverage_at(IVec2::new(1, 2)), 1.0);
        assert_eq!(s.coverage_at(IVec2::new(9, 19)), 1.0);
        assert_eq!(s.coverage_at(IVec2::new(10, 19)), 0.0, "max is exclusive");
        assert_eq!(s.coverage_at(IVec2::new(0, 2)), 0.0);
    }

    #[test]
    fn an_inside_out_or_zero_width_rect_selects_nothing() {
        for (min, max) in [
            (IVec2::new(5, 5), IVec2::new(5, 9)),
            (IVec2::new(5, 5), IVec2::new(9, 5)),
            (IVec2::new(5, 5), IVec2::new(1, 1)),
        ] {
            let s = Selection::Rect { min, max };
            assert!(s.is_empty(), "{min:?}..{max:?}");
            assert_eq!(s.bounds(), None);
        }
    }

    #[test]
    fn no_selection_means_everything_is_selected() {
        let s = Selection::None;
        assert!(
            !s.is_empty(),
            "no selection selects every pixel, so it is not an empty selection"
        );
        assert!(s.is_none());
        assert_eq!(s.bounds(), None, "there is no region to bound");
        assert_eq!(s.coverage_at(IVec2::new(-9999, 12345)), 1.0);
    }

    #[test]
    fn is_empty_is_false_for_no_selection_because_no_selection_means_all_pixels() {
        // The trap: a consumer writes `if sel.is_empty() { return; }` to skip
        // work when nothing is selected. If `Selection::None` answered `true`
        // there, the brush would refuse to paint in exactly the case where it
        // should paint everywhere — the inverse of correct, and silent.
        fn would_skip(s: &Selection) -> bool {
            s.is_empty()
        }

        assert!(
            !would_skip(&Selection::None),
            "an operation with no selection must apply to the whole layer"
        );
        assert!(would_skip(&Selection::Rect {
            min: IVec2::new(5, 5),
            max: IVec2::new(5, 5),
        }));
        assert!(would_skip(&Selection::Mask(
            SelectionMask::new(IVec2::ZERO, 4, 4, vec![0; 16]).unwrap()
        )));
        assert!(!would_skip(&Selection::Rect {
            min: IVec2::ZERO,
            max: IVec2::new(1, 1),
        }));

        // `is_empty` and `coverage_at` agree in every case, which is the
        // property that makes the predicate usable as a guard at all.
        let probe = IVec2::new(5, 5);
        for s in [
            Selection::None,
            Selection::Rect {
                min: probe,
                max: probe,
            },
            Selection::Rect {
                min: IVec2::ZERO,
                max: IVec2::new(9, 9),
            },
            Selection::Mask(SelectionMask::new(IVec2::ZERO, 8, 8, vec![0; 64]).unwrap()),
            Selection::Mask(SelectionMask::filled(IVec2::ZERO, 8, 8).unwrap()),
        ] {
            if s.is_empty() {
                assert_eq!(
                    s.coverage_at(probe),
                    0.0,
                    "{s:?} claims to be empty but covers a pixel"
                );
            }
        }

        // And `is_empty` is not the serialization predicate: exactly one
        // variant is `is_none`, and it is the one that is not empty.
        assert!(Selection::None.is_none() && !Selection::None.is_empty());
    }

    #[test]
    fn a_mask_selection_reports_bounds_instead_of_none() {
        // The old `Selection::Mask` was an inert asset hash whose `bounds()`
        // was always `None`, so every consumer had to fall back to the whole
        // canvas.
        let mut coverage = vec![0u8; 4 * 3];
        coverage[1 * 4 + 2] = 200; // local (2,1)
        let mask = SelectionMask::new(IVec2::new(10, 20), 4, 3, coverage).unwrap();
        let s = Selection::Mask(mask);

        assert_eq!(
            s.bounds(),
            Some((IVec2::new(12, 21), IVec2::new(13, 22))),
            "bounds must be tight around the covered pixels, not the storage rect"
        );
        assert!(!s.is_empty());
        assert!((s.coverage_at(IVec2::new(12, 21)) - 200.0 / 255.0).abs() < 1e-6);
        assert_eq!(s.coverage_at(IVec2::new(11, 21)), 0.0);
        assert_eq!(s.coverage_at(IVec2::new(0, 0)), 0.0, "outside the mask rect");
    }

    #[test]
    fn a_filled_mask_covers_its_whole_rectangle() {
        let m = SelectionMask::filled(IVec2::new(-2, -3), 5, 4).unwrap();
        assert_eq!(
            m.bounds(),
            Some((IVec2::new(-2, -3), IVec2::new(3, 1))),
            "half-open bounds are origin..origin+size"
        );
        assert_eq!(m.coverage_at(IVec2::new(-2, -3)), 255);
        assert_eq!(m.coverage_at(IVec2::new(2, 0)), 255);
        assert_eq!(m.coverage_at(IVec2::new(3, 0)), 0);
        assert!(!m.is_empty());
    }

    #[test]
    fn an_all_zero_mask_is_empty_and_unbounded() {
        let m = SelectionMask::new(IVec2::ZERO, 8, 8, vec![0; 64]).unwrap();
        assert!(m.is_empty());
        assert_eq!(m.bounds(), None);
        assert!(Selection::Mask(m).is_empty());
    }

    #[test]
    fn a_mask_whose_samples_do_not_match_its_size_is_refused() {
        let err = SelectionMask::new(IVec2::ZERO, 4, 4, vec![255; 15]).unwrap_err();
        assert_eq!(
            err,
            SelectionError::CoverageLengthMismatch {
                width: 4,
                height: 4,
                expected: 16,
                got: 15
            }
        );
        // And the same check runs on the deserialization path.
        let json = r#"{"origin":[0,0],"width":4,"height":4,"coverage":[255,255]}"#;
        assert!(serde_json::from_str::<SelectionMask>(json).is_err());
    }

    #[test]
    fn a_mask_whose_far_edge_leaves_the_i32_grid_is_refused() {
        // `tight_bounds` adds the local extent to the origin. With the origin at
        // i32::MAX that addition used to overflow — a panic in debug, a wrong
        // box in release — and it happened *during deserialization*, which is
        // the untrusted path.
        let json = r#"{"origin":[2147483647,0],"width":2,"height":1,"coverage":[255,0]}"#;
        let err = serde_json::from_str::<SelectionMask>(json).unwrap_err();
        assert!(
            err.to_string().contains("past the i32 pixel grid"),
            "a corrupt document must fail to load, not crash the editor: {err}"
        );

        assert_eq!(
            SelectionMask::new(IVec2::new(i32::MAX, 0), 2, 1, vec![255, 0]).unwrap_err(),
            SelectionError::OriginOutOfRange {
                x: i32::MAX,
                y: 0,
                width: 2,
                height: 1
            }
        );
        assert!(matches!(
            SelectionMask::filled(IVec2::new(0, i32::MAX - 1), 1, 4),
            Err(SelectionError::OriginOutOfRange { .. })
        ));
        // The last representable extent is still accepted.
        assert!(SelectionMask::filled(IVec2::new(i32::MAX - 2, 0), 2, 1).is_ok());
    }

    #[test]
    fn coverage_at_is_total_even_at_the_extremes_of_the_grid() {
        // `p - origin` used to be i32 arithmetic: with the origin at i32::MIN,
        // any positive query point overflowed.
        let m = SelectionMask::filled(IVec2::new(i32::MIN, i32::MIN), 2, 2).unwrap();
        assert_eq!(m.coverage_at(IVec2::new(i32::MIN, i32::MIN)), 255);
        assert_eq!(m.coverage_at(IVec2::new(1, 0)), 0);
        assert_eq!(m.coverage_at(IVec2::new(i32::MAX, i32::MAX)), 0);

        let far = SelectionMask::filled(IVec2::new(i32::MAX - 1, i32::MAX - 1), 1, 1).unwrap();
        assert_eq!(far.coverage_at(IVec2::new(i32::MIN, i32::MIN)), 0);
        assert_eq!(far.coverage_at(IVec2::new(i32::MAX - 1, i32::MAX - 1)), 255);
    }

    #[test]
    fn an_empty_selection_is_not_the_absence_of_one() {
        let empty_rect = Selection::Rect {
            min: IVec2::new(5, 5),
            max: IVec2::new(5, 5),
        };
        let empty_mask = Selection::Mask(SelectionMask::new(IVec2::ZERO, 4, 4, vec![0; 16]).unwrap());

        for s in [&empty_rect, &empty_mask] {
            assert!(s.is_empty());
            assert!(
                !s.is_none(),
                "an empty selection must not be serialized away as 'no selection'"
            );
            assert_eq!(
                s.coverage_at(IVec2::new(5, 5)),
                0.0,
                "an empty selection selects nothing..."
            );
        }
        assert!(Selection::None.is_none());
        assert_eq!(
            Selection::None.coverage_at(IVec2::new(5, 5)),
            1.0,
            "...while no selection selects everything, which is why the two cannot be merged"
        );
    }

    #[test]
    fn selections_survive_a_roundtrip() {
        for s in [
            Selection::None,
            Selection::Rect {
                min: IVec2::new(1, 2),
                max: IVec2::new(3, 4),
            },
            Selection::Mask(SelectionMask::filled(IVec2::new(7, 8), 3, 2).unwrap()),
            // The empty forms round-trip as themselves, not as `None`.
            Selection::Rect {
                min: IVec2::new(5, 5),
                max: IVec2::new(5, 5),
            },
            Selection::Mask(SelectionMask::new(IVec2::new(5, 5), 2, 2, vec![0; 4]).unwrap()),
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: Selection = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
            assert_eq!(back.bounds(), s.bounds());
            assert_eq!(back.coverage_at(IVec2::new(5, 5)), s.coverage_at(IVec2::new(5, 5)));
        }
    }
}
