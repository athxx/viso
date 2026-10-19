//! Splicing a repaint into the stream the last paint emitted (§9.7).
//!
//! A paint records, for every node it painted, where its primitives landed:
//! its own drawables and the end of its subtree. A later frame whose paint,
//! layout or structure changed in places re-emits only what changed and
//! splices it into that stream. A node whose look or box changed re-emits its
//! own drawables. A node whose children changed diffs them against the runs
//! the stream holds: a child still in place keeps its run, one gone leaves a
//! gap, one new or moved out of order paints fresh. Every other node's
//! primitives stay where they were, moved by what changed before them.
//!
//! The changed nodes and their ancestors are visited in the order the last
//! paint emitted them, so every edit lands behind the ones before it and the
//! walk knows, at each node, how far the stream before it moved; no node it
//! does not visit is read. A frame whose edits all keep their length and
//! kinds is a patch.
//!
//! What would renumber the transform table or the deferred overlays — a
//! transform root or an overlay entering or leaving — is not spliced: the
//! store paints whole then, as it does when a transform root, a fade or an
//! overlay starts or stops.

use viso_render::{LayerClip, Primitive, Splice, TransformSlot};

use super::{PaintSpans, Space, Span, paint_own, paint_subtree};
use crate::component::NodeStore;
use crate::layout::Vec2;
use crate::node::{Logged, NodeId, NodeIndex};
use crate::transform::TransformTable;

/// Where one node's primitives sit in the stream.
#[derive(Debug, Clone, Copy, Default)]
struct Record {
    own: u32,
    /// How many own drawables, with the node's [`Span`] flags in the top byte.
    len: u32,
    end: u32,
    /// The transform root its own drawables are painted in, or `u32::MAX`.
    slot: u32,
    parent: u32,
    depth: u32,
    /// The node's generation, so a reused slot reads as unpainted.
    generation: u32,
    /// The stream this is a record of.
    epoch: u32,
}

const LEN: u32 = 0x00ff_ffff;

impl Record {
    #[inline]
    fn len(&self) -> u32 {
        self.len & LEN
    }

    #[inline]
    fn has(&self, flag: u8) -> bool {
        (self.len >> 24) as u8 & flag != 0
    }

    /// Its first primitive: a transform root's `Transform` and a fade's
    /// `Layer` open its span.
    #[inline]
    fn start(&self) -> u32 {
        self.own - u32::from(self.has(Span::ROOT)) - u32::from(self.has(Span::FADED))
    }

    /// The run its children painted into.
    #[inline]
    fn children(&self) -> (u32, u32) {
        let scroll = 2 * u32::from(self.has(Span::SCROLL));
        let close = scroll + u32::from(self.has(Span::FADED)) + u32::from(self.has(Span::ROOT));
        (self.own + self.len() + scroll, self.end - close)
    }

    #[inline]
    fn slot(&self) -> Option<u32> {
        (self.slot != u32::MAX).then_some(self.slot)
    }
}

/// Where each node's primitives sit in the stream the last paint emitted, so
/// a later frame can splice its changes into it ([`splice_walk`]).
#[derive(Debug, Default)]
pub(crate) struct PaintIndex {
    stream: Stream,
    scratch: Scratch,
}

/// The records of the stream.
#[derive(Debug, Default)]
struct Stream {
    /// By node slot.
    records: Vec<Record>,
    /// The stream the records of this one are of; an older record's node is
    /// not in it.
    epoch: u32,
    /// The stream a record written now is of, and how far the primitives
    /// being emitted are from where they land in it.
    writing: u32,
    base: i64,
    /// The paint root and how many primitives its stream holds.
    root: Option<NodeId>,
    len: usize,
    /// The space of the nodes outside every transform root.
    top: Space,
}

/// The splice walk's working buffers, kept so a frame allocates nothing.
#[derive(Debug, Default)]
struct Scratch {
    /// By node slot, and the slots marked.
    marks: Vec<u8>,
    marked: Vec<u32>,
    chain: Vec<NodeIndex>,
    items: Vec<Item>,
    stack: Vec<Open>,
    edits: Vec<Edit>,
    /// What the edits insert, back to back.
    fresh: Vec<Primitive>,
    /// The overlays a fresh subtree defers; always empty after a splice.
    deferred: Vec<(NodeId, Space)>,
    /// The records of the visited nodes, written once the walk is done.
    updates: Vec<(u32, Record)>,
    /// The overlays the last paint deferred, with their records then.
    overlays: Vec<(NodeId, Option<Record>)>,
    /// The children the arena logs relinks to, by parent and child.
    touched: Vec<Touched>,
    /// The steps of the children diffs under way, innermost last.
    steps: Vec<Step>,
    passed: Vec<u32>,
}

/// A child its parent's children changed by, with its record in the last
/// stream.
#[derive(Debug, Clone, Copy)]
struct Touched {
    parent: u32,
    child: NodeId,
    old: Option<Record>,
    /// Where among its parent's old children it paints, once found, and its
    /// place among the touched siblings painting there.
    anchor: Option<(u32, u32)>,
    /// The anchor of the sibling after it.
    after: (u32, u32),
    /// It paints where it did: its run stays.
    kept: bool,
}

/// A step of a children diff that knows which children changed: `child`
/// paints fresh at `at`, or with none the old run `at..end` is gone.
#[derive(Debug, Clone, Copy)]
struct Step {
    at: u32,
    end: u32,
    rank: u32,
    child: Option<NodeId>,
}

/// Its own drawables changed.
const OWN: u8 = 1;
/// Its children changed.
const RELINKED: u8 = 2;
/// Found in the tree.
const PLACED: u8 = 4;
/// Not in the tree.
const ORPHAN: u8 = 8;
/// An ancestor's children changed, or an ancestor paints nothing.
const COVERED: u8 = 16;
/// Visited where the last paint emitted it.
const KEPT: u8 = 32;

/// A node the walk visits, by where the last paint emitted it.
#[derive(Debug, Clone, Copy)]
struct Item {
    start: u32,
    end: u32,
    depth: u32,
    node: NodeId,
    bits: u8,
    record: Record,
}

impl Item {
    fn within(&self, start: u32, end: u32, depth: u32) -> bool {
        start <= self.start && self.end <= end && depth < self.depth
    }
}

