//! The map's look: a typed style of layers drawn in order — background,
//! fills, lines, circles, symbols, rasters and hillshading — each over the
//! features of one source layer that pass its filter, with values that are
//! constant, follow the zoom, or read a feature's properties.
//!
//! [`builtin`] holds light and dark styles for the OpenMapTiles and
//! Shortbread schemas. A style is plain data: an app adjusts a built-in or
//! writes its own, and handing the view another style rebuilds the tiles
//! it holds without fetching them again.

pub mod builtin;
mod color;

pub use color::Color;

use crate::mvt::{GeomType, Value};

/// What a style reads a feature's property values through.
pub trait Properties {
    /// The value of property `key`.
    fn get(&self, key: &str) -> Option<&Value>;
    /// The feature's kind of geometry.
    fn kind(&self) -> GeomType;
}

/// A value that blends between stops.
pub trait Blend: Clone + Default {
    /// `a` blended toward `b` by `t` in `[0, 1]`.
    fn blend(a: &Self, b: &Self, t: f32) -> Self;
}

impl Blend for f32 {
    fn blend(a: &f32, b: &f32, t: f32) -> f32 {
        a + (b - a) * t
    }
}

impl Blend for Color {
    fn blend(a: &Color, b: &Color, t: f32) -> Color {
        a.mix(*b, t)
    }
}

impl Blend for String {
    fn blend(a: &String, b: &String, t: f32) -> String {
        if t < 1.0 { a.clone() } else { b.clone() }
    }
}

/// A style value: constant, following the zoom between stops, or chosen by
/// a feature's property.
#[derive(Debug, Clone, PartialEq)]
pub enum Prop<T> {
    Const(T),
    /// Stops of `(zoom, value)` in increasing zoom, blended exponentially
    /// by `base` between them (`1` blends linearly) and held past the ends.
    Zoom {
        base: f32,
        stops: Vec<(f32, T)>,
    },
    /// The first case whose values hold the feature's `key`, else the
    /// default.
    Match {
        key: String,
        cases: Vec<(Vec<Value>, Prop<T>)>,
        default: Box<Prop<T>>,
    },
}

impl<T> From<T> for Prop<T> {
    fn from(v: T) -> Self {
        Prop::Const(v)
    }
}

impl<T: Blend> Prop<T> {
    /// Stops blended linearly.
    pub fn linear(stops: impl IntoIterator<Item = (f32, T)>) -> Self {
        Prop::Zoom {
            base: 1.0,
            stops: stops.into_iter().collect(),
        }
    }

    /// Stops blended exponentially by `base`.
    pub fn exp(base: f32, stops: impl IntoIterator<Item = (f32, T)>) -> Self {
        Prop::Zoom {
            base,
            stops: stops.into_iter().collect(),
        }
    }

    /// The value at `zoom` for `feature`.
    pub fn eval(&self, zoom: f32, feature: &dyn Properties) -> T {
        match self {
            Prop::Const(v) => v.clone(),
            Prop::Zoom { base, stops } => zoom_stops(*base, stops, zoom),
            Prop::Match {
                key,
                cases,
                default,
            } => {
                let value = feature.get(key);
                for (values, prop) in cases {
                    if value.is_some_and(|v| values.iter().any(|c| loose_eq(c, v))) {
                        return prop.eval(zoom, feature);
                    }
                }
                default.eval(zoom, feature)
            }
        }
    }

    /// The value at `zoom` where no feature is at hand: a match takes its
    /// default.
    pub fn at(&self, zoom: f32) -> T {
        match self {
            Prop::Const(v) => v.clone(),
            Prop::Zoom { base, stops } => zoom_stops(*base, stops, zoom),
            Prop::Match { default, .. } => default.at(zoom),
        }
    }
}

fn zoom_stops<T: Blend>(base: f32, stops: &[(f32, T)], zoom: f32) -> T {
    let Some(first) = stops.first() else {
        return T::default();
    };
    if zoom <= first.0 {
        return first.1.clone();
    }
    for w in stops.windows(2) {
        let ((z0, a), (z1, b)) = (&w[0], &w[1]);
        if zoom < *z1 {
            let span = z1 - z0;
            let t = if span <= 0.0 {
                1.0
            } else if (base - 1.0).abs() < 1e-6 {
                (zoom - z0) / span
            } else {
                (base.powf(zoom - z0) - 1.0) / (base.powf(span) - 1.0)
            };
            return T::blend(a, b, t);
        }
    }
    stops[stops.len() - 1].1.clone()
}

