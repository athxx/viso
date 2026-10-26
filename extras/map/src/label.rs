//! Labels a tile offers, worked out off the loop as the tile builds: each
//! symbol layer's text for its features in the reader's language, where it
//! sits in the tile and, along a line, which way it runs.
//!
//! Which of them show is decided on the loop, across tiles, when the camera
//! rests (`view::labels`).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::build::{EXTENT, FeatureRef, key_index};
use crate::mvt::{self, GeomType, Value};
use crate::style::{FontWeight, Paint, Placement, Properties, Shield, Style, TextField};

/// An icon a label shows, by its name in the view's icon set.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelIcon {
    pub name: Arc<str>,
    /// Straight linear RGBA.
    pub color: [f32; 4],
    /// Its side, in points.
    pub size: f32,
}

/// A label a tile offers.
#[derive(Debug, Clone, PartialEq)]
pub struct TileLabel {
    /// The style layer it comes from; later layers place first.
    pub layer: u16,
    /// Its rank within the layer: smaller places first.
    pub rank: f32,
    pub text: Arc<str>,
    /// The language its text is in, when a name was picked by language.
    pub lang: Option<Arc<str>>,
    /// Where it sits, in tile units.
    pub at: [f32; 2],
    /// Which way it runs along a line, in radians in the tile; `None` for a
    /// label that stays level.
    pub angle: Option<f32>,
    /// The stretch of its line it bends along glyph by glyph; `None` for a
    /// label set as one straight run.
    pub curve: Option<Curve>,
    /// The text's size, in points.
    pub size: f32,
    /// Straight linear RGBA.
    pub color: [f32; 4],
    pub halo: [f32; 4],
    /// The halo's width, in points.
    pub halo_width: f32,
    pub weight: FontWeight,
    pub icon: Option<LabelIcon>,
    pub shield: Option<Shield>,
    /// The space kept clear around it, in points.
    pub padding: f32,
    /// Its layer and text, the same in every tile that offers it.
    pub key: u64,
}

/// The stretch of a line a label bends along, and its text cut into the
/// clusters set one by one along it.
#[derive(Debug, Clone, PartialEq)]
pub struct Curve {
    /// The stretch's points, in tile units; the label centers on point `at`.
    pub points: Arc<[[f32; 2]]>,
    pub at: u16,
    pub clusters: Arc<[Cluster]>,
}

/// A character of a label's text and the marks riding on it: the byte range
/// set as one glyph along a curve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cluster {
    pub start: u16,
    pub end: u16,
}

impl Cluster {
    /// Its text in `text`.
    pub fn of<'a>(&self, text: &'a str) -> &'a str {
        &text[usize::from(self.start)..usize::from(self.end)]
    }
}

/// The most clusters a label bends along its line in; a longer one is set
/// straight.
pub const MAX_CLUSTERS: usize = 64;

/// Whether `c` rides on the character before it.
fn rides(c: char) -> bool {
    matches!(
        c as u32,
        0x0300..=0x036f
            | 0x0483..=0x0489
            | 0x1ab0..=0x1aff
            | 0x1dc0..=0x1dff
            | 0x200c..=0x200d
            | 0x20d0..=0x20ff
            | 0x3099..=0x309a
            | 0xfe00..=0xfe0f
            | 0xfe20..=0xfe2f
            | 0x1f3fb..=0x1f3ff
            | 0xe0020..=0xe007f
            | 0xe0100..=0xe01ef
    )
}

/// Whether `c` is of a script whose letters take their shape from their
/// neighbours or run right to left: one set glyph by glyph would read
/// wrong.
fn joined_script(c: char) -> bool {
    matches!(
        c as u32,
        0x0590..=0x08ff
            | 0x0900..=0x0dff
            | 0x0e00..=0x0fff
            | 0x1000..=0x109f
            | 0x1100..=0x11ff
            | 0x1700..=0x18af
            | 0x1900..=0x1cff
            | 0xa800..=0xa9ff
            | 0xaa00..=0xabff
            | 0xd7b0..=0xd7ff
            | 0xfb1d..=0xfdff
            | 0xfe70..=0xfeff
            | 0x10800..=0x10fff
            | 0x11000..=0x11fff
            | 0x1e800..=0x1efff
    )
}

