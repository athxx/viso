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
            let label = |at: [f32; 2], angle: Option<f32>| TileLabel {
                layer: li as u16,
                rank,
                text: Arc::clone(&text),
                lang: lang.map(Arc::from),
                at,
                angle,
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
                            out.push(label(*p, None));
                        }
                    }
                }
                (GeomType::Polygon, _) => {
                    if let Some(ring) = parts.first().filter(|r| r.len() >= 3) {
                        let c = centroid(ring);
                        if inside_tile(c) {
                            out.push(label(c, None));
                        }
                    }
                }
                (GeomType::Line, Placement::Point) => {
                    if let Some(line) = parts.iter().max_by(|a, b| length(a).total_cmp(&length(b)))
                        && line.len() >= 2
                    {
                        let m = along(line, length(line) / 2.0);
                        if inside_tile(m) {
                            out.push(label(m, None));
                        }
                    }
                }
                (GeomType::Line, Placement::Line) => {
                    let width = (estimate_width(&text, size) + 2.0 * sym.padding) * upp;
                    let spacing = sym.spacing.max(1.0) * upp;
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
                            if from < 0.0 || to > len || !straight(line, from, to, 0.6) {
                                continue;
                            }
                            let m = along(line, d);
                            if !inside_tile(m) {
                                continue;
                            }
                            let (a, b) = (along(line, from), along(line, to));
                            // A shield stays level where it repeats.
                            let angle = sym
                                .shield
                                .is_none()
                                .then(|| (b[1] - a[1]).atan2(b[0] - a[0]));
                            out.push(label(m, angle));
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
