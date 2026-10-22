//! Virtual grids: a virtual list whose items flow into lanes across its axis.
//!
//! A grid is a [`virtual_list`](crate::virtual_list) given a [`ListGrid`]:
//! its items still mount only about the viewport and recycle as it scrolls,
//! but they flow into lanes across the scroll axis — a fixed count, or as many
//! lanes at least `min` wide as the viewport fits, recounted as it resizes.
//! An item that spans the line (a section header always does) takes the whole
//! cross extent and starts a line of its own.
//!
//! Laid on lines, the items fill a line lane by lane, and a line is as long as
//! its longest item: a photo grid. Laid as masonry, each item goes on the lane
//! that ends soonest, packed under the item above it: a board of cards of
//! uneven length. A masonry item keeps its lane once it is measured, so the
//! items shown never jump lanes as the items before them are measured.
//!
//! [`Flow`] is the position model a list holds: a plain list's height tree, a
//! grid's lines or a masonry's lanes, answering where an item sits and which
//! items a stretch of the scroll axis shows.

use crate::virtual_list::{DRIFT_EPSILON, HeightTree};

/// The lane of an item that spans the whole line.
pub const FULL_LANE: u16 = u16::MAX;

/// How many lanes a grid has across its axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Columns {
    /// Exactly this many.
    Fixed(u16),
    /// As many at least `min` wide as fit.
    Adaptive { min: f32 },
}

/// How a virtual list lays its items out in lanes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ListGrid {
    /// Its lanes.
    pub columns: Columns,
    /// The room between two items along the scroll axis.
    pub gap: f32,
    /// The room between two lanes.
    pub cross_gap: f32,
    /// Whether each item goes on the lane that ends soonest rather than on
    /// lines.
    pub masonry: bool,
}

impl Default for ListGrid {
    fn default() -> Self {
        ListGrid {
            columns: Columns::Fixed(2),
            gap: 0.0,
            cross_gap: 0.0,
            masonry: false,
        }
    }
}

impl ListGrid {
    /// The lanes a cross extent `cross` holds.
    pub fn lanes_in(&self, cross: f32) -> u16 {
        match self.columns {
            Columns::Fixed(n) => n.clamp(1, FULL_LANE - 1),
            Columns::Adaptive { min } => {
                crate::layout::adaptive_column_count(min, self.cross_gap, cross)
                    .clamp(1, FULL_LANE - 1)
            }
        }
    }
}

/// The items' own extents along the axis, measured or estimated, and which
/// span the line.
#[derive(Debug, Clone)]
struct Cells {
    heights: Vec<f32>,
    measured: Vec<bool>,
    default: f32,
    /// Empty when no item spans the line.
    full: Vec<bool>,
}

impl Cells {
    fn new(count: usize, default: f32) -> Self {
        Cells {
            heights: vec![default; count],
            measured: vec![false; count],
            default,
            full: Vec::new(),
        }
    }

    fn count(&self) -> usize {
        self.heights.len()
    }

    fn full(&self, i: usize) -> bool {
        self.full.get(i).copied().unwrap_or(false)
    }

    fn update(&mut self, i: usize, h: f32) -> bool {
        if i >= self.count() {
            return false;
        }
        self.measured[i] = true;
        if self.heights[i] == h {
            return false;
        }
        self.heights[i] = h;
        true
    }

    fn resize(&mut self, n: usize) {
        self.heights.resize(n, self.default);
        self.measured.resize(n, false);
        if !self.full.is_empty() {
            self.full.resize(n, false);
        }
    }

    fn update_default(&mut self, default: f32) -> bool {
        if (default - self.default).abs() < DRIFT_EPSILON {
            return false;
        }
        self.default = default;
        let mut changed = false;
        for (h, &m) in self.heights.iter_mut().zip(&self.measured) {
            if !m && *h != default {
                *h = default;
                changed = true;
            }
        }
        changed
    }
}

/// Items laid on lines: each line holds a lane's worth, or one item that
/// spans it, and is as long as its longest item.
#[derive(Debug, Clone)]
pub(crate) struct LineFlow {
    cells: Cells,
    lanes: u16,
    gap: f32,
    lane: Vec<u16>,
    item_line: Vec<u32>,
    /// Each line's first item, then the item count.
    line_start: Vec<u32>,
    /// Each line's extent and the gap after it.
    lines: HeightTree,
}

