//! Built-in styles: light and dark, for the OpenMapTiles and Shortbread
//! vector tile schemas.
//!
//! Both schemas share one palette per theme and one road hierarchy, so a
//! map reads the same whichever data it draws: land and water recessive,
//! roads by class with casings from mid zooms, tunnels faint and bridges
//! cased over what they cross, labels haloed in the background's color.

use std::ops::Not;

use crate::mvt::{GeomType, Value};

#[cfg(test)]
use super::Paint;
use super::{
    Cap, Cmp, Color, Filter, FontWeight, Layer, Placement, Prop, Shield, Style, Symbol, TextField,
};

/// The height, in meters, of a building whose data gives none.
const BUILDING_HEIGHT: f32 = 10.0;

/// The colors a built-in style draws with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub land: Color,
    pub water: Color,
    pub water_line: Color,
    pub wood: Color,
    pub grass: Color,
    pub park: Color,
    pub farmland: Color,
    pub sand: Color,
    pub ice: Color,
    pub wetland: Color,
    pub residential: Color,
    pub commercial: Color,
    pub industrial: Color,
    pub institution: Color,
    pub cemetery: Color,
    pub building: Color,
    pub building_edge: Color,
    pub aeroway: Color,
    pub motorway: Color,
    pub trunk: Color,
    pub primary: Color,
    pub secondary: Color,
    pub minor: Color,
    pub path: Color,
    pub casing: Color,
    pub major_casing: Color,
    pub rail: Color,
    pub boundary: Color,
    pub label: Color,
    pub label_minor: Color,
    pub water_label: Color,
    pub halo: Color,
    pub poi: Color,
    pub dark: bool,
}

impl Palette {
    /// The light palette.
    pub const LIGHT: Palette = Palette {
        land: Color::hex(0xf2efe9),
        water: Color::hex(0xaad3df),
        water_line: Color::hex(0x8fc2d4),
        wood: Color::hex(0xc8dcb5),
        grass: Color::hex(0xd8e8c8),
        park: Color::hex(0xcfe5bd),
        farmland: Color::hex(0xeef0de),
        sand: Color::hex(0xf2e8c9),
        ice: Color::hex(0xf4f8fb),
        wetland: Color::hex(0xd6e6dc),
        residential: Color::hex(0xe9e5df),
        commercial: Color::hex(0xf1e3e1),
        industrial: Color::hex(0xe8e0e6),
        institution: Color::hex(0xf0ecd9),
        cemetery: Color::hex(0xd4e2cc),
        building: Color::hex(0xdcd6cf),
        building_edge: Color::hex(0xc9c1b8),
        aeroway: Color::hex(0xd9d3e3),
        motorway: Color::hex(0xf7b16d),
        trunk: Color::hex(0xf9cc84),
        primary: Color::hex(0xfde5a3),
        secondary: Color::hex(0xfdf5c9),
        minor: Color::hex(0xffffff),
        path: Color::hex(0xb8a99a),
        casing: Color::hex(0xcfc6bc),
        major_casing: Color::hex(0xc69a6b),
        rail: Color::hex(0xa8a39c),
        boundary: Color::hex(0x9e8fa6),
        label: Color::hex(0x2b2b2b),
        label_minor: Color::hex(0x5d5a56),
        water_label: Color::hex(0x3d6e8a),
        halo: Color::rgba(255, 255, 255, 0.9),
        poi: Color::hex(0x7a6f66),
        dark: false,
    };

    /// The dark palette.
    pub const DARK: Palette = Palette {
        land: Color::hex(0x1e2125),
        water: Color::hex(0x10263a),
        water_line: Color::hex(0x173550),
        wood: Color::hex(0x1f2b22),
        grass: Color::hex(0x222c24),
        park: Color::hex(0x1f2d23),
        farmland: Color::hex(0x23262a),
        sand: Color::hex(0x2c2a25),
        ice: Color::hex(0x2a2f35),
        wetland: Color::hex(0x1e2a2a),
        residential: Color::hex(0x24272c),
        commercial: Color::hex(0x2a262b),
        industrial: Color::hex(0x272630),
        institution: Color::hex(0x2a2924),
        cemetery: Color::hex(0x212b24),
        building: Color::hex(0x2f3338),
        building_edge: Color::hex(0x383d43),
        aeroway: Color::hex(0x2b2a33),
        motorway: Color::hex(0x8a6239),
        trunk: Color::hex(0x7a6440),
        primary: Color::hex(0x5f5a48),
        secondary: Color::hex(0x4b4b47),
        minor: Color::hex(0x3c4046),
        path: Color::hex(0x55524d),
        casing: Color::hex(0x15171a),
        major_casing: Color::hex(0x2a1f14),
        rail: Color::hex(0x5a5d62),
        boundary: Color::hex(0x7c6d86),
        label: Color::hex(0xe4e1dc),
        label_minor: Color::hex(0xa9a59f),
        water_label: Color::hex(0x7fa9c6),
        halo: Color::rgba(20, 22, 25, 0.9),
        poi: Color::hex(0xa59a8f),
        dark: true,
    };
}