/// A visited node whose subtree the walk is in.
#[derive(Debug, Clone, Copy)]
struct Open {
    node: NodeId,
    record: Record,
    depth: u32,
    /// Where its own drawables land, and how many.
    own: u32,
    len: u32,
    diff: Option<Diff>,
}

/// A children diff under way: the next child to place, and how far into the
/// old run of children it has come.
#[derive(Debug, Clone, Copy)]
struct Diff {
    next: Option<NodeId>,
    cursor: u32,
    end: u32,
    /// Where a fresh child paints.
    inner: Space,
    /// The steps, when the arena knows which children changed.
    steps: Option<Steps>,
}

/// A children diff's steps and touched children in the scratch.
#[derive(Debug, Clone, Copy)]
struct Steps {
    parent: u32,
    touched: (u32, u32),
    base: u32,
    next: u32,
    end: u32,
}

/// `removed` primitives from `at` in the old stream give way to the
/// `inserted` from `from` in the fresh buffer.
#[derive(Debug, Clone, Copy)]
struct Edit {
    at: u32,
    removed: u32,
    from: u32,
    inserted: u32,
}

/// What a child diff made of the child it was asked about.
enum Fate {
    Kept,
    Gone,
}

/// How a splice changed the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spliced {
    /// Every changed primitive kept its place and kind: these many.
    Patched(u32),
    /// Runs of it were replaced: these many primitives inserted.
    Spliced(u32),
}

impl PaintIndex {
    /// Room for `slots` node slots, so recording allocates nothing.
    pub(crate) fn reserve_slots(&mut self, slots: usize) {
        let records = &mut self.stream.records;
        records.reserve(slots.saturating_sub(records.len()));
        let marks = &mut self.scratch.marks;
        marks.reserve(slots.saturating_sub(marks.len()));
    }

    /// Forget every record: the next paint is whole.
    pub(crate) fn clear(&mut self) {
        self.stream.root = None;
    }
}

/// [`paint_with_transforms`](super::paint_with_transforms), recording where
/// each node's primitives land in `index` for a later [`splice_walk`].
pub(crate) fn paint_indexed(
    store: &NodeStore,
    root: NodeId,
    out: &mut Vec<Primitive>,
    table: &mut TransformTable,
    index: &mut PaintIndex,
) {
    let stream = &mut index.stream;
    let start = out.len();
    stream.epoch = stream.writing.wrapping_add(1);
    stream.writing = stream.epoch;
    stream.base = 0;
    stream.root = Some(root);
    stream.top = super::top_space(store, root);
    super::paint_walk(store, root, out, table, stream);
    stream.len = out.len() - start;
}

impl Stream {
    /// `node`'s record in the stream, if that stream painted it.
    #[inline]
    fn of(&self, node: NodeId) -> Option<Record> {
        self.records
            .get(node.index() as usize)
            .filter(|r| r.epoch == self.epoch && r.generation == node.generation())
            .copied()
    }
}

impl PaintSpans for Stream {
    #[inline(always)]
    fn open(&mut self, _: NodeId, _: usize) -> usize {
        0
    }

    #[inline]
    fn close(&mut self, _: usize, node: NodeId, span: Span) {
        let i = node.index() as usize;
        if i >= self.records.len() {
            self.records.resize(i + 1, Record::default());
        }
        let base = self.base;
        let at = |p: usize| (p as i64 + base) as u32;
        let len = (span.own_end - span.own) as u32;
        debug_assert!(len <= LEN, "a node's own drawables fit the record");
        self.records[i] = Record {
            own: at(span.own),
            len: len | u32::from(span.flags) << 24,
            end: at(span.end),
            slot: span.slot.unwrap_or(u32::MAX),
            parent: span.parent,
            depth: span.depth,
            generation: node.generation(),
            epoch: self.writing,
        };
    }
}

/// The slot of `child`'s parent.
#[inline]
fn child_parent(arena: &crate::node::NodeArena, child: NodeId) -> u32 {
    arena.parent_of(child.node_index()).get()
}

/// Whether `node` still opens its span as `record` says: it fades, scrolls
/// and is a transform root as it did.
#[inline]
fn holds(store: &NodeStore, node: NodeId, record: &Record) -> bool {
    let flag = |on: bool, flag: u8| on == record.has(flag);
    flag(store.opacity(node) < 1.0, Span::FADED)
        && flag(store.is_scroll(node), Span::SCROLL)
        && flag(store.is_transform_root(node), Span::ROOT)
}

/// Whether `node` paints anything: it is neither hidden nor transparent.
#[inline]
fn paints(store: &NodeStore, node: NodeId) -> bool {
    !store.hidden(node) && store.opacity(node) > 0.0
}

