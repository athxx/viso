//! Node identity and the retained tree arena.
//!
//! Two kinds of reference point at a node:
//!
//! - an external handle, [`NodeId`], carries a generation, so a holder that
//!   outlives the node (an inspector, the DSL runtime, an app-held handle)
//!   detects that it went stale;
//! - an internal link, [`NodeIndex`], is a bare slot index. Tree links, binding
//!   targets, dirty worklists and paint owners use it: the arena unlinks a node
//!   before freeing it, so an internal link never points at a freed slot and
//!   needs no generation check.

/// A bare arena slot index — the internal link form. [`NodeIndex::NONE`] is the
/// absent link.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeIndex(u32);

impl NodeIndex {
    /// No node.
    pub const NONE: NodeIndex = NodeIndex(u32::MAX);

    /// The index of slot `i`.
    #[inline]
    pub const fn new(i: u32) -> NodeIndex {
        NodeIndex(i)
    }

    /// The raw slot index.
    #[inline]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The slot index as a `usize`, for indexing hot stores.
    #[inline]
    pub const fn slot(self) -> usize {
        self.0 as usize
    }

    /// Whether this is [`NodeIndex::NONE`].
    #[inline]
    pub const fn is_none(self) -> bool {
        self.0 == u32::MAX
    }

    /// `None` for [`NodeIndex::NONE`], else `Some(self)`.
    #[inline]
    pub const fn get_some(self) -> Option<NodeIndex> {
        if self.is_none() { None } else { Some(self) }
    }
}

/// Compact generational node identity — the external handle form.
///
/// The generation of a live node is always odd (see [`NodeArena`]), so a stale
/// handle is detectable with one comparison.
///
/// A debug build also stamps each handle with the arena that issued it, and
/// that arena asserts a handle it is given is its own: one window's handle
/// used on another window's tree panics instead of naming whichever node
/// holds that slot there. Equality, hashing and `Debug` ignore the stamp, so
/// a debug build behaves as a release one does.
#[derive(Clone, Copy)]
pub struct NodeId {
    index: u32,
    generation: u32,
    #[cfg(debug_assertions)]
    owner: u32,
}

impl PartialEq for NodeId {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.generation == other.generation
    }
}

impl Eq for NodeId {}

impl core::hash::Hash for NodeId {
    #[inline]
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.index.hash(state);
        self.generation.hash(state);
    }
}

/// A node's tasks chain on its slot; a reused slot's new node shares the
/// chain but not the old node's tasks.
impl viso_runtime::TaskOwner for NodeId {
    #[inline]
    fn key(self) -> u32 {
        self.index
    }
}

impl core::fmt::Debug for NodeId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NodeId")
            .field("index", &self.index)
            .field("generation", &self.generation)
            .finish()
    }
}

impl NodeId {
    /// A handle to slot `index` that is never live: what a full
    /// fixed-capacity store hands out for a node it refuses. It belongs to no
    /// arena, so any arena may be asked about it.
    pub(crate) const fn refused(index: u32) -> NodeId {
        NodeId {
            index,
            generation: 0,
            #[cfg(debug_assertions)]
            owner: Owner::NONE,
        }
    }

    /// The raw slot index.
    #[inline]
    pub fn index(self) -> u32 {
        self.index
    }

    /// The slot index as an internal link.
    #[inline]
    pub fn node_index(self) -> NodeIndex {
        NodeIndex(self.index)
    }

    /// The generation this handle was issued at.
    #[inline]
    pub fn generation(self) -> u32 {
        self.generation
    }
}

/// Stored tree links of one slot: 20 bytes of bare indices.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeLinks {
    pub parent: NodeIndex,
    pub first_child: NodeIndex,
    pub last_child: NodeIndex,
    pub prev_sibling: NodeIndex,
    pub next_sibling: NodeIndex,
}