impl LineFlow {
    fn new(cells: Cells, lanes: u16, gap: f32) -> Self {
        let mut flow = LineFlow {
            cells,
            lanes,
            gap,
            lane: Vec::new(),
            item_line: Vec::new(),
            line_start: Vec::new(),
            lines: HeightTree::new(0, 0.0),
        };
        flow.relay();
        flow
    }

    /// Break the items into lines anew. O(n), on a data change.
    fn relay(&mut self) {
        let n = self.cells.count();
        self.lane.clear();
        self.item_line.clear();
        self.line_start.clear();
        let mut at = 0u16;
        for i in 0..n {
            let full = self.cells.full(i);
            if at > 0 && (full || at == self.lanes) {
                at = 0;
            }
            if at == 0 {
                self.line_start.push(i as u32);
            }
            self.item_line.push((self.line_start.len() - 1) as u32);
            if full {
                self.lane.push(FULL_LANE);
                at = self.lanes;
            } else {
                self.lane.push(at);
                at += 1;
            }
        }
        self.line_start.push(n as u32);
        let extents = (0..self.line_count())
            .map(|l| self.line_extent(l))
            .collect();
        self.lines = HeightTree::from_heights(extents);
    }

    fn line_count(&self) -> usize {
        self.line_start.len() - 1
    }

    /// Line `l`'s longest item and the gap after it.
    fn line_extent(&self, l: usize) -> f32 {
        let (a, b) = (self.line_start[l] as usize, self.line_start[l + 1] as usize);
        self.cells.heights[a..b]
            .iter()
            .fold(0.0f32, |m, &h| m.max(h))
            + self.gap
    }

    fn top(&self, i: usize) -> f32 {
        match self.item_line.get(i) {
            Some(&l) => self.lines.prefix_sum(l as usize),
            None => self.total(),
        }
    }

    fn total(&self) -> f32 {
        if self.line_count() == 0 {
            0.0
        } else {
            (self.lines.total() - self.gap).max(0.0)
        }
    }

    fn update(&mut self, i: usize, h: f32) -> bool {
        if !self.cells.update(i, h) {
            return false;
        }
        let l = self.item_line[i] as usize;
        self.lines.update(l, self.line_extent(l))
    }

    /// The lines `[first, last]` a stretch from `from` to `to` crosses.
    fn line_span(&self, from: f32, to: f32) -> (usize, usize) {
        let first = self.lines.find_position(from).0;
        let last = self.lines.find_position(to).0.max(first);
        (first, last)
    }
}

/// Items laid as masonry: each on the lane that ends soonest, an item that
/// spans the line under the longest lane.
#[derive(Debug, Clone)]
pub(crate) struct LaneFlow {
    cells: Cells,
    lanes: u16,
    gap: f32,
    lane: Vec<u16>,
    /// Whether an item keeps its lane: it has been measured on it.
    kept: Vec<bool>,
    top: Vec<f32>,
    /// Each lane's items in order, an item that spans the line in every one.
    lane_items: Vec<Vec<u32>>,
    /// Reused: each lane's end while placing.
    ends: Vec<f32>,
    total: f32,
    /// The first item placed out of date, `usize::MAX` for none.
    stale_from: usize,
}

impl LaneFlow {
    fn new(cells: Cells, lanes: u16, gap: f32) -> Self {
        let n = cells.count();
        let mut flow = LaneFlow {
            cells,
            lanes,
            gap,
            lane: vec![0; n],
            kept: vec![false; n],
            top: vec![0.0; n],
            lane_items: vec![Vec::new(); lanes as usize],
            ends: Vec::new(),
            total: 0.0,
            stale_from: 0,
        };
        flow.settle();
        flow
    }

    fn stale(&mut self, from: usize) {
        self.stale_from = self.stale_from.min(from);
    }

