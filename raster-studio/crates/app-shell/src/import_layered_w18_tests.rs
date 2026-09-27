//! W18-H: File > Revert of a `.kra` / `.dxf` reads its layers back, and a
//! TIFF with Photoshop layers opens (and reverts) as those layers — driven
//! the way the user drives it: `Action::Open` with the picker answered,
//! then `Editor::revert_active` (File > Revert).

use std::path::{Path, PathBuf};

use layer_model::LayerKind;

use crate::action::Action;
use crate::dialogs::ScriptedDialogs;
use crate::editor::{Editor, Effect};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

#[path = "../../raster/src/formats/tiff_layers_fixture.rs"]
mod tiff_fixture;

fn editor(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    )
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn wait_for_imports(ed: &mut Editor) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while ed.imports_pending() && std::time::Instant::now() < deadline {
        ed.poll_imports();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn open(dir: &Path, path: PathBuf) -> Editor {
    let mut ed = editor(dir, ScriptedDialogs::new().opening(path));
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::DocumentSet));
    wait_for_imports(&mut ed);
    ed
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
                LayerKind::Group(_) => "group",
                LayerKind::Raster(_) => "raster",
                _ => "other",
            };
            format!("{}:{kind}", layer.name)
        })
        .collect()
}

/// Delete the top layer, then File > Revert.
fn delete_top_and_revert(ed: &mut Editor) -> Result<String, String> {
    let top = ed.active().unwrap().document.layers.root()[0];
    ed.apply_command(editor_core::Command::DeleteLayer { layer_id: top });
    ed.revert_active()
}

// ------------------------------------------------------------- fixtures

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

fn kra_layer_file(rgba: [u8; 4]) -> Vec<u8> {
    let n = 64 * 64;
    let mut tile = vec![0u8];
    for v in [rgba[2], rgba[1], rgba[0], rgba[3]] {
        tile.extend(std::iter::repeat_n(v, n));
    }
    let mut out = format!(
        "VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 4\nDATA 1\n0,0,LZF,{}\n",
        tile.len()
    )
    .into_bytes();
    out.extend(tile);
    out
}

/// A 64x64 `.kra`: "Top" (red) in a group "Set", over "Paper" (white);
/// with `size = false` its maindoc gives no canvas size (its layers cannot
/// be read, only its merged image).
fn kra_file(size: bool) -> Vec<u8> {
    let dims = if size {
        r#"width="64" height="64""#
    } else {
        ""
    };
    let maindoc = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0">
 <IMAGE name="Pic" {dims} colorspacename="RGBA" mime="application/x-kra">
  <layers>
   <layer name="Set" opacity="255" visible="1" compositeop="normal" x="0" y="0"
          nodetype="grouplayer" filename="layer3">
    <layers>
     <layer name="Top" opacity="255" visible="1" compositeop="normal" x="0" y="0"
            nodetype="paintlayer" filename="layer2" colorspacename="RGBA"/>
    </layers>
   </layer>
   <layer name="Paper" opacity="255" visible="1" compositeop="normal" x="0" y="0"
          nodetype="paintlayer" filename="layer1" colorspacename="RGBA"/>
  </layers>
 </IMAGE>
</DOC>"#
    );
    let merged = raster::encode(
        raster::ExportFormat::Png,
        64,
        64,
        &[0, 255, 0, 255].repeat(64 * 64),
    )
    .unwrap();
    stored_zip(&[
        ("mimetype", b"application/x-krita"),
        ("maindoc.xml", maindoc.as_bytes()),
        ("Pic/layers/layer2", &kra_layer_file([255, 0, 0, 255])),
        ("Pic/layers/layer1", &kra_layer_file([255, 255, 255, 255])),
        ("mergedimage.png", &merged),
    ])
}

const PLAN_DXF: &str = "0\nSECTION\n2\nENTITIES\n0\nLINE\n8\nWalls\n10\n0\n20\n0\n11\n100\n21\n0\n0\nCIRCLE\n8\nWalls\n10\n50\n20\n25\n40\n10\n0\nLINE\n8\nDoors\n10\n0\n20\n50\n11\n100\n21\n50\n0\nENDSEC\n0\nEOF\n";

// ----------------------------------------------------------------- tests

