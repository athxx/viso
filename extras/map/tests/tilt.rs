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
use viso::ui::{BuildCx, Component, WindowConfig};
use viso_map::style::{Color, DemEncoding, Terrain, builtin};
use viso_map::{
    Camera, LngLat, MapHandle, MapHandleSlot, MapView, MemorySource, Polyline, RasterSource,
    TileId, View, WorldPoint, map_view,
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
    make: impl Fn(&MapHandleSlot) -> MapView + 'static,
    script: Vec<RawEvent>,
) -> (DrivenApp<MapApp>, MapHandle) {
    let slot = MapHandleSlot::default();
    let s = Rc::clone(&slot);
    SETUP.with(|setup| {
        *setup.borrow_mut() = Some(Box::new(move |cx| make(&s).build(cx)));
    });
    let app = drive_scripted::<MapApp>(script, STEP);
    let handle = slot.borrow().clone().expect("built");
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
        let tiles = handle.tiles();
        let view = handle.view();
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
        assert!(handle.tiles().raised >= 1, "{:?}", handle.tiles());
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
        assert!(handle.tiles().raised >= 4, "{name}: {:?}", handle.tiles());
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
    let make = |slot: &MapHandleSlot| {
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
    let (_, handle) = run(make, script);
    let pitch = handle.camera().pitch;
    assert!(pitch > 20.0 && pitch <= viso_map::MAX_PITCH, "{pitch}");
    assert!(handle.tiles().raised > 0);
    // Shift with Up tilts a step, N faces north and looks down again.
    let mut script = redraws(10);
    script.push(mouse(320.0, 240.0, PointerPhase::Down, none));
    script.push(mouse(320.0, 240.0, PointerPhase::Up, none));
    script.extend(key(KeyCode::Up, shift));
    script.extend(redraws(30));
    script.extend(key(KeyCode::Up, shift));
    script.extend(redraws(30));
    let (_, handle) = run(make, script.clone());
    assert!(
        (handle.camera().pitch - 20.0).abs() < 1e-6,
        "{}",
        handle.camera().pitch
    );
    script.extend(key(KeyCode::N, none));
    script.extend(redraws(30));
    let (_, handle) = run(make, script);
    assert_eq!(handle.camera().pitch, 0.0);
    assert_eq!(handle.tiles().raised, 0);
}
