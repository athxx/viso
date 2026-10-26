//! What the map costs: a tile's build off the loop (decode, tessellate,
//! extract labels), its raise into a 3D model for a tilted camera (flat and
//! over relief), and the view's own work in a frame of a drag (its pointer
//! handler and structure hook, the frame's layout and paint aside).
//!
//! Run release: `cargo bench -p viso-map`. A real OpenMapTiles tile is
//! built too when `VISO_MAP_TILES` names a directory of `z-x-y.pbf` tiles.

#[path = "../tests/support/mod.rs"]
mod support;

use std::cell::{Cell, RefCell};
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use viso::__test_support::{drive_scripted, probe_frames};
use viso::platform::{Modifiers, PointerButtons, PointerPhase, RawEvent, RawPointer, WindowId};
use viso::prelude::*;
use viso::ui::{BuildCx, Component, WindowConfig};
use viso_map::load::{Input, Recipe, build_now};
use viso_map::pyramid::BuildKey;
use viso_map::style::builtin;
use viso_map::{Camera, LngLat, TileId, map_view};

fn tile_build(c: &mut Criterion) {
    let recipe = Recipe::of(builtin::openmaptiles_light());
    let key = BuildKey { style: 1, band: 14 };
    let tile = TileId::new(14, 8802, 5373);
    let bytes = support::synthetic(tile);
    c.bench_function("tile_build/synthetic_z14", |b| {
        b.iter(|| black_box(build_now(Input::Bytes(bytes.clone()), tile, &recipe, key)));
    });
    let decoded = build_now(Input::Bytes(bytes), tile, &recipe, key)
        .expect("builds")
        .tile;
    c.bench_function("tile_build/synthetic_z14_rebuild", |b| {
        b.iter(|| {
            black_box(build_now(
                Input::Decoded(Arc::clone(&decoded)),
                tile,
                &recipe,
                key,
            ))
        });
    });
    if let Some(dir) = support::tiles_dir() {
        let real = TileId::new(14, 8802, 5373);
        if let Ok(bytes) = std::fs::read(dir.join(format!("{}-{}-{}.pbf", real.z, real.x, real.y)))
        {
            c.bench_function("tile_build/berlin_z14", |b| {
                b.iter(|| black_box(build_now(Input::Bytes(bytes.clone()), real, &recipe, key)));
            });
        }
    }
}

/// A real Shortbread tile of Luxembourg City (see `fixtures/ATTRIBUTION`).
const LUXEMBOURG: &[u8] = include_bytes!("../tests/fixtures/luxembourg-14-8469-5582.mvt.gz");

fn tile_raise(c: &mut Criterion) {
    use viso_map::tilt::{Raise, Relief, raise, units_per_meter};
    let style = builtin::shortbread_light();
    let recipe = Recipe::of(style.clone());
    let tile = TileId::new(14, 8469, 5582);
    let key = BuildKey { style: 1, band: 16 };
    let built = build_now(Input::Bytes(LUXEMBOURG.to_vec()), tile, &recipe, key).expect("builds");
    let meshes: Vec<viso::render::Mesh> = [&built.drawing, &built.over]
        .into_iter()
        .flatten()
        .flat_map(|d| d.primitives().iter())
        .filter_map(|p| match p {
            viso::render::Primitive::Mesh(m) => Some(m.clone()),
            _ => None,
        })
        .collect();
    let layers: Vec<&viso_map::mvt::Layer> = built.tile.layers.iter().collect();
    let dem = viso_map::Dem {
        width: 256,
        height: 256,
        meters: (0..256 * 256)
            .map(|i| {
                300.0 + 80.0 * ((i % 256) as f32 / 40.0).sin() * ((i / 256) as f32 / 55.0).cos()
            })
            .collect(),
    };
    for (name, relief) in [
        ("tile_raise/luxembourg_z16", None),
        (
            "tile_raise/luxembourg_z16_relief",
            Some(Relief {
                dem: &dem,
                origin: [0.0, 0.0],
                span: 1.0,
                scale: units_per_meter(tile),
            }),
        ),
    ] {
        let r = Raise {
            tile,
            band: 16,
            meshes: &meshes,
            layers: &layers,
            style: &style,
            background: [1.0; 4],
            relief,
        };
        c.bench_function(name, |b| b.iter(|| black_box(raise(&r))));
    }
}

const WIN: WindowId = WindowId(1);

struct MapApp;

impl Application for MapApp {
    fn new(_: &mut AppCx) -> Self {
        MapApp
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            size: (640.0, 480.0),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        map_view(|tile: TileId| viso_map::Reply::ready(Ok(Some(support::synthetic(tile)))))
            .camera(Camera::new(LngLat::new(13.40, 52.52), 13.0).with_bearing(15.0))
            .style(builtin::openmaptiles_light())
            .build(cx);
    }
}

thread_local! {
    static SINCE: Cell<Option<Instant>> = const { Cell::new(None) };
    static SPENT: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

fn around(on: bool) {
    if on {
        SINCE.with(|s| s.set(Some(Instant::now())));
    } else if let Some(t) = SINCE.with(Cell::take) {
        SPENT.with(|s| s.set(s.get() + t.elapsed()));
    }
}

fn mouse(x: f64, y: f64, phase: PointerPhase, alt: bool) -> RawEvent {
    let m = Modifiers {
        alt,
        ..Modifiers::default()
    };
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        x,
        y,
        PointerButtons::PRIMARY,
        m,
        phase,
    ))
}

/// The view's own time in each of the last `frames` frames of a drag back
/// and forth, turning the map with `alt`, after the tiles load and the drag
/// warms up.
fn drag(frames: usize, alt: bool) -> Vec<Duration> {
    let warm = 24usize;
    let mut script: Vec<RawEvent> = (0..12)
        .map(|_| RawEvent::RedrawRequested { window: WIN })
        .collect();
    script.push(mouse(320.0, 240.0, PointerPhase::Down, alt));
    // Past the drag's slop, then back and forth about where it got to.
    for i in 0..warm + frames {
        let d = (i as f64 * 0.7).sin() * 3.0 + (i.min(8) as f64) * 4.0;
        script.push(mouse(320.0 + d, 240.0 + d / 2.0, PointerPhase::Moved, alt));
        script.push(RawEvent::RedrawRequested { window: WIN });
    }
    let spent: std::rc::Rc<RefCell<Vec<Duration>>> = std::rc::Rc::default();
    let log = std::rc::Rc::clone(&spent);
    probe_frames(Some(Box::new(move |_, _| {
        log.borrow_mut()
            .push(SPENT.with(|s| s.replace(Duration::ZERO)));
    })));
    viso_map::view::probe::set(Some(around));
    drive_scripted::<MapApp>(script, Duration::from_millis(16));
    viso_map::view::probe::set(None);
    probe_frames(None);
    let spent = spent.take();
    let from = spent.len().saturating_sub(frames);
    spent[from..].to_vec()
}

fn pan_frame(c: &mut Criterion) {
    let mut g = c.benchmark_group("view_frame");
    g.sample_size(10);
    // Each sample times at most 400 frames, scaled to the count asked.
    let scaled = |n: u64, alt: bool| {
        let frames = drag(n.clamp(1, 400) as usize, alt);
        let total: Duration = frames.iter().sum();
        total.mul_f64(n as f64 / frames.len().max(1) as f64)
    };
    g.bench_function("pan", |b| b.iter_custom(|n| scaled(n, false)));
    g.bench_function("turn", |b| b.iter_custom(|n| scaled(n, true)));
    g.finish();
}

criterion_group!(benches, tile_build, tile_raise, pan_frame);
criterion_main!(benches);
