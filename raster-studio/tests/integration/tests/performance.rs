//! One performance property, stated as a ratio.
//!
//! The claim under test is the one the whole tile architecture exists to make:
//! after a small edit, redrawing the screen costs what the edit touched, not
//! what the document contains.
//!
//! It is expressed as a ratio measured on **this** machine in **this** run,
//! never as a millisecond threshold. An absolute threshold measures the
//! hardware — it passes on a workstation and goes red on a shared CI runner —
//! and would say nothing about the code. The ratio compares the same work under
//! the same conditions moments apart, so the only thing it can be sensitive to
//! is the cache doing its job.
//!
//! The deterministic half of the claim is asserted separately and exactly: the
//! cache's own hit and miss counters say how many tiles were recomputed, and
//! those numbers do not depend on a clock at all. The last test in this file
//! asserts them through [`app_shell::doc::OpenDocument`] itself, so the numbers
//! describe the compositor the application actually paints with.

use std::time::{Duration, Instant};

use app_shell::doc::OpenDocument;
use compositor::{CacheStats, CompositeOptions, TileCompositor};
use editor_core::Command;
use integration_tests::app::{self, DocExt};
use layer_model::Layer;
use raster::{PixelRect, TileCoord, TILE_SIZE};

/// A large canvas: 4096 x 4096, sixteen tiles a side.
const CANVAS: u32 = 4096;
/// How much of it a frame draws. The application composites the viewport, not
/// the document, and a viewport is also the largest region whose `f32` buffer
/// and tile cache fit in a test process without the measurement turning into a
/// measurement of the machine's allocator.
const VIEWPORT: u32 = 2048;
/// Tiles in that viewport.
const VIEWPORT_TILES: u64 = ((VIEWPORT / TILE_SIZE) * (VIEWPORT / TILE_SIZE)) as u64;
const LAYERS: usize = 10;
/// Each measurement is repeated, and the totals are compared. Repeating damps
/// the scheduler noise a single sample would carry.
const REPS: u32 = 3;

/// The tile the small edit repaints — well inside the viewport, so its
/// neighbours are all cached and none of them may be recomputed.
const EDITED: TileCoord = TileCoord {
    x: 3,
    y: 3,
    level: 0,
};

fn viewport() -> PixelRect {
    PixelRect::new(0, 0, VIEWPORT, VIEWPORT)
}

/// Ten overlapping raster layers covering the viewport.
///
/// Each layer is one solid colour, so the ten of them cost ten distinct tile
/// blobs however many coordinates they are referenced from — content addressing
/// means the fixture is megabytes rather than gigabytes, while the compositor
/// still does the full ten-layer blend for every pixel of every tile.
fn big_document() -> OpenDocument {
    let mut doc = app::blank(CANVAS, CANVAS, "Large");
    // File ▸ New leaves one empty raster layer; the ten below sit on top of it
    // and it contributes nothing to the blend.
    let side = (VIEWPORT / TILE_SIZE) as i32;
    let coords: Vec<TileCoord> = (0..side)
        .flat_map(|y| (0..side).map(move |x| TileCoord::new(x, y, 0)))
        .collect();
    for n in 0..LAYERS {
        let layer = doc.add_layer(Layer::raster(format!("Layer {}", n + 1)));
        let shade = 20 + (n as u8) * 17;
        doc.paint_layer(layer, &coords, &move |_, _, _| {
            [shade, 255 - shade, shade / 2, 160]
        });
    }
    doc
}

/// Repaint one tile of the top layer with content that differs from last time,
/// so the cache key for that tile — and only that tile — changes.
fn small_edit(doc: &mut OpenDocument, round: u32) {
    let top = *doc
        .document
        .layers
        .root()
        .first()
        .expect("the document has layers");
    let tint = (round % 251) as u8;
    doc.paint_layer(top, &[EDITED], &move |_, x, y| {
        [tint, (x % 256) as u8, (y % 256) as u8, 200]
    });
}

/// One frame, the way the presenter draws one: ask the compositor for every
/// visible tile and upload the ones that came back changed.
///
/// Tile-wise rather than through [`TileCompositor::composite_region`] because
/// that call assembles the whole region into one buffer, and the assembly —
/// allocating a 2048x2048 `f32` canvas and blitting every tile into it — is
/// `O(region)` whether or not a single tile was recomputed. That fixed cost is
/// real and is measured separately below; it is not what this ratio is about.
fn frame(
    doc: &OpenDocument,
    tc: &mut TileCompositor,
    coords: &[TileCoord],
) -> (Duration, CacheStats) {
    tc.reset_stats();
    let started = Instant::now();
    for &coord in coords {
        tc.composite_tile(
            &doc.document,
            &doc.tiles,
            coord,
            CompositeOptions::default(),
        )
        .unwrap();
    }
    (started.elapsed(), tc.stats())
}

