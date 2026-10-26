//! Polygon triangulation by ear clipping, holes bridged into the outer
//! ring, with a z-order index over large rings so a ring of thousands of
//! points triangulates in near-linear time. Degenerate and slightly
//! self-intersecting input still yields triangles rather than failing.

/// One ring point in the working list.
#[derive(Clone, Copy)]
struct Node {
    /// The point's index in the caller's vertex list.
    i: u32,
    x: f64,
    y: f64,
    prev: u32,
    next: u32,
    /// Z-order curve value, and the neighbours along it.
    z: u32,
    prev_z: u32,
    next_z: u32,
    steiner: bool,
}

const NIL: u32 = u32::MAX;

/// The scratch a triangulation reuses.
#[derive(Default)]
pub struct Earcut {
    nodes: Vec<Node>,
    queue: Vec<u32>,
}

/// The z-order hashing window: its corner and inverse cell size.
#[derive(Clone, Copy)]
struct Hash {
    min_x: f64,
    min_y: f64,
    inv: f64,
}

impl Earcut {
    /// Triangulate the polygon whose points are `points` (flat `x, y`
    /// pairs), the outer ring first and each hole starting at the point
    /// index in `holes`. Triangles, as point indices offset by `base`, are
    /// appended to `out`.
    pub fn triangulate(
        &mut self,
        points: &[[f32; 2]],
        holes: &[u32],
        base: u32,
        out: &mut Vec<u32>,
    ) {
        self.nodes.clear();
        let outer_end = holes.first().map_or(points.len(), |&h| h as usize);
        if outer_end < 3 {
            return;
        }
        let Some(mut outer) = self.ring(points, 0, outer_end, true) else {
            return;
        };
        if self.nodes[outer as usize].next == self.nodes[outer as usize].prev {
            return;
        }
        if !holes.is_empty() {
            outer = self.eliminate_holes(points, holes, outer);
        }
        let mut hash = None;
        // Hash rings of more than eighty points.
        if points.len() > 80 {
            let (mut min_x, mut min_y) = (f64::MAX, f64::MAX);
            let (mut max_x, mut max_y) = (f64::MIN, f64::MIN);
            for p in &points[..outer_end] {
                min_x = min_x.min(p[0] as f64);
                min_y = min_y.min(p[1] as f64);
                max_x = max_x.max(p[0] as f64);
                max_y = max_y.max(p[1] as f64);
            }
            let size = (max_x - min_x).max(max_y - min_y);
            if size > 0.0 {
                hash = Some(Hash {
                    min_x,
                    min_y,
                    inv: 32767.0 / size,
                });
            }
        }
        let start = out.len();
        self.earcut_linked(outer, hash, 0, out);
        for i in &mut out[start..] {
            *i += base;
        }
    }

    fn push(&mut self, i: u32, x: f64, y: f64, last: u32) -> u32 {
        let id = self.nodes.len() as u32;
        let mut node = Node {
            i,
            x,
            y,
            prev: id,
            next: id,
            z: 0,
            prev_z: NIL,
            next_z: NIL,
            steiner: false,
        };
        if last != NIL {
            let next = self.nodes[last as usize].next;
            node.next = next;
            node.prev = last;
            self.nodes[next as usize].prev = id;
            self.nodes[last as usize].next = id;
        }
        self.nodes.push(node);
        id
    }

    /// Link `points[start..end]` into a ring of the given winding: the outer
    /// ring clockwise on screen, holes the other way.
    fn ring(
        &mut self,
        points: &[[f32; 2]],
        start: usize,
        end: usize,
        clockwise: bool,
    ) -> Option<u32> {
        let area: f64 = (start..end)
            .map(|i| {
                let j = if i + 1 == end { start } else { i + 1 };
                let (a, b) = (points[i], points[j]);
                (a[0] as f64 - b[0] as f64) * (a[1] as f64 + b[1] as f64)
            })
            .sum();
        let mut last = NIL;
        if clockwise == (area > 0.0) {
            for (i, p) in points.iter().enumerate().take(end).skip(start) {
                last = self.push(i as u32, p[0] as f64, p[1] as f64, last);
            }
        } else {
            for i in (start..end).rev() {
                last = self.push(i as u32, points[i][0] as f64, points[i][1] as f64, last);
            }
        }
        if last != NIL && self.equals(last, self.nodes[last as usize].next) {
            let next = self.nodes[last as usize].next;
            self.remove(last);
            last = next;
        }
        (last != NIL).then_some(last)
    }