    /// Place the items from the first out of date on. O(n) from there, once a
    /// frame at most.
    fn settle(&mut self) {
        let from = std::mem::replace(&mut self.stale_from, usize::MAX);
        let n = self.cells.count();
        if from == usize::MAX {
            return;
        }
        let from = from.min(n);
        let lanes = self.lanes as usize;
        let mut ends = std::mem::take(&mut self.ends);
        ends.clear();
        for list in &mut self.lane_items {
            let keep = list.partition_point(|&j| (j as usize) < from);
            list.truncate(keep);
            ends.push(match list.last() {
                Some(&j) => self.top[j as usize] + self.cells.heights[j as usize] + self.gap,
                None => 0.0,
            });
        }
        for i in from..n {
            let h = self.cells.heights[i];
            if self.cells.full(i) {
                let top = ends.iter().fold(0.0f32, |m, &e| m.max(e));
                self.top[i] = top;
                self.lane[i] = FULL_LANE;
                for (end, list) in ends.iter_mut().zip(&mut self.lane_items) {
                    *end = top + h + self.gap;
                    list.push(i as u32);
                }
                continue;
            }
            let lane = if self.kept[i] && (self.lane[i] as usize) < lanes {
                self.lane[i] as usize
            } else {
                // The lane that ends soonest, the first of a tie.
                let mut best = 0;
                for (k, &e) in ends.iter().enumerate() {
                    if e < ends[best] {
                        best = k;
                    }
                }
                best
            };
            self.kept[i] = self.cells.measured[i];
            self.lane[i] = lane as u16;
            self.top[i] = ends[lane];
            ends[lane] += h + self.gap;
            self.lane_items[lane].push(i as u32);
        }
        let end = ends.iter().fold(0.0f32, |m, &e| m.max(e));
        self.total = if n == 0 {
            0.0
        } else {
            (end - self.gap).max(0.0)
        };
        self.ends = ends;
    }

    fn resize(&mut self, n: usize) {
        let was = self.cells.count();
        self.cells.resize(n);
        self.lane.resize(n, 0);
        self.kept.resize(n, false);
        self.top.resize(n, 0.0);
        self.stale(was.min(n));
        self.settle();
    }

    /// The items a stretch from `from` to `to` shows, first and last; for
    /// each lane, the first item ending past `from` and the last starting
    /// before `to`.
    fn span(&self, from: f32, to: f32) -> (usize, usize) {
        let mut first = usize::MAX;
        let mut last = 0;
        for list in &self.lane_items {
            let ends = |j: u32| self.top[j as usize] + self.cells.heights[j as usize];
            let q = list.partition_point(|&j| ends(j) <= from);
            if let Some(&j) = list.get(q) {
                first = first.min(j as usize);
            }
            let p = list.partition_point(|&j| self.top[j as usize] < to);
            if p > 0 {
                last = last.max(list[p - 1] as usize);
            }
        }
        if first == usize::MAX {
            first = last;
        }
        (first.min(last), last)
    }
}

/// Where a virtual list's items sit along its axis and across it.
#[derive(Debug, Clone)]
pub(crate) enum Flow {
    /// One item a line, the whole cross extent each.
    List(HeightTree),
    Lines(LineFlow),
    Lanes(LaneFlow),
}

impl Flow {
    /// `count` items estimated `estimate` long, laid as `grid` says (a plain
    /// list without) on `lanes` lanes.
    pub(crate) fn new(count: usize, estimate: f32, grid: Option<ListGrid>, lanes: u16) -> Self {
        match grid {
            None => Flow::List(HeightTree::new(count, estimate)),
            Some(g) => {
                let cells = Cells::new(count, estimate);
                if g.masonry {
                    Flow::Lanes(LaneFlow::new(cells, lanes, g.gap))
                } else {
                    Flow::Lines(LineFlow::new(cells, lanes, g.gap))
                }
            }
        }
    }

    /// Where item `i` starts along the axis.
    pub(crate) fn top(&self, i: usize) -> f32 {
        match self {
            Flow::List(t) => t.prefix_sum(i),
            Flow::Lines(f) => f.top(i),
            Flow::Lanes(f) => f.top.get(i).copied().unwrap_or(f.total),
        }
    }

    /// Item `i`'s own extent along the axis.
    pub(crate) fn extent(&self, i: usize) -> f32 {
        match self {
            Flow::List(t) => t.point_query(i),
            Flow::Lines(f) => f.cells.heights.get(i).copied().unwrap_or(0.0),
            Flow::Lanes(f) => f.cells.heights.get(i).copied().unwrap_or(0.0),
        }
    }