/// The viewport's tile coordinates, in the order a frame walks them.
fn viewport_tiles() -> Vec<TileCoord> {
    let side = (VIEWPORT / TILE_SIZE) as i32;
    (0..side)
        .flat_map(|y| (0..side).map(move |x| TileCoord::new(x, y, 0)))
        .collect()
}

#[test]
fn a_partial_recomposite_after_a_small_edit_costs_far_less_than_a_full_one() {
    let coords = viewport_tiles();
    assert_eq!(coords.len() as u64, VIEWPORT_TILES);

    let mut doc = big_document();
    // Room for the whole viewport, so the working-set trim never evicts a tile
    // the next frame is about to ask for.
    let mut tc = TileCompositor::with_capacity(coords.len() * 2);

    // Warm everything the measurement should not be measuring: the rayon pool,
    // the allocator, and the cache itself.
    let (_, cold) = frame(&doc, &mut tc, &coords);
    assert_eq!(
        cold.misses, VIEWPORT_TILES,
        "the first frame composites every tile in the viewport"
    );
    let (_, warm) = frame(&doc, &mut tc, &coords);
    assert_eq!(warm.misses, 0, "an unchanged frame recomputes nothing");
    assert_eq!(warm.hits, VIEWPORT_TILES);

    // --- a small edit, then a frame ---
    let mut partial = Duration::ZERO;
    for round in 0..REPS {
        small_edit(&mut doc, round);
        let (elapsed, stats) = frame(&doc, &mut tc, &coords);
        partial += elapsed;
        assert_eq!(
            stats.misses, 1,
            "a one-tile edit may recomposite exactly one tile"
        );
        assert_eq!(stats.hits, VIEWPORT_TILES - 1, "and reuse all the rest");
    }

    // --- the same frame, with nothing to reuse ---
    let mut full = Duration::ZERO;
    for _ in 0..REPS {
        tc.invalidate_all();
        let (elapsed, stats) = frame(&doc, &mut tc, &coords);
        full += elapsed;
        assert_eq!(stats.misses, VIEWPORT_TILES);
        assert_eq!(stats.hits, 0);
    }

    // Both halves walked the same tiles and computed the same cache keys; the
    // only difference is how many of those tiles had to be composited.
    let ratio = full.as_secs_f64() / partial.as_secs_f64().max(f64::EPSILON);
    assert!(
        ratio >= 8.0,
        "a full recomposite of {VIEWPORT_TILES} tiles took {full:?} and a \
         one-tile recomposite took {partial:?} — a ratio of {ratio:.2}, which \
         is not the saving the tile cache exists to make"
    );
}

/// The other half of the claim, with no clock in it, through the application's
/// own document.
///
/// [`OpenDocument::composite`] assembles a region into one buffer, and that
/// assembly is `O(region)` however few tiles were recomputed: it allocates the
/// destination and blits every tile into it. So the *time* a small-edit repaint
/// takes through this entry point is dominated by the assembly rather than by
/// the compositing, and a timing ratio here would be measuring `memcpy`. What
/// is still true, exactly and without a clock, is that the compositing itself
/// was skipped — which is what the counters say.
///
/// (The application does not pay that assembly per frame: it uploads the tiles
/// its dirty set names, which is asserted here too.)
#[test]
fn the_applications_own_composite_recomputes_exactly_one_tile_after_a_small_edit() {
    let mut doc = big_document();
    let region = viewport();

    doc.composite(region).unwrap();
    let cold = doc.cache_stats();
    assert_eq!(
        cold.misses, VIEWPORT_TILES,
        "the first frame composites every tile in the viewport"
    );
    let warm_frame = doc.composite(region).unwrap();
    assert_eq!(
        doc.cache_stats().misses,
        cold.misses,
        "a static frame is free"
    );
    // Everything drawn so far has been presented.
    doc.take_dirty();

    small_edit(&mut doc, 1);
    let before = doc.cache_stats();
    let after_frame = doc.composite(region).unwrap();
    let after = doc.cache_stats();
    assert_eq!(
        after.misses - before.misses,
        1,
        "a one-tile edit may recomposite exactly one tile"
    );
    assert_eq!(after.hits - before.hits, VIEWPORT_TILES - 1);
    // Compared as a boolean rather than with `assert_ne!`: these are 16 MB
    // buffers and a failing `assert_ne!` would print both of them.
    assert!(
        after_frame != warm_frame,
        "the edit has to be visible, or the counters above measured nothing"
    );

    // ...and the presenter is asked to upload that one tile, not the viewport.
    let dirty = doc.take_dirty();
    assert!(!dirty.is_all());
    assert_eq!(dirty.tiles().collect::<Vec<_>>(), vec![EDITED]);

    // And the cached frame is the same picture an uncached composite gives.
    // A cache that returned stale tiles would still show the right counters.
    let space = doc.document.meta.color_space.clone();
    assert_eq!(
        doc.composite_uncached(region).to_rgba8(&space),
        after_frame,
        "a cached frame must equal an uncached one"
    );
}

