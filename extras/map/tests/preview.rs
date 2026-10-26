//! Single real tiles built with each built-in style and drawn by the CPU
//! raster, written as pictures to `VISO_MAP_OUT`: a look at the styles
//! before any view is involved. Needs `VISO_MAP_TILES`.

mod support;

use viso::gpu::{CpuRaster, GpuBackend, RawWindowHandle};
use viso::render::{Border, Mesh, MeshPlace, Primitive, Quad, Rect, Renderer, Rgba};
use viso_map::build::{BuildParams, EXTENT, build_vector};
use viso_map::mvt;
use viso_map::style::builtin;
use viso_map::tess::MeshBuilder;

const SIDE: u32 = 512;

fn draw(mesh: &viso_map::build::TileMesh, background: [f32; 4]) -> Vec<u8> {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, SIDE, SIDE);
    let format = gpu.surface_format(surface);
    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([SIDE as f32, SIDE as f32], 1.0);
    let k = SIDE as f32 / EXTENT;
    let frame = vec![
        Primitive::Quad(Quad {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                w: SIDE as f32,
                h: SIDE as f32,
            },
            color: Rgba::new(background[0], background[1], background[2], 1.0),
            radius: 0.0,
            border: Border::NONE,
        }),
        Primitive::Mesh(
            Mesh::new(mesh.vertices.clone(), mesh.indices.clone()).placed(MeshPlace {
                scale: [k, k],
                offset: [0.0, 0.0],
            }),
        ),
    ];
    r.upload(&mut gpu, &frame);
    r.submit(
        &mut gpu,
        surface,
        [0.0, 0.0, 0.0, 1.0],
        [SIDE as f32, SIDE as f32],
    );
    gpu.read_pixels_bgra8(surface)
}

#[test]
fn layer_by_layer() {
    let (Some(dir), Some(_)) = (support::tiles_dir(), support::out_dir()) else {
        return;
    };
    let Ok(layer) = std::env::var("VISO_MAP_LAYER") else {
        return;
    };
    let tile = mvt::decode(&std::fs::read(dir.join("14-14552-6451.pbf")).unwrap()).unwrap();
    let full = builtin::openmaptiles_light();
    let mut style = full.clone();
    if let Some(upto) = layer.strip_prefix("upto:") {
        let n = style.layers.iter().position(|l| l.id == upto).unwrap() + 1;
        style.layers.truncate(n);
    } else {
        style.layers.retain(|l| l.id.starts_with(&layer));
    }
    eprintln!(
        "{:?}",
        style.layers.iter().map(|l| &l.id).collect::<Vec<_>>()
    );
    let params = BuildParams {
        style: &style,
        zoom: 14.5,
        tile_points: SIDE as f32,
    };
    let mesh = build_vector(&tile, &params, &mut MeshBuilder::new());
    let bg = style.background.at(14.0).linear(1.0);
    support::save(
        &format!("layer-{}", layer.replace(':', "-")),
        SIDE,
        SIDE,
        &draw(&mesh, bg),
    );
}

#[test]
fn vertices_per_layer() {
    let Some(dir) = support::tiles_dir() else {
        return;
    };
    let Ok(name) = std::env::var("VISO_MAP_COUNT") else {
        return;
    };
    let z: f32 = name.split('-').next().unwrap().parse().unwrap();
    let tile = mvt::decode(&std::fs::read(dir.join(format!("{name}.pbf"))).unwrap()).unwrap();
    let full = builtin::openmaptiles_light();
    let mut rows = Vec::new();
    for l in &full.layers {
        let mut style = full.clone();
        style.layers.retain(|x| x.id == l.id);
        let params = BuildParams {
            style: &style,
            zoom: z + 0.5,
            tile_points: SIDE as f32,
        };
        let mesh = build_vector(&tile, &params, &mut MeshBuilder::new());
        if !mesh.vertices.is_empty() {
            rows.push((mesh.vertices.len(), l.id.clone()));
        }
    }
    rows.sort();
    for (n, id) in rows {
        eprintln!("{n:8} {id}");
    }
}

#[test]
fn real_tiles_preview() {
    let (Some(dir), Some(_)) = (support::tiles_dir(), support::out_dir()) else {
        return;
    };
    for name in [
        "14-8802-5373",
        "12-2200-1343",
        "10-550-335",
        "6-34-20",
        "14-14552-6451",
        "3-4-2",
    ] {
        let Ok(bytes) = std::fs::read(dir.join(format!("{name}.pbf"))) else {
            eprintln!("no tile {name}");
            continue;
        };
        let tile = mvt::decode(&bytes).unwrap();
        let z: f32 = name.split('-').next().unwrap().parse().unwrap();
        for style in [builtin::openmaptiles_light(), builtin::openmaptiles_dark()] {
            let params = BuildParams {
                style: &style,
                zoom: z + 0.5,
                tile_points: SIDE as f32,
            };
            let start = std::time::Instant::now();
            let mesh = build_vector(&tile, &params, &mut MeshBuilder::new());
            let took = start.elapsed();
            eprintln!(
                "{name} {}: {} vertices, {} triangles, {} KiB, built in {took:?}",
                style.name,
                mesh.vertices.len(),
                mesh.indices.len() / 3,
                mesh.bytes() / 1024
            );
            let bg = style.background.at(z).linear(1.0);
            let frame = draw(&mesh, bg);
            support::save(&format!("tile-{name}-{}", style.name), SIDE, SIDE, &frame);
        }
    }
}
