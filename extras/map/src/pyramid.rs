//! The tile pyramid: which tiles a view needs, which it draws while they
//! load, and which it keeps.
//!
//! A view at a zoom wants the tiles of one level that cover it, turned as
//! it is, and a ring of prefetched tiles past its edges. Each frame's
//! [`plan`](Pyramid::plan) asks for what is missing, nearest the focus
//! first and a bounded number at once; drops the requests nothing wants any
//! more; and lists what to draw: each wanted tile that is ready, else its
//! nearest ready ancestor, else its ready children, ancestors beneath. Ready
//! tiles stay cached, least recently drawn evicted first, within a byte
//! budget.
//!
//! The pyramid holds what a ready tile carries without looking into it, so
//! it is tested apart from any drawing.

use std::collections::HashMap;

use crate::camera::View;
use crate::geo::{WorldPoint, tile_square};
use crate::source::TileId;

/// A tile as drawn: the source tile and the copy of the world it sits in,
/// counted east from the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Slot {
    pub tile: TileId,
    pub wrap: i32,
}

impl Slot {
    /// `tile` in the first copy of the world.
    pub const fn new(tile: TileId) -> Self {
        Self { tile, wrap: 0 }
    }

    /// The slot of the column `x` of zoom `z` counted from the first copy's
    /// west edge, which may lie in another copy.
    pub fn unwrapped(z: u8, x: i64, y: u32) -> Self {
        let n = i64::from(TileId::span(z));
        Self {
            tile: TileId::new(z, x.rem_euclid(n) as u32, y),
            wrap: x.div_euclid(n) as i32,
        }
    }

    /// Its north-west corner, unwrapped, and its side, in world units.
    pub fn square(self) -> (WorldPoint, f64) {
        let (nw, side) = tile_square(self.tile);
        (WorldPoint::new(nw.x + f64::from(self.wrap), nw.y), side)
    }

    /// The slot of its ancestor `levels` up, in the same copy.
    pub fn ancestor(self, levels: u8) -> Self {
        Self {
            tile: self.tile.ancestor(levels),
            wrap: self.wrap,
        }
    }

    /// The slots of its four children.
    pub fn children(self) -> [Self; 4] {
        self.tile.children().map(|tile| Self {
            tile,
            wrap: self.wrap,
        })
    }
}

/// The tiles of zoom `z` the view shows with `margin` points past its
/// edges, nearest its focus first, into `out`; `scratch` is kept between
/// calls so a steady view allocates nothing.
pub fn covering(
    view: &View,
    z: u8,
    margin: f64,
    scratch: &mut Vec<(f64, Slot)>,
    out: &mut Vec<Slot>,
) {
    out.clear();
    scratch.clear();
    let [w, h] = view.size;
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let (lo_v, hi_v) = ([-margin, -margin], [w + margin, h + margin]);
    let corners = [
        view.to_world([lo_v[0], lo_v[1]]),
        view.to_world([hi_v[0], lo_v[1]]),
        view.to_world([hi_v[0], hi_v[1]]),
        view.to_world([lo_v[0], hi_v[1]]),
    ];
    let (mut lo, mut hi) = (corners[0], corners[0]);
    for c in &corners[1..] {
        lo = WorldPoint::new(lo.x.min(c.x), lo.y.min(c.y));
        hi = WorldPoint::new(hi.x.max(c.x), hi.y.max(c.y));
    }
    let n = f64::from(TileId::span(z));
    let rows = (lo.y.max(0.0) * n).floor() as i64..=((hi.y.min(1.0) * n).ceil() as i64 - 1);
    let cols = (lo.x * n).floor() as i64..=((hi.x * n).ceil() as i64 - 1);
    // At most a few copies of the world either way.
    let max_cols = (4.0 * n) as i64;
    if cols.end() - cols.start() > max_cols {
        return;
    }
    let focus = view.focus();
    let found = scratch;
    for y in rows {
        if y < 0 || y as f64 >= n {
            continue;
        }
        for x in cols.clone() {
            let slot = Slot::unwrapped(z, x, y as u32);
            let (nw, side) = slot.square();
            // The view's corners against the tile's axes...
            if hi.x <= nw.x || lo.x >= nw.x + side || hi.y <= nw.y || lo.y >= nw.y + side {
                continue;
            }
            // ...and the tile's against the view's.
            let pts = [
                view.to_view(nw),
                view.to_view(WorldPoint::new(nw.x + side, nw.y)),
                view.to_view(WorldPoint::new(nw.x + side, nw.y + side)),
                view.to_view(WorldPoint::new(nw.x, nw.y + side)),
            ];
            let (mut a, mut b) = (pts[0], pts[0]);
            for p in &pts[1..] {
                a = [a[0].min(p[0]), a[1].min(p[1])];
                b = [b[0].max(p[0]), b[1].max(p[1])];
            }
            if b[0] <= lo_v[0] || a[0] >= hi_v[0] || b[1] <= lo_v[1] || a[1] >= hi_v[1] {
                continue;
            }
            let c = [
                (a[0] + b[0]) / 2.0 - focus[0],
                (a[1] + b[1]) / 2.0 - focus[1],
            ];
            found.push((c[0] * c[0] + c[1] * c[1], slot));
        }
    }
    found.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    out.extend(found.iter().map(|&(_, s)| s));
}