    #[inline]
    fn n(&self, i: u32) -> &Node {
        &self.nodes[i as usize]
    }

    fn equals(&self, a: u32, b: u32) -> bool {
        let (a, b) = (self.n(a), self.n(b));
        a.x == b.x && a.y == b.y
    }

    fn area(&self, p: u32, q: u32, r: u32) -> f64 {
        let (p, q, r) = (self.n(p), self.n(q), self.n(r));
        (q.y - p.y) * (r.x - q.x) - (q.x - p.x) * (r.y - q.y)
    }

    fn remove(&mut self, p: u32) {
        let Node {
            prev,
            next,
            prev_z,
            next_z,
            ..
        } = self.nodes[p as usize];
        self.nodes[next as usize].prev = prev;
        self.nodes[prev as usize].next = next;
        if prev_z != NIL {
            self.nodes[prev_z as usize].next_z = next_z;
        }
        if next_z != NIL {
            self.nodes[next_z as usize].prev_z = prev_z;
        }
    }

    /// Drop duplicate and collinear points between `start` and `end`.
    fn filter(&mut self, start: u32, end: u32) -> u32 {
        let mut end = if end == NIL { start } else { end };
        let mut p = start;
        loop {
            let mut again = false;
            let next = self.n(p).next;
            let prev = self.n(p).prev;
            if !self.n(p).steiner && (self.equals(p, next) || self.area(prev, p, next) == 0.0) {
                self.remove(p);
                p = prev;
                end = prev;
                if p == self.n(p).next {
                    break;
                }
                again = true;
            } else {
                p = next;
            }
            if !again && p == end {
                break;
            }
        }
        end
    }

    fn earcut_linked(&mut self, ear: u32, hash: Option<Hash>, pass: u8, out: &mut Vec<u32>) {
        let mut ear = ear;
        if pass == 0
            && let Some(h) = hash
        {
            self.index_curve(ear, h);
        }
        let mut stop = ear;
        while self.n(ear).prev != self.n(ear).next {
            let prev = self.n(ear).prev;
            let next = self.n(ear).next;
            let is_ear = match hash {
                Some(h) => self.is_ear_hashed(ear, h),
                None => self.is_ear(ear),
            };
            if is_ear {
                out.extend([self.n(prev).i, self.n(ear).i, self.n(next).i]);
                self.remove(ear);
                // Skipping the next vertex leads to fewer sliver triangles.
                ear = self.n(next).next;
                stop = ear;
                continue;
            }
            ear = next;
            if ear == stop {
                match pass {
                    0 => {
                        let e = self.filter(ear, NIL);
                        self.earcut_linked(e, hash, 1, out);
                    }
                    1 => {
                        let e = self.filter(ear, NIL);
                        let e = self.cure_local_intersections(e, out);
                        self.earcut_linked(e, hash, 2, out);
                    }
                    _ => self.split_earcut(ear, hash, out),
                }
                break;
            }
        }
    }

    fn is_ear(&self, ear: u32) -> bool {
        let (a, b, c) = (self.n(ear).prev, ear, self.n(ear).next);
        if self.area(a, b, c) >= 0.0 {
            return false;
        }
        let (pa, pb, pc) = (self.n(a), self.n(b), self.n(c));
        let (x0, x1) = (pa.x.min(pb.x).min(pc.x), pa.x.max(pb.x).max(pc.x));
        let (y0, y1) = (pa.y.min(pb.y).min(pc.y), pa.y.max(pb.y).max(pc.y));
        let mut p = self.n(c).next;
        while p != a {
            let pp = self.n(p);
            if pp.x >= x0
                && pp.x <= x1
                && pp.y >= y0
                && pp.y <= y1
                && in_triangle(pa.x, pa.y, pb.x, pb.y, pc.x, pc.y, pp.x, pp.y)
                && self.area(pp.prev, p, pp.next) >= 0.0
            {
                return false;
            }
            p = pp.next;
        }
        true
    }