/// File > Revert of a `.kra` and of a `.dxf` brings their layers back (the
/// groups, the shapes), not the flat image the import job decodes.
#[test]
fn revert_of_a_kra_or_dxf_reads_its_layers_back_not_a_flat_image() {
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in [
        ("pic.kra", kra_file(true)),
        ("plan.dxf", PLAN_DXF.as_bytes().to_vec()),
    ] {
        let path = write(dir.path(), name, &bytes);
        let mut ed = open(dir.path(), path);
        let opened = layer_tree(&ed);
        assert!(
            opened.iter().any(|l| l.ends_with(":group")),
            "{name} opened as layers: {opened:?}"
        );
        let message = delete_top_and_revert(&mut ed).unwrap();
        assert!(message.starts_with("Reverted to"), "{message}");
        assert_eq!(layer_tree(&ed), opened, "{name}: the same layers came back");
        assert!(!ed.active().unwrap().is_dirty());
    }
}

/// A `.kra` that opened as layers and whose file no longer yields them is
/// not flattened by File > Revert: Revert refuses and says why.
#[test]
fn revert_of_a_kra_whose_layers_can_no_longer_be_read_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "pic.kra", &kra_file(true));
    let mut ed = open(dir.path(), path.clone());
    let opened = layer_tree(&ed);
    std::fs::write(&path, kra_file(false)).unwrap();
    let err = ed.revert_active().unwrap_err();
    assert!(err.contains("can no longer be read"), "{err}");
    assert_eq!(layer_tree(&ed), opened, "the layers stay as they are");
}

fn pixel(ed: &Editor, x: u32, y: u32) -> [u8; 4] {
    let doc = ed.active().unwrap();
    let rect = doc.canvas_rect();
    let rgba = compositor::composite_region(
        &doc.document,
        &doc.tiles,
        rect,
        0,
        compositor::CompositeOptions::default(),
    )
    .unwrap()
    .to_rgba8(&doc.document.meta.color_space);
    let i = ((y * rect.width + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

/// File > Open of a TIFF with Photoshop layers opens its layers (names,
/// opacity, pixels), in either byte order; a little-endian file's report
/// names what its conversion left out; File > Revert brings them back.
#[test]
fn a_layered_tiff_opens_and_reverts_as_its_photoshop_layers() {
    let dir = tempfile::tempdir().unwrap();
    for le in [false, true] {
        let path = write(
            dir.path(),
            &format!("layers-{le}.tif"),
            &tiff_fixture::layered_tiff(le, 8),
        );
        let mut ed = open(dir.path(), path);
        let tree = layer_tree(&ed);
        assert_eq!(
            tree,
            ["Blue dot:raster", "Background:raster"],
            "le={le}: the layers, top first"
        );
        let doc = ed.active().unwrap();
        let top = doc.document.layers.root()[0];
        let opacity = doc.document.layers.get(top).unwrap().opacity;
        assert!((opacity - 128.0 / 255.0).abs() < 1e-3, "{opacity}");
        assert_eq!(pixel(&ed, 0, 1), [255, 0, 0, 255], "the red background");
        let dot = pixel(&ed, 1, 0);
        assert!(
            dot[2] > 100 && dot[0] < 230,
            "blue at half over red: {dot:?}"
        );
        let status = ed.status().unwrap_or_default().to_string();
        assert!(status.contains("Photoshop layers"), "{status}");
        assert_eq!(
            status.contains("import report"),
            le,
            "only the little-endian conversion leaves records out: {status}"
        );
        delete_top_and_revert(&mut ed).unwrap();
        assert_eq!(
            layer_tree(&ed),
            tree,
            "le={le}: Revert reads the layers back"
        );
    }
}

// ------------------------------------------------------------ PXZ / PVR

/// A Pixlr document: a 2x2 photo, a red rectangle and a text, bottom to
/// top, on a 40x30 canvas.
fn pxz_file() -> Vec<u8> {
    let manifest = br##"{"width": 40, "height": 30, "name": "Card", "stack": [
        {"type": "image", "name": "Photo", "rect": {"x": 0, "y": 0, "w": 2, "h": 2},
         "content": "photo.png"},
        {"type": "shape", "name": "Box", "rect": {"x": 10, "y": 10, "w": 20, "h": 10},
         "format": {"variant": "rectangle", "fill": {"type": "color", "value": "#ff0000"}}},
        {"type": "text", "name": "Title", "rect": {"x": 2, "y": 22, "w": 30, "h": 8},
         "format": {"font": {"name": "Arial"}, "size": 6, "align": "right",
                    "fill": {"type": "color", "value": "#000000"}},
         "content": "Hi"}
    ]}"##;
    let photo =
        raster::encode(raster::ExportFormat::Png, 2, 2, &[0, 0, 255, 255].repeat(4)).unwrap();
    stored_zip(&[
        ("manifest.json", manifest.as_slice()),
        ("photo.png", &photo),
    ])
}

/// A PVR version 3 texture, `w` x `h`, of the 64-bit pixel format `format`.
fn pvr_file(w: u32, h: u32, format: u64, data: &[u8]) -> Vec<u8> {
    let mut out = b"PVR".to_vec();
    out.push(3);
    out.extend(0u32.to_le_bytes());
    out.extend(format.to_le_bytes());
    for v in [0u32, 0, h, w, 1, 1, 1, 1, 0] {
        out.extend(v.to_le_bytes());
    }
    out.extend_from_slice(data);
    out
}

/// RGBA 8888: channel names `rgba`, eight bits each.
const PVR_RGBA8888: u64 = 0x6162_6772 | (0x0808_0808 << 32);

/// File > Open of a Pixlr `.pxz` opens its layers (a raster, a shape and a
/// text layer), names what did not map in the report, and File > Revert
/// reads the same layers back. The Open picker offers `.pxz` and `.pvr`.
#[test]
fn a_pxz_opens_and_reverts_as_its_layers() {
    let filters = crate::dialogs::open_file_filters();
    for ext in ["pxz", "pvr"] {
        assert!(filters[0].1.contains(&ext), ".{ext} is offered");
        assert!(filters[2].1.contains(&ext), ".{ext} is in Images");
    }
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "card.pxz", &pxz_file());
    let mut ed = open(dir.path(), path);
    let doc = ed.active().expect("the .pxz opened");
    assert_eq!((doc.document.width(), doc.document.height()), (40, 30));
    let tree = layer_tree(&ed);
    assert_eq!(
        tree,
        ["Title:other", "Box:shape", "Photo:raster"],
        "top first"
    );
    let top = ed.active().unwrap().document.layers.root()[0];
    assert!(matches!(
        ed.active().unwrap().document.layers.get(top).unwrap().kind,
        LayerKind::Text(_)
    ));
    assert_eq!(pixel(&ed, 1, 1), [0, 0, 255, 255], "the photo");
    assert_eq!(pixel(&ed, 20, 15), [255, 0, 0, 255], "the rectangle");
    let status = ed.status().unwrap_or_default().to_string();
    assert!(
        status.contains("import report"),
        "the alignment is reported: {status}"
    );
    delete_top_and_revert(&mut ed).unwrap();
    assert_eq!(layer_tree(&ed), tree, "Revert reads the layers back");
}

