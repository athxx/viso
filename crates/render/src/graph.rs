//! The frame's render graph (§16.1, §25): which passes exist, what each one
//! reads and writes, and the order they execute in.
//!
//! The renderer does not decide pass order by hand. Every pass that draws —
//! an offscreen layer, a blur rung, the surface — is *recorded* here as a node
//! with its attachment (the one target it writes) and its read set (the targets
//! it samples). One compile then turns that record into the frame's plan:
//!
//! 1. **validate** the dependency order — every read resolves to a pass that
//!    already ran, and no pass reads what it is writing. Under an RHI with no
//!    explicit barriers (`viso-gpu` has none), pass order *is* the barrier and
//!    state lowering, so this check is what makes the lowering sound;
//! 2. **cull** passes nobody reads, transitively;
//! 3. **merge** adjacent passes that write the same attachment into one render
//!    pass, so the GPU does not switch render target (and, on a tile GPU, does
//!    not store and reload the attachment) between them;
//! 4. **lower loads** — pick each compiled pass's load op;
//! 5. **derive transient lifetimes** — the recorded reads are the single source
//!    of truth for how long each pooled target must stay live, replacing
//!    hand-placed [`TransientTargets::read_at`] calls at the effect sites.
//!
//! The graph owns topology only. What a pass *draws* stays in the renderer
//! (`offscreen_passes` / `blur_passes` / the main segment list); a node just
//! carries the [`PassWork`] tag that says where to look. It never sees the
//! widget tree, layout, state bindings, or fonts.
//!
//! # Plan reuse
//!
//! Recording is cheap (pushes into buffers that keep their capacity), but the
//! compile is skipped entirely when this frame's topology hashes equal to the
//! last frame's, and the cached plan is reused. The hash covers the node list,
//! each node's work tag and attachment, and each node's read edges — nothing
//! else. In particular it does **not** cover extents: viewports are derived from
//! the work payload at encode time every frame, so a window resize, a scroll, a
//! recolor, or a blur sigma change that keeps the same ladder shape all reuse the
//! plan. A sigma change that crosses a ladder tier changes the rung count, which
//! is a genuinely different topology and recompiles.

use core::hash::{Hash, Hasher};

use crate::transient::{SURFACE_SLOT, TargetId, TransientTargets};

/// Sentinel for "no such node" in the graph's side tables.
const NO_NODE: u32 = u32::MAX;

/// What a pass node draws. The payload is an index into the renderer's own pass
/// storage — the graph never interprets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PassWork {
    /// The offscreen layer pass at this index in the renderer's offscreen list:
    /// renders a layer's subtree into the layer's base target.
    Offscreen(u32),
    /// The blur rung at this index in the renderer's blur list: reads the
    /// previous rung's target and writes its own.
    Blur(u32),
    /// The frame's surface pass: everything drawn straight to the window, plus
    /// the composites of every offscreen layer.
    Surface,
}

/// How a compiled pass initializes its attachment.
///
/// There is deliberately no discard/`DontCare` variant. A discard load is only
/// correct when the pass *replaces* the attachment's contents, and every
/// renderer pipeline — including the blur — is created with
/// `BlendMode::PremultipliedOver`, so a discarded attachment would be blended
/// against undefined memory. The final blur rung is additionally upsampled by
/// its composite, whose bilinear edge taps can reach outside the rung's used
/// extent, so even replace-blending would not make discard safe there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassLoad {
    /// Clear to transparent black — every transient color attachment starts
    /// empty so the subtree composites correctly over whatever samples it.
    ClearTransparent,
    /// Clear to the frame's background color, supplied at submit time.
    ClearBackground,
}

/// A handle to a recorded pass node. Its numeric value is the node's *slot*: the
/// index the transient pool's lifetime analysis works in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassNodeId(u32);

/// A pass as recorded, before compilation. Reads live in one flat arena shared by
/// every node, so recording a frame allocates nothing once warm.
#[derive(Debug, Clone, Copy)]
struct PassNode {
    /// What this pass draws.
    work: PassWork,
    /// The single target it writes, or `None` for the surface.
    writes: Option<TargetId>,
    /// Start of this node's slice of the read arena.
    first_read: u32,
    /// Length of that slice.
    read_count: u32,
}