impl NodeLinks {
    /// A node with no relatives.
    pub const ORPHAN: NodeLinks = NodeLinks {
        parent: NodeIndex::NONE,
        first_child: NodeIndex::NONE,
        last_child: NodeIndex::NONE,
        prev_sibling: NodeIndex::NONE,
        next_sibling: NodeIndex::NONE,
    };
}

impl Default for NodeLinks {
    fn default() -> Self {
        NodeLinks::ORPHAN
    }
}

/// A node's relatives as live handles — the checked view [`NodeArena::links`]
/// returns, built from the stored [`NodeLinks`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TreeLinks {
    pub parent: Option<NodeId>,
    pub first_child: Option<NodeId>,
    pub last_child: Option<NodeId>,
    pub prev_sibling: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
}

/// Which arena issued a handle, in a debug build: each arena takes the next
/// number of its thread when it is made.
#[cfg(debug_assertions)]
struct Owner(u32);

#[cfg(debug_assertions)]
impl Owner {
    /// No arena's: a handle any arena may be asked about.
    const NONE: u32 = 0;
}

#[cfg(debug_assertions)]
std::thread_local! {
    static NEXT_OWNER: core::cell::Cell<u32> = const { core::cell::Cell::new(1) };
}

#[cfg(debug_assertions)]
impl Default for Owner {
    fn default() -> Self {
        Owner(NEXT_OWNER.with(|next| next.replace(next.get() + 1)))
    }
}

/// Numbers this thread's next arenas from the first again, so a scripted run
/// repeated on it issues the same handles: a test may read a node off one run
/// and look it up on the next.
#[doc(hidden)]
pub fn restart_owners() {
    #[cfg(debug_assertions)]
    NEXT_OWNER.with(|next| next.set(1));
}

/// The arena is full: it was built with [`NodeArena::with_fixed_capacity`] and
/// every slot is live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArenaFull {
    /// The fixed capacity.
    pub capacity: u32,
}

impl core::fmt::Display for ArenaFull {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "node arena is full ({} nodes)", self.capacity)
    }
}

/// Generational arena backing the retained UI tree.
///
/// Storage is two parallel arrays indexed by slot: `generation` (odd = live,
/// even = free; freeing and reusing a slot each bump it by one) and the
/// 20-byte [`NodeLinks`]. Traversal reads only `links`.
///
/// Besides single slots the arena hands out *blocks*: runs of consecutive
/// slots an instance of a template fills in order, so a binding can name its
/// target as an offset from the block's base. A block is reserved
/// whole and released whole; a slot of a reserved block that is freed stays
/// out of the free list until its block is released. Released blocks are
/// kept by length and reused by the next block of the same length, so the
/// rows of a list recycle one another's blocks exactly.
#[derive(Default)]
pub struct NodeArena {
    generation: Vec<u32>,
    links: Vec<NodeLinks>,
    free: Vec<u32>,
    /// Released blocks by length: each entry's bases.
    blocks: Vec<(u32, Vec<u32>)>,
    /// Live slots.
    live: u32,
    /// `Some(n)` when built with a fixed capacity: the arena never grows past
    /// `n` slots and never reallocates after construction.
    fixed: Option<u32>,
    /// The parents whose children changed since the log was last cleared,
    /// each once, and by slot whether it is logged.
    relinked: Vec<u32>,
    logged: Vec<Logged>,
    /// The children each change was to, with their parent's slot, while
    /// fewer than [`TOUCHES`].
    touched: Vec<(u32, NodeId)>,
    overflowed: bool,
    #[cfg(debug_assertions)]
    owner: Owner,
}

/// How much of a parent's children change the log knows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Logged {
    #[default]
    No,
    /// Every child it changed is in the touched log.
    Children,
    /// Not which children.
    Whole,
}

/// The most children the touched log holds before it gives up; a fixed
/// arena's holds at most as many as it has slots.
const TOUCHES: usize = 1024;

impl NodeArena {
    /// The bytes one slot takes: its generation and its links.
    pub const SLOT_BYTES: usize = size_of::<u32>() + size_of::<NodeLinks>();

