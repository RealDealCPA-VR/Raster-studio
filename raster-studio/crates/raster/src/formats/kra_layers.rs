//! W16-L: a Krita document's **layers**: the layer tree in `maindoc.xml`
//! and each paint layer's pixels in Krita's tiled, LZF-compressed format.
//!
//! `maindoc.xml` holds `<DOC><IMAGE name width height ...><layers>` with a
//! `<layer>` element per node: `name`, `opacity` (0-255), `visible`,
//! `compositeop` (the blend mode's Krita id), `x` / `y` (the layer's
//! offset), `filename` (the pixel file, `<image name>/layers/<filename>`)
//! and `nodetype` (`paintlayer`, `grouplayer`, ...); a group nests its
//! children in its own `<layers>`, topmost first.
//!
//! A paint layer's file is a text header (`VERSION 2`, `TILEWIDTH 64`,
//! `TILEHEIGHT 64`, `PIXELSIZE n`, `DATA <tiles>`), then per tile a line
//! `x,y,LZF,<bytes>` and that many bytes: a flag (1 = LZF-compressed, 0 =
//! raw) and the tile's pixels **by byte plane** (every pixel's byte 0, then
//! every pixel's byte 1, ...), which for 8-bit RGBA is B, G, R, A. 16-bit
//! RGBA (pixel size 8, little-endian channels) is read and reduced to 8
//! bits. Other colour models (CMYK, Lab, grey, float) are not read: their
//! layer opens empty and [`KraDocument::notes`] says so.
//!
//! Masks, filter / fill / clone / file / vector layers are not rendered;
//! each is named in the notes (a vector layer's shapes are SVG inside the
//! archive, not pixels).
//!
//! # Untrusted input
//!
//! The XML walk is a small tokenizer with a depth cap; every tile's offset
//! and length are checked against the file; LZF output is capped at the
//! tile's size; the layer count, and the total pixels allocated for them,
//! are checked against [`ImportLimits`] before each layer is allocated.

use super::super::{check_decode, malformed};
use super::{entry, NAME};
use crate::codec::{CodecError, ImportLimits};

/// The deepest group nesting read.
const MAX_DEPTH: usize = 64;
/// The most layers read from one document.
const MAX_LAYERS: usize = 4096;

/// One node of a Krita layer tree.
#[derive(Debug, Clone, PartialEq)]
pub struct KraLayer {
    pub name: String,
    /// The layer's pixels' top-left on the canvas (a paint layer's bounds
    /// are its painted tiles'); `0, 0` for a group.
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
    /// `0.0..=1.0`.
    pub opacity: f32,
    pub visible: bool,
    /// Krita's composite-op id (`normal`, `multiply`, `screen`, ...).
    pub blend: String,
    pub is_group: bool,
    /// Children, topmost first (groups only).
    pub children: Vec<KraLayer>,
    /// Straight-alpha RGBA8, `width * height * 4` bytes (paint layers).
    pub rgba: Vec<u8>,
}

/// A Krita document read as layers.
#[derive(Debug, Clone, PartialEq)]
pub struct KraDocument {
    pub width: u32,
    pub height: u32,
    /// Top-level layers, topmost first.
    pub layers: Vec<KraLayer>,
    /// What was not read, one sentence each.
    pub notes: Vec<String>,
}

#[derive(Debug)]
struct Element {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Element>,
}

