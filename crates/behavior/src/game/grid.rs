//! A uniform grid over boxes, the broadphase of the world's physics step.
//!
//! A box covering at most [`WIDE`] cells is entered once per cell into a
//! list sorted by cell key, so a query binary-searches each cell its region
//! covers; a box covering more (a floor) goes to a short list every query
//! returns. A query returns every box whose cells meet the region's: a
//! superset of the boxes overlapping it, which the caller tests exactly.
//! Boxes and regions come from [`bounds`], padded past any rounding of the
//! exact tests, so a box the query misses cannot overlap the region.
//!
//! Two boxes that overlap share a cell: the one holding the lowest corner of
//! their overlap, which a pair search ([`Grid::cells`]) counts each pair in
//! once.
//!
//! Building sorts the entries; the buffers are kept, so a warmed-up grid
//! rebuilds without allocating.

/// The most cells a box is entered in before it counts as wide.
const WIDE: i64 = 64;

/// The most cells a query visits before it gives up and the caller tests
/// every box.
const MAX_QUERY: i64 = 512;

/// The cell coordinate range, ±2^20, which the 21-bit fields of a key hold.
const LIMIT: i32 = (1 << 20) - 1;

/// A box's lowest and highest corners.
pub(super) type Bounds = ([f32; 3], [f32; 3]);

/// The bounds of a box at `center` with half extents `half`, padded by a
/// millimetre and a relative 2^-17 of its magnitude, well past the rounding
/// of an `f32` comparison of centres and extents.
pub(super) fn bounds(center: [f32; 3], half: [f32; 3]) -> Bounds {
    let mut lo = [0.0; 3];
    let mut hi = [0.0; 3];
    for i in 0..3 {
        let pad = 1e-3 + (center[i].abs() + half[i]) * (1.0 / 131_072.0);
        lo[i] = center[i] - half[i] - pad;
        hi[i] = center[i] + half[i] + pad;
    }
    (lo, hi)
}

/// A grid of slots by the cells their boxes cover.
#[derive(Debug, Default)]
pub(super) struct Grid {
    /// Cells per metre.
    inv: f32,
    /// `(cell key, slot)`, sorted once built.
    entries: Vec<(u64, u32)>,
    wide: Vec<u32>,
}

impl Grid {
    /// Empties it for cells `cell` metres wide.
    pub(super) fn clear(&mut self, cell: f32) {
        self.inv = 1.0 / cell;
        self.entries.clear();
        self.wide.clear();
    }

    /// Enters `slot` with box `bounds`.
    pub(super) fn insert(&mut self, slot: u32, (lo, hi): Bounds) {
        let (lo, hi) = (self.cell(lo), self.cell(hi));
        if span(lo, hi) > WIDE {
            self.wide.push(slot);
            return;
        }
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    self.entries.push((key([x, y, z]), slot));
                }
            }
        }
    }

    /// Sorts the entries by cell; call it after the last insert, before a
    /// query.
    pub(super) fn finish(&mut self) {
        self.entries.sort_unstable_by_key(|e| e.0);
    }

    /// Appends to `out` every slot whose box may overlap `bounds`, a slot
    /// covering several of its cells once per cell. False, appending
    /// nothing, when the region covers too many cells to be worth it.
    pub(super) fn query(&self, (lo, hi): Bounds, out: &mut Vec<u32>) -> bool {
        let (lo, hi) = (self.cell(lo), self.cell(hi));
        if span(lo, hi) > MAX_QUERY {
            return false;
        }
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    let key = key([x, y, z]);
                    let start = self.entries.partition_point(|e| e.0 < key);
                    let cell = self.entries[start..].iter().take_while(|e| e.0 == key);
                    out.extend(cell.map(|e| e.1));
                }
            }
        }
        out.extend_from_slice(&self.wide);
        true
    }

    /// The slots of each cell that has any, cell by cell, with the cell;
    /// built.
    pub(super) fn cells(&self) -> impl Iterator<Item = ([i32; 3], &[(u64, u32)])> {
        self.entries
            .chunk_by(|a, b| a.0 == b.0)
            .map(|run| (cell_of(run[0].0), run))
    }

    /// The wide slots.
    pub(super) fn wide(&self) -> &[u32] {
        &self.wide
    }

    /// The cell of point `p`, each coordinate clamped to the key range;
    /// monotone, so overlapping boxes keep overlapping cell ranges, and the
    /// cell of a componentwise maximum is the maximum of the cells.
    pub(super) fn cell(&self, p: [f32; 3]) -> [i32; 3] {
        // `as` saturates and maps NaN to 0; a NaN box overlaps nothing.
        p.map(|v| ((v * self.inv).floor() as i32).clamp(-LIMIT, LIMIT))
    }
}

/// The number of cells from `lo` to `hi`.
fn span(lo: [i32; 3], hi: [i32; 3]) -> i64 {
    (0..3)
        .map(|i| i64::from(hi[i]) - i64::from(lo[i]) + 1)
        .product()
}

/// A cell's key: its three coordinates offset into 21 bits each.
fn key(cell: [i32; 3]) -> u64 {
    cell.iter()
        .fold(0, |key, &c| key << 21 | (c + LIMIT) as u64)
}

/// The cell a key names.
fn cell_of(key: u64) -> [i32; 3] {
    let field = |shift: u32| ((key >> shift) & 0x1f_ffff) as i32 - LIMIT;
    [field(42), field(21), field(0)]
}
