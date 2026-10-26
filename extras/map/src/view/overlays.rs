//! What an app puts over a view's tiles: shapes, markers and their
//! clusters, and the position puck.
//!
//! Shapes and the puck's accuracy circle are drawings in world frames under
//! the camera node, placed like tiles; markers are the app's own components
//! in the overlay above, each in a sizeless wrapper the view moves to its
//! place, aligned on it by the marker's anchor. Clusters are worked out once
//! per zoom level.

use std::collections::HashMap;
use std::rc::{Rc, Weak};

use std::cell::RefCell;

use viso_ui::{
    Alignment2D, BuildCx, DirtyClass, DispatchPhase, NodeId, PointerPhase, Sizing, StackStyle,
    StructureCx, Turn, Vec2,
};

use super::core::Core;
use super::tilted::Lift;
use crate::camera::{TILE_SIZE, View};
use crate::geo::{LngLat, WorldPoint};
use crate::overlay::{
    PUCK_SIDE, Puck, Shape, ShapeMesh, accuracy_circle, build_shape, puck_drawing,
};
use crate::tess::MeshBuilder;

/// What builds a marker's content.
pub type MarkerContent = Rc<dyn Fn(&mut BuildCx<'_>)>;
/// What builds a cluster's content for its count of markers.
pub type ClusterContent = Rc<dyn Fn(&mut BuildCx<'_>, usize)>;

/// A component pinned to a place: built by the app, moved by the view.
#[derive(Clone)]
pub struct Marker {
    pub id: u64,
    pub at: LngLat,
    /// The point of the marker that sits on the place.
    pub anchor: Alignment2D,
    /// Whether it joins clusters.
    pub clusters: bool,
    content: MarkerContent,
}

impl std::fmt::Debug for Marker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Marker")
            .field("id", &self.id)
            .field("at", &self.at)
            .finish_non_exhaustive()
    }
}

impl Marker {
    /// Marker `id` at `at`, built by `content`, its bottom middle on the
    /// place.
    pub fn new(id: u64, at: LngLat, content: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        Self {
            id,
            at,
            anchor: Alignment2D::Bottom,
            clusters: true,
            content: Rc::new(content),
        }
    }

    /// Puts its `anchor` point on the place.
    pub fn anchor(mut self, anchor: Alignment2D) -> Self {
        self.anchor = anchor;
        self
    }

    /// Keeps it out of clusters.
    pub fn unclustered(mut self) -> Self {
        self.clusters = false;
        self
    }
}

/// How markers near each other gather into one.
#[derive(Clone)]
pub struct Cluster {
    /// Markers closer than this many points gather.
    pub radius: f64,
    /// From this zoom on, none gather.
    pub max_zoom: f64,
    content: ClusterContent,
}

impl Cluster {
    /// Clusters of markers within `radius` points, built by `content` with
    /// their count, up to zoom 16.
    pub fn new(radius: f64, content: impl Fn(&mut BuildCx<'_>, usize) + 'static) -> Self {
        Self {
            radius,
            max_zoom: 16.0,
            content: Rc::new(content),
        }
    }

    /// None gather from `zoom` on.
    pub fn max_zoom(mut self, zoom: f64) -> Self {
        self.max_zoom = zoom;
        self
    }
}

/// An id the view gives a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShapeId(pub u64);

/// What a press landed on above the tiles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Pressed {
    Marker(u64),
    Cluster(usize),
}

/// A world-framed drawing node and where it was placed.
struct Framed {
    mesh: ShapeMesh,
    node: NodeId,
    wrap: f64,
}

struct ShapeEntry {
    id: ShapeId,
    shape: Shape,
    built: Option<Framed>,
    dirty: bool,
}

struct MarkerEntry {
    marker: Marker,
    node: Option<NodeId>,
    at: WorldPoint,
    shown: Option<[f32; 2]>,
    hidden: bool,
    rebuild: bool,
}

