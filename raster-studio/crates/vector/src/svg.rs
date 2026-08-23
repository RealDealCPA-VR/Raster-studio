//! SVG path data: parsing a `d` string and writing one back.
//!
//! This is the migration route off `ShapeLayer`'s placeholder `path_svg`
//! string, and the interchange format for pasting a path in from anywhere else,
//! so it implements the real grammar rather than a convenient subset: all
//! twenty commands, relative and absolute, implicit repeated commands, the
//! moveto-becomes-lineto rule, `.5`-style numbers, exponents, and the
//! flag-packing that lets `a1 1 0 011 1` be six values rather than four.
//!
//! # What a round trip preserves
//! **Geometry, exactly; spelling, no.** The parser lowers the two smooth
//! commands (`S`, `T`) to the explicit `C` and `Q` they stand for, and lowers
//! `A` to cubics, because [`crate::Path`] has no arc primitive to store — an
//! arc is not a Bezier, so keeping one would mean a path element that every
//! other operation in the crate had to special-case. Writing back therefore
//! emits `M`, `L`, `Q`, `C` and `Z` only. The curve is the same curve; the text
//! is not the same text.
//!
//! # Errors carry an offset
//! A malformed `d` is caller input — from a file, from a paste, from a
//! hand-edited document — so it is an error with the byte offset where parsing
//! stopped, never a panic and never a silently truncated path.

use crate::error::VectorError;
use crate::path::{Path, PathEl};
use crate::point::{point, Point};

/// Decimal places [`to_svg`] writes.
///
/// Six is about a nanometre on a metre-wide document and is what most
/// authoring tools emit; it keeps a round trip well inside any tolerance this
/// crate rasterises at.
pub const DEFAULT_PRECISION: usize = 6;

/// Parse SVG path data into a [`Path`].
pub fn parse(d: &str) -> Result<Path, VectorError> {
    Parser::new(d.as_bytes()).run()
}

/// Write a [`Path`] as SVG path data at [`DEFAULT_PRECISION`].
pub fn to_svg(path: &Path) -> String {
    to_svg_with_precision(path, DEFAULT_PRECISION)
}

/// Write a [`Path`] as SVG path data with a chosen number of decimal places.
pub fn to_svg_with_precision(path: &Path, precision: usize) -> String {
    let precision = precision.min(17);
    let mut s = String::new();
    for el in path.elements() {
        if !s.is_empty() {
            s.push(' ');
        }
        match *el {
            PathEl::MoveTo(p) => {
                s.push('M');
                push_point(&mut s, p, precision);
            }
            PathEl::LineTo(p) => {
                s.push('L');
                push_point(&mut s, p, precision);
            }
            PathEl::QuadTo(c, p) => {
                s.push('Q');
                push_point(&mut s, c, precision);
                s.push(' ');
                push_point(&mut s, p, precision);
            }
            PathEl::CurveTo(c1, c2, p) => {
                s.push('C');
                push_point(&mut s, c1, precision);
                s.push(' ');
                push_point(&mut s, c2, precision);
                s.push(' ');
                push_point(&mut s, p, precision);
            }
            PathEl::ClosePath => s.push('Z'),
        }
    }
    s
}

fn push_point(s: &mut String, p: Point, precision: usize) {
    s.push_str(&fmt_num(p.x, precision));
    s.push(' ');
    s.push_str(&fmt_num(p.y, precision));
}

/// A number with trailing zeros trimmed, and no `-0`.
fn fmt_num(v: f64, precision: usize) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    let mut s = format!("{v:.precision$}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" || s.is_empty() {
        "0".to_string()
    } else {
        s
    }
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    path: Path,
    cur: Point,
    sub_start: Point,
    /// Last cubic control point, for `S`; last quadratic one, for `T`.
    last_cubic: Option<Point>,
    last_quad: Option<Point>,
}

