//! The labels a view shows: which of the drawn tiles' labels win their
//! place, decided once the camera rests in a pass bounded in time, and the
//! nodes that show them over the map.
//!
//! A label is a sizeless node in the overlay holding its text (over a halo
//! of offset copies, on a shield when it has one) and its icon, centered
//! on its place. As the camera moves the nodes only move — a line's label
//! turns with the map and is kept upright — and when it rests the pass
//! places them again, highest layer and rank first, against the boxes
//! already taken. Labels that win fade in; those that lose fade out.

use std::collections::HashMap;
use std::rc::Rc;

use viso_render::Rgba;
use viso_ui::{
    Alignment2D, BoxStyle, BuildCx, DirtyClass, FontSize, Instant, LeafStyle, Length, NodeId, Size,
    StackStyle, StructureCx, TextRequest, TextStyle, Turn, Vec2,
};

use crate::camera::View;
use crate::geo::WorldPoint;
use crate::icons::IconSet;
use crate::label::{TileLabel, estimate_width};
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

/// A label showing (or fading).
struct Shown {
    key: u64,
    text: std::sync::Arc<str>,
    node: NodeId,
    at: WorldPoint,
    angle: Option<f32>,
    opacity: f32,
    target: f32,
    /// Where it was put and how it was turned, to skip writes.
    put: Option<[f32; 2]>,
    turned: Option<f32>,
}

/// A label the pass is placing.
#[derive(Clone, Copy)]
struct Candidate {
    tile: u32,
    index: u32,
    layer: u16,
    rank: f32,
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

/// One run of `l`'s text in `color`.
fn text_leaf(cx: &mut BuildCx<'_>, l: &TileLabel, color: [f32; 4]) -> NodeId {
    let leaf = cx.leaf(LeafStyle {
        size: Size {
            width: Length::Fit,
            height: Length::Fit,
        },
        style: BoxStyle::NONE,
    });
    cx.text_request(
        leaf,
        TextRequest {
            text: l.text.to_string(),
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
            reduced_motion: false,
            last: (0, 0),
        }
    }

    /// Whether a fade runs.
    pub fn fading(&self) -> bool {
        self.shown.iter().any(|s| s.opacity != s.target)
    }

    /// The labels showing, their texts and where they are.
    pub fn showing(&self) -> impl Iterator<Item = (&str, WorldPoint)> + '_ {
        self.shown
            .iter()
            .filter(|s| s.target > 0.0)
            .map(|s| (&*s.text, s.at))
    }

    /// Places the labels of the drawn `tiles` for `view`.
    pub fn pass(&mut self, s: &mut StructureCx<'_>, view: &View, tiles: &[(Slot, &Built)]) {
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
        let mut winners: Vec<(Candidate, WorldPoint, [f64; 2])> = Vec::new();
        let mut tried = 0usize;
        for (n, c) in self.candidates.iter().enumerate() {
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
            w.x += (cam.x - w.x).round();
            let covered = tiles.iter().any(|(o, _)| {
                o.tile.z > slot.tile.z && {
                    let (onw, oside) = o.square();
                    w.x >= onw.x && w.x < onw.x + oside && w.y >= onw.y && w.y < onw.y + oside
                }
            });
            if covered {
                continue;
            }
            let p = view.to_view(w);
            if p[0] < -MARGIN
                || p[1] < -MARGIN
                || p[0] > size[0] + MARGIN
                || p[1] > size[1] + MARGIN
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
            let [ew, eh] = extent(l);
            let (bw, bh) = match l.angle {
                Some(a) => {
                    let a = upright(a, view.camera.bearing);
                    let (sin, cos) = (a.sin().abs(), a.cos().abs());
                    (ew * cos + eh * sin, ew * sin + eh * cos)
                }
                None => (ew, eh),
            };
            let pad = l.padding;
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
            self.placed.entry(l.key).or_default().push(p);
            winners.push((*c, w, p));
        }
        self.last = (tried, winners.len());
        // Keep the nodes of labels that stay, near where they showed.
        for sh in &mut self.shown {
            sh.target = 0.0;
        }
        let mut fresh: Vec<(Candidate, WorldPoint)> = Vec::new();
        for (c, w, p) in winners {
            let l = &tiles[c.tile as usize].1.labels[c.index as usize];
            let near = self.shown.iter_mut().find(|sh| {
                sh.key == l.key && sh.target == 0.0 && {
                    let q = view.to_view(sh.at);
                    (q[0] - p[0]).hypot(q[1] - p[1]) < SAME
                }
            });
            match near {
                Some(sh) => {
                    sh.target = 1.0;
                    sh.at = w;
                    sh.angle = l.angle;
                    sh.put = None;
                    sh.turned = None;
                }
                None => fresh.push((c, w)),
            }
        }
        for (c, w) in fresh {
            let l = &tiles[c.tile as usize].1.labels[c.index as usize];
            let node = self.build(s, l);
            let opacity = if self.reduced_motion { 1.0 } else { 0.0 };
            s.store.set_opacity(node, opacity);
            self.shown.push(Shown {
                key: l.key,
                text: std::sync::Arc::clone(&l.text),
                node,
                at: w,
                angle: l.angle,
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
        let mut text_block = None;
        let mut moves: Vec<(NodeId, Vec2)> = Vec::new();
        let wrapper = cx.stack(
            StackStyle {
                size: Size::fixed(0.0, 0.0),
                align: Alignment2D::Center,
                ..StackStyle::default()
            },
            |cx| {
                if let Some((drawing, side)) = &icon {
                    cx.drawing(Size::fixed(*side, *side), drawing.clone());
                }
                let style = match l.shield {
                    Some(sh) => StackStyle {
                        align: Alignment2D::Center,
                        padding: viso_ui::Inset::all(sh.padding),
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
                let block = cx.stack(style, |cx| {
                    let hw = l.halo_width;
                    if hw > 0.0 && l.halo[3] > 0.0 && l.shield.is_none() {
                        for (dx, dy) in [(-hw, -hw), (hw, -hw), (-hw, hw), (hw, hw)] {
                            let h = text_leaf(cx, l, l.halo);
                            moves.push((h, Vec2::new(dx * 0.75, dy * 0.75)));
                        }
                    }
                    text_leaf(cx, l, l.color);
                });
                text_block = Some(block.id());
            },
        );
        if let (Some((_, side)), Some(block)) = (&icon, text_block) {
            let down = side / 2.0 + l.size * 0.65 + 1.0;
            moves.push((block, Vec2::new(0.0, down)));
        }
        for (node, by) in moves {
            s.store.set_translate(node, by);
        }
        wrapper.id()
    }

    /// Moves the labels to where the camera of `view` shows their places.
    pub fn follow(&mut self, s: &mut StructureCx<'_>, view: &View) {
        let cam = view.camera.center.to_world();
        for sh in &mut self.shown {
            let mut w = sh.at;
            w.x += (cam.x - w.x).round();
            let p = view.to_view(w);
            let p = [p[0] as f32, p[1] as f32];
            if sh.put != Some(p) {
                sh.put = Some(p);
                s.store.set_translate(sh.node, Vec2::new(p[0], p[1]));
            }
            if let Some(a) = sh.angle {
                let a = upright(a, view.camera.bearing);
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
}
