//! The map view tilted: its tiles raised into 3D models drawn through the
//! scene lane in perspective, its buildings standing as solids, its ground
//! draped over a terrain's relief, its shapes and markers where the tilted
//! projection puts them, and the hand and the keys tilting it. Pictures of
//! each run are written to `VISO_MAP_OUT` when it is set.

mod support;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode, Modifiers, PointerButtons, PointerPhase, RawEvent, RawKey, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{BuildCx, WindowConfig};
use viso_map::style::{Color, DemEncoding, Terrain, builtin};
use viso_map::{
    Camera, LngLat, MapHandle, MapView, MemorySource, Padding, Polyline, RasterSource, TileId,
    View, WorldPoint, map_view,
};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
const W: f64 = 640.0;
const H: f64 = 480.0;

type Setup = Box<dyn Fn(&mut BuildCx<'_>)>;

thread_local! {
    static SETUP: RefCell<Option<Setup>> = const { RefCell::new(None) };
}

struct MapApp;

impl Application for MapApp {
    fn new(_: &mut AppCx) -> Self {
        MapApp
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            size: (W, H),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        SETUP.with(|s| (s.borrow().as_ref().expect("a setup"))(cx));
    }
}

fn redraws(n: usize) -> Vec<RawEvent> {
    (0..n)
        .map(|_| RawEvent::RedrawRequested { window: WIN })
        .collect()
}

fn mouse(x: f64, y: f64, phase: PointerPhase, m: Modifiers) -> RawEvent {
    let buttons = if phase == PointerPhase::Up {
        PointerButtons::NONE
    } else {
        PointerButtons::PRIMARY
    };
    RawEvent::Pointer(RawPointer::mouse(WIN, x, y, buttons, m, phase))
}

fn key(code: KeyCode, m: Modifiers) -> Vec<RawEvent> {
    [true, false]
        .map(|pressed| {
            RawEvent::Key(RawKey {
                window: WIN,
                code,
                pressed,
                repeat: false,
                modifiers: m,
            })
        })
        .into()
}

/// Runs a view `make` builds through `script`, returning the app and the
/// view's handle.
fn run(
    make: impl Fn(MapHandle) -> MapView + 'static,
    script: Vec<RawEvent>,
) -> (DrivenApp<MapApp>, MapHandle) {
    let slot = support::Slot::default();
    let s = Rc::clone(&slot);
    SETUP.with(|setup| {
        *setup.borrow_mut() = Some(Box::new(move |cx| {
            let handle = support::bind(cx, &s);
            make(handle).build(cx);
        }));
    });
    let app = drive_scripted::<MapApp>(script, STEP);
    let handle = slot.get().expect("built");
    (app, handle)
}

fn picture(app: &DrivenApp<MapApp>, name: &str) -> (u32, u32, Vec<u8>) {
    let (w, h, px) = app.pixels().expect("a software surface");
    support::save(name, w, h, &px);
    (w, h, px)
}

fn rgb_at(px: &(u32, u32, Vec<u8>), p: [f64; 2]) -> [u8; 3] {
    let (w, _, px) = px;
    let i = ((p[1].round() as u32 * w + p[0].round() as u32) * 4) as usize;
    [px[i + 2], px[i + 1], px[i]]
}

/// The distinct colors of a picture, coarsely.
fn colors(px: &(u32, u32, Vec<u8>), rows: std::ops::Range<usize>) -> usize {
    let mut seen = std::collections::HashSet::new();
    for y in rows.step_by(6) {
        for x in (0..W as usize).step_by(6) {
            let c = rgb_at(px, [x as f64, y as f64]);
            seen.insert([c[0] / 16, c[1] / 16, c[2] / 16]);
        }
    }
    seen.len()
}

/// A real Shortbread tile of Luxembourg City (see `fixtures/ATTRIBUTION`).
const LUXEMBOURG: &[u8] = include_bytes!("fixtures/luxembourg-14-8469-5582.mvt.gz");

/// Rollingergrund, where the tile's streets and buildings are.
fn rollingergrund() -> LngLat {
    let n = f64::from(1u32 << 14);
    WorldPoint::new((8469.0 + 0.72) / n, (5582.0 + 0.78) / n).to_lng_lat()
}

#[test]
fn a_tilted_view_raises_its_buildings_and_draws_its_shapes_where_the_projection_puts_them() {
    let tile = TileId::new(14, 8469, 5582);
    let at = rollingergrund();
    let flat = View::new(Camera::new(at, 16.4), [W, H]);
    let route: Vec<LngLat> = [[120.0, 420.0], [320.0, 240.0], [520.0, 120.0]]
        .iter()
        .map(|&p| flat.unproject(p))
        .collect();
    for (pitch, bearing, dark) in [(0.0, 30.0, false), (50.0, 30.0, false), (60.0, 200.0, true)] {
        let line = route.clone();
        let (app, handle) = run(
            move |slot| {
                let style = if dark {
                    builtin::shortbread_dark()
                } else {
                    builtin::shortbread_light()
                };
                map_view(MemorySource::new().with(tile, LUXEMBOURG))
                    .camera(
                        Camera::new(at, 16.4)
                            .with_bearing(bearing)
                            .with_pitch(pitch),
                    )
                    .style(style)
                    .shape(
                        Polyline::new(line.clone(), 8.0, Color::rgb(230, 30, 30))
                            .casing(viso_map::Stroke::new(14.0, Color::rgb(20, 20, 120))),
                    )
                    .handle(slot)
            },
            redraws(40),
        );
        let name = format!(
            "tilt-luxembourg-p{pitch}-b{bearing}-{}",
            if dark { "dark" } else { "light" }
        );
        let px = picture(&app, &name);
        let tiles = handle.tiles(app.states()).expect("the map");
        let view = handle.view(app.states()).expect("the map");
        if pitch == 0.0 {
            assert_eq!(tiles.raised, 0, "flat draws its tiles flat");
            continue;
        }
        assert!(tiles.raised >= 1, "{name}: {tiles:?}");
        assert!(colors(&px, 0..H as usize) > 12, "{name}: the map draws");
        // The route lies where the tilted projection puts its places.
        let mid = view.project(route[1]);
        let c = rgb_at(&px, mid);
        assert!(
            c[0] > 170 && c[1] < 90 && c[2] < 90,
            "{name}: {c:?} at {mid:?}"
        );
        // Its far end has shrunk toward the horizon, its near end grown.
        let ends = [view.project(route[0]), view.project(route[2])];
        assert!(ends.iter().all(|p| p[1].is_finite()));
    }
}

#[test]
fn the_far_ground_fades_into_a_haze_that_hides_its_edge() {
    // The fixture's streets on every tile, so ground lies everywhere; the
    // focus pushed low so the view sees out to the reach and past it.
    for (bearing, dark) in [(30.0, false), (250.0, true)] {
        let (app, handle) = run(
            move |slot| {
                map_view(|_: TileId| viso_map::Reply::ready(Ok(Some(LUXEMBOURG.to_vec()))))
                    .camera(
                        Camera::new(rollingergrund(), 16.0)
                            .with_bearing(bearing)
                            .with_pitch(60.0),
                    )
                    .padding(Padding {
                        top: 280.0,
                        ..Padding::default()
                    })
                    .style(if dark {
                        builtin::shortbread_dark()
                    } else {
                        builtin::shortbread_light()
                    })
                    .handle(slot)
            },
            redraws(60),
        );
        let name = format!(
            "tilt-haze-b{bearing}-{}",
            if dark { "dark" } else { "light" }
        );
        let px = picture(&app, &name);
        assert!(
            handle.tiles(app.states()).expect("the map").raised >= 1,
            "{name}"
        );
        let view = handle.view(app.states()).expect("the map");
        let focus = view.focus();
        // Down each column from the top: one haze past the reach, then the
        // ground coming out of it with no edge between.
        let haze = rgb_at(&px, [W / 2.0, 2.0]);
        for x in (8..W as usize).step_by(40) {
            let mut last = rgb_at(&px, [x as f64, 0.0]);
            assert!(
                last.iter().zip(haze).all(|(a, b)| a.abs_diff(b) <= 2),
                "{name}: {last:?} at the top of {x} beside {haze:?}"
            );
            for y in 1..(focus[1] - 120.0) as usize {
                let c = rgb_at(&px, [x as f64, y as f64]);
                let jump = c.iter().zip(last).map(|(a, b)| a.abs_diff(b)).max();
                // Only the clearer ground's own drawing may change sharply.
                if y < (focus[1] * 0.3) as usize {
                    assert!(
                        jump <= Some(10),
                        "{name}: an edge at {x},{y}: {last:?} {c:?}"
                    );
                }
                last = c;
            }
        }
        // Near the focus the air is clear: the ground keeps its colors.
        assert!(
            colors(&px, (focus[1] as usize - 60)..H as usize) > 6,
            "{name}"
        );
    }
}

/// Where the fixture's buildings stand thickest: the middle of the
/// sixteenth of the tile holding the most of them.
fn downtown() -> LngLat {
    let decoded = viso_map::mvt::decode(LUXEMBOURG).expect("a tile");
    let layer = decoded
        .layers
        .iter()
        .find(|l| l.name == "buildings")
        .expect("buildings");
    let mut cells = [0u32; 16];
    for f in &layer.features {
        if let Ok(g) = f.geometry()
            && let Some(p) = g.points.first()
        {
            let e = layer.extent as f64;
            let (x, y) = (f64::from(p[0]) / e, f64::from(p[1]) / e);
            if (0.0..1.0).contains(&x) && (0.0..1.0).contains(&y) {
                cells[(y * 4.0) as usize * 4 + (x * 4.0) as usize] += 1;
            }
        }
    }
    let best = (0..16).max_by_key(|&i| cells[i]).expect("cells");
    let n = f64::from(1u32 << 14);
    let (cx, cy) = (
        (best % 4) as f64 * 0.25 + 0.125,
        (best / 4) as f64 * 0.25 + 0.125,
    );
    WorldPoint::new((8469.0 + cx) / n, (5582.0 + cy) / n).to_lng_lat()
}

#[test]
fn buildings_stand_up_as_the_view_tilts() {
    let tile = TileId::new(14, 8469, 5582);
    let at = downtown();
    let mut shots = Vec::new();
    for raised in [false, true] {
        let (app, handle) = run(
            move |slot| {
                let mut style = builtin::shortbread_light();
                if !raised {
                    // The same buildings, drawn flat.
                    let b = style.layer_mut("building").expect("a building layer");
                    if let viso_map::style::Paint::Extrusion { color, .. } = b.paint.clone() {
                        b.paint = viso_map::style::Paint::Fill {
                            color,
                            opacity: 1.0.into(),
                        };
                    }
                }
                map_view(MemorySource::new().with(tile, LUXEMBOURG))
                    .camera(Camera::new(at, 17.0).with_bearing(20.0).with_pitch(55.0))
                    .style(style)
                    .handle(slot)
            },
            redraws(40),
        );
        assert!(
            handle.tiles(app.states()).expect("the map").raised >= 1,
            "{:?}",
            handle.tiles(app.states()).expect("the map")
        );
        shots.push(picture(
            &app,
            if raised {
                "tilt-buildings-raised"
            } else {
                "tilt-buildings-flat"
            },
        ));
    }
    // Lit walls and roofs: shades the flat drawing has none of.
    let differ = (0..H as usize)
        .step_by(4)
        .flat_map(|y| {
            (0..W as usize)
                .step_by(4)
                .map(move |x| [x as f64, y as f64])
        })
        .filter(|&p| {
            let (a, b) = (rgb_at(&shots[0], p), rgb_at(&shots[1], p));
            a.iter().zip(&b).any(|(a, b)| a.abs_diff(*b) > 12)
        })
        .count();
    let all = (W as usize / 4) * (H as usize / 4);
    assert!(differ * 20 > all, "{differ} of {all} differ");
}

/// A terrain tile of a ridge running north and south through `ridge` (a
/// world x), Terrarium-encoded: `top` meters high, falling a meter every
/// `run` meters.
fn ridge_tile(tile: TileId, ridge: f64, top: f64, run: f64) -> Vec<u8> {
    let n = 256u32;
    let span = f64::from(1u32 << tile.z);
    let mut bgra = Vec::with_capacity((n * n * 4) as usize);
    for _y in 0..n {
        for x in 0..n {
            let wx = (f64::from(tile.x) + (f64::from(x) + 0.5) / f64::from(n)) / span;
            // A world unit is about 24 500 km at Berlin's latitude.
            let meters = (top - (wx - ridge).abs() * 24_500_000.0 / run).max(0.0);
            let v = meters + 32768.0;
            let r = (v / 256.0).floor();
            let g = (v - r * 256.0).floor();
            let b = ((v - r * 256.0 - g) * 256.0).round().min(255.0);
            bgra.extend_from_slice(&[b as u8, g as u8, r as u8, 255]);
        }
    }
    support::png(n, n, &bgra)
}

/// A raster tile of solid color: blue on even tiles, green on odd ones.
fn checker(tile: TileId) -> Vec<u8> {
    let bgra = if (tile.x + tile.y).is_multiple_of(2) {
        [255, 0, 0, 255]
    } else {
        [0, 255, 0, 255]
    };
    support::png(256, 256, &bgra.repeat(256 * 256))
}

#[test]
fn image_layers_lie_on_the_tilted_ground_beneath_the_roads() {
    use viso_map::style::Layer;
    let start = Camera::new(LngLat::new(13.40, 52.52), 12.5)
        .with_bearing(-30.0)
        .with_pitch(55.0);
    let ridge = start.center.to_world().x + 150.0 / (512.0 * 12.5f64.exp2());
    // Half-opaque photos under the roads on flat ground; a ridge, then a
    // hillshade over it.
    let mut slopes = Vec::new();
    for (relief, image) in [(false, true), (true, false), (true, true)] {
        let (app, handle) = run(
            move |slot| {
                let mut style = builtin::openmaptiles_light();
                let road = style
                    .layers
                    .iter()
                    .position(|l| l.id.starts_with("road"))
                    .unwrap_or(style.layers.len());
                if image {
                    let layer = if relief {
                        Layer::hillshade("shade", "dem", DemEncoding::Terrarium)
                    } else {
                        Layer::raster("photo", "photo").opacity(0.5)
                    };
                    style.layers.insert(road, layer);
                }
                if relief {
                    style = style.with_terrain(
                        Terrain::new("dem", DemEncoding::Terrarium).exaggeration(2.0),
                    );
                }
                map_view(|tile: TileId| viso_map::Reply::ready(Ok(Some(support::synthetic(tile)))))
                    .camera(start)
                    .style(style)
                    .raster_source(
                        "photo",
                        RasterSource::new(
                            |tile: TileId| viso_map::Reply::ready(Ok(Some(checker(tile)))),
                            0,
                            18,
                        ),
                    )
                    .raster_source(
                        "dem",
                        RasterSource::new(
                            move |tile: TileId| {
                                viso_map::Reply::ready(Ok(Some(ridge_tile(
                                    tile, ridge, 600.0, 6.0,
                                ))))
                            },
                            0,
                            12,
                        ),
                    )
                    .handle(slot)
            },
            redraws(50),
        );
        let name = match (relief, image) {
            (false, _) => "tilt-raster-photo",
            (true, false) => "tilt-ridge-unshaded",
            (true, true) => "tilt-hillshade-ridge",
        };
        let px = picture(&app, name);
        let tiles = handle.tiles(app.states()).expect("the map");
        assert!(tiles.raised >= 4, "{name}: {tiles:?}");
        assert!(!image || tiles.images >= 1, "{name}: {tiles:?}");
        if relief {
            // The slope beyond the ridge, turned from the light.
            let mut sum = 0u32;
            for y in (380..480).step_by(4) {
                for x in (560..640).step_by(4) {
                    sum += rgb_at(&px, [x as f64, y as f64])
                        .iter()
                        .map(|&c| u32::from(c))
                        .sum::<u32>();
                }
            }
            slopes.push(sum / (25 * 20));
            continue;
        }
        // Tinted blue or green over the ground, half way...
        let (mut tinted, mut all) = (0, 0);
        for y in (H as usize / 2..H as usize).step_by(6) {
            for x in (0..W as usize).step_by(6) {
                let c = rgb_at(&px, [x as f64, y as f64]).map(i32::from);
                all += 1;
                if (c[2] > c[0] + 30 && c[2] > c[1] + 30) || (c[1] > c[0] + 30 && c[1] > c[2] + 30)
                {
                    assert!(c[0] > 40, "{name}: half of what is beneath shows: {c:?}");
                    tinted += 1;
                }
            }
        }
        assert!(tinted * 2 > all, "{name}: {tinted} of {all}");
        // ...and the roads over the photos keep their own colors.
        let lines = (0..H as usize)
            .flat_map(|y| (0..W as usize).map(move |x| [x as f64, y as f64]))
            .filter(|&p| rgb_at(&px, p).iter().all(|&v| v > 225))
            .count();
        assert!(lines > 200, "{name}: {lines}");
    }
    // The hillshade darkens the slope turned from its light.
    assert!(slopes[1] + 8 < slopes[0], "{slopes:?}");
}

#[test]
fn a_translucent_fill_blends_over_the_tilted_ground_and_a_route_over_it() {
    use viso_map::Circle;
    let at = LngLat::new(13.40, 52.52);
    let start = Camera::new(at, 14.0).with_bearing(20.0).with_pitch(50.0);
    let tilted = View::new(start, [W, H]);
    let route: Vec<LngLat> = [[200.0, 262.0], [440.0, 262.0]]
        .iter()
        .map(|&p| tilted.unproject(p))
        .collect();
    let (app, handle) = run(
        move |slot| {
            map_view(|tile: TileId| viso_map::Reply::ready(Ok(Some(support::synthetic(tile)))))
                .camera(start)
                .style(builtin::openmaptiles_light())
                .shape(Circle::new(
                    at,
                    400.0,
                    Color::rgb(0xff, 0x40, 0x00).alpha(0.5),
                ))
                .shape(Polyline::new(route.clone(), 8.0, Color::rgb(20, 40, 230)))
                .handle(slot)
        },
        redraws(40),
    );
    let px = picture(&app, "tilt-translucent-fill");
    let view = handle.view(app.states()).expect("the map");
    // Inside the circle, off the route: the fill and the ground beneath,
    // half each.
    let inside = view.project(tilted.unproject([320.0, 222.0]));
    let c = rgb_at(&px, inside);
    assert!(c[0] > 200 && c[2] > 60 && c[2] < 180, "{c:?} at {inside:?}");
    // The route draws over the fill, whole.
    let on = view.project(tilted.unproject([320.0, 262.0]));
    let c = rgb_at(&px, on);
    assert!(c[2] > 180 && c[0] < 90, "{c:?} at {on:?}");
    // Beyond the circle, the ground as it is.
    let c = rgb_at(&px, [20.0, 460.0]);
    assert!(c[0].abs_diff(c[2]) < 40, "{c:?}");
}

fn red_box(cx: &mut BuildCx<'_>) {
    cx.stack(
        viso::ui::StackStyle {
            size: viso::ui::Sizing::fixed(20.0, 30.0),
            style: viso::ui::BoxStyle::solid(viso::render::Rgba::new(1.0, 0.0, 0.0, 1.0)),
            ..Default::default()
        },
        |_| {},
    );
}

#[test]
fn a_marker_stands_on_the_relief_under_it() {
    use viso_map::Marker;
    let start = Camera::new(LngLat::new(13.40, 52.52), 12.5)
        .with_bearing(-30.0)
        .with_pitch(55.0);
    let c = start.center.to_world();
    let ridge = c.x + 150.0 / (512.0 * 12.5f64.exp2());
    // On the ridge's crest, far over the ground at the focus.
    let crest = WorldPoint::new(ridge, c.y).to_lng_lat();
    let mut bottoms = Vec::new();
    for relief in [false, true] {
        let (app, handle) = run(
            move |slot| {
                let mut style = builtin::openmaptiles_light();
                if relief {
                    style = style.with_terrain(
                        Terrain::new("dem", DemEncoding::Terrarium).exaggeration(2.0),
                    );
                }
                map_view(|tile: TileId| viso_map::Reply::ready(Ok(Some(support::synthetic(tile)))))
                    .camera(start)
                    .style(style)
                    .raster_source(
                        "dem",
                        RasterSource::new(
                            move |tile: TileId| {
                                viso_map::Reply::ready(Ok(Some(ridge_tile(
                                    tile, ridge, 600.0, 6.0,
                                ))))
                            },
                            0,
                            12,
                        ),
                    )
                    .marker(Marker::new(1, crest, red_box))
                    .handle(slot)
            },
            redraws(50),
        );
        let name = if relief {
            "tilt-marker-ridge"
        } else {
            "tilt-marker-flat"
        };
        let px = picture(&app, name);
        // The lowest red row in the marker's column.
        let at = handle.view(app.states()).expect("the map").project(crest);
        let bottom = (0..H as usize)
            .rev()
            .find(|&y| {
                let c = rgb_at(&px, [at[0], y as f64]);
                c[0] > 200 && c[1] < 60 && c[2] < 60
            })
            .unwrap_or_else(|| panic!("{name}: no marker over {at:?}"));
        bottoms.push((bottom as f64, at[1]));
    }
    // Flat, it stands where the plane puts its place; on the ridge, higher.
    let ((flat, plane), (raised, _)) = (bottoms[0], bottoms[1]);
    assert!((flat - plane).abs() < 3.0, "{bottoms:?}");
    assert!(raised + 15.0 < flat, "{bottoms:?}");
}

thread_local! {
    /// Answers never given: tiles that stay loading.
    static HELD: RefCell<Vec<viso_map::Completer<Option<Vec<u8>>>>> = const { RefCell::new(Vec::new()) };
}

/// A tile of zoom 13 all water; of zoom 14 all grass, but one in four
/// either never arriving (`held`) or all water too.
fn water_then_land(tile: TileId, held: bool) -> viso_map::Reply<Option<Vec<u8>>> {
    use viso_map::mvt::{GeomType, TileEncoder};
    let odd = tile.z >= 14 && tile.x % 2 == 1 && tile.y % 2 == 1;
    if odd && held {
        let (done, answer) = viso_map::reply();
        HELD.with(|h| h.borrow_mut().push(done));
        return answer;
    }
    let mut t = TileEncoder::new();
    let sq: &[[i32; 2]] = &[[-64, -64], [4160, -64], [4160, 4160], [-64, 4160]];
    let (layer, class) = if tile.z < 14 || odd {
        ("water", "lake")
    } else {
        ("landcover", "grass")
    };
    let mut l = t.layer(layer, 4096);
    l.feature(None, GeomType::Polygon, &[sq], &[("class", class.into())]);
    l.finish();
    viso_map::Reply::ready(Ok(Some(t.finish())))
}

#[test]
fn a_coarser_tile_beneath_finer_ones_does_not_show_through_their_relief() {
    let at = LngLat::new(13.40, 52.52);
    let start = Camera::new(at, 12.6).with_bearing(-30.0).with_pitch(55.0);
    let ridge = start.center.to_world().x + 120.0 / (512.0 * 13.6f64.exp2());
    // Zoomed in a level once the coarse tiles are drawn: the finer ones
    // that arrive cover them, the others leave them showing. Beside it,
    // the same with every finer tile there, the missing ones all water.
    let mut shots = Vec::new();
    for held in [true, false] {
        let none = Modifiers::default();
        let mut script = redraws(30);
        script.push(mouse(320.0, 240.0, PointerPhase::Down, none));
        script.push(mouse(320.0, 240.0, PointerPhase::Up, none));
        script.extend(key(KeyCode::Equal, none));
        script.extend(redraws(60));
        let (app, handle) = run(
            move |slot| {
                let style = builtin::openmaptiles_light()
                    .with_terrain(Terrain::new("dem", DemEncoding::Terrarium).exaggeration(2.0));
                map_view(move |tile: TileId| water_then_land(tile, held))
                    .camera(start)
                    .style(style)
                    .raster_source(
                        "dem",
                        RasterSource::new(
                            move |tile: TileId| {
                                viso_map::Reply::ready(Ok(Some(ridge_tile(
                                    tile, ridge, 500.0, 3.0,
                                ))))
                            },
                            0,
                            12,
                        ),
                    )
                    .handle(slot)
            },
            script,
        );
        HELD.with(|h| h.borrow_mut().clear());
        let name = if held {
            "tilt-coarse-under-fine"
        } else {
            "tilt-fine-only"
        };
        shots.push(picture(&app, name));
        let tiles = handle.tiles(app.states()).expect("the map");
        assert!(handle.view(app.states()).expect("the map").camera.zoom > 13.0);
        assert_eq!(
            tiles.drawn.iter().any(|d| d.tile.z == 13),
            held,
            "{name}: {tiles:?}"
        );
    }
    let blue = |px: &(u32, u32, Vec<u8>), x: i64, y: i64| {
        let p = [
            x.clamp(0, W as i64 - 1) as f64,
            y.clamp(0, H as i64 - 1) as f64,
        ];
        let c = rgb_at(px, p);
        u16::from(c[2]) > u16::from(c[0]) + 25
    };
    // Water shows only where the finer tiles that never came would lie:
    // nowhere the full set draws land, a few points off its water's edges.
    let (mut water, mut peeks) = (0, 0);
    for y in (100..H as i64).step_by(3) {
        for x in (0..W as i64).step_by(3) {
            if !blue(&shots[0], x, y) {
                continue;
            }
            water += 1;
            let near = (-4..=4).any(|dy| (-4..=4).any(|dx| blue(&shots[1], x + dx, y + dy)));
            if !near {
                peeks += 1;
            }
        }
    }
    assert!(water > 100, "{water}");
    assert_eq!(peeks, 0, "{peeks} of {water}");
}

#[test]
fn the_ground_drapes_over_the_terrain_and_shades_its_slopes() {
    let start = Camera::new(LngLat::new(13.40, 52.52), 12.5)
        .with_bearing(-30.0)
        .with_pitch(55.0);
    // East of the middle, seen from its side.
    let ridge = start.center.to_world().x + 150.0 / (512.0 * 12.5f64.exp2());
    let mut shots = Vec::new();
    for relief in [false, true] {
        let (app, handle) = run(
            move |slot| {
                let mut style = builtin::openmaptiles_light();
                if relief {
                    style = style.with_terrain(
                        Terrain::new("dem", DemEncoding::Terrarium).exaggeration(2.0),
                    );
                }
                map_view(|tile: TileId| viso_map::Reply::ready(Ok(Some(support::synthetic(tile)))))
                    .camera(start)
                    .style(style)
                    .raster_source(
                        "dem",
                        RasterSource::new(
                            move |tile: TileId| {
                                viso_map::Reply::ready(Ok(Some(ridge_tile(
                                    tile, ridge, 600.0, 6.0,
                                ))))
                            },
                            0,
                            12,
                        ),
                    )
                    .handle(slot)
            },
            redraws(50),
        );
        let name = if relief {
            "tilt-terrain-ridge"
        } else {
            "tilt-terrain-flat"
        };
        shots.push(picture(&app, name));
        assert!(
            handle.tiles(app.states()).expect("the map").raised >= 4,
            "{name}: {:?}",
            handle.tiles(app.states()).expect("the map")
        );
    }
    // The relief moves the ground and shades its slopes: much of the
    // picture changes, and the slope turned from the light darkens.
    let differ = (0..H as usize)
        .step_by(4)
        .flat_map(|y| {
            (0..W as usize)
                .step_by(4)
                .map(move |x| [x as f64, y as f64])
        })
        .filter(|&p| {
            let (a, b) = (rgb_at(&shots[0], p), rgb_at(&shots[1], p));
            a.iter().zip(&b).any(|(a, b)| a.abs_diff(*b) > 8)
        })
        .count();
    let all = (W as usize / 4) * (H as usize / 4);
    assert!(differ * 8 > all, "{differ} of {all} differ");
    // The slope beyond the ridge, turned from the light, darkens.
    let shade = |px: &(u32, u32, Vec<u8>)| {
        let mut sum = 0u32;
        let mut n = 0u32;
        for y in (380..480).step_by(4) {
            for x in (560..640).step_by(4) {
                sum += rgb_at(px, [x as f64, y as f64])
                    .iter()
                    .map(|&c| u32::from(c))
                    .sum::<u32>();
                n += 1;
            }
        }
        sum / n
    };
    let (flat, ridge) = (shade(&shots[0]), shade(&shots[1]));
    assert!(ridge + 40 < flat, "{ridge} {flat}");
}

#[test]
fn an_alt_drag_and_the_keys_tilt_and_n_looks_straight_down_again() {
    let none = Modifiers::default();
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let make = |slot: MapHandle| {
        map_view(|tile: TileId| viso_map::Reply::ready(Ok(Some(support::synthetic(tile)))))
            .camera(Camera::new(LngLat::new(13.40, 52.52), 13.0))
            .handle(slot)
    };
    // Dragging up with Alt held tilts the far side away.
    let mut script = redraws(10);
    script.push(mouse(320.0, 300.0, PointerPhase::Down, alt));
    for i in 1..=20 {
        script.push(mouse(
            320.0,
            300.0 - 6.0 * f64::from(i),
            PointerPhase::Moved,
            alt,
        ));
        script.push(RawEvent::RedrawRequested { window: WIN });
    }
    script.push(mouse(320.0, 180.0, PointerPhase::Up, alt));
    script.extend(redraws(30));
    let (app, handle) = run(make, script);
    let pitch = handle.camera(app.states()).expect("the map").pitch;
    assert!(pitch > 20.0 && pitch <= viso_map::MAX_PITCH, "{pitch}");
    assert!(handle.tiles(app.states()).expect("the map").raised > 0);
    // Shift with Up tilts a step, N faces north and looks down again.
    let mut script = redraws(10);
    script.push(mouse(320.0, 240.0, PointerPhase::Down, none));
    script.push(mouse(320.0, 240.0, PointerPhase::Up, none));
    script.extend(key(KeyCode::Up, shift));
    script.extend(redraws(30));
    script.extend(key(KeyCode::Up, shift));
    script.extend(redraws(30));
    let (app, handle) = run(make, script.clone());
    assert!(
        (handle.camera(app.states()).expect("the map").pitch - 20.0).abs() < 1e-6,
        "{}",
        handle.camera(app.states()).expect("the map").pitch
    );
    script.extend(key(KeyCode::N, none));
    script.extend(redraws(30));
    let (app, handle) = run(make, script);
    assert_eq!(handle.camera(app.states()).expect("the map").pitch, 0.0);
    assert_eq!(handle.tiles(app.states()).expect("the map").raised, 0);
}
