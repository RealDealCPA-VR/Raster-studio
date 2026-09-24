//! Smart filters: a smart object's source, re-filtered on every render.
//!
//! A smart-object layer's stored tiles are its **source**. When its
//! [`layer_model::SmartObjectLayer::filters`] stack holds an active filter, the
//! compositor renders the source over the layer's whole extent, runs the
//! stack bottom-up ([`apply_stack`]), and samples the region it was asked for
//! out of that result. The source tiles are never rewritten, which is what
//! makes the filters non-destructive: switching one off, editing it, or
//! deleting it is a change to the stack and nothing else.
//!
//! # Who runs a filter
//!
//! The filter *implementations* are the `filters` crate, but the table that
//! turns a stored filter key and its parameters into a call — the same table
//! the Filter dialogs use — lives in the UI crate, which depends on this one.
//! So the application installs that table here once, as a
//! [`SmartFilterRunner`], through [`install_runner`]. Until one is installed a
//! smart object renders its source unfiltered and says so through
//! [`runner`]; the installation state is part of every cache key below, so a
//! tile rendered before the install is never served after it.
//!
//! # Whole-extent, cached
//!
//! A blur at a tile's edge reads the neighbouring tile, so a filtered smart
//! object cannot be rendered tile by tile. The filtered extent is computed
//! once and kept in a small process-wide cache keyed by everything it was
//! made from — the layer, the mip level, the extent, every source tile hash,
//! the colour space and the stack itself ([`hash_stack`]) — so an unchanged
//! object costs one lookup per tile, and any change to its source or stack
//! misses by key alone.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use filters::FilterBuffer;
use layer_model::{BlendMode, SmartFilter, SmartParam};

use crate::canvas::Canvas;

/// Runs one stored smart filter over a buffer: `None` when the filter key is
/// unknown to the runner (the filter is then a pass-through).
///
/// The buffer is linear, premultiplied RGBA — what every `filters` function
/// takes — and the result must have the same dimensions; one that does not is
/// ignored rather than resized.
pub type SmartFilterRunner = fn(&SmartFilter, &FilterBuffer) -> Option<FilterBuffer>;

static RUNNER: OnceLock<SmartFilterRunner> = OnceLock::new();

/// Install the application's filter table. The first install wins and later
/// ones are ignored (returning `false`), so calling this from every entry
/// point that might composite is safe.
pub fn install_runner(runner: SmartFilterRunner) -> bool {
    RUNNER.set(runner).is_ok()
}

/// The installed runner, if any.
pub fn runner() -> Option<SmartFilterRunner> {
    RUNNER.get().copied()
}

/// `true` when a smart object carrying `stack` renders through its filters
/// rather than as its bare source: some filter is active and a runner is
/// installed to run it.
pub fn renders_filtered(stack: &[SmartFilter]) -> bool {
    runner().is_some() && layer_model::stack_is_active(stack)
}

/// Feed everything about `stack` that can change a pixel into `h`, plus
/// whether a runner is installed. Inactive filters are skipped, so switching a
/// filter off keys exactly as never having added it — and the tiles cached for
/// the unfiltered object are served again.
pub fn hash_stack<H: Hasher>(stack: &[SmartFilter], h: &mut H) {
    runner().is_some().hash(h);
    for f in stack.iter().filter(|f| f.is_active()) {
        f.filter.hash(h);
        f.effective_opacity().to_bits().hash(h);
        f.blend_mode.shader_index().hash(h);
        f.params.len().hash(h);
        for (key, value) in &f.params {
            key.hash(h);
            match value {
                SmartParam::Float(v) => (0u8, v.to_bits()).hash(h),
                SmartParam::Int(v) => (1u8, *v).hash(h),
                SmartParam::Bool(v) => (2u8, *v).hash(h),
                SmartParam::Choice(v) => (3u8, *v).hash(h),
                SmartParam::Color(c) => {
                    4u8.hash(h);
                    for v in c {
                        v.to_bits().hash(h);
                    }
                }
            }
        }
    }
}

