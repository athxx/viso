//! The labels a view shows: which of the drawn tiles' labels win their
//! place, decided once the camera rests in a pass bounded in time, and the
//! nodes that show them over the map.
//!
//! A label is a sizeless node in the overlay holding its text (over a halo
//! of offset copies, on a shield when it has one) and its icon, centered
//! on its place. A label along a line whose text allows it bends with the
//! line: each cluster of its text is a node of its own, set on the line as
//! the view shows it and turned with it, the run kept reading upright; it
//! shows only where the line turns gently enough from glyph to glyph, and
//! claims a box per glyph. Its clusters' widths are those the text runtime
//! shapes, read once from a few hidden leaves and kept.
//!
//! As the camera moves the nodes only move — a straight line label turns
//! with the map and is kept upright, a bent one is set along its line again
//! when the camera turns, zooms or tilts — and when it rests the pass places
//! them again, highest layer and rank first, against the boxes already
//! taken. Labels that win fade in; those that lose fade out. Tilted over
//! relief, each sits on the ground at its own place.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

use viso_render::Rgba;
use viso_ui::{
    Alignment2D, BoxStyle, BuildCx, DirtyClass, FontSize, Inset, Instant, LeafStyle, Length,
    NodeId, Sizing, StackStyle, StructureCx, TextRequest, TextStyle, Turn, Vec2,
};

use super::tilted::Lift;
use crate::camera::View;
use crate::geo::WorldPoint;
use crate::icons::IconSet;
use crate::label::{Cluster, Curve, TileLabel, estimate_width};
use crate::load::Built;
use crate::pyramid::Slot;
use crate::style::FontWeight;
use crate::tess::MeshBuilder;

/// How long one pass may take before it stops where it got to.
const BUDGET_MS: u128 = 4;
/// How far past the view's edges labels are placed, in points.
const MARGIN: f64 = 48.0;
/// The side of the collision grid's cells, in points.
const CELL: f32 = 64.0;
/// How long a label takes to fade in or out, in seconds.
const FADE: f64 = 0.2;
/// How near (points) a label must be to the one showing its text to be
/// taken for it.
const SAME: f64 = 32.0;
/// How near (points) two labels of one text may show.
const REPEAT: f64 = 160.0;
/// The most a bent label's glyphs turn from one to the next, in radians.
const MAX_TURN: f64 = std::f64::consts::FRAC_PI_4;
/// The smallest a tilted view may show a label's place against its focus:
/// farther, the ground fades into the haze and its labels are left out.
const FARTHEST: f64 = 0.45;
/// How many applies a cluster's leaf may wait to be shaped before its
/// width is estimated instead.
const MEASURE_WAIT: u32 = 120;
/// The share of the halo's width its offset copies sit out by.
const HALO_REACH: f32 = 0.75;

/// A glyph of a bent label: its node (none for a space), where its middle
/// sits from the run's start and how wide it is, in points, and where it
/// was put and how it was turned, to skip writes.
struct Glyph {
    node: Option<NodeId>,
    middle: f64,
    advance: f64,
    put: [f32; 2],
    turned: f32,
}

/// A label bending along its line.
struct Bent {
    /// Its stretch of line, unwrapped near the camera when placed, the
    /// ground's height under each point (meters, raised) and the label's
    /// middle among them.
    points: Vec<WorldPoint>,
    meters: Vec<f64>,
    at: usize,
    glyphs: Vec<Glyph>,
    width: f64,
    /// The view it was last set along its line for.
    laid: Option<(View, f64)>,
}

/// A label showing (or fading).
struct Shown {
    key: u64,
    text: Arc<str>,
    node: NodeId,
    at: WorldPoint,
    /// The ground's height under it, meters raised.
    meters: f64,
    angle: Option<f32>,
    curve: Option<Bent>,
    opacity: f32,
    target: f32,
    /// Where it was put and how it was turned, to skip writes.
    put: Option<[f32; 2]>,
    turned: Option<f32>,
}

/// A label bending along its line, as shown.
#[derive(Debug, Clone, PartialEq)]
pub struct BentLabel {
    pub text: String,
    /// Its glyphs that show ink, in reading order.
    pub glyphs: Vec<PlacedGlyph>,
}

/// A glyph of a [`BentLabel`]: where its middle was set, in view points,
/// and its turn, in radians clockwise.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedGlyph {
    pub at: [f32; 2],
    pub turn: f32,
}

/// A label the pass is placing.
#[derive(Clone, Copy)]
struct Candidate {
    tile: u32,
    index: u32,
    layer: u16,
    rank: f32,
}

/// A glyph set along a line on screen: where its middle goes and which way
/// it turns, radians clockwise.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Place {
    at: [f64; 2],
    angle: f64,
}

/// The label boxes taken so far, by grid cell.
#[derive(Default)]
struct Taken {
    boxes: Vec<[f32; 4]>,
    cells: HashMap<(i32, i32), Vec<u32>>,
}

impl Taken {
    fn clear(&mut self) {
        self.boxes.clear();
        for v in self.cells.values_mut() {
            v.clear();
        }
    }

    fn range(b: [f32; 4]) -> (i32, i32, i32, i32) {
        (
            (b[0] / CELL).floor() as i32,
            (b[1] / CELL).floor() as i32,
            (b[2] / CELL).floor() as i32,
            (b[3] / CELL).floor() as i32,
        )
    }