/// One pass of the compiled plan: a contiguous run of works sharing an
/// attachment, encoded as a single backend render pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompiledPass {
    /// Start of this pass's slice of the plan's work order.
    first_work: u32,
    /// Length of that slice — greater than one only for a merged pass.
    work_count: u32,
    /// The attachment every work in the run writes, or `None` for the surface.
    writes: Option<TargetId>,
}

impl CompiledPass {
    /// The attachment this pass renders into, or `None` for the frame's surface.
    pub const fn writes(&self) -> Option<TargetId> {
        self.writes
    }

    /// How the attachment is initialized — the graph's load lowering, derived
    /// from the attachment itself so the two can never disagree.
    pub const fn load(&self) -> PassLoad {
        match self.writes {
            Some(_) => PassLoad::ClearTransparent,
            None => PassLoad::ClearBackground,
        }
    }
}

/// What one compile produced, reported through `FrameStats` (§30).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GraphStats {
    /// Compiled passes this frame — one backend render pass each, so this is the
    /// frame's render-target switch count.
    pub passes: u32,
    /// Recorded passes folded into a preceding pass by attachment merging.
    pub merges: u32,
    /// Recorded passes dropped because nothing reads what they write.
    pub culled: u32,
    /// `1` when this frame rebuilt the plan, `0` when it reused the cached one.
    pub compiles: u32,
}

/// The frame's pass topology, compiled once per distinct topology.
pub struct RenderGraph {
    /// This frame's recorded nodes, in the order the renderer discovered them.
    nodes: Vec<PassNode>,
    /// Flat read arena; `nodes[i]` owns `reads[first_read..first_read + n]`.
    reads: Vec<TargetId>,
    /// The compiled plan: the passes to encode, in order. Survives frames.
    passes: Vec<CompiledPass>,
    /// The plan's work order; `passes[i]` owns a contiguous run of it.
    work_order: Vec<PassWork>,
    /// Per-node liveness, rebuilt by each cull.
    live: Vec<bool>,
    /// Target index → the node that last writes it, rebuilt per compile.
    producer: Vec<u32>,
    /// Node index → the previous node writing the same target, or [`NO_NODE`].
    /// Keeps a multi-writer attachment's earlier passes alive through the cull.
    prev_writer: Vec<u32>,
    /// Hash of the topology the cached plan was compiled from; `None` until the
    /// first compile.
    topology: Option<u64>,
    /// Last compile's shape, reported every frame.
    stats: GraphStats,
}

impl Default for RenderGraph {
    fn default() -> Self {
        RenderGraph::new()
    }
}

impl RenderGraph {
    /// An empty graph with room for a typical frame's passes.
    pub fn new() -> Self {
        RenderGraph {
            nodes: Vec::with_capacity(8),
            reads: Vec::with_capacity(16),
            passes: Vec::with_capacity(8),
            work_order: Vec::with_capacity(8),
            live: Vec::with_capacity(8),
            producer: Vec::with_capacity(8),
            prev_writer: Vec::with_capacity(8),
            topology: None,
            stats: GraphStats::default(),
        }
    }

    /// Drop the previous frame's record, keeping every buffer's capacity and the
    /// cached plan (which is what [`compile`](Self::compile) may reuse).
    pub fn begin_frame(&mut self) {
        self.nodes.clear();
        self.reads.clear();
        self.stats = GraphStats::default();
    }

    /// Record a pass that draws `work` into `writes` (`None` for the surface).
    ///
    /// The returned id's numeric value is the pass's timeline slot, which is also
    /// [`next_slot`](Self::next_slot) before the call — declare the target the
    /// pass writes at that slot.
    pub fn open(&mut self, work: PassWork, writes: Option<TargetId>) -> PassNodeId {
        let id = PassNodeId(self.nodes.len() as u32);
        self.nodes.push(PassNode {
            work,
            writes,
            first_read: self.reads.len() as u32,
            read_count: 0,
        });
        id
    }

    /// Record that `node` samples `target`.
    ///
    /// # Panics
    /// Panics in debug builds if `node` is not the most recently opened node:
    /// the read arena is flat, so a node's reads must be recorded before the
    /// next one opens.
    pub fn read(&mut self, node: PassNodeId, target: TargetId) {
        let n = &mut self.nodes[node.0 as usize];
        debug_assert_eq!(
            (n.first_read + n.read_count) as usize,
            self.reads.len(),
            "a pass node's reads are recorded before the next node opens"
        );
        n.read_count += 1;
        self.reads.push(target);
    }