    /// The bytes it holds: its slots, which every walk reads, and its
    /// bookkeeping of freed slots and relinked parents, which a structural
    /// edit does.
    pub(crate) fn held_bytes(&self) -> (usize, usize) {
        let slots = self.links.len() * Self::SLOT_BYTES;
        let blocks: usize = self
            .blocks
            .iter()
            .map(|(_, bases)| size_of::<(u32, Vec<u32>)>() + bases.len() * size_of::<u32>())
            .sum();
        let log = self.logged.len() * size_of::<Logged>()
            + (self.free.len() + self.relinked.len()) * size_of::<u32>()
            + self.touched.len() * size_of::<(u32, NodeId)>();
        (slots, log + blocks)
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// An arena that holds at most `capacity` live nodes and allocates its
    /// storage once, up front. [`try_alloc`](Self::try_alloc) reports
    /// [`ArenaFull`] instead of growing.
    pub fn with_fixed_capacity(capacity: u32) -> Self {
        NodeArena {
            generation: Vec::with_capacity(capacity as usize),
            links: Vec::with_capacity(capacity as usize),
            free: Vec::with_capacity(capacity as usize),
            blocks: Vec::new(),
            live: 0,
            fixed: Some(capacity),
            relinked: Vec::with_capacity(capacity as usize),
            logged: Vec::with_capacity(capacity as usize),
            touched: Vec::with_capacity(TOUCHES.min(capacity as usize)),
            overflowed: false,
            #[cfg(debug_assertions)]
            owner: Owner::default(),
        }
    }

    /// The handle to slot `index` at `generation`, stamped as this arena's.
    #[inline]
    fn handle(&self, index: u32, generation: u32) -> NodeId {
        NodeId {
            index,
            generation,
            #[cfg(debug_assertions)]
            owner: self.owner.0,
        }
    }

    /// The slot of `id`, for indexing a store's columns without a liveness
    /// check. A debug build asserts `id` is this arena's.
    #[inline]
    #[track_caller]
    pub fn slot(&self, id: NodeId) -> usize {
        self.assert_owns(id);
        id.index as usize
    }

    /// In a debug build, panics unless this arena issued `id`.
    #[inline]
    #[track_caller]
    fn assert_owns(&self, id: NodeId) {
        #[cfg(debug_assertions)]
        assert!(
            id.owner == self.owner.0 || id.owner == Owner::NONE,
            "{id:?} belongs to another tree: a handle from one window used on another"
        );
        let _ = id;
    }

    /// Drop every node, keeping the storage and any fixed capacity.
    pub fn clear(&mut self) {
        self.generation.clear();
        self.links.clear();
        self.free.clear();
        self.blocks.clear();
        self.live = 0;
        self.relinked.clear();
        self.logged.clear();
        self.touched.clear();
        self.overflowed = false;
    }

    /// Log that `parent`'s children changed: `child` entered, left or
    /// started or stopped painting, or `None` when which is not known.
    pub(crate) fn note_relinked(&mut self, parent: NodeIndex, child: Option<NodeId>) {
        let i = parent.slot();
        if i >= self.logged.len() {
            self.logged.resize(i + 1, Logged::No);
        }
        let touches = self.fixed.map_or(TOUCHES, |n| TOUCHES.min(n as usize));
        let logged = &mut self.logged[i];
        if *logged == Logged::No {
            *logged = Logged::Children;
            self.relinked.push(i as u32);
        }
        match child {
            Some(child) if *logged == Logged::Children && self.touched.len() < touches => {
                // Once, whole: the log never grows on a later frame.
                if self.touched.capacity() == 0 {
                    self.touched.reserve_exact(touches);
                }
                self.touched.push((i as u32, child));
            }
            Some(_) if *logged == Logged::Children => self.overflowed = true,
            _ => *logged = Logged::Whole,
        }
    }

    /// Whether `parent`'s children changed since the log was last cleared.
    #[inline]
    pub(crate) fn is_relinked(&self, parent: NodeIndex) -> bool {
        self.logged
            .get(parent.slot())
            .is_some_and(|&l| l != Logged::No)
    }

    /// How much of `parent`'s children change the log knows.
    #[inline]
    pub(crate) fn logged(&self, parent: u32) -> Logged {
        let logged = self
            .logged
            .get(parent as usize)
            .copied()
            .unwrap_or_default();
        match logged {
            Logged::Children if self.overflowed => Logged::Whole,
            logged => logged,
        }
    }

    /// The children the relinks were to, by their parent's slot, in the
    /// order they changed; a child may be named more than once.
    #[inline]
    pub(crate) fn touched(&self) -> &[(u32, NodeId)] {
        &self.touched
    }

    /// The slots of the parents whose children changed since the log was
    /// last cleared, each once.
    pub(crate) fn relinks(&self) -> &[u32] {
        &self.relinked
    }

    /// Empty the relink log.
    pub(crate) fn clear_relinks(&mut self) {
        for &i in &self.relinked {
            self.logged[i as usize] = Logged::No;
        }
        self.relinked.clear();
        self.touched.clear();
        self.overflowed = false;
    }

    /// Reserve room for `additional` more slots. A no-op for a fixed-capacity
    /// arena, which reserved everything at construction.
    pub fn reserve(&mut self, additional: usize) {
        if self.fixed.is_none() {
            self.generation.reserve(additional);
            self.links.reserve(additional);
        }
    }

    /// Whether another node fits.
    #[inline]
    pub fn has_room(&self) -> bool {
        !self.free.is_empty()
            || self
                .fixed
                .is_none_or(|n| (self.generation.len() as u32) < n)
    }

    /// The fixed capacity, if any.
    #[inline]
    pub fn fixed_capacity(&self) -> Option<u32> {
        self.fixed
    }

    /// The number of slots, live or free. Every hot store is this long.
    #[inline]
    pub fn slot_count(&self) -> usize {
        self.generation.len()
    }

    /// Allocate a fresh node, reusing a free slot when possible.
    ///
    /// # Panics
    ///
    /// When a fixed-capacity arena is full. A fixed capacity is a profile
    /// budget; exceeding it is a configuration error that desktop emulation of
    /// the profile surfaces before the app reaches a device.
    pub fn alloc(&mut self) -> NodeId {
        match self.try_alloc() {
            Ok(id) => id,
            Err(full) => panic!("{full}"),
        }
    }

    /// Allocate a fresh node, or report [`ArenaFull`].
    pub fn try_alloc(&mut self) -> Result<NodeId, ArenaFull> {
        if let Some(index) = self.free.pop() {
            let i = index as usize;
            debug_assert!(self.generation[i] & 1 == 0, "free slot must be even");
            self.generation[i] = self.generation[i].wrapping_add(1);
            self.links[i] = NodeLinks::ORPHAN;
            self.live += 1;
            return Ok(self.handle(index, self.generation[i]));
        }
        let index = self.generation.len() as u32;
        if let Some(capacity) = self.fixed
            && index >= capacity
        {
            return Err(ArenaFull { capacity });
        }
        self.generation.push(1);
        self.links.push(NodeLinks::ORPHAN);
        self.live += 1;
        Ok(self.handle(index, 1))
    }

    /// Free a node. Bumps the slot generation so any surviving [`NodeId`]
    /// becomes detectably stale. The caller has unlinked it from live relatives.
    pub fn free(&mut self, id: NodeId) -> bool {
        if !self.free_kept(id) {
            return false;
        }
        self.free.push(id.index);
        true
    }

    /// Free a node of a reserved block: the slot goes stale but stays with
    /// its block, out of the free list, until the block is released.
    pub fn free_kept(&mut self, id: NodeId) -> bool {
        if !self.is_live(id) {
            return false;
        }
        let i = id.index as usize;
        self.generation[i] = self.generation[i].wrapping_add(1);
        self.live -= 1;
        true
    }

    /// Reserve a block of `len` consecutive free slots and return its base: a
    /// released block of that length when there is one, else new slots at the
    /// end. `None` when a fixed-capacity arena has no room for it at the end.
    /// The slots stay free until [`alloc_at`](Self::alloc_at) fills them.
    pub fn reserve_block(&mut self, len: u32) -> Option<u32> {
        if let Some((_, bases)) = self.blocks.iter_mut().find(|(n, _)| *n == len)
            && let Some(base) = bases.pop()
        {
            return Some(base);
        }
        let base = self.generation.len() as u32;
        if let Some(capacity) = self.fixed
            && base + len > capacity
        {
            return None;
        }
        self.generation
            .resize(self.generation.len() + len as usize, 0);
        self.links
            .resize(self.links.len() + len as usize, NodeLinks::ORPHAN);
        Some(base)
    }

    /// Fill slot `index` of a reserved block.
    ///
    /// # Panics
    ///
    /// When the slot is live.
    pub fn alloc_at(&mut self, index: u32) -> NodeId {
        let i = index as usize;
        assert!(self.generation[i] & 1 == 0, "a block slot is filled once");
        self.generation[i] = self.generation[i].wrapping_add(1);
        self.links[i] = NodeLinks::ORPHAN;
        self.live += 1;
        self.handle(index, self.generation[i])
    }

    /// Give back the block of `len` slots at `base`, every one of them free,
    /// for the next block of the same length.
    pub fn release_block(&mut self, base: u32, len: u32) {
        debug_assert!(
            (base..base + len).all(|i| self.generation[i as usize] & 1 == 0),
            "a released block holds no live node"
        );
        match self.blocks.iter_mut().find(|(n, _)| *n == len) {
            Some((_, bases)) => bases.push(base),
            None => self.blocks.push((len, vec![base])),
        }
    }

    /// The live [`NodeId`] for a slot index, pairing the index with the slot's
    /// current generation. `None` if the slot is free or out of range.
    #[inline]
    pub fn live_id(&self, index: u32) -> Option<NodeId> {
        let generation = *self.generation.get(index as usize)?;
        (generation & 1 == 1).then(|| self.handle(index, generation))
    }

    /// The live handle of an internal link. `None` for [`NodeIndex::NONE`].
    #[inline]
    pub fn id_of(&self, index: NodeIndex) -> Option<NodeId> {
        if index.is_none() {
            None
        } else {
            self.live_id(index.0)
        }
    }

    /// How many nodes are live.
    #[inline]
    pub fn live_count(&self) -> usize {
        self.live as usize
    }

    /// Whether an id refers to a currently-live node.
    #[inline]
    #[track_caller]
    pub fn is_live(&self, id: NodeId) -> bool {
        self.assert_owns(id);
        self.generation.get(id.index as usize) == Some(&id.generation) && id.generation & 1 == 1
    }

    /// The stored links of a slot — the unchecked traversal path. The slot must
    /// be live; internal links always are.
    #[inline]
    pub fn raw(&self, index: NodeIndex) -> &NodeLinks {
        &self.links[index.slot()]
    }

    /// The parent of a slot.
    #[inline]
    pub fn parent_of(&self, index: NodeIndex) -> NodeIndex {
        self.links[index.slot()].parent
    }

    /// The first child of a slot.
    #[inline]
    pub fn first_child_of(&self, index: NodeIndex) -> NodeIndex {
        self.links[index.slot()].first_child
    }

    /// The next sibling of a slot.
    #[inline]
    pub fn next_sibling_of(&self, index: NodeIndex) -> NodeIndex {
        self.links[index.slot()].next_sibling
    }

    /// The children of a slot, in order.
    #[inline]
    pub fn children_of(&self, index: NodeIndex) -> Children<'_> {
        Children {
            links: &self.links,
            next: self.links[index.slot()].first_child,
        }
    }