    fn free(&self, b: [f32; 4]) -> bool {
        let (x0, y0, x1, y1) = Self::range(b);
        for x in x0..=x1 {
            for y in y0..=y1 {
                if let Some(v) = self.cells.get(&(x, y)) {
                    for &i in v {
                        let o = self.boxes[i as usize];
                        if b[0] < o[2] && o[0] < b[2] && b[1] < o[3] && o[1] < b[3] {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    fn take(&mut self, b: [f32; 4]) {
        let i = self.boxes.len() as u32;
        self.boxes.push(b);
        let (x0, y0, x1, y1) = Self::range(b);
        for x in x0..=x1 {
            for y in y0..=y1 {
                self.cells.entry((x, y)).or_default().push(i);
            }
        }
    }
}

/// The widths of the clusters bent labels set, as the text runtime shapes
/// them, by [`cluster_key`]; and the hidden leaves shaping those not yet
/// read.
#[derive(Default)]
struct Advances {
    known: HashMap<u64, f32>,
    /// Each leaf with its key, the estimate to fall back on and the
    /// applies it has waited.
    pending: Vec<(u64, NodeId, f32, u32)>,
    parent: Option<NodeId>,
}

/// The key a cluster's width is kept by: its text, size, weight and
/// language.
fn cluster_key(text: &str, l: &TileLabel) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    l.size.to_bits().hash(&mut h);
    (l.weight as u8).hash(&mut h);
    l.lang.hash(&mut h);
    h.finish()
}

/// The view's labels.
pub(crate) struct Labels {
    parent: NodeId,
    shown: Vec<Shown>,
    /// A pass is wanted when the camera next rests.
    pub due: bool,
    /// Seconds of fading the clock has run since the last apply.
    pub fade_dt: f64,
    pub icons: Rc<IconSet>,
    builder: MeshBuilder,
    taken: Taken,
    candidates: Vec<Candidate>,
    /// Labels placed in this pass, by key: where they went.
    placed: HashMap<u64, Vec<[f64; 2]>>,
    advances: Advances,
    /// Scratch for setting bent labels along their lines.
    screen: Vec<[f64; 2]>,
    along: Along,
    places: Vec<Place>,
    pub reduced_motion: bool,
    /// The last pass's counts, for tests and tuning.
    pub last: (usize, usize),
}

/// The upright screen angle of a label running at `angle` (radians in the
/// world) under a map turned to `bearing` degrees.
fn upright(angle: f32, bearing: f64) -> f32 {
    use std::f32::consts::PI;
    let mut a = angle - bearing.to_radians() as f32;
    while a > PI / 2.0 {
        a -= PI;
    }
    while a <= -PI / 2.0 {
        a += PI;
    }
    a
}

/// `a` wrapped into `(-π, π]`.
fn wrap(a: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    let a = a.rem_euclid(TAU);
    if a > PI { a - TAU } else { a }
}

/// The point `s` along `line`, whose points lie `lengths` along it; before
/// its start and past its end, on along its end segments.
fn point_at(line: &[[f64; 2]], lengths: &[f64], s: f64) -> [f64; 2] {
    let n = line.len();
    if n == 1 {
        return line[0];
    }
    let i = lengths.partition_point(|&d| d <= s).clamp(1, n - 1);
    let (a, b) = (line[i - 1], line[i]);
    let span = lengths[i] - lengths[i - 1];
    let t = if span > 0.0 {
        (s - lengths[i - 1]) / span
    } else {
        0.0
    };
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// Scratch for setting glyphs along a line: how far along it each of its
/// points lies, and how far it has turned by each, in radians either way.
#[derive(Default)]
struct Along {
    lengths: Vec<f64>,
    turns: Vec<f64>,
}

/// Sets the glyphs of a run `width` points wide — each a `(middle,
/// advance)` from the run's start — along screen polyline `line`, the run
/// centered on its point `at` and reading upright, into `out`; each glyph
/// turned with the line across its own width. `strict` asks whether the
/// run belongs there: it must lie on the line, the line may turn at most
/// [`MAX_TURN`] between neighbouring glyphs, and no glyph may read upside
/// down. Otherwise the glyphs are always set, past the line's ends along
/// its end segments.
fn lay_along(
    line: &[[f64; 2]],
    at: usize,
    width: f64,
    glyphs: impl Iterator<Item = (f64, f64)>,
    strict: bool,
    along: &mut Along,
    out: &mut Vec<Place>,
) -> bool {
    out.clear();
    let Along { lengths, turns } = along;
    lengths.clear();
    turns.clear();
    if line.is_empty() || at >= line.len() {
        return false;
    }
    let (mut total, mut turned) = (0.0, 0.0);
    let mut heading: Option<f64> = None;
    lengths.push(0.0);
    for w in line.windows(2) {
        let (dx, dy) = (w[1][0] - w[0][0], w[1][1] - w[0][1]);
        let len = dx.hypot(dy);
        if len > 1e-9 {
            let h = dy.atan2(dx);
            if let Some(prev) = heading {
                turned += wrap(h - prev).abs();
            }
            heading = Some(h);
        }
        // The turn reached at the segment's first point.
        turns.push(turned);
        total += len;
        lengths.push(total);
    }
    turns.push(turned);
    let lengths: &[f64] = lengths;
    let middle = lengths[at];
    let (from, to) = (middle - width / 2.0, middle + width / 2.0);
    if strict && (from < 0.0 || to > total || total <= 0.0) {
        return false;
    }
    // Which way it reads: along the line, or back if that reads upside
    // down; straight down reads top to bottom.
    let (a, b) = (point_at(line, lengths, from), point_at(line, lengths, to));
    let chord = (b[1] - a[1]).atan2(b[0] - a[0]);
    let back = chord > std::f64::consts::FRAC_PI_2 || chord <= -std::f64::consts::FRAC_PI_2;
    let (dir, reading) = if back {
        (-1.0, wrap(chord + std::f64::consts::PI))
    } else {
        (1.0, chord)
    };
    let start = if back { to } else { from };
    let mut last = None;
    for (m, advance) in glyphs {
        let s = start + dir * m;
        let p = point_at(line, lengths, s);
        let (u, v) = (
            point_at(line, lengths, s - dir * advance / 2.0),
            point_at(line, lengths, s + dir * advance / 2.0),
        );
        let angle = if (v[0] - u[0]).hypot(v[1] - u[1]) > 1e-9 {
            (v[1] - u[1]).atan2(v[0] - u[0])
        } else {
            reading
        };
        if strict {
            let sharp = last.is_some_and(|prev| along_turn(lengths, turns, prev, s) > MAX_TURN);
            if sharp || wrap(angle - reading).abs() > std::f64::consts::FRAC_PI_2 {
                return false;
            }
        }
        last = Some(s);
        out.push(Place { at: p, angle });
    }
    true
}

/// How far a line whose points lie `lengths` along it, having turned by
/// `turns` at each, turns between `a` and `b` along it, the points at
/// either end included.
fn along_turn(lengths: &[f64], turns: &[f64], a: f64, b: f64) -> f64 {
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    // The last point at or before `b`, and the last one before `a`.
    let upto = lengths.partition_point(|&d| d <= b);
    let before = lengths.partition_point(|&d| d < a);
    let reached = |i: usize| if i == 0 { 0.0 } else { turns[i - 1] };
    reached(upto) - reached(before)
}

/// The box a glyph of half extents `half` set at `place` covers on screen,
/// `pad` clear around it.
fn glyph_box(place: Place, half: [f64; 2], pad: f64) -> [f32; 4] {
    let (sin, cos) = (place.angle.sin().abs(), place.angle.cos().abs());
    let hw = half[0] * cos + half[1] * sin + pad;
    let hh = half[0] * sin + half[1] * cos + pad;
    let [x, y] = place.at;
    [
        (x - hw) as f32,
        (y - hh) as f32,
        (x + hw) as f32,
        (y + hh) as f32,
    ]
}

/// Its box's extent before turning: text and halo, icon above.
fn extent(l: &TileLabel) -> [f32; 2] {
    let shield = l.shield.map_or(0.0, |s| s.padding + s.border_width);
    let w = estimate_width(&l.text, l.size) + 2.0 * (l.halo_width + shield);
    let h = l.size * 1.3 + 2.0 * shield;
    match &l.icon {
        Some(i) => [w.max(i.size), h + i.size + 1.0],
        None => [w, h],
    }
}

/// A bent glyph's half height on screen, its halo included.
fn glyph_half_height(l: &TileLabel) -> f64 {
    f64::from(l.size * 0.6 + l.halo_width)
}

fn weight(w: FontWeight) -> viso_ui::FontWeight {
    match w {
        FontWeight::Regular => viso_ui::FontWeight::REGULAR,
        FontWeight::Medium => viso_ui::FontWeight(500),
        FontWeight::Bold => viso_ui::FontWeight::BOLD,
    }
}

fn rgba(c: [f32; 4]) -> Rgba {
    Rgba::new(c[0], c[1], c[2], c[3])
}

/// One run of `text` in `l`'s look, in `color`.
fn text_leaf(cx: &mut BuildCx<'_>, l: &TileLabel, text: &str, color: [f32; 4]) -> NodeId {
    let leaf = cx.leaf(LeafStyle {
        size: Sizing {
            width: Length::Fit,
            height: Length::Fit,
        },
        style: BoxStyle::NONE,
    });
    cx.text_request(
        leaf,
        TextRequest {
            text: text.to_owned(),
            font_size: FontSize::Fixed(l.size),
            color: rgba(color),
            soft_wrap: false,
            locale: l.lang.as_deref().map(str::to_owned),
            style: TextStyle {
                weight: weight(l.weight),
                ..TextStyle::default()
            },
            spans: Vec::new(),
        },
    );
    leaf.id()
}

/// What `content` builds, its parent centering it `by` off its middle: the
/// offset taken up by padding, so it needs no move of its own.
fn shifted(cx: &mut BuildCx<'_>, by: [f32; 2], content: impl FnOnce(&mut BuildCx<'_>)) {
    let pad = |v: f32| (2.0 * v).max(0.0);
    cx.stack(
        StackStyle {
            align: Alignment2D::Center,
            padding: Inset {
                left: pad(by[0]),
                top: pad(by[1]),
                right: pad(-by[0]),
                bottom: pad(-by[1]),
            },
            ..StackStyle::default()
        },
        content,
    );
}

/// `text` in `l`'s look over its halo of offset copies, all centered.
fn inked(cx: &mut BuildCx<'_>, l: &TileLabel, text: &str) {
    let hw = l.halo_width * HALO_REACH;
    if hw > 0.0 && l.halo[3] > 0.0 && l.shield.is_none() {
        for by in [[-hw, -hw], [hw, -hw], [-hw, hw], [hw, hw]] {
            shifted(cx, by, |cx| {
                text_leaf(cx, l, text, l.halo);
            });
        }
    }
    text_leaf(cx, l, text, l.color);
}

/// A sizeless stack centering what it holds on its origin.
fn sizeless() -> StackStyle {
    StackStyle {
        size: Sizing::fixed(0.0, 0.0),
        align: Alignment2D::Center,
        ..StackStyle::default()
    }
}

/// `curve`'s points in the world, `l` lying in `slot`, unwrapped by
/// `shift` worlds east.
fn world_points(slot: Slot, curve: &Curve, shift: f64, out: &mut Vec<WorldPoint>) {
    let (nw, side) = slot.square();
    let k = side / f64::from(crate::build::EXTENT);
    out.clear();
    out.extend(curve.points.iter().map(|p| {
        WorldPoint::new(
            nw.x + f64::from(p[0]) * k + shift,
            nw.y + f64::from(p[1]) * k,
        )
    }));
}

/// The view points of `points`, raised by the ground under each, into
/// `out`; their smallest size against the focus.
fn project_all(
    view: &View,
    points: &[WorldPoint],
    meters: &[f64],
    lift: Option<&Lift<'_>>,
    out: &mut Vec<[f64; 2]>,
) -> f64 {
    out.clear();
    let mut least = f64::INFINITY;
    for (i, &w) in points.iter().enumerate() {
        let h = lift.map_or(0.0, |g| g.points(view, w, meters[i]));
        let (p, k) = view.projected(w, h);
        least = least.min(k);
        out.push(p);
    }
    least
}

impl Labels {
    pub fn new(parent: NodeId) -> Self {
        Self {
            parent,
            shown: Vec::new(),
            due: true,
            fade_dt: 0.0,
            icons: Rc::new(IconSet::new()),
            builder: MeshBuilder::new(),
            taken: Taken::default(),
            candidates: Vec::new(),
            placed: HashMap::new(),
            advances: Advances::default(),
            screen: Vec::new(),
            along: Along::default(),
            places: Vec::new(),
            reduced_motion: false,
            last: (0, 0),
        }
    }

    /// Whether a fade runs.
    pub fn fading(&self) -> bool {
        self.shown.iter().any(|s| s.opacity != s.target)
    }

    /// Whether clusters are being shaped to read their widths.
    pub fn measuring(&self) -> bool {
        !self.advances.pending.is_empty()
    }

    /// The labels showing, their texts and where they are.
    pub fn showing(&self) -> impl Iterator<Item = (&str, WorldPoint)> + '_ {
        self.shown
            .iter()
            .filter(|s| s.target > 0.0)
            .map(|s| (&*s.text, s.at))
    }

    /// The bent labels showing, with where their glyphs were last set.
    pub fn bent(&self) -> Vec<BentLabel> {
        self.shown
            .iter()
            .filter(|s| s.target > 0.0)
            .filter_map(|s| {
                let b = s.curve.as_ref()?;
                let p = s.put?;
                let glyphs = b
                    .glyphs
                    .iter()
                    .filter(|g| g.node.is_some())
                    .map(|g| PlacedGlyph {
                        at: [p[0] + g.put[0], p[1] + g.put[1]],
                        turn: g.turned,
                    })
                    .collect();
                Some(BentLabel {
                    text: s.text.to_string(),
                    glyphs,
                })
            })
            .collect()
    }

    /// Reads the widths of the clusters shaped since the last apply,
    /// asking for a pass when any arrived.
    pub fn measure(&mut self, s: &mut StructureCx<'_>) {
        if self.advances.pending.is_empty() {
            return;
        }
        let mut scratch = Vec::new();
        let mut arrived = false;
        let a = &mut self.advances;
        a.pending.retain_mut(|(key, leaf, estimate, waited)| {
            let shaped = matches!(
                s.store.content_payload(*leaf),
                Some(viso_ui::Content::Text { .. })
            );
            let b = s.store.bounds(*leaf);
            let width = if shaped && b.h > 0.0 {
                // A space shapes no ink and may lay out empty.
                if b.w > 0.0 { b.w } else { *estimate }
            } else if *waited >= MEASURE_WAIT {
                *estimate
            } else {
                *waited += 1;
                return true;
            };
            a.known.insert(*key, width);
            s.store.free_tree(*leaf, s.effects, &mut scratch);
            arrived = true;
            false
        });
        if arrived {
            self.due = true;
        }
    }

    /// The widths of `l`'s clusters, or `None` while some are shaped to read
    /// them, which this asks for.
    fn advances_of(
        &mut self,
        s: &mut StructureCx<'_>,
        l: &TileLabel,
        clusters: &[Cluster],
    ) -> Option<f64> {
        let mut total = 0.0;
        let mut missing = false;
        for c in clusters {
            let text = c.of(&l.text);
            let key = cluster_key(text, l);
            match self.advances.known.get(&key) {
                Some(&w) => total += f64::from(w),
                None => {
                    missing = true;
                    if self.advances.pending.iter().any(|p| p.0 == key) {
                        continue;
                    }
                    let parent = self.measuring_parent(s);
                    let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, parent);
                    let leaf = text_leaf(&mut cx, l, text, l.color);
                    let estimate = if text.trim().is_empty() {
                        l.size * 0.27
                    } else {
                        estimate_width(text, l.size)
                    };
                    self.advances.pending.push((key, leaf, estimate, 0));
                    s.store.mark_dirty(
                        parent,
                        DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
                    );
                }
            }
        }
        (!missing).then_some(total)
    }

    /// The hidden parent of the leaves shaped to read their widths.
    fn measuring_parent(&mut self, s: &mut StructureCx<'_>) -> NodeId {
        if let Some(p) = self.advances.parent
            && s.store.arena().is_live(p)
        {
            return p;
        }
        let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, self.parent);
        let p = cx.stack(sizeless(), |_| {}).id();
        s.store.set_opacity(p, 0.0);
        self.advances.parent = Some(p);
        p
    }

    /// Places the labels of the drawn `tiles` for `view`, each raised over
    /// the ground `lift` gives.
    pub fn pass(
        &mut self,
        s: &mut StructureCx<'_>,
        view: &View,
        tiles: &[(Slot, &Built)],
        lift: Option<&Lift<'_>>,
    ) {
        self.due = false;
        let start = Instant::now();
        // A tile's labels count where no deeper drawn tile covers them.
        self.candidates.clear();
        for (ti, (_, built)) in tiles.iter().enumerate() {
            for (li, l) in built.labels.iter().enumerate() {
                self.candidates.push(Candidate {
                    tile: ti as u32,
                    index: li as u32,
                    layer: l.layer,
                    rank: l.rank,
                });
            }
        }
        self.candidates
            .sort_unstable_by(|a, b| b.layer.cmp(&a.layer).then(a.rank.total_cmp(&b.rank)));
        self.taken.clear();
        for v in self.placed.values_mut() {
            v.clear();
        }
        let cam = view.camera.center.to_world();
        let size = view.size;
        let mut winners: Vec<(Candidate, WorldPoint, [f64; 2], f64)> = Vec::new();
        let mut points: Vec<WorldPoint> = Vec::new();
        let mut meters: Vec<f64> = Vec::new();
        let mut tried = 0usize;
        let candidates = std::mem::take(&mut self.candidates);
        for (n, c) in candidates.iter().enumerate() {
            if n % 64 == 63 && start.elapsed().as_millis() >= BUDGET_MS {
                break;
            }
            tried += 1;
            let (slot, built) = tiles[c.tile as usize];
            let l = &built.labels[c.index as usize];
            let (nw, side) = slot.square();
            let k = side / f64::from(crate::build::EXTENT);
            let mut w =
                WorldPoint::new(nw.x + f64::from(l.at[0]) * k, nw.y + f64::from(l.at[1]) * k);
            let shift = (cam.x - w.x).round();
            w.x += shift;
            let covered = tiles.iter().any(|(o, _)| {
                o.tile.z > slot.tile.z && {
                    let (onw, oside) = o.square();
                    w.x >= onw.x && w.x < onw.x + oside && w.y >= onw.y && w.y < onw.y + oside
                }
            });
            if covered {
                continue;
            }
            let m = lift.map_or(0.0, |g| g.meters(w));
            let h = lift.map_or(0.0, |g| g.points(view, w, m));
            let (p, near) = view.projected(w, h);
            if p[0] < -MARGIN
                || p[1] < -MARGIN
                || p[0] > size[0] + MARGIN
                || p[1] > size[1] + MARGIN
                || near < FARTHEST
            {
                continue;
            }
            if self
                .placed
                .get(&l.key)
                .is_some_and(|v| v.iter().any(|q| (q[0] - p[0]).hypot(q[1] - p[1]) < REPEAT))
            {
                continue;
            }
            let pad = l.padding;
            if let Some(curve) = &l.curve {
                let Some(width) = self.advances_of(s, l, &curve.clusters) else {
                    continue;
                };
                world_points(slot, curve, shift, &mut points);
                meters.clear();
                meters.extend(points.iter().map(|&w| lift.map_or(0.0, |g| g.meters(w))));
                project_all(view, &points, &meters, lift, &mut self.screen);
                let known = &self.advances.known;
                let glyphs = glyph_run(l, &curve.clusters, known);
                if !lay_along(
                    &self.screen,
                    usize::from(curve.at),
                    width,
                    glyphs,
                    true,
                    &mut self.along,
                    &mut self.places,
                ) {
                    continue;
                }
                let hh = glyph_half_height(l);
                let mut free = true;
                for (place, (_, advance)) in
                    self.places.iter().zip(glyph_run(l, &curve.clusters, known))
                {
                    let b = glyph_box(*place, [advance / 2.0, hh], f64::from(pad));
                    if !self.taken.free(b) {
                        free = false;
                        break;
                    }
                }
                if !free {
                    continue;
                }
                for (place, (_, advance)) in
                    self.places.iter().zip(glyph_run(l, &curve.clusters, known))
                {
                    self.taken
                        .take(glyph_box(*place, [advance / 2.0, hh], f64::from(pad)));
                }
            } else {
                let [ew, eh] = extent(l);
                let (bw, bh) = match l.angle {
                    Some(a) => {
                        let a = upright(a, view.camera.bearing);
                        let (sin, cos) = (a.sin().abs(), a.cos().abs());
                        (ew * cos + eh * sin, ew * sin + eh * cos)
                    }
                    None => (ew, eh),
                };
                let (px, py) = (p[0] as f32, p[1] as f32);
                let b = [
                    px - bw / 2.0 - pad,
                    py - bh / 2.0 - pad,
                    px + bw / 2.0 + pad,
                    py + bh / 2.0 + pad,
                ];
                if !self.taken.free(b) {
                    continue;
                }
                self.taken.take(b);
            }
            self.placed.entry(l.key).or_default().push(p);
            winners.push((*c, w, p, m));
        }
        self.candidates = candidates;
        self.last = (tried, winners.len());
        // Keep the nodes of labels that stay, near where they showed.
        for sh in &mut self.shown {
            sh.target = 0.0;
        }
        let mut fresh: Vec<(Candidate, WorldPoint, f64)> = Vec::new();
        for (c, w, p, m) in winners {
            let (slot, built) = tiles[c.tile as usize];
            let l = &built.labels[c.index as usize];
            let near = self.shown.iter_mut().find(|sh| {
                sh.key == l.key && sh.target == 0.0 && {
                    let h = lift.map_or(0.0, |g| g.points(view, sh.at, sh.meters));
                    let q = view.to_view_at(sh.at, h);
                    (q[0] - p[0]).hypot(q[1] - p[1]) < SAME
                }
            });
            match near {
                Some(sh) => {
                    sh.target = 1.0;
                    sh.at = w;
                    sh.meters = m;
                    sh.angle = l.angle;
                    sh.put = None;
                    sh.turned = None;
                    if let (Some(b), Some(curve)) = (&mut sh.curve, &l.curve) {
                        set_points(b, slot, curve, w, lift);
                    }
                }
                None => fresh.push((c, w, m)),
            }
        }
        for (c, w, m) in fresh {
            let (slot, built) = tiles[c.tile as usize];
            let l = &built.labels[c.index as usize];
            let (node, curve) = match &l.curve {
                Some(curve) => {
                    let (node, glyphs, width) = self.build_bent(s, l, curve);
                    let mut b = Bent {
                        points: Vec::new(),
                        meters: Vec::new(),
                        at: usize::from(curve.at),
                        glyphs,
                        width,
                        laid: None,
                    };
                    set_points(&mut b, slot, curve, w, lift);
                    (node, Some(b))
                }
                None => (self.build(s, l), None),
            };
            let opacity = if self.reduced_motion { 1.0 } else { 0.0 };
            s.store.set_opacity(node, opacity);
            self.shown.push(Shown {
                key: l.key,
                text: Arc::clone(&l.text),
                node,
                at: w,
                meters: m,
                angle: l.angle,
                curve,
                opacity,
                target: 1.0,
                put: None,
                turned: None,
            });
        }
        if self.reduced_motion {
            self.fade(s, f64::INFINITY);
        }
        s.store.mark_dirty(
            self.parent,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
        );
    }

    /// A node showing `l`, centered on its origin.
    fn build(&mut self, s: &mut StructureCx<'_>, l: &TileLabel) -> NodeId {
        let icon = l.icon.as_ref().and_then(|i| {
            self.icons
                .drawing(&i.name, i.color, i.size, &mut self.builder)
                .map(|d| (d, i.size))
        });
        let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, self.parent);
        let wrapper = cx.stack(sizeless(), |cx| {
            if let Some((drawing, side)) = &icon {
                cx.drawing(Sizing::fixed(*side, *side), drawing.clone());
            }
            let style = match l.shield {
                Some(sh) => StackStyle {
                    align: Alignment2D::Center,
                    padding: Inset::all(sh.padding),
                    style: BoxStyle {
                        fill: rgba(sh.fill.linear(1.0)),
                        radius: sh.radius,
                        radius_pct: 0.0,
                        border: viso_render::Border {
                            width: sh.border_width,
                            color: rgba(sh.border.linear(1.0)),
                        },
                    },
                    ..StackStyle::default()
                },
                None => StackStyle {
                    align: Alignment2D::Center,
                    ..StackStyle::default()
                },
            };
            // Under its icon, when it has one.
            let down = icon
                .as_ref()
                .map_or(0.0, |(_, side)| side / 2.0 + l.size * 0.65 + 1.0);
            shifted(cx, [0.0, down], |cx| {
                cx.stack(style, |cx| inked(cx, l, &l.text));
            });
        });
        wrapper.id()
    }

    /// A node of `l` bending along `curve`: a sizeless one at its middle
    /// holding a node per cluster that shows ink, each centered on its own
    /// origin. Returns it, its glyphs and its width.
    fn build_bent(
        &mut self,
        s: &mut StructureCx<'_>,
        l: &TileLabel,
        curve: &Curve,
    ) -> (NodeId, Vec<Glyph>, f64) {
        let mut glyphs = Vec::with_capacity(curve.clusters.len());
        let mut pen = 0.0;
        let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, self.parent);
        let wrapper = cx.stack(
            StackStyle {
                size: Sizing::fixed(0.0, 0.0),
                align: Alignment2D::TopStart,
                ..StackStyle::default()
            },
            |cx| {
                for c in curve.clusters.iter() {
                    let text = c.of(&l.text);
                    let advance = self.advances.known.get(&cluster_key(text, l)).map_or_else(
                        || f64::from(estimate_width(text, l.size)),
                        |&w| f64::from(w),
                    );
                    let node = (!text.trim().is_empty())
                        .then(|| cx.stack(sizeless(), |cx| inked(cx, l, text)).id());
                    glyphs.push(Glyph {
                        node,
                        middle: pen + advance / 2.0,
                        advance,
                        put: [f32::NAN; 2],
                        turned: f32::NAN,
                    });
                    pen += advance;
                }
            },
        );
        (wrapper.id(), glyphs, pen)
    }

