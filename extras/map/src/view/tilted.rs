//! A tilted view: the tiles drawn, each raised into a 3D model on the
//! worker pool, shown in one scene that fills the view, seen in perspective
//! by a camera that matches the view's own projection.
//!
//! The scene sits beneath the overlay, so labels, markers and the puck stay
//! flat, placed where the tilted projection puts their places. A tile's
//! model is handed to the asset table and held by the scene's node while
//! the tile is drawn; the shapes and the accuracy circle are raised the same
//! way, on the loop, as they change. With a terrain in the style, a tile
//! waits for the elevations under it before it is raised over them.

use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use std::cell::RefCell;

use viso_math::{LinearStraight, Mat4, vec3};
use viso_render::asset::{AssetKind, ResourceId, ResourceRevision};
use viso_render::scene3d::model::ModelAsset;
use viso_render::scene3d::{
    AmbientOcclusion, Camera as Eye, Lighting, Projection, SceneModel, Shadows,
};
use viso_ui::context::UpdateCx;
use viso_ui::scenes::{SceneSlot, SceneSlots};
use viso_ui::{
    Asset, AssetBytes, AssetPayload, AssetSource, BuildCx, Continuation, LeafStyle, NodeId, Size,
    StructureCx, TaskId, Vec2,
};

use super::core::Core;
use super::rasters::{Rasters, Ready};
use crate::camera::{REACH, TILE_SIZE, View};
use crate::geo::WorldPoint;
use crate::load::{Built, off_loop};
use crate::overlay::ShapeMesh;
use crate::pyramid::Slot;
use crate::raster::{Dem, RasterSource, dem_off_loop};
use crate::source::TileId;
use crate::style::{Paint, Style, Terrain};
use crate::tilt::{
    Imagery, Raise, Relief, image_order, raise, raise_image, raise_shape, units_per_meter,
};

/// How far a tile drawn for another zoom sits beneath (an ancestor) or over
/// (a descendant) the tiles of the zoom drawn, per level, in points of the
/// level drawn: a finer tile's drawing covers a coarser one beneath it.
const LEVEL_STEP: f32 = 0.6;

/// How far shapes lie over the ground, in points of the level drawn; and
/// further over relief, which they follow only at their own points.
const SHAPE_LIFT: f32 = 0.5;
const SHAPE_LIFT_RELIEF: f32 = 2.0;
/// How far each of a shape's colors lies over the one drawn before it.
const SHAPE_STEP: f32 = 0.1;
/// How high the boxes of shapes rise, in points of the level drawn: far over
/// the eye, so the scene blends them after every tile's ground; each shape
/// a step higher than the one before, so a later one draws over it.
const SHAPES_ABOVE: f32 = 1.0e6;
const SHAPE_ORDER: f32 = 1.0e5;

/// The terrain tiles kept beyond those under the tiles drawn.
const KEEP_ELEVATIONS: usize = 64;

fn next_id() -> ResourceId {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    ResourceId(0x6d61_7300_0000_0000 | NEXT.fetch_add(1, Ordering::Relaxed))
}

/// A model handed to the asset table, as the scene's node shows it.
fn model_asset(id: ResourceId) -> Asset {
    Asset {
        id,
        kind: AssetKind::Model,
        source: AssetSource::Bytes(AssetBytes::Static(&[])),
    }
}

enum Stage {
    /// Raising on the worker pool.
    Raising {
        task: Option<TaskId>,
        stop: Arc<AtomicBool>,
    },
    /// Raised, not yet handed to the asset table.
    Raised(Arc<ModelAsset>),
    /// Handed to the asset table as `id`; resident once it is uploaded.
    Held(ResourceId, Option<Arc<SceneModel>>),
    /// It draws nothing.
    Nothing,
}

struct TileModel {
    /// The build it raises.
    built: Rc<Built>,
    stage: Stage,
    used: bool,
}

/// An image layer's tile laid over a tile drawn: by the image layer and
/// the image's tile.
type Pictures = HashMap<(usize, TileId), Picture>;

struct Picture {
    /// The image it lays.
    image: Ready,
    stage: Stage,
    /// The opacity its layer draws at.
    tint: f32,
    used: bool,
}

enum Elevations {
    Loading {
        task: Option<TaskId>,
        stop: Arc<AtomicBool>,
    },
    Ready(Arc<Dem>),
    Missing,
}

/// The terrain of the style, as far as it is loaded.
struct Ground {
    terrain: Terrain,
    source: RasterSource,
    tiles: HashMap<TileId, Elevations>,
    /// Bumped as elevations arrive.
    revision: u64,
}

impl Ground {
    /// The terrain tile under `tile`, if the source reaches its level.
    fn under(&self, tile: TileId) -> Option<TileId> {
        let z = tile.z.clamp(self.source.min_zoom, self.source.max_zoom);
        (z <= tile.z).then(|| tile.ancestor(tile.z - z))
    }