/// File > Open of a `.pvr` opens the texture at its size and colours; one
/// in a compression this build does not decode opens nothing and says so by
/// name.
#[test]
fn a_pvr_opens_as_its_texture_and_an_undecoded_one_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let texels = [
        [10u8, 20, 30, 255],
        [200, 100, 50, 255],
        [0, 0, 0, 255],
        [9, 9, 9, 255],
    ];
    let data: Vec<u8> = texels.concat();
    let path = write(dir.path(), "tex.pvr", &pvr_file(2, 2, PVR_RGBA8888, &data));
    let mut ed = open(dir.path(), path);
    let doc = ed.active().expect("the .pvr opened");
    assert_eq!((doc.document.width(), doc.document.height()), (2, 2));
    assert_eq!(pixel(&ed, 1, 0), texels[1]);
    assert_eq!(pixel(&ed, 1, 1), texels[3]);
    delete_top_and_revert(&mut ed).unwrap();
    assert_eq!(pixel(&ed, 0, 0), texels[0], "Revert reads the texture back");

    // PVRTC 2 bpp (format 1) is refused by name, opening nothing.
    let path = write(dir.path(), "tex2.pvr", &pvr_file(8, 8, 1, &[0; 32]));
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(path));
    let result = ed.dispatch(Action::Open);
    wait_for_imports(&mut ed);
    assert!(ed.active().is_none(), "no document opened");
    let said = format!("{result:?} {}", ed.status().unwrap_or_default());
    assert!(said.contains("PVRTC 2 bpp"), "{said}");
}