    /// Moves the labels to where the camera of `view` shows their places,
    /// each raised over the ground `lift` gives; a bent label is set along
    /// its line again when the view changed more than its center flat.
    pub fn follow(&mut self, s: &mut StructureCx<'_>, view: &View, lift: Option<&Lift<'_>>) {
        let cam = view.camera.center.to_world();
        let tilted = view.tilt().is_some();
        // What a bent label's setting depends on beyond a flat pan.
        let mut layout = *view;
        if !tilted {
            layout.camera.center = Default::default();
        }
        let layout = (layout, lift.map_or(0.0, Lift::focus));
        for sh in &mut self.shown {
            let shift = (cam.x - sh.at.x).round();
            let mut w = sh.at;
            w.x += shift;
            let h = lift.map_or(0.0, |g| g.points(view, w, sh.meters));
            let p = view.to_view_at(w, h);
            let p = [p[0] as f32, p[1] as f32];
            if sh.put != Some(p) {
                sh.put = Some(p);
                s.store.set_translate(sh.node, Vec2::new(p[0], p[1]));
            }
            if let Some(b) = &mut sh.curve {
                if b.laid == Some(layout) && !tilted {
                    continue;
                }
                b.laid = Some(layout);
                if shift != 0.0 {
                    for q in &mut b.points {
                        q.x += shift;
                    }
                    sh.at.x += shift;
                }
                project_all(view, &b.points, &b.meters, lift, &mut self.screen);
                lay_along(
                    &self.screen,
                    b.at,
                    b.width,
                    b.glyphs.iter().map(|g| (g.middle, g.advance)),
                    false,
                    &mut self.along,
                    &mut self.places,
                );
                for (g, place) in b.glyphs.iter_mut().zip(&self.places) {
                    let Some(node) = g.node else {
                        continue;
                    };
                    let at = [
                        (place.at[0] - f64::from(p[0])) as f32,
                        (place.at[1] - f64::from(p[1])) as f32,
                    ];
                    if g.put != at {
                        g.put = at;
                        s.store.set_translate(node, Vec2::new(at[0], at[1]));
                    }
                    let a = place.angle as f32;
                    if g.turned != a {
                        g.turned = a;
                        s.store.set_turn(
                            node,
                            Turn {
                                scale: 1.0,
                                rotation: a,
                                origin: Alignment2D::Center,
                            },
                        );
                    }
                }
                continue;
            }
            if let Some(a) = sh.angle {
                let a = match view.tilt() {
                    None => upright(a, view.camera.bearing),
                    // Tilted, the line runs on screen as its places do.
                    Some(_) => {
                        let step = 1.0 / view.camera.scale();
                        let ahead = WorldPoint::new(
                            w.x + f64::from(a.cos()) * step,
                            w.y + f64::from(a.sin()) * step,
                        );
                        let q = view.to_view_at(ahead, h);
                        let on = ((q[1] - f64::from(p[1])) as f32)
                            .atan2((q[0] - f64::from(p[0])) as f32);
                        upright(on, 0.0)
                    }
                };
                if sh.turned != Some(a) {
                    sh.turned = Some(a);
                    s.store.set_turn(
                        sh.node,
                        Turn {
                            scale: 1.0,
                            rotation: a,
                            origin: Alignment2D::Center,
                        },
                    );
                }
            }
        }
    }

