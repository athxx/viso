//! The max-rects free-rectangle packer shared by the coverage and color atlases.
//!
//! Both atlases pack rectangles into a square texture and differ only in the
//! bytes-per-texel of their CPU backing; the placement geometry is identical,
//! so it lives here once. The packer works in atlas *texel* space (integer
//! origin/size) and knows nothing about pixels or channels — callers translate
//! its origins into blits and normalized UVs.
//!
//! Allocation is best-short-side-fit over the set of maximal empty rectangles:
//! among the free rects a box fits in, pick the one whose smaller leftover
//! dimension is smallest (ties broken by the larger leftover, then leftover
//! area), so the tightest pocket is consumed first and large open regions stay
//! open. A placement splits each intersecting free rect into its (up to four)
//! L-shaped remainders and prunes any remainder wholly contained in another.

/// A rectangle in atlas texel space (integer origin/size, top-left origin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TexelRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl TexelRect {
    pub fn max_x(self) -> u32 {
        self.x + self.w
    }

    pub fn max_y(self) -> u32 {
        self.y + self.h
    }

    /// Whether this rectangle fully contains `other`.
    fn contains(self, other: TexelRect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.max_x() <= self.max_x()
            && other.max_y() <= self.max_y()
    }

    /// Whether this rectangle overlaps `other` with positive area.
    fn intersects(self, other: TexelRect) -> bool {
        self.x < other.max_x()
            && self.max_x() > other.x
            && self.y < other.max_y()
            && self.max_y() > other.y
    }
}

/// The max-rects free-rectangle packer for one atlas plane.
///
/// Holds the set of maximal empty rectangles; allocation is best-short-side-fit
/// over them. Callers see texel origins, never the internal free set.
#[derive(Debug, Clone)]
pub struct RectPacker {
    size: u32,
    free: Vec<TexelRect>,
}

impl RectPacker {
    pub fn new(size: u32) -> Self {
        Self {
            size,
            free: vec![TexelRect {
                x: 0,
                y: 0,
                w: size,
                h: size,
            }],
        }
    }

    /// Reset to a single empty rect covering the whole atlas.
    pub fn reset(&mut self) {
        self.free.clear();
        self.free.push(TexelRect {
            x: 0,
            y: 0,
            w: self.size,
            h: self.size,
        });
    }

    /// The best free-rect origin for a `w×h` box, best-short-side-fit, or `None`
    /// if it fits in no free rect. Rank is `(short_leftover, long_leftover,
    /// leftover_area)`, smallest wins.
    pub fn best_origin(&self, w: u32, h: u32) -> Option<(u32, u32)> {
        let mut best: Option<(u32, u32)> = None;
        let mut best_rank = (u32::MAX, u32::MAX, u32::MAX);
        for f in self.free.iter().copied() {
            if w > f.w || h > f.h {
                continue;
            }
            let left_w = f.w - w;
            let left_h = f.h - h;
            let short = left_w.min(left_h);
            let long = left_w.max(left_h);
            let area = f.w * f.h - w * h;
            let rank = (short, long, area);
            if rank < best_rank {
                best_rank = rank;
                best = Some((f.x, f.y));
            }
        }
        best
    }

    /// Allocate a `w×h` box, returning its texel origin. Splits every free rect
    /// the used box intersects into its L-shaped remainders and prunes any
    /// remainder contained in another.
    pub fn allocate(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let (x, y) = self.best_origin(w, h)?;
        let used = TexelRect { x, y, w, h };
        let mut next = Vec::with_capacity(self.free.len() * 2);
        for f in self.free.drain(..) {
            if !f.intersects(used) {
                next.push(f);
                continue;
            }
            split(f, used, &mut next);
        }
        self.free = next;
        prune_contained(&mut self.free);
        Some((x, y))
    }
}

/// Split `free` around the intersecting `used` box into up to four maximal
/// remainder rects (top, bottom, left, right bands), pushing the non-empty ones.
fn split(free: TexelRect, used: TexelRect, out: &mut Vec<TexelRect>) {
    // Top band (above `used` within `free`).
    if used.y > free.y {
        out.push(TexelRect {
            x: free.x,
            y: free.y,
            w: free.w,
            h: used.y - free.y,
        });
    }
    // Bottom band (below `used`).
    if used.max_y() < free.max_y() {
        out.push(TexelRect {
            x: free.x,
            y: used.max_y(),
            w: free.w,
            h: free.max_y() - used.max_y(),
        });
    }
    // Left band (left of `used`).
    if used.x > free.x {
        out.push(TexelRect {
            x: free.x,
            y: free.y,
            w: used.x - free.x,
            h: free.h,
        });
    }
    // Right band (right of `used`).
    if used.max_x() < free.max_x() {
        out.push(TexelRect {
            x: used.max_x(),
            y: free.y,
            w: free.max_x() - used.max_x(),
            h: free.h,
        });
    }
}

/// Drop any free rect wholly contained in another (max-rects maximality).
fn prune_contained(rects: &mut Vec<TexelRect>) {
    let mut i = 0;
    while i < rects.len() {
        let mut remove_i = false;
        let mut j = i + 1;
        while j < rects.len() {
            if rects[i].contains(rects[j]) {
                rects.swap_remove(j);
                continue;
            }
            if rects[j].contains(rects[i]) {
                remove_i = true;
                break;
            }
            j += 1;
        }
        if remove_i {
            rects.swap_remove(i);
        } else {
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_alloc_lands_at_origin() {
        let mut p = RectPacker::new(64);
        assert_eq!(p.allocate(10, 10), Some((0, 0)));
    }

    #[test]
    fn two_allocs_do_not_overlap() {
        let mut p = RectPacker::new(64);
        let a = p.allocate(10, 10).unwrap();
        let b = p.allocate(10, 10).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn oversize_box_fits_nowhere() {
        let mut p = RectPacker::new(8);
        assert_eq!(p.allocate(9, 9), None);
    }

    #[test]
    fn reset_reopens_the_whole_atlas() {
        let mut p = RectPacker::new(16);
        assert!(p.allocate(16, 16).is_some());
        assert_eq!(p.allocate(1, 1), None);
        p.reset();
        assert_eq!(p.allocate(16, 16), Some((0, 0)));
    }
}
