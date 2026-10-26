//! A view's image layers: for each raster or hillshade layer of the style,
//! a pyramid of image tiles from the raster source it names, decoded (and
//! shaded) on the worker pool, each shown by a node under the camera node
//! between the vector tiles drawn beneath and over the images.
//!
//! A tile's image is handed to the store's asset table once; its node is a
//! leaf showing that asset, placed like a vector tile, so a camera move
//! moves only the camera node.

use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use std::cell::RefCell;

use viso_render::Rgba;
use viso_render::asset::{RasterImage, ResourceId, ResourceRevision};
use viso_ui::context::UpdateCx;
use viso_ui::{
    AssetFit, AssetPayload, BoxStyle, BuildCx, Continuation, DirtyClass, LeafStyle, NodeId, Sizing,
    StackStyle, StructureCx, TaskId,
};

use super::core::{Core, place_tile};
use crate::camera::{TILE_SIZE, View};
use crate::pyramid::{Budget, BuildKey, Plan, Pyramid, Slot, covering};
use crate::raster::{
    Dem, ImageKind, RasterSource, Shading, dem_off_loop, image_off_loop, neighbours, shade_off_loop,
};
use crate::source::TileId;
use crate::style::{Paint, Prop, Style};

/// The bytes of decoded images a layer keeps.
const KEEP: usize = 64 << 20;

/// A tile's image, handed to the asset table as `id`.
pub(crate) struct Image {
    id: ResourceId,
    pixels: Arc<RasterImage>,
}

pub(crate) type Ready = Rc<Image>;

impl Image {
    /// Its pixels.
    pub fn pixels(&self) -> &Arc<RasterImage> {
        &self.pixels
    }
}

/// An image layer's tiles drawn, in paint order: slot, image and node.
pub(crate) type Drawn = [(Slot, Ready, NodeId)];

fn next_id() -> ResourceId {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    // A range of its own, apart from the ids paths hash to.
    ResourceId(0x6d61_7000_0000_0000 | NEXT.fetch_add(1, Ordering::Relaxed))
}

struct Request {
    task: Option<TaskId>,
    stop: Arc<AtomicBool>,
}

/// One image layer.
struct Set {
    /// The zooms its style layer shows at.
    zooms: (f32, f32),
    opacity: Prop<f32>,
    kind: ImageKind,
    source: RasterSource,
    node: NodeId,
    pyramid: Pyramid<Ready>,
    requests: HashMap<TileId, Request>,
    plan: Plan,
    visible: Vec<Slot>,
    prefetch: Vec<Slot>,
    scratch: Vec<(f64, Slot)>,
    /// The tiles drawn, in paint order: slot, image and node.
    tiles: Vec<(Slot, Ready, NodeId)>,
    /// A hillshade's elevations by tile, kept while its tile is, and which
    /// of its neighbours its shading read (bit `row * 3 + column`), `None`
    /// before it is shaded.
    dems: HashMap<TileId, (Arc<Dem>, Option<u16>)>,
    /// Elevations arrived and waiting to be shaded.
    to_shade: Vec<TileId>,
    dirty: bool,
    /// The opacity its nodes are tinted with.
    tint: f32,
}

/// The view's image layers.
pub(crate) struct Rasters {
    parent: NodeId,
    sources: HashMap<String, RasterSource>,
    sets: Vec<Set>,
    /// Bumped when the style or the sources change, to set the layers up
    /// again; a tile arriving for an older generation is dropped.
    generation: u64,
    built: u64,
    free_scratch: Vec<NodeId>,
}

impl Rasters {
    pub fn new(parent: NodeId) -> Self {
        Self {
            parent,
            sources: HashMap::new(),
            sets: Vec::new(),
            generation: 1,
            built: 0,
            free_scratch: Vec::new(),
        }
    }

    /// Reads `source` for the style's layers that name `name`.
    pub fn set_source(&mut self, name: &str, source: Option<RasterSource>) {
        match source {
            Some(s) => self.sources.insert(name.to_owned(), s),
            None => self.sources.remove(name),
        };
        self.generation += 1;
    }

    /// The style changes from `old` to `new`: its image layers are set up
    /// again if they differ.
    pub fn restyle(&mut self, old: &Style, new: &Style) {
        let images = |s: &Style| {
            s.layers
                .iter()
                .filter(|l| matches!(l.paint, Paint::Raster { .. } | Paint::Hillshade { .. }))
                .map(|l| (l.source.clone(), l.paint.clone(), l.min_zoom, l.max_zoom))
                .collect::<Vec<_>>()
        };
        if images(old) != images(new) {
            self.generation += 1;
        }
    }

    /// The raster sources by name.
    pub fn sources(&self) -> &HashMap<String, RasterSource> {
        &self.sources
    }

