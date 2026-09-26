//! W16-L: File > Open of the new formats and resources, driven the way the
//! user drives it: `Action::Open` with the picker answered (and, for the
//! import worker's formats, the job polled to completion), read back from
//! the document, its layers and the compositor's font list.

use std::path::{Path, PathBuf};

use layer_model::{AdjustmentKind, LayerKind};

use crate::action::Action;
use crate::dialogs::ScriptedDialogs;
use crate::editor::{Editor, Effect};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

fn editor(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    );
    ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
    ed
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn png(dir: &Path) -> PathBuf {
    let rgba = vec![200u8; 8 * 8 * 4];
    write(
        dir,
        "doc.png",
        &raster::encode(raster::ExportFormat::Png, 8, 8, &rgba).unwrap(),
    )
}

fn active_adjustment(ed: &Editor) -> AdjustmentKind {
    let doc = ed.active().unwrap();
    let id = doc.document.active_layer().unwrap();
    match &doc.document.layers.get(id).unwrap().kind {
        LayerKind::Adjustment(a) => a.kind.clone(),
        other => panic!("not an adjustment layer: {other:?}"),
    }
}

fn wait_for_imports(ed: &mut Editor) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while ed.imports_pending() && std::time::Instant::now() < deadline {
        ed.poll_imports();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// An `.acv` with a composite curve and a red curve.
fn acv() -> Vec<u8> {
    let mut b = Vec::new();
    for v in [4u16, 2] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    // Composite: (in 0, out 0), (in 128, out 200), (in 255, out 255);
    // stored output first.
    b.extend_from_slice(&3u16.to_be_bytes());
    for (out, inp) in [(0u16, 0u16), (200, 128), (255, 255)] {
        b.extend_from_slice(&out.to_be_bytes());
        b.extend_from_slice(&inp.to_be_bytes());
    }
    // Red: inverted.
    b.extend_from_slice(&2u16.to_be_bytes());
    for (out, inp) in [(255u16, 0u16), (0, 255)] {
        b.extend_from_slice(&out.to_be_bytes());
        b.extend_from_slice(&inp.to_be_bytes());
    }
    b
}

#[test]
fn file_open_of_an_acv_adds_a_curves_layer_with_its_curves() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "Contrast.acv", &acv());
    assert!(Editor::is_library_file(&file));
    // No document: refused by name, nothing opens.
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(file.clone()));
    let err = ed.dispatch(Action::Open).unwrap_err().to_string();
    assert!(err.contains("open a document"), "{err}");

    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(file));
    ed.open_path(&png(dir.path())).unwrap();
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::DocumentEdited));
    match active_adjustment(&ed) {
        AdjustmentKind::CurvesFull {
            composite,
            red,
            green,
            blue,
        } => {
            assert_eq!(composite.len(), 3);
            assert!((composite[1][0] - 128.0 / 255.0).abs() < 1e-6);
            assert!((composite[1][1] - 200.0 / 255.0).abs() < 1e-6);
            assert_eq!(red, vec![[0.0, 1.0], [1.0, 0.0]]);
            assert_eq!(green, vec![[0.0, 0.0], [1.0, 1.0]]);
            assert_eq!(blue, vec![[0.0, 0.0], [1.0, 1.0]]);
        }
        other => panic!("not a Curves layer: {other:?}"),
    }
    assert_eq!(
        ed.status.as_deref(),
        Some("Added a Curves layer from Contrast.acv")
    );
}

