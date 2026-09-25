//! W13X-5: Filter ▸ Render ▸ Flame follows the active path.
//!
//! Photopea's Flame burns along the current path and, with none, stops with
//! "Make a path first". Here the path is the Paths panel's current one (its
//! selected path, else the Work Path) that the menu context parks every
//! frame in document space ([`crate::menu_bridge::current_vector_path`]),
//! flattened to polylines in document pixels, which are the filtered
//! buffer's pixels (the Filter dialog previews, and a run filters, a
//! document-sized buffer of the layer).
//!
//! * [`active_path`] reads it; `None` is the refusal ([`NO_PATH`]).
//! * A Flame smart filter stores the path it was made with: [`encode`]
//!   writes it into the entry's parameter map under `path.` keys (the map is
//!   the one thing a stored smart filter carries), and [`decode_stored`] /
//!   [`decode_dialog`] read it back for the render and for a re-edit, so a
//!   later change to the Paths panel does not move a stored flame.

use std::collections::BTreeMap;

use filters::flame::FlamePath;

/// Photopea's refusal.
pub(crate) const NO_PATH: &str = filters::flame::NO_PATH;

/// Flattening tolerance, in document pixels.
const TOLERANCE: f64 = 0.25;

/// The key prefix a stored Flame's path lives under.
const PREFIX: &str = "path.";

/// Whether `id` is the path-drawn filter.
pub(crate) fn is_flame(id: ui::menu::FilterId) -> bool {
    id == ui::menu::FilterId::Flame
}

/// The current path's subpaths flattened in document pixels, or `None` when
/// there is no path with any length.
pub(crate) fn active_path() -> Option<Vec<FlamePath>> {
    path_of_svg(&crate::menu_bridge::current_vector_path()?)
}

/// SVG path data (document space) flattened to the renderer's polylines.
pub(crate) fn path_of_svg(svg: &str) -> Option<Vec<FlamePath>> {
    let path = vector::parse_svg(svg).ok()?;
    let out: Vec<FlamePath> = path
        .flatten(TOLERANCE)
        .into_iter()
        .map(|p| FlamePath {
            points: p.points.iter().map(|q| [q.x as f32, q.y as f32]).collect(),
            closed: p.closed,
        })
        .filter(|p| p.points.len() >= 2 && has_length(&p.points))
        .collect();
    (!out.is_empty()).then_some(out)
}

fn has_length(points: &[[f32; 2]]) -> bool {
    points.windows(2).any(|w| w[0] != w[1])
}

fn closed_key(sub: usize) -> String {
    format!("{PREFIX}{sub:03}.closed")
}

fn chunk_key(sub: usize, chunk: usize) -> String {
    format!("{PREFIX}{sub:03}.{chunk:05}")
}

/// The path as smart-filter parameters: per subpath a `closed` flag and its
/// points two to an entry (`[x0, y0, x1, y1]`; a lone last point pads with
/// NaN).
pub(crate) fn encode(path: &[FlamePath]) -> BTreeMap<String, layer_model::SmartParam> {
    let mut out = BTreeMap::new();
    for (sub, p) in path.iter().enumerate() {
        out.insert(closed_key(sub), layer_model::SmartParam::Bool(p.closed));
        for (chunk, pair) in p.points.chunks(2).enumerate() {
            let b = pair.get(1).copied().unwrap_or([f32::NAN, f32::NAN]);
            out.insert(
                chunk_key(sub, chunk),
                layer_model::SmartParam::Color([pair[0][0], pair[0][1], b[0], b[1]]),
            );
        }
    }
    out
}

/// Rebuild a path from `path.` entries, in key order.
fn decode<'a>(entries: impl Iterator<Item = (&'a str, Entry)>) -> Option<Vec<FlamePath>> {
    let mut subs: BTreeMap<usize, FlamePath> = BTreeMap::new();
    for (key, entry) in entries {
        let Some(rest) = key.strip_prefix(PREFIX) else {
            continue;
        };
        let Some((sub, tail)) = rest.split_once('.') else {
            continue;
        };
        let Ok(sub) = sub.parse::<usize>() else {
            continue;
        };
        let slot = subs.entry(sub).or_insert_with(|| FlamePath {
            points: Vec::new(),
            closed: false,
        });
        match (tail, entry) {
            ("closed", Entry::Flag(closed)) => slot.closed = closed,
            (_, Entry::Pair(c)) => {
                for p in [[c[0], c[1]], [c[2], c[3]]] {
                    if p[0].is_finite() && p[1].is_finite() {
                        slot.points.push(p);
                    }
                }
            }
            _ => {}
        }
    }
    let out: Vec<FlamePath> = subs
        .into_values()
        .filter(|p| p.points.len() >= 2 && has_length(&p.points))
        .collect();
    (!out.is_empty()).then_some(out)
}

enum Entry {
    Flag(bool),
    Pair([f32; 4]),
    Other,
}