impl Element {
    fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// A tolerant XML reader: elements and attributes only (text, comments,
/// processing instructions and doctypes are skipped).
fn parse_xml(text: &str) -> Result<Element, CodecError> {
    let bad = |why: &str| malformed(NAME, format!("maindoc.xml: {why}"));
    let b = text.as_bytes();
    let mut stack: Vec<Element> = vec![Element {
        name: String::new(),
        attrs: Vec::new(),
        children: Vec::new(),
    }];
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &text[i..];
        if rest.starts_with("<!--") {
            i += rest.find("-->").map_or(b.len() - i, |e| e + 3);
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            i += rest.find('>').map_or(b.len() - i, |e| e + 1);
            continue;
        }
        // Find the tag's end, allowing '>' inside quoted attribute values.
        let mut j = i + 1;
        let mut quote = None;
        while j < b.len() {
            match (quote, b[j]) {
                (None, b'"' | b'\'') => quote = Some(b[j]),
                (Some(q), c) if c == q => quote = None,
                (None, b'>') => break,
                _ => {}
            }
            j += 1;
        }
        if j >= b.len() {
            return Err(bad("a tag is not closed"));
        }
        let tag = &text[i + 1..j];
        i = j + 1;
        if let Some(name) = tag.strip_prefix('/') {
            let done = stack.pop().ok_or_else(|| bad("unbalanced tags"))?;
            if done.name != name.trim() {
                return Err(bad("mismatched tags"));
            }
            stack
                .last_mut()
                .ok_or_else(|| bad("unbalanced tags"))?
                .children
                .push(done);
            continue;
        }
        let self_closing = tag.ends_with('/');
        let tag = tag.trim_end_matches('/');
        let name_end = tag.find(|c: char| c.is_whitespace()).unwrap_or(tag.len());
        let mut element = Element {
            name: tag[..name_end].to_string(),
            attrs: Vec::new(),
            children: Vec::new(),
        };
        let mut a = &tag[name_end..];
        while let Some(eq) = a.find('=') {
            let key = a[..eq].trim().to_string();
            let after = a[eq + 1..].trim_start();
            let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                break;
            };
            let Some(close) = after[1..].find(q) else {
                return Err(bad("an attribute is not closed"));
            };
            element.attrs.push((key, unescape(&after[1..1 + close])));
            a = &after[close + 2..];
        }
        if self_closing {
            stack
                .last_mut()
                .ok_or_else(|| bad("unbalanced tags"))?
                .children
                .push(element);
        } else {
            if stack.len() > MAX_DEPTH * 3 {
                return Err(bad("the layer tree is nested too deeply"));
            }
            stack.push(element);
        }
    }
    // Close anything left open (tolerated), then return the root.
    while let Some(done) = stack.pop() {
        match stack.last_mut() {
            Some(parent) => parent.children.push(done),
            None => return Ok(done),
        }
    }
    Err(bad("empty"))
}

/// The next `\n`-terminated line of `data` from `at`, moving `at` past it.
fn next_line(data: &[u8], at: &mut usize) -> Option<String> {
    let rest = data.get(*at..)?;
    let end = rest.iter().position(|b| *b == b'\n')?;
    let s = String::from_utf8_lossy(&rest[..end]).into_owned();
    *at += end + 1;
    Some(s)
}

/// LZF (liblzf) decompression into exactly `out_len` bytes.
pub fn lzf_decompress(input: &[u8], out_len: usize) -> Result<Vec<u8>, CodecError> {
    let bad = || malformed(NAME, "a tile's LZF data is damaged");
    let mut out = Vec::with_capacity(out_len);
    let mut i = 0;
    while i < input.len() {
        let ctrl = usize::from(input[i]);
        i += 1;
        if ctrl < 32 {
            let run = ctrl + 1;
            let lit = input.get(i..i + run).ok_or_else(bad)?;
            if out.len() + run > out_len {
                return Err(bad());
            }
            out.extend_from_slice(lit);
            i += run;
        } else {
            let mut len = ctrl >> 5;
            if len == 7 {
                len += usize::from(*input.get(i).ok_or_else(bad)?);
                i += 1;
            }
            let low = usize::from(*input.get(i).ok_or_else(bad)?);
            i += 1;
            let back = ((ctrl & 0x1F) << 8) + low + 1;
            let len = len + 2;
            if back > out.len() || out.len() + len > out_len {
                return Err(bad());
            }
            let from = out.len() - back;
            for k in 0..len {
                let byte = out[from + k];
                out.push(byte);
            }
        }
    }
    if out.len() != out_len {
        return Err(bad());
    }
    Ok(out)
}