    /// The elevations under `tile`, `None` while they load, `Some(None)`
    /// where there are none.
    fn relief(&self, tile: TileId) -> Option<Option<(TileId, Arc<Dem>)>> {
        let Some(under) = self.under(tile) else {
            return Some(None);
        };
        match self.tiles.get(&under) {
            Some(Elevations::Ready(dem)) => Some(Some((under, Arc::clone(dem)))),
            Some(Elevations::Missing) => Some(None),
            _ => None,
        }
    }

    /// The elevation in meters at world point `w`, from the deepest terrain
    /// tile loaded over it at `z` or above.
    fn meters(&self, w: WorldPoint, z: u8) -> Option<f32> {
        let mut tile = w.tile(z.clamp(self.source.min_zoom, self.source.max_zoom));
        loop {
            if let Some(Elevations::Ready(dem)) = self.tiles.get(&tile) {
                let n = f64::from(TileId::span(tile.z));
                let u = w.x.rem_euclid(1.0) * n - f64::from(tile.x);
                let v = w.y * n - f64::from(tile.y);
                return Some(dem.sample(u.clamp(0.0, 1.0), v.clamp(0.0, 1.0)));
            }
            if tile.z <= self.source.min_zoom {
                return None;
            }
            tile = tile.parent()?;
        }
    }
}

/// Where a tilted view over relief raises its places: the ground's height
/// under them, and at the focus, whose plane the view's projection lies in.
pub(crate) struct Lift<'a> {
    ground: &'a Ground,
    zoom: u8,
    focus: f64,
}

impl Lift<'_> {
    /// The ground's height under `w`, in meters raised by the terrain's
    /// exaggeration; sea level where no elevations are loaded.
    pub fn meters(&self, w: WorldPoint) -> f64 {
        let m = self.ground.meters(w, self.zoom).unwrap_or(0.0);
        f64::from(m * self.ground.terrain.exaggeration)
    }

    /// The ground's height at the focus, in meters raised.
    pub fn focus(&self) -> f64 {
        self.focus
    }

    /// Where `view` shows world point `w` on the relief under it.
    pub fn to_view(&self, view: &View, w: WorldPoint) -> [f64; 2] {
        view.to_view_at(w, self.points(view, w, self.meters(w)))
    }

    /// How far over the plane `view` projects onto ground `meters` high at
    /// `w` lies, in points of its zoom.
    pub fn points(&self, view: &View, w: WorldPoint, meters: f64) -> f64 {
        (meters - self.focus) * view.camera.scale() / w.meters_per_unit().max(1e-9)
    }
}

/// A shape raised, as the asset table holds it (no id where it draws
/// nothing), and where its frame lies.
struct ShapeModel {
    id: Option<ResourceId>,
    model: Option<Arc<SceneModel>>,
    origin: WorldPoint,
    zoom: f64,
    middle: WorldPoint,
}

impl ShapeModel {
    fn ready(&self) -> bool {
        self.id.is_none() || self.model.is_some()
    }
}

/// Lets go of the models of `shapes`.
fn release_shapes(s: &mut StructureCx<'_>, slot: SceneSlot, shapes: &mut Vec<ShapeModel>) {
    for shape in shapes.drain(..) {
        if let Some(id) = shape.id {
            s.store.release_scene_model(slot, id);
        }
    }
}

/// The view's tilted half.
pub(crate) struct Tilted {
    root: NodeId,
    /// The overlay: the scene goes beneath it.
    overlay: NodeId,
    scene: Option<(NodeId, SceneSlot)>,
    size: [f32; 2],
    shown: bool,
    models: HashMap<TileId, TileModel>,
    /// The image layers' tiles over each tile drawn.
    pictures: HashMap<TileId, Pictures>,
    shapes: Vec<ShapeModel>,
    /// The shapes as they were, drawn until those raised since are
    /// resident.
    retiring: Vec<ShapeModel>,
    shapes_key: Option<(u64, u8, u64)>,
    ground: Option<Ground>,
    /// Bumped when the tiles are let go of, so a raise arriving after is
    /// dropped.
    generation: u64,
    /// Models handed to the asset table and not yet resident: the frame
    /// clock looks again next frame.
    pub waiting: bool,
}

/// What a tilted apply reads of the rest of the view.
pub(crate) struct Frame<'a> {
    pub view: &'a View,
    pub band: u8,
    /// The zoom of the tiles drawn.
    pub tile_zoom: u8,
    pub anchor: [f64; 2],
    pub style: &'a Arc<Style>,
    pub sources: &'a HashMap<String, RasterSource>,
    /// The image layers, whose tiles lie over the tiles drawn.
    pub rasters: &'a Rasters,
    pub me: &'a Weak<RefCell<Core>>,
}

impl Tilted {
    pub fn new(root: NodeId, overlay: NodeId) -> Self {
        Self {
            root,
            overlay,
            scene: None,
            size: [0.0; 2],
            shown: false,
            models: HashMap::new(),
            pictures: HashMap::new(),
            shapes: Vec::new(),
            retiring: Vec::new(),
            shapes_key: None,
            ground: None,
            generation: 1,
            waiting: false,
        }
    }

