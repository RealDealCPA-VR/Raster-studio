//! The design-system gate for `ui::dialogs`.
//!
//! `no_hardcoded_style.rs` deliberately skips this directory and says it has
//! its own gate. This is it, and it is the same rule: nothing under
//! `src/dialogs` may name a colour, a font, a radius, a gap or a stroke width
//! as a literal, because the claim that a re-skin is an edit to the `design`
//! crate has to be true of the dialogs too.
//!
//! # The one difference: an explicit exemption
//!
//! A colour picker cannot be re-skinned all the way down. The saturation/value
//! square *is* white-to-hue-to-black — that is the definition of the model its
//! numeric fields report — and the marker dragged around on top of it has to
//! read against a colour the user chose, so it cannot be any palette entry at
//! all. Rather than pretend otherwise, a line may carry a
//! `// design-exempt: <reason>` comment. The gate then requires the reason to
//! be there and to say something, so an exemption is a sentence somebody had to
//! write rather than a lint suppression nobody reads.

use std::path::{Path, PathBuf};

/// Patterns that mean "a style value was written literally", and the fix.
const FORBIDDEN: &[(&str, &str)] = &[
    ("Color32::WHITE", "use design::color32 with a ColorRole"),
    ("Color32::BLACK", "use design::color32 with a ColorRole"),
    ("Color32::RED", "use design::color32 with a ColorRole"),
    ("Color32::GREEN", "use design::color32 with a ColorRole"),
    ("Color32::BLUE", "use design::color32 with a ColorRole"),
    ("Color32::GRAY", "use design::color32 with a ColorRole"),
    ("Color32::from_rgb(", "use design::color32 with a ColorRole"),
    (
        "Color32::from_gray(",
        "use design::color32 with a ColorRole",
    ),
    (
        "FontId::new(",
        "use design::egui_theme::font_id with a TypeRole",
    ),
    ("FontId::proportional(", "use design::egui_theme::font_id"),
    ("FontId::monospace(", "use design::egui_theme::font_id"),
    (
        "Rounding::same(",
        "use design::egui_theme::rounding with a Radius",
    ),
    (
        "TextStyle::Heading",
        "use design::egui_theme::font_id with a TypeRole",
    ),
];

/// Calls whose first argument must be a token, never a number.
///
/// Gaps and stroke widths were the original list. Sizes were not, and that gap
/// was worse than having no gate: roughly thirty-four literal dimensions sat in
/// shipping dialog code — `ui.set_width(240.0)`, `vec2(48.0, 24.0)`,
/// `.max_height(320.0)`, and a literal corner *radius* passed to
/// `checkerboard` — while this file reported the design-system rule as held.
/// Every one of them now resolves through `design::tokens::grid` by way of
/// `dialogs::sizes`, and the gate says so.
const FORBIDDEN_NUMERIC: &[(&str, &str)] = &[
    ("add_space(", "use a design::Space rung"),
    (
        "Margin::same(",
        "use a design::Space rung or a Metrics field",
    ),
    ("Margin::symmetric(", "use a design::Space rung"),
    ("Stroke::new(", "use design's BorderWidths"),
    (
        "set_width(",
        "use a dialogs::sizes extent or a Metrics field",
    ),
    (
        "set_height(",
        "use a dialogs::sizes extent or a Metrics field",
    ),
    ("desired_width(", "use a dialogs::sizes text-field width"),
    ("max_height(", "use a dialogs::sizes extent"),
    ("max_width(", "use a dialogs::sizes extent"),
    ("vec2(", "use a dialogs::sizes extent"),
    (
        ".a = ",
        "an opacity is a design decision — name it, or take it from a palette role",
    ),
    (
        "checkerboard(ui, rect, ",
        "use Radius::*.resolve, never a literal corner radius",
    ),
];