/// The road hierarchy: each class's width in points at zoom 6, 10, 14 and
/// 18, and the zoom it appears at.
#[derive(Clone, Copy)]
struct RoadClass {
    widths: [f32; 4],
    from: f32,
    /// The zoom its casing appears at.
    cased_from: f32,
}

const MOTORWAY: RoadClass = RoadClass {
    widths: [0.6, 2.2, 6.0, 22.0],
    from: 4.0,
    cased_from: 8.0,
};
const TRUNK: RoadClass = RoadClass {
    widths: [0.5, 1.8, 5.5, 20.0],
    from: 5.0,
    cased_from: 9.0,
};
const PRIMARY: RoadClass = RoadClass {
    widths: [0.3, 1.4, 5.0, 18.0],
    from: 7.0,
    cased_from: 10.0,
};
const SECONDARY: RoadClass = RoadClass {
    widths: [0.2, 1.0, 4.0, 16.0],
    from: 9.0,
    cased_from: 11.0,
};
const MINOR: RoadClass = RoadClass {
    widths: [0.1, 0.4, 2.6, 13.0],
    from: 11.0,
    cased_from: 13.0,
};
const SERVICE: RoadClass = RoadClass {
    widths: [0.1, 0.2, 1.2, 7.0],
    from: 13.0,
    cased_from: 15.0,
};

impl RoadClass {
    fn width(self, extra: f32) -> Prop<f32> {
        let [a, b, c, d] = self.widths;
        Prop::exp(
            1.5,
            [
                (6.0, a + extra * 0.25),
                (10.0, b + extra * 0.5),
                (14.0, c + extra),
                (18.0, d + extra * 2.0),
            ],
        )
    }
}

/// What a schema calls things.
struct Schema {
    /// The source layers.
    water: &'static str,
    waterway: &'static str,
    landcover: &'static str,
    landuse: &'static str,
    building: &'static str,
    roads: &'static str,
    boundary: &'static str,
    places: &'static str,
    road_names: &'static str,
    water_names: &'static str,
    pois: &'static str,
    /// The property naming a feature's kind.
    kind: &'static str,
    name: fn() -> TextField,
}

const OPENMAPTILES: Schema = Schema {
    water: "water",
    waterway: "waterway",
    landcover: "landcover",
    landuse: "landuse",
    building: "building",
    roads: "transportation",
    boundary: "boundary",
    places: "place",
    road_names: "transportation_name",
    water_names: "water_name",
    pois: "poi",
    kind: "class",
    name: TextField::openmaptiles,
};

const SHORTBREAD: Schema = Schema {
    water: "water_polygons",
    waterway: "water_lines",
    landcover: "land",
    landuse: "sites",
    building: "buildings",
    roads: "streets",
    boundary: "boundaries",
    places: "place_labels",
    road_names: "street_labels",
    water_names: "water_polygons_labels",
    pois: "pois",
    kind: "kind",
    name: TextField::shortbread,
};

fn kinds(s: &Schema, values: &[&str]) -> Filter {
    Filter::is_in(s.kind, values.iter().copied())
}

fn text(s: &Schema, size: Prop<f32>, color: Color, p: &Palette) -> Symbol {
    Symbol {
        text: Some((s.name)()),
        size,
        color: Prop::Const(color),
        halo: Prop::Const(p.halo),
        halo_width: Prop::Const(1.4),
        ..Symbol::default()
    }
}

/// The road classes of a schema, from least to most important: the values
/// its kind property takes for each, and how each draws.
fn road_classes(
    openmaptiles: bool,
    p: &Palette,
) -> [(&'static [&'static str], RoadClass, Color, Color); 6] {
    let minor: &'static [&'static str] = if openmaptiles {
        &["minor", "busway"]
    } else {
        &[
            "residential",
            "unclassified",
            "living_street",
            "pedestrian",
            "busway",
        ]
    };
    [
        (&["service", "track"], SERVICE, p.minor, p.casing),
        (minor, MINOR, p.minor, p.casing),
        (&["secondary", "tertiary"], SECONDARY, p.secondary, p.casing),
        (&["primary"], PRIMARY, p.primary, p.major_casing),
        (&["trunk"], TRUNK, p.trunk, p.major_casing),
        (&["motorway"], MOTORWAY, p.motorway, p.major_casing),
    ]
}

