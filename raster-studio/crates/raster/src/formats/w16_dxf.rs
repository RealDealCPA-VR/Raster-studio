//! W16-L: AutoCAD DXF (ASCII), its entities drawn.
//!
//! A DXF is a list of group-code / value line pairs. The `ENTITIES`
//! section's entities are read, and the layer table's colours with them:
//!
//! | Entity | Drawn as |
//! | --- | --- |
//! | `LINE` | a line |
//! | `LWPOLYLINE`, `POLYLINE` + `VERTEX` | a polyline, open or closed; a vertex's bulge (group 42) becomes the circular arc it describes |
//! | `CIRCLE`, `ARC` | the circle / counter-clockwise arc, sampled |
//! | `ELLIPSE` | the ellipse or elliptical arc, sampled |
//! | `SPLINE` | the B-spline (rational when weights are given) sampled from its control points and knots; its fit points, or its control polygon, when the knots do not fit |
//! | `POINT` | a dot |
//! | `TEXT`, `MTEXT` | text at its insertion point, height and rotation (MTEXT formatting codes are stripped, `\P` breaks the line) |
//!
//! Anything else (`INSERT` block references, `HATCH`, `DIMENSION`, 3D
//! solids) is not drawn. Colours come from the entity's ACI index (group
//! 62) or true colour (420), else its layer's colour; ACI 7 is drawn black.
//!
//! The drawing's extent is fitted into a 1024-pixel square with a 16-pixel
//! margin on a white page, Y flipped (DXF's Y axis points up), and drawn by
//! `resvg` through [`crate::codec::svg_import::rasterize`], with its size
//! and allocation checks. [`to_svg`] is the SVG it draws, for callers that
//! want the geometry as vectors. A binary DXF is refused by name.

use std::collections::HashMap;
use std::f64::consts::PI;
use std::fmt::Write as _;

use super::super::malformed;
use crate::codec::{CodecError, DecodedSurface, ImportFormat, ImportLimits};

const NAME: &str = "DXF";
const BINARY: &[u8] = b"AutoCAD Binary DXF";
/// The longer side of the drawing, in pixels.
const FIT: f64 = 1024.0;
const MARGIN: f64 = 16.0;
/// Caps on what one file may make the reader build.
const MAX_ENTITIES: usize = 200_000;
const MAX_POINTS: usize = 4_000_000;

/// `true` when `head` starts like a DXF: a `0` / `SECTION` pair, a `999`
/// comment, or the binary DXF sentinel.
pub fn looks_like_dxf(head: &[u8]) -> bool {
    if head.starts_with(BINARY) {
        return true;
    }
    let text = String::from_utf8_lossy(head);
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    match (lines.next(), lines.next()) {
        (Some("0"), Some("SECTION")) => true,
        (Some("999"), Some(_)) => text.contains("SECTION"),
        _ => false,
    }
}

/// One drawable thing, in drawing units.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// A polyline (lines, arcs and curves are sampled into these).
    Path {
        points: Vec<(f64, f64)>,
        closed: bool,
    },
    /// A dot.
    Point { x: f64, y: f64 },
    /// Text, its baseline starting at (`x`, `y`).
    Text {
        x: f64,
        y: f64,
        height: f64,
        rotation: f64,
        text: String,
    },
}

/// A shape with its layer and colour.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub layer: String,
    pub color: [u8; 3],
    pub shape: Shape,
}

fn aci(index: i64) -> Option<[u8; 3]> {
    Some(match index.abs() {
        1 => [255, 0, 0],
        2 => [255, 255, 0],
        3 => [0, 255, 0],
        4 => [0, 255, 255],
        5 => [0, 0, 255],
        6 => [255, 0, 255],
        7 => [0, 0, 0],
        8 => [128, 128, 128],
        9 => [192, 192, 192],
        250..=255 => {
            let v = ((index.abs() - 250) * 51) as u8;
            [v, v, v]
        }
        _ => return None,
    })
}