    /// The lane item `i` is on, [`FULL_LANE`] across a whole line.
    pub(crate) fn lane(&self, i: usize) -> u16 {
        match self {
            Flow::List(_) => FULL_LANE,
            Flow::Lines(f) => f.lane.get(i).copied().unwrap_or(FULL_LANE),
            Flow::Lanes(f) => f.lane.get(i).copied().unwrap_or(FULL_LANE),
        }
    }

    /// How many lanes it lays its items on.
    pub(crate) fn lanes(&self) -> u16 {
        match self {
            Flow::List(_) => 1,
            Flow::Lines(f) => f.lanes,
            Flow::Lanes(f) => f.lanes,
        }
    }

    /// The whole extent along the axis.
    pub(crate) fn total(&self) -> f32 {
        match self {
            Flow::List(t) => t.total(),
            Flow::Lines(f) => f.total(),
            Flow::Lanes(f) => f.total,
        }
    }

    /// The estimate an item not measured yet takes.
    pub(crate) fn default_height(&self) -> f32 {
        match self {
            Flow::List(t) => t.default_height(),
            Flow::Lines(f) => f.cells.default,
            Flow::Lanes(f) => f.cells.default,
        }
    }

    /// Item `i` measured `h` long; whether that moved anything. A masonry
    /// places the items after it at the next [`settle`](Self::settle).
    pub(crate) fn update(&mut self, i: usize, h: f32) -> bool {
        match self {
            Flow::List(t) => t.update(i, h),
            Flow::Lines(f) => f.update(i, h),
            Flow::Lanes(f) => {
                let changed = f.cells.update(i, h);
                if changed || !f.kept.get(i).copied().unwrap_or(true) {
                    f.stale(i);
                }
                changed
            }
        }
    }

    /// Re-estimate every item not measured yet at `estimate`.
    pub(crate) fn update_default_height(&mut self, estimate: f32) -> bool {
        match self {
            Flow::List(t) => t.update_default_height(estimate),
            Flow::Lines(f) => {
                let changed = f.cells.update_default(estimate);
                if changed {
                    f.relay();
                }
                changed
            }
            Flow::Lanes(f) => {
                let changed = f.cells.update_default(estimate);
                if changed {
                    f.stale(0);
                }
                changed
            }
        }
    }

    /// Grow or shrink to `n` items.
    pub(crate) fn resize(&mut self, n: usize) {
        match self {
            Flow::List(t) => t.resize(n),
            Flow::Lines(f) => {
                if n != f.cells.count() {
                    f.cells.resize(n);
                    f.relay();
                }
            }
            Flow::Lanes(f) => {
                if n != f.cells.count() {
                    f.resize(n);
                }
            }
        }
    }

    /// Say which items span the line; `full` is empty for none.
    pub(crate) fn set_full(&mut self, full: Vec<bool>) {
        match self {
            Flow::List(_) => {}
            Flow::Lines(f) => {
                if f.cells.full != full {
                    f.cells.full = full;
                    f.relay();
                }
            }
            Flow::Lanes(f) => {
                if f.cells.full != full {
                    let from = f
                        .cells
                        .full
                        .iter()
                        .zip(&full)
                        .position(|(a, b)| a != b)
                        .unwrap_or(f.cells.full.len().min(full.len()));
                    f.cells.full = full;
                    f.stale(from);
                }
            }
        }
    }

    /// Lay the items on `lanes` lanes; whether that is a change.
    pub(crate) fn set_lanes(&mut self, lanes: u16) -> bool {
        match self {
            Flow::List(_) => false,
            Flow::Lines(f) => {
                if f.lanes == lanes {
                    return false;
                }
                f.lanes = lanes;
                f.relay();
                true
            }
            Flow::Lanes(f) => {
                if f.lanes == lanes {
                    return false;
                }
                f.lanes = lanes;
                f.kept.fill(false);
                f.lane_items = vec![Vec::new(); lanes as usize];
                f.stale(0);
                true
            }
        }
    }

    /// Bring a masonry's places up to date.
    pub(crate) fn settle(&mut self) {
        if let Flow::Lanes(f) = self {
            f.settle();
        }
    }