/// A 2-point `.3dl` (blue fastest) and the same LUT as a `.look` (red
/// fastest) both become the inverting Color Lookup table, red fastest.
#[test]
fn file_open_of_a_3dl_or_look_adds_a_color_lookup_layer() {
    let dir = tempfile::tempdir().unwrap();
    let mut dl = String::from("# inverted\n0 1023\n");
    for r in 0..2 {
        for g in 0..2 {
            for b in 0..2 {
                dl.push_str(&format!(
                    "{} {} {}\n",
                    (1 - r) * 1023,
                    (1 - g) * 1023,
                    (1 - b) * 1023
                ));
            }
        }
    }
    let mut hex = String::new();
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                for v in [1 - r, 1 - g, 1 - b] {
                    for byte in (v as f32).to_le_bytes() {
                        hex.push_str(&format!("{byte:02x}"));
                    }
                }
            }
        }
    }
    let look = format!(
        "<?xml version=\"1.0\"?>\n<look>\n <LUT>\n  <size>\"2\"</size>\n  <data>\"{hex}\"</data>\n </LUT>\n</look>\n"
    );
    let want: Vec<[f32; 3]> = (0..8)
        .map(|i| {
            let (r, g, b) = (i & 1, (i >> 1) & 1, (i >> 2) & 1);
            [(1 - r) as f32, (1 - g) as f32, (1 - b) as f32]
        })
        .collect();
    for (name, bytes) in [
        ("Invert.3dl", dl.into_bytes()),
        ("Invert.look", look.into_bytes()),
    ] {
        let file = write(dir.path(), name, &bytes);
        let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(file));
        ed.open_path(&png(dir.path())).unwrap();
        assert_eq!(
            ed.dispatch(Action::Open),
            Ok(Effect::DocumentEdited),
            "{name}"
        );
        match active_adjustment(&ed) {
            AdjustmentKind::ColorLookup {
                name: n,
                size,
                table,
            } => {
                assert_eq!((n.as_str(), size), ("Invert", 2), "{name}");
                assert_eq!(table, want, "{name}");
            }
            other => panic!("{name}: not a Color Lookup layer: {other:?}"),
        }
    }
    // A damaged table is refused by name and adds nothing.
    let bad = write(dir.path(), "Bad.3dl", b"0 1023\n1 2 3\n");
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(bad));
    ed.open_path(&png(dir.path())).unwrap();
    let layers = ed.active().unwrap().document.layers.len();
    let err = ed.dispatch(Action::Open).unwrap_err().to_string();
    assert!(err.contains("needs 8 entries"), "{err}");
    assert_eq!(ed.active().unwrap().document.layers.len(), layers);
}

/// A WOFF 1 file with its tables stored uncompressed.
fn woff1(font: &[u8]) -> Vec<u8> {
    let be16 = |at: usize| usize::from(u16::from_be_bytes([font[at], font[at + 1]]));
    let be32 = |at: usize| u32::from_be_bytes([font[at], font[at + 1], font[at + 2], font[at + 3]]);
    let n = be16(4);
    let mut out = vec![0u8; 44 + n * 20];
    out[..4].copy_from_slice(b"wOFF");
    out[4..8].copy_from_slice(&font[..4]);
    out[12..14].copy_from_slice(&(n as u16).to_be_bytes());
    out[16..20].copy_from_slice(&(font.len() as u32).to_be_bytes());
    for i in 0..n {
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        let dir = 12 + i * 16;
        let (off, len) = (be32(dir + 8) as usize, be32(dir + 12));
        let at = 44 + i * 20;
        let data_at = out.len() as u32;
        out[at..at + 4].copy_from_slice(&font[dir..dir + 4]);
        out[at + 4..at + 8].copy_from_slice(&data_at.to_be_bytes());
        out[at + 8..at + 12].copy_from_slice(&len.to_be_bytes());
        out[at + 12..at + 16].copy_from_slice(&len.to_be_bytes());
        out[at + 16..at + 20].copy_from_slice(&font[dir + 4..dir + 8]);
        out.extend_from_slice(&font[off..off + len as usize]);
    }
    let total = out.len() as u32;
    out[8..12].copy_from_slice(&total.to_be_bytes());
    out
}

#[test]
fn file_open_of_a_woff_font_loads_its_family_for_text() {
    let dir = tempfile::tempdir().unwrap();
    let font = dejavu::sans_mono::bold();
    let file = write(dir.path(), "Mono.woff", &woff1(font));
    assert!(crate::dialogs::is_font_path(&file));
    assert!(crate::dialogs::open_file_filters()[0].1.contains(&"woff2"));
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(file));
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::Tool));
    let status = ed.status.clone().unwrap_or_default();
    assert!(
        status.starts_with("Loaded font DejaVu Sans Mono"),
        "{status}"
    );
    assert!(compositor::font_families()
        .iter()
        .any(|f| f == "DejaVu Sans Mono"));
    // A damaged web font is refused, not loaded.
    let broken = write(
        dir.path(),
        "Broken.woff2",
        b"wOF2\0\0\0\0garbage-garbage-garbage",
    );
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(broken));
    assert!(ed.dispatch(Action::Open).is_err());
}