/// Run `stack` over `source` with `runner`, bottom-up, and return the result
/// on the same rect.
///
/// Each active filter sees what the filters below it produced. Its result
/// lands through the filter's own blend mode and opacity: Normal at 100 % is
/// exactly the filtered pixels, lower opacities mix back toward the input.
pub fn apply_stack(source: &Canvas, stack: &[SmartFilter], runner: SmartFilterRunner) -> Canvas {
    let rect = source.rect();
    let mut current = source.pixels().to_vec();
    for filter in stack.iter().filter(|f| f.is_active()) {
        let Ok(buffer) = FilterBuffer::from_pixels(rect.width, rect.height, current.clone()) else {
            break;
        };
        let Some(out) = runner(filter, &buffer) else {
            continue;
        };
        if out.dimensions() != (rect.width, rect.height) {
            continue;
        }
        let k = filter.effective_opacity();
        for (dst, &fx) in current.iter_mut().zip(out.pixels()) {
            let layered = blend(filter.blend_mode, *dst, fx);
            *dst = lerp4(*dst, layered, k);
        }
    }
    Canvas::from_pixels(rect, current).unwrap_or_else(|_| source.clone())
}

/// W10-I: [`apply_stack`] seen through the smart filters' shared mask:
/// `coverage` (one value per pixel of `source`, row-major, already resolved
/// through the mask's invert and density) weighs the filtered result against
/// the unfiltered source — 1 shows the filters, 0 the bare source, as
/// Photopea's filter mask does. `None` is no mask: the stack's result as is.
/// A coverage of the wrong length is ignored rather than misapplied.
pub fn apply_stack_masked(
    source: &Canvas,
    stack: &[SmartFilter],
    runner: SmartFilterRunner,
    coverage: Option<&[f32]>,
) -> Canvas {
    let filtered = apply_stack(source, stack, runner);
    let Some(coverage) = coverage.filter(|c| c.len() == source.pixels().len()) else {
        return filtered;
    };
    let mixed: Vec<[f32; 4]> = source
        .pixels()
        .iter()
        .zip(filtered.pixels())
        .zip(coverage)
        .map(|((&src, &fx), &k)| lerp4(src, fx, k.clamp(0.0, 1.0)))
        .collect();
    Canvas::from_pixels(source.rect(), mixed).unwrap_or(filtered)
}

/// W10-I: everything about a filter mask besides its stored tiles that can
/// change a pixel — whether there is one, and its switch, invert and
/// density — into `h`, for the filtered extent's cache key.
pub fn hash_filter_mask<H: Hasher>(mask: Option<&layer_model::LayerMask>, h: &mut H) {
    match mask {
        None => 0u8.hash(h),
        Some(m) => {
            1u8.hash(h);
            m.id.0.as_bytes().hash(h);
            m.enabled.hash(h);
            m.inverted.hash(h);
            m.density().to_bits().hash(h);
        }
    }
}

/// One filtered pixel `fx` put back over the pixel it was made from, `below`,
/// in `mode`. Both premultiplied.
///
/// Normal is plain replacement — a blur's soft edge must come out soft, not
/// laid over the sharp original. Every other mode mixes the two colours with
/// the reference [`BlendMode::blend_rgb`] where the input had colour, and
/// keeps the filtered colour where it had none; the alpha is the filtered
/// alpha either way.
fn blend(mode: BlendMode, below: [f32; 4], fx: [f32; 4]) -> [f32; 4] {
    if mode == BlendMode::Normal {
        return fx;
    }
    let a = fx[3];
    if a <= 0.0 {
        return fx;
    }
    let ab = below[3].clamp(0.0, 1.0);
    let s = [fx[0] / a, fx[1] / a, fx[2] / a];
    let b = if ab > 0.0 {
        [below[0] / ab, below[1] / ab, below[2] / ab]
    } else {
        s
    };
    let mixed = mode.blend_rgb(b, s);
    let c = [
        (1.0 - ab) * s[0] + ab * mixed[0],
        (1.0 - ab) * s[1] + ab * mixed[1],
        (1.0 - ab) * s[2] + ab * mixed[2],
    ];
    [c[0] * a, c[1] * a, c[2] * a, a]
}

fn lerp4(a: [f32; 4], b: [f32; 4], k: f32) -> [f32; 4] {
    [
        a[0] + (b[0] - a[0]) * k,
        a[1] + (b[1] - a[1]) * k,
        a[2] + (b[2] - a[2]) * k,
        a[3] + (b[3] - a[3]) * k,
    ]
}