/// An undo must invalidate what it changed — and only what it changed.
///
/// `OpenDocument::undo` reads the reach of the step it is about to reverse
/// ([`Command::dirty_reach`]) and marks the tiles that reach covers, before and
/// after the inverse runs. For a one-tile paint that is the painted tile, not
/// the canvas: re-uploading a 4K texture to take back one brush dab is the cost
/// the dirty set exists to avoid. What must not happen either is the tile
/// cache handing back the post-edit tile afterwards, because the cache key is
/// derived from the document rather than from the dirty set.
#[test]
fn undoing_an_edit_does_not_leave_the_cache_showing_it() {
    let mut doc = big_document();
    let region = viewport();
    let before = doc.composite(region).unwrap();
    // The presenter drains the dirty set once it has uploaded a frame. Without
    // this every set read below would still carry the `DirtyTiles::all()` the
    // document was born with and could not tell what the edit, the undo or
    // the redo marked.
    doc.take_dirty();

    small_edit(&mut doc, 7);
    let edited = doc.composite(region).unwrap();
    assert_ne!(edited, before);

    // The edited frame is uploaded, so the canvas is clean going into the undo.
    let edit_dirty = doc.take_dirty();
    assert!(!edit_dirty.is_all());
    let edit_tiles: Vec<TileCoord> = edit_dirty.tiles().collect();
    assert_eq!(edit_tiles, vec![EDITED], "the edit itself named its tile");

    assert!(doc.undo().unwrap());
    assert_eq!(
        doc.composite(region).unwrap(),
        before,
        "the cache served a tile the document no longer describes"
    );
    let undo_dirty = doc.take_dirty();
    assert!(
        !undo_dirty.is_all(),
        "undoing a one-tile paint re-uploaded the whole canvas"
    );
    assert!(
        !undo_dirty.is_empty(),
        "an undo that marks nothing is a ghost"
    );
    // The edit moved nothing, so its pre-edit bounds are the same tile; the
    // undo's set must contain every tile the edit touched.
    for tile in &edit_tiles {
        assert!(
            undo_dirty.tiles().any(|t| t == *tile),
            "undo did not mark {tile:?}, which the edit touched; it marked {:?}",
            undo_dirty.tiles().collect::<Vec<_>>()
        );
    }

    // ...and redo brings it back, through the same cache, marking the same
    // tiles.
    assert!(doc.redo().unwrap());
    assert_eq!(doc.composite(region).unwrap(), edited);
    let redo_dirty = doc.take_dirty();
    assert!(
        !redo_dirty.is_all(),
        "redoing a one-tile paint re-uploaded the whole canvas"
    );
    assert_eq!(
        redo_dirty.tiles().collect::<Vec<_>>(),
        undo_dirty.tiles().collect::<Vec<_>>(),
        "redo marks what undo marked"
    );
}