    /// Runs the fades on by `dt` seconds, freeing the labels faded out.
    pub fn fade(&mut self, s: &mut StructureCx<'_>, dt: f64) {
        let step = (dt / FADE) as f32;
        let mut scratch = Vec::new();
        self.shown.retain_mut(|sh| {
            if sh.opacity == sh.target {
                return sh.target > 0.0;
            }
            sh.opacity = if sh.target > sh.opacity {
                (sh.opacity + step).min(sh.target)
            } else {
                (sh.opacity - step).max(sh.target)
            };
            if sh.opacity <= 0.0 && sh.target <= 0.0 {
                s.store.free_tree(sh.node, s.effects, &mut scratch);
                return false;
            }
            s.store.set_opacity(sh.node, sh.opacity);
            true
        });
    }
}

/// Each of `clusters`' `(middle, advance)` from the run's start, by the
/// widths `known`, estimated where unknown.
fn glyph_run<'a>(
    l: &'a TileLabel,
    clusters: &'a [Cluster],
    known: &'a HashMap<u64, f32>,
) -> impl Iterator<Item = (f64, f64)> + 'a {
    let mut pen = 0.0;
    clusters.iter().map(move |c| {
        let text = c.of(&l.text);
        let advance = known.get(&cluster_key(text, l)).map_or_else(
            || f64::from(estimate_width(text, l.size)),
            |&w| f64::from(w),
        );
        let m = pen + advance / 2.0;
        pen += advance;
        (m, advance)
    })
}