/// The path a stored Flame smart filter was made with.
pub(crate) fn decode_stored(
    params: &BTreeMap<String, layer_model::SmartParam>,
) -> Option<Vec<FlamePath>> {
    decode(params.iter().map(|(k, v)| {
        let entry = match v {
            layer_model::SmartParam::Bool(b) => Entry::Flag(*b),
            layer_model::SmartParam::Color(c) => Entry::Pair(*c),
            _ => Entry::Other,
        };
        (k.as_str(), entry)
    }))
}

/// The same, from the stored parameters a re-edit dialog is seeded with.
pub(crate) fn decode_dialog(
    stored: &[(String, ui::dialogs::ParamValue)],
) -> Option<Vec<FlamePath>> {
    decode(stored.iter().map(|(k, v)| {
        let entry = match v {
            ui::dialogs::ParamValue::Bool(b) => Entry::Flag(*b),
            ui::dialogs::ParamValue::Color(c) => Entry::Pair(*c),
            _ => Entry::Other,
        };
        (k.as_str(), entry)
    }))
}

/// `invocation` ready to run: a Flame without a path takes the current one,
/// or is refused with Photopea's "Make a path first". Every other filter is
/// returned as it is.
pub(crate) fn with_active_path(
    invocation: &ui::dialogs::FilterInvocation,
) -> Result<std::borrow::Cow<'_, ui::dialogs::FilterInvocation>, String> {
    if !is_flame(invocation.filter.id) || invocation.params.path().is_some() {
        return Ok(std::borrow::Cow::Borrowed(invocation));
    }
    let path = active_path().ok_or_else(|| NO_PATH.to_string())?;
    Ok(std::borrow::Cow::Owned(ui::dialogs::FilterInvocation {
        filter: invocation.filter,
        params: invocation.params.clone().with_path(path),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_round_trips_through_the_smart_filter_map() {
        let path = path_of_svg("M4 20 L44 20 L30 5 M2 2 L10 2 L10 10 Z").expect("a path");
        assert_eq!(path.len(), 2);
        assert!(!path[0].closed && path[1].closed);
        let stored = encode(&path);
        assert_eq!(decode_stored(&stored), Some(path.clone()));
        let dialog: Vec<(String, ui::dialogs::ParamValue)> = stored
            .iter()
            .map(|(k, v)| {
                let v = match v {
                    layer_model::SmartParam::Bool(b) => ui::dialogs::ParamValue::Bool(*b),
                    layer_model::SmartParam::Color(c) => ui::dialogs::ParamValue::Color(*c),
                    _ => unreachable!(),
                };
                (k.clone(), v)
            })
            .collect();
        assert_eq!(decode_dialog(&dialog), Some(path));
    }

    #[test]
    fn no_path_or_a_dot_is_no_path() {
        assert_eq!(path_of_svg(""), None);
        assert_eq!(path_of_svg("M5 5 L5 5"), None);
        assert_eq!(decode_stored(&BTreeMap::new()), None);
    }

    use crate::dialog_host::{ActiveDialog, DialogHost};
    use crate::dialogs::ScriptedDialogs;
    use crate::editor::Editor;
    use crate::menu_bridge::{context, perform, run_filter_invocation};
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use ui::menu::{FilterId, MenuAction};

    const W: u32 = 128;
    const H: u32 = 96;

    /// A black 128x96 document.
    fn black_doc(dir: &std::path::Path) -> Editor {
        let png = dir.join("black.png");
        let mut rgba = vec![0u8; (W * H * 4) as usize];
        for px in rgba.chunks_mut(4) {
            px[3] = 255;
        }
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, W, H, &rgba).unwrap(),
        )
        .unwrap();
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        ed.open_path(&png).unwrap();
        ed
    }

    /// The Paths panel holding `svg` as its Work Path, parked by the menu
    /// context the way every frame parks it.
    fn paths_panel(ed: &mut Editor, svg: Option<&str>) {
        let mut ws = ui::Workspace::new();
        ws.paths.work_path = svg.map(|s| vector::parse_svg(s).unwrap());
        let _ = context(ed, &ws);
    }

    fn composite(ed: &mut Editor) -> Vec<u8> {
        ed.active_mut()
            .unwrap()
            .composite(raster::PixelRect::new(0, 0, W, H))
            .unwrap()
    }

    /// Pixels that differ, as (x, y).
    fn changed(a: &[u8], b: &[u8]) -> Vec<(u32, u32)> {
        (0..W * H)
            .filter(|i| {
                let i = (*i * 4) as usize;
                a[i..i + 4] != b[i..i + 4]
            })
            .map(|i| (i % W, i / W))
            .collect()
    }

    /// Distance from (x, y) to the segment (10, 80)-(60, 80).
    fn off_path(x: u32, y: u32) -> f32 {
        off_path_at(x, y, 80.0)
    }

    /// Distance from (x, y) to the segment (10, row)-(60, row).
    fn off_path_at(x: u32, y: u32, row: f32) -> f32 {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let dx = if px < 10.0 {
            10.0 - px
        } else if px > 60.0 {
            px - 60.0
        } else {
            0.0
        };
        (dx * dx + (py - row).powi(2)).sqrt()
    }

    /// Filter > Render > Flame, opened from the menu over a document whose
    /// Paths panel holds a path, previews and confirms along THAT path: the
    /// pixels near it burn, and none far from it change. One undo step.
    fn open_flame(ed: &mut Editor) -> ui::dialogs::FilterInvocation {
        let mut host = DialogHost::default();
        assert!(
            host.open_for_menu_action(&MenuAction::Filter(FilterId::Flame), ed),
            "Flame opened no dialog over a path"
        );
        let ActiveDialog::Filter(dialog) = host.active_for_test() else {
            panic!("not the filter dialog");
        };
        let mut invocation = dialog.invocation();
        assert!(
            invocation.params.path().is_some(),
            "the dialog has the path"
        );
        assert!(invocation
            .params
            .set("width", ui::dialogs::ParamValue::Int(10)));
        invocation
    }

    #[test]
    fn w13x5_flame_burns_along_the_paths_panel_path_and_refuses_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = black_doc(dir.path());
        let before = composite(&mut ed);

        // No path: Photopea's refusal, and no dialog.
        paths_panel(&mut ed, None);
        let mut host = DialogHost::default();
        assert!(!host.open_for_menu_action(&MenuAction::Filter(FilterId::Flame), &ed));
        assert_eq!(
            perform(MenuAction::Filter(FilterId::Flame), &mut ed),
            Err("Make a path first".to_string())
        );
        assert_eq!(composite(&mut ed), before, "the refusal changed nothing");

        // A path in the Paths panel.
        paths_panel(&mut ed, Some("M10 80 L60 80"));
        let depth = ed.active().unwrap().history.undo_depth();
        let invocation = open_flame(&mut ed);
        run_filter_invocation(&mut ed, &invocation).expect("burned");
        assert_eq!(ed.active().unwrap().history.undo_depth(), depth + 1);
        let lit = changed(&before, &composite(&mut ed));
        assert!(
            lit.iter().filter(|(x, y)| off_path(*x, *y) < 4.0).count() > 60,
            "the path burns"
        );
        let far: Vec<_> = lit
            .iter()
            .filter(|(x, y)| off_path(*x, *y) > 16.0)
            .collect();
        assert!(far.is_empty(), "far from the path but lit: {far:?}");
    }

    /// As a smart filter the path is captured: the entry stores it, renders
    /// along it, and a later change of the Paths panel does not move it.
    #[test]
    fn w13x5_a_flame_smart_filter_keeps_the_path_it_was_made_with() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = black_doc(dir.path());
        perform(MenuAction::ConvertForSmartFilters, &mut ed).unwrap();
        let before = composite(&mut ed);
        paths_panel(&mut ed, Some("M10 80 L60 80"));
        let invocation = open_flame(&mut ed);
        let status = run_filter_invocation(&mut ed, &invocation).expect("added");
        assert!(status.contains("smart filter"), "{status}");
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let stored = match &ed
            .active()
            .unwrap()
            .document
            .layers
            .get(layer)
            .unwrap()
            .kind
        {
            layer_model::LayerKind::SmartObject(so) => so.filters[0].clone(),
            other => panic!("not a smart object: {other:?}"),
        };
        assert_eq!(
            decode_stored(&stored.params),
            invocation.params.path().map(<[FlamePath]>::to_vec),
            "the entry stores the path"
        );
        let burned = composite(&mut ed);
        let lit = changed(&before, &burned);
        assert!(lit.iter().filter(|(x, y)| off_path(*x, *y) < 4.0).count() > 60);
        assert!(lit.iter().all(|(x, y)| off_path(*x, *y) <= 16.0));
        // Another path in the panel: the stored flame stays where it was.
        paths_panel(&mut ed, Some("M10 20 L60 20"));
        assert_eq!(composite(&mut ed), burned);
        // The stored entry, run afresh by the application's own runner (no
        // cached render) while the panel holds the NEW path, still burns
        // along the path it was made with and not along the panel's.
        assert!(
            active_path().is_some(),
            "the panel's new path is live while the entry re-runs"
        );
        let src = filters::FilterBuffer::from_rgba8(W, H, &before).unwrap();
        let rerun = crate::menu_bridge::run_smart_filter(&stored, &src)
            .expect("the stored flame runs")
            .to_rgba8();
        let lit = changed(&before, &rerun);
        assert!(
            lit.iter().filter(|(x, y)| off_path(*x, *y) < 4.0).count() > 60,
            "the re-run burns along the path it was made with"
        );
        let near_new: Vec<_> = lit
            .iter()
            .filter(|(x, y)| off_path_at(*x, *y, 20.0) < 8.0)
            .collect();
        assert!(
            near_new.is_empty(),
            "the re-run followed the panel's new path: {near_new:?}"
        );
    }
}
