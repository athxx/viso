//! Labels along lines bending with them glyph by glyph: set on the line as
//! the view shows it at any turn and tilt, kept reading upright, left out
//! where the line turns too sharply, repeated along a long line and kept
//! clear of each other glyph by glyph. Pictures of each run are written to
//! `VISO_MAP_OUT` when it is set; with `VISO_MAP_LUXEMBOURG` naming a
//! directory of real `z-x-y.pbf` tiles of Luxembourg, pictures of its
//! streets too.

mod support;

use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{RawEvent, WindowId};
use viso::prelude::*;
use viso::ui::{BuildCx, WindowConfig};
use viso_map::mvt::{GeomType, TileEncoder};
use viso_map::style::builtin;
use viso_map::{Camera, LngLat, MapHandle, MapView, TileId, View, WorldPoint, map_view};

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
    fn new(cx: &mut AppCx) -> Self {
        for face in fonts() {
            cx.load_font(&face[..]);
        }
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
        support::patient(cx, |cx| {
            SETUP.with(|s| (s.borrow().as_ref().expect("a setup"))(cx));
        });
    }
}

/// A face for Latin text: a system one where there is one, and the
/// repository's subset.
fn fonts() -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = [
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/Library/Fonts/Arial Unicode.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ]
    .iter()
    .filter_map(|p| std::fs::read(p).ok())
    .take(1)
    .collect();
    out.push(include_bytes!("../../../crates/text/tests/fixtures/DejaVuSans-subset.ttf").to_vec());
    out
}

fn redraws(n: usize) -> Vec<RawEvent> {
    (0..n)
        .map(|_| RawEvent::RedrawRequested { window: WIN })
        .collect()
}

fn run(
    make: impl Fn(MapHandle) -> MapView + 'static,
    frames: usize,
) -> (DrivenApp<MapApp>, MapHandle) {
    let slot = support::slot();
    let s = Rc::clone(&slot);
    SETUP.with(|setup| {
        *setup.borrow_mut() = Some(Box::new(move |cx| {
            let handle = support::bind(cx, &s);
            make(handle).build(cx);
        }));
    });
    let app = drive_scripted::<MapApp>(redraws(frames), STEP);
    let handle = slot.get().expect("built");
    (app, handle)
}

fn picture(app: &DrivenApp<MapApp>, name: &str) {
    let (w, h, px) = app.pixels().expect("a software surface");
    support::save(name, w, h, &px);
}

/// The middle of the made-up roads.
const MIDDLE: LngLat = LngLat::new(13.40, 52.52);
/// The arc's radius, in world units: about 300 points at zoom 15.
const RADIUS: f64 = 300.0 / (512.0 * 32768.0);

/// A road bending through a half circle about [`MIDDLE`], named "Curved
/// Avenue"; a zigzag of sharp corners south of it, named "Zigzag Lane".
fn roads() -> (Vec<WorldPoint>, Vec<WorldPoint>) {
    let c = MIDDLE.to_world();
    let arc = (0..=48)
        .map(|i| {
            let a = PI * (1.0 + f64::from(i) / 48.0);
            WorldPoint::new(
                c.x + RADIUS * a.cos(),
                c.y + RADIUS * a.sin() + RADIUS * 0.5,
            )
        })
        .collect();
    let zig = (0..=16)
        .map(|i| {
            let x = c.x - RADIUS + f64::from(i) * RADIUS / 8.0;
            let y = c.y + RADIUS * if i % 2 == 0 { 0.55 } else { 0.72 };
            WorldPoint::new(x, y)
        })
        .collect();
    (arc, zig)
}