    /// The parent of a live node.
    #[inline]
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.link(id, |l| l.parent)
    }

    /// The first child of a live node.
    #[inline]
    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.link(id, |l| l.first_child)
    }

    /// The last child of a live node.
    #[inline]
    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.link(id, |l| l.last_child)
    }

    /// The next sibling of a live node.
    #[inline]
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.link(id, |l| l.next_sibling)
    }

    /// The previous sibling of a live node.
    #[inline]
    pub fn prev_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.link(id, |l| l.prev_sibling)
    }

    /// The children of a live node as live handles, in order. Empty for a stale
    /// handle.
    #[inline]
    pub fn children(&self, id: NodeId) -> ChildIds<'_> {
        let next = if self.is_live(id) {
            self.links[id.index as usize].first_child
        } else {
            NodeIndex::NONE
        };
        ChildIds { arena: self, next }
    }

    /// One stored link of a live node, as a live handle. An internal link always
    /// points at a live slot, so the generation read is the only extra load.
    #[inline]
    fn link(&self, id: NodeId, pick: impl FnOnce(&NodeLinks) -> NodeIndex) -> Option<NodeId> {
        if !self.is_live(id) {
            return None;
        }
        let target = pick(&self.links[id.index as usize]);
        if target.is_none() {
            return None;
        }
        Some(self.handle(target.0, self.generation[target.slot()]))
    }

    /// Ancestry links for a live node, as live handles.
    pub fn links(&self, id: NodeId) -> Option<TreeLinks> {
        if !self.is_live(id) {
            return None;
        }
        let raw = &self.links[id.index as usize];
        Some(TreeLinks {
            parent: self.id_of(raw.parent),
            first_child: self.id_of(raw.first_child),
            last_child: self.id_of(raw.last_child),
            prev_sibling: self.id_of(raw.prev_sibling),
            next_sibling: self.id_of(raw.next_sibling),
        })
    }

    /// Clear a live node's child endpoints without touching the children, for a
    /// caller that frees the whole child subtree right after.
    pub fn forget_children(&mut self, id: NodeId) -> bool {
        if !self.is_live(id) {
            return false;
        }
        let raw = &mut self.links[id.index as usize];
        raw.first_child = NodeIndex::NONE;
        raw.last_child = NodeIndex::NONE;
        self.note_relinked(id.node_index(), None);
        true
    }

    /// Append `child` as the last child of `parent`, wiring the sibling chain.
    /// Both must be live; a no-op returning `false` otherwise.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> bool {
        if !self.is_live(parent) || !self.is_live(child) {
            return false;
        }
        let (p, c) = (parent.node_index(), child.node_index());
        let prev_last = self.links[p.slot()].last_child;
        if !prev_last.is_none() {
            self.links[prev_last.slot()].next_sibling = c;
        }
        let child_links = &mut self.links[c.slot()];
        child_links.parent = p;
        child_links.prev_sibling = prev_last;
        child_links.next_sibling = NodeIndex::NONE;
        let parent_links = &mut self.links[p.slot()];
        if parent_links.first_child.is_none() {
            parent_links.first_child = c;
        }
        parent_links.last_child = c;
        self.note_relinked(p, Some(child));
        true
    }

    /// Link the orphan `child` under `parent` immediately before its child
    /// `before`, or last when `before` is `None`. A no-op returning `false` when
    /// a node is stale, `child` still has a parent, or `before` is not a child
    /// of `parent`.
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, before: Option<NodeId>) -> bool {
        if !self.is_live(parent)
            || !self.is_live(child)
            || !self.links[child.index as usize].parent.is_none()
        {
            return false;
        }
        let Some(before) = before else {
            return self.append_child(parent, child);
        };
        let (p, c, b) = (parent.node_index(), child.node_index(), before.node_index());
        if !self.is_live(before) || self.links[b.slot()].parent != p {
            return false;
        }
        let prev = self.links[b.slot()].prev_sibling;
        if prev.is_none() {
            self.links[p.slot()].first_child = c;
        } else {
            self.links[prev.slot()].next_sibling = c;
        }
        self.links[b.slot()].prev_sibling = c;
        let child_links = &mut self.links[c.slot()];
        child_links.parent = p;
        child_links.prev_sibling = prev;
        child_links.next_sibling = b;
        self.note_relinked(p, Some(child));
        true
    }

    /// Unlink `child` from its parent and siblings, leaving it a live orphan
    /// (parent/prev/next cleared, but the slot stays occupied and its generation
    /// is untouched — the id remains valid). This is the detach half of node
    /// recycling: a virtual list parks a mounted host out of the tree without
    /// freeing it, then re-attaches it elsewhere with [`append_child`]. A no-op
    /// returning `false` if `child` is not live or has no parent.
    ///
    /// [`append_child`]: Self::append_child
    pub fn detach_child(&mut self, child: NodeId) -> bool {
        if !self.is_live(child) {
            return false;
        }
        let c = child.node_index();
        let NodeLinks {
            parent,
            prev_sibling: prev,
            next_sibling: next,
            ..
        } = self.links[c.slot()];
        if parent.is_none() {
            return false;
        }
        if prev.is_none() {
            self.links[parent.slot()].first_child = next;
        } else {
            self.links[prev.slot()].next_sibling = next;
        }
        if next.is_none() {
            self.links[parent.slot()].last_child = prev;
        } else {
            self.links[next.slot()].prev_sibling = prev;
        }
        let child_links = &mut self.links[c.slot()];
        child_links.parent = NodeIndex::NONE;
        child_links.prev_sibling = NodeIndex::NONE;
        child_links.next_sibling = NodeIndex::NONE;
        self.note_relinked(parent, Some(child));
        true
    }
}