/// Which roads a pass draws: tunnels, the surface, or bridges.
#[derive(Clone, Copy, PartialEq)]
enum Level {
    Tunnel,
    Surface,
    Bridge,
}

fn level_filter(openmaptiles: bool, level: Level) -> Filter {
    if openmaptiles {
        match level {
            Level::Tunnel => Filter::eq("brunnel", "tunnel"),
            Level::Bridge => Filter::eq("brunnel", "bridge"),
            Level::Surface => Filter::is_in("brunnel", ["tunnel", "bridge"]).not(),
        }
    } else {
        let yes = |key: &str| Filter::In(key.to_owned(), vec![Value::Bool(true)]);
        match level {
            Level::Tunnel => yes("tunnel"),
            Level::Bridge => yes("bridge"),
            Level::Surface => Filter::Or(vec![yes("tunnel"), yes("bridge")]).not(),
        }
    }
}

fn roads(style: &mut Style, s: &Schema, p: &Palette, level: Level) {
    let omt = s.kind == "class";
    let tag = match level {
        Level::Tunnel => "tunnel",
        Level::Surface => "road",
        Level::Bridge => "bridge",
    };
    let classes = road_classes(omt, p);
    // Paths and rail ride with the roads at their level.
    let path_filter = if omt {
        Filter::eq("class", "path")
    } else {
        kinds(s, &["footway", "path", "cycleway", "steps", "bridleway"])
    };
    let rail_filter = if omt {
        Filter::is_in("class", ["rail", "transit"])
    } else {
        kinds(
            s,
            &[
                "rail",
                "light_rail",
                "subway",
                "tram",
                "narrow_gauge",
                "monorail",
                "funicular",
            ],
        )
    };
    let faint = if level == Level::Tunnel { 0.5 } else { 1.0 };
    let path = Layer::line(
        &format!("{tag}-path"),
        s.roads,
        p.path,
        Prop::exp(1.4, [(14.0, 0.8), (18.0, 2.4)]),
    )
    .zooms(14.0, 24.0)
    .filter(Filter::And(vec![path_filter, level_filter(omt, level)]))
    .opacity(faint);
    style.layers.push(path);
    for (values, class, _, casing) in &classes {
        let filter = Filter::And(vec![
            Filter::is_in(s.kind, values.iter().copied()),
            level_filter(omt, level),
            Filter::Kind(GeomType::Polygon).not(),
        ]);
        let layer = Layer::line(
            &format!("{tag}-{}-casing", values[0]),
            s.roads,
            *casing,
            class.width(2.0),
        )
        .zooms(class.cased_from, 24.0)
        .filter(filter)
        .opacity(faint);
        style.layers.push(layer);
    }
    for (values, class, color, _) in &classes {
        let filter = Filter::And(vec![
            Filter::is_in(s.kind, values.iter().copied()),
            level_filter(omt, level),
            Filter::Kind(GeomType::Polygon).not(),
        ]);
        let color = if level == Level::Tunnel {
            color.mix(p.land, 0.5)
        } else {
            *color
        };
        style.layers.push(
            Layer::line(
                &format!("{tag}-{}", values[0]),
                s.roads,
                color,
                class.width(0.0),
            )
            .zooms(class.from, 24.0)
            .filter(filter)
            .cap(if level == Level::Bridge {
                Cap::Butt
            } else {
                Cap::Round
            }),
        );
    }
    style.layers.push(
        Layer::line(
            &format!("{tag}-rail"),
            s.roads,
            p.rail,
            Prop::exp(1.4, [(12.0, 0.6), (18.0, 2.5)]),
        )
        .zooms(11.0, 24.0)
        .filter(Filter::And(vec![rail_filter, level_filter(omt, level)]))
        .opacity(faint),
    );
}

