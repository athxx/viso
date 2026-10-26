//! Turning a tile's bytes into a drawing, off the loop thread.
//!
//! A tile is decoded and built on the shared worker pool
//! ([`viso_ui::work`]), one job per tile, each worker keeping its own
//! tessellation scratch; the job answers through a [`Reply`] the loop's task
//! awaits. A job whose tile is no longer wanted by the time it starts does
//! nothing.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use viso_render::{Mesh, Primitive};
use viso_services::{Reply, ServiceError, reply};
use viso_ui::{Drawing, Vec2};

use crate::build::{BuildParams, EXTENT, TileMesh, build_layers_in};
use crate::camera::TILE_SIZE;
use crate::geojson::DataLayer;
use crate::label::{TileLabel, extract};
use crate::mvt::{self, VectorTile};
use crate::pyramid::BuildKey;
use crate::source::TileId;
use crate::style::{Paint, Style};
use crate::tess::MeshBuilder;

/// A built tile: what it decoded to, kept to rebuild it and to answer
/// queries, and its drawing, `None` when it draws nothing.
#[derive(Debug)]
pub struct Built {
    pub tile: Arc<VectorTile>,
    /// The app's data layers' parts in the tile.
    pub data: Vec<mvt::Layer>,
    pub drawing: Option<Drawing>,
    /// What it draws over the style's image layers.
    pub over: Option<Drawing>,
    /// The labels it offers, in the order they place.
    pub labels: Vec<TileLabel>,
    /// What it holds: its mesh and, roughly, its decoded features.
    pub bytes: usize,
    pub key: BuildKey,
}

/// What a build starts from.
pub enum Input {
    /// The tile as the source sent it, perhaps compressed.
    Bytes(Vec<u8>),
    /// A tile decoded before, built again for another style or zoom.
    Decoded(Arc<VectorTile>),
}

thread_local! {
    static SCRATCH: RefCell<MeshBuilder> = RefCell::new(MeshBuilder::new());
}

/// The zoom a tile of `tile` built for `key` takes its style's values at,
/// and its side on screen there.
pub fn params_for(tile: TileId, key: BuildKey) -> (f32, f32) {
    let scale = f64::from(i32::from(key.band) - i32::from(tile.z)).exp2();
    (f32::from(key.band), (TILE_SIZE * scale) as f32)
}

/// The app's data layers a build cuts its tile's part of.
pub type Data = Arc<[Arc<DataLayer>]>;

/// What a tile is built with besides its own features.
#[derive(Debug)]
pub struct Recipe {
    pub style: Arc<Style>,
    pub data: Data,
    /// The languages names are picked in, each followed by its primary
    /// subtag.
    pub languages: Vec<String>,
}

impl Recipe {
    /// Builds by `style` alone.
    pub fn of(style: Style) -> Self {
        Self {
            style: Arc::new(style),
            data: Arc::new([]),
            languages: Vec::new(),
        }
    }
}

/// Builds `input` of `tile` by `recipe` for `key` here and now.
pub fn build_now(
    input: Input,
    tile: TileId,
    recipe: &Recipe,
    key: BuildKey,
) -> Result<Built, ServiceError> {
    let style = &*recipe.style;
    let (decoded, raw) = match input {
        Input::Bytes(bytes) => {
            let raw = bytes.len();
            let tile = mvt::decode(&bytes)
                .map_err(|e| ServiceError::Failed(format!("tile {tile}: {e}")))?;
            (Arc::new(tile), raw)
        }
        Input::Decoded(tile) => (tile, 0),
    };
    let (zoom, tile_points) = params_for(tile, key);
    let params = BuildParams {
        style,
        zoom,
        tile_points,
    };
    let data: Vec<mvt::Layer> = recipe.data.iter().filter_map(|d| d.slice(tile)).collect();
    let layers: Vec<&mvt::Layer> = decoded.layers.iter().chain(&data).collect();
    // The style's layers before its first image layer draw beneath the
    // images, the rest over them.
    let split = style
        .layers
        .iter()
        .position(|l| matches!(l.paint, Paint::Raster { .. } | Paint::Hillshade { .. }))
        .unwrap_or(style.layers.len());
    let (mesh, over) = SCRATCH.with(|b| {
        let b = &mut b.borrow_mut();
        let under = build_layers_in(&layers, &params, 0..split, b);
        let over = (split < style.layers.len())
            .then(|| build_layers_in(&layers, &params, split..usize::MAX, b));
        (under, over)
    });
    let mut labels = Vec::new();
    extract(
        &layers,
        style,
        zoom,
        tile_points,
        &recipe.languages,
        &mut labels,
    );
    let bytes = mesh.bytes()
        + over.as_ref().map_or(0, TileMesh::bytes)
        + decoded_bytes(&decoded).max(raw * 2)
        + data.iter().map(layer_bytes).sum::<usize>()
        + labels.len() * (std::mem::size_of::<TileLabel>() + 16);
    let drawing_of = |mesh: TileMesh| {
        (!mesh.is_empty()).then(|| {
            let mesh = Mesh::new(mesh.vertices, mesh.indices);
            Drawing::new(vec![Primitive::Mesh(mesh)], Vec2::new(EXTENT, EXTENT))
        })
    };
    let drawing = drawing_of(mesh);
    let over = over.and_then(drawing_of);
    Ok(Built {
        tile: decoded,
        data,
        drawing,
        over,
        labels,
        bytes,
        key,
    })
}