/// Sets `b`'s stretch of line to `curve`'s in `slot`, unwrapped to lie by
/// its middle `w`, with the ground's height under each point.
fn set_points(b: &mut Bent, slot: Slot, curve: &Curve, w: WorldPoint, lift: Option<&Lift<'_>>) {
    world_points(slot, curve, 0.0, &mut b.points);
    let at = usize::from(curve.at);
    if let Some(m) = b.points.get(at) {
        let shift = (w.x - m.x).round();
        for q in &mut b.points {
            q.x += shift;
        }
    }
    b.at = at;
    b.meters.clear();
    b.meters
        .extend(b.points.iter().map(|&q| lift.map_or(0.0, |g| g.meters(q))));
    b.laid = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_along_a_line_is_kept_upright_as_the_map_turns() {
        use std::f32::consts::PI;
        assert!((upright(0.0, 0.0)).abs() < 1e-6);
        assert!((upright(PI, 0.0)).abs() < 1e-6);
        // Running east, under a map turned 120 degrees: it would read
        // upside down, so it turns the other way.
        let a = upright(0.0, 120.0);
        assert!(a > -PI / 2.0 && a <= PI / 2.0);
        assert!((a - (-120f32.to_radians() + PI)).abs() < 1e-5);
    }

    #[test]
    fn boxes_collide_through_the_grid() {
        let mut t = Taken::default();
        assert!(t.free([0.0, 0.0, 10.0, 10.0]));
        t.take([0.0, 0.0, 10.0, 10.0]);
        assert!(!t.free([5.0, 5.0, 200.0, 7.0]));
        assert!(t.free([10.0, 0.0, 20.0, 10.0]));
        t.clear();
        assert!(t.free([5.0, 5.0, 6.0, 6.0]));
    }

    /// Five glyphs ten points wide.
    fn five() -> impl Iterator<Item = (f64, f64)> {
        (0..5).map(|i| (f64::from(i) * 10.0 + 5.0, 10.0))
    }

    fn lay(line: &[[f64; 2]], at: usize, strict: bool) -> Option<Vec<Place>> {
        let (mut along, mut out) = (Along::default(), Vec::new());
        lay_along(line, at, 50.0, five(), strict, &mut along, &mut out).then_some(out)
    }

    #[test]
    fn glyphs_follow_a_bending_line_one_by_one() {
        // East, then bending gently south-east.
        let line = [[0.0, 0.0], [40.0, 0.0], [80.0, 20.0], [120.0, 40.0]];
        let placed = lay(&line, 1, true).expect("fits");
        assert_eq!(placed.len(), 5);
        // Centered on the bend: the first glyph runs east, the last down
        // the slope, each on the line.
        assert!((placed[0].at[0] - 20.0).abs() < 1e-9 && placed[0].angle.abs() < 1e-9);
        let slope = 0.5f64.atan();
        assert!((placed[4].angle - slope).abs() < 1e-9);
        for p in &placed {
            let on = if p.at[0] <= 40.0 {
                p.at[1].abs()
            } else {
                (p.at[1] - (p.at[0] - 40.0) / 2.0).abs()
            };
            assert!(on < 1e-9, "{p:?}");
        }
        // The middle glyph straddles the bend and turns halfway.
        assert!(placed[2].angle > 0.0 && placed[2].angle < slope);
    }

    #[test]
    fn a_line_running_west_reads_back_and_a_sharp_one_rejects() {
        // West: read from its far end, glyphs turned to read upright.
        let west = [[100.0, 0.0], [0.0, 0.0]];
        let placed = lay(&west, 0, false).expect("set");
        let placed_mid = lay(&[[100.0, 0.0], [50.0, 0.0], [0.0, 0.0]], 1, true).expect("fits");
        assert!(placed.iter().all(|p| p.angle.abs() < 1e-9));
        assert!(placed_mid.windows(2).all(|w| w[1].at[0] > w[0].at[0]));
        // Straight up reads top to bottom.
        let up = lay(&[[0.0, 100.0], [0.0, 50.0], [0.0, 0.0]], 1, true).expect("fits");
        assert!(up.windows(2).all(|w| w[1].at[1] > w[0].at[1]));
        assert!(
            up.iter()
                .all(|p| (p.angle - std::f64::consts::FRAC_PI_2).abs() < 1e-9)
        );
        // A right angle under the run turns too sharply between glyphs.
        let corner = [[0.0, 0.0], [30.0, 0.0], [30.0, 30.0]];
        assert!(lay(&corner, 1, true).is_none());
        // Too short to hold the run.
        assert!(lay(&[[0.0, 0.0], [20.0, 0.0], [40.0, 0.0]], 1, true).is_none());
        // Not strict, it is set anyway, past the line's ends.
        let set = lay(&[[0.0, 0.0], [20.0, 0.0], [40.0, 0.0]], 1, false).expect("set");
        assert!((set[0].at[0] - 0.0).abs() < 1e-9 && (set[4].at[0] - 40.0).abs() < 1e-9);
    }

    #[test]
    fn a_turned_glyphs_box_covers_it() {
        let p = Place {
            at: [10.0, 10.0],
            angle: std::f64::consts::FRAC_PI_2,
        };
        let b = glyph_box(p, [6.0, 2.0], 1.0);
        assert!(
            (b[0] - 7.0).abs() < 1e-5 && (b[3] - 17.0).abs() < 1e-5,
            "{b:?}"
        );
    }
}
