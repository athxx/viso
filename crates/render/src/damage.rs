//! Damage: the surface pixels a frame changes (§30.3).
//!
//! Every frame the renderer lists what the surface pass draws, one entry per
//! paint-order slot that lands on the surface: a 64-bit key of everything that
//! decides its pixels (its retained store entry, the context it was lowered
//! under, the content of the offscreen pass or backdrop capture it samples,
//! how far its transform root moved) and the rect it can touch, clipped as it
//! is drawn. Damage is the difference between this frame's list and the last
//! presented one: [`diff`] matches the two in order — an unchanged prefix and
//! suffix, then a windowed resync, so an inserted row or a removed one damages
//! that row rather than everything after it — and every unmatched entry
//! contributes its rect, old and new.
//!
//! [`Region::merge`] folds the rects into at most [`MAX_RECTS`], and gives up
//! on a frame whose damage covers most of the surface ([`FULL_FRAME_SHARE`]):
//! redrawing it whole is cheaper than scissoring it. A frame with no damage is
//! not submitted at all.
//!
//! The drawable a frame renders into may hold an older frame than the last one
//! presented (a swapchain of several images). [`History`] keeps the regions of
//! the last few frames, so a drawable `age` frames old is brought up to date
//! by redrawing the union of the last `age` regions.

use crate::primitive::Rect;

/// The most rects a frame's damage is merged into.
pub const MAX_RECTS: usize = 4;

/// The share of the surface above which a frame is redrawn whole.
pub const FULL_FRAME_SHARE: f32 = 0.6;

/// How far [`diff`] looks ahead, in entries, to resynchronize two lists.
const WINDOW: usize = 32;

/// How many frames of damage [`History`] keeps: the oldest drawable a frame
/// can be brought up to date from.
pub const HISTORY: usize = 4;

/// What one entry of a frame's surface pass draws: the key of everything that
/// decides its pixels, and the rect it can touch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drawn {
    pub key: u64,
    pub rect: Rect,
}

impl Drawn {
    fn same(&self, other: &Drawn) -> bool {
        self.key == other.key && rect_bits(self.rect) == rect_bits(other.rect)
    }
}

fn rect_bits(r: Rect) -> [u32; 4] {
    [r.x.to_bits(), r.y.to_bits(), r.w.to_bits(), r.h.to_bits()]
}

/// The rects `old` and `new` differ by, each handed to `out`: the rect of
/// every entry one list draws and the other does not draw at the same place in
/// the sequence.
pub fn diff(old: &[Drawn], new: &[Drawn], mut out: impl FnMut(Rect)) {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a.same(b)).count();
    let (old, new) = (&old[prefix..], &new[prefix..]);
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take_while(|(a, b)| a.same(b))
        .count();
    let (old, new) = (&old[..old.len() - suffix], &new[..new.len() - suffix]);
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        if old[i].same(&new[j]) {
            i += 1;
            j += 1;
            continue;
        }
        let (di, dj) = resync(&old[i..], &new[j..]).unwrap_or((1, 1));
        old[i..i + di].iter().for_each(|d| out(d.rect));
        new[j..j + dj].iter().for_each(|d| out(d.rect));
        i += di;
        j += dj;
    }
    old[i..].iter().for_each(|d| out(d.rect));
    new[j..].iter().for_each(|d| out(d.rect));
}

/// The fewest entries to skip in `old` and `new` (both within [`WINDOW`]) so
/// their heads match again, preferring the smaller total skip.
fn resync(old: &[Drawn], new: &[Drawn]) -> Option<(usize, usize)> {
    for total in 1..=2 * WINDOW {
        for di in total.saturating_sub(WINDOW)..=total.min(WINDOW) {
            let dj = total - di;
            if let (Some(a), Some(b)) = (old.get(di), new.get(dj))
                && a.same(b)
            {
                return Some((di, dj));
            }
        }
    }
    None
}

/// The pixels a frame redraws: none, all of them, or a few rects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Region {
    Empty,
    Full,
    Rects(Rects),
}

/// At most [`MAX_RECTS`] disjoint rects, held inline.
#[derive(Clone, Copy)]
pub struct Rects {
    rects: [Rect; MAX_RECTS],
    len: u8,
}

