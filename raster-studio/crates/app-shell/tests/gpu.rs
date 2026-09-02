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
use app_shell::presenter::{CanvasPresenter, ChannelMask};
use editor_core::pixels::{PixelTarget, TileDelta, TileEdit};
use editor_core::Command;
use raster::{PixelFormat, Tile, TileCoord, TILE_SIZE};
use render::GpuContext;
use ui::panels::channels::ChannelKind;

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
        color_space: color::ColorSpace::Srgb,
        icc_profile: None,
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

#[test]
fn hiding_a_channel_changes_the_texture_the_canvas_samples() {
    // The Channels panel used to move a flag nothing read. This is the whole
    // path it now travels: the panel's state becomes a `ChannelMask`, the
    // presenter re-uploads through it, and the texture the canvas draws has
    // the isolated channel in it — read back from the GPU, not asserted about
    // an intent.
    let gpu = gpu_or_skip!();
    let image = probe(300, 200);
    let mut doc = open(&image);

    let mut presenter = CanvasPresenter::new();
    presenter.sync(&gpu, &mut doc).unwrap();
    let full = presenter
        .texture()
        .unwrap()
        .read_level(&gpu, 0)
        .unwrap()
        .into_rgba8();

    // Isolate red exactly as `Ctrl+3` and the eye toggles do.
    let mut workspace = ui::Workspace::new();
    workspace
        .channels
        .isolate(&doc.document.meta.color_space, ChannelKind::Component(0));
    assert!(presenter.set_channel_mask(ChannelMask::from_channels(&workspace.channels)));

    let report = presenter.sync(&gpu, &mut doc).unwrap();
    assert!(
        report.full_uploads == 1,
        "a channel toggle dirties no tile, so the canvas must be sent whole: {report:?}"
    );
    let isolated = presenter
        .texture()
        .unwrap()
        .read_level(&gpu, 0)
        .unwrap()
        .into_rgba8();
    assert!(
        worst_diff(&isolated, &full) > 1,
        "hiding two channels did not change a single pixel on the GPU"
    );
    for (px, was) in isolated.chunks_exact(4).zip(full.chunks_exact(4)) {
        assert_eq!(px[0], was[0], "red is the channel that was kept");
        assert_eq!(px[1], 0, "green survived isolation");
        assert_eq!(px[2], 0, "blue survived isolation");
        assert_eq!(px[3], was[3], "alpha is not a colour component");
    }

    // Ctrl+2 brings the composite back, and the GPU gets the original again.
    workspace
        .channels
        .isolate(&doc.document.meta.color_space, ChannelKind::Composite);
    assert!(presenter.set_channel_mask(ChannelMask::from_channels(&workspace.channels)));
    presenter.sync(&gpu, &mut doc).unwrap();
    let back = presenter.texture().unwrap().read_level(&gpu, 0).unwrap();
    assert_eq!(
        worst_diff(back.as_rgba8(), &full),
        0,
        "showing every channel again did not restore the composite"
    );
}

/// A two-tone image: the left half one flat colour, the right half another.
///
/// Flat within each half so a power-of-two box downscale is exact, and split so
/// a downscale that read the wrong block, the wrong stride or a mirrored row
/// cannot pass.
fn two_tone(width: u32, height: u32) -> DecodedImage {
    let mut rgba8 = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for _ in 0..height {
        for x in 0..width {
            let px = if x < width / 2 {
                [200, 30, 40, 255]
            } else {
                [30, 60, 200, 255]
            };
            rgba8.extend_from_slice(&px);
        }
    }
    DecodedImage {
        width,
        height,
        rgba8,
        color_space: color::ColorSpace::Srgb,
        icc_profile: None,
    }
}