    /// The items `[start, end)` to mount for a viewport `extent` long
    /// `scroll` from the start: those it shows and `overscan` lines more
    /// past either end.
    pub(crate) fn window(&self, scroll: f32, extent: f32, overscan: usize) -> (usize, usize) {
        match self {
            Flow::List(t) => {
                let n = t.count();
                let first = t.find_position(scroll).0;
                let last = t.find_position(scroll + extent).0;
                (first.saturating_sub(overscan), (last + overscan + 1).min(n))
            }
            Flow::Lines(f) => {
                let lines = f.line_count();
                if lines == 0 {
                    return (0, 0);
                }
                let (first, last) = f.line_span(scroll, scroll + extent);
                let first = first.saturating_sub(overscan);
                let last = (last + overscan).min(lines - 1);
                (
                    f.line_start[first] as usize,
                    f.line_start[last + 1] as usize,
                )
            }
            Flow::Lanes(f) => {
                let n = f.cells.count();
                if n == 0 {
                    return (0, 0);
                }
                let (first, last) = f.span(scroll, scroll + extent);
                let more = overscan * f.lanes as usize;
                (first.saturating_sub(more), (last + more + 1).min(n))
            }
        }
    }

    /// The items a viewport `extent` long `scroll` from the start shows,
    /// first and last inclusive. The list must not be empty.
    pub(crate) fn visible(&self, scroll: f32, extent: f32) -> (usize, usize) {
        let end = (scroll + extent - 0.5).max(scroll);
        match self {
            Flow::List(t) => {
                let last_row = t.count() - 1;
                let first = t.find_position(scroll).0.min(last_row);
                (first, t.find_position(end).0.clamp(first, last_row))
            }
            Flow::Lines(f) => {
                let (first, last) = f.line_span(scroll, end);
                (
                    f.line_start[first] as usize,
                    f.line_start[last + 1] as usize - 1,
                )
            }
            Flow::Lanes(f) => f.span(scroll, end),
        }
    }

    /// Push onto `out` the stretches a snapping viewport may rest on about
    /// offset `at`, each `(start, length, item)`: the rows of a list, the
    /// lines of a grid, none of a masonry.
    pub(crate) fn snap_spans(&self, at: f32, out: &mut Vec<(f32, f32, usize)>) {
        match self {
            Flow::List(t) => {
                if t.count() == 0 {
                    return;
                }
                let (row, _) = t.find_position(at.max(0.0));
                for r in row.saturating_sub(1)..=(row + 1).min(t.count() - 1) {
                    out.push((t.prefix_sum(r), t.point_query(r), r));
                }
            }
            Flow::Lines(f) => {
                let lines = f.line_count();
                if lines == 0 {
                    return;
                }
                let (line, _) = f.lines.find_position(at.max(0.0));
                for l in line.saturating_sub(1)..=(line + 1).min(lines - 1) {
                    out.push((
                        f.lines.prefix_sum(l),
                        f.lines.point_query(l) - f.gap,
                        f.line_start[l] as usize,
                    ));
                }
            }
            Flow::Lanes(_) => {}
        }
    }