/// `text` cut into clusters, or `None` when it cannot bend glyph by glyph:
/// empty, too long, or in a script set as a whole.
pub fn clusters(text: &str) -> Option<Arc<[Cluster]>> {
    if text.len() > usize::from(u16::MAX) {
        return None;
    }
    let mut out: Vec<Cluster> = Vec::new();
    let mut joiner = false;
    for (i, c) in text.char_indices() {
        if joined_script(c) {
            return None;
        }
        let end = (i + c.len_utf8()) as u16;
        match out.last_mut() {
            Some(last) if rides(c) || joiner => last.end = end,
            _ => out.push(Cluster {
                start: i as u16,
                end,
            }),
        }
        joiner = c == '\u{200d}';
    }
    (!out.is_empty() && out.len() <= MAX_CLUSTERS).then(|| out.into())
}

/// The stretch of `line` from `from` to `to` along it, with the point `at`
/// along it among its points, and that point's index.
fn stretch(line: &[[f32; 2]], from: f32, at: f32, to: f32) -> (Vec<[f32; 2]>, u16) {
    let mut out = vec![along(line, from)];
    let mut index = None;
    let push = |out: &mut Vec<[f32; 2]>, p: [f32; 2]| {
        if out.last() != Some(&p) {
            out.push(p);
        }
        (out.len() - 1) as u16
    };
    let mut d = 0.0;
    for w in line.windows(2) {
        let next = d + (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
        if index.is_none() && at < next {
            index = Some(push(&mut out, along(line, at)));
        }
        if next > from && next < to {
            push(&mut out, w[1]);
        }
        d = next;
    }
    let index = index.unwrap_or_else(|| push(&mut out, along(line, at)));
    push(&mut out, along(line, to));
    (out, index)
}

/// About how wide `text` is at `size` points: a full em for wide
/// characters, a little over half for the rest.
pub fn estimate_width(text: &str, size: f32) -> f32 {
    text.chars()
        .map(|c| {
            let u = c as u32;
            let wide = (0x1100..=0x115f).contains(&u)
                || (0x2e80..=0xa4cf).contains(&u)
                || (0xac00..=0xd7a3).contains(&u)
                || (0xf900..=0xfaff).contains(&u)
                || (0xff00..=0xff60).contains(&u);
            if wide { size } else { size * 0.58 }
        })
        .sum()
}

/// Each preferred language, then its primary subtag (`zh-Hant`, then
/// `zh`), once each.
pub fn expand_languages(languages: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in languages {
        let tag = tag.replace('_', "-");
        let mut parts: Vec<&str> = tag.split('-').collect();
        while !parts.is_empty() {
            let t = parts.join("-");
            if !out.contains(&t) {
                out.push(t);
            }
            parts.pop();
        }
    }
    out
}

/// `field`'s text for a feature, and the language it picked.
fn text_of<'a, 'l>(
    field: &TextField,
    props: &'a dyn Properties,
    languages: &'l [String],
) -> Option<(&'a str, Option<&'l str>)> {
    match field {
        TextField::Key(key) => props.get(key).and_then(Value::as_str).map(|s| (s, None)),
        TextField::Name { pattern, fallback } => {
            for lang in languages {
                let key = pattern.replace("{lang}", lang);
                if let Some(s) = props.get(&key).and_then(Value::as_str)
                    && !s.is_empty()
                {
                    return Some((s, Some(lang.as_str())));
                }
            }
            props
                .get(fallback)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|s| (s, None))
        }
    }
}

fn length(line: &[[f32; 2]]) -> f32 {
    line.windows(2)
        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
        .sum()
}

/// The point `d` along `line`.
fn along(line: &[[f32; 2]], mut d: f32) -> [f32; 2] {
    for w in line.windows(2) {
        let s = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
        if d <= s && s > 0.0 {
            let t = d / s;
            return [
                w[0][0] + (w[1][0] - w[0][0]) * t,
                w[0][1] + (w[1][1] - w[0][1]) * t,
            ];
        }
        d -= s;
    }
    *line.last().expect("points")
}

/// Whether `line` bends by less than `limit` radians between `from` and
/// `to` along it.
fn straight(line: &[[f32; 2]], from: f32, to: f32, limit: f32) -> bool {
    let mut d = 0.0;
    let mut first: Option<f32> = None;
    for w in line.windows(2) {
        let s = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
        if d + s >= from && d <= to && s > 0.0 {
            let a = (w[1][1] - w[0][1]).atan2(w[1][0] - w[0][0]);
            match first {
                None => first = Some(a),
                Some(f) => {
                    let mut turn = (a - f).abs() % std::f32::consts::TAU;
                    if turn > std::f32::consts::PI {
                        turn = std::f32::consts::TAU - turn;
                    }
                    if turn > limit {
                        return false;
                    }
                }
            }
        }
        d += s;
    }
    true
}