    /// Whether the scene is showing.
    pub fn shown(&self) -> bool {
        self.shown
    }

    /// Where the view raises its places over the relief it shows, when
    /// tilted over one; the tiles drawn being of zoom `tile_zoom`.
    pub fn lift(&self, view: &View, tile_zoom: u8) -> Option<Lift<'_>> {
        let ground = self.ground.as_ref().filter(|_| self.shown)?;
        let mut lift = Lift {
            ground,
            zoom: tile_zoom,
            focus: 0.0,
        };
        lift.focus = lift.meters(view.camera.center.to_world());
        Some(lift)
    }

    /// Bumped as elevations arrive.
    pub fn relief_revision(&self) -> u64 {
        self.ground.as_ref().map_or(0, |g| g.revision)
    }

    /// The tiles raised and resident, for the counters a test reads.
    pub fn resident(&self) -> usize {
        self.models
            .values()
            .filter(|m| matches!(m.stage, Stage::Held(_, Some(_))))
            .count()
    }

    /// Hides the scene and lets go of every model: the camera looks
    /// straight down again.
    pub fn hide(&mut self, s: &mut StructureCx<'_>) {
        if !self.shown {
            return;
        }
        self.shown = false;
        self.waiting = false;
        self.generation += 1;
        let Some((node, slot)) = self.scene else {
            return;
        };
        s.store.set_hidden(node, true);
        for (_, m) in self.models.drain() {
            release(s, slot, m.stage);
        }
        self.release_pictures(s, slot);
        release_shapes(s, slot, &mut self.shapes);
        release_shapes(s, slot, &mut self.retiring);
        self.shapes_key = None;
        if let Some(ground) = &mut self.ground {
            for (_, e) in ground.tiles.drain() {
                stop_elevations(s, e);
            }
        }
        self.ground = None;
    }

    /// A tile's model arrived from the worker pool.
    fn arrive(&mut self, generation: u64, tile: TileId, model: Option<ModelAsset>) {
        if generation != self.generation {
            return;
        }
        if let Some(m) = self.models.get_mut(&tile)
            && matches!(m.stage, Stage::Raising { .. })
        {
            m.stage = match model {
                Some(model) => Stage::Raised(Arc::new(model)),
                None => Stage::Nothing,
            };
        }
    }

    /// An image laid over a tile arrived from the worker pool.
    fn arrive_picture(
        &mut self,
        generation: u64,
        tile: TileId,
        key: (usize, TileId),
        model: Option<ModelAsset>,
    ) {
        if generation != self.generation {
            return;
        }
        if let Some(p) = self.pictures.get_mut(&tile).and_then(|p| p.get_mut(&key))
            && matches!(p.stage, Stage::Raising { .. })
        {
            p.stage = match model {
                Some(model) => Stage::Raised(Arc::new(model)),
                None => Stage::Nothing,
            };
        }
    }

    /// A terrain tile arrived.
    fn arrive_elevations(&mut self, generation: u64, tile: TileId, dem: Option<Arc<Dem>>) {
        if generation != self.generation {
            return;
        }
        if let Some(ground) = &mut self.ground
            && let Some(e) = ground.tiles.get_mut(&tile)
            && matches!(e, Elevations::Loading { .. })
        {
            *e = match dem {
                Some(dem) => Elevations::Ready(dem),
                None => Elevations::Missing,
            };
            ground.revision += 1;
        }
    }

    /// Brings the scene to `f`, drawing `drawn` (the tiles in paint order,
    /// each with its build) and the overlays' `framed` drawings.
    pub fn apply<'b>(
        &mut self,
        s: &mut StructureCx<'_>,
        f: &Frame<'_>,
        drawn: &[(Slot, Rc<Built>)],
        framed: impl Iterator<Item = (&'b ShapeMesh, f64)>,
        shapes_revision: u64,
    ) {
        let (node, slot) = self.scene_node(s);
        if !self.shown {
            self.shown = true;
            s.store.set_hidden(node, false);
        }
        self.place(s, node, f.view);
        self.ground_for(s, f, drawn);
        // The tiles' models: raised once per build, let go of as they leave.
        for m in self.models.values_mut() {
            m.used = false;
        }
        for (slot_drawn, built) in drawn {
            let tile = slot_drawn.tile;
            let stale = self
                .models
                .get(&tile)
                .is_none_or(|m| !Rc::ptr_eq(&m.built, built));
            if stale {
                let relief = match &self.ground {
                    Some(g) => match g.relief(tile) {
                        Some(r) => r,
                        // Its elevations are on their way.
                        None => {
                            if let Some(m) = self.models.get_mut(&tile) {
                                m.used = true;
                            }
                            continue;
                        }
                    },
                    None => None,
                };
                if let Some(old) = self.models.remove(&tile) {
                    release(s, slot, old.stage);
                }
                let stage = self.start(s, f, tile, built, relief);
                self.models.insert(
                    tile,
                    TileModel {
                        built: Rc::clone(built),
                        stage,
                        used: true,
                    },
                );
            } else if let Some(m) = self.models.get_mut(&tile) {
                m.used = true;
            }
        }
        let gone: Vec<TileId> = self
            .models
            .iter()
            .filter(|(_, m)| !m.used)
            .map(|(t, _)| *t)
            .collect();
        for tile in gone {
            if let Some(m) = self.models.remove(&tile) {
                release(s, slot, m.stage);
            }
        }
        self.pictures_for(s, slot, f, drawn);
        // Hand what arrived to the asset table, and ask for what it holds.
        self.waiting = false;
        let stages = self.models.values_mut().map(|m| &mut m.stage).chain(
            self.pictures
                .values_mut()
                .flat_map(|p| p.values_mut().map(|p| &mut p.stage)),
        );
        for stage in stages {
            if let Stage::Raised(model) = stage {
                let id = next_id();
                s.store.provide_asset(
                    id,
                    ResourceRevision::default(),
                    AssetPayload::Model(Arc::clone(model)),
                );
                *stage = Stage::Held(id, None);
            }
            if let Stage::Held(id, resident @ None) = stage {
                *resident = s.store.scene_model(slot, &model_asset(*id));
                // Waiting for it, or drawing it for the first time: one
                // frame more either way.
                self.waiting = true;
            }
        }
        let framed: Vec<(&ShapeMesh, f64)> = framed.collect();
        self.shapes_for(s, slot, f, &framed, shapes_revision);
        self.fill(s, slot, f, drawn);
    }

    /// The scene's node and slot, made the first time.
    fn scene_node(&mut self, s: &mut StructureCx<'_>) -> (NodeId, SceneSlot) {
        if let Some(scene) = self.scene
            && s.store.arena().is_live(scene.0)
        {
            return scene;
        }
        let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, self.root);
        let leaf = cx.leaf(LeafStyle {
            size: Size::fixed(1.0, 1.0),
            style: viso_ui::BoxStyle::NONE,
        });
        let slot = cx.scene(leaf).expect("a live node");
        let node = leaf.id();
        // Beneath the overlay.
        s.store.arena_detach(node);
        s.store
            .arena_insert_before(self.root, node, Some(self.overlay));
        s.store.mark_dirty(
            self.root,
            viso_ui::DirtyClass::MEASURE | viso_ui::DirtyClass::LAYOUT | viso_ui::DirtyClass::PAINT,
        );
        self.scene = Some((node, slot));
        self.size = [1.0; 2];
        (node, slot)
    }

    /// Sizes the scene's node about the view's focus, which its camera looks
    /// at through its middle; the view clips what reaches past it.
    fn place(&mut self, s: &mut StructureCx<'_>, node: NodeId, view: &View) {
        let [w, h] = view.size;
        let f = view.focus();
        let size = [
            (2.0 * f[0].max(w - f[0])).max(1.0) as f32,
            (2.0 * f[1].max(h - f[1])).max(1.0) as f32,
        ];
        if size != self.size {
            self.size = size;
            s.store.set_fixed_size(node, Size::fixed(size[0], size[1]));
        }
        s.store.set_translate(
            node,
            Vec2::new((f[0] - w / 2.0) as f32, (f[1] - h / 2.0) as f32),
        );
    }

    /// Loads the elevations under the tiles drawn, when the style has a
    /// terrain whose source the view reads.
    fn ground_for(&mut self, s: &mut StructureCx<'_>, f: &Frame<'_>, drawn: &[(Slot, Rc<Built>)]) {
        let wanted = f
            .style
            .terrain
            .as_ref()
            .and_then(|t| f.sources.get(&t.source).map(|src| (t, src)));
        let Some((terrain, source)) = wanted else {
            if let Some(ground) = self.ground.take() {
                for (_, e) in ground.tiles {
                    stop_elevations(s, e);
                }
                self.drop_models(s);
            }
            return;
        };
        if self.ground.as_ref().is_none_or(|g| g.terrain != *terrain) {
            if let Some(ground) = self.ground.take() {
                for (_, e) in ground.tiles {
                    stop_elevations(s, e);
                }
            }
            self.drop_models(s);
            self.ground = Some(Ground {
                terrain: terrain.clone(),
                source: source.clone(),
                tiles: HashMap::new(),
                revision: 0,
            });
        }
        let generation = self.generation;
        let ground = self.ground.as_mut().expect("set");
        let mut under: Vec<TileId> = drawn
            .iter()
            .filter_map(|(slot, _)| ground.under(slot.tile))
            .collect();
        let center = f.view.camera.center.to_world().tile(f.tile_zoom);
        under.extend(ground.under(center));
        under.sort_unstable_by_key(|t| (t.z, t.x, t.y));
        under.dedup();
        for &tile in &under {
            if ground.tiles.contains_key(&tile) {
                continue;
            }
            let fetch = ground.source.source.request(tile);
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = Arc::clone(&stop);
            let encoding = ground.terrain.encoding;
            let me = f.me.clone();
            let task = Box::pin(async move {
                let dem = match fetch.await {
                    Ok(Some(bytes)) if !stopped.load(Ordering::Relaxed) => {
                        let (answer, flag) = dem_off_loop(bytes, tile, encoding);
                        let _keep = super::core::StopWith(stopped, flag);
                        answer.await.ok()
                    }
                    _ => None,
                };
                Some(Box::new(move |cx: &mut UpdateCx<'_>| {
                    if let Some(core) = me.upgrade()
                        && let Ok(mut core) = core.try_borrow_mut()
                    {
                        core.tilted.arrive_elevations(generation, tile, dem);
                        core.wake_with(cx);
                    }
                }) as Continuation)
            });
            let task = s.store.spawn_task(self.root, task);
            ground
                .tiles
                .insert(tile, Elevations::Loading { task, stop });
        }
        if ground.tiles.len() > under.len() + KEEP_ELEVATIONS {
            let extra: Vec<TileId> = ground
                .tiles
                .keys()
                .filter(|t| !under.contains(t))
                .copied()
                .collect();
            for tile in extra {
                if let Some(e) = ground.tiles.remove(&tile) {
                    stop_elevations(s, e);
                }
            }
        }
    }

    /// Lets go of every tile's model, to raise them again.
    fn drop_models(&mut self, s: &mut StructureCx<'_>) {
        let Some((_, slot)) = self.scene else {
            return;
        };
        for (_, m) in self.models.drain() {
            release(s, slot, m.stage);
        }
        self.release_pictures(s, slot);
        release_shapes(s, slot, &mut self.shapes);
        release_shapes(s, slot, &mut self.retiring);
        self.shapes_key = None;
    }

    fn release_pictures(&mut self, s: &mut StructureCx<'_>, slot: SceneSlot) {
        for (_, pictures) in self.pictures.drain() {
            for (_, p) in pictures {
                release(s, slot, p.stage);
            }
        }
    }

    /// Brings the image layers' tiles laid over the tiles `drawn` to those
    /// the layers draw: each laid once per image, let go of as it leaves.
    fn pictures_for(
        &mut self,
        s: &mut StructureCx<'_>,
        slot: SceneSlot,
        f: &Frame<'_>,
        drawn: &[(Slot, Rc<Built>)],
    ) {
        for pictures in self.pictures.values_mut() {
            for p in pictures.values_mut() {
                p.used = false;
            }
        }
        for (layer, (tint, images)) in f.rasters.layers().enumerate() {
            if tint.is_nan() || tint <= 0.0 {
                continue;
            }
            for (slot_drawn, built) in drawn {
                let tile = slot_drawn.tile;
                let relief = match &self.ground {
                    Some(g) => match g.relief(tile) {
                        Some(r) => r,
                        None => continue,
                    },
                    None => None,
                };
                for (slot_image, image, _) in images {
                    if slot_image.wrap != slot_drawn.wrap || !meets(tile, slot_image.tile) {
                        continue;
                    }
                    let key = (layer, slot_image.tile);
                    let pictures = self.pictures.entry(tile).or_default();
                    if let Some(p) = pictures.get_mut(&key)
                        && Rc::ptr_eq(&p.image, image)
                    {
                        p.used = true;
                        p.tint = tint;
                        continue;
                    }
                    if let Some(old) = pictures.remove(&key) {
                        release(s, slot, old.stage);
                    }
                    let stage =
                        self.lay_image(s, f, tile, built.key.band, key, image, relief.clone());
                    self.pictures.entry(tile).or_default().insert(
                        key,
                        Picture {
                            image: Rc::clone(image),
                            stage,
                            tint,
                            used: true,
                        },
                    );
                }
            }
        }
        for pictures in self.pictures.values_mut() {
            pictures.retain(|_, p| {
                if !p.used {
                    release(s, slot, std::mem::replace(&mut p.stage, Stage::Nothing));
                }
                p.used
            });
        }
        self.pictures.retain(|_, p| !p.is_empty());
    }

    /// Starts laying layer `key.0`'s image of tile `key.1` over `tile`,
    /// drawn for level `band`, over `relief`.
    #[allow(clippy::too_many_arguments)]
    fn lay_image(
        &self,
        s: &mut StructureCx<'_>,
        f: &Frame<'_>,
        tile: TileId,
        band: u8,
        key: (usize, TileId),
        image: &Ready,
        relief: Option<(TileId, Arc<Dem>)>,
    ) -> Stage {
        let pixels = Arc::clone(image.pixels());
        let exaggeration = self.ground.as_ref().map_or(1.0, |g| g.terrain.exaggeration);
        let (answer, stop) = off_loop(move || {
            let relief = relief
                .as_ref()
                .map(|(under, dem)| relief_of(tile, *under, dem, exaggeration));
            let Some(rgba) = pixels.to_straight_rgba8() else {
                return Ok(None);
            };
            Ok(raise_image(&Imagery {
                tile,
                band,
                image_tile: key.1,
                width: pixels.width,
                height: pixels.height,
                rgba: &rgba,
                order: image_order(key.0, key.1.z),
                relief,
            }))
        });
        let generation = self.generation;
        let me = f.me.clone();
        let task = Box::pin(async move {
            let model = answer.await.ok().flatten();
            Some(Box::new(move |cx: &mut UpdateCx<'_>| {
                if let Some(core) = me.upgrade()
                    && let Ok(mut core) = core.try_borrow_mut()
                {
                    core.tilted.arrive_picture(generation, tile, key, model);
                    core.wake_with(cx);
                }
            }) as Continuation)
        });
        let task = s.store.spawn_task(self.root, task);
        Stage::Raising { task, stop }
    }

    /// Starts raising `tile`'s build `built`, over `relief`.
    fn start(
        &mut self,
        s: &mut StructureCx<'_>,
        f: &Frame<'_>,
        tile: TileId,
        built: &Rc<Built>,
        relief: Option<(TileId, Arc<Dem>)>,
    ) -> Stage {
        let meshes_of = |d: &Option<viso_ui::Drawing>| -> Vec<viso_render::Mesh> {
            d.iter()
                .flat_map(|d| d.primitives().iter())
                .filter_map(|p| match p {
                    viso_render::Primitive::Mesh(m) => Some(m.clone()),
                    _ => None,
                })
                .collect()
        };
        let (meshes, over) = (meshes_of(&built.drawing), meshes_of(&built.over));
        let images = f
            .style
            .layers
            .iter()
            .filter(|l| matches!(l.paint, Paint::Raster { .. } | Paint::Hillshade { .. }))
            .count();
        let decoded = Arc::clone(&built.tile);
        let style = Arc::clone(f.style);
        let band = built.key.band;
        let background = style.background.at(f32::from(band)).linear(1.0);
        let exaggeration = self.ground.as_ref().map_or(1.0, |g| g.terrain.exaggeration);
        let (answer, stop) = off_loop(move || {
            let layers: Vec<&crate::mvt::Layer> = decoded.layers.iter().collect();
            let relief = relief
                .as_ref()
                .map(|(under, dem)| relief_of(tile, *under, dem, exaggeration));
            Ok(raise(&Raise {
                tile,
                band,
                meshes: &meshes,
                over: &over,
                images,
                layers: &layers,
                style: &style,
                background,
                relief,
            }))
        });
        let generation = self.generation;
        let me = f.me.clone();
        let task = Box::pin(async move {
            let model = answer.await.ok().flatten();
            Some(Box::new(move |cx: &mut UpdateCx<'_>| {
                if let Some(core) = me.upgrade()
                    && let Ok(mut core) = core.try_borrow_mut()
                {
                    core.tilted.arrive(generation, tile, model);
                    core.wake_with(cx);
                }
            }) as Continuation)
        });
        let task = s.store.spawn_task(self.root, task);
        Stage::Raising { task, stop }
    }

    /// Raises the overlays' drawings again when they, the level or the
    /// relief under them changed.
    fn shapes_for(
        &mut self,
        s: &mut StructureCx<'_>,
        slot: SceneSlot,
        f: &Frame<'_>,
        framed: &[(&ShapeMesh, f64)],
        revision: u64,
    ) {
        let relief = self.ground.as_ref().map_or(0, |g| g.revision);
        let key = (revision, f.band, relief);
        if self.shapes_key == Some(key) {
            for shape in &mut self.shapes {
                if let (Some(id), None) = (shape.id, &shape.model) {
                    shape.model = s.store.scene_model(slot, &model_asset(id));
                    self.waiting = true;
                }
            }
            if self.shapes.iter().all(ShapeModel::ready) {
                release_shapes(s, slot, &mut self.retiring);
            }
            return;
        }
        self.shapes_key = Some(key);
        // What shows now keeps showing until its successors are resident.
        if self.shapes.iter().all(ShapeModel::ready) {
            release_shapes(s, slot, &mut self.retiring);
            self.retiring = std::mem::take(&mut self.shapes);
        } else {
            release_shapes(s, slot, &mut self.shapes);
        }
        let ground = self.ground.as_ref();
        for (order, &(mesh, _)) in framed.iter().enumerate() {
            let k = (f64::from(f.band) - mesh.zoom).exp2() as f32;
            let meshes: Vec<viso_render::Mesh> = mesh
                .drawing
                .primitives()
                .iter()
                .filter_map(|p| match p {
                    viso_render::Primitive::Mesh(m) => Some(m.clone()),
                    _ => None,
                })
                .collect();
            let frame_scale = TILE_SIZE * mesh.zoom.exp2();
            let height = ground.map(|g| {
                let exaggeration = g.terrain.exaggeration;
                let origin = mesh.origin;
                let z = f.tile_zoom;
                move |p: [f32; 2]| {
                    let w = WorldPoint::new(
                        origin.x + f64::from(p[0]) / frame_scale,
                        origin.y + f64::from(p[1]) / frame_scale,
                    );
                    let meters = g.meters(w, z).unwrap_or(0.0);
                    (f64::from(meters * exaggeration) * frame_scale / w.meters_per_unit().max(1e-9))
                        as f32
                }
            });
            let lift = if ground.is_some() {
                SHAPE_LIFT_RELIEF
            } else {
                SHAPE_LIFT
            } / k;
            let model = raise_shape(
                &meshes,
                k,
                lift,
                SHAPE_STEP / k,
                (SHAPES_ABOVE + order as f32 * SHAPE_ORDER) / k,
                height.as_ref().map(|h| h as &dyn Fn([f32; 2]) -> f32),
            );
            if let Some(model) = model {
                let id = next_id();
                s.store.provide_asset(
                    id,
                    ResourceRevision::default(),
                    AssetPayload::Model(Arc::new(model)),
                );
                let model = s.store.scene_model(slot, &model_asset(id));
                self.waiting = true;
                self.shapes.push(ShapeModel {
                    id: Some(id),
                    model,
                    origin: mesh.origin,
                    zoom: mesh.zoom,
                    middle: mesh.middle,
                });
            } else {
                self.shapes.push(ShapeModel {
                    id: None,
                    model: None,
                    origin: mesh.origin,
                    zoom: mesh.zoom,
                    middle: mesh.middle,
                });
            }
        }
    }

    /// Writes the scene's frame: its camera, its light and every resident
    /// model where the tiles and shapes lie.
    fn fill(
        &mut self,
        s: &mut StructureCx<'_>,
        slot: SceneSlot,
        f: &Frame<'_>,
        drawn: &[(Slot, Rc<Built>)],
    ) {
        let view = f.view;
        let band_scale = TILE_SIZE * f64::from(f.band).exp2();
        let c = view.camera.center.to_world();
        let focus = [
            c.x * band_scale - f.anchor[0],
            c.y * band_scale - f.anchor[1],
        ];
        let elevation = self.lift(view, f.tile_zoom).map_or(0.0, |g| {
            g.focus() * band_scale / c.meters_per_unit().max(1e-9)
        });
        let scale = (view.camera.zoom - f64::from(f.band)).exp2();
        let d = view.eye_distance() / scale;
        let (pc, ps) = view.tilt().unwrap_or((1.0, 0.0));
        let b = view.camera.bearing.to_radians();
        let down = [-b.sin(), b.cos()];
        let target = [focus[0], elevation, focus[1]];
        let eye = [
            target[0] + down[0] * d * ps,
            target[1] + d * pc,
            target[2] + down[1] * d * ps,
        ];
        let up = [-down[0] * pc, ps, -down[1] * pc];
        let v3 = |p: [f64; 3]| vec3(p[0] as f32, p[1] as f32, p[2] as f32);
        let height = f64::from(self.size[1]);
        let fov_y = 2.0 * (height / 2.0 / view.eye_distance()).atan();
        let bg = f.style.background.at(f32::from(f.band)).linear(1.0);
        let scenes: &mut SceneSlots = s.store.scenes_mut();
        let Some(frame) = scenes.frame_mut(slot) else {
            return;
        };
        frame.clear_instances();
        frame.camera = Eye {
            eye: v3(eye),
            target: v3(target),
            up: v3(up),
            projection: Projection::Perspective {
                fov_y: fov_y as f32,
            },
            near: (d * 0.02) as f32,
            far: f32::INFINITY,
        };
        let air = Haze::of(bg, d, ps);
        frame.lighting = Lighting {
            direction: vec3(-0.45, 0.8, 0.4),
            intensity: 1.1,
            sky: [0.55, 0.57, 0.62],
            ground: [0.3, 0.3, 0.3],
            ambient: 0.9,
            reflections: 0.0,
            shadows: Shadows::NONE,
            occlusion: AmbientOcclusion::NONE,
            fog: air.color,
            fog_density: air.density as f32,
            fog_start: air.start as f32,
            ..Lighting::default()
        };
        frame.sky = None;
        frame.environment = None;
        frame.background = LinearStraight::new(air.color[0], air.color[1], air.color[2], 1.0);
        let white = LinearStraight::new(1.0, 1.0, 1.0, 1.0);
        for (slot_drawn, _) in drawn {
            let Some(m) = self.models.get(&slot_drawn.tile) else {
                continue;
            };
            let Stage::Held(_, Some(model)) = &m.stage else {
                continue;
            };
            let (nw, _) = slot_drawn.square();
            let z = slot_drawn.tile.z;
            let k = (f64::from(f.band) - f64::from(z)).exp2() as f32;
            let lift = (f32::from(z) - f32::from(f.tile_zoom)) * LEVEL_STEP;
            let at = [
                (nw.x * band_scale - f.anchor[0]) as f32,
                lift,
                (nw.y * band_scale - f.anchor[1]) as f32,
            ];
            let at = placed(at, k);
            frame.push_model(model, &model.rig().rest_pose(), &at, white);
            for p in self
                .pictures
                .get(&slot_drawn.tile)
                .into_iter()
                .flat_map(|p| p.values())
            {
                if let Stage::Held(_, Some(model)) = &p.stage {
                    let tint = LinearStraight::new(1.0, 1.0, 1.0, p.tint);
                    frame.push_model(model, &model.rig().rest_pose(), &at, tint);
                }
            }
        }
        let shapes = if self.shapes.iter().all(ShapeModel::ready) {
            &self.shapes
        } else {
            &self.retiring
        };
        for shape in shapes {
            let Some(model) = &shape.model else {
                continue;
            };
            let o = shape.origin;
            let wrap = (c.x - shape.middle.x).round();
            let k = (f64::from(f.band) - shape.zoom).exp2() as f32;
            let at = [
                ((o.x + wrap) * band_scale - f.anchor[0]) as f32,
                0.0,
                (o.y * band_scale - f.anchor[1]) as f32,
            ];
            frame.push_model(model, &model.rig().rest_pose(), &placed(at, k), white);
        }
        s.store.commit_scene(slot);
    }
}