fn fits_file() -> Vec<u8> {
    let card = |t: &str| {
        let mut c = t.as_bytes().to_vec();
        c.resize(80, b' ');
        c
    };
    let mut b = card("SIMPLE  =                    T");
    for t in [
        "BITPIX  =                    8",
        "NAXIS   =                    2",
        "NAXIS1  =                    3",
        "NAXIS2  =                    2",
        "END",
    ] {
        b.extend(card(t));
    }
    b.resize(2880, b' ');
    // Bottom row first: the bottom row is dark, the top row bright.
    b.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
    b.resize(5760, 0);
    b
}

/// File > Open's import job opens the W16 formats as documents: a FITS at
/// its size (top row bright: the flip), a DXF drawn at its fitted size.
#[test]
fn file_open_of_fits_and_dxf_opens_documents() {
    let dir = tempfile::tempdir().unwrap();
    let fits = write(dir.path(), "m31.fits", &fits_file());
    let dxf = write(
        dir.path(),
        "plan.dxf",
        b"0\nSECTION\n2\nENTITIES\n0\nLINE\n8\n0\n10\n0\n20\n0\n11\n100\n21\n50\n0\nENDSEC\n0\nEOF\n",
    );
    for ext in [
        "fits", "dxf", "jp2", "vtf", "dcm", "clip", "pxd", "cdr", "indd",
    ] {
        assert!(
            crate::dialogs::open_file_filters()[2].1.contains(&ext),
            ".{ext} is not in the Images filter"
        );
    }
    let mut ed = editor(
        dir.path(),
        ScriptedDialogs::new().opening(fits).opening(dxf),
    );
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::DocumentSet));
    wait_for_imports(&mut ed);
    {
        let doc = ed.active_mut().expect("the FITS opened");
        assert_eq!((doc.document.width(), doc.document.height()), (3, 2));
        let rgba = doc.composite(doc.canvas_rect()).unwrap();
        assert_eq!(
            &rgba[..4],
            &[255, 255, 255, 255],
            "top row is the file's last"
        );
        assert_eq!(&rgba[12..16], &[0, 0, 0, 255]);
    }
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::DocumentSet));
    wait_for_imports(&mut ed);
    let doc = ed.active_mut().expect("the DXF opened");
    assert_eq!((doc.document.width(), doc.document.height()), (1056, 544));
}

// ------------------------------------------------ KRA and DXF as layers