    /// The slot the next [`open`](Self::open) will record at.
    pub fn next_slot(&self) -> u32 {
        self.nodes.len() as u32
    }

    /// The timeline slot the surface pass occupies: the number of texture-writing
    /// passes recorded this frame, which is exactly what [`SURFACE_SLOT`]
    /// resolves to inside [`TransientTargets::assign`].
    pub fn surface_slot(&self) -> u32 {
        self.nodes.iter().filter(|n| n.writes.is_some()).count() as u32
    }

    /// Turn this frame's record into the plan to encode, reusing the cached plan
    /// when the topology is unchanged.
    ///
    /// # Panics
    /// Panics in debug builds if the record is not a valid graph — see
    /// [`validate`](Self::validate).
    pub fn compile(&mut self) {
        let topology = self.hash_topology();
        if self.topology == Some(topology) {
            self.stats.compiles = 0;
            self.report();
            return;
        }
        self.map_writers();
        self.validate();
        self.cull();
        self.merge();
        self.topology = Some(topology);
        self.stats.compiles = 1;
        self.report();
    }

    /// Extend every declared target's lifetime to cover the reads this frame
    /// recorded. The graph is the only place that knows who reads what and when,
    /// so it is the only place that drives the pool's interval analysis.
    ///
    /// Runs every frame, including when the plan was reused: the plan is
    /// topology, the lifetimes belong to this frame's virtual targets.
    pub fn apply_lifetimes(&self, targets: &mut TransientTargets) {
        for i in 0..self.nodes.len() {
            let node = self.nodes[i];
            // The surface pass's slot is not known until the texture passes are
            // counted, so it reads through the pool's own sentinel.
            let slot = match node.writes {
                Some(_) => i as u32,
                None => SURFACE_SLOT,
            };
            for &target in self.reads_of(i) {
                targets.read_at(target, slot);
            }
        }
    }

    /// The compiled passes to encode, in order.
    pub fn passes(&self) -> &[CompiledPass] {
        &self.passes
    }

    /// The works `pass` encodes, in order. Never empty.
    pub fn works(&self, pass: &CompiledPass) -> &[PassWork] {
        let start = pass.first_work as usize;
        &self.work_order[start..start + pass.work_count as usize]
    }

    /// This frame's graph counters.
    pub fn stats(&self) -> GraphStats {
        self.stats
    }

    /// `nodes[i]`'s read slice.
    fn reads_of(&self, i: usize) -> &[TargetId] {
        let node = &self.nodes[i];
        let start = node.first_read as usize;
        &self.reads[start..start + node.read_count as usize]
    }

    /// Index this frame's writers: the last writer of each target (what a reader
    /// depends on) and, per node, the previous writer of the same target.
    fn map_writers(&mut self) {
        self.producer.clear();
        self.prev_writer.clear();
        self.prev_writer.resize(self.nodes.len(), NO_NODE);
        for i in 0..self.nodes.len() {
            let Some(target) = self.nodes[i].writes else {
                continue;
            };
            let slot = target.index();
            if self.producer.len() <= slot {
                self.producer.resize(slot + 1, NO_NODE);
            }
            self.prev_writer[i] = self.producer[slot];
            self.producer[slot] = i as u32;
        }
    }

    /// The node a read of `target` depends on, or [`NO_NODE`] if nobody writes it.
    fn writer_of(&self, target: TargetId) -> u32 {
        self.producer
            .get(target.index())
            .copied()
            .unwrap_or(NO_NODE)
    }

    /// Check the record is a well-formed, executable graph.
    ///
    /// With no explicit barriers in the RHI, execution order is the only
    /// synchronization there is: a pass may sample a target only if the pass
    /// that wrote it already ran, and a pass may never sample its own
    /// attachment (that is a read-write hazard no ordering can fix).
    ///
    /// # Panics
    /// Panics in debug builds when either rule is broken, or when a
    /// texture-writing pass follows the surface pass.
    fn validate(&self) {
        for i in 0..self.nodes.len() {
            let node = self.nodes[i];
            debug_assert!(
                node.writes.is_some() || node.work == PassWork::Surface,
                "only the surface pass writes no transient target"
            );
            debug_assert!(
                node.writes.is_some() || i + 1 == self.nodes.len(),
                "the surface pass is the graph's last node"
            );
            for &target in self.reads_of(i) {
                let writer = self.writer_of(target);
                debug_assert!(
                    writer != NO_NODE,
                    "pass {i} samples a target no pass writes"
                );
                debug_assert!(
                    (writer as usize) < i,
                    "pass {i} samples a target written at or after it ({writer})"
                );
            }
        }
    }