/// A paint layer's tiles, placed: (x0, y0, width, height, RGBA8).
fn read_tiles(
    data: &[u8],
    sixteen: bool,
    limits: ImportLimits,
    budget: &mut u64,
) -> Result<(i64, i64, u32, u32, Vec<u8>), CodecError> {
    let bad = |why: &str| malformed(NAME, format!("a layer's pixel data: {why}"));
    let mut at = 0;
    let (mut tw, mut th, mut pixel_size, mut version) = (64usize, 64usize, 4usize, 0usize);
    let count = loop {
        let l = next_line(data, &mut at).ok_or_else(|| bad("the header is cut short"))?;
        let mut parts = l.split_whitespace();
        let key = parts.next().unwrap_or("");
        let value: usize = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        match key {
            "VERSION" => version = value,
            "TILEWIDTH" => tw = value,
            "TILEHEIGHT" => th = value,
            "PIXELSIZE" => pixel_size = value,
            "DATA" => break value,
            _ => return Err(bad("the header has an unknown line")),
        }
    };
    if version != 2 {
        return Err(CodecError::Unsupported(format!(
            "Krita tile format version {version} is not supported (version 2 is)"
        )));
    }
    if !(1..=512).contains(&tw) || !(1..=512).contains(&th) {
        return Err(bad("the tile size is out of range"));
    }
    if pixel_size != if sixteen { 8 } else { 4 } {
        return Err(bad(
            "the pixel size does not match the layer's colour model",
        ));
    }
    let tile_bytes = tw * th * pixel_size;
    let mut tiles = Vec::new();
    for _ in 0..count {
        let l = next_line(data, &mut at).ok_or_else(|| bad("a tile header is cut short"))?;
        let f: Vec<&str> = l.trim().split(',').collect();
        if f.len() != 4 || f[2] != "LZF" {
            return Err(bad("a tile header is damaged"));
        }
        let x: i64 = f[0]
            .parse()
            .map_err(|_| bad("a tile position is damaged"))?;
        let y: i64 = f[1]
            .parse()
            .map_err(|_| bad("a tile position is damaged"))?;
        let len: usize = f[3].parse().map_err(|_| bad("a tile length is damaged"))?;
        let body = data
            .get(at..at.saturating_add(len))
            .filter(|b| b.len() == len && !b.is_empty())
            .ok_or_else(|| bad("a tile runs past the file"))?;
        at += len;
        if x.unsigned_abs() > 1 << 30 || y.unsigned_abs() > 1 << 30 {
            return Err(bad("a tile lies too far from the canvas"));
        }
        *budget = budget.checked_sub(tile_bytes as u64).ok_or_else(|| {
            CodecError::LimitExceeded("the layers' tiles exceed the import allocation limit".into())
        })?;
        let planar = if body[0] == 1 {
            lzf_decompress(&body[1..], tile_bytes)?
        } else {
            let raw = &body[1..];
            if raw.len() != tile_bytes {
                return Err(bad("an uncompressed tile has the wrong size"));
            }
            raw.to_vec()
        };
        tiles.push((x, y, planar));
    }
    if tiles.is_empty() {
        return Ok((0, 0, 0, 0, Vec::new()));
    }
    let x0 = tiles.iter().map(|t| t.0).min().unwrap_or(0);
    let y0 = tiles.iter().map(|t| t.1).min().unwrap_or(0);
    let x1 = tiles.iter().map(|t| t.0 + tw as i64).max().unwrap_or(0);
    let y1 = tiles.iter().map(|t| t.1 + th as i64).max().unwrap_or(0);
    let (w, h) = (
        u32::try_from(x1 - x0).map_err(|_| bad("the layer is too large"))?,
        u32::try_from(y1 - y0).map_err(|_| bad("the layer is too large"))?,
    );
    check_decode(limits, w, h, 4, 0)?;
    let mut rgba = vec![0u8; w as usize * h as usize * 4];
    let n = tw * th;
    for (tx, ty, planar) in tiles {
        for p in 0..n {
            let byte = |i: usize| planar[i * n + p];
            let px = if pixel_size == 4 {
                [byte(2), byte(1), byte(0), byte(3)]
            } else {
                // 16-bit little-endian B, G, R, A: the high bytes.
                [byte(5), byte(3), byte(1), byte(7)]
            };
            let (x, y) = ((tx - x0) as usize + p % tw, (ty - y0) as usize + p / tw);
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&px);
        }
    }
    Ok((x0, y0, w, h, rgba))
}

struct Ctx<'a> {
    zip: &'a [u8],
    image_name: String,
    limits: ImportLimits,
    notes: Vec<String>,
    count: usize,
    budget: u64,
}