/// Two values equal for matching: numbers by value, text by text.
fn loose_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        _ => false,
    }
}

fn order(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y),
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

/// Which features a layer draws.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Filter {
    /// Every feature.
    #[default]
    All,
    /// Features passing every filter.
    And(Vec<Filter>),
    /// Features passing any filter.
    Or(Vec<Filter>),
    Not(Box<Filter>),
    /// Features whose `key` holds one of the values.
    In(String, Vec<Value>),
    /// Features that have `key`.
    Has(String),
    /// Features whose `key` compares to the value as asked.
    Cmp(String, Cmp, Value),
    /// Features of this kind of geometry.
    Kind(GeomType),
}

impl std::ops::Not for Filter {
    type Output = Filter;

    /// Features failing this filter.
    fn not(self) -> Filter {
        Filter::Not(Box::new(self))
    }
}

/// A comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Ne,
}

impl Filter {
    /// Features whose `key` is `value`.
    pub fn eq(key: &str, value: impl Into<Value>) -> Self {
        Filter::In(key.to_owned(), vec![value.into()])
    }

    /// Features whose `key` is any of `values`.
    pub fn is_in<V: Into<Value>>(key: &str, values: impl IntoIterator<Item = V>) -> Self {
        Filter::In(key.to_owned(), values.into_iter().map(Into::into).collect())
    }

    /// Features whose `key` compares to `value` by `cmp`.
    pub fn cmp(key: &str, cmp: Cmp, value: impl Into<Value>) -> Self {
        Filter::Cmp(key.to_owned(), cmp, value.into())
    }

    /// Whether `feature` passes.
    pub fn test(&self, feature: &dyn Properties) -> bool {
        match self {
            Filter::All => true,
            Filter::And(all) => all.iter().all(|f| f.test(feature)),
            Filter::Or(any) => any.iter().any(|f| f.test(feature)),
            Filter::Not(f) => !f.test(feature),
            Filter::In(key, values) => feature
                .get(key)
                .is_some_and(|v| values.iter().any(|c| loose_eq(c, v))),
            Filter::Has(key) => feature.get(key).is_some(),
            Filter::Cmp(key, cmp, value) => {
                let Some(v) = feature.get(key) else {
                    return *cmp == Cmp::Ne;
                };
                if *cmp == Cmp::Ne {
                    return !loose_eq(v, value);
                }
                order(v, value).is_some_and(|o| match cmp {
                    Cmp::Lt => o.is_lt(),
                    Cmp::Le => o.is_le(),
                    Cmp::Gt => o.is_gt(),
                    Cmp::Ge => o.is_ge(),
                    Cmp::Ne => o.is_ne(),
                })
            }
            Filter::Kind(kind) => feature.kind() == *kind,
        }
    }
}

pub use crate::tess::{Cap, Join};

/// The font weight a label is shaped in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontWeight {
    #[default]
    Regular,
    Medium,
    Bold,
}

/// Where a label's text comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum TextField {
    /// The feature's name in the reader's language: `pattern` with
    /// `{lang}` replaced by each language the reader prefers in turn, then
    /// `fallback`.
    Name { pattern: String, fallback: String },
    /// Property `key` as it is.
    Key(String),
}

impl TextField {
    /// The OpenMapTiles name: `name:xx`, then `name`.
    pub fn openmaptiles() -> Self {
        TextField::Name {
            pattern: "name:{lang}".into(),
            fallback: "name".into(),
        }
    }

    /// The Shortbread name: `name_xx`, then `name`.
    pub fn shortbread() -> Self {
        TextField::Name {
            pattern: "name_{lang}".into(),
            fallback: "name".into(),
        }
    }