fn build(name: &str, s: &Schema, p: &Palette) -> Style {
    let omt = s.kind == "class";
    let mut style = Style::new(name, p.land);
    style.dark = p.dark;
    if !omt {
        style.layers.push(Layer::fill("ocean", "ocean", p.water));
    }
    // Land cover and use, quietest first.
    let cover: &[(&str, &[&str], Color)] = if omt {
        &[
            ("farmland", &["farmland"], p.farmland),
            ("ice", &["ice"], p.ice),
            ("sand", &["sand"], p.sand),
            ("wetland", &["wetland"], p.wetland),
            ("grass", &["grass"], p.grass),
            ("wood", &["wood"], p.wood),
        ]
    } else {
        &[
            (
                "farmland",
                &["farmland", "farmyard", "orchard", "vineyard", "allotments"],
                p.farmland,
            ),
            ("sand", &["sand", "beach", "bare_rock", "scree"], p.sand),
            ("ice", &["glacier"], p.ice),
            (
                "wetland",
                &["swamp", "bog", "marsh", "wet_meadow"],
                p.wetland,
            ),
            (
                "grass",
                &[
                    "grass",
                    "grassland",
                    "meadow",
                    "heath",
                    "scrub",
                    "village_green",
                ],
                p.grass,
            ),
            ("wood", &["forest"], p.wood),
            (
                "park",
                &["park", "garden", "playground", "golf_course"],
                p.park,
            ),
            ("residential", &["residential", "garages"], p.residential),
            ("commercial", &["commercial", "retail"], p.commercial),
            (
                "industrial",
                &["industrial", "railway", "quarry", "landfill"],
                p.industrial,
            ),
            ("cemetery", &["cemetery", "grave_yard"], p.cemetery),
        ]
    };
    for (id, values, color) in cover {
        style.layers.push(
            Layer::fill(&format!("landcover-{id}"), s.landcover, *color).filter(kinds(s, values)),
        );
    }
    let uses: &[(&str, &[&str], Color, f32)] = if omt {
        &[
            (
                "residential",
                &["residential", "suburb", "quarter", "neighbourhood"],
                p.residential,
                10.0,
            ),
            ("commercial", &["commercial", "retail"], p.commercial, 11.0),
            (
                "industrial",
                &["industrial", "railway", "garages", "quarry"],
                p.industrial,
                11.0,
            ),
            ("cemetery", &["cemetery"], p.cemetery, 12.0),
            (
                "institution",
                &[
                    "school",
                    "university",
                    "college",
                    "kindergarten",
                    "hospital",
                    "library",
                ],
                p.institution,
                12.0,
            ),
            (
                "pitch",
                &["pitch", "stadium", "playground", "track"],
                p.park,
                13.0,
            ),
        ]
    } else {
        &[
            (
                "institution",
                &[
                    "school",
                    "university",
                    "college",
                    "kindergarten",
                    "hospital",
                ],
                p.institution,
                12.0,
            ),
            (
                "pitch",
                &["pitch", "stadium", "sports_centre"],
                p.park,
                13.0,
            ),
        ]
    };
    for (id, values, color, from) in uses {
        style.layers.push(
            Layer::fill(&format!("landuse-{id}"), s.landuse, *color)
                .zooms(*from, 24.0)
                .filter(kinds(s, values)),
        );
    }
    if omt {
        style.layers.push(
            Layer::fill("park", "park", p.park)
                .opacity(Prop::linear([(6.0, 0.4), (12.0, 0.8)]))
                .filter(Filter::Kind(GeomType::Polygon)),
        );
    }
    style.layers.push(Layer::fill("water", s.water, p.water));
    style.layers.push(
        Layer::line(
            "waterway-river",
            s.waterway,
            p.water_line,
            Prop::exp(1.4, [(8.0, 0.6), (14.0, 3.0), (18.0, 12.0)]),
        )
        .zooms(8.0, 24.0)
        .filter(kinds(s, &["river", "canal"])),
    );
    style.layers.push(
        Layer::line(
            "waterway-stream",
            s.waterway,
            p.water_line,
            Prop::exp(1.4, [(12.0, 0.4), (14.0, 1.0), (18.0, 4.0)]),
        )
        .zooms(12.0, 24.0)
        .filter(kinds(s, &["stream", "drain", "ditch"])),
    );
    if omt {
        style.layers.push(
            Layer::fill("aeroway-area", "aeroway", p.aeroway)
                .zooms(11.0, 24.0)
                .filter(Filter::Kind(GeomType::Polygon)),
        );
        style.layers.push(
            Layer::line(
                "aeroway-runway",
                "aeroway",
                p.aeroway,
                Prop::exp(1.5, [(10.0, 1.0), (14.0, 12.0), (18.0, 60.0)]),
            )
            .zooms(10.0, 24.0)
            .filter(Filter::And(vec![
                Filter::eq("class", "runway"),
                Filter::Kind(GeomType::Line),
            ])),
        );
        style.layers.push(
            Layer::line(
                "aeroway-taxiway",
                "aeroway",
                p.aeroway,
                Prop::exp(1.5, [(12.0, 0.5), (14.0, 2.0), (18.0, 16.0)]),
            )
            .zooms(12.0, 24.0)
            .filter(Filter::And(vec![
                Filter::eq("class", "taxiway"),
                Filter::Kind(GeomType::Line),
            ])),
        );
    }
    style.layers.push(
        Layer::extrusion(
            "building",
            s.building,
            Prop::linear([(13.0, p.land), (15.5, p.building)]),
            BUILDING_HEIGHT,
        )
        .zooms(13.0, 24.0),
    );
    style.layers.push(
        Layer::line("building-edge", s.building, p.building_edge, 0.6)
            .zooms(15.5, 24.0)
            .opacity(Prop::linear([(15.5, 0.0), (16.5, 1.0)])),
    );
    roads(&mut style, s, p, Level::Tunnel);
    roads(&mut style, s, p, Level::Surface);
    roads(&mut style, s, p, Level::Bridge);
    // Boundaries: countries solid, regions dashed, maritime lines left out.
    let admin = |level: f64| Filter::cmp("admin_level", Cmp::Le, level);
    let maritime = Filter::In(
        "maritime".into(),
        vec![Value::Number(1.0), Value::Bool(true)],
    )
    .not();
    style.layers.push(
        Layer::line(
            "boundary-state",
            s.boundary,
            p.boundary,
            Prop::exp(1.3, [(3.0, 0.5), (12.0, 1.4)]),
        )
        .zooms(3.0, 24.0)
        .filter(Filter::And(vec![
            admin(4.0),
            admin(2.0).not(),
            maritime.clone(),
        ]))
        .dashed(&[3.0, 2.0])
        .opacity(0.8),
    );
    style.layers.push(
        Layer::line(
            "boundary-country",
            s.boundary,
            p.boundary,
            Prop::exp(1.3, [(0.0, 0.6), (10.0, 2.0), (16.0, 3.0)]),
        )
        .filter(Filter::And(vec![admin(2.0), maritime])),
    );
    // Labels, least important first: later layers place first.
    let mut water = text(
        s,
        Prop::exp(1.2, [(4.0, 11.0), (14.0, 14.0)]),
        p.water_label,
        p,
    );
    water.weight = FontWeight::Regular;
    water.rank = Some("rank".into());
    style
        .layers
        .push(Layer::symbol("water-name", s.water_names, water.clone()));
    if !omt {
        water.placement = Placement::Line;
        style
            .layers
            .push(Layer::symbol("waterway-name", "water_lines_labels", water));
    }
    // Points of interest, giving way to the streets' names.
    let mut poi = text(s, Prop::Const(11.0), p.poi, p);
    poi.icon = Some(Prop::Match {
        key: if omt { "class" } else { "amenity" }.into(),
        cases: Vec::new(),
        default: Box::new(Prop::Const(String::from("dot"))),
    });
    poi.icon_color = Prop::Const(p.poi);
    poi.icon_size = 12.0;
    poi.rank = Some("rank".into());
    poi.max_width = 7.0;
    style
        .layers
        .push(Layer::symbol("poi", s.pois, poi).zooms(15.0, 24.0));
    // Road numbers on shields, along the main roads.
    let shield = Symbol {
        text: Some(TextField::Key("ref".into())),
        size: Prop::Const(10.0),
        color: Prop::Const(if p.dark {
            Color::hex(0xe8e8e8)
        } else {
            Color::hex(0x333333)
        }),
        halo_width: Prop::Const(0.0),
        weight: FontWeight::Bold,
        placement: Placement::Line,
        spacing: 640.0,
        padding: 4.0,
        shield: Some(Shield::new(
            if p.dark {
                Color::hex(0x3a3d42)
            } else {
                Color::WHITE
            },
            p.major_casing,
        )),
        ..Symbol::default()
    };
    style.layers.push(
        Layer::symbol("road-shield", s.road_names, shield)
            .zooms(10.0, 24.0)
            .filter(Filter::Has("ref".into())),
    );
    let mut road = text(
        s,
        Prop::exp(1.2, [(13.0, 10.0), (18.0, 14.0)]),
        p.label_minor,
        p,
    );
    road.placement = Placement::Line;
    road.spacing = 360.0;
    style
        .layers
        .push(Layer::symbol("road-name", s.road_names, road).zooms(13.0, 24.0));
    let place_kind = |values: &[&str]| kinds(s, values);
    let mut small = text(
        s,
        Prop::exp(1.2, [(10.0, 10.0), (16.0, 14.0)]),
        p.label_minor,
        p,
    );
    small.rank = Some("rank".into());
    style.layers.push(
        Layer::symbol("place-small", s.places, small)
            .zooms(11.0, 24.0)
            .filter(place_kind(&[
                "suburb",
                "quarter",
                "neighbourhood",
                "hamlet",
                "isolated_dwelling",
                "locality",
            ])),
    );
    let mut town = text(s, Prop::exp(1.2, [(7.0, 11.0), (14.0, 16.0)]), p.label, p);
    town.rank = Some("rank".into());
    style.layers.push(
        Layer::symbol("place-town", s.places, town)
            .zooms(8.0, 24.0)
            .filter(place_kind(&["town", "village"])),
    );
    let mut city = text(s, Prop::exp(1.2, [(4.0, 12.0), (12.0, 20.0)]), p.label, p);
    city.weight = FontWeight::Medium;
    city.rank = Some(if omt { "rank" } else { "population" }.into());
    style.layers.push(
        Layer::symbol("place-city", s.places, city)
            .zooms(4.0, 16.0)
            .filter(place_kind(&["city", "capital", "state_capital"])),
    );
    let mut region = text(
        s,
        Prop::exp(1.2, [(3.0, 10.0), (7.0, 13.0)]),
        p.label_minor,
        p,
    );
    region.uppercase = true;
    region.letter_spacing = 0.12;
    style.layers.push(
        Layer::symbol("place-state", s.places, region)
            .zooms(4.0, 8.0)
            .filter(place_kind(&["state", "province"])),
    );
    let mut country = text(s, Prop::exp(1.2, [(1.0, 11.0), (6.0, 16.0)]), p.label, p);
    country.weight = FontWeight::Bold;
    country.rank = Some("rank".into());
    style.layers.push(
        Layer::symbol("place-country", s.places, country)
            .zooms(1.0, 7.0)
            .filter(place_kind(&["country", "continent"])),
    );
    style
}