/// A stored (uncompressed) ZIP of `entries`; CRCs are zero (the Krita
/// reader does not check them).
fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut dir = Vec::new();
    for (name, data) in entries {
        let local = out.len() as u32;
        let size = (data.len() as u32).to_le_bytes();
        let name_len = (name.len() as u16).to_le_bytes();
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&size);
        out.extend_from_slice(&size);
        out.extend_from_slice(&name_len);
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        dir.extend_from_slice(b"PK\x01\x02");
        dir.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0]);
        dir.extend_from_slice(&[0; 8]);
        dir.extend_from_slice(&size);
        dir.extend_from_slice(&size);
        dir.extend_from_slice(&name_len);
        dir.extend_from_slice(&[0; 12]);
        dir.extend_from_slice(&local.to_le_bytes());
        dir.extend_from_slice(name.as_bytes());
    }
    let dir_at = out.len() as u32;
    out.extend_from_slice(&dir);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
    out.extend_from_slice(&dir_at.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

/// A Krita layer file holding one uncompressed 64x64 tile of `rgba` at
/// tile origin (`x`, `y`): Krita stores a tile's pixels as BGRA byte planes.
fn kra_layer_file(x: i64, y: i64, rgba: [u8; 4]) -> Vec<u8> {
    let n = 64 * 64;
    let mut tile = vec![0u8];
    for v in [rgba[2], rgba[1], rgba[0], rgba[3]] {
        tile.extend(std::iter::repeat_n(v, n));
    }
    let mut out = format!(
        "VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 4\nDATA 1\n{x},{y},LZF,{}\n",
        tile.len()
    )
    .into_bytes();
    out.extend(tile);
    out
}

const KRA_MAINDOC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0">
 <IMAGE name="Pic" width="64" height="64" colorspacename="RGBA" mime="application/x-kra">
  <layers>
   <layer name="Top" opacity="128" visible="1" compositeop="multiply" x="0" y="0"
          nodetype="paintlayer" filename="layer4" colorspacename="RGBA"/>
   <layer name="Shade" opacity="255" visible="0" compositeop="normal" x="0" y="0"
          nodetype="grouplayer" filename="layer3">
    <layers>
     <layer name="Inner" opacity="255" visible="1" compositeop="screen" x="0" y="0"
            nodetype="paintlayer" filename="layer2" colorspacename="RGBA"/>
    </layers>
   </layer>
   <layer name="Blur" nodetype="adjustmentlayer" filename="layer5"/>
   <layer name="Paper" opacity="255" visible="1" compositeop="normal" x="0" y="0"
          nodetype="paintlayer" filename="layer1" colorspacename="RGBA"/>
  </layers>
 </IMAGE>
</DOC>"#;

/// A 64x64 `.kra`: "Top" (red, Multiply, 50%), a hidden group "Shade"
/// holding "Inner" (blue, Screen), "Paper" (white, bottom), a filter layer
/// the reader leaves out, and a green `mergedimage.png` that must NOT be
/// what opens.
fn kra_file() -> Vec<u8> {
    let merged = raster::encode(
        raster::ExportFormat::Png,
        64,
        64,
        &[0, 255, 0, 255].repeat(64 * 64),
    )
    .unwrap();
    stored_zip(&[
        ("mimetype", b"application/x-krita"),
        ("maindoc.xml", KRA_MAINDOC.as_bytes()),
        ("Pic/layers/layer4", &kra_layer_file(0, 0, [255, 0, 0, 255])),
        ("Pic/layers/layer2", &kra_layer_file(0, 0, [0, 0, 255, 255])),
        (
            "Pic/layers/layer1",
            &kra_layer_file(0, 0, [255, 255, 255, 255]),
        ),
        ("mergedimage.png", &merged),
    ])
}

/// Every layer of the active document as `name:kind`, depth first.
fn layer_tree(ed: &Editor) -> Vec<String> {
    let doc = ed.active().expect("a document opened");
    doc.document
        .layers
        .iter_depth_first()
        .into_iter()
        .map(|id| {
            let layer = doc.document.layers.get(id).unwrap();
            let kind = match &layer.kind {
                LayerKind::Shape(_) => "shape",
                LayerKind::Text(_) => "text",
                LayerKind::Group(_) => "group",
                LayerKind::Raster(_) => "raster",
                _ => "other",
            };
            format!("{}:{kind}", layer.name)
        })
        .collect()
}

fn layer_named(ed: &Editor, name: &str) -> layer_model::Layer {
    let doc = ed.active().unwrap();
    let id = doc
        .document
        .layers
        .iter_depth_first()
        .into_iter()
        .find(|id| doc.document.layers.get(*id).unwrap().name == name)
        .unwrap_or_else(|| panic!("no layer {name:?}"));
    doc.document.layers.get(id).unwrap().clone()
}

/// File > Open of a `.kra` opens Krita's layer tree (names, opacity,
/// visibility, blend modes, the group), not its merged image; the filter
/// layer the reader leaves out is in the import report.
#[test]
fn file_open_of_a_kra_opens_its_layers_not_the_merged_image() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "pic.kra", &kra_file());
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(path));
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::DocumentSet));
    wait_for_imports(&mut ed);
    let mut tree = layer_tree(&ed);
    tree.sort();
    assert_eq!(
        tree,
        vec!["Inner:raster", "Paper:raster", "Shade:group", "Top:raster"]
    );
    let top = layer_named(&ed, "Top");
    assert_eq!(top.blend_mode, layer_model::BlendMode::Multiply);
    assert!((top.opacity - 128.0 / 255.0).abs() < 1e-6 && top.visible);
    assert!(!layer_named(&ed, "Shade").visible);
    assert_eq!(
        layer_named(&ed, "Inner").blend_mode,
        layer_model::BlendMode::Screen
    );
    // Stacking: Top, then the group, then Paper (one direction or the other).
    {
        let doc = ed.active().unwrap();
        let order: Vec<String> = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .map(|id| doc.document.layers.get(id).unwrap().name.clone())
            .collect();
        let at = |n: &str| order.iter().position(|o| o == n).unwrap();
        let (top, shade, paper) = (at("Top"), at("Shade"), at("Paper"));
        assert!(
            (top < shade && shade < paper) || (top > shade && shade > paper),
            "{order:?}"
        );
    }
    let status = ed.status.clone().unwrap_or_default();
    assert!(status.contains("3 layers in 1 group"), "{status}");
    assert!(status.contains("import report"), "{status}");
    // The composite is red multiplied at 50% over white, not the green
    // merged image.
    let doc = ed.active_mut().unwrap();
    assert_eq!((doc.document.width(), doc.document.height()), (64, 64));
    let rgba = doc.composite(doc.canvas_rect()).unwrap();
    let px = [rgba[0], rgba[1], rgba[2], rgba[3]];
    assert!(
        px[0] > 250 && px[1] > 90 && px[1] < 200 && px[3] == 255,
        "{px:?}"
    );
}

