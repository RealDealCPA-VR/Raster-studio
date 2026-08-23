//! End-to-end proof that the pipeline `document -> compositor -> GPU` carries
//! the document's own pixels, and that an edit moves only what changed.
//!
//! These need a real adapter, so every test **skips** (prints and returns) when
//! none can be created — the same policy `render`'s GPU tests use, so the suite
//! still runs on a machine with no GPU and no software fallback.
//!
//! What is *not* covered here: the surface, the window, and the egui overlay.
//! Those need a display. The presenter is the last stage that can be checked
//! without one, and it is the stage where "the image is not part of the
//! document" would show up.

use app_shell::doc::{DocumentId, OpenDocument};
use app_shell::import::{document_from_image, DecodedImage};
use app_shell::presenter::CanvasPresenter;
use editor_core::pixels::{PixelTarget, TileDelta, TileEdit};
use editor_core::Command;
use raster::{PixelFormat, Tile, TileCoord, TILE_SIZE};
use render::GpuContext;

fn gpu() -> Option<GpuContext> {
    match pollster::block_on(GpuContext::headless()) {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            eprintln!("SKIP: no GPU adapter available ({e:#})");
            None
        }
    }
}

macro_rules! gpu_or_skip {
    () => {
        match gpu() {
            Some(g) => g,
            None => return,
        }
    };
}

/// A deterministic image whose every pixel differs from its neighbours, so a
/// shifted, flipped or stale upload cannot pass.
fn probe(width: u32, height: u32) -> DecodedImage {
    let mut rgba8 = vec![0u8; (width as usize) * (height as usize) * 4];
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            rgba8[i] = (x % 251) as u8;
            rgba8[i + 1] = (y % 241) as u8;
            rgba8[i + 2] = ((x * 7 + y * 13) % 239) as u8;
            rgba8[i + 3] = 255;
        }
    }
    DecodedImage {
        width,
        height,
        rgba8,
    }
}

fn open(image: &DecodedImage) -> OpenDocument {
    OpenDocument::from_import(
        DocumentId(1),
        document_from_image(image, "probe.png", 100).unwrap(),
    )
}

/// Largest absolute per-channel difference between two RGBA8 buffers.
fn worst_diff(a: &[u8], b: &[u8]) -> i32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x as i32 - *y as i32).abs())
        .max()
        .unwrap_or(0)
}

#[test]
fn the_texture_the_canvas_samples_holds_the_opened_image() {
    let gpu = gpu_or_skip!();
    // Not a multiple of TILE_SIZE, so edge-tile padding is in play.
    let image = probe(300, 200);
    let mut doc = open(&image);

    let mut presenter = CanvasPresenter::new();
    let report = presenter.sync(&gpu, &mut doc).unwrap();
    assert!(
        report.texture_replaced,
        "the first frame builds the texture"
    );
    assert_eq!(report.full_uploads, 1);
    assert_eq!(presenter.size(), (300, 200));
    assert_eq!(presenter.showing(), Some(DocumentId(1)));

    let texture = presenter.texture().expect("a texture was built");
    let back = texture.read_level(&gpu, 0).unwrap();
    assert_eq!((back.width(), back.height()), (300, 200));

    // The texture is sRGB, so the readback is display-encoded exactly like the
    // source. One quantisation step is the compositor's linear round trip.
    let diff = worst_diff(back.as_rgba8(), &image.rgba8);
    assert!(diff <= 1, "the GPU texture differs from the file by {diff}");
}

#[test]
fn a_second_frame_with_no_edit_uploads_nothing() {
    let gpu = gpu_or_skip!();
    let mut doc = open(&probe(300, 200));
    let mut presenter = CanvasPresenter::new();
    presenter.sync(&gpu, &mut doc).unwrap();

    let report = presenter.sync(&gpu, &mut doc).unwrap();
    assert!(
        report.did_nothing(),
        "a static document must not move pixels across the bus: {report:?}"
    );
}