/// The children of one node, in order.
pub struct Children<'a> {
    links: &'a [NodeLinks],
    next: NodeIndex,
}

impl Iterator for Children<'_> {
    type Item = NodeIndex;

    #[inline]
    fn next(&mut self) -> Option<NodeIndex> {
        let current = self.next.get_some()?;
        self.next = self.links[current.slot()].next_sibling;
        Some(current)
    }
}

/// The children of one node as live handles, in order.
pub struct ChildIds<'a> {
    arena: &'a NodeArena,
    next: NodeIndex,
}

impl Iterator for ChildIds<'_> {
    type Item = NodeId;

    #[inline]
    fn next(&mut self) -> Option<NodeId> {
        let current = self.next.get_some()?;
        self.next = self.arena.links[current.slot()].next_sibling;
        Some(
            self.arena
                .handle(current.0, self.arena.generation[current.slot()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_twenty_bytes() {
        assert_eq!(core::mem::size_of::<NodeLinks>(), 20);
        assert_eq!(core::mem::size_of::<NodeIndex>(), 4);
    }

    #[test]
    fn alloc_produces_live_ids() {
        let mut arena = NodeArena::new();
        let a = arena.alloc();
        let b = arena.alloc();
        assert!(arena.is_live(a));
        assert!(arena.is_live(b));
        assert_ne!(a, b);
        assert_eq!(a.generation() & 1, 1, "live generation is odd");
    }

    #[test]
    fn freed_id_becomes_stale() {
        let mut arena = NodeArena::new();
        let a = arena.alloc();
        assert!(arena.free(a));
        assert!(!arena.is_live(a), "stale handle must be detectable");
        assert!(!arena.free(a));
        assert_eq!(arena.live_id(a.index()), None);
    }

    #[test]
    fn fixed_capacity_reports_full_and_reuses() {
        let mut arena = NodeArena::with_fixed_capacity(2);
        let a = arena.alloc();
        let _b = arena.alloc();
        assert_eq!(arena.try_alloc(), Err(ArenaFull { capacity: 2 }));
        arena.free(a);
        let c = arena.try_alloc().expect("a freed slot is reused");
        assert_eq!(c.index(), a.index());
        assert_eq!(arena.slot_count(), 2);
    }

    #[test]
    fn insert_before_links_at_the_anchor() {
        let mut arena = NodeArena::new();
        let parent = arena.alloc();
        let a = arena.alloc();
        let b = arena.alloc();
        let c = arena.alloc();
        arena.append_child(parent, b);
        assert!(arena.insert_before(parent, a, Some(b)));
        assert!(arena.insert_before(parent, c, None));
        assert!(
            !arena.insert_before(parent, c, Some(a)),
            "c already has a parent"
        );
        let order: Vec<_> = arena
            .children_of(parent.node_index())
            .map(|i| arena.id_of(i).unwrap())
            .collect();
        assert_eq!(order, [a, b, c]);
        assert_eq!(arena.children(parent).collect::<Vec<_>>(), [a, b, c]);
        assert_eq!(arena.parent(b), Some(parent));
        assert_eq!(arena.next_sibling(c), None);
        assert_eq!(arena.links(b).unwrap().prev_sibling, Some(a));
        assert_eq!(arena.links(parent).unwrap().last_child, Some(c));
    }

    #[test]
    fn detach_child_unlinks_but_keeps_alive() {
        let mut arena = NodeArena::new();
        let parent = arena.alloc();
        let a = arena.alloc();
        let b = arena.alloc();
        let c = arena.alloc();
        arena.append_child(parent, a);
        arena.append_child(parent, b);
        arena.append_child(parent, c);

        assert!(arena.detach_child(b));
        assert!(arena.is_live(b), "detach must not free the slot");
        let bl = arena.links(b).unwrap();
        assert_eq!(bl.parent, None);
        assert_eq!(bl.prev_sibling, None);
        assert_eq!(bl.next_sibling, None);
        assert_eq!(arena.links(a).unwrap().next_sibling, Some(c));
        assert_eq!(arena.links(c).unwrap().prev_sibling, Some(a));
        assert_eq!(arena.links(parent).unwrap().first_child, Some(a));
        assert_eq!(arena.links(parent).unwrap().last_child, Some(c));

        assert!(arena.detach_child(a));
        assert_eq!(arena.links(parent).unwrap().first_child, Some(c));
        assert_eq!(arena.links(c).unwrap().prev_sibling, None);

        assert!(arena.detach_child(c));
        assert_eq!(arena.links(parent).unwrap().first_child, None);
        assert_eq!(arena.links(parent).unwrap().last_child, None);

        assert!(!arena.detach_child(b));
        assert!(arena.append_child(parent, b));
        assert_eq!(arena.links(parent).unwrap().first_child, Some(b));
    }

    #[test]
    fn a_released_block_is_reused_by_the_next_of_its_length() {
        let mut arena = NodeArena::new();
        let single = arena.alloc();
        let base = arena.reserve_block(3).unwrap();
        assert_eq!(base, 1);
        let ids: Vec<_> = (base..base + 3).map(|i| arena.alloc_at(i)).collect();
        assert_eq!(arena.live_count(), 4);
        for &id in &ids {
            assert!(arena.free_kept(id));
        }
        assert_eq!(arena.alloc().index(), 4, "a kept slot is not a free slot");
        arena.release_block(base, 3);
        assert_eq!(arena.reserve_block(2), Some(5));
        assert_eq!(arena.reserve_block(3), Some(base));
        assert!(arena.is_live(single));
    }

    #[test]
    fn a_fixed_arena_reserves_no_block_past_its_capacity() {
        let mut arena = NodeArena::with_fixed_capacity(4);
        assert_eq!(arena.reserve_block(3), Some(0));
        assert_eq!(arena.reserve_block(2), None);
    }

    #[test]
    fn reused_slot_bumps_generation() {
        let mut arena = NodeArena::new();
        let a = arena.alloc();
        arena.free(a);
        let b = arena.alloc();
        assert_eq!(a.index(), b.index());
        assert_ne!(a.generation(), b.generation());
        assert!(!arena.is_live(a));
        assert!(arena.is_live(b));
    }
}