    /// The text for `feature`, in the first of `languages` it has a name in.
    pub fn text<'a>(&self, feature: &'a dyn Properties, languages: &[String]) -> Option<&'a str> {
        match self {
            TextField::Key(key) => feature.get(key).and_then(Value::as_str),
            TextField::Name { pattern, fallback } => {
                let mut key = String::with_capacity(pattern.len() + 4);
                for lang in languages {
                    key.clear();
                    key.push_str(&pattern.replace("{lang}", lang));
                    if let Some(s) = feature.get(&key).and_then(Value::as_str)
                        && !s.is_empty()
                    {
                        return Some(s);
                    }
                }
                feature.get(fallback).and_then(Value::as_str)
            }
        }
    }
}

/// How a label sits on its feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// At the point (a line's or polygon's at its middle).
    #[default]
    Point,
    /// Along the line, repeated every `spacing` points.
    Line,
}

/// A text label and its icon.
#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub text: Option<TextField>,
    /// The text's size, in points.
    pub size: Prop<f32>,
    pub color: Prop<Color>,
    pub halo: Prop<Color>,
    /// The halo's width, in points.
    pub halo_width: Prop<f32>,
    pub weight: FontWeight,
    pub uppercase: bool,
    /// Extra space between letters, in ems.
    pub letter_spacing: f32,
    /// Where a point label's text wraps, in ems.
    pub max_width: f32,
    pub placement: Placement,
    /// The space between repeats along a line, in points.
    pub spacing: f32,
    /// The icon drawn at a point label: the name of an icon in the view's
    /// icon set, read from a feature's property.
    pub icon: Option<Prop<String>>,
    pub icon_color: Prop<Color>,
    /// The icon's size, in points.
    pub icon_size: f32,
    /// A numeric property whose smaller values place first.
    pub rank: Option<String>,
    /// The space kept clear around the label, in points.
    pub padding: f32,
    /// A plate the text sits on, such as a road's number shield.
    pub shield: Option<Shield>,
}

/// The plate a label's text sits on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shield {
    pub fill: Color,
    pub border: Color,
    /// The border's width, in points.
    pub border_width: f32,
    /// The corners' radius, in points.
    pub radius: f32,
    /// The space between the text and the plate's edge, in points.
    pub padding: f32,
}

impl Shield {
    /// A plate of `fill` edged with `border`.
    pub const fn new(fill: Color, border: Color) -> Self {
        Self {
            fill,
            border,
            border_width: 1.0,
            radius: 3.0,
            padding: 2.5,
        }
    }
}

impl Default for Symbol {
    fn default() -> Self {
        Self {
            text: Some(TextField::openmaptiles()),
            size: Prop::Const(12.0),
            color: Prop::Const(Color::BLACK),
            halo: Prop::Const(Color::WHITE),
            halo_width: Prop::Const(1.25),
            weight: FontWeight::Regular,
            uppercase: false,
            letter_spacing: 0.0,
            max_width: 8.0,
            placement: Placement::Point,
            spacing: 320.0,
            icon: None,
            icon_color: Prop::Const(Color::BLACK),
            icon_size: 14.0,
            rank: None,
            padding: 2.0,
            shield: None,
        }
    }
}

/// The elevation encoding of a terrain source's tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemEncoding {
    /// `(R × 256 + G + B / 256) − 32768` meters.
    Terrarium,
    /// `−10000 + (R × 65536 + G × 256 + B) × 0.1` meters.
    MapboxRgb,
}

/// What a layer draws.
#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    Fill {
        color: Prop<Color>,
        opacity: Prop<f32>,
    },
    Line {
        color: Prop<Color>,
        /// The width, in points.
        width: Prop<f32>,
        opacity: Prop<f32>,
        /// Dash and gap lengths in widths; empty is solid.
        dashes: Vec<f32>,
        join: Join,
        cap: Cap,
    },
    Circle {
        /// The radius, in points.
        radius: Prop<f32>,
        color: Prop<Color>,
        stroke_width: Prop<f32>,
        stroke_color: Prop<Color>,
    },
    Symbol(Box<Symbol>),
    /// Images from a raster source.
    Raster {
        opacity: Prop<f32>,
    },
    /// Shading from a terrain source.
    Hillshade {
        encoding: DemEncoding,
        /// How strongly the slopes shade, `0` to `1`.
        exaggeration: f32,
        shadow: Color,
        highlight: Color,
        /// The light's direction, degrees clockwise from north.
        illumination: f32,
    },
}