    /// The stretch a snapping viewport rests on to show item `i`: its row,
    /// its line, or itself.
    pub(crate) fn snap_span(&self, i: usize) -> (f32, f32) {
        match self {
            Flow::Lines(f) if i < f.item_line.len() => {
                let l = f.item_line[i] as usize;
                (f.lines.prefix_sum(l), f.lines.point_query(l) - f.gap)
            }
            _ => (self.top(i), self.extent(i)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(columns: u16, gap: f32, masonry: bool) -> Option<ListGrid> {
        Some(ListGrid {
            columns: Columns::Fixed(columns),
            gap,
            cross_gap: 0.0,
            masonry,
        })
    }

    #[test]
    fn lines_hold_a_lane_s_worth_each_as_long_as_their_longest() {
        let mut f = Flow::new(7, 10.0, grid(3, 2.0, false), 3);
        assert_eq!(
            (0..7).map(|i| f.lane(i)).collect::<Vec<_>>(),
            [0, 1, 2, 0, 1, 2, 0]
        );
        assert_eq!(
            (0..7).map(|i| f.top(i)).collect::<Vec<_>>(),
            [0., 0., 0., 12., 12., 12., 24.]
        );
        assert_eq!(f.total(), 34.0);
        assert!(f.update(4, 30.0));
        assert_eq!(f.top(6), 44.0, "line 1 is as long as its longest");
        assert_eq!(f.window(13.0, 5.0, 0), (3, 6));
        assert_eq!(f.window(13.0, 5.0, 1), (0, 7));
        assert_eq!(f.visible(0.0, 13.0), (0, 5));
    }

    #[test]
    fn an_item_spanning_the_line_takes_one_of_its_own() {
        let mut f = Flow::new(6, 10.0, grid(2, 0.0, false), 2);
        f.set_full(vec![true, false, false, false, true, false]);
        assert_eq!(
            (0..6).map(|i| f.lane(i)).collect::<Vec<_>>(),
            [FULL_LANE, 0, 1, 0, FULL_LANE, 0]
        );
        assert_eq!(
            (0..6).map(|i| f.top(i)).collect::<Vec<_>>(),
            [0., 10., 10., 20., 30., 40.]
        );
        let mut spans = Vec::new();
        f.snap_spans(25.0, &mut spans);
        assert_eq!(spans, [(10.0, 10.0, 1), (20.0, 10.0, 3), (30.0, 10.0, 4)]);
    }

    #[test]
    fn lanes_recount_and_reflow() {
        let mut f = Flow::new(6, 10.0, grid(2, 0.0, false), 2);
        assert!(f.set_lanes(3));
        assert!(!f.set_lanes(3));
        assert_eq!(f.top(5), 10.0);
        assert_eq!(f.total(), 20.0);
        f.resize(7);
        assert_eq!(f.top(6), 20.0);
    }

    #[test]
    fn masonry_packs_each_item_under_the_lane_ending_soonest() {
        let mut f = Flow::new(5, 10.0, grid(2, 1.0, true), 2);
        for (i, h) in [(0, 30.0), (1, 10.0), (2, 10.0), (3, 10.0), (4, 10.0)] {
            f.update(i, h);
        }
        f.settle();
        // 0 on lane 0 (0..30); 1, 2, 3 stack on lane 1 (0, 11, 22); 4 back
        // on lane 0 at 31, as lane 1 ends at 33.
        assert_eq!(
            (0..5).map(|i| f.lane(i)).collect::<Vec<_>>(),
            [0, 1, 1, 1, 0]
        );
        assert_eq!(
            (0..5).map(|i| f.top(i)).collect::<Vec<_>>(),
            [0., 0., 11., 22., 31.]
        );
        assert_eq!(f.total(), 41.0);
        assert_eq!(f.visible(12.0, 10.0), (0, 2));
        assert_eq!(f.window(35.0, 5.0, 0), (4, 5));
    }

    #[test]
    fn a_measured_masonry_item_keeps_its_lane() {
        let mut f = Flow::new(3, 10.0, grid(2, 0.0, true), 2);
        for i in 0..3 {
            f.update(i, 10.0);
        }
        f.settle();
        assert_eq!((0..3).map(|i| f.lane(i)).collect::<Vec<_>>(), [0, 1, 0]);
        // Item 0 grows: item 2 would now pack under lane 1, but stays.
        f.update(0, 40.0);
        f.settle();
        assert_eq!(f.lane(2), 0);
        assert_eq!(f.top(2), 40.0);
        assert_eq!(f.total(), 50.0);
    }

    #[test]
    fn a_masonry_item_spanning_the_line_starts_under_the_longest_lane() {
        let mut f = Flow::new(4, 10.0, grid(2, 0.0, true), 2);
        f.set_full(vec![false, false, true, false]);
        f.update(0, 25.0);
        f.settle();
        assert_eq!(f.lane(2), FULL_LANE);
        assert_eq!(f.top(2), 25.0);
        assert_eq!((f.lane(3), f.top(3)), (0, 35.0));
        assert_eq!(f.total(), 45.0);
    }

    #[test]
    fn adaptive_columns_fit_the_cross_extent() {
        let g = ListGrid {
            columns: Columns::Adaptive { min: 100.0 },
            cross_gap: 10.0,
            ..ListGrid::default()
        };
        assert_eq!(g.lanes_in(310.0), 2);
        assert_eq!(g.lanes_in(320.0), 3);
        assert_eq!(g.lanes_in(50.0), 1);
    }
}