/// The middle of a ring by its area, or of its points for a sliver.
fn centroid(ring: &[[f32; 2]]) -> [f32; 2] {
    let (mut a, mut cx, mut cy) = (0.0f64, 0.0f64, 0.0f64);
    let n = ring.len();
    for i in 0..n {
        let (p, q) = (ring[i], ring[(i + 1) % n]);
        let cross = f64::from(p[0]) * f64::from(q[1]) - f64::from(q[0]) * f64::from(p[1]);
        a += cross;
        cx += (f64::from(p[0]) + f64::from(q[0])) * cross;
        cy += (f64::from(p[1]) + f64::from(q[1])) * cross;
    }
    if a.abs() < 1e-6 {
        let k = 1.0 / n.max(1) as f32;
        return ring
            .iter()
            .fold([0.0, 0.0], |c, p| [c[0] + p[0] * k, c[1] + p[1] * k]);
    }
    [(cx / (3.0 * a)) as f32, (cy / (3.0 * a)) as f32]
}

/// The most a line may turn under a label that bends with it, in radians:
/// sharper turns are left to the next repeat.
const BEND: f32 = 1.6;

fn inside_tile(p: [f32; 2]) -> bool {
    p[0] >= 0.0 && p[0] < EXTENT && p[1] >= 0.0 && p[1] < EXTENT
}