/// One layer of a style.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub id: String,
    /// The source it reads, by name; `""` is the view's main source.
    pub source: String,
    /// The source layer its features come from (vector sources).
    pub source_layer: String,
    pub min_zoom: f32,
    pub max_zoom: f32,
    pub filter: Filter,
    pub paint: Paint,
}

impl Layer {
    fn new(id: &str, source_layer: &str, paint: Paint) -> Self {
        Self {
            id: id.to_owned(),
            source: String::new(),
            source_layer: source_layer.to_owned(),
            min_zoom: 0.0,
            max_zoom: 24.0,
            filter: Filter::All,
            paint,
        }
    }

    /// A fill of `source_layer`'s polygons.
    pub fn fill(id: &str, source_layer: &str, color: impl Into<Prop<Color>>) -> Self {
        Self::new(
            id,
            source_layer,
            Paint::Fill {
                color: color.into(),
                opacity: Prop::Const(1.0),
            },
        )
    }

    /// A stroke of `source_layer`'s lines (and polygon outlines).
    pub fn line(
        id: &str,
        source_layer: &str,
        color: impl Into<Prop<Color>>,
        width: impl Into<Prop<f32>>,
    ) -> Self {
        Self::new(
            id,
            source_layer,
            Paint::Line {
                color: color.into(),
                width: width.into(),
                opacity: Prop::Const(1.0),
                dashes: Vec::new(),
                join: Join::Round,
                cap: Cap::Butt,
            },
        )
    }

    /// Circles at `source_layer`'s points.
    pub fn circle(
        id: &str,
        source_layer: &str,
        radius: impl Into<Prop<f32>>,
        color: impl Into<Prop<Color>>,
    ) -> Self {
        Self::new(
            id,
            source_layer,
            Paint::Circle {
                radius: radius.into(),
                color: color.into(),
                stroke_width: Prop::Const(0.0),
                stroke_color: Prop::Const(Color::WHITE),
            },
        )
    }

    /// Labels of `source_layer`'s features.
    pub fn symbol(id: &str, source_layer: &str, symbol: Symbol) -> Self {
        Self::new(id, source_layer, Paint::Symbol(Box::new(symbol)))
    }

    /// The images of raster source `source`.
    pub fn raster(id: &str, source: &str) -> Self {
        Self {
            source: source.to_owned(),
            ..Self::new(
                id,
                "",
                Paint::Raster {
                    opacity: Prop::Const(1.0),
                },
            )
        }
    }

    /// Hillshading from terrain source `source`.
    pub fn hillshade(id: &str, source: &str, encoding: DemEncoding) -> Self {
        Self {
            source: source.to_owned(),
            ..Self::new(
                id,
                "",
                Paint::Hillshade {
                    encoding,
                    exaggeration: 0.5,
                    shadow: Color::rgba(0, 0, 0, 0.35),
                    highlight: Color::rgba(255, 255, 255, 0.25),
                    illumination: 315.0,
                },
            )
        }
    }

    /// The layer shown from `min` to `max` zoom.
    pub fn zooms(mut self, min: f32, max: f32) -> Self {
        self.min_zoom = min;
        self.max_zoom = max;
        self
    }

    /// The layer drawing only features passing `filter`.
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filter = filter;
        self
    }

    /// The layer reading source `source` instead of the main one.
    pub fn from_source(mut self, source: &str) -> Self {
        self.source = source.to_owned();
        self
    }

    /// The line layer dashed by `dashes`, in widths.
    pub fn dashed(mut self, dashes: &[f32]) -> Self {
        if let Paint::Line { dashes: d, .. } = &mut self.paint {
            *d = dashes.to_vec();
        }
        self
    }

    /// The line layer capped by `cap`.
    pub fn cap(mut self, cap: Cap) -> Self {
        if let Paint::Line { cap: c, .. } = &mut self.paint {
            *c = cap;
        }
        self
    }

    /// The layer drawn at `opacity`.
    pub fn opacity(mut self, opacity: impl Into<Prop<f32>>) -> Self {
        match &mut self.paint {
            Paint::Fill { opacity: o, .. }
            | Paint::Line { opacity: o, .. }
            | Paint::Raster { opacity: o } => *o = opacity.into(),
            _ => {}
        }
        self
    }

    /// Whether the layer shows at `zoom`.
    pub fn shows_at(&self, zoom: f32) -> bool {
        zoom >= self.min_zoom && zoom < self.max_zoom
    }
}