/// About what a decoded tile holds.
fn decoded_bytes(tile: &VectorTile) -> usize {
    tile.layers.iter().map(layer_bytes).sum()
}

fn layer_bytes(l: &mvt::Layer) -> usize {
    l.features
        .iter()
        .map(|f| f.tags.len() * 4 + 64)
        .sum::<usize>()
        + l.keys.iter().map(|k| k.len() + 24).sum::<usize>()
        + l.values.len() * 32
}

/// A build of `input` queued on the worker pool, and the flag that stops it
/// if raised before it starts.
pub fn build_off_loop(
    input: Input,
    tile: TileId,
    recipe: Arc<Recipe>,
    key: BuildKey,
) -> (Reply<Built>, Arc<AtomicBool>) {
    let (done, answer) = reply();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    viso_ui::work::submit(move || {
        if stopped.load(Ordering::Relaxed) {
            done.complete(Err(ServiceError::Cancelled));
            return;
        }
        done.complete(build_now(input, tile, &recipe, key));
    });
    (answer, stop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::{GeomType, TileEncoder};
    use crate::style::{Color, Layer};

    fn bytes() -> Vec<u8> {
        let mut t = TileEncoder::new();
        let mut water = t.layer("water", 4096);
        let sq: &[[i32; 2]] = &[[0, 0], [2048, 0], [2048, 2048], [0, 2048]];
        water.feature(None, GeomType::Polygon, &[sq], &[]);
        water.finish();
        t.finish_gzip()
    }

    #[test]
    fn a_tile_builds_into_one_placed_mesh_and_rebuilds_from_what_it_decoded() {
        let style =
            Recipe::of(Style::new("t", Color::WHITE).with(Layer::fill("w", "water", Color::BLACK)));
        let tile = TileId::new(3, 1, 1);
        let key = BuildKey { style: 1, band: 3 };
        let built = build_now(Input::Bytes(bytes()), tile, &style, key).unwrap();
        let drawing = built.drawing.as_ref().unwrap();
        assert_eq!(drawing.primitives().len(), 1);
        assert_eq!(drawing.extent(), Vec2::new(EXTENT, EXTENT));
        assert!(built.bytes > 0);
        let again = build_now(Input::Decoded(built.tile.clone()), tile, &style, key).unwrap();
        assert!(Arc::ptr_eq(&again.tile, &built.tile));
        let empty = Recipe::of(Style::new("t", Color::WHITE));
        assert!(
            build_now(Input::Decoded(built.tile), tile, &empty, key)
                .unwrap()
                .drawing
                .is_none()
        );
        assert!(build_now(Input::Bytes(vec![0xff, 3]), tile, &style, key).is_err());
    }

    #[test]
    fn an_overzoomed_tile_is_built_for_its_band() {
        let tile = TileId::new(14, 0, 0);
        assert_eq!(
            params_for(tile, BuildKey { style: 0, band: 14 }),
            (14.0, 512.0)
        );
        assert_eq!(
            params_for(tile, BuildKey { style: 0, band: 16 }),
            (16.0, 2048.0)
        );
    }

    #[test]
    fn a_job_runs_on_the_pool_or_inline_and_answers() {
        let style = Arc::new(Recipe::of(Style::new("t", Color::WHITE).with(Layer::fill(
            "w",
            "water",
            Color::BLACK,
        ))));
        let key = BuildKey { style: 1, band: 3 };
        let tile = TileId::new(3, 0, 0);
        let (mut answer, _) = build_off_loop(Input::Bytes(bytes()), tile, style.clone(), key);
        let result = loop {
            viso_ui::work::run_inline(std::time::Duration::MAX);
            if let Some(result) = answer.try_take() {
                break result;
            }
            std::thread::yield_now();
        };
        assert!(result.unwrap().drawing.is_some());
        if viso_ui::work::threads() == 0 {
            let (mut answer, stop) = build_off_loop(Input::Bytes(bytes()), tile, style, key);
            stop.store(true, Ordering::Relaxed);
            viso_ui::work::run_inline(std::time::Duration::MAX);
            assert!(matches!(
                answer.try_take(),
                Some(Err(ServiceError::Cancelled))
            ));
        }
    }
}