/// Calls that *defend* a dimension: `.max(…)` / `.min(…)` around a size.
///
/// These cannot use the rule above — "any digit after the call is a violation" —
/// because `.max(1.0)` is also how a body guards a division by a zero-width
/// rectangle and `.min(1.0)` is how a preview's scale factor is capped. That is
/// arithmetic, not style, and forcing it through `sizes` would be nonsense.
///
/// The line between the two is the grid itself. A bare float below one grid
/// unit is a ratio; a bare float at or above one grid unit is a length in
/// points, and a length in points belongs in [`dialogs::sizes`] or in `design`'s
/// `Metrics`. That is exactly what the two lines this rule was written for were
/// doing: `combo_width.max(120.0)` and `interact_size.y.max(16.0)` both pinned a
/// control's minimum size — a design decision — where nothing could re-scale it.
///
/// Integers are left alone: `states.max(1)`, `worker_threads.min(256)` and
/// `autosave_minutes.min(24 * 60)` are preference ranges, not points.
const FORBIDDEN_DIMENSION_GUARD: &[(&str, &str)] = &[
    (
        "max(",
        "use a dialogs::sizes extent or a design Metrics field",
    ),
    (
        "min(",
        "use a dialogs::sizes extent or a design Metrics field",
    ),
];

/// One grid unit, in points. `design::tokens::UNIT_PT`, restated here because a
/// gate that imports the thing it is policing can be silenced by editing it.
const GRID_UNIT_PT: f32 = 4.0;

/// Calls whose first argument must not be a SCREAMING_CASE constant.
///
/// A constant is a literal with a name. `vec2(PREVIEW_SIZE.0 as f32,
/// PREVIEW_SIZE.1 as f32)` walked straight past the numeric rule — the character
/// after `vec2(` is a letter — while putting a brush preview's on-screen extent
/// outside `sizes` and therefore outside the on-grid invariant that covers every
/// other dialog extent. A point-space extent is `sizes`' to own, whether it is
/// spelled as a number or as a name.
const FORBIDDEN_CONST_EXTENT: &[(&str, &str)] = &[(
    "vec2(",
    "an on-screen extent belongs in dialogs::sizes, even when it has a name",
)];

/// Converting the *user's* own colour is not the design system's to choose.
const SANCTIONED: &[&str] = &["Color32::from_rgba_unmultiplied("];

/// The marker that opts one line out, with its reason.
const EXEMPT: &str = "// design-exempt:";

fn dialogs_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("dialogs")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The part of a file that ships: everything before its test module.
///
/// A test that paints black on white to check geometry is not a design
/// decision, and holding it to the gate would only push it into a helper.
fn shipping_source(path: &Path) -> String {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    match text.find("#[cfg(test)]") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// Everything before the first `//`, so a rule quoted in prose is not read as a
/// violation of itself.
fn without_comments(line: &str) -> &str {
    match line.find("//") {
        Some(at) => &line[..at],
        None => line,
    }
}

/// The reason on an exempt line, if the line is exempt.
fn exemption(line: &str) -> Option<&str> {
    line.find(EXEMPT).map(|at| line[at + EXEMPT.len()..].trim())
}

fn scan(check: impl Fn(&str) -> Vec<String>) -> Vec<String> {
    let mut files = Vec::new();
    rust_files(&dialogs_src(), &mut files);
    assert!(
        files.len() >= 10,
        "the dialogs module lost its source files: found {}",
        files.len()
    );

    let mut violations = Vec::new();
    for path in &files {
        for (number, line) in shipping_source(path).lines().enumerate() {
            if exemption(line).is_some() {
                continue;
            }
            let code = without_comments(line);
            if SANCTIONED.iter().any(|s| code.contains(s)) {
                continue;
            }
            for message in check(code) {
                violations.push(format!("{}:{}: {message}", path.display(), number + 1));
            }
        }
    }
    violations
}

#[test]
fn no_dialog_names_a_colour_a_font_or_a_radius_literally() {
    let violations = scan(|code| {
        FORBIDDEN
            .iter()
            .filter(|(pattern, _)| code.contains(pattern))
            .map(|(pattern, fix)| format!("`{pattern}` — {fix}"))
            .collect()
    });
    assert!(
        violations.is_empty(),
        "style values written literally in the dialogs:\n{}",
        violations.join("\n")
    );
}

/// Every place in `code` where `call` appears, as the offset just past it.
fn call_sites(code: &str, call: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(at) = code[from..].find(call) {
        let after = from + at + call.len();
        out.push(after);
        from = after;
    }
    out
}

/// The numeric literal starting at `at`, if there is one.
///
/// Returns the text as written (underscores kept) so the caller can tell an
/// integer from a float without re-scanning.
fn literal_at(code: &str, at: usize) -> Option<String> {
    let rest = code[at..].trim_start();
    let literal: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '_')
        .collect();
    literal
        .starts_with(|c: char| c.is_ascii_digit())
        .then_some(literal)
}

