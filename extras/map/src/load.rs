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

use crate::build::{BuildParams, EXTENT, build_vector};
use crate::camera::TILE_SIZE;
use crate::mvt::{self, VectorTile};
use crate::pyramid::BuildKey;
use crate::source::TileId;
use crate::style::Style;
use crate::tess::MeshBuilder;

/// A built tile: what it decoded to, kept to rebuild it and to answer
/// queries, and its drawing, `None` when it draws nothing.
#[derive(Debug)]
pub struct Built {
    pub tile: Arc<VectorTile>,
    pub drawing: Option<Drawing>,
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

/// Builds `input` of `tile` with `style` for `key` here and now.
pub fn build_now(
    input: Input,
    tile: TileId,
    style: &Style,
    key: BuildKey,
) -> Result<Built, ServiceError> {
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
    let mesh = SCRATCH.with(|b| build_vector(&decoded, &params, &mut b.borrow_mut()));
    let bytes = mesh.bytes() + decoded_bytes(&decoded).max(raw * 2);
    let drawing = (!mesh.is_empty()).then(|| {
        let mesh = Mesh::new(mesh.vertices, mesh.indices);
        Drawing::new(vec![Primitive::Mesh(mesh)], Vec2::new(EXTENT, EXTENT))
    });
    Ok(Built {
        tile: decoded,
        drawing,
        bytes,
        key,
    })
}

/// About what a decoded tile holds.
fn decoded_bytes(tile: &VectorTile) -> usize {
    tile.layers
        .iter()
        .map(|l| {
            l.features
                .iter()
                .map(|f| f.tags.len() * 4 + 64)
                .sum::<usize>()
                + l.keys.iter().map(|k| k.len() + 24).sum::<usize>()
                + l.values.len() * 32
        })
        .sum()
}

/// A build of `input` queued on the worker pool, and the flag that stops it
/// if raised before it starts.
pub fn build_off_loop(
    input: Input,
    tile: TileId,
    style: Arc<Style>,
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
        done.complete(build_now(input, tile, &style, key));
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
        let style = Style::new("t", Color::WHITE).with(Layer::fill("w", "water", Color::BLACK));
        let tile = TileId::new(3, 1, 1);
        let key = BuildKey { style: 1, band: 3 };
        let built = build_now(Input::Bytes(bytes()), tile, &style, key).unwrap();
        let drawing = built.drawing.as_ref().unwrap();
        assert_eq!(drawing.primitives().len(), 1);
        assert_eq!(drawing.extent(), Vec2::new(EXTENT, EXTENT));
        assert!(built.bytes > 0);
        let again = build_now(Input::Decoded(built.tile.clone()), tile, &style, key).unwrap();
        assert!(Arc::ptr_eq(&again.tile, &built.tile));
        let empty = Style::new("t", Color::WHITE);
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
        let style =
            Arc::new(Style::new("t", Color::WHITE).with(Layer::fill("w", "water", Color::BLACK)));
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