/// Bring `out`, the stream the last paint from `root` emitted, up to date by
/// re-emitting the own drawables of the nodes `changed` names (by slot) and
/// diffing the children of those `relinked` names.
///
/// A patch rewrites `out` in place and lists the positions it rewrote in
/// `patched`, ascending; a splice lists the runs it replaced in `splices`,
/// in the new stream's indices.
///
/// Returns `None`, with `out` as it was, when that is not a splice: the
/// paint was not from `root`, or a transform root or an overlay would enter
/// or leave the stream. Paint it whole.
#[allow(clippy::too_many_arguments)]
pub(crate) fn splice_walk(
    store: &NodeStore,
    root: NodeId,
    table: &mut TransformTable,
    index: &mut PaintIndex,
    changed: impl Iterator<Item = u32> + Clone,
    relinked: &[u32],
    out: &mut Vec<Primitive>,
    patched: &mut Vec<u32>,
    splices: &mut Vec<Splice>,
) -> Option<Spliced> {
    let PaintIndex { stream, scratch } = index;
    if stream.root != Some(root) || out.len() < stream.len {
        return None;
    }
    stream.writing = stream.epoch.wrapping_add(1);
    // Where no children changed, the changed nodes' runs are rewritten in
    // place unless one changes length or kind.
    if relinked.is_empty() {
        let mut walk = Walk {
            store,
            table: &mut *table,
            stream: &mut *stream,
            out: &out[..],
            s: &mut *scratch,
            delta: 0,
            reshaped: false,
            unordered: true,
        };
        let patchable = walk.patch(changed.clone());
        if patchable {
            patched.clear();
            patch(scratch, out, patched);
            // A node both repainted and moved is rewritten twice.
            patched.sort_unstable();
            patched.dedup();
        }
        scratch.items.clear();
        scratch.edits.clear();
        scratch.fresh.clear();
        if patchable {
            return Some(Spliced::Patched(patched.len() as u32));
        }
    }
    // Before a fresh paint can take a freed overlay's slot.
    scratch.overlays.clear();
    let deferred = table
        .overlays
        .iter()
        .map(|&(overlay, _)| (overlay, stream.of(overlay)));
    scratch.overlays.extend(deferred);
    let walked = place(store, root, stream, scratch, changed, relinked).and_then(|()| {
        let mut walk = Walk {
            store,
            table,
            stream: &mut *stream,
            out: &out[..],
            s: &mut *scratch,
            delta: 0,
            reshaped: false,
            unordered: false,
        };
        walk.run(root)?;
        Some((walk.delta, walk.reshaped))
    });
    for &slot in &scratch.marked {
        scratch.marks[slot as usize] = 0;
    }
    scratch.marked.clear();
    scratch.items.clear();
    scratch.stack.clear();
    let spliced = walked.map(|(delta, reshaped)| {
        if !reshaped && delta == 0 && in_place(scratch, out) {
            patched.clear();
            patch(scratch, out, patched);
            Spliced::Patched(patched.len() as u32)
        } else {
            splices.clear();
            let inserted = splice(stream, scratch, out, splices);
            Spliced::Spliced(inserted)
        }
    });
    scratch.edits.clear();
    scratch.fresh.clear();
    scratch.updates.clear();
    scratch.touched.clear();
    scratch.steps.clear();
    spliced
}

/// Mark the nodes the walk visits — those `changed` and `relinked` name and
/// their ancestors — and list those the last paint painted, by where it
/// emitted them. `None` when a node that paints has no record and no change
/// above it explains why.
fn place(
    store: &NodeStore,
    root: NodeId,
    stream: &Stream,
    s: &mut Scratch,
    changed: impl Iterator<Item = u32>,
    relinked: &[u32],
) -> Option<()> {
    let arena = store.arena();
    if s.marks.len() < arena.slot_count() {
        s.marks.resize(arena.slot_count(), 0);
    }
    fn note(s: &mut Scratch, slot: u32, bit: u8) {
        let mark = &mut s.marks[slot as usize];
        if *mark == 0 {
            s.marked.push(slot);
        }
        *mark |= bit;
    }
    for &slot in relinked {
        if arena.live_id(slot).is_some() {
            note(s, slot, RELINKED);
        }
    }
    // Their records before a fresh paint can take a freed child's slot.
    let touched = arena.touched().iter().map(|&(parent, child)| Touched {
        parent,
        child,
        old: stream.of(child),
        anchor: None,
        after: (0, 0),
        kept: false,
    });
    s.touched.extend(touched);
    s.touched
        .sort_unstable_by_key(|t| (t.parent, t.child.index(), t.child.generation()));
    s.touched.dedup_by_key(|t| (t.parent, t.child));
    for slot in changed {
        if arena.live_id(slot).is_some() {
            note(s, slot, OWN);
        }
    }
    let root = root.node_index();
    for k in 0..s.marked.len() {
        let slot = s.marked[k];
        if s.marks[slot as usize] & (PLACED | ORPHAN) != 0 {
            continue;
        }
        // Up to the root, or to a node already found.
        s.chain.clear();
        let mut at = NodeIndex::new(slot);
        let above = loop {
            s.chain.push(at);
            if at == root {
                break Some(false);
            }
            let parent = arena.parent_of(at);
            if parent.is_none() {
                break None;
            }
            let mark = s.marks[parent.slot()];
            if mark & ORPHAN != 0 {
                break None;
            }
            if mark & PLACED != 0 {
                let id = arena.id_of(parent)?;
                break Some(mark & (COVERED | RELINKED) != 0 || !paints(store, id));
            }
            at = parent;
        };
        let Some(mut covered) = above else {
            for k in 0..s.chain.len() {
                note(s, s.chain[k].get(), ORPHAN);
            }
            continue;
        };
        // Down again, placing each.
        for k in (0..s.chain.len()).rev() {
            let at = s.chain[k];
            let id = arena.id_of(at)?;
            note(s, at.get(), PLACED | if covered { COVERED } else { 0 });
            let bits = s.marks[at.slot()];
            match stream.of(id) {
                Some(record) => s.items.push(Item {
                    start: record.start(),
                    end: record.end,
                    depth: record.depth,
                    node: id,
                    bits: bits & (OWN | RELINKED),
                    record,
                }),
                // Painted fresh by a changed ancestor, or not at all.
                None if covered || !paints(store, id) => {}
                None => return None,
            }
            covered |= bits & RELINKED != 0 || !paints(store, id);
        }
    }
    // Pre-order: by start, an ancestor before its descendants. Siblings at
    // one start paint nothing but the last, in their order.
    s.items
        .sort_unstable_by_key(|item| (item.start, item.depth, item.end));
    let key = |item: &Item| (item.start, item.depth, item.end);
    let mut k = 0;
    while k + 1 < s.items.len() {
        let run = s.items[k..]
            .iter()
            .take_while(|item| key(item) == key(&s.items[k]))
            .count();
        if run > 1 {
            order_siblings(store, &s.marks, &mut s.items[k..k + run])?;
        }
        k += run;
    }
    Some(())
}

/// Put `items`, siblings that paint nothing at one place, in their order
/// under a parent whose children did not change.
fn order_siblings(store: &NodeStore, marks: &[u8], items: &mut [Item]) -> Option<()> {
    let arena = store.arena();
    let parent = arena.parent(items[0].node)?;
    if marks[parent.index() as usize] & RELINKED != 0
        || items
            .iter()
            .any(|item| arena.parent(item.node) != Some(parent))
    {
        return None;
    }
    let mut placed = 0;
    for child in arena.children(parent) {
        if let Some(at) = items[placed..].iter().position(|item| item.node == child) {
            items.swap(placed, placed + at);
            placed += 1;
            if placed == items.len() {
                return Some(());
            }
        }
    }
    None
}