    /// Mark the passes that contribute to the frame, dropping the rest.
    ///
    /// A pass with no attachment is an output and always live; anything else
    /// lives only if a live pass reads what it writes, or if it shares its
    /// attachment with a live later pass. Sweeping in reverse makes that
    /// transitive in one pass, because reads and same-attachment chains always
    /// point backwards (`validate` proved it).
    fn cull(&mut self) {
        self.live.clear();
        self.live.resize(self.nodes.len(), false);
        for i in 0..self.nodes.len() {
            if self.nodes[i].writes.is_none() {
                self.live[i] = true;
            }
        }
        for i in (0..self.nodes.len()).rev() {
            if !self.live[i] {
                continue;
            }
            let prev = self.prev_writer[i];
            if prev != NO_NODE {
                self.live[prev as usize] = true;
            }
            let node = self.nodes[i];
            let start = node.first_read as usize;
            for k in start..start + node.read_count as usize {
                let writer = self.writer_of(self.reads[k]);
                if writer != NO_NODE {
                    self.live[writer as usize] = true;
                }
            }
        }
    }

    /// Lay the live passes out in order, folding each run that writes one
    /// attachment into a single render pass. The folded pass keeps the first
    /// one's load, so the run still starts from a cleared attachment and the
    /// later draws simply append.
    fn merge(&mut self) {
        self.passes.clear();
        self.work_order.clear();
        for i in 0..self.nodes.len() {
            if !self.live[i] {
                continue;
            }
            let node = self.nodes[i];
            let mergeable = self
                .passes
                .last()
                .is_some_and(|prev| prev.writes.is_some() && prev.writes == node.writes);
            if mergeable {
                let last = self
                    .passes
                    .last_mut()
                    .expect("a mergeable run has a preceding pass");
                last.work_count += 1;
            } else {
                self.passes.push(CompiledPass {
                    first_work: self.work_order.len() as u32,
                    work_count: 1,
                    writes: node.writes,
                });
            }
            self.work_order.push(node.work);
        }
    }

    /// Restate the cached plan's shape as this frame's counters. Derived rather
    /// than accumulated, so a reused plan reports the same numbers as the frame
    /// that compiled it.
    fn report(&mut self) {
        self.stats.passes = self.passes.len() as u32;
        self.stats.merges = (self.work_order.len() - self.passes.len()) as u32;
        self.stats.culled = (self.nodes.len() - self.work_order.len()) as u32;
    }