#[test]
fn an_edit_uploads_only_the_tile_it_touched_and_the_texture_shows_it() {
    let gpu = gpu_or_skip!();
    // Three tiles across, two down: plenty that must not be re-uploaded.
    let image = probe(TILE_SIZE * 3, TILE_SIZE * 2);
    let mut doc = open(&image);
    let layer = doc.document.active_layer().unwrap();

    let mut presenter = CanvasPresenter::new();
    presenter.sync(&gpu, &mut doc).unwrap();

    // Repaint exactly one interior tile a flat colour.
    let mut tile = Tile::transparent(PixelFormat::Rgba8);
    for px in tile.data_mut().chunks_exact_mut(4) {
        px.copy_from_slice(&[10, 200, 30, 255]);
    }
    let hash = doc.tiles.insert_tile(&tile);
    let coord = TileCoord::new(1, 0, 0);
    doc.apply(Command::PaintTiles {
        target: PixelTarget::Layer(layer),
        delta: TileDelta::single(TileEdit::set(coord, hash)),
    })
    .unwrap();

    let report = presenter.sync(&gpu, &mut doc).unwrap();
    assert_eq!(
        (
            report.texture_replaced,
            report.full_uploads,
            report.tile_uploads
        ),
        (false, 0, 1),
        "one edited tile must be one tile upload, not a whole-canvas one"
    );

    let back = presenter.texture().unwrap().read_level(&gpu, 0).unwrap();
    let px = |x: u32, y: u32| {
        let i = ((y * (TILE_SIZE * 3) + x) * 4) as usize;
        &back.as_rgba8()[i..i + 4]
    };
    // Inside the edited tile: the new colour.
    let edited = px(TILE_SIZE + 5, 5);
    assert!(
        worst_diff(edited, &[10, 200, 30, 255]) <= 1,
        "the edited tile did not reach the GPU: {edited:?}"
    );
    // Its neighbours: untouched source pixels.
    for (x, y) in [(5u32, 5u32), (TILE_SIZE * 2 + 5, 5), (5, TILE_SIZE + 5)] {
        let i = ((y * (TILE_SIZE * 3) + x) * 4) as usize;
        let expected = &image.rgba8[i..i + 4];
        assert!(
            worst_diff(px(x, y), expected) <= 1,
            "({x},{y}) was disturbed by an edit in another tile"
        );
    }
}

#[test]
fn switching_documents_replaces_the_texture_even_at_the_same_size() {
    let gpu = gpu_or_skip!();
    let first = probe(128, 96);
    let mut second_image = probe(128, 96);
    // Same dimensions, different content — the case where a stale texture is
    // invisible to a size check.
    for px in second_image.rgba8.chunks_exact_mut(4) {
        px.copy_from_slice(&[240, 20, 60, 255]);
    }

    let mut a = open(&first);
    let mut b = OpenDocument::from_import(
        DocumentId(2),
        document_from_image(&second_image, "other.png", 100).unwrap(),
    );

    let mut presenter = CanvasPresenter::new();
    presenter.sync(&gpu, &mut a).unwrap();
    let report = presenter.sync(&gpu, &mut b).unwrap();
    assert!(
        report.texture_replaced,
        "a different document needs its own texture"
    );
    assert_eq!(presenter.showing(), Some(DocumentId(2)));

    let back = presenter.texture().unwrap().read_level(&gpu, 0).unwrap();
    let diff = worst_diff(back.as_rgba8(), &second_image.rgba8);
    assert!(
        diff <= 1,
        "the previous document's pixels are still up: {diff}"
    );
}

#[test]
fn undo_puts_the_original_pixels_back_on_the_gpu() {
    let gpu = gpu_or_skip!();
    let image = probe(200, 150);
    let mut doc = open(&image);
    let layer = doc.document.active_layer().unwrap();

    let mut presenter = CanvasPresenter::new();
    presenter.sync(&gpu, &mut doc).unwrap();

    let mut tile = Tile::transparent(PixelFormat::Rgba8);
    tile.data_mut().fill(200);
    let hash = doc.tiles.insert_tile(&tile);
    doc.apply(Command::PaintTiles {
        target: PixelTarget::Layer(layer),
        delta: TileDelta::single(TileEdit::set(TileCoord::new(0, 0, 0), hash)),
    })
    .unwrap();
    presenter.sync(&gpu, &mut doc).unwrap();
    let painted = presenter
        .texture()
        .unwrap()
        .read_level(&gpu, 0)
        .unwrap()
        .into_rgba8();
    assert!(
        worst_diff(&painted, &image.rgba8) > 1,
        "the paint should have changed what is on the GPU"
    );

    assert!(doc.undo().unwrap());
    presenter.sync(&gpu, &mut doc).unwrap();
    let back = presenter.texture().unwrap().read_level(&gpu, 0).unwrap();
    let diff = worst_diff(back.as_rgba8(), &image.rgba8);
    assert!(diff <= 1, "undo left the canvas {diff} off the original");
}