fn read_node(ctx: &mut Ctx<'_>, e: &Element, depth: usize) -> Result<Option<KraLayer>, CodecError> {
    if depth > MAX_DEPTH {
        return Err(malformed(NAME, "the layer tree is nested too deeply"));
    }
    ctx.count += 1;
    if ctx.count > MAX_LAYERS {
        return Err(CodecError::LimitExceeded(format!(
            "the document has more than {MAX_LAYERS} layers"
        )));
    }
    let name = e.attr("name").unwrap_or("Layer").to_string();
    let kind = e.attr("nodetype").unwrap_or("paintlayer");
    let opacity = e
        .attr("opacity")
        .and_then(|v| v.parse::<f32>().ok())
        .map_or(1.0, |o| (o / 255.0).clamp(0.0, 1.0));
    let visible = e.attr("visible") != Some("0");
    let blend = e.attr("compositeop").unwrap_or("normal").to_string();
    let offset = |k: &str| e.attr(k).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    let mut layer = KraLayer {
        name: name.clone(),
        x: 0,
        y: 0,
        width: 0,
        height: 0,
        opacity,
        visible,
        blend,
        is_group: false,
        children: Vec::new(),
        rgba: Vec::new(),
    };
    if e.children.iter().any(|c| c.name == "masks") {
        ctx.notes
            .push(format!("\u{201c}{name}\u{201d}: its masks were not read"));
    }
    match kind {
        "grouplayer" => {
            layer.is_group = true;
            if let Some(list) = e.children.iter().find(|c| c.name == "layers") {
                for child in list.children.iter().filter(|c| c.name == "layer") {
                    if let Some(l) = read_node(ctx, child, depth + 1)? {
                        layer.children.push(l);
                    }
                }
            }
        }
        "paintlayer" => {
            let space = e.attr("colorspacename").unwrap_or("RGBA");
            if space != "RGBA" && space != "RGBA16" {
                ctx.notes.push(format!(
                    "\u{201c}{name}\u{201d}: its colour model ({space}) is not read, so it opens empty"
                ));
                return Ok(Some(layer));
            }
            let file = e.attr("filename").unwrap_or("");
            let path = format!("{}/layers/{file}", ctx.image_name);
            let Some(data) = entry(ctx.zip, &path, ctx.limits.max_alloc_bytes)? else {
                ctx.notes.push(format!(
                    "\u{201c}{name}\u{201d}: its pixel file {path} is missing"
                ));
                return Ok(Some(layer));
            };
            let (x, y, w, h, rgba) =
                read_tiles(&data, space == "RGBA16", ctx.limits, &mut ctx.budget)?;
            // The layer's offset is as untrusted as a tile's origin: bound
            // it the same way so the sum can never overflow.
            let (dx, dy) = (offset("x"), offset("y"));
            if dx.unsigned_abs() > 1 << 30 || dy.unsigned_abs() > 1 << 30 {
                return Err(malformed(
                    NAME,
                    format!("\u{201c}{name}\u{201d} lies too far from the canvas"),
                ));
            }
            layer.x = x + dx;
            layer.y = y + dy;
            layer.width = w;
            layer.height = h;
            layer.rgba = rgba;
        }
        other => {
            ctx.notes.push(format!(
                "\u{201c}{name}\u{201d}: a Krita {other} is not rendered, so it was left out"
            ));
            return Ok(None);
        }
    }
    Ok(Some(layer))
}