/// What a ready tile was built for: a style's revision and the zoom band
/// it was built at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuildKey {
    pub style: u64,
    pub band: u8,
}

/// A tile's state in the pyramid.
#[derive(Debug, Clone)]
pub enum TileState<T> {
    /// Asked of the source, or being built.
    Loading,
    /// Built.
    Ready {
        data: T,
        key: BuildKey,
        /// Being rebuilt for another key, shown as it is meanwhile.
        rebuilding: bool,
    },
    /// The source has no such tile.
    Missing,
    /// The source failed; asked again from the plan `retry`.
    Failed { retry: u64 },
}

#[derive(Debug, Clone)]
struct Entry<T> {
    state: TileState<T>,
    bytes: usize,
    /// The plan that last wanted or drew it.
    used: u64,
}

/// What one plan decided.
#[derive(Debug, Default, Clone)]
pub struct Plan {
    /// Tiles to ask the source for and build.
    pub fetch: Vec<TileId>,
    /// Ready tiles to build again from what they hold, for the plan's key.
    pub rebuild: Vec<TileId>,
    /// Requests nothing wants any more: stop them.
    pub cancel: Vec<TileId>,
    /// What to draw, ancestors before what lies over them.
    pub draw: Vec<Slot>,
}

impl Plan {
    fn clear(&mut self) {
        self.fetch.clear();
        self.rebuild.clear();
        self.cancel.clear();
        self.draw.clear();
    }
}

/// How the pyramid is bounded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    /// The bytes of ready tiles it keeps.
    pub bytes: usize,
    /// The requests in flight at once.
    pub requests: usize,
    /// How many levels up a missing tile looks for one to show instead.
    pub ancestors: u8,
    /// Plans a failed tile waits before it is asked for again.
    pub retry: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            bytes: 192 << 20,
            requests: 12,
            ancestors: 8,
            retry: 240,
        }
    }
}

/// The tile pyramid of one source; `T` is what a ready tile carries.
#[derive(Debug)]
pub struct Pyramid<T> {
    entries: HashMap<TileId, Entry<T>>,
    /// The source's zoom range.
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub budget: Budget,
    clock: u64,
    bytes: usize,
    /// Scratch kept between plans.
    seen: Vec<Slot>,
    evict: Vec<(u64, TileId)>,
}

impl<T: Clone> Pyramid<T> {
    /// An empty pyramid over a source of zooms `min_zoom..=max_zoom`.
    pub fn new(min_zoom: u8, max_zoom: u8) -> Self {
        Self {
            entries: HashMap::new(),
            min_zoom,
            max_zoom: max_zoom.max(min_zoom),
            budget: Budget::default(),
            clock: 0,
            bytes: 0,
            seen: Vec::new(),
            evict: Vec::new(),
        }
    }

    /// The level of tiles a view in zoom band `band` draws: the band
    /// within the source's range, past its deepest level drawn larger.
    pub fn tile_zoom(&self, band: u8) -> u8 {
        band.clamp(self.min_zoom, self.max_zoom)
    }

    /// The bytes of the tiles it holds.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The tiles it holds in any state.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The state of `tile`, if held.
    pub fn state(&self, tile: TileId) -> Option<&TileState<T>> {
        self.entries.get(&tile).map(|e| &e.state)
    }

    /// What ready `tile` carries.
    pub fn ready(&self, tile: TileId) -> Option<&T> {
        match &self.entries.get(&tile)?.state {
            TileState::Ready { data, .. } => Some(data),
            _ => None,
        }
    }

    fn is_ready(&self, tile: TileId) -> bool {
        matches!(
            self.entries.get(&tile).map(|e| &e.state),
            Some(TileState::Ready { .. })
        )
    }