/// The identifier starting at `at`, if there is one.
fn identifier_at(code: &str, at: usize) -> Option<String> {
    let rest = code[at..].trim_start();
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    name.starts_with(|c: char| c.is_ascii_alphabetic())
        .then_some(name)
}

/// Whether `name` is a SCREAMING_CASE constant rather than a type or a local.
fn is_screaming_const(name: &str) -> bool {
    name.len() >= 2
        && name.chars().any(|c| c.is_ascii_alphabetic())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Every style rule that reads *what follows* a call, over one line of code.
///
/// One implementation, used by the scan over the real sources and by the
/// self-test that proves the rules fire — a gate whose self-test runs different
/// code from the gate is a gate that can pass while the real rule is broken.
fn style_hits(code: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (call, fix) in FORBIDDEN_NUMERIC {
        for at in call_sites(code, call) {
            if literal_at(code, at).is_some() {
                out.push((*call, format!("`{call}` with a literal number — {fix}")));
            }
        }
    }
    for (call, fix) in FORBIDDEN_DIMENSION_GUARD {
        for at in call_sites(code, call) {
            let Some(literal) = literal_at(code, at) else {
                continue;
            };
            // Integers are ranges, not points; only a float can be a length.
            if !literal.contains('.') {
                continue;
            }
            let value: f32 = match literal.replace('_', "").trim_end_matches('.').parse() {
                Ok(value) => value,
                Err(_) => continue,
            };
            if value >= GRID_UNIT_PT {
                out.push((*call, format!("`{call}{literal})` is a length — {fix}")));
            }
        }
    }
    for (call, fix) in FORBIDDEN_CONST_EXTENT {
        for at in call_sites(code, call) {
            if identifier_at(code, at).is_some_and(|name| is_screaming_const(&name)) {
                out.push((*call, format!("`{call}` with a named constant — {fix}")));
            }
        }
    }
    out
}

#[test]
fn no_dialog_writes_a_gap_a_stroke_width_or_a_length_as_a_bare_number() {
    let violations = scan(|code| style_hits(code).into_iter().map(|(_, m)| m).collect());
    assert!(
        violations.is_empty(),
        "spacing written as bare numbers in the dialogs:\n{}",
        violations.join("\n")
    );
}

#[test]
fn every_exemption_carries_a_reason() {
    let mut files = Vec::new();
    rust_files(&dialogs_src(), &mut files);
    let mut found = 0usize;
    let mut bare = Vec::new();
    for path in &files {
        for (number, line) in shipping_source(path).lines().enumerate() {
            if let Some(reason) = exemption(line) {
                found += 1;
                if reason.len() < 12 {
                    bare.push(format!(
                        "{}:{}: exemption reason is too thin: {reason:?}",
                        path.display(),
                        number + 1
                    ));
                }
            }
        }
    }
    assert!(bare.is_empty(), "{}", bare.join("\n"));
    // The exemptions that exist today are the colour picker's model colours and
    // the identity tint. If they all disappear, this gate has stopped being
    // exercised and the count below should be re-examined rather than deleted.
    assert!(
        found > 0,
        "no exemptions found — has the marker been renamed?"
    );
}

/// Run the size checks over one line of source, the way `scan` does.
fn numeric_hits(line: &str) -> Vec<&'static str> {
    style_hits(without_comments(line))
        .into_iter()
        .map(|(call, _)| call)
        .collect()
}