/// The labels `style`'s symbol layers offer from a tile's `layers` at
/// `zoom`, the tile `tile_points` across on screen, in the reader's
/// `languages` (expanded), into `out`.
pub fn extract(
    layers: &[&mvt::Layer],
    style: &Style,
    zoom: f32,
    tile_points: f32,
    languages: &[String],
    out: &mut Vec<TileLabel>,
) {
    let upp = EXTENT / tile_points.max(1.0);
    for (li, style_layer) in style.layers.iter().enumerate() {
        let Paint::Symbol(sym) = &style_layer.paint else {
            continue;
        };
        if !style_layer.source.is_empty() || !style_layer.shows_at(zoom) {
            continue;
        }
        let Some(field) = &sym.text else {
            continue;
        };
        let Some(layer) = layers.iter().find(|l| l.name == style_layer.source_layer) else {
            continue;
        };
        let keys = key_index(layer);
        let scale = EXTENT / layer.extent.max(1) as f32;
        for feature in &layer.features {
            let props = FeatureRef::new(layer, feature, &keys);
            if !style_layer.filter.test(&props) {
                continue;
            }
            let Some((text, lang)) = text_of(field, &props, languages) else {
                continue;
            };
            // A shield shows the first of a road's numbers.
            let text = match sym.shield {
                Some(_) => text.split(';').next().unwrap_or(text).trim(),
                None => text,
            };
            let text: Arc<str> = if sym.uppercase {
                text.to_uppercase().into()
            } else {
                text.into()
            };
            let size = sym.size.eval(zoom, &props);
            if size <= 0.0 {
                continue;
            }
            let Ok(geometry) = feature.geometry() else {
                continue;
            };
            let parts: Vec<Vec<[f32; 2]>> = geometry
                .parts()
                .map(|p| {
                    p.iter()
                        .map(|q| [q[0] as f32 * scale, q[1] as f32 * scale])
                        .collect()
                })
                .collect();
            let rank = sym
                .rank
                .as_ref()
                .and_then(|k| props.get(k))
                .and_then(Value::as_f64)
                .map_or(f32::MAX, |r| r as f32);
            let icon = sym.icon.as_ref().map(|name| LabelIcon {
                name: name.eval(zoom, &props).into(),
                color: sym.icon_color.eval(zoom, &props).linear(1.0),
                size: sym.icon_size,
            });
            let mut key = DefaultHasher::new();
            li.hash(&mut key);
            text.hash(&mut key);
            let label = |at: [f32; 2], angle: Option<f32>, curve: Option<Curve>| TileLabel {
                layer: li as u16,
                rank,
                text: Arc::clone(&text),
                lang: lang.map(Arc::from),
                at,
                angle,
                curve,
                size,
                color: sym.color.eval(zoom, &props).linear(1.0),
                halo: sym.halo.eval(zoom, &props).linear(1.0),
                halo_width: sym.halo_width.eval(zoom, &props),
                weight: sym.weight,
                icon: icon.clone(),
                shield: sym.shield,
                padding: sym.padding,
                key: key.finish(),
            };
            match (feature.kind, sym.placement) {
                (GeomType::Point, _) => {
                    for p in parts.iter().flatten() {
                        if inside_tile(*p) {
                            out.push(label(*p, None, None));
                        }
                    }
                }
                (GeomType::Polygon, _) => {
                    if let Some(ring) = parts.first().filter(|r| r.len() >= 3) {
                        let c = centroid(ring);
                        if inside_tile(c) {
                            out.push(label(c, None, None));
                        }
                    }
                }
                (GeomType::Line, Placement::Point) => {
                    if let Some(line) = parts.iter().max_by(|a, b| length(a).total_cmp(&length(b)))
                        && line.len() >= 2
                    {
                        let m = along(line, length(line) / 2.0);
                        if inside_tile(m) {
                            out.push(label(m, None, None));
                        }
                    }
                }
                (GeomType::Line, Placement::Line) => {
                    let width = (estimate_width(&text, size) + 2.0 * sym.padding) * upp;
                    let spacing = sym.spacing.max(1.0) * upp;
                    // A shield stays level where it repeats; other text bends
                    // with its line where its script allows.
                    let bends = sym.shield.is_none().then(|| clusters(&text)).flatten();
                    for line in parts.iter().filter(|l| l.len() >= 2) {
                        let len = length(line);
                        if len < width {
                            continue;
                        }
                        let count = (len / spacing).floor().max(1.0);
                        let step = len / count;
                        for i in 0..count as u32 {
                            let d = step * (i as f32 + 0.5);
                            let (from, to) = (d - width / 2.0, d + width / 2.0);
                            let limit = if bends.is_some() { BEND } else { 0.6 };
                            if from < 0.0 || to > len || !straight(line, from, to, limit) {
                                continue;
                            }
                            let m = along(line, d);
                            if !inside_tile(m) {
                                continue;
                            }
                            let (a, b) = (along(line, from), along(line, to));
                            let angle = sym
                                .shield
                                .is_none()
                                .then(|| (b[1] - a[1]).atan2(b[0] - a[0]));
                            // Room either side for its true width and the
                            // zoom it is shown at.
                            let curve = bends.as_ref().map(|c| {
                                let (points, at) =
                                    stretch(line, (d - width).max(0.0), d, (d + width).min(len));
                                Curve {
                                    points: points.into(),
                                    at,
                                    clusters: Arc::clone(c),
                                }
                            });
                            out.push(label(m, angle, curve));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // The order they place in: later layers first, then by rank.
    out.sort_by(|a, b| b.layer.cmp(&a.layer).then(a.rank.total_cmp(&b.rank)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::TileEncoder;
    use crate::style::{Color, Layer, Prop, Symbol};

    fn tile() -> mvt::VectorTile {
        let mut t = TileEncoder::new();
        let mut l = t.layer("place", 4096);
        l.feature(
            None,
            GeomType::Point,
            &[&[[1000, 1000]]],
            &[
                ("name", "Tokyo".into()),
                ("name:ja", "東京".into()),
                ("rank", 1.0.into()),
            ],
        );
        l.feature(
            None,
            GeomType::Point,
            &[&[[3000, 1000]]],
            &[("name", "Αθήνα".into()), ("rank", 2.0.into())],
        );
        // Outside the tile, in its buffer: the next tile labels it.
        l.feature(
            None,
            GeomType::Point,
            &[&[[4200, 1000]]],
            &[("name", "Elsewhere".into())],
        );
        l.finish();
        let mut l = t.layer("road", 4096);
        let line: &[[i32; 2]] = &[[0, 3000], [4096, 3000]];
        l.feature(
            None,
            GeomType::Line,
            &[line],
            &[("name", "Unter den Linden".into())],
        );
        l.finish();
        mvt::decode(&t.finish()).unwrap()
    }

    fn style() -> Style {
        let mut road = Symbol {
            placement: Placement::Line,
            spacing: 200.0,
            ..Symbol::default()
        };
        road.size = Prop::Const(12.0);
        Style::new("l", Color::WHITE)
            .with(Layer::symbol("road", "road", road))
            .with(Layer::symbol(
                "place",
                "place",
                Symbol {
                    rank: Some("rank".into()),
                    ..Symbol::default()
                },
            ))
    }

    fn labels(languages: &[&str]) -> Vec<TileLabel> {
        let t = tile();
        let layers: Vec<&mvt::Layer> = t.layers.iter().collect();
        let langs = expand_languages(&languages.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let mut out = Vec::new();
        extract(&layers, &style(), 14.0, 512.0, &langs, &mut out);
        out
    }

    #[test]
    fn names_follow_the_readers_language_and_fall_back_to_the_local_one() {
        let ja = labels(&["ja-JP"]);
        let texts: Vec<&str> = ja.iter().map(|l| &*l.text).collect();
        assert_eq!(&texts[..2], ["東京", "Αθήνα"], "places first, by rank");
        assert_eq!(ja[0].lang.as_deref(), Some("ja"));
        assert_eq!(ja[1].lang, None);
        let en = labels(&["en"]);
        assert_eq!(&*en[0].text, "Tokyo");
        assert!(en.iter().all(|l| &*l.text != "Elsewhere"));
        assert_eq!(
            expand_languages(&["zh-Hant-TW".into(), "zh".into()]),
            ["zh-Hant-TW", "zh-Hant", "zh"]
        );
    }

    #[test]
    fn a_line_label_repeats_along_it_and_runs_its_way() {
        let road: Vec<TileLabel> = labels(&[]).into_iter().filter(|l| l.layer == 0).collect();
        // 512 points of road, a label every 200: two.
        assert_eq!(road.len(), 2, "{road:?}");
        assert!(road.iter().all(|l| l.angle == Some(0.0)));
        assert!(road.iter().all(|l| (l.at[1] - 3000.0).abs() < 0.5));
        assert_eq!(road[0].key, road[1].key);
        // Each bends along a stretch of its road holding its middle.
        for l in &road {
            let c = l.curve.as_ref().expect("a curve");
            assert_eq!(c.points[usize::from(c.at)], l.at);
            assert_eq!(c.clusters.len(), "Unter den Linden".chars().count());
            assert!(
                c.points.first().unwrap()[0] < l.at[0] && c.points.last().unwrap()[0] > l.at[0]
            );
        }
    }

    #[test]
    fn text_cuts_into_clusters_its_marks_riding_on_their_letters() {
        let cut = |t: &str| -> Option<Vec<String>> {
            clusters(t).map(|c| c.iter().map(|c| c.of(t).to_owned()).collect())
        };
        assert_eq!(cut("Rue 1").unwrap(), ["R", "u", "e", " ", "1"]);
        // A combining acute stays on its e; an emoji's skin tone on it.
        assert_eq!(cut("Cafe\u{301}").unwrap(), ["C", "a", "f", "e\u{301}"]);
        assert_eq!(
            cut("a\u{1f44d}\u{1f3fd}").unwrap(),
            ["a", "\u{1f44d}\u{1f3fd}"]
        );
        // Joined by a zero-width joiner: one cluster.
        assert_eq!(cut("\u{1f469}\u{200d}\u{1f52c}").unwrap().len(), 1);
        assert_eq!(cut("東京駅").unwrap().len(), 3);
        // Arabic joins its letters, Hebrew runs right to left: set whole.
        assert_eq!(cut("القاهرة"), None);
        assert_eq!(cut("תל אביב"), None);
        assert_eq!(cut(""), None);
        assert_eq!(cut(&"x".repeat(MAX_CLUSTERS + 1)), None);
    }

    #[test]
    fn a_stretch_holds_its_middle_among_its_points() {
        let line = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [20.0, 10.0]];
        let (pts, at) = stretch(&line, 5.0, 12.0, 25.0);
        assert_eq!(
            pts,
            [
                [5.0, 0.0],
                [10.0, 0.0],
                [10.0, 2.0],
                [10.0, 10.0],
                [15.0, 10.0]
            ]
        );
        assert_eq!(at, 2);
        // A middle on a vertex is that vertex, once.
        let (pts, at) = stretch(&line, 0.0, 10.0, 30.0);
        assert_eq!(pts, [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [20.0, 10.0]]);
        assert_eq!(at, 1);
    }

    #[test]
    fn wide_characters_are_estimated_an_em_wide() {
        assert_eq!(estimate_width("東京", 10.0), 20.0);
        assert!((estimate_width("ab", 10.0) - 11.6).abs() < 1e-4);
        assert!(straight(
            &[[0.0, 0.0], [10.0, 0.0], [20.0, 1.0]],
            0.0,
            20.0,
            0.2
        ));
        assert!(!straight(
            &[[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]],
            0.0,
            20.0,
            0.6
        ));
        assert_eq!(
            centroid(&[[0.0, 0.0], [4.0, 0.0], [4.0, 2.0], [0.0, 2.0]]),
            [2.0, 1.0]
        );
    }
}