/// The group-code / value pairs of an ASCII DXF.
fn pairs(bytes: &[u8]) -> Result<Vec<(i32, String)>, CodecError> {
    if bytes.starts_with(BINARY) {
        return Err(CodecError::Unsupported(
            "binary DXF is not supported; save the drawing as ASCII DXF".into(),
        ));
    }
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.lines();
    let mut out = Vec::new();
    while let Some(code) = lines.next() {
        let code = code.trim();
        if code.is_empty() {
            continue;
        }
        let code: i32 = code
            .parse()
            .map_err(|_| malformed(NAME, format!("\"{code}\" is not a group code")))?;
        let value = lines
            .next()
            .ok_or_else(|| malformed(NAME, "the file ends after a group code"))?;
        out.push((code, value.trim_end_matches('\r').trim().to_string()));
    }
    Ok(out)
}

/// The entities of an ASCII DXF.
pub fn parse(bytes: &[u8]) -> Result<Vec<Entity>, CodecError> {
    let pairs = pairs(bytes)?;
    // Layer colours from the LAYER table.
    let mut layer_colors: HashMap<String, [u8; 3]> = HashMap::new();
    let mut groups: Vec<(String, Vec<(i32, String)>)> = Vec::new();
    let mut section = String::new();
    let mut i = 0;
    while i < pairs.len() {
        let (code, value) = &pairs[i];
        if *code == 0 && value == "SECTION" {
            section = pairs
                .get(i + 1)
                .filter(|p| p.0 == 2)
                .map(|p| p.1.clone())
                .unwrap_or_default();
            i += 2;
            continue;
        }
        if *code == 0 && value == "ENDSEC" {
            section.clear();
            i += 1;
            continue;
        }
        if *code == 0 && (section == "ENTITIES" || section == "TABLES") {
            let mut body = Vec::new();
            let mut j = i + 1;
            while j < pairs.len() && pairs[j].0 != 0 {
                body.push(pairs[j].clone());
                j += 1;
            }
            if section == "TABLES" {
                if value == "LAYER" {
                    let name = body.iter().find(|p| p.0 == 2).map(|p| p.1.clone());
                    let color = body
                        .iter()
                        .find(|p| p.0 == 62)
                        .and_then(|p| p.1.parse::<i64>().ok())
                        .and_then(aci);
                    if let (Some(n), Some(c)) = (name, color) {
                        layer_colors.insert(n, c);
                    }
                }
            } else {
                if groups.len() >= MAX_ENTITIES {
                    return Err(CodecError::LimitExceeded(format!(
                        "the drawing has more than {MAX_ENTITIES} entities"
                    )));
                }
                groups.push((value.clone(), body));
            }
            i = j;
            continue;
        }
        i += 1;
    }
    let mut out = Vec::new();
    let mut points = 0usize;
    type Group = Vec<(i32, String)>;
    let mut polyline: Option<(Group, Vec<Group>)> = None;
    for (kind, body) in groups {
        match kind.as_str() {
            "POLYLINE" => polyline = Some((body, Vec::new())),
            "VERTEX" => {
                if let Some((_, vertices)) = polyline.as_mut() {
                    vertices.push(body);
                }
            }
            "SEQEND" => {
                if let Some((head, vertices)) = polyline.take() {
                    let mut all = head.clone();
                    all.retain(|p| p.0 != 10 && p.0 != 20 && p.0 != 42);
                    for v in vertices {
                        all.extend(v.into_iter().filter(|p| matches!(p.0, 10 | 20 | 42)));
                    }
                    push(&mut out, &mut points, "LWPOLYLINE", &all, &layer_colors)?;
                }
            }
            other => push(&mut out, &mut points, other, &body, &layer_colors)?,
        }
    }
    Ok(out)
}