struct ClusterEntry {
    members: Vec<usize>,
    at: WorldPoint,
    node: NodeId,
    shown: Option<[f32; 2]>,
}

/// A view's overlays.
pub(crate) struct Overlays {
    /// The labels' parent, first in the overlay: the puck goes after it.
    labels_node: NodeId,
    shapes_node: NodeId,
    accuracy_node: NodeId,
    overlay: NodeId,
    shapes: Vec<ShapeEntry>,
    next_shape: u64,
    shapes_dirty: bool,
    markers: Vec<MarkerEntry>,
    markers_dirty: bool,
    cluster: Option<Cluster>,
    clusters: Vec<ClusterEntry>,
    clustered: Option<u8>,
    puck: Option<Puck>,
    puck_node: Option<(NodeId, bool)>,
    accuracy: Option<Framed>,
    puck_dirty: bool,
    pub pressed: Option<Pressed>,
    builder: MeshBuilder,
    free: Vec<NodeId>,
    /// Bumped as the shapes or the accuracy circle are built anew.
    pub revision: u64,
}

/// The whole worlds to shift `w` east by to lie nearest `cam`.
fn wrap_near(cam: WorldPoint, w: WorldPoint) -> f64 {
    (cam.x - w.x).round()
}

/// Places a world-framed node for level `band` around `anchor`.
fn place_framed(s: &mut StructureCx<'_>, f: &Framed, band: u8, anchor: [f64; 2]) {
    let band_scale = TILE_SIZE * f64::from(band).exp2();
    let o = f.mesh.origin;
    let tau = [
        (o.x + f.wrap) * band_scale - anchor[0],
        o.y * band_scale - anchor[1],
    ];
    let k = (f64::from(band) - f.mesh.zoom).exp2();
    s.store
        .set_translate(f.node, Vec2::new(tau[0] as f32, tau[1] as f32));
    s.store.set_turn(
        f.node,
        Turn {
            scale: k as f32,
            rotation: 0.0,
            origin: Alignment2D::TopStart,
        },
    );
}

/// Lays out what was just built under `parent`.
fn built_under(s: &mut StructureCx<'_>, parent: NodeId) {
    s.store.mark_dirty(
        parent,
        DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
    );
}

fn framed_node(s: &mut StructureCx<'_>, parent: NodeId, mesh: &ShapeMesh) -> NodeId {
    let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, parent);
    let node = cx
        .drawing(
            Sizing::fixed(mesh.size[0], mesh.size[1]),
            mesh.drawing.clone(),
        )
        .id();
    built_under(s, parent);
    node
}

/// A sizeless node at the overlay's corner holding what `content` builds,
/// aligned on its origin by `anchor`, that notes presses as `pressed`.
fn pinned(
    s: &mut StructureCx<'_>,
    parent: NodeId,
    anchor: Alignment2D,
    me: &Weak<RefCell<Core>>,
    pressed: Pressed,
    content: impl FnOnce(&mut BuildCx<'_>),
) -> NodeId {
    let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, parent);
    let h = cx.stack(
        StackStyle {
            size: Sizing::fixed(0.0, 0.0),
            align: anchor,
            ..StackStyle::default()
        },
        content,
    );
    let me = me.clone();
    cx.on_pointer(h, move |ev| {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        if ev.pointer().is_some_and(|p| p.phase == PointerPhase::Down)
            && let Some(core) = me.upgrade()
            && let Ok(mut core) = core.try_borrow_mut()
        {
            core.overlays.pressed = Some(pressed);
        }
    });
    built_under(s, parent);
    h.id()
}

/// Moves pinned node `node` to view point `p` when it is not there.
fn pin(s: &mut StructureCx<'_>, node: NodeId, p: [f64; 2], shown: &mut Option<[f32; 2]>) {
    let p = [p[0] as f32, p[1] as f32];
    if *shown != Some(p) {
        *shown = Some(p);
        s.store.set_translate(node, Vec2::new(p[0], p[1]));
    }
}