    fn is_ear_hashed(&self, ear: u32, h: Hash) -> bool {
        let (a, b, c) = (self.n(ear).prev, ear, self.n(ear).next);
        if self.area(a, b, c) >= 0.0 {
            return false;
        }
        let (pa, pb, pc) = (self.n(a), self.n(b), self.n(c));
        let (x0, x1) = (pa.x.min(pb.x).min(pc.x), pa.x.max(pb.x).max(pc.x));
        let (y0, y1) = (pa.y.min(pb.y).min(pc.y), pa.y.max(pb.y).max(pc.y));
        let min_z = z_order(x0, y0, h);
        let max_z = z_order(x1, y1, h);
        let inside = |p: u32| {
            let pp = self.n(p);
            p != a
                && p != c
                && pp.x >= x0
                && pp.x <= x1
                && pp.y >= y0
                && pp.y <= y1
                && in_triangle(pa.x, pa.y, pb.x, pb.y, pc.x, pc.y, pp.x, pp.y)
                && self.area(pp.prev, p, pp.next) >= 0.0
        };
        let mut p = self.n(ear).prev_z;
        let mut n = self.n(ear).next_z;
        while p != NIL && self.n(p).z >= min_z && n != NIL && self.n(n).z <= max_z {
            if inside(p) || inside(n) {
                return false;
            }
            p = self.n(p).prev_z;
            n = self.n(n).next_z;
        }
        while p != NIL && self.n(p).z >= min_z {
            if inside(p) {
                return false;
            }
            p = self.n(p).prev_z;
        }
        while n != NIL && self.n(n).z <= max_z {
            if inside(n) {
                return false;
            }
            n = self.n(n).next_z;
        }
        true
    }

    fn cure_local_intersections(&mut self, start: u32, out: &mut Vec<u32>) -> u32 {
        let mut start = start;
        let mut p = start;
        loop {
            let a = self.n(p).prev;
            let b = self.n(self.n(p).next).next;
            if !self.equals(a, b)
                && self.intersects(a, p, self.n(p).next, b)
                && self.locally_inside(a, b)
                && self.locally_inside(b, a)
            {
                out.extend([self.n(a).i, self.n(p).i, self.n(b).i]);
                let next = self.n(p).next;
                self.remove(p);
                self.remove(next);
                p = b;
                start = b;
            }
            p = self.n(p).next;
            if p == start {
                break;
            }
        }
        self.filter(p, NIL)
    }

    fn split_earcut(&mut self, start: u32, hash: Option<Hash>, out: &mut Vec<u32>) {
        let mut a = start;
        loop {
            let mut b = self.n(self.n(a).next).next;
            while b != self.n(a).prev {
                if self.n(a).i != self.n(b).i && self.is_valid_diagonal(a, b) {
                    let mut c = self.split(a, b);
                    let na = self.n(a).next;
                    a = self.filter(a, na);
                    let nc = self.n(c).next;
                    c = self.filter(c, nc);
                    self.earcut_linked(a, hash, 0, out);
                    self.earcut_linked(c, hash, 0, out);
                    return;
                }
                b = self.n(b).next;
            }
            a = self.n(a).next;
            if a == start {
                break;
            }
        }
    }

    fn eliminate_holes(&mut self, points: &[[f32; 2]], holes: &[u32], outer: u32) -> u32 {
        self.queue.clear();
        for (k, &start) in holes.iter().enumerate() {
            let end = holes.get(k + 1).map_or(points.len(), |&e| e as usize);
            if end <= start as usize {
                continue;
            }
            if let Some(list) = self.ring(points, start as usize, end, false) {
                if list == self.n(list).next {
                    self.nodes[list as usize].steiner = true;
                }
                let left = self.leftmost(list);
                self.queue.push(left);
            }
        }
        let mut queue = std::mem::take(&mut self.queue);
        queue.sort_by(|&a, &b| {
            let (pa, pb) = (self.n(a), self.n(b));
            pa.x.total_cmp(&pb.x).then(pa.y.total_cmp(&pb.y))
        });
        let mut outer = outer;
        for &hole in &queue {
            outer = self.eliminate_hole(hole, outer);
        }
        self.queue = queue;
        outer
    }

    fn eliminate_hole(&mut self, hole: u32, outer: u32) -> u32 {
        let Some(bridge) = self.find_hole_bridge(hole, outer) else {
            return outer;
        };
        let reverse = self.split(bridge, hole);
        let nr = self.n(reverse).next;
        self.filter(reverse, nr);
        let nb = self.n(bridge).next;
        self.filter(bridge, nb)
    }

