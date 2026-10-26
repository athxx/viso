//! Clipping geometry to a tile's square: rings by Sutherland–Hodgman, which
//! keeps a clipped ring a ring (running along the square's edges where it
//! was cut), and lines into the runs that lie inside.

/// An axis-aligned square from `lo` to `hi` on both axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Square {
    pub lo: f32,
    pub hi: f32,
}

impl Square {
    /// Whether `p` lies inside or on the square.
    #[inline]
    pub fn contains(self, p: [f32; 2]) -> bool {
        p[0] >= self.lo && p[0] <= self.hi && p[1] >= self.lo && p[1] <= self.hi
    }

    /// Whether the edge `a`–`b` runs along one of the square's sides.
    #[inline]
    pub fn on_side(self, a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] == b[0] && (a[0] == self.lo || a[0] == self.hi))
            || (a[1] == b[1] && (a[1] == self.lo || a[1] == self.hi))
    }
}

/// Clip `ring` to `square` into `out`, replacing what it held; `scratch` is
/// reused between the four sides. A ring wholly inside comes back as it
/// was, one wholly outside empty.
pub fn clip_ring(
    ring: &[[f32; 2]],
    square: Square,
    out: &mut Vec<[f32; 2]>,
    scratch: &mut Vec<[f32; 2]>,
) {
    out.clear();
    if ring.iter().all(|&p| square.contains(p)) {
        out.extend_from_slice(ring);
        return;
    }
    out.extend_from_slice(ring);
    // Each side: the axis it bounds, its value, and whether inside is above.
    for (axis, at, above) in [
        (0, square.lo, true),
        (0, square.hi, false),
        (1, square.lo, true),
        (1, square.hi, false),
    ] {
        std::mem::swap(out, scratch);
        out.clear();
        let n = scratch.len();
        if n == 0 {
            return;
        }
        let inside = |p: [f32; 2]| if above { p[axis] >= at } else { p[axis] <= at };
        for i in 0..n {
            let a = scratch[if i == 0 { n - 1 } else { i - 1 }];
            let b = scratch[i];
            let (ia, ib) = (inside(a), inside(b));
            if ia != ib {
                let t = (at - a[axis]) / (b[axis] - a[axis]);
                let mut p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                p[axis] = at;
                out.push(p);
            }
            if ib {
                out.push(b);
            }
        }
    }
}

/// Clip the polyline `line` to `square`, calling `run` with each run of it
/// that lies inside, in order; `scratch` holds the run being built.
pub fn clip_line(
    line: &[[f32; 2]],
    square: Square,
    scratch: &mut Vec<[f32; 2]>,
    mut run: impl FnMut(&[[f32; 2]]),
) {
    if line.iter().all(|&p| square.contains(p)) {
        run(line);
        return;
    }
    scratch.clear();
    for w in line.windows(2) {
        let Some((a, b)) = clip_segment(w[0], w[1], square) else {
            if scratch.len() >= 2 {
                run(scratch);
            }
            scratch.clear();
            continue;
        };
        if scratch.last() != Some(&a) {
            if scratch.len() >= 2 {
                run(scratch);
            }
            scratch.clear();
            scratch.push(a);
        }
        scratch.push(b);
        // The segment left the square: the run ends here.
        if b != w[1] {
            if scratch.len() >= 2 {
                run(scratch);
            }
            scratch.clear();
        }
    }
    if scratch.len() >= 2 {
        run(scratch);
    }
}

/// The part of segment `a`–`b` inside `square` (Liang–Barsky).
fn clip_segment(a: [f32; 2], b: [f32; 2], square: Square) -> Option<([f32; 2], [f32; 2])> {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [
        (-dx, a[0] - square.lo),
        (dx, square.hi - a[0]),
        (-dy, a[1] - square.lo),
        (dy, square.hi - a[1]),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let t = q / p;
        if p < 0.0 {
            t0 = t0.max(t);
        } else {
            t1 = t1.min(t);
        }
        if t0 > t1 {
            return None;
        }
    }
    let at = |t: f32| [a[0] + dx * t, a[1] + dy * t];
    let start = if t0 > 0.0 { at(t0) } else { a };
    let end = if t1 < 1.0 { at(t1) } else { b };
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQ: Square = Square { lo: 0.0, hi: 10.0 };

    #[test]
    fn rings_clip_to_the_square() {
        let (mut out, mut scratch) = (Vec::new(), Vec::new());
        let ring = [[-5.0, -5.0], [5.0, -5.0], [5.0, 5.0], [-5.0, 5.0]];
        clip_ring(&ring, SQ, &mut out, &mut scratch);
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|&p| SQ.contains(p)));
        assert!(out.contains(&[0.0, 0.0]) && out.contains(&[5.0, 5.0]));
        let edges = (0..4)
            .filter(|&i| SQ.on_side(out[i], out[(i + 1) % 4]))
            .count();
        assert_eq!(edges, 2, "two edges run along the square's sides");
        clip_ring(
            &[[20.0, 20.0], [30.0, 20.0], [30.0, 30.0]],
            SQ,
            &mut out,
            &mut scratch,
        );
        assert!(out.is_empty());
        let inside = [[1.0, 1.0], [2.0, 1.0], [2.0, 2.0]];
        clip_ring(&inside, SQ, &mut out, &mut scratch);
        assert_eq!(out, inside);
    }

    #[test]
    fn lines_clip_into_runs() {
        let mut scratch = Vec::new();
        let mut runs = Vec::new();
        // In, out, back in.
        let line = [[5.0, 5.0], [15.0, 5.0], [15.0, 8.0], [5.0, 8.0]];
        clip_line(&line, SQ, &mut scratch, |r| runs.push(r.to_vec()));
        assert_eq!(
            runs,
            vec![vec![[5.0, 5.0], [10.0, 5.0]], vec![[10.0, 8.0], [5.0, 8.0]]]
        );
        runs.clear();
        clip_line(&[[-5.0, 5.0], [15.0, 5.0]], SQ, &mut scratch, |r| {
            runs.push(r.to_vec())
        });
        assert_eq!(runs, vec![vec![[0.0, 5.0], [10.0, 5.0]]]);
        runs.clear();
        clip_line(&[[-5.0, -5.0], [-1.0, -1.0]], SQ, &mut scratch, |r| {
            runs.push(r.to_vec())
        });
        assert!(runs.is_empty());
    }
}