/// How much of the clear air a tilted view keeps before its ground fades,
/// in eye distances short of the reach.
const HAZE_DEPTH: f64 = 1.7;

/// The air of a tilted view: the ground fades into a haze of the style's
/// background toward the sky, from clear near the eye to all haze at the
/// reach, so the ground's far edge never shows; past it, haze.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Haze {
    /// Linear RGB.
    color: [f32; 3],
    /// Distances from the eye, in the scene's units.
    start: f64,
    density: f64,
}

impl Haze {
    /// The haze over `background` (straight linear RGBA) seen by an eye `d`
    /// from the focus, tilted to pitch sine `ps`.
    fn of(background: [f32; 4], d: f64, ps: f64) -> Haze {
        let [r, g, b, _] = background;
        let light = 0.2126 * r + 0.7152 * g + 0.0722 * b > 0.25;
        // A pale blue on a light map, a deep one on a dark map.
        let sky = if light {
            [0.62, 0.71, 0.84]
        } else {
            [0.035, 0.045, 0.075]
        };
        let color = core::array::from_fn(|i| background[i] * 0.55 + sky[i] * 0.45);
        // From the eye to the ground at the reach: across the focus's
        // distance and the reach, down the eye's height.
        let pc = (1.0 - ps * ps).max(0.0).sqrt();
        let reach = d * (REACH + ps).hypot(pc);
        let start = (reach - HAZE_DEPTH * d).max(d);
        // All but a hundredth hazed at the reach.
        let density = 100f64.ln() / (reach - start).max(1e-9);
        Haze {
            color,
            start,
            density,
        }
    }
}

