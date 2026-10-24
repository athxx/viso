//! What a grid has selected: ranges of rows, or of cells, each from where it
//! was begun to where it was taken.

/// What a grid lets the user select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GridSelect {
    /// Nothing: the cursor only moves.
    None,
    /// Whole rows.
    #[default]
    Rows,
    /// Rectangles of cells.
    Cells,
}

/// A rectangle of a grid's rows and columns, both inclusive: data rows, and
/// columns by their place on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridRange {
    pub rows: (usize, usize),
    pub columns: (usize, usize),
}

impl GridRange {
    fn holds(&self, row: usize, column: usize) -> bool {
        (self.rows.0..=self.rows.1).contains(&row)
            && (self.columns.0..=self.columns.1).contains(&column)
    }
}

/// What a grid has selected, as its handlers are told it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GridSelection {
    ranges: Vec<GridRange>,
    /// The column each place on screen shows.
    order: Vec<usize>,
}

impl GridSelection {
    /// Whether nothing is selected.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// The selected rectangles, in the order they were taken; columns by
    /// their place on screen.
    pub fn ranges(&self) -> &[GridRange] {
        &self.ranges
    }

    /// Whether data row `row`'s cell in column `column` (as declared) is
    /// selected.
    pub fn contains(&self, row: usize, column: usize) -> bool {
        self.order
            .iter()
            .position(|&c| c == column)
            .is_some_and(|place| self.ranges.iter().any(|r| r.holds(row, place)))
    }

    /// The data rows any of whose cells are selected, in order.
    pub fn rows(&self) -> Vec<usize> {
        let mut rows: Vec<usize> = self
            .ranges
            .iter()
            .flat_map(|r| r.rows.0..=r.rows.1)
            .collect();
        rows.sort_unstable();
        rows.dedup();
        rows
    }

    /// The selected cells as `(row, column)`, columns as declared, row by
    /// row and across each in screen order.
    pub fn cells(&self) -> Vec<(usize, usize)> {
        let mut cells: Vec<(usize, usize)> = self
            .ranges
            .iter()
            .flat_map(|r| {
                (r.rows.0..=r.rows.1)
                    .flat_map(move |row| (r.columns.0..=r.columns.1).map(move |p| (row, p)))
            })
            .collect();
        cells.sort_unstable();
        cells.dedup();
        cells
            .into_iter()
            .map(|(row, p)| (row, self.order[p]))
            .collect()
    }
}

/// A cell: its data row and its column's place on screen.
pub(super) type Spot = (usize, usize);

/// The selection as it is worked: each range from where it was begun to
/// where it was taken, the last the one Shift extends.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Selection {
    spans: Vec<(Spot, Spot)>,
}

impl Selection {
    /// Select `at` alone.
    pub(super) fn only(&mut self, at: Spot) {
        self.spans.clear();
        self.spans.push((at, at));
    }

    /// Select nothing.
    pub(super) fn clear(&mut self) {
        self.spans.clear();
    }

    /// Begin another range at `at`, keeping those taken.
    pub(super) fn add(&mut self, at: Spot) {
        self.spans.push((at, at));
    }

    /// Take the last range to `at` from where it was begun, or from `from`
    /// with none.
    pub(super) fn extend(&mut self, from: Spot, at: Spot) {
        match self.spans.last_mut() {
            Some(span) => span.1 = at,
            None => self.spans.push((from, at)),
        }
    }

    /// Select every cell of `rows` rows of `columns` columns.
    pub(super) fn all(&mut self, rows: usize, columns: usize) {
        self.spans.clear();
        if rows > 0 && columns > 0 {
            self.spans.push(((0, 0), (rows - 1, columns - 1)));
        }
    }

    /// Drop row `row` from every range, splitting those it was inside.
    pub(super) fn drop_row(&mut self, row: usize) {
        let mut kept = Vec::with_capacity(self.spans.len() + 1);
        for &(a, b) in &self.spans {
            let (top, bottom) = (a.0.min(b.0), a.0.max(b.0));
            if !(top..=bottom).contains(&row) {
                kept.push((a, b));
                continue;
            }
            let (left, right) = (a.1.min(b.1), a.1.max(b.1));
            if row > top {
                kept.push(((top, left), (row - 1, right)));
            }
            if row < bottom {
                kept.push(((row + 1, left), (bottom, right)));
            }
        }
        self.spans = kept;
    }