    /// The requests in flight.
    pub fn loading(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(e.state, TileState::Loading))
            .count()
    }

    /// Plans a frame that wants `visible` drawn and `prefetch` ready soon,
    /// each nearest first, built for `key`, into `out`. A tile it says to
    /// fetch or rebuild is marked loading until [`arrive`](Self::arrive)d.
    pub fn plan(&mut self, visible: &[Slot], prefetch: &[Slot], key: BuildKey, out: &mut Plan) {
        out.clear();
        self.clock += 1;
        let clock = self.clock;
        let mut in_flight = self.loading();
        // Ask for what is wanted, the visible tiles first.
        for (i, slot) in visible.iter().chain(prefetch).enumerate() {
            let tile = slot.tile;
            let is_visible = i < visible.len();
            match self.entries.get_mut(&tile) {
                Some(entry) => {
                    entry.used = clock;
                    match &mut entry.state {
                        TileState::Ready {
                            key: built,
                            rebuilding,
                            ..
                        } if *built != key && !*rebuilding && is_visible => {
                            *rebuilding = true;
                            out.rebuild.push(tile);
                        }
                        TileState::Failed { retry }
                            if *retry <= clock && in_flight < self.budget.requests =>
                        {
                            entry.state = TileState::Loading;
                            in_flight += 1;
                            out.fetch.push(tile);
                        }
                        _ => {}
                    }
                }
                None => {
                    if in_flight < self.budget.requests || (is_visible && i < 4) {
                        self.entries.insert(
                            tile,
                            Entry {
                                state: TileState::Loading,
                                bytes: 0,
                                used: clock,
                            },
                        );
                        in_flight += 1;
                        out.fetch.push(tile);
                    }
                }
            }
        }
        // Stop what nothing wants.
        for (tile, entry) in &self.entries {
            if matches!(entry.state, TileState::Loading) && entry.used != clock {
                out.cancel.push(*tile);
            }
        }
        for tile in &out.cancel {
            self.entries.remove(tile);
        }
        // Draw what is ready, standing in for what is not.
        self.seen.clear();
        for &slot in visible {
            if self.is_ready(slot.tile) {
                self.seen.push(slot);
                continue;
            }
            let mut stand_in = None;
            for up in 1..=self.budget.ancestors.min(slot.tile.z) {
                let a = slot.ancestor(up);
                if self.is_ready(a.tile) {
                    stand_in = Some(a);
                    break;
                }
            }
            match stand_in {
                Some(a) => self.seen.push(a),
                None => {
                    for c in slot.children() {
                        if self.is_ready(c.tile) {
                            self.seen.push(c);
                        }
                    }
                }
            }
        }
        self.seen
            .sort_unstable_by_key(|s| (s.tile.z, s.tile.y, s.wrap, s.tile.x));
        self.seen.dedup();
        for slot in &self.seen {
            if let Some(entry) = self.entries.get_mut(&slot.tile) {
                entry.used = clock;
            }
        }
        out.draw.extend_from_slice(&self.seen);
        self.trim(clock);
    }

    /// Evicts the least recently used tiles nothing wants this plan until
    /// the ready ones fit the budget.
    fn trim(&mut self, clock: u64) {
        if self.bytes <= self.budget.bytes {
            return;
        }
        self.evict.clear();
        self.evict.extend(
            self.entries
                .iter()
                .filter(|(_, e)| e.used != clock && !matches!(e.state, TileState::Loading))
                .map(|(t, e)| (e.used, *t)),
        );
        self.evict.sort_unstable();
        for i in 0..self.evict.len() {
            if self.bytes <= self.budget.bytes {
                break;
            }
            let tile = self.evict[i].1;
            if let Some(entry) = self.entries.remove(&tile) {
                self.bytes -= entry.bytes;
            }
        }
    }

    /// `tile` arrived: built into `data` of `bytes` for `key`, missing from
    /// the source (`None`), or failed (`Err`). One no longer wanted is
    /// dropped. Returns whether it is now held ready.
    pub fn arrive(
        &mut self,
        tile: TileId,
        result: Result<Option<(T, usize, BuildKey)>, ()>,
    ) -> bool {
        let clock = self.clock;
        let retry = self.budget.retry;
        let Some(entry) = self.entries.get_mut(&tile) else {
            return false;
        };
        let state = match result {
            Ok(Some((data, bytes, key))) => {
                self.bytes = self.bytes - entry.bytes + bytes;
                entry.bytes = bytes;
                TileState::Ready {
                    data,
                    key,
                    rebuilding: false,
                }
            }
            Ok(None) => {
                if matches!(entry.state, TileState::Ready { .. }) {
                    return true;
                }
                TileState::Missing
            }
            Err(()) => {
                if let TileState::Ready { rebuilding, .. } = &mut entry.state {
                    *rebuilding = false;
                    return true;
                }
                TileState::Failed {
                    retry: clock + retry,
                }
            }
        };
        let ready = matches!(state, TileState::Ready { .. });
        entry.state = state;
        ready
    }

    /// Forgets every tile, as a new source does; what was loading is
    /// returned to be stopped.
    pub fn clear(&mut self) -> Vec<TileId> {
        let loading = self
            .entries
            .iter()
            .filter(|(_, e)| matches!(e.state, TileState::Loading))
            .map(|(t, _)| *t)
            .collect();
        self.entries.clear();
        self.bytes = 0;
        loading
    }

    /// Forgets the tiles the source had none of, to ask for them again.
    pub fn forget_missing(&mut self) {
        self.entries
            .retain(|_, e| !matches!(e.state, TileState::Missing));
    }

    /// Every ready tile and what it carries.
    pub fn ready_tiles(&self) -> impl Iterator<Item = (TileId, &T)> {
        self.entries.iter().filter_map(|(t, e)| match &e.state {
            TileState::Ready { data, .. } => Some((*t, data)),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;
    use crate::geo::LngLat;

    const KEY: BuildKey = BuildKey { style: 1, band: 2 };

    fn covering_(view: &View, z: u8, margin: f64, out: &mut Vec<Slot>) {
        covering(view, z, margin, &mut Vec::new(), out);
    }

    fn view(zoom: f64, bearing: f64) -> View {
        View::new(
            Camera::new(LngLat::new(0.0, 0.0), zoom).with_bearing(bearing),
            [800.0, 600.0],
        )
    }

    #[test]
    fn the_covering_holds_the_tiles_under_the_view_nearest_first() {
        let mut out = Vec::new();
        // 800 × 600 points at zoom 2 over (0, 0): the four middle tiles of
        // a 2048-point world.
        covering_(&view(2.0, 0.0), 2, 0.0, &mut out);
        let mut tiles: Vec<_> = out.iter().map(|s| (s.tile.x, s.tile.y)).collect();
        tiles.sort();
        assert_eq!(tiles, [(1, 1), (1, 2), (2, 1), (2, 2)]);
        // Turned, the view reaches others: a long strip half a tile east
        // of a corner spans three columns and two rows, and on end one
        // column and two rows.
        let strip = |bearing| {
            View::new(
                Camera::new(LngLat::new(11.25, 0.0), 4.0).with_bearing(bearing),
                [1000.0, 100.0],
            )
        };
        covering_(&strip(0.0), 4, 0.0, &mut out);
        assert_eq!(out.len(), 6);
        covering_(&strip(90.0), 4, 0.0, &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|s| s.tile.x == 8));
        // Off the middle, the nearest comes first.
        let off = View::new(Camera::new(LngLat::new(-30.0, 30.0), 3.0), [800.0, 600.0]);
        covering_(&off, 3, 0.0, &mut out);
        assert_eq!(out[0].tile, off.camera.center.to_world().tile(3));
        // A margin brings in the ring past the edges.
        covering_(&view(3.0, 0.0), 3, 0.0, &mut out);
        let bare = out.len();
        covering_(&view(3.0, 0.0), 3, 256.0, &mut out);
        assert!(out.len() > bare);
    }

    #[test]
    fn the_covering_wraps_round_the_antimeridian() {
        let mut out = Vec::new();
        let v = View::new(Camera::new(LngLat::new(179.9, 0.0), 3.0), [800.0, 200.0]);
        covering_(&v, 3, 0.0, &mut out);
        assert!(out.iter().any(|s| s.wrap == 1 && s.tile.x == 0));
        assert!(out.iter().any(|s| s.wrap == 0 && s.tile.x == 7));
        let (nw, _) = Slot::unwrapped(3, 8, 0).square();
        assert_eq!(nw.x, 1.0);
    }

    fn slots(tiles: &[(u8, u32, u32)]) -> Vec<Slot> {
        tiles
            .iter()
            .map(|&(z, x, y)| Slot::new(TileId::new(z, x, y)))
            .collect()
    }

    #[test]
    fn a_plan_asks_for_what_is_missing_and_draws_ancestors_meanwhile() {
        let mut p: Pyramid<&'static str> = Pyramid::new(0, 14);
        let mut plan = Plan::default();
        let parent = slots(&[(1, 0, 0)]);
        p.plan(&parent, &[], KEY, &mut plan);
        assert_eq!(plan.fetch, [TileId::new(1, 0, 0)]);
        assert!(plan.draw.is_empty());
        assert!(p.arrive(TileId::new(1, 0, 0), Ok(Some(("parent", 100, KEY)))));
        let kids = slots(&[(2, 0, 0), (2, 1, 0)]);
        p.plan(&kids, &[], KEY, &mut plan);
        assert_eq!(plan.fetch.len(), 2);
        assert_eq!(plan.draw, slots(&[(1, 0, 0)]), "the parent stands in once");
        p.arrive(TileId::new(2, 0, 0), Ok(Some(("kid", 10, KEY))));
        p.plan(&kids, &[], KEY, &mut plan);
        assert!(plan.fetch.is_empty(), "the other is still loading");
        assert_eq!(plan.draw, slots(&[(1, 0, 0), (2, 0, 0)]), "beneath first");
        // Once nothing stands in for it, the parent may go.
        p.budget.bytes = 0;
        p.plan(&kids, &[], KEY, &mut plan);
        assert_eq!(p.bytes(), 110, "still drawn");
        p.arrive(TileId::new(2, 1, 0), Ok(Some(("kid", 10, KEY))));
        p.plan(&kids, &[], KEY, &mut plan);
        assert_eq!(plan.draw, kids);
        assert_eq!(p.bytes(), 20, "the parent is evicted, the kids kept");
        p.budget.bytes = 1 << 20;
        // Zoomed back out, the children stand in for the parent gone.
        let grandkid = slots(&[(3, 7, 7)]);
        p.plan(&grandkid, &[], KEY, &mut plan);
        p.plan(&parent, &[], KEY, &mut plan);
        assert_eq!(plan.fetch, [TileId::new(1, 0, 0)]);
        assert_eq!(plan.cancel, [TileId::new(3, 7, 7)], "no longer wanted");
        assert_eq!(plan.draw, kids);
    }

    #[test]
    fn requests_are_bounded_and_failures_wait() {
        let mut p: Pyramid<()> = Pyramid::new(0, 14);
        p.budget.requests = 3;
        p.budget.retry = 2;
        let mut plan = Plan::default();
        let want: Vec<Slot> = (0..8).map(|x| Slot::new(TileId::new(3, x, 0))).collect();
        p.plan(&want, &[], KEY, &mut plan);
        assert_eq!(plan.fetch.len(), 4, "the nearest few always go");
        p.plan(&want, &[], KEY, &mut plan);
        assert!(plan.fetch.is_empty());
        p.arrive(TileId::new(3, 0, 0), Err(()));
        p.arrive(TileId::new(3, 1, 0), Ok(None));
        p.plan(&want, &[], KEY, &mut plan);
        assert_eq!(plan.fetch, [TileId::new(3, 4, 0)]);
        p.arrive(TileId::new(3, 2, 0), Ok(Some(((), 1, KEY))));
        p.plan(&want, &[], KEY, &mut plan);
        assert!(plan.fetch.contains(&TileId::new(3, 0, 0)), "asked again");
        assert!(!plan.fetch.contains(&TileId::new(3, 1, 0)), "missing stays");
    }

    #[test]
    fn a_new_key_rebuilds_what_shows_and_keeps_it_meanwhile() {
        let mut p: Pyramid<u32> = Pyramid::new(0, 14);
        let mut plan = Plan::default();
        let want = slots(&[(2, 1, 1)]);
        p.plan(&want, &[], KEY, &mut plan);
        p.arrive(TileId::new(2, 1, 1), Ok(Some((1, 5, KEY))));
        let dark = BuildKey { style: 2, band: 2 };
        p.plan(&want, &[], dark, &mut plan);
        assert_eq!(plan.rebuild, [TileId::new(2, 1, 1)]);
        assert_eq!(plan.draw, want, "the old build shows meanwhile");
        p.plan(&want, &[], dark, &mut plan);
        assert!(plan.rebuild.is_empty(), "asked once");
        p.arrive(TileId::new(2, 1, 1), Ok(Some((2, 7, dark))));
        assert_eq!(p.ready(TileId::new(2, 1, 1)), Some(&2));
        assert_eq!(p.bytes(), 7);
    }
}