impl Rects {
    /// `rects`, which must be at most [`MAX_RECTS`].
    pub fn from_slice(rects: &[Rect]) -> Rects {
        assert!(rects.len() <= MAX_RECTS, "more than {MAX_RECTS} rects");
        let mut out = Rects {
            rects: [Rect::ZERO; MAX_RECTS],
            len: rects.len() as u8,
        };
        out.rects[..rects.len()].copy_from_slice(rects);
        out
    }
}

impl std::ops::Deref for Rects {
    type Target = [Rect];

    fn deref(&self) -> &[Rect] {
        &self.rects[..usize::from(self.len)]
    }
}

impl PartialEq for Rects {
    fn eq(&self, other: &Rects) -> bool {
        **self == **other
    }
}

impl std::fmt::Debug for Rects {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl Region {
    /// `rects`, each grown by `margin` and cut to `surface`, merged into at
    /// most [`MAX_RECTS`]; [`Region::Full`] when they cover more than
    /// [`FULL_FRAME_SHARE`] of `surface`.
    pub fn merge(rects: &[Rect], margin: f32, surface: Rect) -> Region {
        let mut merger = Merger::new(margin, surface);
        rects.iter().for_each(|&r| merger.add(r));
        merger.finish()
    }

    /// Whether it redraws nothing.
    pub fn is_empty(&self) -> bool {
        matches!(self, Region::Empty)
    }

    /// `self` and `other` together, still within [`MAX_RECTS`].
    pub fn union(&self, other: &Region, surface: Rect) -> Region {
        match (self, other) {
            (Region::Full, _) | (_, Region::Full) => Region::Full,
            (Region::Empty, r) | (r, Region::Empty) => *r,
            (Region::Rects(a), Region::Rects(b)) => {
                let mut merger = Merger::new(0.0, surface);
                a.iter().chain(b.iter()).for_each(|&r| merger.add(r));
                merger.finish()
            }
        }
    }

    /// Its pixel area within `surface`.
    pub fn area(&self, surface: Rect) -> f32 {
        match self {
            Region::Empty => 0.0,
            Region::Full => surface.w * surface.h,
            Region::Rects(rects) => rects.iter().map(|r| r.w * r.h).sum(),
        }
    }
}

/// Folds rects one at a time into at most [`MAX_RECTS`] disjoint ones (see
/// [`Region::merge`]), on the stack.
#[derive(Debug, Clone, Copy)]
pub struct Merger {
    rects: [Rect; MAX_RECTS + 1],
    len: usize,
    margin: f32,
    surface: Rect,
}

impl Merger {
    pub fn new(margin: f32, surface: Rect) -> Merger {
        Merger {
            rects: [Rect::ZERO; MAX_RECTS + 1],
            len: 0,
            margin,
            surface,
        }
    }

    /// Adds `r`, grown by the margin and cut to the surface.
    pub fn add(&mut self, r: Rect) {
        let r = r.inflate(self.margin).intersect(self.surface);
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        self.absorb(r);
        if self.len > MAX_RECTS {
            self.fold_cheapest();
        }
    }

    pub fn finish(self) -> Region {
        if self.len == 0 {
            return Region::Empty;
        }
        let rects = &self.rects[..self.len];
        let area: f32 = rects.iter().map(|r| r.w * r.h).sum();
        if area > FULL_FRAME_SHARE * self.surface.w * self.surface.h {
            return Region::Full;
        }
        Region::Rects(Rects::from_slice(rects))
    }

    /// Adds `r`, swallowing every rect it overlaps (and growing to their
    /// union) so the set stays disjoint.
    fn absorb(&mut self, mut r: Rect) {
        let mut i = 0;
        while i < self.len {
            if self.rects[i].intersects(r) {
                r = r.union(self.take(i));
                i = 0;
            } else {
                i += 1;
            }
        }
        self.rects[self.len] = r;
        self.len += 1;
    }

    /// Removes rect `i`, moving the last into its place.
    fn take(&mut self, i: usize) -> Rect {
        let r = self.rects[i];
        self.len -= 1;
        self.rects[i] = self.rects[self.len];
        r
    }

