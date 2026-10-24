//! Where a grid's columns stand: their order on screen, their widths, and
//! which of them a viewport scrolled across shows.

use viso_ui::AcrossView;

/// The columns built on each side of those in view, so a small scroll
/// across shows a built one.
const OVERSCAN: usize = 1;

/// How far across a viewport not yet laid out is taken to show.
const UNLAID: f32 = 1920.0;

/// Where each column starts from the grid's start edge, in screen order,
/// with the total width last: `starts[p]..starts[p + 1]` is column `p`'s.
pub(super) fn starts(order: &[usize], widths: impl Fn(usize) -> f32) -> Vec<f32> {
    let mut at = 0.0;
    let mut out = Vec::with_capacity(order.len() + 1);
    out.push(0.0);
    for &model in order {
        at += widths(model);
        out.push(at);
    }
    out
}

/// The screen positions `[first, end)` of the columns `view` shows, widened
/// by the overscan.
pub(super) fn window(starts: &[f32], view: AcrossView) -> (usize, usize) {
    let count = starts.len().saturating_sub(1);
    if count == 0 {
        return (0, 0);
    }
    let extent = if view.extent > 0.0 {
        view.extent
    } else {
        UNLAID
    };
    let from = view.offset.max(0.0);
    let to = from + extent;
    // The column holding `from`, and the first one starting at or past `to`.
    let first = starts[1..]
        .partition_point(|&end| end <= from)
        .min(count - 1);
    let end = starts[..count]
        .partition_point(|&start| start < to)
        .max(first + 1);
    (first.saturating_sub(OVERSCAN), (end + OVERSCAN).min(count))
}

/// The screen position of the column `distance` from the grid's start edge,
/// if one lies there.
pub(super) fn at(starts: &[f32], distance: f32) -> Option<usize> {
    let count = starts.len().saturating_sub(1);
    if distance < 0.0 || count == 0 || distance >= starts[count] {
        return None;
    }
    Some(starts[1..].partition_point(|&end| end <= distance))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(offset: f32, extent: f32) -> AcrossView {
        AcrossView { offset, extent }
    }

    #[test]
    fn the_window_holds_the_columns_in_view_and_one_each_side() {
        let s = starts(&[0, 1, 2, 3, 4, 5], |_| 100.0);
        assert_eq!(s.last(), Some(&600.0));
        assert_eq!(window(&s, view(0.0, 250.0)), (0, 4));
        assert_eq!(window(&s, view(250.0, 100.0)), (1, 5));
        assert_eq!(window(&s, view(500.0, 100.0)), (4, 6));
        assert_eq!(window(&s, view(0.0, 0.0)), (0, 6), "not laid out yet");
        assert_eq!(window(&[0.0], view(0.0, 100.0)), (0, 0));
    }

    #[test]
    fn a_distance_finds_its_column() {
        let s = starts(&[2, 0, 1], |m| [50.0, 80.0, 120.0][m]);
        assert_eq!(s, [0.0, 120.0, 170.0, 250.0]);
        assert_eq!(at(&s, 0.0), Some(0));
        assert_eq!(at(&s, 119.9), Some(0));
        assert_eq!(at(&s, 120.0), Some(1));
        assert_eq!(at(&s, 249.0), Some(2));
        assert_eq!(at(&s, 250.0), None);
        assert_eq!(at(&s, -1.0), None);
    }
}