/// OpenMapTiles tiles of [`roads`].
fn tile_of(tile: TileId) -> Vec<u8> {
    let n = f64::from(1u32 << tile.z);
    let local = |w: &WorldPoint| {
        [
            ((w.x * n - f64::from(tile.x)) * 4096.0).round() as i32,
            ((w.y * n - f64::from(tile.y)) * 4096.0).round() as i32,
        ]
    };
    let (arc, zig) = roads();
    let arc: Vec<[i32; 2]> = arc.iter().map(local).collect();
    let zig: Vec<[i32; 2]> = zig.iter().map(local).collect();
    let mut t = TileEncoder::new();
    let mut l = t.layer("transportation", 4096);
    for line in [&arc, &zig] {
        l.feature(None, GeomType::Line, &[line], &[("class", "minor".into())]);
    }
    l.finish();
    let mut l = t.layer("transportation_name", 4096);
    l.feature(
        None,
        GeomType::Line,
        &[&arc],
        &[("name", "Curved Avenue".into()), ("class", "minor".into())],
    );
    l.feature(
        None,
        GeomType::Line,
        &[&zig],
        &[("name", "Zigzag Lane".into()), ("class", "minor".into())],
    );
    l.finish();
    t.finish()
}

/// The distance from view point `p` to the polyline `line`.
fn off_line(p: [f64; 2], line: &[[f64; 2]]) -> f64 {
    line.windows(2)
        .map(|w| {
            let (a, b) = (w[0], w[1]);
            let d = [b[0] - a[0], b[1] - a[1]];
            let len2 = d[0] * d[0] + d[1] * d[1];
            let t = if len2 > 0.0 {
                (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (p[0] - a[0] - d[0] * t).hypot(p[1] - a[1] - d[1] * t)
        })
        .fold(f64::INFINITY, f64::min)
}

fn wrap(a: f64) -> f64 {
    let a = a.rem_euclid(2.0 * PI);
    if a > PI { a - 2.0 * PI } else { a }
}

#[test]
fn a_line_label_bends_with_its_line_at_every_turn_and_tilt() {
    for (bearing, pitch) in [
        (0.0, 0.0),
        (70.0, 0.0),
        (160.0, 0.0),
        (250.0, 0.0),
        (20.0, 45.0),
        (200.0, 60.0),
    ] {
        let camera = Camera::new(MIDDLE, 15.0)
            .with_bearing(bearing)
            .with_pitch(pitch);
        let (app, handle) = run(
            move |slot| {
                map_view(|tile: TileId| viso_map::Reply::ready(Ok(Some(tile_of(tile)))))
                    .camera(camera)
                    .style(builtin::openmaptiles_light())
                    .languages(&["en"])
                    .handle(slot)
            },
            60,
        );
        let name = format!("labels-curved-b{bearing}-p{pitch}");
        picture(&app, &name);
        let view: View = handle.view(app.states()).expect("the map");
        let (arc, _) = roads();
        let line: Vec<[f64; 2]> = arc.iter().map(|&w| view.to_view(w)).collect();
        let bent = handle.label_glyphs(app.states());
        let curved: Vec<_> = bent.iter().filter(|b| b.text == "Curved Avenue").collect();
        assert!(!curved.is_empty(), "{name}: {bent:?}");
        // The sharp zigzag holds no label.
        assert!(
            bent.iter().all(|b| b.text != "Zigzag Lane"),
            "{name}: {bent:?}"
        );
        for label in &curved {
            let glyphs: Vec<([f32; 2], f32)> =
                label.glyphs.iter().map(|g| (g.at, g.turn)).collect();
            assert_eq!(glyphs.len(), "CurvedAvenue".len(), "{name}");
            let mut turns = (f64::INFINITY, f64::NEG_INFINITY);
            for (i, (at, turn)) in glyphs.iter().enumerate() {
                let p = [f64::from(at[0]), f64::from(at[1])];
                // Each glyph sits on the road.
                assert!(off_line(p, &line) < 1.5, "{name}: glyph {i} at {p:?}");
                let t = f64::from(*turn);
                turns = (turns.0.min(t), turns.1.max(t));
                if i > 0 {
                    let (prev, pt) = &glyphs[i - 1];
                    // Neighbours turn little from each other, and the run
                    // reads on from the one before.
                    assert!(wrap(t - f64::from(*pt)).abs() < PI / 4.0 + 1e-3, "{name}");
                    let step = [p[0] - f64::from(prev[0]), p[1] - f64::from(prev[1])];
                    let along = step[0] * t.cos() + step[1] * t.sin();
                    assert!(along > 0.0, "{name}: glyph {i} reads back");
                }
            }
            // It bends: its glyphs turn through an arc.
            assert!(turns.1 - turns.0 > 0.12, "{name}: {turns:?}");
            // Upright: its run reads left to right, or top to bottom.
            let (first, last) = (glyphs[0].0, glyphs[glyphs.len() - 1].0);
            let chord = f64::from(last[1] - first[1]).atan2(f64::from(last[0] - first[0]));
            assert!(
                chord > -PI / 2.0 && chord <= PI / 2.0 + 1e-6,
                "{name}: {chord}"
            );
        }
    }
}

#[test]
fn bent_labels_repeat_along_a_long_line_and_keep_clear_of_each_other() {
    let camera = Camera::new(MIDDLE, 14.2);
    let (app, handle) = run(
        move |slot| {
            map_view(|tile: TileId| {
                let n = f64::from(1u32 << tile.z);
                let local = |w: WorldPoint| {
                    [
                        ((w.x * n - f64::from(tile.x)) * 4096.0).round() as i32,
                        ((w.y * n - f64::from(tile.y)) * 4096.0).round() as i32,
                    ]
                };
                // A long wavy road across the view, and a second crossing
                // it, both named.
                let c = MIDDLE.to_world();
                let span = 620.0 / (512.0 * 14.2f64.exp2());
                let wave: Vec<[i32; 2]> = (0..=120)
                    .map(|i| {
                        let t = f64::from(i) / 120.0 - 0.5;
                        local(WorldPoint::new(
                            c.x + t * span,
                            c.y + (t * 9.0).sin() * span * 0.06,
                        ))
                    })
                    .collect();
                let cross: Vec<[i32; 2]> = (0..=40)
                    .map(|i| {
                        let t = f64::from(i) / 40.0 - 0.5;
                        local(WorldPoint::new(c.x + t * span * 0.1, c.y + t * span * 0.6))
                    })
                    .collect();
                let mut t = TileEncoder::new();
                let mut l = t.layer("transportation", 4096);
                for line in [&wave, &cross] {
                    l.feature(None, GeomType::Line, &[line], &[("class", "minor".into())]);
                }
                l.finish();
                let mut l = t.layer("transportation_name", 4096);
                l.feature(
                    None,
                    GeomType::Line,
                    &[&wave],
                    &[("name", "Long Wavy Road".into())],
                );
                l.feature(
                    None,
                    GeomType::Line,
                    &[&cross],
                    &[("name", "Cross Street".into())],
                );
                l.finish();
                viso_map::Reply::ready(Ok(Some(t.finish())))
            })
            .camera(camera)
            .style({
                let mut style = builtin::openmaptiles_light();
                if let Some(viso_map::style::Paint::Symbol(sym)) =
                    style.layer_mut("road-name").map(|l| &mut l.paint)
                {
                    sym.spacing = 200.0;
                }
                style
            })
            .handle(slot)
        },
        60,
    );
    picture(&app, "labels-curved-repeat");
    let bent = handle.label_glyphs(app.states());
    let waves = bent.iter().filter(|b| b.text == "Long Wavy Road").count();
    assert!(waves >= 2, "repeats along the line: {bent:?}");
    // No two glyphs of different labels overlap.
    let boxes: Vec<(usize, [f32; 2])> = bent
        .iter()
        .enumerate()
        .flat_map(|(i, b)| b.glyphs.iter().map(move |g| (i, g.at)))
        .collect();
    for (i, a) in &boxes {
        for (j, b) in &boxes {
            if i != j {
                assert!((a[0] - b[0]).hypot(a[1] - b[1]) > 6.0, "{a:?} {b:?}");
            }
        }
    }
}

/// A terrain tile of a bowl about `low` (a world point), Terrarium-encoded:
/// rising a meter in `run` across, to `top` meters.
fn bowl_tile(tile: TileId, low: WorldPoint, top: f64, run: f64) -> Vec<u8> {
    let n = 256u32;
    let span = f64::from(1u32 << tile.z);
    let mut bgra = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let wx = (f64::from(tile.x) + (f64::from(x) + 0.5) / f64::from(n)) / span;
            let wy = (f64::from(tile.y) + (f64::from(y) + 0.5) / f64::from(n)) / span;
            // A world unit is about 24 500 km at Berlin's latitude.
            let d = (wx - low.x).hypot(wy - low.y) * 24_500_000.0;
            let meters = (d / run).min(top);
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
fn tilted_over_relief_a_bent_label_stands_on_it() {
    use viso_map::RasterSource;
    use viso_map::style::{DemEncoding, Terrain};
    let camera = Camera::new(MIDDLE, 15.0)
        .with_bearing(20.0)
        .with_pitch(50.0);
    // The ground rising all round the view's middle, so the label away
    // from it stands high.
    let low = MIDDLE.to_world();
    let mut runs = Vec::new();
    for relief in [false, true] {
        let (app, handle) = run(
            move |slot| {
                let mut style = builtin::openmaptiles_light();
                if relief {
                    style = style.with_terrain(Terrain::new("dem", DemEncoding::Terrarium));
                }
                map_view(|tile: TileId| viso_map::Reply::ready(Ok(Some(tile_of(tile)))))
                    .camera(camera)
                    .style(style)
                    .raster_source(
                        "dem",
                        RasterSource::new(
                            move |tile: TileId| {
                                viso_map::Reply::ready(Ok(Some(bowl_tile(tile, low, 600.0, 3.0))))
                            },
                            0,
                            14,
                        ),
                    )
                    .languages(&["en"])
                    .handle(slot)
            },
            90,
        );
        picture(
            &app,
            if relief {
                "labels-curved-on-relief"
            } else {
                "labels-curved-flat-p50"
            },
        );
        let bent = handle.label_glyphs(app.states());
        let label = bent
            .into_iter()
            .find(|b| b.text == "Curved Avenue")
            .unwrap_or_else(|| panic!("relief {relief}: no bent label"));
        let n = label.glyphs.len() as f32;
        let mid = label
            .glyphs
            .iter()
            .fold([0.0, 0.0], |m, g| [m[0] + g.at[0] / n, m[1] + g.at[1] / n]);
        runs.push(mid);
    }
    // Raised on the rising ground, it shows higher in the view than on the
    // plane.
    assert!(runs[1][1] + 10.0 < runs[0][1], "{runs:?}");
}

/// Real streets of Luxembourg at several zooms, turns and tilts, when
/// `VISO_MAP_LUXEMBOURG` names a directory of their tiles.
#[test]
fn real_streets_bend_their_names() {
    let Some(dir) = std::env::var_os("VISO_MAP_LUXEMBOURG").map(std::path::PathBuf::from) else {
        return;
    };
    for (zoom, bearing, pitch, dark) in [
        (16.0, 0.0, 0.0, false),
        (16.5, 40.0, 0.0, false),
        (15.5, 200.0, 0.0, true),
        (16.2, 120.0, 50.0, false),
        (16.0, 300.0, 60.0, true),
    ] {
        let dir = dir.clone();
        let camera = Camera::new(LngLat::new(6.1300, 49.6110), zoom)
            .with_bearing(bearing)
            .with_pitch(pitch);
        let (app, handle) = run(
            move |slot| {
                let dir = dir.clone();
                map_view(move |tile: TileId| {
                    let path = dir.join(format!("{}-{}-{}.pbf", tile.z, tile.x, tile.y));
                    viso_map::Reply::ready(Ok(std::fs::read(path).ok()))
                })
                .camera(camera)
                .style(if dark {
                    builtin::shortbread_dark()
                } else {
                    builtin::shortbread_light()
                })
                .languages(&["fr"])
                .handle(slot)
            },
            90,
        );
        picture(
            &app,
            &format!("labels-luxembourg-z{zoom}-b{bearing}-p{pitch}"),
        );
        assert!(
            !handle.label_glyphs(app.states()).is_empty(),
            "z{zoom} b{bearing} p{pitch}"
        );
    }
}