    /// Whether the last range was begun at `at` and holds nothing else.
    pub(super) fn is_only(&self, at: Spot) -> bool {
        matches!(self.spans.as_slice(), [(a, b)] if *a == at && *b == at)
    }

    /// The ranges as `mode` reads them, of a grid of `columns` columns: a
    /// row range spans every column.
    pub(super) fn ranges(&self, mode: GridSelect, columns: usize) -> Vec<GridRange> {
        if mode == GridSelect::None || columns == 0 {
            return Vec::new();
        }
        self.spans
            .iter()
            .map(|&(a, b)| GridRange {
                rows: (a.0.min(b.0), a.0.max(b.0)),
                columns: match mode {
                    GridSelect::Rows => (0, columns - 1),
                    _ => (a.1.min(b.1), a.1.max(b.1)),
                },
            })
            .collect()
    }

    /// Whether `at` is selected as `mode` reads the ranges.
    pub(super) fn holds(&self, mode: GridSelect, at: Spot) -> bool {
        let row = |a: Spot, b: Spot| (a.0.min(b.0)..=a.0.max(b.0)).contains(&at.0);
        let column = |a: Spot, b: Spot| (a.1.min(b.1)..=a.1.max(b.1)).contains(&at.1);
        match mode {
            GridSelect::None => false,
            GridSelect::Rows => self.spans.iter().any(|&(a, b)| row(a, b)),
            GridSelect::Cells => self.spans.iter().any(|&(a, b)| row(a, b) && column(a, b)),
        }
    }

    /// Whether row `row` is selected whole as `mode` reads the ranges.
    pub(super) fn holds_row(&self, mode: GridSelect, row: usize) -> bool {
        mode == GridSelect::Rows && self.holds(mode, (row, 0))
    }

    /// The selection as handlers are told it, with the column each place on
    /// screen shows.
    pub(super) fn told(&self, mode: GridSelect, order: &[usize]) -> GridSelection {
        GridSelection {
            ranges: self.ranges(mode, order.len()),
            order: order.to_vec(),
        }
    }

    /// Keep every range within `rows` rows, dropping those wholly past.
    pub(super) fn clamp(&mut self, rows: usize) {
        if rows == 0 {
            self.spans.clear();
            return;
        }
        self.spans.retain(|&(a, b)| a.0.min(b.0) < rows);
        for (a, b) in &mut self.spans {
            a.0 = a.0.min(rows - 1);
            b.0 = b.0.min(rows - 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_extend_add_and_lose_a_row() {
        let mut s = Selection::default();
        s.only((2, 1));
        s.extend((0, 0), (4, 3));
        assert!(s.holds(GridSelect::Cells, (3, 2)));
        assert!(!s.holds(GridSelect::Cells, (3, 0)));
        assert!(
            s.holds(GridSelect::Rows, (3, 0)),
            "a row range spans across"
        );
        s.add((9, 0));
        assert_eq!(
            s.ranges(GridSelect::Rows, 4),
            [
                GridRange {
                    rows: (2, 4),
                    columns: (0, 3)
                },
                GridRange {
                    rows: (9, 9),
                    columns: (0, 3)
                }
            ]
        );
        s.drop_row(3);
        assert_eq!(
            s.told(GridSelect::Rows, &[0, 1, 2, 3]).rows(),
            [2, 4, 9],
            "the row is gone, its range split"
        );
    }

    #[test]
    fn a_told_selection_reads_columns_as_declared() {
        let mut s = Selection::default();
        s.only((1, 0));
        s.extend((1, 0), (1, 1));
        // Screen order: declared column 2, then 0, then 1.
        let told = s.told(GridSelect::Cells, &[2, 0, 1]);
        assert!(told.contains(1, 2) && told.contains(1, 0));
        assert!(!told.contains(1, 1));
        assert_eq!(told.cells(), [(1, 2), (1, 0)]);
        s.clamp(1);
        assert!(s.ranges(GridSelect::Cells, 3).is_empty(), "past the rows");
    }
}
