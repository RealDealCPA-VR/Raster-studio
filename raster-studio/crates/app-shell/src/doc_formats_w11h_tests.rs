//! W11-H: the last Photopea format gaps, through the product's own routes.
//!
//! Opening goes through [`Editor::open_path`] (the road drag-and-drop, recent
//! files and startup files take, and which File > Open's job mirrors).
//! Exporting goes through [`Editor::request_export`] (the Export As dialog's
//! confirmed job) and [`Editor::dispatch`]`(Action::Export)` with the picker
//! answering a path (File > Export and File > Save as PSD, worker included).

use std::path::{Path, PathBuf};

use crate::action::Action;
use crate::dialogs::ScriptedDialogs;
use crate::doc::{DocumentId, OpenDocument};
use crate::editor::Editor;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

fn editor(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    )
}

/// 6x4 opaque pixels, every one distinct.
fn pixels() -> Vec<u8> {
    (0..24u32)
        .flat_map(|i| [(i * 10) as u8, (200 - i * 5) as u8, (i * 3) as u8, 255])
        .collect()
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// The active document's canvas size and composite.
fn active(ed: &mut Editor) -> (u32, u32, Vec<u8>) {
    let doc = ed.active_mut().expect("a document opened");
    let (w, h) = (doc.document.width(), doc.document.height());
    let rgba = doc.composite(doc.canvas_rect()).unwrap();
    (w, h, rgba)
}

/// A stored-only ZIP: enough for a `.kra` (Krita writes `mimetype` stored).
fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for b in bytes {
            crc ^= u32::from(*b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    let mut out = Vec::new();
    let mut dir = Vec::new();
    for (name, data) in entries {
        let local = out.len() as u32;
        let sizes = [crc32(data), data.len() as u32, data.len() as u32];
        out.extend_from_slice(b"PK\x03\x04\x14\0\0\0\0\0\0\0\0\0");
        for v in sizes {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        dir.extend_from_slice(b"PK\x01\x02\x14\0\x14\0\0\0\0\0\0\0\0\0");
        for v in sizes {
            dir.extend_from_slice(&v.to_le_bytes());
        }
        dir.extend_from_slice(&(name.len() as u16).to_le_bytes());
        dir.extend_from_slice(&[0; 12]);
        dir.extend_from_slice(&local.to_le_bytes());
        dir.extend_from_slice(name.as_bytes());
    }
    let at = out.len() as u32;
    out.extend_from_slice(&dir);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
    out.extend_from_slice(&at.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

#[test]
fn exr_hdr_icns_iff_and_kra_open_as_documents() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    let px = pixels();
    let png = raster::encode(raster::ExportFormat::Png, 6, 4, &px).unwrap();

    // OpenEXR: through 32-bit float and back, the 8-bit pixels are exact,
    // and the document opens at 32 Bits/Channel.
    let exr = write(
        dir.path(),
        "a.exr",
        &raster::encode(raster::ExportFormat::Exr, 6, 4, &px).unwrap(),
    );
    ed.open_path(&exr).unwrap();
    assert_eq!(active(&mut ed), (6, 4, px.clone()), "EXR");
    assert_eq!(ed.active_mut().unwrap().document.meta.bit_depth, 32);

    // Radiance HDR, flat RGBE: linear 0.5 red, then 4.0 white.
    let mut hdr = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 2\n".to_vec();
    hdr.extend_from_slice(&[128, 0, 0, 128, 128, 128, 128, 131]);
    let hdr = write(dir.path(), "sky.hdr", &hdr);
    ed.open_path(&hdr).unwrap();
    let (w, h, rgba) = active(&mut ed);
    assert_eq!((w, h), (2, 1));
    // sRGB-encoded 0.5 is 188 of 255.
    assert!(rgba[0].abs_diff(188) <= 1, "{rgba:?}");
    // On screen (an 8-bit composite) 4.0 shows as white; the document keeps
    // it (see `a_float_file_opens_at_32_bits_and_its_highlights_reach_exr`).
    assert_eq!(&rgba[4..8], &[255, 255, 255, 255], "shown as white");
    assert_eq!(ed.active_mut().unwrap().document.meta.bit_depth, 32);

    // Apple ICNS whose one entry is a PNG.
    let mut icns = b"icns".to_vec();
    icns.extend_from_slice(&((png.len() + 16) as u32).to_be_bytes());
    icns.extend_from_slice(b"ic07");
    icns.extend_from_slice(&((png.len() + 8) as u32).to_be_bytes());
    icns.extend_from_slice(&png);
    let icns = write(dir.path(), "app.icns", &icns);
    ed.open_path(&icns).unwrap();
    assert_eq!(active(&mut ed), (6, 4, px.clone()), "ICNS");

    // Deluxe Paint PBM (chunky IFF): two palette pixels.
    let mut bmhd = vec![0, 2, 0, 1, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 1, 1, 0, 2, 0, 1];
    let mut iff_body = b"BMHD\0\0\0\x14".to_vec();
    iff_body.append(&mut bmhd);
    iff_body.extend_from_slice(b"CMAP\0\0\0\x06\xff\0\0\0\0\xff");
    iff_body.extend_from_slice(b"BODY\0\0\0\x02\x01\x00");
    let mut iff = b"FORM".to_vec();
    iff.extend_from_slice(&((iff_body.len() + 4) as u32).to_be_bytes());
    iff.extend_from_slice(b"PBM ");
    iff.extend_from_slice(&iff_body);
    let iff = write(dir.path(), "pic.lbm", &iff);
    ed.open_path(&iff).unwrap();
    assert_eq!(
        active(&mut ed),
        (2, 1, vec![0, 0, 255, 255, 255, 0, 0, 255]),
        "IFF"
    );

    // Krita: the merged image.
    let kra = stored_zip(&[
        ("mimetype", b"application/x-krita"),
        ("maindoc.xml", b"<DOC/>"),
        ("mergedimage.png", &png),
    ]);
    let kra = write(dir.path(), "paint.kra", &kra);
    ed.open_path(&kra).unwrap();
    assert_eq!(active(&mut ed), (6, 4, px), "KRA");

    // The open filter offers every one of them.
    let offered = &crate::dialogs::open_file_filters()[0].1;
    for ext in ["exr", "hdr", "icns", "iff", "ilbm", "lbm", "kra"] {
        assert!(offered.contains(&ext), ".{ext} is not offered");
    }
}

/// The Export As dialog's confirmed job writes EXR and JPEG XL rows.
#[test]
fn export_as_writes_exr_and_jpeg_xl() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    let px = pixels();
    let src = write(
        dir.path(),
        "src.png",
        &raster::encode(raster::ExportFormat::Png, 6, 4, &px).unwrap(),
    );
    ed.open_path(&src).unwrap();
    let job = ui::dialogs::ExportJob {
        base_name: "shot".to_string(),
        entries: vec![
            ui::dialogs::ExportEntry::new("", raster::ExportFormat::Exr, 1.0),
            ui::dialogs::ExportEntry::new("", raster::ExportFormat::Jxl, 1.0),
        ],
    };
    ed.request_export(job, out.clone());
    ed.poll_exports();
    for (name, format) in [
        ("shot.exr", raster::ImportFormat::Exr),
        ("shot.jxl", raster::ImportFormat::Jxl),
    ] {
        let s = raster::decode_surface_path(&out.join(name), raster::ImportLimits::default())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(s.source_format, format, "{name}");
        assert_eq!(s.into_decoded_image().rgba8, px, "{name} is exact");
    }
}

/// File > Export to `x.psb` writes a version-2 file that reopens layered,
/// and the Save as PSD picker offers `.psb`.
#[test]
fn file_export_to_psb_writes_a_large_document_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("layers.psb");
    crate::dialogs::arm_psd_save();
    let request = crate::dialogs::ExportPickerRequest::next(Path::new("/work/photo.png"));
    assert!(request.leads_with_psd());
    assert!(
        request.filters.iter().any(|(_, e)| *e == ["psb"]),
        "Save as PSD offers .psb"
    );
    let mut ed = editor(
        dir.path(),
        ScriptedDialogs::new().exporting_to(target.clone()),
    );
    let px = pixels();
    let src = write(
        dir.path(),
        "src.png",
        &raster::encode(raster::ExportFormat::Png, 6, 4, &px).unwrap(),
    );
    ed.open_path(&src).unwrap();
    ed.dispatch(Action::Export)
        .unwrap_or_else(|e| panic!("File > Export to .psb: {e}"));
    ed.poll_exports();
    let bytes = std::fs::read(&target).expect("the .psb was written");
    assert!(psd::is_psb(&bytes), "version 2 on disk");
    let file = psd::read(&bytes).unwrap();
    assert!(!file.layers.is_empty(), "layered");
    ed.open_path(&target).unwrap();
    assert_eq!(active(&mut ed), (6, 4, px));
}

/// Layered bytes for a canvas past 30 000 px are a `.psb` (`psd::write`
/// has no other encoding for them): the one write every layered save goes
/// through refuses them under a `.psd` name, naming `.psb`, and writes them
/// under a `.psb` one. (The app's own route for such a canvas is
/// `save_as_psd_past_30000_px_offers_and_writes_a_psb`.)
#[test]
fn psb_bytes_are_refused_under_a_psd_name_and_written_under_a_psb_one() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (30_001u32, 2u32);
    let rgba = vec![90u8; (w * h * 4) as usize];
    let bytes = psd::write(&psd::from_rgba8(w, h, &rgba).unwrap()).unwrap();
    assert!(psd::is_psb(&bytes));
    let as_psd = dir.path().join("wide.psd");
    let err = crate::doc::write_atomically(&as_psd, &bytes)
        .unwrap_err()
        .to_string();
    assert!(err.contains(".psb"), "{err}");
    assert!(!as_psd.exists(), "nothing half-written under the .psd name");
    let as_psb = dir.path().join("wide.psb");
    crate::doc::write_atomically(&as_psb, &bytes).unwrap();
    assert_eq!(std::fs::read(&as_psb).unwrap(), bytes);
    // A small document saved as .psd stays a version-1 file.
    let doc = OpenDocument::blank(DocumentId(11_801), 4, 3, "small", 8);
    let mut doc = doc.unwrap();
    let small = dir.path().join("small.psd");
    doc.export_psd_to(&small).unwrap();
    assert!(!psd::is_psb(&std::fs::read(&small).unwrap()));
}

/// The layer's `f32` samples of the active document (straight RGBA in the
/// document's encoding).
fn active_f32(ed: &Editor) -> Vec<f32> {
    let doc = ed.active().expect("a document opened");
    let layer = doc.document.active_layer().expect("an active layer");
    crate::depth32::layer_rgbaf32(doc, layer)
}

/// Straight linear RGBA of 2x1 pixels: a 4.0 red highlight at full alpha,
/// then a half-transparent grey.
const FLOAT_PIXELS: [f32; 8] = [4.0, 0.5, 0.25, 1.0, 0.2, 0.2, 0.2, 0.5];

fn assert_highlights_kept(linear: &[f32], what: &str) {
    for (i, (want, got)) in FLOAT_PIXELS.iter().zip(linear).enumerate() {
        assert!(
            (want - got).abs() < 1e-3,
            "{what}: sample {i} wrote {want}, read {got}"
        );
    }
}

/// W11-H: an OpenEXR (and a Radiance HDR) opens as a 32 Bits/Channel
/// document holding the file's float samples, unclipped, by both roads that
/// File > Open takes (the dialog's import job, and `open_path`); the open is
/// not an undo step. File > Export to `.exr` and an Export As EXR row then write
/// that float composite, the 4.0 highlight included.
#[test]
fn a_float_file_opens_at_32_bits_and_its_highlights_reach_exr() {
    let dir = tempfile::tempdir().unwrap();
    let exr_bytes = raster::codec::formats::float::encode_exr_linear(2, 1, &FLOAT_PIXELS).unwrap();
    let exr = write(dir.path(), "shot.exr", &exr_bytes);
    let by_file = dir.path().join("file.exr");

    // File > Open: the picker answers, the import job decodes off-thread.
    let mut ed = editor(
        dir.path(),
        ScriptedDialogs::new()
            .opening(&exr)
            .exporting_to(by_file.clone()),
    );
    ed.dispatch(Action::Open).unwrap();
    ed.poll_imports();
    {
        let doc = ed.active().expect("File > Open opened the EXR");
        assert_eq!(doc.document.meta.bit_depth, 32, "a 32-bit document");
        assert!(!doc.history.can_undo(), "opening is not an undo step");
    }
    let samples = active_f32(&ed);
    let red = color::linear_to_srgb(4.0);
    assert!(red > 1.5, "{red}");
    assert!(
        (samples[0] - red).abs() < 1e-4,
        "kept above 1.0: {samples:?}"
    );
    assert!(
        (samples[7] - 0.5).abs() < 1e-6,
        "straight alpha: {samples:?}"
    );

    // File > Export to `.exr`: the float composite, 4.0 included.
    ed.dispatch(Action::Export)
        .unwrap_or_else(|e| panic!("File > Export to .exr: {e}"));
    ed.poll_exports();
    let (w, h, linear) = raster::codec::formats::float::decode_linear(
        raster::ImportFormat::Exr,
        &std::fs::read(&by_file).expect("the .exr was written"),
        raster::ImportLimits::default(),
        0,
    )
    .unwrap();
    assert_eq!((w, h), (2, 1));
    assert_highlights_kept(&linear, "File > Export");

    // Export As, an EXR row at 100%: the same float file.
    let out = dir.path().join("out");
    let job = ui::dialogs::ExportJob {
        base_name: "row".to_string(),
        entries: vec![ui::dialogs::ExportEntry::new(
            "",
            raster::ExportFormat::Exr,
            1.0,
        )],
    };
    ed.request_export(job, out.clone());
    ed.poll_exports();
    let (_, _, linear) = raster::codec::formats::float::decode_linear(
        raster::ImportFormat::Exr,
        &std::fs::read(out.join("row.exr")).expect("the Export As row was written"),
        raster::ImportLimits::default(),
        0,
    )
    .unwrap();
    assert_highlights_kept(&linear, "Export As");

    // `open_path` (drag-and-drop, recent files) takes the same road, and a
    // Radiance file too: 4.0 linear red survives in the f32 tile.
    ed.open_path(&exr).unwrap();
    assert_eq!(ed.active().unwrap().document.meta.bit_depth, 32);
    assert!((active_f32(&ed)[0] - red).abs() < 1e-4);
    // Flat RGBE: mantissa 128 at exponent 131 is 0.5 * 2^3 = 4.0.
    let mut hdr = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 1\n".to_vec();
    hdr.extend_from_slice(&[128, 0, 0, 131]);
    let hdr = write(dir.path(), "sky.hdr", &hdr);
    ed.open_path(&hdr).unwrap();
    assert_eq!(ed.active().unwrap().document.meta.bit_depth, 32);
    let px = active_f32(&ed);
    assert!((px[0] - red).abs() < 0.05, "RGBE's 8-bit mantissa: {px:?}");

    // An 8-bit file still opens at 8 bits.
    let png = write(
        dir.path(),
        "plain.png",
        &raster::encode(raster::ExportFormat::Png, 6, 4, &pixels()).unwrap(),
    );
    ed.open_path(&png).unwrap();
    assert_eq!(ed.active().unwrap().document.meta.bit_depth, 8);
}

/// W11-H: File > Export writes `.exr` and `.jxl` by name, and its picker
/// offers both.
#[test]
fn file_export_writes_exr_and_jpeg_xl_by_name() {
    let request = crate::dialogs::ExportPickerRequest::next(Path::new("/work/photo.png"));
    for ext in ["exr", "jxl"] {
        assert!(
            request.filters.iter().any(|(_, e)| *e == [ext]),
            "File > Export offers .{ext}"
        );
        assert!(crate::doc::export_format_for(Path::new(&format!("a.{ext}"))).is_some());
    }
    let dir = tempfile::tempdir().unwrap();
    let px = pixels();
    let src = write(
        dir.path(),
        "src.png",
        &raster::encode(raster::ExportFormat::Png, 6, 4, &px).unwrap(),
    );
    for (name, format) in [
        ("shot.exr", raster::ImportFormat::Exr),
        ("shot.jxl", raster::ImportFormat::Jxl),
    ] {
        let target = dir.path().join(name);
        let mut ed = editor(
            dir.path(),
            ScriptedDialogs::new().exporting_to(target.clone()),
        );
        ed.open_path(&src).unwrap();
        ed.dispatch(Action::Export)
            .unwrap_or_else(|e| panic!("File > Export to {name}: {e}"));
        ed.poll_exports();
        let s = raster::decode_surface_path(&target, raster::ImportLimits::default())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(s.source_format, format, "{name}");
        assert_eq!(s.into_decoded_image().rgba8, px, "{name} is exact");
    }
}

/// A picker that answers like [`ScriptedDialogs`] and keeps every export
/// request it was shown where the test can still read it.
struct SeeingDialogs {
    inner: ScriptedDialogs,
    seen: std::rc::Rc<std::cell::RefCell<Vec<crate::dialogs::ExportPickerRequest>>>,
}

impl crate::dialogs::FileDialogs for SeeingDialogs {
    fn pick_open_file(&mut self) -> Option<PathBuf> {
        self.inner.pick_open_file()
    }
    fn pick_place_file(&mut self) -> Option<PathBuf> {
        self.inner.pick_place_file()
    }
    fn pick_replace_file(&mut self) -> Option<PathBuf> {
        self.inner.pick_replace_file()
    }
    fn pick_open_project(&mut self) -> Option<PathBuf> {
        self.inner.pick_open_project()
    }
    fn pick_save_path(&mut self, suggested: &Path) -> Option<PathBuf> {
        self.inner.pick_save_path(suggested)
    }
    fn pick_export_path(&mut self, suggested: &Path) -> Option<PathBuf> {
        let answer = self.inner.pick_export_path(suggested);
        let request = self.inner.export_requests.last().cloned();
        self.seen.borrow_mut().extend(request);
        answer
    }
    fn pick_export_folder(&mut self) -> Option<PathBuf> {
        self.inner.pick_export_folder()
    }
    fn confirm_close(&mut self, document: &str) -> crate::dialogs::CloseChoice {
        self.inner.confirm_close(document)
    }
    fn confirm_recover(&mut self, document: &str) -> bool {
        self.inner.confirm_recover(document)
    }
    fn report_error(&mut self, title: &str, message: &str) {
        self.inner.report_error(title, message)
    }
    fn report_notice(&mut self, title: &str, message: &str) {
        self.inner.report_notice(title, message)
    }
}

/// W11-H: File > Save as PSD on a canvas past 30 000 px offers the large
/// document format first and writes a version-2 `.psb` that reopens at its
/// full size; the same canvas under a `.psd` name is refused, naming `.psb`.
#[test]
fn save_as_psd_past_30000_px_offers_and_writes_a_psb() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (30_001u32, 2u32);
    let rgba: Vec<u8> = (0..w * h)
        .flat_map(|i| [(i % 251) as u8, 90, (i % 7) as u8 * 30, 255])
        .collect();
    let src = write(
        dir.path(),
        "wide.png",
        &raster::encode(raster::ExportFormat::Png, w, h, &rgba).unwrap(),
    );
    let target = dir.path().join("wide.psb");
    let psd_path = dir.path().join("wide.psd");
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.path()),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(SeeingDialogs {
            inner: ScriptedDialogs::new()
                .exporting_to(target.clone())
                .exporting_to(psd_path.clone()),
            seen: seen.clone(),
        }),
    );
    ed.open_path(&src).unwrap();
    crate::layer_ops::save_as_psd(&mut ed).unwrap_or_else(|e| panic!("{e}"));
    ed.poll_exports();
    {
        let seen = seen.borrow();
        let request = seen.first().expect("the picker was asked");
        assert_eq!(request.filters[0].1, ["psb"], "{:?}", request.filters);
        assert_eq!(request.suggested_extension().as_deref(), Some("psb"));
        assert_eq!(request.title, "Save as PSB");
    }
    let bytes = std::fs::read(&target).expect("the .psb was written");
    assert!(psd::is_psb(&bytes), "version 2 on disk");
    ed.open_path(&target).unwrap();
    let (rw, rh, back) = active(&mut ed);
    assert_eq!((rw, rh), (w, h));
    assert_eq!(back, rgba, "the pixels survive the .psb");

    // The same canvas under a `.psd` name: refused, nothing written.
    ed.open_path(&src).unwrap();
    let outcome = crate::layer_ops::save_as_psd(&mut ed);
    ed.poll_exports();
    assert!(
        !psd_path.exists(),
        "a 30001 px canvas was written as a .psd"
    );
    let said = format!("{outcome:?} {:?}", ed.status());
    assert!(said.contains(".psb"), "the refusal names .psb: {said}");

    // A canvas inside the limit is still offered `.psd` first.
    crate::dialogs::arm_psd_save_for((30_000, 2));
    let small = crate::dialogs::ExportPickerRequest::next(Path::new("/w/a.png"));
    assert!(small.leads_with_psd());
    assert_eq!(small.suggested_extension().as_deref(), Some("psd"));
}
