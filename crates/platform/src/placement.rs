//! Keeping a window on the screens: a frame placed by the app (restored
//! from a session whose monitors were different, say) is moved, and shrunk
//! if it must be, so it opens wholly on a screen.

/// A rectangle on the desktop: top-left corner and size, all in one unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DeskRect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

impl DeskRect {
    pub(crate) fn new(x: f64, y: f64, width: f64, height: f64) -> DeskRect {
        DeskRect {
            x,
            y,
            width,
            height,
        }
    }

    fn overlap(&self, other: &DeskRect) -> f64 {
        let w = (self.x + self.width).min(other.x + other.width) - self.x.max(other.x);
        let h = (self.y + self.height).min(other.y + other.height) - self.y.max(other.y);
        w.max(0.0) * h.max(0.0)
    }

    /// How far `self`'s center is from the nearest point of `other`.
    fn distance(&self, other: &DeskRect) -> f64 {
        let (cx, cy) = (self.x + self.width / 2.0, self.y + self.height / 2.0);
        let dx = cx - cx.clamp(other.x, other.x + other.width);
        let dy = cy - cy.clamp(other.y, other.y + other.height);
        dx.hypot(dy)
    }
}

/// `frame` kept where it is when the screens' usable `areas` hold all of it
/// (one screen or several side by side); otherwise moved onto the area it
/// overlaps most (the nearest when it overlaps none), shrunk to fit it.
/// Itself when there are no areas to go by.
pub(crate) fn fit_on_screens(frame: DeskRect, areas: &[DeskRect]) -> DeskRect {
    let covered: f64 = areas.iter().map(|a| frame.overlap(a)).sum();
    let size = frame.width * frame.height;
    if areas.is_empty() || covered >= size - 0.5 {
        return frame;
    }
    let best = areas
        .iter()
        .max_by(|a, b| {
            let by_overlap = frame.overlap(a).total_cmp(&frame.overlap(b));
            by_overlap.then_with(|| frame.distance(b).total_cmp(&frame.distance(a)))
        })
        .copied()
        .unwrap_or(frame);
    let width = frame.width.min(best.width);
    let height = frame.height.min(best.height);
    DeskRect {
        x: frame.x.clamp(best.x, best.x + best.width - width),
        y: frame.y.clamp(best.y, best.y + best.height - height),
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: DeskRect = DeskRect {
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 1080.0,
    };
    const RIGHT: DeskRect = DeskRect {
        x: 1920.0,
        y: 0.0,
        width: 1280.0,
        height: 800.0,
    };

    #[test]
    fn a_frame_on_the_screens_stays_put() {
        let on_left = DeskRect::new(100.0, 100.0, 800.0, 600.0);
        assert_eq!(fit_on_screens(on_left, &[LEFT, RIGHT]), on_left);
        let spanning = DeskRect::new(1500.0, 100.0, 800.0, 600.0);
        assert_eq!(fit_on_screens(spanning, &[LEFT, RIGHT]), spanning);
        assert_eq!(fit_on_screens(on_left, &[]), on_left);
    }

    #[test]
    fn a_frame_off_the_screens_comes_back_onto_the_nearest() {
        // The right monitor was unplugged.
        let was_right = DeskRect::new(2200.0, 100.0, 800.0, 600.0);
        assert_eq!(
            fit_on_screens(was_right, &[LEFT]),
            DeskRect::new(1120.0, 100.0, 800.0, 600.0)
        );
        let above = DeskRect::new(300.0, -900.0, 800.0, 600.0);
        assert_eq!(
            fit_on_screens(above, &[LEFT, RIGHT]),
            DeskRect::new(300.0, 0.0, 800.0, 600.0)
        );
    }

    #[test]
    fn a_frame_hanging_off_moves_onto_the_screen_it_overlaps_most_and_shrinks() {
        let low = DeskRect::new(2000.0, 500.0, 800.0, 600.0);
        assert_eq!(
            fit_on_screens(low, &[LEFT, RIGHT]),
            DeskRect::new(2000.0, 200.0, 800.0, 600.0)
        );
        let huge = DeskRect::new(-50.0, -50.0, 2500.0, 1400.0);
        assert_eq!(fit_on_screens(huge, &[LEFT, RIGHT]), LEFT);
    }
}