    fn find_hole_bridge(&self, hole: u32, outer: u32) -> Option<u32> {
        let mut p = outer;
        let (hx, hy) = (self.n(hole).x, self.n(hole).y);
        let mut qx = f64::NEG_INFINITY;
        let mut m = NIL;
        // The segment left of the hole's leftmost point, nearest it, that a
        // ray to the left meets.
        loop {
            let pp = self.n(p);
            let np = self.n(pp.next);
            if hy <= pp.y && hy >= np.y && np.y != pp.y {
                let x = pp.x + (hy - pp.y) * (np.x - pp.x) / (np.y - pp.y);
                if x <= hx && x > qx {
                    qx = x;
                    m = if pp.x < np.x { p } else { pp.next };
                    if x == hx {
                        return Some(m);
                    }
                }
            }
            p = pp.next;
            if p == outer {
                break;
            }
        }
        if m == NIL {
            return None;
        }
        // Of the points inside the triangle of the hole point, the hit and
        // the segment end, the one at the least angle to the ray.
        let stop = m;
        let (mx, my) = (self.n(m).x, self.n(m).y);
        let mut tan_min = f64::INFINITY;
        p = m;
        loop {
            let pp = self.n(p);
            if hx >= pp.x
                && pp.x >= mx
                && hx != pp.x
                && in_triangle(
                    if hy < my { hx } else { qx },
                    hy,
                    mx,
                    my,
                    if hy < my { qx } else { hx },
                    hy,
                    pp.x,
                    pp.y,
                )
            {
                let tan = (hy - pp.y).abs() / (hx - pp.x);
                if self.locally_inside(p, hole)
                    && (tan < tan_min
                        || (tan == tan_min
                            && (pp.x > self.n(m).x
                                || (pp.x == self.n(m).x && self.sector_contains_sector(m, p)))))
                {
                    m = p;
                    tan_min = tan;
                }
            }
            p = pp.next;
            if p == stop {
                break;
            }
        }
        Some(m)
    }

    fn sector_contains_sector(&self, m: u32, p: u32) -> bool {
        let (mm, pp) = (self.n(m), self.n(p));
        self.area(mm.prev, m, pp.prev) < 0.0 && self.area(pp.next, m, mm.next) < 0.0
    }

    fn index_curve(&mut self, start: u32, h: Hash) {
        let mut p = start;
        loop {
            if self.n(p).z == 0 {
                let z = z_order(self.n(p).x, self.n(p).y, h);
                self.nodes[p as usize].z = z;
            }
            let prev = self.n(p).prev;
            let next = self.n(p).next;
            self.nodes[p as usize].prev_z = prev;
            self.nodes[p as usize].next_z = next;
            p = next;
            if p == start {
                break;
            }
        }
        let prev = self.n(p).prev_z;
        self.nodes[prev as usize].next_z = NIL;
        self.nodes[p as usize].prev_z = NIL;
        self.sort_linked(p);
    }

    /// Sort the z-order list by merge sort, bottom up.
    fn sort_linked(&mut self, list: u32) {
        let mut list = list;
        let mut size = 1;
        loop {
            let mut p = list;
            list = NIL;
            let mut tail = NIL;
            let mut merges = 0;
            while p != NIL {
                merges += 1;
                let mut q = p;
                let mut p_size = 0;
                for _ in 0..size {
                    p_size += 1;
                    q = self.n(q).next_z;
                    if q == NIL {
                        break;
                    }
                }
                let mut q_size = size;
                while p_size > 0 || (q_size > 0 && q != NIL) {
                    let e;
                    if p_size != 0 && (q_size == 0 || q == NIL || self.n(p).z <= self.n(q).z) {
                        e = p;
                        p = self.n(p).next_z;
                        p_size -= 1;
                    } else {
                        e = q;
                        q = self.n(q).next_z;
                        q_size -= 1;
                    }
                    if tail != NIL {
                        self.nodes[tail as usize].next_z = e;
                    } else {
                        list = e;
                    }
                    self.nodes[e as usize].prev_z = tail;
                    tail = e;
                }
                p = q;
            }
            self.nodes[tail as usize].next_z = NIL;
            size *= 2;
            if merges <= 1 {
                break;
            }
        }
    }