/// Whether tiles `a` and `b` overlap: one holds the other.
fn meets(a: TileId, b: TileId) -> bool {
    let (fine, coarse) = if a.z >= b.z { (a, b) } else { (b, a) };
    let d = fine.z - coarse.z;
    d < 32 && fine.x >> d == coarse.x && fine.y >> d == coarse.y
}

/// The elevations under `tile` from terrain tile `under`'s `dem`, raised by
/// `exaggeration`.
fn relief_of(tile: TileId, under: TileId, dem: &Dem, exaggeration: f32) -> Relief<'_> {
    let span = f64::from(under.z as i32 - tile.z as i32).exp2();
    Relief {
        dem,
        origin: [
            f64::from(tile.x) * span - f64::from(under.x),
            f64::from(tile.y) * span - f64::from(under.y),
        ],
        span,
        scale: units_per_meter(tile) * exaggeration,
    }
}

/// The transform placing a model's origin at `at`, scaled by `k`.
fn placed(at: [f32; 3], k: f32) -> Mat4 {
    Mat4 {
        v: [
            k, 0.0, 0.0, 0.0, 0.0, k, 0.0, 0.0, 0.0, 0.0, k, 0.0, at[0], at[1], at[2], 1.0,
        ],
    }
}

/// Lets go of a tile's model at whatever stage it is.
fn release(s: &mut StructureCx<'_>, slot: SceneSlot, stage: Stage) {
    match stage {
        Stage::Raising { task, stop } => {
            stop.store(true, Ordering::Relaxed);
            if let Some(task) = task {
                s.store.cancel_task(task);
            }
        }
        Stage::Held(id, _) => s.store.release_scene_model(slot, id),
        Stage::Raised(_) | Stage::Nothing => {}
    }
}

fn stop_elevations(s: &mut StructureCx<'_>, e: Elevations) {
    if let Elevations::Loading { task, stop } = e {
        stop.store(true, Ordering::Relaxed);
        if let Some(task) = task {
            s.store.cancel_task(task);
        }
    }
}