fn push(
    out: &mut Vec<Entity>,
    points: &mut usize,
    kind: &str,
    body: &[(i32, String)],
    layers: &HashMap<String, [u8; 3]>,
) -> Result<(), CodecError> {
    let Some(shape) = shape(kind, body) else {
        return Ok(());
    };
    if let Shape::Path { points: p, .. } = &shape {
        *points += p.len();
        if *points > MAX_POINTS {
            return Err(CodecError::LimitExceeded(format!(
                "the drawing samples to more than {MAX_POINTS} points"
            )));
        }
    }
    let layer = body
        .iter()
        .find(|p| p.0 == 8)
        .map(|p| p.1.clone())
        .unwrap_or_else(|| "0".into());
    let true_color = body
        .iter()
        .find(|p| p.0 == 420)
        .and_then(|p| p.1.parse::<u32>().ok())
        .map(|v| [(v >> 16) as u8, (v >> 8) as u8, v as u8]);
    let color = true_color
        .or_else(|| {
            body.iter()
                .find(|p| p.0 == 62)
                .and_then(|p| p.1.parse::<i64>().ok())
                .and_then(aci)
        })
        .or_else(|| layers.get(&layer).copied())
        .unwrap_or([0, 0, 0]);
    out.push(Entity {
        layer,
        color,
        shape,
    });
    Ok(())
}