    fn leftmost(&self, start: u32) -> u32 {
        let mut p = start;
        let mut left = start;
        loop {
            let (pp, l) = (self.n(p), self.n(left));
            if pp.x < l.x || (pp.x == l.x && pp.y < l.y) {
                left = p;
            }
            p = pp.next;
            if p == start {
                break;
            }
        }
        left
    }

    fn is_valid_diagonal(&self, a: u32, b: u32) -> bool {
        let (na, nb) = (self.n(a), self.n(b));
        self.n(na.next).i != nb.i
            && self.n(na.prev).i != nb.i
            && !self.intersects_polygon(a, b)
            && ((self.locally_inside(a, b)
                && self.locally_inside(b, a)
                && self.middle_inside(a, b)
                && (self.area(na.prev, a, nb.prev) != 0.0 || self.area(a, nb.prev, b) != 0.0))
                || (self.equals(a, b)
                    && self.area(na.prev, a, na.next) > 0.0
                    && self.area(nb.prev, b, nb.next) > 0.0))
    }

    fn intersects(&self, p1: u32, q1: u32, p2: u32, q2: u32) -> bool {
        let o1 = sign(self.area(p1, q1, p2));
        let o2 = sign(self.area(p1, q1, q2));
        let o3 = sign(self.area(p2, q2, p1));
        let o4 = sign(self.area(p2, q2, q1));
        (o1 != o2 && o3 != o4)
            || (o1 == 0 && self.on_segment(p1, p2, q1))
            || (o2 == 0 && self.on_segment(p1, q2, q1))
            || (o3 == 0 && self.on_segment(p2, p1, q2))
            || (o4 == 0 && self.on_segment(p2, q1, q2))
    }

    fn on_segment(&self, p: u32, q: u32, r: u32) -> bool {
        let (p, q, r) = (self.n(p), self.n(q), self.n(r));
        q.x <= p.x.max(r.x) && q.x >= p.x.min(r.x) && q.y <= p.y.max(r.y) && q.y >= p.y.min(r.y)
    }

    fn intersects_polygon(&self, a: u32, b: u32) -> bool {
        let mut p = a;
        loop {
            let pp = self.n(p);
            let (ai, bi) = (self.n(a).i, self.n(b).i);
            if pp.i != ai
                && self.n(pp.next).i != ai
                && pp.i != bi
                && self.n(pp.next).i != bi
                && self.intersects(p, pp.next, a, b)
            {
                return true;
            }
            p = pp.next;
            if p == a {
                break;
            }
        }
        false
    }

    fn locally_inside(&self, a: u32, b: u32) -> bool {
        let na = self.n(a);
        if self.area(na.prev, a, na.next) < 0.0 {
            self.area(a, b, na.next) >= 0.0 && self.area(a, na.prev, b) >= 0.0
        } else {
            self.area(a, b, na.prev) < 0.0 || self.area(a, na.next, b) < 0.0
        }
    }

    fn middle_inside(&self, a: u32, b: u32) -> bool {
        let (mut p, mut inside) = (a, false);
        let (px, py) = (
            (self.n(a).x + self.n(b).x) / 2.0,
            (self.n(a).y + self.n(b).y) / 2.0,
        );
        loop {
            let pp = self.n(p);
            let np = self.n(pp.next);
            if ((pp.y > py) != (np.y > py))
                && np.y != pp.y
                && px < (np.x - pp.x) * (py - pp.y) / (np.y - pp.y) + pp.x
            {
                inside = !inside;
            }
            p = pp.next;
            if p == a {
                break;
            }
        }
        inside
    }

    /// Link `a` and `b` by two bridge edges, splitting the ring in two;
    /// returns the copy of `b` that starts the second ring.
    fn split(&mut self, a: u32, b: u32) -> u32 {
        let (na, nb) = (self.nodes[a as usize], self.nodes[b as usize]);
        let a2 = self.nodes.len() as u32;
        let b2 = a2 + 1;
        let an = na.next;
        let bp = nb.prev;
        self.nodes.push(Node {
            prev: NIL,
            next: NIL,
            prev_z: NIL,
            next_z: NIL,
            z: 0,
            steiner: false,
            ..na
        });
        self.nodes.push(Node {
            prev: NIL,
            next: NIL,
            prev_z: NIL,
            next_z: NIL,
            z: 0,
            steiner: false,
            ..nb
        });
        self.nodes[a as usize].next = b;
        self.nodes[b as usize].prev = a;
        self.nodes[a2 as usize].next = an;
        self.nodes[an as usize].prev = a2;
        self.nodes[b2 as usize].next = a2;
        self.nodes[a2 as usize].prev = b2;
        self.nodes[bp as usize].next = b2;
        self.nodes[b2 as usize].prev = bp;
        b2
    }
}