#[test]
fn a_document_larger_than_the_device_presents_instead_of_aborting() {
    // The one-click crash. `create_texture` returns no `Result`: an oversized
    // request goes to the driver, comes back through wgpu's uncaptured-error
    // handler, and — with the default handler and this build's
    // `panic = "abort"` — kills the process with no unwind, no dialog and no
    // autosave flush. A Nikon Z8 JPEG is 8256x5504 and the WebGPU baseline is
    // 8192, so this used to be reachable by opening an ordinary photograph.
    let gpu = gpu_or_skip!();
    let limit = gpu.max_texture_dimension_2d();
    // One pixel past whatever this machine's device really allows, so the test
    // exercises the refusal on the hardware it is running on rather than on an
    // assumed 8192.
    let width = limit + 1;
    let image = two_tone(width, 64);
    let mut doc = open(&image);

    let mut presenter = CanvasPresenter::new();
    let report = presenter
        .sync(&gpu, &mut doc)
        .expect("an oversized document must present");
    assert!(report.texture_replaced);

    // The camera still maps the document's own extent — the texture is
    // stretched back over it — so panning and zooming are unchanged.
    assert_eq!(presenter.size(), (width, 64));

    let fit = presenter.fit();
    assert_eq!(fit.level, 1, "one halving is enough to fit {width}");
    let (tw, th) = presenter.texture_size();
    assert_eq!((tw, th), (fit.width, fit.height));
    assert!(
        tw <= limit && th <= limit,
        "presented {tw}x{th} on a device that stops at {limit}"
    );
    assert_eq!(
        gpu.take_last_error(),
        None,
        "an oversized texture reached the driver"
    );

    // ...and what is presented is the document's own pixels, halved. Each half
    // is flat, so the box average of every interior block is exactly its
    // colour; one code value of tolerance is the compositor's linear round
    // trip.
    let back = presenter
        .texture()
        .expect("a texture was built")
        .read_level(&gpu, 0)
        .unwrap();
    assert_eq!((back.width(), back.height()), (tw, th));
    let left = back.pixel(4, 4);
    let right = back.pixel(tw - 5, 4);
    assert!(
        worst_diff(&left, &[200, 30, 40, 255]) <= 2,
        "the left half of the document is not on the left of the texture: {left:?}"
    );
    assert!(
        worst_diff(&right, &[30, 60, 200, 255]) <= 2,
        "the right half of the document is not on the right of the texture: {right:?}"
    );

    // A second frame with nothing dirty still costs nothing, downscaled or not.
    assert!(presenter.sync(&gpu, &mut doc).unwrap().did_nothing());
}

#[test]
fn an_edit_to_a_downscaled_document_uploads_only_the_tile_it_touched() {
    // A downscaled document used to have no per-tile path: every dirty frame
    // recomposited the whole canvas band by band and re-uploaded the whole
    // texture, synchronously on the frame path. Measured in release on a 134
    // Mpx document that was 5.4 s per brush dab — 0.18 fps, a hard freeze —
    // and about 1.8 s for the 8256x5504 camera JPEG this change exists for.
    // Tiles are 256 px and aligned, so a dirty tile is an aligned `256 >>
    // level` square of the fitted texture and nothing else touches it.
    let gpu = gpu_or_skip!();
    let limit = gpu.max_texture_dimension_2d();
    let image = two_tone(limit + 1, 64);
    let mut doc = open(&image);
    let layer = doc.document.active_layer().unwrap();

    let mut presenter = CanvasPresenter::new();
    presenter.sync(&gpu, &mut doc).unwrap();
    let fit = presenter.fit();
    assert!(!fit.is_exact(), "the fixture must be downscaled");
    assert!(fit.supports_tiled_upload(limit + 1, 64));

    // What is on screen far from the edit, before the edit.
    let (tw, _) = presenter.texture_size();
    let before = presenter
        .texture()
        .unwrap()
        .read_level(&gpu, 0)
        .unwrap()
        .pixel(tw - 5, 4);

    let mut tile = Tile::transparent(PixelFormat::Rgba8);
    for px in tile.data_mut().chunks_exact_mut(4) {
        px.copy_from_slice(&[10, 200, 30, 255]);
    }
    let hash = doc.tiles.insert_tile(&tile);
    doc.apply(Command::PaintTiles {
        target: PixelTarget::Layer(layer),
        delta: TileDelta::single(TileEdit::set(TileCoord::new(0, 0, 0), hash)),
    })
    .unwrap();

    let report = presenter.sync(&gpu, &mut doc).unwrap();
    assert_eq!(
        report.tile_uploads, 1,
        "one dab must cost one tile upload, not a whole document: {report:?}"
    );
    assert_eq!(
        report.full_uploads, 0,
        "the whole document was recomposited for a 256 px edit: {report:?}"
    );

    let back = presenter.texture().unwrap().read_level(&gpu, 0).unwrap();
    let px = back.pixel(4, 4);
    assert!(
        worst_diff(&px, &[10, 200, 30, 255]) <= 2,
        "the painted tile is not in the presented texture: {px:?}"
    );
    // The texel one past the tile's own square must be untouched, which is
    // where an off-by-one in the level division would land.
    let edge = back.pixel(128, 4);
    assert!(
        worst_diff(&edge, &[200, 30, 40, 255]) <= 2,
        "the upload spilled past the tile's texels: {edge:?}"
    );
    let far = back.pixel(tw - 5, 4);
    assert_eq!(
        far, before,
        "a texel far from the edit changed: {before:?} -> {far:?}"
    );
    assert_eq!(gpu.take_last_error(), None, "the upload was out of range");

    // The last whole tile of the row is where an off-by-one in the texel
    // arithmetic runs past the texture, and an out-of-range `write_texture` is
    // exactly the uncaptured error this whole change exists to stop.
    let last_tile = (image.width / TILE_SIZE - 1) as i32;
    doc.apply(Command::PaintTiles {
        target: PixelTarget::Layer(layer),
        delta: TileDelta::single(TileEdit::set(TileCoord::new(last_tile, 0, 0), hash)),
    })
    .unwrap();
    let report = presenter.sync(&gpu, &mut doc).unwrap();
    assert_eq!(report.tile_uploads, 1, "{report:?}");
    let back = presenter.texture().unwrap().read_level(&gpu, 0).unwrap();
    assert_eq!(
        gpu.take_last_error(),
        None,
        "the edge tile's upload ran past the texture"
    );
    let px = back.pixel(tw - 1, 4);
    assert!(
        worst_diff(&px, &[10, 200, 30, 255]) <= 2,
        "the last texel of the row did not get the edit: {px:?}"
    );
}