#[test]
fn the_size_rules_fire_on_the_code_they_were_written_for() {
    // These are the real lines that were in the shipping dialogs before the
    // sizes module existed. Each one must be caught, and its token-resolved
    // replacement must not be — otherwise the rule is either decorative or
    // unusable.
    let offenders = [
        ("                ui.set_width(280.0);", "set_width("),
        ("                    .max_height(320.0)", "max_height("),
        (
            "            ui.add(egui::TextEdit::singleline(&mut n).desired_width(200.0));",
            "desired_width(",
        ),
        ("        let field = vec2(240.0, 200.0);", "vec2("),
        (
            "                super::controls::checkerboard(ui, rect, 4.0);",
            "checkerboard(ui, rect, ",
        ),
        // The three the reviewer found, exactly as they were written.
        ("        .min(ui.spacing().combo_width.max(120.0));", "max("),
        (
            "        let handle = ui.spacing().interact_size.y.max(16.0);",
            "max(",
        ),
        (
            "        let size = vec2(PREVIEW_SIZE.0 as f32, PREVIEW_SIZE.1 as f32);",
            "vec2(",
        ),
        ("    shade.a = 150;", ".a = "),
    ];
    for (line, expected) in offenders {
        let hits = numeric_hits(line);
        assert!(
            hits.contains(&expected),
            "the gate missed {line:?}; it fired on {hits:?}"
        );
    }

    let fixed = [
        "                ui.set_width(sizes::preview_column_width());",
        "                    .max_height(sizes::list_max_height())",
        "            ui.add(egui::TextEdit::singleline(&mut n).desired_width(sizes::text_field_wide()));",
        "        let field = sizes::saturation_value_field();",
        "                super::controls::checkerboard(ui, rect, radius);",
        "    vec2(grid(12.0), grid(6.0))",
        "        .fixed_size(egui::vec2(width, 0.0))",
        "        .min(ui.spacing().combo_width.max(sizes::combo_min_width()));",
        "        let handle = ui.spacing().interact_size.y.max(t.metrics.min_hit_target);",
        "        let size = sizes::brush_stroke_preview();",
        "    shade.a = scrim_alpha(palette.is_dark());",
        // Arithmetic, not style: a ratio guard, a scale cap, a range clamp.
        // Holding these to the rule would only push real maths into a helper.
        "            let t = (pos.x - bar.left()) / bar.width().max(1.0);",
        "                let scale = (sizes::filter_preview_width() / size.x.max(1.0)).min(2.0);",
        "                (glow.size_px * 0.3).max(1.0),",
        "                                stroke.size_px.max(0.5),",
        "        self.performance.worker_threads = self.performance.worker_threads.min(256);",
        "        self.general.autosave_minutes = self.general.autosave_minutes.min(24 * 60);",
        "                self.prefs.history.states = states.max(1) as u32;",
        "            let t = ((pos.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 0.999_9);",
        "        let size = vec2(width, width * h as f32 / w.max(1) as f32);",
        "        ui.allocate_exact_size(vec2(width, bar_height + 2.0 * handle), Sense::hover());",
    ];
    for line in fixed {
        assert!(
            numeric_hits(line).is_empty(),
            "the gate rejects the token-resolved form {line:?}"
        );
    }
}

#[test]
fn the_length_guard_splits_lengths_from_ratios_at_the_grid() {
    // A float at or above one grid unit is points; below it is a ratio. The
    // boundary is the whole rule, so it is pinned from both sides.
    assert_eq!(numeric_hits("let x = a.max(4.0);"), vec!["max("]);
    assert!(numeric_hits("let x = a.max(3.99);").is_empty());
    assert!(numeric_hits("let x = a.max(4);").is_empty(), "an integer");
    assert_eq!(numeric_hits("let x = a.min(120.0);"), vec!["min("]);

    // A named constant is a literal that got a name.
    assert_eq!(numeric_hits("vec2(PREVIEW_SIZE.0, 1.0)"), vec!["vec2("]);
    assert!(numeric_hits("vec2(Space::Small.pt(), width)").is_empty());
    assert!(numeric_hits("vec2(width, height)").is_empty());
}

#[test]
fn the_gate_actually_catches_something() {
    // A gate nobody has seen fail is a gate nobody knows works.
    let bad = "        painter.rect_filled(rect, Rounding::same(4.0), Color32::WHITE);";
    let hits = FORBIDDEN.iter().filter(|(p, _)| bad.contains(p)).count();
    assert_eq!(hits, 2, "expected both patterns to fire");

    let numeric = "        ui.add_space(12.0);";
    let call = "add_space(";
    let after = numeric.find(call).unwrap() + call.len();
    assert!(numeric[after..]
        .trim_start()
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit()));

    let good = "        ui.add_space(Space::Small.pt());";
    let after = good.find(call).unwrap() + call.len();
    assert!(!good[after..]
        .trim_start()
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit()));

    // The exemption marker is honoured, and an empty one is not.
    assert_eq!(
        exemption("const X: Color32 = Color32::WHITE; // design-exempt: it is the identity tint"),
        Some("it is the identity tint")
    );
    assert_eq!(exemption("let a = 1;"), None);
    assert!(exemption("// design-exempt:").unwrap().len() < 12);
}