impl Overlays {
    pub fn new(
        shapes_node: NodeId,
        accuracy_node: NodeId,
        overlay: NodeId,
        labels_node: NodeId,
    ) -> Self {
        Self {
            labels_node,
            shapes_node,
            accuracy_node,
            overlay,
            shapes: Vec::new(),
            next_shape: 1,
            shapes_dirty: false,
            markers: Vec::new(),
            markers_dirty: false,
            cluster: None,
            clusters: Vec::new(),
            clustered: None,
            puck: None,
            puck_node: None,
            accuracy: None,
            puck_dirty: false,
            pressed: None,
            builder: MeshBuilder::new(),
            free: Vec::new(),
            revision: 0,
        }
    }

    /// The world-framed drawings, in paint order: the shapes, then the
    /// accuracy circle; each with the whole worlds east it is drawn at.
    pub fn framed(&self) -> impl Iterator<Item = (&ShapeMesh, f64)> {
        self.shapes
            .iter()
            .filter_map(|e| e.built.as_ref())
            .chain(self.accuracy.as_ref())
            .map(|f| (&f.mesh, f.wrap))
    }

    /// Adds `shape` over the others.
    pub fn add_shape(&mut self, shape: Shape) -> ShapeId {
        let id = ShapeId(self.next_shape);
        self.next_shape += 1;
        self.shapes.push(ShapeEntry {
            id,
            shape,
            built: None,
            dirty: true,
        });
        self.shapes_dirty = true;
        id
    }

    /// Replaces shape `id`; whether it was there.
    pub fn set_shape(&mut self, id: ShapeId, shape: Shape) -> bool {
        let Some(e) = self.shapes.iter_mut().find(|e| e.id == id) else {
            return false;
        };
        if e.shape != shape {
            e.shape = shape;
            e.dirty = true;
            self.shapes_dirty = true;
        }
        true
    }

    /// Takes shape `id` away; whether it was there.
    pub fn remove_shape(&mut self, id: ShapeId) -> bool {
        let Some(e) = self.shapes.iter_mut().find(|e| e.id == id) else {
            return false;
        };
        // Freed at the next apply.
        e.dirty = true;
        e.shape = Shape::Line(crate::overlay::Polyline::new(
            Vec::new(),
            0.0,
            crate::style::Color::TRANSPARENT,
        ));
        e.id = ShapeId(0);
        self.shapes_dirty = true;
        true
    }

    /// Shape `id`, if there.
    pub fn shape(&self, id: ShapeId) -> Option<&Shape> {
        self.shapes.iter().find(|e| e.id == id).map(|e| &e.shape)
    }

    /// Adds `marker`, or replaces the one of its id.
    pub fn set_marker(&mut self, marker: Marker) {
        let at = marker.at.to_world();
        match self.markers.iter_mut().find(|e| e.marker.id == marker.id) {
            Some(e) => {
                e.rebuild |= !Rc::ptr_eq(&e.marker.content, &marker.content)
                    || e.marker.anchor != marker.anchor;
                e.marker = marker;
                e.at = at;
                e.shown = None;
            }
            None => self.markers.push(MarkerEntry {
                marker,
                node: None,
                at,
                shown: None,
                hidden: false,
                rebuild: true,
            }),
        }
        self.markers_dirty = true;
        self.clustered = None;
    }

    /// Moves marker `id` to `at`; whether it was there.
    pub fn move_marker(&mut self, id: u64, at: LngLat) -> bool {
        let Some(e) = self.markers.iter_mut().find(|e| e.marker.id == id) else {
            return false;
        };
        e.marker.at = at;
        e.at = at.to_world();
        e.shown = None;
        self.markers_dirty = true;
        if e.marker.clusters && self.cluster.is_some() {
            self.clustered = None;
        }
        true
    }