struct Walk<'a> {
    store: &'a NodeStore,
    table: &'a mut TransformTable,
    stream: &'a mut Stream,
    out: &'a [Primitive],
    s: &'a mut Scratch,
    /// How far the stream so far moved.
    delta: i64,
    /// Some node left the stream or painted fresh.
    reshaped: bool,
    /// The edits come in any order, none joining another: a patch.
    unordered: bool,
}

impl Walk<'_> {
    fn run(&mut self, root: NodeId) -> Option<()> {
        let root_end = self.stream.of(root)?.end;
        let mut gone: Option<Item> = None;
        for k in 0..self.s.items.len() {
            let item = self.s.items[k];
            if gone.is_some_and(|g| item.within(g.start, g.end, g.depth)) {
                continue;
            }
            gone = None;
            // An overlay sits past the whole main walk, inside no one.
            let overlay = item.node != root && self.store.is_overlay(item.node);
            while let Some(top) = self.s.stack.last() {
                if !overlay && item.within(top.record.start(), top.record.end, top.depth) {
                    break;
                }
                self.close()?;
            }
            let Some(top) = self.s.stack.last_mut() else {
                let at_top = if overlay {
                    item.start >= root_end
                } else {
                    item.node == root
                };
                if !at_top || !paints(self.store, item.node) {
                    return None;
                }
                self.open(&item)?;
                continue;
            };
            match top.diff.take() {
                Some(mut diff) => {
                    let fate = self.advance(&mut diff, Some(&item));
                    if let Some(top) = self.s.stack.last_mut() {
                        top.diff = Some(diff);
                    }
                    match fate? {
                        Fate::Kept => self.open(&item)?,
                        Fate::Gone => gone = Some(item),
                    }
                }
                None => {
                    let parent = top.node;
                    let arena = self.store.arena();
                    if arena.parent(item.node) != Some(parent)
                        || item.record.parent != parent.index()
                        || overlay
                        || !paints(self.store, item.node)
                    {
                        return None;
                    }
                    self.open(&item)?;
                }
            }
        }
        while !self.s.stack.is_empty() {
            self.close()?;
        }
        self.overlays_hold().then_some(())
    }

    /// Whether every overlay the last paint deferred still is, from the
    /// same parent, in the same order: its parent's subtree kept its place
    /// and none came or went.
    fn overlays_hold(&self) -> bool {
        let (store, arena) = (self.store, self.store.arena());
        let mut last: Option<(NodeId, u32)> = None;
        self.s.overlays.iter().all(|&(overlay, record)| {
            // One that painted nothing must still paint nothing.
            let Some(record) = record else {
                return !arena.is_live(overlay) || !paints(store, overlay);
            };
            let parent = child_parent(arena, overlay);
            if !arena.is_live(overlay)
                || !store.is_overlay(overlay)
                || !paints(store, overlay)
                || record.parent != parent
            {
                return false;
            }
            let Some(parent_id) = arena.live_id(parent) else {
                return false;
            };
            let kept = match self.s.marks[parent as usize] {
                mark if mark & PLACED != 0 => mark & KEPT != 0,
                _ => self
                    .stream
                    .of(parent_id)
                    .is_some_and(|r| !self.removed(r.start())),
            };
            // Overlays of one parent defer in its children's order.
            let ordered = match last {
                Some((before, at)) if at == parent => arena
                    .children(parent_id)
                    .skip_while(|&c| c != before)
                    .any(|c| c == overlay),
                _ => true,
            };
            last = Some((overlay, parent));
            kept && ordered
        })
    }

    /// Whether old stream position `at` is inside a run an edit replaced.
    fn removed(&self, at: u32) -> bool {
        let edits = &self.s.edits;
        let k = edits.partition_point(|e| e.at + e.removed <= at);
        edits.get(k).is_some_and(|e| e.at <= at)
    }

    /// Where old stream position `at` lands, given the edits so far.
    fn moved(&self, at: u32) -> u32 {
        (i64::from(at) + self.delta) as u32
    }

    /// The displacement transform root `slot` was painted at.
    fn painted(&self, slot: Option<u32>) -> Option<Vec2> {
        match slot {
            None => Some(self.stream.top.snapped),
            Some(slot) => {
                let &[x, y] = self.table.painted().get(slot as usize)?;
                Some(Vec2 { x, y })
            }
        }
    }

    /// Rewrite the own drawables of the nodes `changed` names in place, no
    /// parent's children having changed. `false` when one was not painted,
    /// or its run would change length or kind: walk the tree then.
    fn patch(&mut self, changed: impl Iterator<Item = u32>) -> bool {
        let (store, arena) = (self.store, self.store.arena());
        for slot in changed {
            let Some(node) = arena.live_id(slot) else {
                continue;
            };
            let Some(record) = self.stream.of(node) else {
                return false;
            };
            if !paints(store, node)
                || !holds(store, node, &record)
                || self.own(node, &record).is_none()
                || self.delta != 0
            {
                return false;
            }
        }
        in_place(self.s, self.out)
    }

    /// Visit a node still where the last paint emitted it.
    fn open(&mut self, item: &Item) -> Option<()> {
        let (store, node, record) = (self.store, item.node, item.record);
        if !holds(store, node, &record) {
            return None;
        }
        let (own, len) = if item.bits & OWN != 0 {
            self.own(node, &record)?
        } else {
            (self.moved(record.own), record.len())
        };
        let diff = if item.bits & RELINKED != 0 {
            let (cursor, end) = record.children();
            let slot = if record.has(Span::SCROLL) {
                match self.out.get(cursor.checked_sub(1)? as usize)? {
                    Primitive::Transform(TransformSlot(slot)) => Some(*slot),
                    _ => return None,
                }
            } else {
                record.slot()
            };
            let snapped = self.painted(slot)?;
            let logged = store.arena().logged(node.index());
            #[cfg(test)]
            let logged = match tests::BLIND.get() {
                true => Logged::Whole,
                false => logged,
            };
            let steps = match logged {
                Logged::Children => Some(self.steps(node, (cursor, end))?),
                _ => None,
            };
            Some(Diff {
                next: store.arena().first_child(node),
                cursor,
                end,
                inner: Space {
                    slot,
                    raw: snapped,
                    snapped,
                    depth: record.depth + 1,
                },
                steps,
            })
        } else {
            None
        };
        self.s.marks[node.index() as usize] |= KEPT;
        self.s.stack.push(Open {
            node,
            record,
            depth: item.depth,
            own,
            len,
            diff,
        });
        Some(())
    }

    /// Leave the innermost visited node: finish its children diff and note
    /// where its primitives now are.
    fn close(&mut self) -> Option<()> {
        let mut open = self.s.stack.pop()?;
        if let Some(mut diff) = open.diff.take() {
            self.advance(&mut diff, None)?;
            if let Some(steps) = diff.steps {
                self.s.steps.truncate(steps.base as usize);
            }
        }
        let record = open.record;
        let end = self.moved(record.end);
        self.s.updates.push((
            open.node.index(),
            Record {
                own: open.own,
                len: open.len | record.len & !LEN,
                end,
                epoch: self.stream.writing,
                ..record
            },
        ));
        Some(())
    }

    /// Re-emit `node`'s own drawables over the run `record` gives them, and
    /// rewrite the layers it clips: its fade over its reach and its scroll
    /// clip over its box. Returns where its drawables land, and how many.
    fn own(&mut self, node: NodeId, record: &Record) -> Option<(u32, u32)> {
        let store = self.store;
        let shift = self.painted(record.slot())?;
        let world = store.bounds(node).translate(shift);
        let (own, len) = (record.own as usize, record.len() as usize);
        if record.has(Span::FADED) {
            let at = own.checked_sub(1)?;
            let Primitive::Layer(old) = self.out.get(at)? else {
                return None;
            };
            let layer = LayerClip {
                clip: store.reach(node).translate(shift),
                opacity: store.opacity(node),
                ..*old
            };
            if layer != *old {
                self.replace(at as u32, Primitive::Layer(layer))?;
            }
        }
        let landed = self.moved(record.own);
        let from = self.s.fresh.len();
        paint_own(store, node, world, &mut self.s.fresh);
        let emitted = self.s.fresh.len() - from;
        if emitted > LEN as usize {
            return None;
        }
        if self.s.fresh[from..] == *self.out.get(own..own + len)? {
            self.s.fresh.truncate(from);
        } else {
            self.edit(record.own, record.len(), from)?;
        }
        if record.has(Span::SCROLL) {
            let at = own + len;
            let Primitive::Layer(old) = self.out.get(at)? else {
                return None;
            };
            if old.clip != world {
                let layer = LayerClip {
                    clip: world,
                    ..*old
                };
                self.replace(at as u32, Primitive::Layer(layer))?;
            }
        }
        Some((landed, emitted as u32))
    }

    /// Advance a children diff: up to the child `target` when given —
    /// reporting whether it kept its run — else through the last child.
    fn advance(&mut self, diff: &mut Diff, target: Option<&Item>) -> Option<Fate> {
        if diff.steps.is_some() {
            return self.step(diff, target);
        }
        let store = self.store;
        let arena = store.arena();
        loop {
            let Some(child) = diff.next else {
                if target.is_some() {
                    return Some(Fate::Gone);
                }
                self.gap(diff, diff.end)?;
                return Some(Fate::Kept);
            };
            let next = arena.next_sibling(child);
            diff.next = next;
            if !paints(store, child) {
                continue;
            }
            let record = self.stream.of(child);
            if store.is_overlay(child) {
                // One the last paint deferred here still is; one new here is
                // not spliced.
                if record.is_none_or(|r| r.parent != child_parent(arena, child)) {
                    return None;
                }
                continue;
            }
            let parent = child_parent(arena, child);
            match record {
                Some(r) if r.parent == parent && r.start() >= diff.cursor && r.end <= diff.end => {
                    if let Some(target) = target {
                        if child != target.node && r.start() == target.start {
                            return None;
                        }
                        if child != target.node && r.start() > target.start {
                            diff.next = Some(child);
                            return Some(Fate::Gone);
                        }
                    }
                    self.gap(diff, r.start())?;
                    diff.cursor = r.end;
                    if target.is_some_and(|t| t.node == child) {
                        return Some(Fate::Kept);
                    }
                }
                _ => {
                    self.fresh(child, diff)?;
                    if target.is_some_and(|t| t.node == child) {
                        return Some(Fate::Gone);
                    }
                }
            }
        }
    }

    /// List the steps of `parent`'s children diff over its old run
    /// `cursor..end`: each touched child that painted there leaves its run,
    /// and each that paints there now paints fresh after the last sibling
    /// before it that kept its run.
    fn steps(&mut self, parent: NodeId, (cursor, end): (u32, u32)) -> Option<Steps> {
        let (store, arena) = (self.store, self.store.arena());
        let p = parent.index();
        let lo = self.s.touched.partition_point(|t| t.parent < p);
        let hi = lo + self.s.touched[lo..].partition_point(|t| t.parent == p);
        let base = self.s.steps.len();
        for k in lo..hi {
            let child = self.s.touched[k].child;
            let attached = arena.parent(child) == Some(parent) && paints(store, child);
            // An overlay new here is not spliced.
            if attached && store.is_overlay(child) {
                return None;
            }
            let anchor = match attached {
                true => Some(self.anchor(parent, (cursor, end), (lo, hi), k)?),
                false => None,
            };
            let t = self.s.touched[k];
            if t.kept {
                continue;
            }
            if let Some(r) = t.old.filter(|r| r.parent == p) {
                if r.start() < cursor || r.end > end {
                    return None;
                }
                self.s.steps.push(Step {
                    at: r.start(),
                    end: r.end,
                    rank: 0,
                    child: None,
                });
            }
            if let Some((at, rank)) = anchor {
                self.s.steps.push(Step {
                    at,
                    end: at,
                    rank,
                    child: Some(child),
                });
            }
        }
        // A fresh child lands before a run that leaves where it paints.
        self.s.steps[base..].sort_unstable_by_key(|s| (s.at, s.child.is_none(), s.rank));
        Some(Steps {
            parent: p,
            touched: (lo as u32, hi as u32),
            base: base as u32,
            next: base as u32,
            end: self.s.steps.len() as u32,
        })
    }

    /// Where the touched child `k` of `parent` paints among the old children
    /// run `cursor..end`: at the end of the last sibling before it that kept
    /// its run, and how many touched siblings paint there before it. Each
    /// touched sibling it passes is placed on the way, so a run of new
    /// children is walked once.
    fn anchor(
        &mut self,
        parent: NodeId,
        (cursor, end): (u32, u32),
        (lo, hi): (usize, usize),
        k: usize,
    ) -> Option<(u32, u32)> {
        if let Some(anchor) = self.s.touched[k].anchor {
            return Some(anchor);
        }
        let (store, arena) = (self.store, self.store.arena());
        self.s.passed.clear();
        let mut prev = arena.prev_sibling(self.s.touched[k].child);
        let mut next = loop {
            let Some(sibling) = prev else {
                break (cursor, 0);
            };
            let key = (sibling.index(), sibling.generation());
            let touched = self.s.touched[lo..hi]
                .binary_search_by_key(&key, |t| (t.child.index(), t.child.generation()));
            if let Ok(j) = touched {
                if self.s.touched[lo + j].anchor.is_some() {
                    break self.s.touched[lo + j].after;
                }
                self.s.passed.push((lo + j) as u32);
            } else if paints(store, sibling) && !store.is_overlay(sibling) {
                let r = self.stream.of(sibling).filter(|r| {
                    r.parent == parent.index() && r.start() >= cursor && r.end <= end
                })?;
                break (r.end, 0);
            }
            prev = arena.prev_sibling(sibling);
        };
        for i in (0..self.s.passed.len()).rev() {
            let j = self.s.passed[i] as usize;
            next = self.settle(parent, end, j, next);
        }
        self.settle(parent, end, k, next);
        self.s.touched[k].anchor
    }

    /// Place the touched child `j` of `parent` at `anchor`: it keeps its run
    /// when that run starts there and no touched sibling paints there before
    /// it. Returns the anchor of the sibling after it.
    fn settle(&mut self, parent: NodeId, end: u32, j: usize, anchor: (u32, u32)) -> (u32, u32) {
        let store = self.store;
        let t = &mut self.s.touched[j];
        t.anchor = Some(anchor);
        let child = t.child;
        let paints = store.arena().parent(child) == Some(parent)
            && paints(store, child)
            && !store.is_overlay(child);
        let kept = t.old.filter(|r| {
            paints && anchor == (r.start(), 0) && r.parent == parent.index() && r.end <= end
        });
        t.kept = kept.is_some();
        t.after = match (kept, paints) {
            (Some(r), _) => (r.end, 0),
            (None, true) => (anchor.0, anchor.1 + 1),
            (None, false) => anchor,
        };
        t.after
    }

    /// Advance a children diff by its steps: up to the child `target` when
    /// given — reporting whether it kept its run — else through the last.
    fn step(&mut self, diff: &mut Diff, target: Option<&Item>) -> Option<Fate> {
        let mut steps = diff.steps?;
        while steps.next < steps.end {
            let step = self.s.steps[steps.next as usize];
            if target.is_some_and(|t| step.at > t.start) {
                break;
            }
            steps.next += 1;
            diff.cursor = step.at;
            match step.child {
                Some(child) => self.fresh(child, diff)?,
                None => {
                    self.gap(diff, step.end)?;
                    diff.cursor = step.end;
                }
            }
        }
        diff.steps = Some(steps);
        let Some(target) = target else {
            return Some(Fate::Kept);
        };
        let (lo, hi) = (steps.touched.0 as usize, steps.touched.1 as usize);
        let key = (target.node.index(), target.node.generation());
        let touched = self.s.touched[lo..hi]
            .binary_search_by_key(&key, |t| (t.child.index(), t.child.generation()));
        if touched.is_ok_and(|j| !self.s.touched[lo + j].kept) {
            return Some(Fate::Gone);
        }
        let r = target.record;
        if child_parent(self.store.arena(), target.node) != steps.parent
            || r.parent != steps.parent
            || r.start() < diff.cursor
            || r.end > diff.end
        {
            return None;
        }
        diff.cursor = r.end;
        Some(Fate::Kept)
    }

    /// The old children run from the diff's cursor to `to` is gone.
    fn gap(&mut self, diff: &Diff, to: u32) -> Option<()> {
        let from = diff.cursor;
        if to <= from {
            return Some(());
        }
        let roots = self
            .out
            .get(from as usize..to as usize)?
            .iter()
            .any(|p| matches!(p, Primitive::Transform(_)));
        if roots {
            return None;
        }
        self.reshaped = true;
        self.edit(from, to - from, self.s.fresh.len())
    }

    /// Paint `child` fresh at the diff's cursor, recording where it lands.
    fn fresh(&mut self, child: NodeId, diff: &Diff) -> Option<()> {
        let from = self.s.fresh.len();
        self.stream.base = i64::from(self.moved(diff.cursor)) - from as i64;
        let roots = self.table.len();
        paint_subtree(
            self.store,
            child,
            diff.inner,
            &mut self.s.fresh,
            self.table,
            &mut self.s.deferred,
            self.stream,
        );
        self.stream.base = 0;
        if self.table.len() != roots || !self.s.deferred.is_empty() {
            self.s.deferred.clear();
            return None;
        }
        self.reshaped = true;
        self.edit(diff.cursor, 0, from)
    }

    /// Replace the old primitive at `at` with `with`.
    fn replace(&mut self, at: u32, with: Primitive) -> Option<()> {
        let from = self.s.fresh.len();
        self.s.fresh.push(with);
        self.edit(at, 1, from)
    }

    /// Replace `removed` old primitives from `at` with the fresh ones from
    /// `from` on, joining the edit before when they touch.
    fn edit(&mut self, at: u32, removed: u32, from: usize) -> Option<()> {
        let inserted = (self.s.fresh.len() - from) as u32;
        if removed == 0 && inserted == 0 {
            return Some(());
        }
        self.delta += i64::from(inserted) - i64::from(removed);
        let from = from as u32;
        if let Some(last) = self.s.edits.last_mut()
            && !self.unordered
        {
            let end = last.at + last.removed;
            if end > at {
                return None;
            }
            if end == at && last.from + last.inserted == from {
                last.removed += removed;
                last.inserted += inserted;
                return Some(());
            }
        }
        self.s.edits.push(Edit {
            at,
            removed,
            from,
            inserted,
        });
        Some(())
    }
}