#[test]
fn a_downscaled_edit_lands_where_the_whole_document_path_would_put_it() {
    // The per-tile downscaled upload and the whole-document one are two
    // filters over the same pixels, and they have to agree — otherwise the
    // first pan or channel toggle, which forces a full re-upload, would visibly
    // shift or re-shade everything the user painted.
    let gpu = gpu_or_skip!();
    let limit = gpu.max_texture_dimension_2d();
    let image = probe(limit + 1, 64);
    let mut doc = open(&image);
    let layer = doc.document.active_layer().unwrap();

    let mut presenter = CanvasPresenter::new();
    presenter.sync(&gpu, &mut doc).unwrap();
    assert!(!presenter.fit().is_exact());

    let mut tile = Tile::transparent(PixelFormat::Rgba8);
    for (i, px) in tile.data_mut().chunks_exact_mut(4).enumerate() {
        px.copy_from_slice(&[(i % 251) as u8, (i % 199) as u8, (i % 173) as u8, 255]);
    }
    let hash = doc.tiles.insert_tile(&tile);
    doc.apply(Command::PaintTiles {
        target: PixelTarget::Layer(layer),
        delta: TileDelta::single(TileEdit::set(TileCoord::new(1, 0, 0), hash)),
    })
    .unwrap();
    let report = presenter.sync(&gpu, &mut doc).unwrap();
    assert_eq!(report.tile_uploads, 1);
    let tiled = presenter.texture().unwrap().read_level(&gpu, 0).unwrap();

    // Force the whole-document path over the identical document state.
    presenter.set_channel_mask(ChannelMask {
        components: [true, true, false],
    });
    presenter.sync(&gpu, &mut doc).unwrap();
    presenter.set_channel_mask(ChannelMask::ALL);
    let report = presenter.sync(&gpu, &mut doc).unwrap();
    assert_eq!(report.full_uploads, 1, "the fixture must re-upload whole");
    let whole = presenter.texture().unwrap().read_level(&gpu, 0).unwrap();

    assert_eq!(
        worst_diff(tiled.as_rgba8(), whole.as_rgba8()),
        0,
        "the per-tile downscale and the whole-document downscale disagree"
    );
}

/// P3.3: the diagnostics bundle carries the adapter the live wgpu context
/// actually got, and never consents to upload.
#[test]
fn the_diagnostics_bundle_names_the_live_adapter_and_never_consents() {
    let gpu = gpu_or_skip!();
    let name = gpu.adapter.get_info().name;
    assert!(!name.is_empty(), "the adapter reports a name");
    let mut bundle = telemetry::DiagnosticBundle::new(env!("CARGO_PKG_VERSION"));
    bundle.gpu_adapter = name.clone();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("diagnostics.json");
    std::fs::write(&target, bundle.to_json()).unwrap();
    let written = std::fs::read_to_string(&target).unwrap();
    assert!(
        written.contains(&name),
        "the exported JSON names the real adapter"
    );
    let parsed: telemetry::DiagnosticBundle = serde_json::from_str(&written).unwrap();
    assert!(!parsed.upload_consented, "export never consents to upload");
}