    /// Takes marker `id` away; whether it was there.
    pub fn remove_marker(&mut self, id: u64) -> bool {
        let Some(i) = self.markers.iter().position(|e| e.marker.id == id) else {
            return false;
        };
        let e = self.markers.remove(i);
        if let Some(node) = e.node {
            self.free.push(node);
        }
        self.markers_dirty = true;
        self.clustered = None;
        true
    }

    /// The markers, by id, where they are.
    pub fn markers(&self) -> impl Iterator<Item = (u64, LngLat)> + '_ {
        self.markers.iter().map(|e| (e.marker.id, e.marker.at))
    }

    /// The markers shown on their own (not gathered), and the clusters'
    /// sizes and places.
    pub fn shown(&self) -> (Vec<u64>, Vec<(usize, LngLat)>) {
        (
            self.markers
                .iter()
                .filter(|e| !e.hidden)
                .map(|e| e.marker.id)
                .collect(),
            self.clusters
                .iter()
                .map(|c| (c.members.len(), c.at.to_lng_lat()))
                .collect(),
        )
    }

    /// The members of cluster `index`.
    pub fn cluster_members(&self, index: usize) -> Vec<LngLat> {
        self.clusters.get(index).map_or_else(Vec::new, |c| {
            c.members
                .iter()
                .map(|&i| self.markers[i].marker.at)
                .collect()
        })
    }

    /// Gathers markers by `cluster`, or no longer with `None`.
    pub fn set_cluster(&mut self, cluster: Option<Cluster>) {
        self.cluster = cluster;
        self.clustered = None;
        self.markers_dirty = true;
    }

    /// Shows the user's position, or no longer with `None`.
    pub fn set_puck(&mut self, puck: Option<Puck>) {
        if self.puck != puck {
            self.puck = puck;
            self.puck_dirty = true;
        }
    }

    pub fn puck(&self) -> Option<Puck> {
        self.puck
    }

    /// Writes the overlays into the tree for `view`, its tiles placed for
    /// level `band` about `anchor`; `placed` when they were placed anew.
    /// Tilted over relief, `lift` stands the puck and the markers on the
    /// ground under them.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &mut self,
        s: &mut StructureCx<'_>,
        view: &View,
        band: u8,
        anchor: [f64; 2],
        placed: bool,
        me: &Weak<RefCell<Core>>,
        lift: Option<&Lift<'_>>,
    ) {
        let at = |w: WorldPoint| match lift {
            Some(l) => l.to_view(view, w),
            None => view.to_view(w),
        };
        for node in self.free.drain(..) {
            let mut scratch = Vec::new();
            s.store.free_tree(node, s.effects, &mut scratch);
        }
        let cam = view.camera.center.to_world();
        self.apply_shapes(s, cam, band, anchor, placed);
        self.apply_puck(s, view, &at, cam, band, anchor, placed);
        self.apply_markers(s, &at, cam, band, me);
    }

    fn apply_shapes(
        &mut self,
        s: &mut StructureCx<'_>,
        cam: WorldPoint,
        band: u8,
        anchor: [f64; 2],
        placed: bool,
    ) {
        if std::mem::take(&mut self.shapes_dirty) {
            self.revision += 1;
            let mut order_changed = false;
            let mut scratch = Vec::new();
            self.shapes.retain_mut(|e| {
                if !e.dirty {
                    return true;
                }
                e.dirty = false;
                if let Some(f) = e.built.take() {
                    s.store.free_tree(f.node, s.effects, &mut scratch);
                }
                if e.id.0 == 0 {
                    return false;
                }
                if let Some(mesh) = build_shape(&e.shape, &mut self.builder) {
                    let node = framed_node(s, self.shapes_node, &mesh);
                    let wrap = wrap_near(cam, mesh.middle);
                    let f = Framed { mesh, node, wrap };
                    place_framed(s, &f, band, anchor);
                    e.built = Some(f);
                    order_changed = true;
                }
                true
            });
            if order_changed {
                let in_order = {
                    let mut children = s.store.arena().children(self.shapes_node);
                    self.shapes
                        .iter()
                        .filter_map(|e| e.built.as_ref())
                        .all(|f| children.next() == Some(f.node))
                };
                if !in_order {
                    for f in self.shapes.iter().filter_map(|e| e.built.as_ref()) {
                        s.store.arena_detach(f.node);
                    }
                    for f in self.shapes.iter().filter_map(|e| e.built.as_ref()) {
                        s.store.arena_insert_before(self.shapes_node, f.node, None);
                    }
                }
            }
        }
        for f in self.shapes.iter_mut().filter_map(|e| e.built.as_mut()) {
            let wrap = wrap_near(cam, f.mesh.middle);
            if placed || wrap != f.wrap {
                f.wrap = wrap;
                place_framed(s, f, band, anchor);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_puck(
        &mut self,
        s: &mut StructureCx<'_>,
        view: &View,
        to_view: &dyn Fn(WorldPoint) -> [f64; 2],
        cam: WorldPoint,
        band: u8,
        anchor: [f64; 2],
        placed: bool,
    ) {
        let mut scratch = Vec::new();
        if std::mem::take(&mut self.puck_dirty) {
            self.revision += 1;
            let heading = self.puck.and_then(|p| p.heading).is_some();
            if let Some((node, had)) = self.puck_node
                && (self.puck.is_none() || had != heading)
            {
                s.store.free_tree(node, s.effects, &mut scratch);
                self.puck_node = None;
            }
            if let Some(f) = self.accuracy.take() {
                s.store.free_tree(f.node, s.effects, &mut scratch);
            }
            if let Some(p) = self.puck {
                if self.puck_node.is_none() {
                    let drawing = puck_drawing(heading, &mut self.builder);
                    let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, self.overlay);
                    let node = cx
                        .drawing(Sizing::fixed(PUCK_SIDE, PUCK_SIDE), drawing)
                        .id();
                    // Over the labels, beneath the markers.
                    s.store.arena_detach(node);
                    let next = s.store.arena().next_sibling(self.labels_node);
                    s.store.arena_insert_before(self.overlay, node, next);
                    self.puck_node = Some((node, heading));
                }
                if p.accuracy > 0.0
                    && let Some(mesh) = build_shape(&accuracy_circle(&p).into(), &mut self.builder)
                {
                    let node = framed_node(s, self.accuracy_node, &mesh);
                    let wrap = wrap_near(cam, mesh.middle);
                    let f = Framed { mesh, node, wrap };
                    place_framed(s, &f, band, anchor);
                    self.accuracy = Some(f);
                }
            }
        }
        if let Some(f) = &mut self.accuracy {
            let wrap = wrap_near(cam, f.mesh.middle);
            if placed || wrap != f.wrap {
                f.wrap = wrap;
                place_framed(s, f, band, anchor);
            }
        }
        if let (Some(p), Some((node, _))) = (self.puck, self.puck_node) {
            let mut w = p.at.to_world();
            w.x += wrap_near(cam, w);
            let at = to_view(w);
            let half = f64::from(PUCK_SIDE) / 2.0;
            s.store.set_translate(
                node,
                Vec2::new((at[0] - half) as f32, (at[1] - half) as f32),
            );
            let turn = p.heading.unwrap_or(0.0) - view.camera.bearing;
            s.store.set_turn(
                node,
                Turn {
                    scale: 1.0,
                    rotation: turn.to_radians() as f32,
                    origin: Alignment2D::Center,
                },
            );
        }
    }

    fn apply_markers(
        &mut self,
        s: &mut StructureCx<'_>,
        to_view: &dyn Fn(WorldPoint) -> [f64; 2],
        cam: WorldPoint,
        band: u8,
        me: &Weak<RefCell<Core>>,
    ) {
        if std::mem::take(&mut self.markers_dirty) {
            let mut scratch = Vec::new();
            for e in &mut self.markers {
                if e.rebuild {
                    e.rebuild = false;
                    if let Some(node) = e.node.take() {
                        s.store.free_tree(node, s.effects, &mut scratch);
                    }
                }
                if e.node.is_none() {
                    let content = Rc::clone(&e.marker.content);
                    let node = pinned(
                        s,
                        self.overlay,
                        e.marker.anchor,
                        me,
                        Pressed::Marker(e.marker.id),
                        move |cx| content(cx),
                    );
                    e.node = Some(node);
                    e.shown = None;
                    if e.hidden {
                        s.store.set_hidden(node, true);
                    }
                }
            }
        }
        if self.clustered != Some(band) {
            self.recluster(s, band, me);
        }
        for e in &mut self.markers {
            let (Some(node), false) = (e.node, e.hidden) else {
                continue;
            };
            let mut w = e.at;
            w.x += wrap_near(cam, w);
            pin(s, node, to_view(w), &mut e.shown);
        }
        for c in &mut self.clusters {
            let mut w = c.at;
            w.x += wrap_near(cam, w);
            pin(s, c.node, to_view(w), &mut c.shown);
        }
    }

    /// Gathers the markers for level `band` and rebuilds the clusters.
    fn recluster(&mut self, s: &mut StructureCx<'_>, band: u8, me: &Weak<RefCell<Core>>) {
        self.clustered = Some(band);
        let mut scratch = Vec::new();
        for c in self.clusters.drain(..) {
            s.store.free_tree(c.node, s.effects, &mut scratch);
        }
        let mut gathered = vec![false; self.markers.len()];
        if let Some(cluster) = self
            .cluster
            .clone()
            .filter(|c| f64::from(band) < c.max_zoom && c.radius > 0.0)
        {
            let k = TILE_SIZE * f64::from(band).exp2();
            let r = cluster.radius;
            let cell = |w: WorldPoint| ((w.x * k / r).floor() as i64, (w.y * k / r).floor() as i64);
            let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
            for (i, e) in self.markers.iter().enumerate() {
                if e.marker.clusters {
                    grid.entry(cell(e.at)).or_default().push(i);
                }
            }
            let mut taken = vec![false; self.markers.len()];
            for i in 0..self.markers.len() {
                let e = &self.markers[i];
                if !e.marker.clusters || taken[i] {
                    continue;
                }
                let (cx, cy) = cell(e.at);
                let mut members = vec![i];
                for dx in -1..=1 {
                    for dy in -1..=1 {
                        for &j in grid.get(&(cx + dx, cy + dy)).map_or(&[][..], Vec::as_slice) {
                            if j == i || taken[j] {
                                continue;
                            }
                            let o = self.markers[j].at;
                            let d = ((o.x - e.at.x) * k).hypot((o.y - e.at.y) * k);
                            if d < r {
                                members.push(j);
                            }
                        }
                    }
                }
                if members.len() < 2 {
                    continue;
                }
                let n = members.len() as f64;
                let mut at = WorldPoint::new(0.0, 0.0);
                for &m in &members {
                    taken[m] = true;
                    gathered[m] = true;
                    at = at + self.markers[m].at * (1.0 / n);
                }
                let index = self.clusters.len();
                let content = Rc::clone(&cluster.content);
                let count = members.len();
                let node = pinned(
                    s,
                    self.overlay,
                    Alignment2D::Center,
                    me,
                    Pressed::Cluster(index),
                    move |cx| content(cx, count),
                );
                self.clusters.push(ClusterEntry {
                    members,
                    at,
                    node,
                    shown: None,
                });
            }
        }
        for (e, &g) in self.markers.iter_mut().zip(&gathered) {
            if e.hidden != g {
                e.hidden = g;
                e.shown = None;
                if let Some(node) = e.node {
                    s.store.set_hidden(node, g);
                }
            }
        }
    }
}