/// Whether the walk's edits all keep their length and kinds.
fn in_place(s: &Scratch, out: &[Primitive]) -> bool {
    s.edits.iter().all(|e| {
        let (at, from) = (e.at as usize, e.from as usize);
        e.removed == e.inserted
            && out[at..at + e.removed as usize]
                .iter()
                .zip(&s.fresh[from..from + e.inserted as usize])
                .all(|(old, new)| core::mem::discriminant(old) == core::mem::discriminant(new))
    })
}

/// Rewrite the edits' primitives in place, listing the positions that
/// changed.
fn patch(s: &mut Scratch, out: &mut [Primitive], patched: &mut Vec<u32>) {
    for e in &s.edits {
        for k in 0..e.removed {
            let (at, from) = ((e.at + k) as usize, (e.from + k) as usize);
            // A run of one differs: it is an edit.
            if e.removed == 1 || out[at] != s.fresh[from] {
                core::mem::swap(&mut out[at], &mut s.fresh[from]);
                patched.push(at as u32);
            }
        }
    }
}

/// Splice the edits into `out`, listing the runs in the new stream's
/// indices, and move every record to the new stream. Returns how many
/// primitives went in.
fn splice(
    stream: &mut Stream,
    s: &mut Scratch,
    out: &mut Vec<Primitive>,
    splices: &mut Vec<Splice>,
) -> u32 {
    let mut shift = 0i64;
    let mut inserted = 0;
    for e in &s.edits {
        splices.push(Splice {
            at: (i64::from(e.at) + shift) as u32,
            removed: e.removed,
            inserted: e.inserted,
        });
        shift += i64::from(e.inserted) - i64::from(e.removed);
        inserted += e.inserted;
    }
    for e in s.edits.iter().rev() {
        let (at, from) = (e.at as usize, e.from as usize);
        let new = s.fresh[from..from + e.inserted as usize]
            .iter_mut()
            .map(|p| core::mem::replace(p, Primitive::LayerEnd));
        out.splice(at..at + e.removed as usize, new).for_each(drop);
    }
    // The nodes the walk did not visit sit inside no edit, whole: each moves
    // by what the edits before it moved, or is gone with the run it sat in.
    let (old, writing) = (stream.epoch, stream.writing);
    for record in stream.records.iter_mut().filter(|r| r.epoch == old) {
        let start = record.start();
        let k = s.edits.partition_point(|e| e.at + e.removed <= start);
        if s.edits.get(k).is_some_and(|e| e.at <= start) {
            continue;
        }
        let by = splices
            .get(k)
            .map_or(shift, |sp| i64::from(sp.at) - i64::from(s.edits[k].at));
        record.own = (i64::from(record.own) + by) as u32;
        record.end = (i64::from(record.end) + by) as u32;
        record.epoch = writing;
    }
    for &(slot, record) in &s.updates {
        stream.records[slot as usize] = record;
    }
    stream.epoch = writing;
    stream.len = (stream.len as i64 + shift) as usize;
    inserted
}