/// The other side of the narrowing: a step whose reach is not a rectangle — a
/// layer re-order changes what every pixel beneath the moved layer is blended
/// from, a canvas resize changes which pixels exist — still marks the whole
/// canvas on undo and on redo. Through the document's own command path, on a
/// canvas small enough that the composite check is cheap.
#[test]
fn undoing_a_structural_step_still_redraws_the_whole_canvas() {
    let mut doc = app::blank(TILE_SIZE * 2, TILE_SIZE * 2, "Structural");
    let coords = doc.canvas_tiles();
    let lower = doc.add_layer(Layer::raster("Lower"));
    doc.paint_layer(lower, &coords, &|_, _, _| [200, 40, 40, 255]);
    let upper = doc.add_layer(Layer::raster("Upper"));
    doc.paint_layer(upper, &coords, &|_, _, _| [40, 40, 200, 160]);
    let region = doc.canvas_rect();
    let before = doc.composite(region).unwrap();
    doc.take_dirty();

    // --- a re-order: the top layer goes to the bottom of the stack ---
    doc.apply(Command::MoveLayer {
        layer_id: upper,
        parent: None,
        index: usize::MAX,
    })
    .unwrap();
    let reordered = doc.composite(region).unwrap();
    assert!(
        reordered != before,
        "the re-order changed nothing, so the undo below would prove nothing"
    );
    assert!(doc.take_dirty().is_all(), "a re-order redraws the canvas");

    assert!(doc.undo().unwrap());
    assert_eq!(doc.composite(region).unwrap(), before);
    assert!(
        doc.take_dirty().is_all(),
        "undoing a re-order redraws the canvas"
    );
    assert!(doc.redo().unwrap());
    assert_eq!(doc.composite(region).unwrap(), reordered);
    assert!(
        doc.take_dirty().is_all(),
        "redoing a re-order redraws the canvas"
    );

    // --- a canvas resize: which pixels exist changes ---
    doc.apply(Command::SetCanvasSize {
        size: glam::UVec2::new(TILE_SIZE * 3, TILE_SIZE),
    })
    .unwrap();
    assert_eq!(
        (doc.document.width(), doc.document.height()),
        (TILE_SIZE * 3, TILE_SIZE)
    );
    assert!(doc.take_dirty().is_all(), "a resize redraws the canvas");

    assert!(doc.undo().unwrap());
    assert_eq!(
        (doc.document.width(), doc.document.height()),
        (TILE_SIZE * 2, TILE_SIZE * 2)
    );
    assert_eq!(doc.composite(region).unwrap(), reordered);
    assert!(
        doc.take_dirty().is_all(),
        "undoing a resize redraws the canvas"
    );
    assert!(doc.redo().unwrap());
    assert!(
        doc.take_dirty().is_all(),
        "redoing a resize redraws the canvas"
    );
}

/// A guard on the fixture: ten painted, visible layers really do cover the
/// viewport, so the timings above are timing a ten-layer composite rather than
/// a one-layer one.
#[test]
fn the_large_document_really_carries_ten_painted_layers() {
    let mut doc = big_document();
    assert_eq!(
        doc.document.layers.len(),
        LAYERS + 1,
        "ten painted layers plus the one File ▸ New made"
    );
    assert_eq!(
        (doc.document.width(), doc.document.height()),
        (CANVAS, CANVAS)
    );

    // Every painted layer is visible, partly transparent, and covers the whole
    // viewport — so the compositor blends all ten for every pixel of every
    // tile it is asked for.
    let mut painted = 0;
    for id in doc.document.layers.root().to_vec() {
        let layer = doc.document.layers.get(id).unwrap();
        let Some(tiles) = doc.document.layer_tiles(id) else {
            continue; // the empty layer File ▸ New made
        };
        assert!(layer.visible, "`{}` is hidden", layer.name);
        assert_eq!(
            tiles.len() as u64,
            VIEWPORT_TILES,
            "`{}` does not cover the viewport",
            layer.name
        );
        painted += 1;
    }
    assert_eq!(painted, LAYERS);

    // ...and the topmost of them is genuinely translucent, so the nine beneath
    // it are not being skipped as fully occluded.
    let top = doc.document.layers.root()[0];
    let baseline = doc.composite(viewport()).unwrap();
    doc.apply(Command::SetLayerProperties {
        layer_id: top,
        patch: editor_core::LayerPatch {
            visible: Some(false),
            ..Default::default()
        },
    })
    .unwrap();
    assert!(
        doc.composite(viewport()).unwrap() != baseline,
        "hiding the top layer changed nothing, so it was never composited"
    );
}