/// Which filter of which layer's stack the Layers panel asked to re-edit —
/// the double-click on a smart-filter row.
///
/// The panel (in `ui`) cannot call into the application, and the Filter
/// dialog the application opens in answer is the ordinary one for that
/// filter. This is the note passed between the two: the panel leaves it
/// ([`request_edit`]) as it asks for the dialog, and the application takes it
/// ([`take_edit_request`]) when the dialog opens, to fill in the stored
/// parameters and to replace that entry rather than append a new one on
/// confirm. Per thread, because the panel and the dialog host share the UI
/// thread and nothing else should see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditRequest {
    pub layer: layer_model::LayerId,
    /// Index into the layer's stack, bottom (first applied) = 0.
    pub index: usize,
}

thread_local! {
    static EDIT_REQUEST: std::cell::Cell<Option<EditRequest>> =
        const { std::cell::Cell::new(None) };
}

/// Leave an [`EditRequest`] for the next Filter dialog to open.
pub fn request_edit(request: EditRequest) {
    EDIT_REQUEST.with(|r| r.set(Some(request)));
}

/// Take the pending [`EditRequest`], leaving none.
pub fn take_edit_request() -> Option<EditRequest> {
    EDIT_REQUEST.with(|r| r.take())
}

/// W10-I: the stack with the filter at `from` moved to `to` — the Layers
/// panel's drag of a smart-filter row onto another. Both are stack indices
/// (bottom, first applied, = 0), and every other filter keeps its relative
/// order. `None` when the move changes nothing or either index is outside
/// the stack, so a drop on the row it started from emits no command.
///
/// Order is part of the result: filters do not commute (a blur then a
/// sharpen is not a sharpen then a blur), which is why the drag exists.
pub fn move_filter(stack: &[SmartFilter], from: usize, to: usize) -> Option<Vec<SmartFilter>> {
    if from == to || from >= stack.len() || to >= stack.len() {
        return None;
    }
    let mut next = stack.to_vec();
    let moved = next.remove(from);
    next.insert(to, moved);
    Some(next)
}

/// How many filtered extents the cache keeps.
const CACHE_ENTRIES: usize = 8;

/// The most pixels the cache keeps in total. A single extent past this is
/// still rendered, just not kept.
const CACHE_PIXELS: u64 = 64 * 1024 * 1024;

type Entry = (u64, Arc<Canvas>);

fn cache() -> &'static Mutex<Vec<Entry>> {
    static CACHE: OnceLock<Mutex<Vec<Entry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// The filtered extent cached under `key`, or `make`'s answer, cached.
///
/// Most-recently-used last; the oldest entries are dropped first when either
/// bound is passed. A poisoned lock is treated as an empty cache rather than
/// failing the frame.
pub(crate) fn cached_or<F>(key: u64, make: F) -> Arc<Canvas>
where
    F: FnOnce() -> Canvas,
{
    if let Ok(mut entries) = cache().lock() {
        if let Some(i) = entries.iter().position(|(k, _)| *k == key) {
            let entry = entries.remove(i);
            let canvas = Arc::clone(&entry.1);
            entries.push(entry);
            return canvas;
        }
    }
    let made = Arc::new(make());
    if let Ok(mut entries) = cache().lock() {
        let size = |c: &Canvas| u64::from(c.width()) * u64::from(c.height());
        if size(&made) <= CACHE_PIXELS {
            entries.retain(|(k, _)| *k != key);
            entries.push((key, Arc::clone(&made)));
            let mut total: u64 = entries.iter().map(|(_, c)| size(c)).sum();
            while entries.len() > CACHE_ENTRIES || total > CACHE_PIXELS {
                let (_, dropped) = entries.remove(0);
                total -= size(&dropped);
            }
        }
    }
    made
}

/// A fresh hasher for a cache key, seeded with a domain tag.
pub(crate) fn key_hasher() -> DefaultHasher {
    let mut h = DefaultHasher::new();
    "smart-filter-extent".hash(&mut h);
    h
}

#[cfg(test)]
pub(crate) mod test_runner {
    //! The runner the compositor's own tests install: the three filters they
    //! use, called the way the application's table calls them.
    use super::*;