/// Read a `.kra`'s layer tree and every paint layer's pixels.
pub fn read(bytes: &[u8], limits: ImportLimits) -> Result<KraDocument, CodecError> {
    let xml = entry(bytes, "maindoc.xml", limits.max_alloc_bytes)?
        .ok_or_else(|| malformed(NAME, "it has no maindoc.xml"))?;
    let root = parse_xml(&String::from_utf8_lossy(&xml))?;
    let image = root
        .children
        .iter()
        .find(|c| c.name == "DOC")
        .and_then(|d| d.children.iter().find(|c| c.name == "IMAGE"))
        .ok_or_else(|| malformed(NAME, "maindoc.xml has no IMAGE"))?;
    let dim = |k: &str| {
        image
            .attr(k)
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0)
    };
    let (width, height) = (dim("width"), dim("height"));
    if width == 0 || height == 0 {
        return Err(malformed(NAME, "maindoc.xml gives the canvas no size"));
    }
    limits.check_dimensions(width, height)?;
    let mut ctx = Ctx {
        zip: bytes,
        image_name: image.attr("name").unwrap_or("").to_string(),
        limits,
        notes: Vec::new(),
        count: 0,
        budget: limits.max_alloc_bytes,
    };
    let mut layers = Vec::new();
    if let Some(list) = image.children.iter().find(|c| c.name == "layers") {
        for e in list.children.iter().filter(|c| c.name == "layer") {
            if let Some(l) = read_node(&mut ctx, e, 0)? {
                layers.push(l);
            }
        }
    }
    Ok(KraDocument {
        width,
        height,
        layers,
        notes: ctx.notes,
    })
}