    /// The image tiles asked for and not yet arrived.
    pub fn loading(&self) -> usize {
        self.sets.iter().map(|s| s.requests.len()).sum()
    }

    /// Each image layer's tiles drawn, in paint order, and the opacity it
    /// draws them at.
    pub fn layers(&self) -> impl Iterator<Item = (f32, &Drawn)> + '_ {
        self.sets.iter().map(|s| (s.tint, &*s.tiles))
    }

    /// The image tiles drawn.
    pub fn drawn(&self) -> usize {
        self.sets.iter().map(|s| s.tiles.len()).sum()
    }

    fn setup(&mut self, s: &mut StructureCx<'_>, style: &Style) {
        for mut set in self.sets.drain(..) {
            for (tile, r) in set.requests.drain() {
                r.stop.store(true, Ordering::Relaxed);
                if let Some(task) = r.task {
                    s.store.cancel_task(task);
                }
                set.source.source.cancel(tile);
            }
            s.store
                .free_tree(set.node, s.effects, &mut self.free_scratch);
        }
        for layer in &style.layers {
            let kind = match &layer.paint {
                Paint::Raster { .. } => ImageKind::Image,
                Paint::Hillshade {
                    encoding,
                    exaggeration,
                    shadow,
                    highlight,
                    illumination,
                } => ImageKind::Shade(Shading {
                    encoding: *encoding,
                    exaggeration: *exaggeration,
                    shadow: *shadow,
                    highlight: *highlight,
                    illumination: *illumination,
                }),
                _ => continue,
            };
            let opacity = match &layer.paint {
                Paint::Raster { opacity } => opacity.clone(),
                _ => Prop::Const(1.0),
            };
            let Some(source) = self.sources.get(&layer.source) else {
                continue;
            };
            let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, self.parent);
            let node = cx
                .stack(
                    StackStyle {
                        size: Sizing::fixed(0.0, 0.0),
                        align: viso_ui::Alignment2D::TopStart,
                        ..StackStyle::default()
                    },
                    |_| {},
                )
                .id();
            let mut pyramid = Pyramid::new(source.min_zoom, source.max_zoom);
            pyramid.budget = Budget {
                bytes: KEEP,
                ..Budget::default()
            };
            self.sets.push(Set {
                zooms: (layer.min_zoom, layer.max_zoom),
                opacity,
                kind,
                source: source.clone(),
                node,
                pyramid,
                requests: HashMap::new(),
                plan: Plan::default(),
                visible: Vec::new(),
                prefetch: Vec::new(),
                scratch: Vec::new(),
                tiles: Vec::new(),
                dems: HashMap::new(),
                to_shade: Vec::new(),
                dirty: true,
                tint: f32::NAN,
            });
        }
        s.store.mark_dirty(
            self.parent,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
        );
        self.built = self.generation;
    }

    /// A tile of layer `set` arrived, for `generation`.
    pub fn arrive(
        &mut self,
        generation: u64,
        set: usize,
        tile: TileId,
        result: Result<Option<Arc<RasterImage>>, ()>,
    ) {
        if generation != self.built {
            return;
        }
        let Some(set) = self.sets.get_mut(set) else {
            return;
        };
        if set.requests.remove(&tile).is_none() {
            return;
        }
        let result = result.map(|image| {
            image.map(|pixels| {
                let bytes = pixels.width as usize * pixels.height as usize * 4;
                let image = Rc::new(Image {
                    id: next_id(),
                    pixels,
                });
                (image, bytes, BuildKey { style: 0, band: 0 })
            })
        });
        set.pyramid.arrive(tile, result);
        set.dirty = true;
    }

    /// A hillshade's elevations of `tile` arrived for `generation`: shaded
    /// with its neighbours once the layers are next brought up to date.
    pub fn arrive_dem(
        &mut self,
        generation: u64,
        set: usize,
        tile: TileId,
        result: Result<Option<Arc<Dem>>, ()>,
    ) {
        if generation != self.built {
            return;
        }
        match result {
            Ok(Some(dem)) => {
                if let Some(s) = self.sets.get_mut(set)
                    && s.requests.contains_key(&tile)
                {
                    s.dems.insert(tile, (dem, None));
                    s.to_shade.push(tile);
                    s.dirty = true;
                }
            }
            Ok(None) => self.arrive(generation, set, tile, Ok(None)),
            Err(()) => self.arrive(generation, set, tile, Err(())),
        }
    }

    /// Brings the layers' nodes to what `view` shows, placed for level
    /// `band` about `anchor`; `rebase` places every node anew.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &mut self,
        s: &mut StructureCx<'_>,
        style: &Style,
        view: &View,
        band: u8,
        anchor: [f64; 2],
        rebase: bool,
        me: &Weak<RefCell<Core>>,
    ) {
        if self.built != self.generation {
            self.setup(s, style);
        }
        let zoom = view.camera.zoom as f32;
        let generation = self.built;
        for (index, set) in self.sets.iter_mut().enumerate() {
            set.shade_arrived(s, index, generation, me);
            let z = set
                .pyramid
                .tile_zoom(band.saturating_add(set.source.depth()));
            let shows = zoom >= set.zooms.0 && zoom < set.zooms.1;
            if shows {
                covering(view, z, 0.0, &mut set.scratch, &mut set.prefetch);
            } else {
                set.prefetch.clear();
            }
            if set.prefetch != set.visible {
                std::mem::swap(&mut set.visible, &mut set.prefetch);
                set.dirty = true;
            }
            let tint = set.opacity.at(f32::from(band)).clamp(0.0, 1.0);
            let retint = tint != set.tint;
            set.tint = tint;
            if set.dirty {
                set.dirty = false;
                if shows {
                    covering(view, z, 256.0, &mut set.scratch, &mut set.prefetch);
                } else {
                    set.prefetch.clear();
                }
                let mut plan = std::mem::take(&mut set.plan);
                let key = BuildKey { style: 0, band: 0 };
                set.pyramid
                    .plan(&set.visible, &set.prefetch, key, &mut plan);
                for tile in &plan.cancel {
                    if let Some(r) = set.requests.remove(tile) {
                        r.stop.store(true, Ordering::Relaxed);
                        if let Some(task) = r.task {
                            s.store.cancel_task(task);
                        }
                        set.source.source.cancel(*tile);
                    }
                }
                for &tile in &plan.fetch {
                    set.start(s, tile, index, generation, me);
                }
                set.place(s, &plan.draw, &mut self.free_scratch);
                let pyramid = &set.pyramid;
                set.dems.retain(|t, _| pyramid.state(*t).is_some());
                set.plan = plan;
                set.put(s, band, anchor);
            } else if rebase {
                set.put(s, band, anchor);
            }
            if retint {
                for (_, image, node) in &set.tiles {
                    s.store.set_asset(
                        *node,
                        image.id,
                        AssetFit::Fill,
                        Some(Rgba::new(1.0, 1.0, 1.0, tint)),
                    );
                }
            }
        }
    }
}