    pub(crate) fn run(f: &SmartFilter, src: &FilterBuffer) -> Option<FilterBuffer> {
        let float = |k: &str| match f.params.get(k) {
            Some(SmartParam::Float(v)) => *v,
            _ => 0.0,
        };
        match f.filter.as_str() {
            "GaussianBlur" => Some(filters::blur::gaussian_blur(
                src,
                float("radius"),
                filters::EdgeMode::Clamp,
            )),
            "Invert" => {
                let mut out = src.clone();
                for p in out.pixels_mut() {
                    *p = [p[3] - p[0], p[3] - p[1], p[3] - p[2], p[3]];
                }
                Some(out)
            }
            "Brighten" => {
                let mut out = src.clone();
                for p in out.pixels_mut() {
                    for c in 0..3 {
                        p[c] = (p[c] * 2.0).min(p[3]);
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// Install [`run`]. Every compositor test that needs smart filters calls
    /// this; the first call in the process wins, and it is the same function.
    pub(crate) fn install() {
        install_runner(run);
        assert!(runner().is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster::PixelRect;
    use std::collections::BTreeMap;

    fn blur(radius: f32) -> SmartFilter {
        let mut p = BTreeMap::new();
        p.insert("radius".to_string(), SmartParam::Float(radius));
        SmartFilter::new("GaussianBlur", p)
    }

    /// A 16x1 hard edge: black on the left, white on the right, opaque.
    fn edge() -> Canvas {
        let rect = PixelRect::new(0, 0, 16, 1);
        let px = (0..16)
            .map(|x| {
                if x < 8 {
                    [0.0, 0.0, 0.0, 1.0]
                } else {
                    [1.0; 4]
                }
            })
            .collect();
        Canvas::from_pixels(rect, px).unwrap()
    }

    #[test]
    fn an_empty_or_switched_off_stack_returns_the_source() {
        let src = edge();
        assert_eq!(apply_stack(&src, &[], test_runner::run), src);
        let mut off = blur(3.0);
        off.enabled = false;
        assert_eq!(apply_stack(&src, &[off], test_runner::run), src);
    }

    #[test]
    fn a_blur_softens_the_edge_and_a_half_opacity_blur_lands_halfway() {
        let src = edge();
        let full = apply_stack(&src, &[blur(3.0)], test_runner::run);
        let px7 = full.get(7, 0)[0];
        assert!(px7 > 0.05 && px7 < 0.5, "{px7}");
        let mut half = blur(3.0);
        half.opacity = 0.5;
        let halfway = apply_stack(&src, &[half], test_runner::run);
        assert!((halfway.get(7, 0)[0] - px7 * 0.5).abs() < 1e-5);
    }

    #[test]
    fn the_stack_runs_bottom_up() {
        // Brighten clamps, so it does not commute with Invert: 0.3 brightened
        // then inverted is 0.4, inverted then brightened is 1.0.
        let rect = PixelRect::new(0, 0, 1, 1);
        let src = Canvas::from_pixels(rect, vec![[0.3, 0.3, 0.3, 1.0]]).unwrap();
        let invert = SmartFilter::new("Invert", BTreeMap::new());
        let brighten = SmartFilter::new("Brighten", BTreeMap::new());
        let a = apply_stack(&src, &[brighten.clone(), invert.clone()], test_runner::run);
        let b = apply_stack(&src, &[invert, brighten], test_runner::run);
        assert!((a.get(0, 0)[0] - 0.4).abs() < 1e-5, "{:?}", a.get(0, 0));
        assert!((b.get(0, 0)[0] - 1.0).abs() < 1e-5, "{:?}", b.get(0, 0));
    }

    #[test]
    fn an_unknown_filter_is_a_pass_through() {
        let src = edge();
        let odd = SmartFilter::new("NotAFilter", BTreeMap::new());
        assert_eq!(apply_stack(&src, &[odd], test_runner::run), src);
    }

    #[test]
    fn a_non_normal_blend_mixes_with_the_input() {
        // Multiply of the inverted image over the original: black stays black,
        // white times black is black — the whole edge goes dark.
        let src = edge();
        let mut invert = SmartFilter::new("Invert", BTreeMap::new());
        invert.blend_mode = BlendMode::Multiply;
        let out = apply_stack(&src, &[invert], test_runner::run);
        for x in 0..16 {
            assert!(out.get(x, 0)[0] < 1e-5, "{x}: {:?}", out.get(x, 0));
            assert!((out.get(x, 0)[3] - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn switching_a_filter_off_keys_like_never_adding_it() {
        test_runner::install();
        let key = |stack: &[SmartFilter]| {
            let mut h = DefaultHasher::new();
            hash_stack(stack, &mut h);
            h.finish()
        };
        let mut off = blur(3.0);
        off.enabled = false;
        assert_eq!(key(&[off.clone()]), key(&[]));
        assert_ne!(key(&[blur(3.0)]), key(&[]));
        assert_ne!(key(&[blur(3.0)]), key(&[blur(4.0)]));
        let mut faded = blur(3.0);
        faded.opacity = 0.5;
        assert_ne!(key(&[faded]), key(&[blur(3.0)]));
    }

    #[test]
    fn the_cache_serves_by_key_and_evicts_the_oldest() {
        let mut made = 0;
        let rect = PixelRect::new(0, 0, 2, 2);
        let base = 0xC0FF_EE00_u64;
        let a = cached_or(base, || {
            made += 1;
            Canvas::transparent(rect).unwrap()
        });
        let b = cached_or(base, || {
            made += 1;
            Canvas::transparent(rect).unwrap()
        });
        assert_eq!(made, 1);
        assert!(Arc::ptr_eq(&a, &b));
        for i in 1..=(CACHE_ENTRIES as u64 + 1) {
            cached_or(base + i, || Canvas::transparent(rect).unwrap());
        }
        let c = cached_or(base, || {
            made += 1;
            Canvas::transparent(rect).unwrap()
        });
        assert_eq!(made, 2, "the oldest entry was evicted");
        assert!(!Arc::ptr_eq(&a, &c));
    }
}

#[cfg(test)]
mod composite_tests {
    //! The route through the real compositor: a smart-object layer in a
    //! document, composited by [`crate::composite_region`] and the caching
    //! [`crate::TileCompositor`].
    use super::*;
    use crate::testkit::TestDoc;
    use crate::{composite_region, CompositeOptions, TileCompositor};
    use layer_model::{AssetId, Layer, LayerId, LayerKind, SmartObjectLayer};
    use raster::{PixelRect, TileCoord};
    use std::collections::BTreeMap;

    fn blur(radius: f32) -> SmartFilter {
        let mut p = BTreeMap::new();
        p.insert("radius".to_string(), SmartParam::Float(radius));
        SmartFilter::new("GaussianBlur", p)
    }

    /// A 512x256 linear document holding one smart object whose left tile is
    /// opaque black and right tile opaque white: a hard edge exactly on the
    /// tile boundary at x = 256.
    fn edge_doc() -> (TestDoc, LayerId) {
        let mut t = TestDoc::linear(512, 256);
        let id = t.push(Layer::with_kind(
            "Smart",
            LayerKind::SmartObject(SmartObjectLayer {
                asset: AssetId::new(),
                linked: false,
                filters: Vec::new(),
                filter_mask: None,
            }),
        ));
        t.paint_tile(id, TileCoord::new(0, 0, 0), [0, 0, 0, 255]);
        t.paint_tile(id, TileCoord::new(1, 0, 0), [255, 255, 255, 255]);
        (t, id)
    }

    fn set_stack(t: &mut TestDoc, id: LayerId, stack: Vec<SmartFilter>) {
        match &mut t.doc.layers.get_mut(id).unwrap().kind {
            LayerKind::SmartObject(so) => so.filters = stack,
            _ => unreachable!(),
        }
    }

    fn left_tile(t: &TestDoc) -> Canvas {
        composite_region(
            &t.doc,
            &t.src,
            PixelRect::new(0, 0, 256, 256),
            0,
            CompositeOptions::default(),
        )
        .unwrap()
    }

    #[test]
    fn a_blur_on_a_smart_object_reads_across_the_tile_edge_and_keeps_its_source() {
        test_runner::install();
        let (mut t, id) = edge_doc();
        let tiles_before = t.doc.layer_tiles(id).unwrap().clone();
        let bare = left_tile(&t);
        assert_eq!(bare.get(255, 100), [0.0, 0.0, 0.0, 1.0]);

        set_stack(&mut t, id, vec![blur(4.0)]);
        let blurred = left_tile(&t);
        // The region asked for is the black tile alone; the white spilling
        // into its last column can only come from the whole-extent render.
        let edge = blurred.get(255, 100)[0];
        assert!(edge > 0.1 && edge < 0.9, "{edge}");
        assert!(
            blurred.get(10, 100)[0] < 1e-4,
            "far from the edge stays black"
        );
        assert_eq!(
            t.doc.layer_tiles(id).unwrap(),
            &tiles_before,
            "the source tiles are untouched"
        );

        // The eye off: the object draws its bare source again.
        t.doc.layers.get_mut(id).unwrap().kind = match &t.doc.layers.get(id).unwrap().kind {
            LayerKind::SmartObject(so) => {
                let mut so = so.clone();
                so.filters[0].enabled = false;
                LayerKind::SmartObject(so)
            }
            _ => unreachable!(),
        };
        assert_eq!(left_tile(&t), bare);

        // A larger radius reaches further in.
        set_stack(&mut t, id, vec![blur(4.0)]);
        let near = left_tile(&t).get(248, 100)[0];
        set_stack(&mut t, id, vec![blur(12.0)]);
        let far = left_tile(&t).get(248, 100)[0];
        assert!(
            far > near + 1e-3,
            "radius 12 reaches x = 248 more: {near} vs {far}"
        );
    }

    #[test]
    fn the_tile_cache_sees_every_stack_edit_by_key_alone() {
        test_runner::install();
        let (mut t, id) = edge_doc();
        let region = PixelRect::new(0, 0, 512, 256);
        let mut tc = TileCompositor::new();
        let cold = |t: &TestDoc| {
            TileCompositor::new()
                .composite_region(&t.doc, &t.src, region, 0, CompositeOptions::default())
                .unwrap()
        };
        let mut warm = |t: &TestDoc| {
            tc.composite_region(&t.doc, &t.src, region, 0, CompositeOptions::default())
                .unwrap()
        };
        let bare = warm(&t);
        for stack in [
            vec![blur(4.0)],
            vec![blur(9.0)],
            {
                let mut f = blur(9.0);
                f.opacity = 0.5;
                vec![f]
            },
            {
                let mut f = blur(9.0);
                f.enabled = false;
                vec![f]
            },
        ] {
            set_stack(&mut t, id, stack);
            assert_eq!(warm(&t), cold(&t), "a stack edit served a stale tile");
        }
        assert_eq!(warm(&t), bare, "the eye off is the bare source");
        // Repainting the *other* tile of a filtered object must reach this
        // one: the white tile is what bleeds into the black one.
        set_stack(&mut t, id, vec![blur(4.0)]);
        let before = warm(&t);
        t.paint_tile(id, TileCoord::new(1, 0, 0), [0, 0, 0, 255]);
        let after = warm(&t);
        assert_eq!(after, cold(&t));
        assert!(after.get(255, 100)[0] < before.get(255, 100)[0]);
    }

    /// W10-I: dragging a smart-filter row reorders the stack through
    /// [`move_filter`], and the compositor renders the new order. Invert and
    /// Brighten do not commute on a grey source (Invert then Brighten is
    /// min(2(1 - g), 1); Brighten then Invert is 1 - min(2g, 1)), so the two
    /// orders give two composites; moving back restores the first exactly.
    #[test]
    fn a_reordered_stack_renders_in_its_new_order() {
        test_runner::install();
        let (mut t, id) = edge_doc();
        t.paint_tile(id, TileCoord::new(0, 0, 0), [64, 64, 64, 255]);
        let named = |key: &str| SmartFilter::new(key, BTreeMap::new());
        let stack = vec![named("Invert"), named("Brighten")];
        // Moving a filter onto itself, or past either end, is no move.
        assert_eq!(move_filter(&stack, 1, 1), None);
        assert_eq!(move_filter(&stack, 0, 2), None);
        assert_eq!(move_filter(&stack, 2, 0), None);

        set_stack(&mut t, id, stack.clone());
        let invert_first = left_tile(&t).get(10, 100)[0];

        let moved = move_filter(&stack, 1, 0).expect("a real move");
        assert_eq!(moved[0].filter, "Brighten");
        assert_eq!(moved[1].filter, "Invert");
        set_stack(&mut t, id, moved.clone());
        let brighten_first = left_tile(&t).get(10, 100)[0];
        assert!(
            (invert_first - brighten_first).abs() > 0.1,
            "the order must change the pixel: {invert_first} vs {brighten_first}"
        );

        set_stack(&mut t, id, move_filter(&moved, 0, 1).expect("and back"));
        assert_eq!(left_tile(&t).get(10, 100)[0], invert_first);
        assert_eq!(
            t.doc.layer_tiles(id).map(|m| m.iter().count()),
            Some(2),
            "reordering never touches the source tiles"
        );
    }

    fn set_filter_mask(t: &mut TestDoc, id: LayerId, mask: Option<layer_model::LayerMask>) {
        match &mut t.doc.layers.get_mut(id).unwrap().kind {
            LayerKind::SmartObject(so) => so.filter_mask = mask,
            _ => unreachable!(),
        }
    }

    /// W10-I: the smart filters' shared mask weighs the filtered result
    /// against the bare source, pixel by pixel: where it is white the Invert
    /// shows, where it is black the object's own (black) source does. Its
    /// switch and its invert are honoured, and the caching tile compositor
    /// sees a repaint of the mask by key alone.
    #[test]
    fn the_filter_mask_limits_the_smart_filters_to_where_it_is_white() {
        test_runner::install();
        let (mut t, id) = edge_doc();
        set_stack(
            &mut t,
            id,
            vec![SmartFilter::new("Invert", BTreeMap::new())],
        );
        assert!(
            (left_tile(&t).get(10, 200)[0] - 1.0).abs() < 1e-5,
            "no mask: the Invert shows everywhere"
        );
        // Top half of the left tile white, bottom half black; the right tile
        // white.
        let mask_id = layer_model::MaskId::new();
        t.paint_mask_with(mask_id, TileCoord::new(0, 0, 0), |_, y| {
            if y < 128 {
                255
            } else {
                0
            }
        });
        t.paint_mask_tile(mask_id, TileCoord::new(1, 0, 0), 255);
        let mut mask = layer_model::LayerMask::new(mask_id);
        set_filter_mask(&mut t, id, Some(mask.clone()));
        let masked = left_tile(&t);
        assert!(
            (masked.get(10, 50)[0] - 1.0).abs() < 1e-5,
            "white mask: filtered"
        );
        assert!(masked.get(10, 200)[0] < 1e-5, "black mask: the bare source");
        assert!((masked.get(10, 200)[3] - 1.0).abs() < 1e-6);

        // Inverted, the halves swap.
        mask.inverted = true;
        set_filter_mask(&mut t, id, Some(mask.clone()));
        let inverted = left_tile(&t);
        assert!(inverted.get(10, 50)[0] < 1e-5);
        assert!((inverted.get(10, 200)[0] - 1.0).abs() < 1e-5);

        // Switched off, the mask is ignored.
        mask.inverted = false;
        mask.enabled = false;
        set_filter_mask(&mut t, id, Some(mask.clone()));
        assert!((left_tile(&t).get(10, 200)[0] - 1.0).abs() < 1e-5);

        // The caching compositor: repainting the mask reaches the cache.
        mask.enabled = true;
        set_filter_mask(&mut t, id, Some(mask));
        let region = PixelRect::new(0, 0, 512, 256);
        let mut tc = TileCompositor::new();
        let before = tc
            .composite_region(&t.doc, &t.src, region, 0, CompositeOptions::default())
            .unwrap();
        assert!(before.get(10, 200)[0] < 1e-5);
        t.paint_mask_tile(mask_id, TileCoord::new(0, 0, 0), 255);
        let after = tc
            .composite_region(&t.doc, &t.src, region, 0, CompositeOptions::default())
            .unwrap();
        let cold = TileCompositor::new()
            .composite_region(&t.doc, &t.src, region, 0, CompositeOptions::default())
            .unwrap();
        assert_eq!(after, cold, "a mask repaint served a stale tile");
        assert!((after.get(10, 200)[0] - 1.0).abs() < 1e-5);
    }
}