#[cfg(test)]
mod tests {
    use super::super::paint_tree;
    use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore, Painted, ScrollStyle};
    use crate::content::Content;
    use crate::layout::{Axis, Size};
    use crate::node::NodeId;
    use crate::style::BoxStyle;
    use viso_render::{GlyphInstanceData, Primitive, Rect, Rgba, TextureId};

    const SURFACE: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 1200.0,
    };

    fn color(k: u32) -> Rgba {
        let a = if k.is_multiple_of(5) { 0.0 } else { 1.0 };
        Rgba::new((k % 7) as f32 / 7.0, (k % 3) as f32 / 3.0, 0.5, a)
    }

    /// A run of `n` glyphs.
    fn text(n: usize) -> Content {
        let rect = |x: f32, w: f32| Rect {
            x,
            y: 0.0,
            w,
            h: 6.0,
        };
        Content::Text {
            glyphs: (0..n)
                .map(|i| GlyphInstanceData {
                    rect: rect(i as f32 * 5.0, 4.0),
                    uv: rect(0.0, 0.5),
                })
                .collect(),
            atlas: TextureId::new(1),
            color_glyphs: Vec::new(),
            color_atlas: None,
            color: color(1),
            natural: crate::layout::Vec2 {
                x: n as f32 * 5.0,
                y: 6.0,
            },
            baseline: 5.0,
            shaped_at_width: None,
            soft_wrap: false,
        }
    }

    /// A card: a row holding a swatch and a label, detached.
    fn card(store: &mut NodeStore, k: u32) -> (NodeId, NodeId) {
        let mut label = None;
        let card = {
            let mut cx = BuildCx::new(store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    size: Size::fixed(200.0, 12.0 + (k % 3) as f32 * 4.0),
                    style: BoxStyle::solid(color(k)),
                    ..FlexStyle::default()
                },
                |cx| {
                    cx.leaf(LeafStyle {
                        size: Size::fixed(8.0, 8.0),
                        style: BoxStyle::solid(color(k + 1)),
                    });
                    label = Some(cx.leaf(LeafStyle::default()).id());
                },
            )
            .id()
        };
        let label = label.unwrap();
        store.set_content_payload(label, text(1 + k as usize % 4));
        (card, label)
    }

    /// A column holding a scroll viewport and a few loose cards; the viewport
    /// holds `n` cards. Returns the root, the two containers, and the cards
    /// with their labels.
    fn page(store: &mut NodeStore, n: u32) -> (NodeId, [NodeId; 2], Vec<(NodeId, NodeId)>) {
        let mut viewport = None;
        let root = {
            let mut cx = BuildCx::new(store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    size: Size::fill(),
                    ..FlexStyle::default()
                },
                |cx| {
                    viewport = Some(
                        cx.scroll(
                            ScrollStyle {
                                axis: Axis::Column,
                                size: Size::fixed(300.0, 600.0),
                                style: BoxStyle::solid(color(2)),
                            },
                            |_| {},
                        )
                        .id(),
                    );
                },
            )
            .id()
        };
        let viewport = viewport.unwrap();
        let mut cards = Vec::new();
        for k in 0..n + 3 {
            let (card, label) = card(store, k);
            let parent = if k < n { viewport } else { root };
            store.arena_insert_before(parent, card, None);
            cards.push((card, label));
        }
        // A popup over everything, and a card slid sideways.
        store.set_overlay(cards[n as usize].0, true);
        store.set_translate(cards[1].0, crate::layout::Vec2 { x: 3.0, y: 0.0 });
        (root, [viewport, root], cards)
    }

    /// A deterministic stream of numbers.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, n: u32) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) % u64::from(n.max(1))) as u32
        }
    }

    /// Lay out and repaint a frame, checking the stream against a whole
    /// paint of the tree as it stands.
    #[track_caller]
    fn frame(store: &mut NodeStore, root: NodeId, out: &mut Vec<Primitive>) -> Painted {
        store.relayout_dirty(root, SURFACE, &mut Vec::new(), &mut Vec::new());
        let painted = store.repaint_dirty(root, out);
        let mut whole = Vec::new();
        paint_tree(store, root, &mut whole);
        assert!(*out == whole, "{painted:?}: the stream a whole paint emits");
        if let Painted::Spliced(_) = painted {
            let splices = store.splices();
            assert!(!splices.is_empty());
            for w in splices.windows(2) {
                assert!(w[0].at + w[0].inserted <= w[1].at, "ascending, apart");
            }
        }
        store.clear_dirty();
        painted
    }

    #[test]
    fn every_change_splices_into_what_a_whole_paint_emits() {
        const SEEDS: u64 = 16;
        const FRAMES: u32 = 400;
        let (mut spliced, mut patched) = (0, 0);
        for seed in 0..SEEDS {
            let (s, p) = changes(seed, FRAMES);
            spliced += s;
            patched += p;
            BLIND.set(true);
            changes(seed, FRAMES);
            BLIND.set(false);
        }
        // The rest start or stop a fade, or move a transform root or an
        // overlay in or out.
        let whole = SEEDS as u32 * FRAMES - spliced - patched;
        assert!(
            spliced > 3000 && patched > 2000 && whole < 1000,
            "{spliced} spliced, {patched} patched, {whole} whole"
        );
    }

    thread_local! {
        /// Diff every relinked parent's children whole, as if the arena did
        /// not know which changed.
        pub(super) static BLIND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Run `frames` frames of random changes from `seed` over a page,
    /// checking each; returns how many spliced and how many patched.
    fn changes(seed: u64, frames: u32) -> (u32, u32) {
        let mut store = NodeStore::new();
        let (root, parents, mut cards) = page(&mut store, 12);
        let mut out = Vec::new();
        assert!(matches!(
            frame(&mut store, root, &mut out),
            Painted::Whole(_)
        ));
        let mut rng = Lcg(seed);
        let mut spliced = 0;
        let mut patched = 0;
        let mut scratch = Vec::new();
        let mut effects = crate::reactive::EffectStore::default();
        for step in 0..frames {
            for _ in 0..1 + rng.below(3) {
                let k = rng.below(cards.len() as u32) as usize;
                let (card, label) = cards[k];
                match rng.below(9) {
                    0 => store.set_fill(card, color(step)),
                    1 => store.set_fill(label, color(step)),
                    2 => store.set_hidden(card, !store.hidden(card)),
                    3 => {
                        let opacity = [0.0, 1.0, 1.0, 0.5][rng.below(4) as usize];
                        store.set_opacity(card, opacity);
                    }
                    4 => {
                        let (new, label) = self::card(&mut store, step);
                        let parent = parents[rng.below(2) as usize];
                        let before = store.arena().children(parent).nth(rng.below(8) as usize);
                        store.arena_insert_before(parent, new, before);
                        cards.push((new, label));
                    }
                    5 if cards.len() > 4 => {
                        store.free_tree(card, &mut effects, &mut scratch);
                        cards.swap_remove(k);
                    }
                    6 => {
                        // Out and back in elsewhere, as a list recycles a row.
                        let parent = parents[rng.below(2) as usize];
                        if let Some(old) = store.parent(card) {
                            store.mark_dirty(old, crate::dirty::DirtyClass::LAYOUT);
                        }
                        store.arena_detach(card);
                        let before = store.arena().children(parent).nth(rng.below(8) as usize);
                        store.arena_insert_before(parent, card, before);
                    }
                    7 => store
                        .set_fixed_size(card, Size::fixed(200.0, 10.0 + rng.below(4) as f32 * 3.0)),
                    _ => store.set_content_payload(label, text(rng.below(6) as usize)),
                }
            }
            match frame(&mut store, root, &mut out) {
                Painted::Spliced(_) => spliced += 1,
                Painted::Patched(_) => patched += 1,
                _ => {}
            }
        }
        (spliced, patched)
    }

    #[test]
    fn a_splice_under_a_scrolled_root_paints_where_the_root_was_painted() {
        use crate::layout::Vec2;
        let mut store = NodeStore::new();
        let (root, [viewport, _], cards) = page(&mut store, 6);
        let mut out = Vec::new();
        frame(&mut store, root, &mut out);
        // The scroll moves the retained draws; nothing repaints.
        store.scroll_by(viewport, Vec2 { x: 0.0, y: 30.0 });
        store.update_transforms();
        assert_eq!(store.repaint_dirty(root, &mut out), Painted::Nothing);
        store.clear_dirty();
        // A card mounts and another changes, under the scrolled content.
        let (new, _) = card(&mut store, 9);
        store.arena_insert_before(viewport, new, Some(cards[3].0));
        store.set_fill(cards[4].0, color(3));
        store.relayout_dirty(root, SURFACE, &mut Vec::new(), &mut Vec::new());
        assert!(matches!(
            store.repaint_dirty(root, &mut out),
            Painted::Spliced(_)
        ));
        store.clear_dirty();
        // Scrolled back, the displacement is the one the stream was painted at.
        store.scroll_by(viewport, Vec2 { x: 0.0, y: -30.0 });
        store.update_transforms();
        store.clear_dirty();
        let mut whole = Vec::new();
        paint_tree(&store, root, &mut whole);
        assert!(out == whole, "painted where the root was painted");
    }
}