/// OpenMapTiles in the light palette.
pub fn openmaptiles_light() -> Style {
    build("openmaptiles-light", &OPENMAPTILES, &Palette::LIGHT)
}

/// OpenMapTiles in the dark palette.
pub fn openmaptiles_dark() -> Style {
    build("openmaptiles-dark", &OPENMAPTILES, &Palette::DARK)
}

/// Shortbread in the light palette.
pub fn shortbread_light() -> Style {
    build("shortbread-light", &SHORTBREAD, &Palette::LIGHT)
}

/// Shortbread in the dark palette.
pub fn shortbread_dark() -> Style {
    build("shortbread-dark", &SHORTBREAD, &Palette::DARK)
}

/// OpenMapTiles in `palette`.
pub fn openmaptiles(palette: &Palette) -> Style {
    build("openmaptiles", &OPENMAPTILES, palette)
}

/// Shortbread in `palette`.
pub fn shortbread(palette: &Palette) -> Style {
    build("shortbread", &SHORTBREAD, palette)
}

/// The paint of every layer of `style` that strokes or fills, for tests.
#[cfg(test)]
fn drawn(style: &Style) -> impl Iterator<Item = &Paint> {
    style.layers.iter().map(|l| &l.paint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_styles_cover_both_schemas_in_both_themes() {
        for style in [
            openmaptiles_light(),
            openmaptiles_dark(),
            shortbread_light(),
            shortbread_dark(),
        ] {
            assert!(
                style.layers.len() > 40,
                "{}: {}",
                style.name,
                style.layers.len()
            );
            let ids: std::collections::HashSet<_> = style.layers.iter().map(|l| &l.id).collect();
            assert_eq!(
                ids.len(),
                style.layers.len(),
                "{}: ids are unique",
                style.name
            );
            assert!(drawn(&style).any(|p| matches!(p, Paint::Symbol(_))));
            assert_eq!(style.dark, style.name.ends_with("dark"));
        }
        let light = openmaptiles_light();
        let dark = openmaptiles_dark();
        assert!(light.background.at(10.0).luminance() > dark.background.at(10.0).luminance());
        // The same layers in both themes: switching keeps the structure.
        let ids = |s: &Style| s.layers.iter().map(|l| l.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&light), ids(&dark));
    }
}