/// The document's visible layers composited source-over (every blend mode
/// drawn as Normal) onto a transparent canvas: the flat image a `.kra`
/// without a `mergedimage.png` opens as.
pub fn flatten(doc: &KraDocument, limits: ImportLimits) -> Result<Vec<u8>, CodecError> {
    check_decode(limits, doc.width, doc.height, 8, 0)?;
    let n = doc.width as usize * doc.height as usize;
    let mut acc = vec![0f32; n * 4];
    fn draw(acc: &mut [f32], doc: &KraDocument, layers: &[KraLayer], opacity: f32) {
        for l in layers.iter().rev() {
            if !l.visible {
                continue;
            }
            let o = opacity * l.opacity;
            if l.is_group {
                draw(acc, doc, &l.children, o);
                continue;
            }
            for y in 0..l.height as i64 {
                let cy = l.y + y;
                if cy < 0 || cy >= i64::from(doc.height) {
                    continue;
                }
                for x in 0..l.width as i64 {
                    let cx = l.x + x;
                    if cx < 0 || cx >= i64::from(doc.width) {
                        continue;
                    }
                    let s = ((y * i64::from(l.width) + x) * 4) as usize;
                    let d = ((cy * i64::from(doc.width) + cx) * 4) as usize;
                    let a = f32::from(l.rgba[s + 3]) / 255.0 * o;
                    let da = acc[d + 3];
                    let out_a = a + da * (1.0 - a);
                    for c in 0..3 {
                        let sc = f32::from(l.rgba[s + c]) / 255.0;
                        let dc = acc[d + c];
                        acc[d + c] = if out_a > 0.0 {
                            (sc * a + dc * da * (1.0 - a)) / out_a
                        } else {
                            0.0
                        };
                    }
                    acc[d + 3] = out_a;
                }
            }
        }
    }
    draw(&mut acc, doc, &doc.layers, 1.0);
    Ok(acc
        .iter()
        .map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8)
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A tile in Krita's layout: `planes` is per-pixel BGRA, flattened by
    /// byte plane, optionally LZF-compressed with runs (back references of
    /// distance 1) and literals.
    pub fn tile_bytes(pixels: &[[u8; 4]], compress: bool) -> Vec<u8> {
        let n = pixels.len();
        let mut planar = vec![0u8; n * 4];
        for (p, px) in pixels.iter().enumerate() {
            // BGRA.
            let bgra = [px[2], px[1], px[0], px[3]];
            for (i, v) in bgra.iter().enumerate() {
                planar[i * n + p] = *v;
            }
        }
        if !compress {
            let mut out = vec![0u8];
            out.extend(planar);
            return out;
        }
        let mut out = vec![1u8];
        let mut i = 0;
        while i < planar.len() {
            // A run: one literal byte, then a back reference of distance 1.
            let mut run = 1;
            while i + run < planar.len() && planar[i + run] == planar[i] && run < 200 {
                run += 1;
            }
            if run >= 4 {
                out.push(0);
                out.push(planar[i]);
                let len = run - 1 - 2;
                if len < 7 {
                    out.push((len << 5) as u8);
                } else {
                    out.push(7 << 5);
                    out.push((len - 7) as u8);
                }
                out.push(0);
                i += run;
            } else {
                out.push(0);
                out.push(planar[i]);
                i += 1;
            }
        }
        out
    }

    /// A Krita layer file of 64x64 tiles.
    pub fn layer_file(tiles: &[(i64, i64, Vec<u8>)]) -> Vec<u8> {
        let mut out = format!(
            "VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 4\nDATA {}\n",
            tiles.len()
        )
        .into_bytes();
        for (x, y, data) in tiles {
            out.extend(format!("{x},{y},LZF,{}\n", data.len()).into_bytes());
            out.extend_from_slice(data);
        }
        out
    }

    #[test]
    fn lzf_literals_and_back_references_decode() {
        // "abcabcabcX": literal "abc", back-ref distance 3 length 6, "X".
        let input = [2, b'a', b'b', b'c', (4 << 5), 2, 0, b'X'];
        assert_eq!(lzf_decompress(&input, 10).unwrap(), b"abcabcabcX");
        assert!(lzf_decompress(&input, 9).is_err());
        assert!(
            lzf_decompress(&[0x20, 5], 4).is_err(),
            "a reference before the start"
        );
        for cut in 0..input.len() {
            let _ = lzf_decompress(&input[..cut], 10);
        }
    }

    fn kra_file() -> Vec<u8> {
        let maindoc = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE DOC PUBLIC '-//KDE//DTD krita 2.0//EN' 'http://www.calligra.org/DTD/krita-2.0.dtd'>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0" kritaVersion="5.2.2">
 <IMAGE name="Doc &amp; Co" width="70" height="66" colorspacename="RGBA" mime="application/x-kra">
  <layers>
   <layer name="Top" opacity="128" visible="1" compositeop="multiply" x="0" y="0"
          nodetype="paintlayer" filename="layer3" colorspacename="RGBA" uuid="{1}"/>
   <layer name="Group" opacity="255" visible="0" compositeop="normal" x="0" y="0"
          nodetype="grouplayer" filename="layer4" uuid="{2}">
    <layers>
     <layer name="Inner" opacity="255" visible="1" compositeop="normal" x="5" y="0"
            nodetype="paintlayer" filename="layer2" colorspacename="RGBA" uuid="{3}"/>
    </layers>
   </layer>
   <layer name="Levels" nodetype="adjustmentlayer" filename="layer5" uuid="{4}"/>
  </layers>
 </IMAGE>
</DOC>"#;
        let mut top = vec![[10u8, 20, 30, 255]; 64 * 64];
        top[0] = [200, 0, 0, 128];
        let inner = vec![[0u8, 0, 255, 255]; 64 * 64];
        super::super::super::more_formats_w16::test_util::zip(
            &[
                ("mimetype", super::super::MIMETYPE),
                ("maindoc.xml", maindoc.as_slice()),
                (
                    "Doc & Co/layers/layer3",
                    &layer_file(&[(64, 0, tile_bytes(&top, true))]),
                ),
                (
                    "Doc & Co/layers/layer2",
                    &layer_file(&[(0, 64, tile_bytes(&inner, false))]),
                ),
            ],
            false,
        )
    }

    #[test]
    fn a_kra_reads_its_layer_tree_and_tiles() {
        let file = kra_file();
        let doc = read(&file, ImportLimits::default()).unwrap();
        assert_eq!((doc.width, doc.height), (70, 66));
        assert_eq!(doc.layers.len(), 2, "the adjustment layer is left out");
        let top = &doc.layers[0];
        assert_eq!(top.name, "Top");
        assert_eq!(top.blend, "multiply");
        assert!((top.opacity - 128.0 / 255.0).abs() < 1e-6 && top.visible);
        assert_eq!((top.x, top.y, top.width, top.height), (64, 0, 64, 64));
        assert_eq!(&top.rgba[..8], &[200, 0, 0, 128, 10, 20, 30, 255]);
        let group = &doc.layers[1];
        assert!(group.is_group && !group.visible);
        let inner = &group.children[0];
        assert_eq!((inner.name.as_str(), inner.x, inner.y), ("Inner", 5, 64));
        assert_eq!(&inner.rgba[..4], &[0, 0, 255, 255]);
        assert!(
            doc.notes.iter().any(|n| n.contains("adjustmentlayer")),
            "{:?}",
            doc.notes
        );
    }

    /// A one-layer 64x64 `.kra` whose paint layer has offset `(x, y)` and
    /// one tile at `tile`.
    fn kra_with_offset(x: &str, y: &str, tile: (i64, i64)) -> Vec<u8> {
        let maindoc = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<DOC syntaxVersion="2.0">
 <IMAGE name="D" width="64" height="64" colorspacename="RGBA">
  <layers>
   <layer name="P" opacity="255" visible="1" compositeop="normal" x="{x}" y="{y}"
          nodetype="paintlayer" filename="layer1" colorspacename="RGBA"/>
  </layers>
 </IMAGE>
</DOC>"#
        );
        let px = vec![[1u8, 2, 3, 255]; 64 * 64];
        super::super::super::more_formats_w16::test_util::zip(
            &[
                ("mimetype", super::super::MIMETYPE),
                ("maindoc.xml", maindoc.as_bytes()),
                (
                    "D/layers/layer1",
                    &layer_file(&[(tile.0, tile.1, tile_bytes(&px, false))]),
                ),
            ],
            false,
        )
    }

    #[test]
    fn a_kra_layer_offset_or_tile_origin_at_the_i64_edges_errors_never_panics() {
        let max = i64::MAX.to_string();
        let min = i64::MIN.to_string();
        let far = ((1i64 << 30) + 1).to_string();
        for (x, y, tile) in [
            (max.as_str(), "0", (64, 0)),
            ("0", max.as_str(), (0, 64)),
            (min.as_str(), "0", (0, 0)),
            ("0", min.as_str(), (0, 0)),
            (far.as_str(), "0", (0, 0)),
            ("0", "0", (i64::MIN, 0)),
            ("0", "0", (0, i64::MIN)),
        ] {
            let file = kra_with_offset(x, y, tile);
            let r = read(&file, ImportLimits::default());
            assert!(
                matches!(&r, Err(CodecError::Unsupported(m)) if m.contains("too far")),
                "x={x} y={y} tile={tile:?}: {r:?}"
            );
        }
        // An in-range offset still reads.
        let ok = read(
            &kra_with_offset("-3", "7", (64, 0)),
            ImportLimits::default(),
        )
        .unwrap();
        assert_eq!((ok.layers[0].x, ok.layers[0].y), (61, 7));
    }

    #[test]
    fn a_kra_without_a_merged_image_opens_as_its_layers_composited() {
        let file = kra_file();
        assert!(super::super::looks_like_kra(&file));
        let s = crate::codec::decode_surface_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (70, 66, crate::codec::ImportFormat::Kra)
        );
        let crate::codec::SurfacePixels::Rgba8(px) = &s.pixels else {
            panic!()
        };
        let at = |x: usize, y: usize| &px[(y * 70 + x) * 4..(y * 70 + x) * 4 + 4];
        // Top's first pixel at (64, 0): alpha 128/255 at 50% opacity.
        assert_eq!(at(64, 0), &[200, 0, 0, 64]);
        assert_eq!(at(65, 0), &[10, 20, 30, 128]);
        // The hidden group's layer is not drawn.
        assert_eq!(at(10, 65), &[0, 0, 0, 0]);
        let info = crate::codec::probe_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!((info.width, info.height), (70, 66));
    }

    #[test]
    fn damaged_krita_layers_error_and_never_panic() {
        super::super::super::more_formats_w16::test_util::fuzz(
            &kra_file(),
            crate::codec::ImportFormat::Kra,
        );
        let tight = ImportLimits {
            max_alloc_bytes: 20_000,
            ..ImportLimits::default()
        };
        assert!(read(&kra_file(), tight).is_err());
    }

    #[test]
    fn xml_attributes_nesting_and_entities_parse() {
        let root = parse_xml(
            "<?xml version=\"1.0\"?><!DOCTYPE DOC><DOC a='1'><IMAGE name=\"A &amp; B\" \
             width=\"3\"><layers><layer name=\"x&gt;y\"/></layers></IMAGE></DOC>",
        )
        .unwrap();
        let image = &root.children[0].children[0];
        assert_eq!(image.attr("name"), Some("A & B"));
        assert_eq!(image.children[0].children[0].attr("name"), Some("x>y"));
        assert!(parse_xml("<a><b></a>").is_err());
    }
}