/// A map style: a background and layers drawn in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    pub name: String,
    pub background: Prop<Color>,
    pub layers: Vec<Layer>,
    /// Whether the style is dark: markers, the puck and controls pick
    /// contrasting colors by it.
    pub dark: bool,
}

impl Style {
    /// An empty style on `background`.
    pub fn new(name: &str, background: impl Into<Prop<Color>>) -> Self {
        Self {
            name: name.to_owned(),
            background: background.into(),
            layers: Vec::new(),
            dark: false,
        }
    }

    /// The style with `layer` drawn over the rest.
    pub fn with(mut self, layer: Layer) -> Self {
        self.layers.push(layer);
        self
    }

    /// The layer called `id`.
    pub fn layer(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    /// The layer called `id`, to change.
    pub fn layer_mut(&mut self, id: &str) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }

    /// The names of the sources other than the main one the layers read.
    pub fn extra_sources(&self) -> impl Iterator<Item = &str> {
        let mut seen: Vec<&str> = Vec::new();
        self.layers.iter().filter_map(move |l| {
            if l.source.is_empty() || seen.contains(&l.source.as_str()) {
                None
            } else {
                seen.push(&l.source);
                Some(l.source.as_str())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct F(Vec<(&'static str, Value)>, GeomType);

    impl Properties for F {
        fn get(&self, key: &str) -> Option<&Value> {
            self.0.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
        }
        fn kind(&self) -> GeomType {
            self.1
        }
    }

    #[test]
    fn zoom_stops_blend_and_hold() {
        let p = Prop::linear([(10.0, 1.0), (14.0, 5.0)]);
        let f = F(vec![], GeomType::Line);
        assert_eq!(p.eval(8.0, &f), 1.0);
        assert_eq!(p.eval(12.0, &f), 3.0);
        assert_eq!(p.eval(20.0, &f), 5.0);
        let e = Prop::exp(2.0, [(10.0, 0.0), (12.0, 3.0)]);
        assert!((e.eval(11.0, &f) - 1.0).abs() < 1e-6);
        let c = Prop::linear([(0.0, Color::BLACK), (10.0, Color::WHITE)]);
        assert!((c.eval(5.0, &f).r - 0.5).abs() < 1e-6);
    }

    #[test]
    fn matches_and_filters_read_properties() {
        use std::ops::Not;
        let road = F(
            vec![("class", "primary".into()), ("rank", 3.0.into())],
            GeomType::Line,
        );
        let width = Prop::Match {
            key: "class".into(),
            cases: vec![(vec!["motorway".into(), "primary".into()], Prop::Const(4.0))],
            default: Box::new(Prop::Const(1.0)),
        };
        assert_eq!(width.eval(12.0, &road), 4.0);
        assert_eq!(width.at(12.0), 1.0);
        let f = Filter::And(vec![
            Filter::is_in("class", ["primary", "secondary"]),
            Filter::cmp("rank", Cmp::Le, 3.0),
            Filter::Kind(GeomType::Line),
            Filter::Has("brunnel".into()).not(),
        ]);
        assert!(f.test(&road));
        assert!(!Filter::cmp("rank", Cmp::Gt, 3.0).test(&road));
        assert!(Filter::cmp("missing", Cmp::Ne, 1.0).test(&road));
        assert!(!Filter::Or(vec![Filter::eq("class", "minor")]).test(&road));
    }

    #[test]
    fn names_fall_back_by_language() {
        let place = F(
            vec![
                ("name", "東京".into()),
                ("name:en", "Tokyo".into()),
                ("name:de", "".into()),
            ],
            GeomType::Point,
        );
        let field = TextField::openmaptiles();
        let langs = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(field.text(&place, &langs(&["de", "en"])), Some("Tokyo"));
        assert_eq!(field.text(&place, &langs(&["fr"])), Some("東京"));
        let sb = F(vec![("name_en", "Tokyo".into())], GeomType::Point);
        assert_eq!(
            TextField::shortbread().text(&sb, &langs(&["en"])),
            Some("Tokyo")
        );
    }
}