    /// Merges the two rects whose union adds the least area.
    fn fold_cheapest(&mut self) {
        let rects = &self.rects[..self.len];
        let mut best = (0, 1, f32::INFINITY);
        for a in 0..rects.len() {
            for b in a + 1..rects.len() {
                let u = rects[a].union(rects[b]);
                let cost = u.w * u.h - rects[a].w * rects[a].h - rects[b].w * rects[b].h;
                if cost < best.2 {
                    best = (a, b, cost);
                }
            }
        }
        let (a, b, _) = best;
        let other = self.take(b);
        let joined = self.take(a).union(other);
        self.absorb(joined);
    }
}

/// The damage regions of the last [`HISTORY`] presented frames, newest first.
#[derive(Debug)]
pub struct History {
    regions: [Region; HISTORY],
    len: usize,
}

impl Default for History {
    fn default() -> History {
        History {
            regions: [Region::Empty; HISTORY],
            len: 0,
        }
    }
}

impl History {
    /// Forget every frame: the next drawable is redrawn whole.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Records the region of a frame just presented.
    pub fn push(&mut self, region: Region) {
        self.regions.copy_within(..HISTORY - 1, 1);
        self.regions[0] = region;
        self.len = (self.len + 1).min(HISTORY);
    }

    /// What a drawable last drawn `age` frames ago must redraw to show
    /// `current`: the union of `current` and the `age - 1` frames presented
    /// since. A drawable of unknown age (`0`) or older than the history is
    /// redrawn whole.
    pub fn redraw(&self, current: &Region, age: u32, surface: Rect) -> Region {
        let since = (age as usize).wrapping_sub(1);
        if age == 0 || since > self.len {
            return Region::Full;
        }
        self.regions[..since]
            .iter()
            .fold(*current, |acc, r| acc.union(r, surface))
    }
}

/// A 64-bit hash, fast on the small fixed-size records it keys; not for
/// untrusted input.
#[derive(Debug, Clone, Copy)]
pub struct Key(u64);

const K: u64 = 0x9e37_79b9_7f4a_7c15;

impl Key {
    pub const fn new(seed: u64) -> Key {
        Key(seed ^ K)
    }

    #[inline]
    pub fn word(self, v: u64) -> Key {
        Key((self.0.rotate_left(5) ^ v).wrapping_mul(K))
    }

    #[inline]
    pub fn f32s(mut self, vs: &[f32]) -> Key {
        for v in vs {
            self = self.word(u64::from(v.to_bits()));
        }
        self
    }

    #[inline]
    pub fn rect(self, r: Rect) -> Key {
        self.f32s(&[r.x, r.y, r.w, r.h])
    }

    pub fn bytes(mut self, bytes: &[u8]) -> Key {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for c in chunks {
            self = self.word(u64::from_le_bytes(*c));
        }
        let mut tail = [0u8; 8];
        tail[..rest.len()].copy_from_slice(rest);
        self.word(u64::from_le_bytes(tail) ^ ((bytes.len() as u64) << 56))
    }