fn num(body: &[(i32, String)], code: i32) -> Option<f64> {
    body.iter()
        .find(|p| p.0 == code)
        .and_then(|p| p.1.parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

fn all_nums(body: &[(i32, String)], code: i32) -> Vec<f64> {
    body.iter()
        .filter(|p| p.0 == code)
        .filter_map(|p| p.1.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .collect()
}

/// Segments for a sweep of `angle` radians: about one per 5 degrees.
fn steps(angle: f64) -> usize {
    ((angle.abs() / (PI / 36.0)).ceil() as usize).clamp(2, 720)
}

fn arc(cx: f64, cy: f64, r: f64, start: f64, sweep: f64) -> Vec<(f64, f64)> {
    let n = steps(sweep);
    (0..=n)
        .map(|k| {
            let a = start + sweep * k as f64 / n as f64;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

/// The points of the arc a bulge `b` puts between `p` and `q` (excluding
/// `p`, including `q`).
fn bulge(p: (f64, f64), q: (f64, f64), b: f64) -> Vec<(f64, f64)> {
    let (dx, dy) = (q.0 - p.0, q.1 - p.1);
    let d = (dx * dx + dy * dy).sqrt();
    if b == 0.0 || d == 0.0 || !b.is_finite() {
        return vec![q];
    }
    let theta = 4.0 * b.atan();
    let (ux, uy) = (dx / d, dy / d);
    let h = d / (2.0 * (theta / 2.0).tan());
    let (cx, cy) = ((p.0 + q.0) / 2.0 - uy * h, (p.1 + q.1) / 2.0 + ux * h);
    let r = ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt();
    let start = (p.1 - cy).atan2(p.0 - cx);
    let mut pts = arc(cx, cy, r, start, theta);
    pts.remove(0);
    if let Some(last) = pts.last_mut() {
        *last = q;
    }
    pts
}

/// A B-spline of `degree` over `knots` and `ctrl` (with `weights`, rational),
/// sampled.
fn bspline(
    degree: usize,
    knots: &[f64],
    ctrl: &[(f64, f64)],
    weights: &[f64],
) -> Option<Vec<(f64, f64)>> {
    let n = ctrl.len();
    if degree == 0 || degree > 11 || n <= degree || knots.len() != n + degree + 1 {
        return None;
    }
    if knots.windows(2).any(|w| w[1] < w[0]) {
        return None;
    }
    let (t0, t1) = (knots[degree], knots[n]);
    if t1.partial_cmp(&t0) != Some(std::cmp::Ordering::Greater) {
        return None;
    }
    let w = |i: usize| weights.get(i).copied().filter(|w| *w > 0.0).unwrap_or(1.0);
    let samples = (n * 16).clamp(16, 4096);
    let mut out = Vec::with_capacity(samples + 1);
    for s in 0..=samples {
        let t = t0 + (t1 - t0) * s as f64 / samples as f64;
        // The span k with knots[k] <= t < knots[k + 1] (last span at t1).
        let mut k = degree;
        while k < n - 1 && knots[k + 1] <= t {
            k += 1;
        }
        // de Boor, in homogeneous coordinates.
        let mut d: Vec<(f64, f64, f64)> = (0..=degree)
            .map(|j| {
                let i = j + k - degree;
                let wi = w(i);
                (ctrl[i].0 * wi, ctrl[i].1 * wi, wi)
            })
            .collect();
        for r in 1..=degree {
            for j in (r..=degree).rev() {
                let i = j + k - degree;
                let denom = knots[i + degree + 1 - r] - knots[i];
                let a = if denom == 0.0 {
                    0.0
                } else {
                    (t - knots[i]) / denom
                };
                d[j] = (
                    (1.0 - a) * d[j - 1].0 + a * d[j].0,
                    (1.0 - a) * d[j - 1].1 + a * d[j].1,
                    (1.0 - a) * d[j - 1].2 + a * d[j].2,
                );
            }
        }
        let (x, y, wt) = d[degree];
        if wt == 0.0 {
            return None;
        }
        out.push((x / wt, y / wt));
    }
    Some(out)
}

fn xy_list(body: &[(i32, String)], xc: i32, yc: i32) -> Vec<(f64, f64)> {
    let xs = all_nums(body, xc);
    let ys = all_nums(body, yc);
    xs.into_iter().zip(ys).collect()
}

fn strip_mtext(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('P') | Some('n') => out.push('\n'),
                Some('~') => out.push(' '),
                Some('\\') => out.push('\\'),
                Some('{') => out.push('{'),
                Some('}') => out.push('}'),
                Some('L' | 'l' | 'O' | 'o' | 'K' | 'k') => {}
                // \fArial|b0;  \H2.5;  \C1;  \S1^2; ... up to the semicolon.
                Some(_) => {
                    for d in chars.by_ref() {
                        if d == ';' {
                            break;
                        }
                    }
                }
                None => {}
            },
            '{' | '}' => {}
            c => out.push(c),
        }
    }
    out
}

fn shape(kind: &str, b: &[(i32, String)]) -> Option<Shape> {
    let p = |xc, yc| Some((num(b, xc)?, num(b, yc)?));
    match kind {
        "LINE" => Some(Shape::Path {
            points: vec![p(10, 20)?, p(11, 21)?],
            closed: false,
        }),
        "LWPOLYLINE" => {
            let closed = num(b, 70).is_some_and(|f| (f as i64) & 1 == 1);
            let mut verts: Vec<((f64, f64), f64)> = Vec::new();
            for (code, value) in b {
                let Ok(v) = value.parse::<f64>() else {
                    continue;
                };
                if !v.is_finite() {
                    continue;
                }
                match code {
                    10 => verts.push(((v, 0.0), 0.0)),
                    20 => {
                        if let Some(last) = verts.last_mut() {
                            last.0 .1 = v;
                        }
                    }
                    42 => {
                        if let Some(last) = verts.last_mut() {
                            last.1 = v;
                        }
                    }
                    _ => {}
                }
            }
            let first = verts.first()?.0;
            let mut points = vec![first];
            let count = verts.len();
            let segments = if closed { count } else { count - 1 };
            for s in 0..segments {
                let (from, bul) = verts[s];
                let to = verts[(s + 1) % count].0;
                points.extend(bulge(from, to, bul));
            }
            if closed && points.len() > 1 {
                points.pop();
            }
            Some(Shape::Path { points, closed })
        }
        "CIRCLE" => {
            let (cx, cy) = p(10, 20)?;
            let r = num(b, 40)?.abs();
            let mut points = arc(cx, cy, r, 0.0, 2.0 * PI);
            points.pop();
            Some(Shape::Path {
                points,
                closed: true,
            })
        }
        "ARC" => {
            let (cx, cy) = p(10, 20)?;
            let r = num(b, 40)?.abs();
            let a0 = num(b, 50).unwrap_or(0.0).to_radians();
            let mut a1 = num(b, 51).unwrap_or(360.0).to_radians();
            while a1 <= a0 {
                a1 += 2.0 * PI;
            }
            Some(Shape::Path {
                points: arc(cx, cy, r, a0, (a1 - a0).min(2.0 * PI)),
                closed: false,
            })
        }
        "ELLIPSE" => {
            let (cx, cy) = p(10, 20)?;
            let (mx, my) = p(11, 21)?;
            let ratio = num(b, 40).unwrap_or(1.0);
            let t0 = num(b, 41).unwrap_or(0.0);
            let mut t1 = num(b, 42).unwrap_or(2.0 * PI);
            while t1 <= t0 {
                t1 += 2.0 * PI;
            }
            let sweep = (t1 - t0).min(2.0 * PI);
            let full = (sweep - 2.0 * PI).abs() < 1e-9;
            let n = steps(sweep);
            let mut points: Vec<(f64, f64)> = (0..=n)
                .map(|k| {
                    let t = t0 + sweep * k as f64 / n as f64;
                    (
                        cx + mx * t.cos() - ratio * my * t.sin(),
                        cy + my * t.cos() + ratio * mx * t.sin(),
                    )
                })
                .collect();
            if full {
                points.pop();
            }
            Some(Shape::Path {
                points,
                closed: full,
            })
        }
        "SPLINE" => {
            let closed = num(b, 70).is_some_and(|f| (f as i64) & 1 == 1);
            let degree = num(b, 71).unwrap_or(3.0).max(0.0) as usize;
            let knots = all_nums(b, 40);
            let weights = all_nums(b, 41);
            let ctrl = xy_list(b, 10, 20);
            let fit = xy_list(b, 11, 21);
            let points = bspline(degree, &knots, &ctrl, &weights)
                .or_else(|| (fit.len() >= 2).then(|| fit.clone()))
                .or_else(|| (ctrl.len() >= 2).then(|| ctrl.clone()))?;
            Some(Shape::Path { points, closed })
        }
        "POINT" => {
            let (x, y) = p(10, 20)?;
            Some(Shape::Point { x, y })
        }
        "TEXT" | "MTEXT" => {
            let (x, y) = p(10, 20)?;
            let height = num(b, 40).filter(|h| *h > 0.0).unwrap_or(1.0);
            let mut rotation = num(b, 50).unwrap_or(0.0);
            let text = if kind == "MTEXT" {
                if let (Some(dx), Some(dy)) = (num(b, 11), num(b, 21)) {
                    rotation = dy.atan2(dx).to_degrees();
                }
                let raw: String = b
                    .iter()
                    .filter(|p| p.0 == 3)
                    .chain(b.iter().filter(|p| p.0 == 1))
                    .map(|p| p.1.as_str())
                    .collect();
                strip_mtext(&raw)
            } else {
                b.iter().find(|p| p.0 == 1)?.1.clone()
            };
            // MTEXT's insertion point is its top-left corner by default.
            let y = if kind == "MTEXT" { y - height } else { y };
            Some(Shape::Text {
                x,
                y,
                height,
                rotation,
                text,
            })
        }
        _ => None,
    }
}

fn escape(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .fold(String::new(), |mut o, c| {
            match c {
                '&' => o.push_str("&amp;"),
                '<' => o.push_str("&lt;"),
                '>' => o.push_str("&gt;"),
                '"' => o.push_str("&quot;"),
                c => o.push(c),
            }
            o
        })
}

/// The entities as an SVG document (Y flipped, fitted to [`FIT`] pixels
/// with a margin, on a white page), and its pixel size.
pub fn to_svg(entities: &[Entity]) -> (String, u32, u32) {
    let mut lo = (f64::INFINITY, f64::INFINITY);
    let mut hi = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut grow = |x: f64, y: f64| {
        lo = (lo.0.min(x), lo.1.min(y));
        hi = (hi.0.max(x), hi.1.max(y));
    };
    for e in entities {
        match &e.shape {
            Shape::Path { points, .. } => points.iter().for_each(|p| grow(p.0, p.1)),
            Shape::Point { x, y } => grow(*x, *y),
            Shape::Text {
                x, y, height, text, ..
            } => {
                let longest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0);
                grow(*x, *y);
                grow(x + height * 0.6 * longest as f64, y + height);
            }
        }
    }
    if !lo.0.is_finite() {
        lo = (0.0, 0.0);
        hi = (1.0, 1.0);
    }
    let (dw, dh) = (hi.0 - lo.0, hi.1 - lo.1);
    let span = dw.max(dh);
    let scale = if span > 0.0 { FIT / span } else { 1.0 };
    let w = (dw * scale + 2.0 * MARGIN).ceil().max(1.0) as u32;
    let h = (dh * scale + 2.0 * MARGIN).ceil().max(1.0) as u32;
    let tx = |x: f64| (x - lo.0) * scale + MARGIN;
    let ty = |y: f64| (hi.1 - y) * scale + MARGIN;
    let mut svg = String::new();
    let _ = write!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">\
         <rect id=\"Background\" width=\"{w}\" height=\"{h}\" fill=\"#ffffff\"/>"
    );
    let mut layer: Option<&str> = None;
    for e in entities {
        if layer != Some(e.layer.as_str()) {
            if layer.is_some() {
                svg.push_str("</g>");
            }
            let _ = write!(svg, "<g id=\"{}\">", escape(&e.layer));
            layer = Some(e.layer.as_str());
        }
        let [r, g, b] = e.color;
        let color = format!("#{r:02x}{g:02x}{b:02x}");
        match &e.shape {
            Shape::Path { points, closed } => {
                let mut d = String::new();
                for (i, (x, y)) in points.iter().enumerate() {
                    let _ = write!(
                        d,
                        "{}{:.3} {:.3} ",
                        if i == 0 { 'M' } else { 'L' },
                        tx(*x),
                        ty(*y)
                    );
                }
                if *closed {
                    d.push('Z');
                }
                let _ = write!(
                    svg,
                    "<path d=\"{d}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"1\"/>"
                );
            }
            Shape::Point { x, y } => {
                let _ = write!(
                    svg,
                    "<circle cx=\"{:.3}\" cy=\"{:.3}\" r=\"1.5\" fill=\"{color}\"/>",
                    tx(*x),
                    ty(*y)
                );
            }
            Shape::Text {
                x,
                y,
                height,
                rotation,
                text,
            } => {
                let size = height * scale;
                for (i, line) in text.lines().enumerate() {
                    let (px, py) = (tx(*x), ty(*y) + i as f64 * size * 1.4);
                    let _ = write!(
                        svg,
                        "<text x=\"{px:.3}\" y=\"{py:.3}\" font-size=\"{size:.3}\" \
                         font-family=\"sans-serif\" fill=\"{color}\" \
                         transform=\"rotate({:.3} {px:.3} {:.3})\">{}</text>",
                        -rotation,
                        ty(*y),
                        escape(line)
                    );
                }
            }
        }
    }
    if layer.is_some() {
        svg.push_str("</g>");
    }
    svg.push_str("</svg>");
    (svg, w, h)
}

/// The entities, or a refusal when none can be drawn.
fn drawable(bytes: &[u8]) -> Result<Vec<Entity>, CodecError> {
    let entities = parse(bytes)?;
    if entities.is_empty() {
        return Err(CodecError::Unsupported(
            "this DXF has no entities that can be drawn (LINE, LWPOLYLINE, POLYLINE, CIRCLE, \
             ARC, ELLIPSE, SPLINE, POINT, TEXT, MTEXT)"
                .into(),
        ));
    }
    Ok(entities)
}

/// The drawing as **vector layers** (what File > Open opens): one group per
/// DXF layer, each entity a shape (or text) layer inside it, and the white
/// page as a `Background` shape at the bottom. Read by the SVG layer reader
/// from [`to_svg`]'s document, so the layers draw what [`decode`]
/// rasterises.
pub fn layers(
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<crate::codec::svg_import::layers::VectorLayers, CodecError> {
    let entities = drawable(bytes)?;
    let (svg, _, _) = to_svg(&entities);
    crate::codec::svg_import::layers::read_layers(
        svg.as_bytes(),
        limits,
        ImportFormat::Dxf,
        "the drawing",
    )
}

/// Decode: the drawing, rasterised.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let entities = drawable(bytes)?;
    let (svg, _, _) = to_svg(&entities);
    let mut s = crate::codec::svg_import::rasterize(svg.as_bytes(), limits)?;
    s.source_format = ImportFormat::Dxf;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::super::test_util::fuzz;
    use super::*;
    use crate::codec::{decode_surface_bytes, probe_bytes, SurfacePixels};

    fn dxf(entities: &str) -> Vec<u8> {
        format!(
            "0\nSECTION\n2\nHEADER\n0\nENDSEC\n0\nSECTION\n2\nTABLES\n0\nTABLE\n2\nLAYER\n\
             0\nLAYER\n2\nRed\n62\n1\n0\nENDTAB\n0\nENDSEC\n\
             0\nSECTION\n2\nENTITIES\n{entities}0\nENDSEC\n0\nEOF\n"
        )
        .replace('\n', "\r\n")
        .into_bytes()
    }

    fn at(s: &DecodedSurface, x: u32, y: u32) -> [u8; 4] {
        let SurfacePixels::Rgba8(px) = &s.pixels else {
            panic!()
        };
        let i = ((y * s.width + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    }

    #[test]
    fn lines_polylines_circles_and_text_are_drawn() {
        // A 100 x 50 rectangle (closed LWPOLYLINE on layer Red, by-layer
        // colour), its diagonal (blue LINE), a circle and some text.
        let file = dxf(
            "0\nLWPOLYLINE\n8\nRed\n90\n4\n70\n1\n10\n0\n20\n0\n10\n100\n20\n0\n10\n100\n20\n50\n10\n0\n20\n50\n\
             0\nLINE\n8\n0\n62\n5\n10\n0\n20\n0\n11\n100\n21\n50\n\
             0\nCIRCLE\n8\n0\n10\n50\n20\n25\n40\n10\n\
             0\nTEXT\n8\n0\n10\n5\n20\n5\n40\n5\n1\nA&B\n",
        );
        assert!(looks_like_dxf(&file));
        let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!(s.source_format, ImportFormat::Dxf);
        // 100 units -> 1024 px, plus 16 px margins; 50 units -> 512 px.
        assert_eq!((s.width, s.height), (1056, 544));
        // The rectangle's bottom edge (y = 0) is at the bottom: red.
        let bottom = at(&s, 300, 544 - 16);
        assert!(
            bottom[0] > 200 && bottom[1] < 200 && bottom[1] == bottom[2],
            "{bottom:?}"
        );
        // The diagonal passes through the middle: blue.
        let mid = at(&s, 527, 272);
        assert!(mid[2] > 150 && mid[0] < 150, "{mid:?}");
        // Inside, away from every stroke: the white page.
        assert_eq!(at(&s, 200, 100), [255, 255, 255, 255]);
        // The circle's rightmost point (60, 25).
        let right = at(&s, (60.0 * 10.24 + 16.0) as u32, 272);
        assert!(right[0] < 100 && right[1] < 100, "{right:?}");
        let info = probe_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!((info.width, info.format), (1056, ImportFormat::Dxf));
    }

    #[test]
    fn arcs_bulges_ellipses_splines_and_polylines_sample() {
        let e = parse(&dxf(
            "0\nARC\n10\n0\n20\n0\n40\n1\n50\n0\n51\n90\n\
             0\nLWPOLYLINE\n90\n2\n70\n0\n10\n0\n20\n0\n42\n1\n10\n2\n20\n0\n\
             0\nELLIPSE\n10\n0\n20\n0\n11\n2\n21\n0\n40\n0.5\n\
             0\nSPLINE\n71\n1\n72\n4\n73\n2\n40\n0\n40\n0\n40\n1\n40\n1\n10\n0\n20\n0\n10\n4\n20\n2\n\
             0\nPOLYLINE\n70\n1\n0\nVERTEX\n10\n0\n20\n0\n0\nVERTEX\n10\n1\n20\n0\n0\nVERTEX\n10\n1\n20\n1\n0\nSEQEND\n\
             0\nMTEXT\n10\n0\n20\n10\n40\n2\n1\n{\\fArial;one}\\Ptwo\n",
        ))
        .unwrap();
        assert_eq!(e.len(), 6);
        let Shape::Path { points, .. } = &e[0].shape else {
            panic!()
        };
        let last = points.last().unwrap();
        assert!(
            (last.0).abs() < 1e-9 && (last.1 - 1.0).abs() < 1e-9,
            "{last:?}"
        );
        // A bulge of 1 is a half circle below the chord 0..2 (clockwise
        // from the start as seen with Y up means it bows to -y).
        let Shape::Path { points, .. } = &e[1].shape else {
            panic!()
        };
        let lowest = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
        assert!((lowest + 1.0).abs() < 1e-3, "{lowest}");
        let Shape::Path { points, closed } = &e[2].shape else {
            panic!()
        };
        assert!(*closed);
        let top = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
        assert!((top - 1.0).abs() < 1e-2, "{top}");
        // A degree-1 spline is its control polygon.
        let Shape::Path { points, .. } = &e[3].shape else {
            panic!()
        };
        let mid = points[points.len() / 2];
        assert!(
            (mid.0 - 2.0).abs() < 1e-6 && (mid.1 - 1.0).abs() < 1e-6,
            "{mid:?}"
        );
        let Shape::Path { points, closed } = &e[4].shape else {
            panic!()
        };
        assert!(*closed && points.len() == 3);
        let Shape::Text { text, .. } = &e[5].shape else {
            panic!()
        };
        assert_eq!(text, "one\ntwo");
    }

    #[test]
    fn binary_or_empty_or_damaged_dxf_errors_and_never_panics() {
        let mut bin = BINARY.to_vec();
        bin.extend_from_slice(b"\r\n\x1a\0");
        let err = decode_surface_bytes(&bin, ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("binary DXF"), "{err}");
        let err = decode_surface_bytes(&dxf(""), ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("no entities"), "{err}");
        fuzz(
            &dxf("0\nLINE\n10\n0\n20\n0\n11\n3\n21\n4\n0\nSPLINE\n71\n3\n40\n0\n40\n0\n40\n0\n40\n0\n40\n1\n40\n1\n40\n1\n40\n1\n10\n0\n20\n0\n10\n1\n20\n2\n10\n2\n20\n-1\n10\n3\n20\n0\n"),
            ImportFormat::Dxf,
        );
    }

    #[test]
    fn layers_are_a_group_per_dxf_layer_and_damaged_files_never_panic() {
        let file = dxf("0\nLINE\n8\nWalls\n10\n0\n20\n0\n11\n100\n21\n50\n0\nCIRCLE\n8\nDoors\n10\n50\n20\n25\n40\n10\n");
        let v = layers(&file, ImportLimits::default()).unwrap();
        assert_eq!((v.width, v.height), (1056, 544));
        let names: Vec<&str> = v.design.nodes.iter().map(|l| l.name.as_str()).collect();
        assert!(
            names.contains(&"Walls") && names.contains(&"Doors"),
            "{names:?}"
        );
        assert!(names.contains(&"Background"), "{names:?}");
        assert!(layers(&dxf(""), ImportLimits::default()).is_err());
        for cut in 0..file.len() {
            let _ = layers(&file[..cut], ImportLimits::default());
        }
    }
}