fn sign(v: f64) -> i8 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

#[allow(clippy::too_many_arguments)]
fn in_triangle(ax: f64, ay: f64, bx: f64, by: f64, cx: f64, cy: f64, px: f64, py: f64) -> bool {
    (cx - px) * (ay - py) >= (ax - px) * (cy - py)
        && (ax - px) * (by - py) >= (bx - px) * (ay - py)
        && (bx - px) * (cy - py) >= (cx - px) * (by - py)
}

/// The z-order of a point: the bits of its cell coordinates interleaved.
fn z_order(x: f64, y: f64, h: Hash) -> u32 {
    let spread = |v: f64| {
        let mut v = (v as u32) & 0xffff;
        v = (v | (v << 8)) & 0x00ff_00ff;
        v = (v | (v << 4)) & 0x0f0f_0f0f;
        v = (v | (v << 2)) & 0x3333_3333;
        (v | (v << 1)) & 0x5555_5555
    };
    spread((x - h.min_x) * h.inv) | (spread((y - h.min_y) * h.inv) << 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(points: &[[f32; 2]], tris: &[u32]) -> f64 {
        tris.chunks(3)
            .map(|t| {
                let (a, b, c) = (
                    points[t[0] as usize],
                    points[t[1] as usize],
                    points[t[2] as usize],
                );
                (((b[0] - a[0]) as f64 * (c[1] - a[1]) as f64)
                    - ((c[0] - a[0]) as f64 * (b[1] - a[1]) as f64))
                    .abs()
                    / 2.0
            })
            .sum()
    }

    #[test]
    fn squares_with_holes_cover_their_area() {
        let points = [
            [0.0, 0.0],
            [10.0, 0.0],
            [10.0, 10.0],
            [0.0, 10.0],
            [2.0, 2.0],
            [2.0, 4.0],
            [4.0, 4.0],
            [4.0, 2.0],
            [6.0, 6.0],
            [8.0, 6.0],
            [8.0, 8.0],
            [6.0, 8.0],
        ];
        let mut e = Earcut::default();
        let mut out = Vec::new();
        e.triangulate(&points, &[4, 8], 0, &mut out);
        assert!((area(&points, &out) - 92.0).abs() < 1e-9);
        let mut based = Vec::new();
        e.triangulate(&points[..4], &[], 10, &mut based);
        assert_eq!(based.len(), 6);
        assert!(based.iter().all(|&i| (10..14).contains(&i)));
    }

    #[test]
    fn large_and_concave_rings_triangulate() {
        // A star of 400 points: concave everywhere, hashed.
        let points: Vec<[f32; 2]> = (0..400)
            .map(|i| {
                let a = i as f32 / 400.0 * std::f32::consts::TAU;
                let r = if i % 2 == 0 { 100.0 } else { 60.0 };
                [r * a.cos(), r * a.sin()]
            })
            .collect();
        let mut out = Vec::new();
        Earcut::default().triangulate(&points, &[], 0, &mut out);
        assert_eq!(out.len(), (400 - 2) * 3);
        let shoelace: f64 = (0..400)
            .map(|i| {
                let (a, b) = (points[i], points[(i + 1) % 400]);
                a[0] as f64 * b[1] as f64 - b[0] as f64 * a[1] as f64
            })
            .sum::<f64>()
            .abs()
            / 2.0;
        assert!((area(&points, &out) - shoelace).abs() / shoelace < 1e-6);
    }

    #[test]
    fn degenerate_rings_do_not_panic() {
        let mut e = Earcut::default();
        let mut out = Vec::new();
        e.triangulate(&[[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]], &[], 0, &mut out);
        e.triangulate(
            &[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            &[],
            0,
            &mut out,
        );
        // A bow tie.
        e.triangulate(
            &[[0.0, 0.0], [2.0, 2.0], [2.0, 0.0], [0.0, 2.0]],
            &[],
            0,
            &mut out,
        );
        e.triangulate(&[[0.0, 0.0]], &[], 0, &mut out);
    }
}