    #[inline]
    pub fn finish(self) -> u64 {
        let h = self.0;
        (h ^ (h >> 31)).wrapping_mul(0xbf58_476d_1ce4_e5b9) ^ (h >> 29)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    fn row(i: u64) -> Drawn {
        Drawn {
            key: i,
            rect: rect(0.0, i as f32 * 10.0, 100.0, 10.0),
        }
    }

    fn rows(keys: &[u64]) -> Vec<Drawn> {
        keys.iter().map(|&k| row(k)).collect()
    }

    fn damage(old: &[Drawn], new: &[Drawn]) -> Vec<Rect> {
        let mut out = Vec::new();
        diff(old, new, |r| out.push(r));
        out
    }

    #[test]
    fn identical_lists_damage_nothing() {
        let a = rows(&[1, 2, 3, 4]);
        assert!(damage(&a, &a).is_empty());
    }

    #[test]
    fn a_changed_entry_damages_its_old_and_new_rect() {
        let a = rows(&[1, 2, 3, 4]);
        let mut b = a.clone();
        b[2].key = 99;
        b[2].rect = rect(5.0, 5.0, 1.0, 1.0);
        assert_eq!(damage(&a, &b), [a[2].rect, b[2].rect]);
    }

    #[test]
    fn an_inserted_row_damages_that_row_only() {
        let a: Vec<Drawn> = (0..200).map(row).collect();
        let mut b = a.clone();
        b.insert(50, row(1000));
        assert_eq!(damage(&a, &b), [row(1000).rect]);
        assert_eq!(damage(&b, &a), [row(1000).rect]);
    }

    #[test]
    fn two_separate_changes_damage_two_places() {
        let a: Vec<Drawn> = (0..300).map(row).collect();
        let mut b = a.clone();
        b[20].key = 7000;
        b[250].key = 8000;
        assert_eq!(
            damage(&a, &b),
            [a[20].rect, b[20].rect, a[250].rect, b[250].rect]
        );
    }

    #[test]
    fn swapping_two_overlapping_draws_damages_them() {
        let a = vec![
            Drawn {
                key: 1,
                rect: rect(0.0, 0.0, 10.0, 10.0),
            },
            Drawn {
                key: 2,
                rect: rect(5.0, 5.0, 10.0, 10.0),
            },
        ];
        let b = vec![a[1], a[0]];
        assert!(!damage(&a, &b).is_empty(), "z-order is part of the picture");
    }

    #[test]
    fn a_long_replacement_past_the_window_still_resyncs_at_the_tail() {
        let a: Vec<Drawn> = (0..400).map(row).collect();
        let mut b = a.clone();
        for d in &mut b[100..200] {
            d.key += 10_000;
        }
        let out = damage(&a, &b);
        assert_eq!(out.len(), 200, "the hundred changed rows, old and new");
    }

    #[test]
    fn merging_keeps_at_most_four_disjoint_rects() {
        let surface = rect(0.0, 0.0, 1000.0, 1000.0);
        let rects: Vec<Rect> = (0..10)
            .map(|i| rect(i as f32 * 90.0, (i % 3) as f32 * 300.0, 10.0, 10.0))
            .collect();
        let Region::Rects(merged) = Region::merge(&rects, 0.0, surface) else {
            panic!("small damage stays rects");
        };
        assert!(merged.len() <= MAX_RECTS);
        for r in &rects {
            assert!(merged.iter().any(|m| m.union(*r) == *m), "{r:?} is covered");
        }
        for (i, a) in merged.iter().enumerate() {
            for b in &merged[i + 1..] {
                assert!(!a.intersects(*b), "{a:?} and {b:?} overlap");
            }
        }
    }

    #[test]
    fn most_of_the_surface_is_a_full_frame() {
        let surface = rect(0.0, 0.0, 100.0, 100.0);
        assert_eq!(
            Region::merge(&[rect(0.0, 0.0, 90.0, 90.0)], 0.0, surface),
            Region::Full
        );
        assert_eq!(Region::merge(&[], 0.0, surface), Region::Empty);
        assert_eq!(
            Region::merge(&[rect(200.0, 0.0, 9.0, 9.0)], 0.0, surface),
            Region::Empty,
            "off-surface damage is none"
        );
    }

    #[test]
    fn an_older_drawable_redraws_what_changed_since() {
        let surface = rect(0.0, 0.0, 100.0, 100.0);
        let one = Region::Rects(Rects::from_slice(&[rect(0.0, 0.0, 5.0, 5.0)]));
        let two = Region::Rects(Rects::from_slice(&[rect(50.0, 50.0, 5.0, 5.0)]));
        let now = Region::Rects(Rects::from_slice(&[rect(20.0, 20.0, 5.0, 5.0)]));
        let mut h = History::default();
        h.push(one);
        h.push(two);
        assert_eq!(h.redraw(&now, 1, surface), now);
        let Region::Rects(r) = h.redraw(&now, 3, surface) else {
            panic!()
        };
        assert_eq!(r.len(), 3);
        assert_eq!(h.redraw(&now, 0, surface), Region::Full);
        assert_eq!(h.redraw(&now, 9, surface), Region::Full);
    }
}