    /// Hash everything the plan depends on, and nothing else: the node list,
    /// each node's work tag and attachment, and its read edges. Extents, colors,
    /// transforms, and physical texture ids are all deliberately excluded — see
    /// the module docs on plan reuse.
    fn hash_topology(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.nodes.len().hash(&mut hasher);
        for i in 0..self.nodes.len() {
            let node = self.nodes[i];
            node.work.hash(&mut hasher);
            match node.writes {
                Some(target) => (1u8, target.index()).hash(&mut hasher),
                None => 0u8.hash(&mut hasher),
            }
            node.read_count.hash(&mut hasher);
            for &target in self.reads_of(i) {
                target.index().hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transient::{TargetDesc, TargetUsage};
    use viso_gpu::TextureFormat;

    /// Declare `n` throwaway targets so tests can talk about real [`TargetId`]s
    /// the same way the renderer does — bucketing is pure, so no backend is
    /// needed until `assign`.
    fn targets(n: u32) -> (TransientTargets, Vec<TargetId>) {
        let mut pool = TransientTargets::new();
        let ids = (0..n)
            .map(|slot| {
                pool.declare(
                    TargetDesc {
                        width: 64,
                        height: 64,
                        format: TextureFormat::Bgra8Unorm,
                        usage: TargetUsage::COLOR_ATTACHMENT,
                        samples: 1,
                        label: "test",
                    },
                    slot,
                )
            })
            .collect();
        (pool, ids)
    }

    /// A frame with nothing offscreen is one surface pass — the graph adds no
    /// passes of its own.
    #[test]
    fn a_flat_frame_compiles_to_one_surface_pass() {
        let mut g = RenderGraph::new();
        g.begin_frame();
        g.open(PassWork::Surface, None);
        g.compile();

        assert_eq!(g.stats().passes, 1);
        assert_eq!(g.stats().merges, 0);
        assert_eq!(g.stats().culled, 0);
        assert_eq!(g.surface_slot(), 0);
        assert_eq!(g.works(&g.passes()[0]), &[PassWork::Surface]);
        assert_eq!(g.passes()[0].writes(), None);
    }

    /// A layer + blur rung + surface chain keeps every pass, in record order, and
    /// the surface sits at the slot after the texture passes.
    #[test]
    fn a_write_read_chain_keeps_every_pass_in_order() {
        let (_pool, t) = targets(2);
        let mut g = RenderGraph::new();
        g.begin_frame();
        g.open(PassWork::Offscreen(0), Some(t[0]));
        let blur = g.open(PassWork::Blur(0), Some(t[1]));
        g.read(blur, t[0]);
        let surface = g.open(PassWork::Surface, None);
        g.read(surface, t[1]);
        g.compile();

        assert_eq!(g.stats().passes, 3);
        assert_eq!(g.stats().culled, 0);
        assert_eq!(g.surface_slot(), 2);
        let order: Vec<PassWork> = g
            .passes()
            .iter()
            .flat_map(|p| g.works(p).iter().copied())
            .collect();
        assert_eq!(
            order,
            [PassWork::Offscreen(0), PassWork::Blur(0), PassWork::Surface]
        );
    }

    /// Load lowering is per attachment: transient targets clear to transparent,
    /// the surface clears to the frame's background.
    #[test]
    fn load_lowering_is_clear_per_attachment() {
        let (_pool, t) = targets(1);
        let mut g = RenderGraph::new();
        g.begin_frame();
        g.open(PassWork::Offscreen(0), Some(t[0]));
        let surface = g.open(PassWork::Surface, None);
        g.read(surface, t[0]);
        g.compile();

        assert_eq!(g.passes()[0].load(), PassLoad::ClearTransparent);
        assert_eq!(g.passes()[1].load(), PassLoad::ClearBackground);
    }

    /// A pass whose target nobody samples is dropped, and dropping it drops the
    /// pass that fed it: the sweep is transitive.
    #[test]
    fn unread_passes_are_culled_transitively() {
        let (_pool, t) = targets(3);
        let mut g = RenderGraph::new();
        g.begin_frame();
        // A two-rung ladder whose result the surface never composites.
        g.open(PassWork::Offscreen(0), Some(t[0]));
        let dead = g.open(PassWork::Blur(0), Some(t[1]));
        g.read(dead, t[0]);
        // A live layer the surface does composite.
        g.open(PassWork::Offscreen(1), Some(t[2]));
        let surface = g.open(PassWork::Surface, None);
        g.read(surface, t[2]);
        g.compile();

        assert_eq!(
            g.stats().culled,
            2,
            "the dead rung and its producer both go"
        );
        assert_eq!(g.stats().passes, 2);
        let order: Vec<PassWork> = g
            .passes()
            .iter()
            .flat_map(|p| g.works(p).iter().copied())
            .collect();
        assert_eq!(order, [PassWork::Offscreen(1), PassWork::Surface]);
    }

    /// Adjacent passes writing one attachment become a single render pass: two
    /// works, one target switch, and the earlier pass survives the cull because
    /// it contributes to the same attachment.
    #[test]
    fn adjacent_passes_writing_one_attachment_merge() {
        let (_pool, t) = targets(1);
        let mut g = RenderGraph::new();
        g.begin_frame();
        g.open(PassWork::Offscreen(0), Some(t[0]));
        g.open(PassWork::Offscreen(1), Some(t[0]));
        let surface = g.open(PassWork::Surface, None);
        g.read(surface, t[0]);
        g.compile();

        assert_eq!(g.stats().passes, 2, "the merged pass plus the surface");
        assert_eq!(g.stats().merges, 1);
        assert_eq!(g.stats().culled, 0);
        assert_eq!(
            g.works(&g.passes()[0]),
            &[PassWork::Offscreen(0), PassWork::Offscreen(1)]
        );
        assert_eq!(g.passes()[0].writes(), Some(t[0]));
        assert_eq!(
            g.passes()[0].load(),
            PassLoad::ClearTransparent,
            "a merged run still starts from the first pass's clear"
        );
    }

    /// Passes writing the same attachment with something else in between are not
    /// merged — folding them would reorder draws across the intervening pass.
    #[test]
    fn non_adjacent_passes_writing_one_attachment_do_not_merge() {
        let (_pool, t) = targets(2);
        let mut g = RenderGraph::new();
        g.begin_frame();
        g.open(PassWork::Offscreen(0), Some(t[0]));
        g.open(PassWork::Offscreen(1), Some(t[1]));
        g.open(PassWork::Offscreen(2), Some(t[0]));
        let surface = g.open(PassWork::Surface, None);
        g.read(surface, t[0]);
        g.read(surface, t[1]);
        g.compile();

        assert_eq!(g.stats().merges, 0);
        assert_eq!(g.stats().passes, 4);
    }

    /// An unchanged topology reuses the compiled plan; a changed one rebuilds it.
    /// Reuse is reported, not assumed: `compiles` is the observable.
    #[test]
    fn an_unchanged_topology_reuses_the_plan() {
        let (_pool, t) = targets(1);
        let record = |g: &mut RenderGraph| {
            g.begin_frame();
            g.open(PassWork::Offscreen(0), Some(t[0]));
            let surface = g.open(PassWork::Surface, None);
            g.read(surface, t[0]);
            g.compile();
        };

        let mut g = RenderGraph::new();
        record(&mut g);
        assert_eq!(g.stats().compiles, 1, "the first frame has to compile");
        let plan = g.passes().to_vec();

        record(&mut g);
        assert_eq!(g.stats().compiles, 0, "an identical topology is reused");
        assert_eq!(g.passes(), plan.as_slice());
        assert_eq!(g.stats().passes, 2);

        // One more layer is a different topology.
        let (_pool2, t2) = targets(2);
        g.begin_frame();
        g.open(PassWork::Offscreen(0), Some(t2[0]));
        g.open(PassWork::Offscreen(1), Some(t2[1]));
        let surface = g.open(PassWork::Surface, None);
        g.read(surface, t2[0]);
        g.read(surface, t2[1]);
        g.compile();
        assert_eq!(g.stats().compiles, 1);
        assert_eq!(g.stats().passes, 3);
    }

    /// Every read extends its target's pooled lifetime, and the surface's reads
    /// go in through the pool's sentinel so the pool resolves them to the final
    /// slot. Observable through the pool's peak occupancy: two chained rungs can
    /// alias nothing here, since both are read after they are written.
    #[test]
    fn reads_drive_the_pooled_lifetimes() {
        let (mut pool, t) = targets(2);
        let mut g = RenderGraph::new();
        g.begin_frame();
        g.open(PassWork::Offscreen(0), Some(t[0]));
        let blur = g.open(PassWork::Blur(0), Some(t[1]));
        g.read(blur, t[0]);
        let surface = g.open(PassWork::Surface, None);
        g.read(surface, t[1]);
        g.compile();
        g.apply_lifetimes(&mut pool);

        // t[0] dies at the blur's slot (1), t[1] at the surface's (2).
        assert_eq!(g.surface_slot(), 2);
        let mut gpu = viso_gpu::HeadlessRaster::new();
        let sampler =
            viso_gpu::GpuBackend::create_sampler(&mut gpu, &viso_gpu::SamplerDesc::LINEAR_CLAMP);
        pool.assign(&mut gpu, sampler, g.surface_slot());
        assert_eq!(
            pool.stats().targets,
            2,
            "overlapping lifetimes cannot share one physical target"
        );
    }

    /// Sampling a target no pass writes is a graph bug, not a silent no-op.
    #[test]
    #[should_panic(expected = "samples a target no pass writes")]
    #[cfg(debug_assertions)]
    fn reading_an_unwritten_target_is_rejected() {
        let (_pool, t) = targets(1);
        let mut g = RenderGraph::new();
        g.begin_frame();
        let surface = g.open(PassWork::Surface, None);
        g.read(surface, t[0]);
        g.compile();
    }

    /// Sampling one's own attachment is a read-write hazard no ordering fixes.
    #[test]
    #[should_panic(expected = "written at or after it")]
    #[cfg(debug_assertions)]
    fn reading_its_own_attachment_is_rejected() {
        let (_pool, t) = targets(1);
        let mut g = RenderGraph::new();
        g.begin_frame();
        let pass = g.open(PassWork::Blur(0), Some(t[0]));
        g.read(pass, t[0]);
        g.open(PassWork::Surface, None);
        g.compile();
    }
}