impl Set {
    /// Shades the elevations arrived, and shades again the tiles beside
    /// them that were shaded without them.
    fn shade_arrived(
        &mut self,
        s: &mut StructureCx<'_>,
        index: usize,
        generation: u64,
        me: &Weak<RefCell<Core>>,
    ) {
        if self.to_shade.is_empty() {
            return;
        }
        let arrived = std::mem::take(&mut self.to_shade);
        let mut again = Vec::new();
        for &tile in &arrived {
            for (i, j, n) in neighbours(tile) {
                // `tile` as `n`'s neighbour.
                let bit = 1u16 << ((2 - j) * 3 + (2 - i));
                if let Some((_, Some(read))) = self.dems.get(&n)
                    && read & bit == 0
                    && !arrived.contains(&n)
                {
                    again.push(n);
                }
            }
        }
        again.sort_unstable_by_key(|t| (t.x, t.y));
        again.dedup();
        for tile in arrived.into_iter().chain(again) {
            self.shade(s, tile, index, generation, me);
        }
    }

    /// Shades `tile`'s elevations with those of its neighbours known.
    fn shade(
        &mut self,
        s: &mut StructureCx<'_>,
        tile: TileId,
        index: usize,
        generation: u64,
        me: &Weak<RefCell<Core>>,
    ) {
        let ImageKind::Shade(shading) = self.kind else {
            return;
        };
        let mut around: [[Option<Arc<Dem>>; 3]; 3] = Default::default();
        let mut read = 0u16;
        for (i, j, n) in neighbours(tile) {
            if let Some((d, _)) = self.dems.get(&n) {
                around[j][i] = Some(Arc::clone(d));
                read |= 1 << (j * 3 + i);
            }
        }
        let Some((dem, shaded)) = self.dems.get_mut(&tile) else {
            return;
        };
        *shaded = Some(read);
        let (answer, stop) = shade_off_loop(tile, Arc::clone(dem), around, shading);
        let me = me.clone();
        let task = Box::pin(async move {
            let result = answer.await.map(Some).map_err(|_| ());
            Some(Box::new(move |cx: &mut UpdateCx<'_>| {
                if let Some(core) = me.upgrade()
                    && let Ok(mut core) = core.try_borrow_mut()
                {
                    core.rasters.arrive(generation, index, tile, result);
                    core.wake_with(cx);
                }
            }) as Continuation)
        });
        let task = s.store.spawn_task(self.node, task);
        if let Some(old) = self.requests.insert(tile, Request { task, stop }) {
            old.stop.store(true, Ordering::Relaxed);
            if let Some(task) = old.task {
                s.store.cancel_task(task);
            }
        }
    }

    fn start(
        &mut self,
        s: &mut StructureCx<'_>,
        tile: TileId,
        index: usize,
        generation: u64,
        me: &Weak<RefCell<Core>>,
    ) {
        let fetch = self.source.source.request(tile);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let kind = self.kind;
        let me = me.clone();
        if let ImageKind::Shade(shading) = kind {
            // Elevations first: shaded on the loop's next pass with what
            // is known of their neighbours.
            let task = Box::pin(async move {
                let result = match fetch.await {
                    Ok(Some(bytes)) if !stopped.load(Ordering::Relaxed) => {
                        let (answer, flag) = dem_off_loop(bytes, tile, shading.encoding);
                        let _keep = super::core::StopWith(stopped, flag);
                        answer.await.map(Some).map_err(|_| ())
                    }
                    Ok(None) => Ok(None),
                    _ => Err(()),
                };
                Some(Box::new(move |cx: &mut UpdateCx<'_>| {
                    if let Some(core) = me.upgrade()
                        && let Ok(mut core) = core.try_borrow_mut()
                    {
                        core.rasters.arrive_dem(generation, index, tile, result);
                        core.wake_with(cx);
                    }
                }) as Continuation)
            });
            let task = s.store.spawn_task(self.node, task);
            self.requests.insert(tile, Request { task, stop });
            return;
        }
        let task = Box::pin(async move {
            let result = match fetch.await {
                Ok(Some(bytes)) if !stopped.load(Ordering::Relaxed) => {
                    let (answer, flag) = image_off_loop(bytes, tile, kind);
                    let _keep = super::core::StopWith(stopped, flag);
                    answer.await.map(Some).map_err(|_| ())
                }
                Ok(None) => Ok(None),
                _ => Err(()),
            };
            Some(Box::new(move |cx: &mut UpdateCx<'_>| {
                if let Some(core) = me.upgrade()
                    && let Ok(mut core) = core.try_borrow_mut()
                {
                    core.rasters.arrive(generation, index, tile, result);
                    core.wake_with(cx);
                }
            }) as Continuation)
        });
        let task = s.store.spawn_task(self.node, task);
        self.requests.insert(tile, Request { task, stop });
    }

    /// Brings its nodes to `draw`, in order.
    fn place(&mut self, s: &mut StructureCx<'_>, draw: &[Slot], free: &mut Vec<NodeId>) {
        let same = draw.len() == self.tiles.len()
            && draw.iter().zip(&self.tiles).all(|(slot, (t, image, _))| {
                slot == t
                    && self
                        .pyramid
                        .ready(slot.tile)
                        .is_some_and(|r| Rc::ptr_eq(r, image))
            });
        if same {
            return;
        }
        let mut old = std::mem::take(&mut self.tiles);
        let tint = Some(Rgba::new(1.0, 1.0, 1.0, self.tint));
        for &slot in draw {
            let Some(image) = self.pyramid.ready(slot.tile).cloned() else {
                continue;
            };
            if let Some(i) = old
                .iter()
                .position(|(t, im, _)| *t == slot && Rc::ptr_eq(im, &image))
            {
                self.tiles.push(old.swap_remove(i));
                continue;
            }
            if !s.store.assets().knows(image.id) {
                s.store.provide_asset(
                    image.id,
                    ResourceRevision::default(),
                    AssetPayload::Raster(Arc::clone(&image.pixels)),
                );
            }
            let side = TILE_SIZE as f32;
            let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, self.node);
            let node = cx
                .leaf(LeafStyle {
                    size: Sizing::fixed(side, side),
                    style: BoxStyle::NONE,
                })
                .id();
            s.store.set_asset(node, image.id, AssetFit::Fill, tint);
            self.tiles.push((slot, image, node));
        }
        for (_, _, node) in old {
            s.store.free_tree(node, s.effects, free);
        }
        // Paint order: ancestors beneath.
        let in_order = {
            let mut children = s.store.arena().children(self.node);
            self.tiles
                .iter()
                .all(|(_, _, n)| children.next() == Some(*n))
        };
        if !in_order {
            for (_, _, n) in &self.tiles {
                s.store.arena_detach(*n);
            }
            for (_, _, n) in &self.tiles {
                s.store.arena_insert_before(self.node, *n, None);
            }
        }
        s.store.mark_dirty(
            self.node,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
        );
    }

    /// Places its nodes for level `band` about `anchor`.
    fn put(&self, s: &mut StructureCx<'_>, band: u8, anchor: [f64; 2]) {
        for (slot, _, node) in &self.tiles {
            place_tile(s, *node, *slot, band, anchor);
        }
    }
}