impl<'a> Parser<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            pos: 0,
            path: Path::new(),
            cur: Point::ZERO,
            sub_start: Point::ZERO,
            last_cubic: None,
            last_quad: None,
        }
    }

    fn err<T>(&self, reason: &str) -> Result<T, VectorError> {
        Err(VectorError::Svg {
            offset: self.pos,
            reason: reason.to_string(),
        })
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn is_wsp(b: u8) -> bool {
        matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0c)
    }

    fn skip_wsp(&mut self) {
        while self.peek().is_some_and(Self::is_wsp) {
            self.pos += 1;
        }
    }

    /// Whitespace and at most one comma, the SVG separator production.
    fn skip_sep(&mut self) {
        self.skip_wsp();
        if self.peek() == Some(b',') {
            self.pos += 1;
            self.skip_wsp();
        }
    }

    fn number(&mut self) -> Result<f64, VectorError> {
        self.skip_sep();
        let start = self.pos;
        if matches!(self.peek(), Some(b'+') | Some(b'-')) {
            self.pos += 1;
        }
        let mut digits = false;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
            digits = true;
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
                digits = true;
            }
        }
        if !digits {
            self.pos = start;
            return self.err("expected a number");
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            if self.peek().is_some_and(|b| b.is_ascii_digit()) {
                while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    self.pos += 1;
                }
            } else {
                // Not an exponent after all — `1e` is the number 1 followed by
                // garbage, and the garbage is the next token's problem.
                self.pos = save;
            }
        }
        let text = std::str::from_utf8(&self.src[start..self.pos])
            .ok()
            .and_then(|t| t.parse::<f64>().ok());
        match text {
            Some(v) if v.is_finite() => Ok(v),
            Some(_) => self.err("number is out of range"),
            None => self.err("expected a number"),
        }
    }

    /// An arc flag: exactly one `0` or `1`, with no separator required after it.
    fn flag(&mut self) -> Result<bool, VectorError> {
        self.skip_sep();
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                Ok(false)
            }
            Some(b'1') => {
                self.pos += 1;
                Ok(true)
            }
            _ => self.err("expected an arc flag (0 or 1)"),
        }
    }

    fn coord_pair(&mut self, relative: bool) -> Result<Point, VectorError> {
        let x = self.number()?;
        let y = self.number()?;
        Ok(if relative {
            self.cur + point(x, y)
        } else {
            point(x, y)
        })
    }

    /// `true` when the next token starts a number, i.e. the previous command
    /// repeats implicitly.
    fn more_args(&mut self) -> bool {
        self.skip_sep();
        matches!(self.peek(), Some(b) if b.is_ascii_digit() || b == b'.' || b == b'+' || b == b'-')
    }

    fn run(mut self) -> Result<Path, VectorError> {
        self.skip_wsp();
        if self.peek().is_none() {
            return Ok(self.path);
        }
        if !matches!(self.peek(), Some(b'M') | Some(b'm')) {
            return self.err("path data must begin with a moveto");
        }

        let mut cmd = 0u8;
        loop {
            self.skip_sep();
            let Some(b) = self.peek() else { break };
            if b.is_ascii_alphabetic() {
                cmd = b;
                self.pos += 1;
            } else if cmd == 0 || !self.more_args() {
                return self.err("expected a command letter");
            }
            self.exec(cmd)?;
            // A moveto's extra coordinate pairs are linetos, not movetos.
            cmd = match cmd {
                b'M' => b'L',
                b'm' => b'l',
                other => other,
            };
            // Nothing follows a `Z` unless a new command letter does.
            if matches!(cmd, b'Z' | b'z') {
                self.skip_sep();
                if self.peek().is_some_and(|b| !b.is_ascii_alphabetic()) {
                    return self.err("expected a command letter after closepath");
                }
            } else if !self.more_args() {
                // Next iteration must read a command letter.
                self.skip_sep();
                if self.peek().is_some_and(|b| !b.is_ascii_alphabetic()) {
                    return self.err("expected a command letter");
                }
            }
        }
        Ok(self.path)
    }

    fn exec(&mut self, cmd: u8) -> Result<(), VectorError> {
        let rel = cmd.is_ascii_lowercase();
        let (mut cubic, mut quad) = (None, None);
        match cmd.to_ascii_uppercase() {
            b'M' => {
                let p = self.coord_pair(rel)?;
                self.path.move_to(p);
                self.sub_start = p;
                self.cur = p;
            }
            b'L' => {
                let p = self.coord_pair(rel)?;
                self.path.line_to(p);
                self.cur = p;
            }
            b'H' => {
                let x = self.number()?;
                let p = point(if rel { self.cur.x + x } else { x }, self.cur.y);
                self.path.line_to(p);
                self.cur = p;
            }
            b'V' => {
                let y = self.number()?;
                let p = point(self.cur.x, if rel { self.cur.y + y } else { y });
                self.path.line_to(p);
                self.cur = p;
            }
            b'C' => {
                let c1 = self.coord_pair(rel)?;
                let c2 = self.coord_pair(rel)?;
                let p = self.coord_pair(rel)?;
                self.path.curve_to(c1, c2, p);
                cubic = Some(c2);
                self.cur = p;
            }
            b'S' => {
                let c2 = self.coord_pair(rel)?;
                let p = self.coord_pair(rel)?;
                // The implied first control point is the previous one
                // reflected; with no previous cubic it is the current point.
                let c1 = self.last_cubic.map_or(self.cur, |c| self.cur * 2.0 - c);
                self.path.curve_to(c1, c2, p);
                cubic = Some(c2);
                self.cur = p;
            }
            b'Q' => {
                let c = self.coord_pair(rel)?;
                let p = self.coord_pair(rel)?;
                self.path.quad_to(c, p);
                quad = Some(c);
                self.cur = p;
            }
            b'T' => {
                let p = self.coord_pair(rel)?;
                let c = self.last_quad.map_or(self.cur, |q| self.cur * 2.0 - q);
                self.path.quad_to(c, p);
                quad = Some(c);
                self.cur = p;
            }
            b'A' => {
                let rx = self.number()?;
                let ry = self.number()?;
                let rot = self.number()?;
                let large = self.flag()?;
                let sweep = self.flag()?;
                let p = self.coord_pair(rel)?;
                self.path.arc_to(rx, ry, rot.to_radians(), large, sweep, p);
                self.cur = p;
            }
            b'Z' => {
                self.path.close();
                self.cur = self.sub_start;
            }
            _ => return self.err("unknown command"),
        }
        self.last_cubic = cubic;
        self.last_quad = quad;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(p: &Path, n: usize) -> Vec<Point> {
        (0..=n)
            .filter_map(|i| p.point_at(i as f64 / n as f64))
            .collect()
    }

    fn same_geometry(a: &Path, b: &Path, eps: f64) {
        assert_eq!(
            a.elements().len(),
            b.elements().len(),
            "different element counts:\n{}\n{}",
            to_svg(a),
            to_svg(b)
        );
        for (i, (x, y)) in sample(a, 200).iter().zip(sample(b, 200).iter()).enumerate() {
            assert!(x.distance(*y) < eps, "sample {i}: {x:?} vs {y:?}");
        }
        let (ba, bb) = (a.bounds(), b.bounds());
        assert!(ba.min.distance(bb.min) < eps && ba.max.distance(bb.max) < eps);
        assert!((a.length() - b.length()).abs() < eps);
    }

    #[test]
    fn a_round_trip_preserves_the_geometry() {
        let d = "M 10 20 L 30 40 H 50 V 60 \
                 C 70 80 90 100 110 120 S 130 140 150 160 \
                 Q 170 180 190 200 T 210 220 \
                 A 25 25 -30 0 1 250 230 Z";
        let p1 = parse(d).unwrap();
        let text = to_svg_with_precision(&p1, 12);
        let p2 = parse(&text).unwrap();
        same_geometry(&p1, &p2, 1e-9);
        // And the written form is itself stable.
        assert_eq!(text, to_svg_with_precision(&p2, 12));
        // The default precision is still far tighter than any raster tolerance.
        same_geometry(&p1, &parse(&to_svg(&p1)).unwrap(), 1e-4);
    }

    /// The `A` command's third number is an angle in **degrees**, and
    /// [`Path::arc_to`] takes radians. Nothing else in the parser converts
    /// units, so if that one call loses its `to_radians` every rotated ellipse
    /// in every file silently becomes a differently rotated one — and with
    /// equal radii, which is what most arcs in the wild have, the mistake is
    /// invisible. This pins it with `rx != ry`, where it is not.
    #[test]
    fn an_arcs_rotation_is_read_as_degrees_and_stored_as_radians() {
        let parsed = parse("M0 0 A50 20 45 0 1 60 0").unwrap();

        let mut radians = Path::new();
        radians.move_to(point(0.0, 0.0)).arc_to(
            50.0,
            20.0,
            std::f64::consts::FRAC_PI_4,
            false,
            true,
            point(60.0, 0.0),
        );
        assert_eq!(parsed, radians, "45 degrees is FRAC_PI_4");

        // Taking the attribute at face value would be a rotation of 45
        // radians - a real angle, and a visibly different arc.
        let mut degrees_as_radians = Path::new();
        degrees_as_radians.move_to(point(0.0, 0.0)).arc_to(
            50.0,
            20.0,
            45.0,
            false,
            true,
            point(60.0, 0.0),
        );
        assert_ne!(parsed, degrees_as_radians);
        let a = parsed.point_at(0.5).unwrap();
        let b = degrees_as_radians.point_at(0.5).unwrap();
        assert!(a.distance(b) > 1.0, "{a:?} vs {b:?}");

        // And the conversion survives the round trip through the writer.
        same_geometry(
            &parsed,
            &parse(&to_svg_with_precision(&parsed, 12)).unwrap(),
            1e-9,
        );
    }

    #[test]
    fn relative_commands_mean_the_same_as_their_absolute_forms() {
        let abs = parse("M10 10 L20 10 L20 20 C20 30 30 30 30 20 Q30 10 40 10 Z").unwrap();
        let rel = parse("m10 10 l10 0 l0 10 c0 10 10 10 10 0 q0 -10 10 -10 z").unwrap();
        assert_eq!(abs, rel);
    }

    #[test]
    fn a_repeated_command_letter_may_be_omitted() {
        // "M x y x y x y" is a moveto followed by *linetos*, which is the rule
        // most hand-written parsers get wrong.
        let implicit = parse("M0 0 10 0 10 10").unwrap();
        let explicit = parse("M0 0 L10 0 L10 10").unwrap();
        assert_eq!(implicit, explicit);
        assert_eq!(implicit.subpaths().len(), 1);

        let curves = parse("M0 0C1 2 3 4 5 6 7 8 9 10 11 12").unwrap();
        assert_eq!(curves.elements().len(), 3);
        assert!(matches!(curves.elements()[2], PathEl::CurveTo(..)));
    }

    #[test]
    fn the_smooth_commands_reflect_the_previous_control_point() {
        let smooth = parse("M0 0 C0 10 10 10 10 0 S20 -10 20 0").unwrap();
        // The reflection of (10,10) about (10,0) is (10,-10).
        assert_eq!(
            smooth.elements()[2],
            PathEl::CurveTo(point(10.0, -10.0), point(20.0, -10.0), point(20.0, 0.0))
        );
        // With no preceding cubic, the implied control point is the current one.
        let lone = parse("M5 5 S10 10 15 5").unwrap();
        assert_eq!(
            lone.elements()[1],
            PathEl::CurveTo(point(5.0, 5.0), point(10.0, 10.0), point(15.0, 5.0))
        );
        // Same rule for quadratics.
        let t = parse("M0 0 Q5 10 10 0 T20 0").unwrap();
        assert_eq!(
            t.elements()[2],
            PathEl::QuadTo(point(15.0, -10.0), point(20.0, 0.0))
        );
        // A `T` after a non-quadratic uses the current point.
        let t2 = parse("M0 0 L10 0 T20 0").unwrap();
        assert_eq!(
            t2.elements()[2],
            PathEl::QuadTo(point(10.0, 0.0), point(20.0, 0.0))
        );
    }

    #[test]
    fn number_syntax_covers_what_real_files_contain() {
        let p = parse("M.5-.5 L1e2 1E-2 l+3.5,4. 5 6").unwrap();
        assert_eq!(p.elements()[0], PathEl::MoveTo(point(0.5, -0.5)));
        assert_eq!(p.elements()[1], PathEl::LineTo(point(100.0, 0.01)));
        assert_eq!(p.elements()[2], PathEl::LineTo(point(103.5, 4.01)));
        // implicit repeat of the relative lineto
        assert_eq!(p.elements()[3], PathEl::LineTo(point(108.5, 10.01)));
    }

    #[test]
    fn arc_flags_may_be_packed_against_their_neighbours() {
        // "a25 25 -30 0130 20" is rx=25 ry=25 rot=-30 large=0 sweep=1 x=30 y=20.
        let packed = parse("M0 0a25 25 -30 0130 20").unwrap();
        let spaced = parse("M0 0 a 25 25 -30 0 1 30 20").unwrap();
        assert_eq!(packed, spaced);
        assert_eq!(packed.current_point(), Some(point(30.0, 20.0)));
        assert!(packed.elements().len() > 1);
    }

    #[test]
    fn close_returns_the_pen_to_the_subpath_start() {
        let p = parse("M10 10 L20 10 Z L30 30").unwrap();
        let subs = p.subpaths();
        assert_eq!(subs.len(), 2);
        assert!(subs[0].closed);
        assert_eq!(subs[1].start, point(10.0, 10.0));
        assert_eq!(subs[1].end(), point(30.0, 30.0));
        // and a relative command after Z is relative to that start
        let r = parse("M10 10 L20 10 Z l5 5").unwrap();
        assert_eq!(r.current_point(), Some(point(15.0, 15.0)));
    }

    #[test]
    fn every_command_letter_is_understood() {
        let d = "M0 0 m1 1 L2 2 l1 1 H5 h1 V7 v1 \
                 C8 8 9 9 10 10 c1 1 2 2 3 3 S14 14 15 15 s1 1 2 2 \
                 Q18 18 19 19 q1 1 2 2 T23 23 t1 1 A2 2 0 0 0 26 26 a2 2 0 1 1 28 28 Z";
        let p = parse(d).unwrap();
        assert!(p.is_finite());
        assert!(p.length() > 0.0);
        assert_eq!(
            p.current_point(),
            Some(point(1.0, 1.0)),
            "Z returns to the last M"
        );
    }

    #[test]
    fn an_empty_or_blank_string_is_an_empty_path_not_an_error() {
        assert!(parse("").unwrap().is_empty());
        assert!(parse("   \t\r\n ").unwrap().is_empty());
        assert_eq!(to_svg(&Path::new()), "");
    }

    #[test]
    fn malformed_data_is_an_error_with_an_offset_not_a_panic() {
        let cases = [
            "L10 10",              // must start with a moveto
            "M10",                 // truncated coordinate pair
            "M0 0 C1 2 3 4",       // truncated curve
            "M0 0 A1 1 0 5 1 2 2", // 5 is not a flag
            "M0 0 X9 9",           // unknown command
            "M0 0 L1e999 1",       // out of range
            "M0 0 L,",             // separator with no number
            "M0 0 Z 5 5",          // arguments after a closepath
        ];
        for d in cases {
            match parse(d) {
                Err(VectorError::Svg { offset, reason }) => {
                    assert!(offset <= d.len(), "{d:?} reported offset {offset}");
                    assert!(!reason.is_empty());
                }
                other => panic!("{d:?} should not have parsed: {other:?}"),
            }
        }
    }

    #[test]
    fn a_serialised_path_is_compact_and_reparsable() {
        let mut p = Path::new();
        p.move_to(point(0.0, -0.0))
            .line_to(point(1.5, 2.0))
            .quad_to(point(3.0, 4.0), point(5.0, 6.0))
            .curve_to(point(7.0, 8.0), point(9.0, 10.0), point(11.0, 12.0))
            .close();
        let s = to_svg(&p);
        assert_eq!(s, "M0 0 L1.5 2 Q3 4 5 6 C7 8 9 10 11 12 Z");
        assert_eq!(parse(&s).unwrap(), p);
        // Non-finite coordinates are written as 0 rather than as `NaN`, which
        // would make the whole string unparsable.
        let mut bad = Path::new();
        bad.move_to(point(f64::NAN, f64::INFINITY));
        assert_eq!(to_svg(&bad), "M0 0");
        assert!(parse(&to_svg(&bad)).is_ok());
    }

    #[test]
    fn a_shape_primitive_survives_a_trip_through_svg() {
        // The migration path off `ShapeLayer::path_svg` in one test.
        use crate::point::Bounds;
        for shape in [
            crate::shapes::rect(Bounds::from_xywh(3.0, 4.0, 20.0, 10.0)),
            crate::shapes::rounded_rect(
                Bounds::from_xywh(0.0, 0.0, 40.0, 30.0),
                crate::shapes::CornerRadii::new(2.0, 4.0, 6.0, 8.0),
            ),
            crate::shapes::ellipse(point(50.0, 50.0), point(30.0, 20.0)),
            crate::shapes::star(point(0.0, 0.0), 20.0, 8.0, 5, 0.3),
            crate::shapes::arrow(
                point(0.0, 0.0),
                point(60.0, 20.0),
                crate::shapes::ArrowStyle::default(),
            ),
        ] {
            let back = parse(&to_svg_with_precision(&shape, 12)).unwrap();
            same_geometry(&shape, &back, 1e-8);
        }
    }
}