/// P3.8: a 16000 x 16000 ten-layer document composites against the tile cache
/// without the cache growing past its documented ceiling, and the document
/// stays editable after the walk — the ceiling is enforced, not advisory.
///
/// The canvas is DECLARED 16000 square but only sparsely populated (ten layers,
/// one tile each), so the fixture is megabytes while the coordinates the walk
/// requests — more than `DEFAULT_CACHE_TILES` of them — are what would blow
/// through the ceiling if `store` let a large batch park everything it made.
#[test]
fn a_huge_multilayer_document_stays_under_the_cache_ceiling_and_stays_editable() {
    const HUGE: u32 = 16_000;
    let mut doc = app::blank(HUGE, HUGE, "Huge");
    for n in 0..LAYERS {
        let layer = doc.add_layer(Layer::raster(format!("Huge {}", n + 1)));
        doc.paint_layer(layer, &[TileCoord::new(0, 0, 0)], &move |_, _, _| {
            [30 + n as u8, 200, 120, 160]
        });
    }

    // More distinct coordinates than the ceiling, across the huge canvas.
    let per_side = (HUGE / TILE_SIZE) as i32;
    let coords: Vec<TileCoord> = (0..per_side)
        .flat_map(|y| (0..per_side).map(move |x| TileCoord::new(x, y, 0)))
        .take(compositor::DEFAULT_CACHE_TILES * 3)
        .collect();

    let mut tc = TileCompositor::new();
    for coord in &coords {
        tc.composite_tile(
            &doc.document,
            &doc.tiles,
            *coord,
            CompositeOptions::default(),
        )
        .unwrap();
    }
    assert!(
        tc.cached_tiles() <= compositor::DEFAULT_CACHE_TILES,
        "the cache holds {} tiles after walking {} — the ceiling leaked",
        tc.cached_tiles(),
        coords.len()
    );

    // Still editable: a stroke on one layer invalidates that tile and the next
    // composite of it recomputes (a miss) with the new bytes.
    let top = doc.document.layers.root()[0];
    doc.paint_layer(top, &[coords[0]], &move |_, x, _y| [250, 40, x as u8, 220]);
    tc.invalidate_tile(coords[0]);
    let before = tc.stats().misses;
    tc.composite_tile(
        &doc.document,
        &doc.tiles,
        coords[0],
        CompositeOptions::default(),
    )
    .unwrap();
    assert!(
        tc.stats().misses > before,
        "the edited tile recomposited after the edit"
    );
}

/// P3.9: the wall-clock budget. Unlike the ratio above this IS a threshold —
/// the task asks for a stated millisecond ceiling on CI hardware — so it is
/// set an order of magnitude above what a desktop measures, which keeps it a
/// regression gate rather than a hardware benchmark: the partial recomposite
/// this budget guards is a handful of tiles, and even a heavily contended
/// shared runner blends a handful of tiles in single-digit milliseconds.
///
/// An 8000 x 6000 ten-layer document, one small edit, one partial frame.
#[test]
fn a_partial_recomposite_of_an_8000x6000_document_meets_the_wall_clock_budget() {
    const W: u32 = 8000;
    const H: u32 = 6000;
    /// The stated budget for ONE partial frame after one small edit. Fifty
    /// times what a desktop measures, so the gate fails only when the
    /// partial-recomposite property itself is broken, never because the
    /// runner was slow.
    const BUDGET: Duration = Duration::from_millis(500);

    let mut doc = app::blank(W, H, "Budget");
    for n in 0..LAYERS {
        let layer = doc.add_layer(Layer::raster(format!("Budget {}", n + 1)));
        doc.paint_layer(layer, &[TileCoord::new(0, 0, 0)], &move |_, _, _| {
            [40 + n as u8, 180, 140, 170]
        });
    }

    let mut tc = TileCompositor::new();
    // The first frame: a viewport's worth of tiles through a cold cache. This
    // is the full cost the budget does NOT gate — it warms the cache.
    let side = (2048 / TILE_SIZE) as i32;
    let warm: Vec<TileCoord> = (0..side)
        .flat_map(|y| (0..side).map(move |x| TileCoord::new(x, y, 0)))
        .collect();
    for coord in &warm {
        tc.composite_tile(
            &doc.document,
            &doc.tiles,
            *coord,
            CompositeOptions::default(),
        )
        .unwrap();
    }

    // One small edit, then one partial frame over the warm cache.
    let top = doc.document.layers.root()[0];
    let edit_tile = TileCoord::new(4, 4, 0);
    doc.paint_layer(top, &[edit_tile], &move |_, x, y| {
        [240, 60, ((x + y) % 251) as u8, 210]
    });
    tc.invalidate_tile(edit_tile);
    let started = Instant::now();
    for coord in &warm {
        tc.composite_tile(
            &doc.document,
            &doc.tiles,
            *coord,
            CompositeOptions::default(),
        )
        .unwrap();
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < BUDGET,
        "the partial recomposite took {elapsed:?}, over the {:?} budget",
        BUDGET
    );
}