/// A drop of a `.kra` onto a window with a document open opens its layers
/// in a new tab (it is not placed as a picture).
#[test]
fn a_dropped_kra_opens_its_layers_in_a_new_document() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "pic.kra", &kra_file());
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.open_path(&png(dir.path())).unwrap();
    let mut shell = crate::shell::Shell::new(ed, Vec::new());
    shell.on_dropped_files(&[path]);
    let ed = shell.editor();
    assert_eq!(ed.documents().len(), 2);
    assert_eq!(layer_tree(ed).len(), 4);
}

/// A `.kra` whose layers cannot be read (maindoc.xml gives no canvas size)
/// opens its merged image and says why.
#[test]
fn a_kra_whose_layers_cannot_be_read_opens_its_merged_image_saying_why() {
    let dir = tempfile::tempdir().unwrap();
    let maindoc = br#"<DOC><IMAGE name="Pic" width="0" height="0"><layers/></IMAGE></DOC>"#;
    let merged = raster::encode(
        raster::ExportFormat::Png,
        4,
        4,
        &[0, 255, 0, 255].repeat(16),
    )
    .unwrap();
    let file = stored_zip(&[
        ("mimetype", b"application/x-krita"),
        ("maindoc.xml", maindoc),
        ("mergedimage.png", &merged),
    ]);
    let path = write(dir.path(), "flat.kra", &file);
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(path));
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::DocumentSet));
    let status = ed.status.clone().unwrap_or_default();
    assert!(status.contains("its layers could not be read"), "{status}");
    let doc = ed.active_mut().unwrap();
    assert_eq!((doc.document.width(), doc.document.height()), (4, 4));
    let rgba = doc.composite(doc.canvas_rect()).unwrap();
    assert_eq!(&rgba[..4], &[0, 255, 0, 255]);
}

const PLAN_DXF: &str = "0\nSECTION\n2\nENTITIES\n0\nLINE\n8\nWalls\n10\n0\n20\n0\n11\n100\n21\n0\n0\nCIRCLE\n8\nWalls\n10\n50\n20\n25\n40\n10\n0\nLINE\n8\nDoors\n10\n0\n20\n50\n11\n100\n21\n50\n0\nENDSEC\n0\nEOF\n";

/// File > Open of a DXF opens one group per DXF layer holding a shape layer
/// per entity, over the white page.
#[test]
fn file_open_of_a_dxf_opens_its_layers_as_vector_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "plan.dxf", PLAN_DXF.as_bytes());
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(path));
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::DocumentSet));
    wait_for_imports(&mut ed);
    let tree = layer_tree(&ed);
    assert!(tree.contains(&"Walls:group".to_string()), "{tree:?}");
    assert!(tree.contains(&"Doors:group".to_string()), "{tree:?}");
    assert!(tree.contains(&"Background:shape".to_string()), "{tree:?}");
    let shapes = tree.iter().filter(|t| t.ends_with(":shape")).count();
    assert_eq!(shapes, 4, "three entities and the page: {tree:?}");
    let doc = ed.active().unwrap();
    assert_eq!((doc.document.width(), doc.document.height()), (1056, 544));
}
