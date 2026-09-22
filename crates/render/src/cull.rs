//! Which primitives a frame tests for visibility, and who tests them (§20, §24.1).
//!
//! Culling is a cost, not a free win. Testing one primitive's bounds against the
//! viewport is a handful of compares, so for an ordinary UI frame — a window of
//! widgets, every one of them on screen — the per-primitive test *is* the optimal
//! algorithm: it visits each primitive once, rejects almost nothing, and any
//! structure built to speed it up would cost more to build than the test saves.
//!
//! §20 and §24.1 name two escalations, and both are entered by scene shape rather
//! than by ambition:
//!
//! - **Chunk-level CPU cull.** Once a scene is far larger than its viewport — a map,
//!   a node graph, a timeline, a document at zoom — most primitives are off screen,
//!   and the per-primitive walk spends its whole budget proving that. Binning
//!   primitives into a coarse grid lets one compare reject a whole chunk, so the
//!   frame's cost tracks what is *visible* rather than what exists. This module
//!   implements it ([`ChunkedCull`]); it needs no GPU capability at all.
//! - **GPU cull / indirect draw.** Where the scene is large enough that even
//!   chunk-level rejection is a measurable share of the frame, the visibility test
//!   itself can move to a compute dispatch writing draw arguments the GPU then
//!   consumes. This module only *selects* that plan; no dispatch ships here,
//!   because no backend in this repository exposes one (§20.1).
//!
//! The hard rule §20 states is the one this module exists to make arithmetic: **50
//! UI nodes must never produce a compute dispatch for "GPU-driven"**. So the
//! decision is a conjunction of capability, scale, and *measured* cull cost, and
//! every incomplete description of a frame lands on [`CullPlan::PerPrimitive`] —
//! which is also what the renderer does today, so ordinary UI has no dependency on
//! anything here (§7.2).
//!
//! The thresholds are internal benchmark parameters (§20 fixes no ABI). What is
//! stable is the shape: a capability veto, a scene far larger than its viewport, and
//! a cull cost large enough to be worth moving.

use crate::Rect;

/// How a frame decides which primitives to submit (§20, §24.1).
///
/// Ordered by the scene size each answers, not by sophistication. All three submit
/// the same visible set; they differ only in how much work proving that costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CullPlan {
    /// Test every primitive's bounds against the clip, on the CPU, as they are
    /// lowered.
    ///
    /// The default and what this renderer does. Optimal whenever the scene is
    /// roughly viewport-sized, which is every ordinary UI frame: the walk has to
    /// visit each primitive anyway to lower it, so the test is nearly free and no
    /// acceleration structure can beat it.
    #[default]
    PerPrimitive,
    /// Bin primitives into a coarse grid and reject whole chunks before testing
    /// their members ([`ChunkedCull`]).
    ///
    /// For a scene much larger than its viewport, where the per-primitive walk's
    /// cost is dominated by primitives that were never going to be drawn. Pure CPU,
    /// so it is available on every backend — which is why it is the escalation that
    /// actually ships.
    Chunked,
    /// A compute dispatch tests visibility and writes the draw arguments the GPU
    /// consumes through indirect draw (§24.1).
    ///
    /// For a scene large enough that chunk-level rejection is itself a measurable
    /// share of the frame. Selected only against a backend that has both dispatch
    /// and indirect draw; not realized here, since no backend does.
    GpuIndirect,
}

/// Primitives a frame can hold before a coarse rejection structure could pay for
/// itself.
///
/// A dense UI screen is a few hundred primitives and a busy one a few thousand;
/// below this floor the grid's own build cost exceeds the per-primitive tests it
/// would skip. Internal benchmark parameter (§20).
const SMALL_SCENE: u32 = 2_048;

/// Share of a scene's primitives that must fall outside the viewport before coarse
/// rejection has anything to reject.
///
/// This is the condition that separates "large scene" from "large *canvas*". A
/// hundred thousand primitives that are all visible must still all be submitted,
/// and no culling plan changes that — the frame is fill-bound, not cull-bound.
/// Internal benchmark parameter (§20).
const MOSTLY_OFFSCREEN: f32 = 0.5;

/// Primitives below which moving the visibility test onto the GPU cannot pay for a
/// dispatch, a barrier, and a buffer round trip.
///
/// Far above [`SMALL_SCENE`]: the CPU chunk walk is cheap and scales well, so the
/// crossover is where per-chunk bookkeeping stops fitting in the frame's slack, not
/// where the scene merely stops being small. Internal benchmark parameter (§24.1).
const HUGE_SCENE: u32 = 100_000;

/// Share of frame CPU time spent culling before the test is worth moving to the GPU.
///
/// The §7.3 gate in type form: this defaults to `0.0`, so an unprofiled frame and a
/// frame where culling is not the limit are the same input and neither can reach
/// [`CullPlan::GpuIndirect`] at any scale. Internal benchmark parameter (§24.1).
const CULL_DOMINATES: f32 = 0.2;

/// What a frame asks of the culling path, as the §20 / §24.1 entry conditions name
/// it.
///
/// Every field but the two capabilities is a property of the *scene*, so the
/// decision is portable: a backend without dispatch simply never offers the GPU
/// plan, and the same scene keeps rendering through the chunked or per-primitive
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CullWorkload {
    /// Whether the backend can consume draw arguments from a buffer
    /// ([`viso_gpu::Caps::indirect_draw`]).
    pub indirect_draw: bool,
    /// Whether the backend can dispatch the kernel that would write them
    /// ([`viso_gpu::Caps::compute_dispatch`]).
    pub compute_dispatch: bool,
    /// Primitives in the scene, visible or not — the work a per-primitive walk has
    /// to do.
    pub primitives: u32,
    /// Fraction of those primitives whose bounds fall outside the viewport, in
    /// `[0, 1]`. The rejection a coarse structure could make in bulk.
    pub offscreen_share: f32,
    /// Fraction of the frame's CPU time spent deciding visibility, in `[0, 1]`.
    /// Measured, not assumed (§7.3); `0.0` means nobody profiled it.
    pub cull_share: f32,
}

impl CullWorkload {
    /// Whether the scene is past the size where a coarse structure could pay for
    /// its own construction.
    pub fn scene_is_large(&self) -> bool {
        self.primitives > SMALL_SCENE
    }

    /// Whether enough of the scene is off screen for bulk rejection to have
    /// anything to reject.
    ///
    /// Separate from the size because the two come apart: a large scene that is
    /// entirely visible has nothing to cull, and a plan that reorganized it would
    /// add cost for no rejection.
    pub fn mostly_offscreen(&self) -> bool {
        self.offscreen_share >= MOSTLY_OFFSCREEN
    }

    /// Whether culling is *measurably* a limit on this frame — the §7.3 gate.
    pub fn culling_is_the_bottleneck(&self) -> bool {
        self.cull_share >= CULL_DOMINATES
    }

    /// Whether the scene is large enough that a dispatch plus an indirect round
    /// trip could be amortized.
    pub fn scene_is_huge(&self) -> bool {
        self.primitives > HUGE_SCENE
    }

    /// Whether the backend can actually run a GPU cull: it takes both halves — a
    /// kernel to write the arguments and a draw that reads them. Either alone is
    /// not the capability (§17.1).
    pub fn gpu_driven_is_available(&self) -> bool {
        self.indirect_draw && self.compute_dispatch
    }
}

impl CullPlan {
    /// Pick the culling plan for a scene (§20, §24.1).
    ///
    /// Conjunctive at both escalation steps, and checked from the most demanding
    /// down. The GPU plan needs the capability *and* a huge scene *and* most of it
    /// off screen *and* culling measured as a real share of the frame; the chunked
    /// plan needs a large scene *and* most of it off screen. Anything else is
    /// [`CullPlan::PerPrimitive`].
    ///
    /// The fall-through is the point: an all-default [`CullWorkload`] — which is
    /// what an undescribed frame is — selects the plan that needs no capability and
    /// no structure, so 50 UI nodes cannot reach a dispatch however capable the
    /// backend claims to be (§20's hard rule, §7.2).
    pub fn select(workload: CullWorkload) -> CullPlan {
        if workload.gpu_driven_is_available()
            && workload.scene_is_huge()
            && workload.mostly_offscreen()
            && workload.culling_is_the_bottleneck()
        {
            CullPlan::GpuIndirect
        } else if workload.scene_is_large() && workload.mostly_offscreen() {
            CullPlan::Chunked
        } else {
            CullPlan::PerPrimitive
        }
    }

    /// Whether this plan dispatches a compute kernel to decide visibility.
    ///
    /// The frame-level form of §20's hard rule: this is `false` for both plans that
    /// ship, so a scene of ordinary UI nodes provably issues no dispatch.
    pub fn dispatches_compute(self) -> bool {
        matches!(self, CullPlan::GpuIndirect)
    }
}

/// A uniform grid of chunks over a scene's bounds, the index [`ChunkedCull`] bins
/// into.
///
/// Uniform rather than hierarchical on purpose: the scenes this serves are broadly
/// even in density (a map, a node graph, a timeline), one grid level already turns
/// the frame's cost from scene-sized into viewport-sized, and a flat grid costs one
/// multiply-add to look up with no pointer chasing (§43). A quadtree's extra levels
/// buy their keep only under extreme clustering, which is a later question with its
/// own benchmark.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkGrid {
    /// Top-left corner of chunk `(0, 0)`, in the scene's physical-pixel space.
    origin: [f32; 2],
    /// Edge length of one square chunk, always `> 0`.
    cell: f32,
    /// Chunks across, always `>= 1`.
    cols: u32,
    /// Chunks down, always `>= 1`.
    rows: u32,
}

/// Primitives a chunk should hold, on average.
///
/// The tradeoff a chunk size is: large chunks reject rarely, small chunks cost more
/// per-chunk tests and more memory. A few dozen keeps the per-chunk test amortized
/// over enough members to be worth making while staying fine enough that a chunk is
/// usually entirely in or entirely out of the viewport. Internal benchmark
/// parameter (§20).
const PRIMITIVES_PER_CHUNK: u32 = 64;

impl ChunkGrid {
    /// A grid over `bounds` sized so a scene of `primitives` averages
    /// [`PRIMITIVES_PER_CHUNK`] per chunk.
    ///
    /// Degenerate input is handled by construction rather than by assertion: an
    /// empty or non-finite `bounds`, or a scene of nothing, yields a one-cell grid,
    /// which makes [`ChunkedCull`] equivalent to the per-primitive walk instead of
    /// wrong.
    pub fn over(bounds: Rect, primitives: u32) -> ChunkGrid {
        let w = bounds.w.max(1.0);
        let h = bounds.h.max(1.0);
        let chunks = (primitives / PRIMITIVES_PER_CHUNK).max(1);
        // Square cells covering `bounds` in about `chunks` of them: the area per
        // chunk is `w * h / chunks`, so its edge is that area's square root.
        let cell = (w * h / chunks as f32).sqrt().max(1.0);
        let cols = (w / cell).ceil().max(1.0) as u32;
        let rows = (h / cell).ceil().max(1.0) as u32;
        ChunkGrid {
            origin: [bounds.x, bounds.y],
            cell,
            cols,
            rows,
        }
    }

    /// How many chunks the grid holds.
    pub fn len(&self) -> usize {
        self.cols as usize * self.rows as usize
    }

    /// A grid always holds at least one chunk, so this is never true. Present
    /// because [`len`](Self::len) is.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Chunks across and down.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.cols, self.rows)
    }

    /// The chunk a point falls in, clamped into the grid.
    ///
    /// Clamping rather than rejecting is what keeps the cull *conservative*: a
    /// primitive outside the grid's bounds still lands in some chunk, that chunk's
    /// union grows to contain it, and it is therefore still tested. A cull that
    /// silently dropped it would be a correctness bug, not a faster cull.
    fn chunk_of(&self, x: f32, y: f32) -> usize {
        let col = (((x - self.origin[0]) / self.cell) as i64).clamp(0, self.cols as i64 - 1);
        let row = (((y - self.origin[1]) / self.cell) as i64).clamp(0, self.rows as i64 - 1);
        row as usize * self.cols as usize + col as usize
    }
}

/// What a chunked cull cost and returned, so the escalation can be justified rather
/// than assumed (§7.3, §30).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CullOutcome {
    /// Chunks rejected whole, without testing any member.
    pub chunks_rejected: u32,
    /// Primitives whose own bounds were tested — the work the per-primitive walk
    /// would have done for the entire scene.
    pub primitives_tested: u32,
    /// Primitives that survived and must be submitted.
    pub primitives_visible: u32,
}

/// A chunk-level visibility index over a scene, in paint order.
///
/// Built once per scene change (cold), queried once per frame against the viewport.
/// The query walks chunks, skips every chunk whose union of member bounds misses the
/// viewport, and tests members only inside the survivors — so a frame's cost tracks
/// the visible region rather than the scene.
///
/// **Paint order is preserved.** The visible set comes back ascending in primitive
/// index, because a culling structure that reordered submission would break z-order
/// (§16.2) — a far worse bug than a slow frame.
#[derive(Debug, Clone, Default)]
pub struct ChunkedCull {
    /// The grid, or `None` for an empty scene.
    grid: Option<ChunkGrid>,
    /// Union of the bounds of each chunk's members; [`Rect::ZERO`] for a chunk with
    /// none. Indexed by chunk.
    chunk_bounds: Vec<Rect>,
    /// Primitive indices grouped by chunk, each group ascending: `chunk_start[c]`
    /// until `chunk_start[c + 1]` are chunk `c`'s members. A flat pair of arrays
    /// rather than a `Vec<Vec<_>>`, so building it is two passes over one allocation
    /// apiece and querying it never chases a pointer (§28, §43).
    members: Vec<u32>,
    /// Prefix offsets into [`members`](Self::members), `chunk_bounds.len() + 1` long.
    chunk_start: Vec<u32>,
    /// Write cursors for the placement pass, kept as a field so a rebuild reuses the
    /// allocation instead of cloning `chunk_start` every time (§28).
    cursor: Vec<u32>,
}

impl ChunkedCull {
    /// Bin `bounds` — one rect per primitive, in paint order — into a grid sized for
    /// the scene.
    ///
    /// Reuses this instance's allocations, so a scene that changes every frame does
    /// not re-allocate (§28). The grid is derived from the scene's own extent, so a
    /// caller never picks a chunk size.
    pub fn build(&mut self, bounds: &[Rect]) {
        self.members.clear();
        self.chunk_bounds.clear();
        self.chunk_start.clear();

        if bounds.is_empty() {
            self.grid = None;
            return;
        }

        let extent = bounds.iter().fold(Rect::ZERO, |acc, b| acc.union(*b));
        let grid = ChunkGrid::over(extent, bounds.len() as u32);
        self.grid = Some(grid);

        // Counting sort by chunk: count members, prefix-sum into offsets, then place.
        // Two passes and no per-chunk Vec, and it yields each chunk's members already
        // ascending in primitive index — which is what preserves paint order.
        self.chunk_bounds.resize(grid.len(), Rect::ZERO);
        self.chunk_start.resize(grid.len() + 1, 0);
        for b in bounds {
            let c = grid.chunk_of(b.x, b.y);
            self.chunk_start[c + 1] += 1;
            self.chunk_bounds[c] = self.chunk_bounds[c].union(*b);
        }
        for c in 0..grid.len() {
            self.chunk_start[c + 1] += self.chunk_start[c];
        }
        self.members.resize(bounds.len(), 0);
        self.cursor.clear();
        self.cursor.extend_from_slice(&self.chunk_start);
        for (i, b) in bounds.iter().enumerate() {
            let c = grid.chunk_of(b.x, b.y);
            self.members[self.cursor[c] as usize] = i as u32;
            self.cursor[c] += 1;
        }
    }

    /// The grid this index was built over, or `None` for an empty scene.
    pub fn grid(&self) -> Option<ChunkGrid> {
        self.grid
    }

    /// Append the primitives visible in `viewport` to `visible`, ascending in paint
    /// order, and report what the query cost.
    ///
    /// `bounds` must be the same slice [`build`](Self::build) was given; the index
    /// stores no copy of it, because the caller already owns one and a second would
    /// be pure memory (§28).
    pub fn cull(&self, bounds: &[Rect], viewport: Rect, visible: &mut Vec<u32>) -> CullOutcome {
        let mut outcome = CullOutcome::default();
        let Some(grid) = self.grid else {
            return outcome;
        };
        let appended_from = visible.len();

        for c in 0..grid.len() {
            let start = self.chunk_start[c] as usize;
            let end = self.chunk_start[c + 1] as usize;
            if start == end {
                continue;
            }
            // One compare rejects every member of this chunk. This is the whole win:
            // the members are never touched, so they cost nothing but the memory
            // they sit in.
            if !overlaps(self.chunk_bounds[c], viewport) {
                outcome.chunks_rejected += 1;
                continue;
            }
            for &i in &self.members[start..end] {
                outcome.primitives_tested += 1;
                if overlaps(bounds[i as usize], viewport) {
                    outcome.primitives_visible += 1;
                    visible.push(i);
                }
            }
        }
        // Chunks are walked in row-major order, so their members interleave in the
        // output; paint order is a property of submission, so restore it here rather
        // than making every caller remember to. Only what this call appended is
        // sorted — whatever the caller had already collected is theirs.
        visible[appended_from..].sort_unstable();
        outcome
    }
}

/// Whether two rects share any area. Empty rects overlap nothing, including
/// themselves — a zero-area primitive draws nothing, so culling it is free and
/// correct.
fn overlaps(a: Rect, b: Rect) -> bool {
    let i = a.intersect(b);
    i.w > 0.0 && i.h > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The workload §24.1 names GPU culling for: a huge, mostly-offscreen scene on a
    /// backend with both halves of the capability, with culling profiled as a real
    /// share of the frame.
    fn a_zoomed_out_million_node_graph() -> CullWorkload {
        CullWorkload {
            indirect_draw: true,
            compute_dispatch: true,
            primitives: 1_000_000,
            offscreen_share: 0.99,
            cull_share: 0.4,
        }
    }

    /// A large canvas whose viewport shows a small part of it: the case chunk-level
    /// CPU rejection is for, and it needs no capability.
    fn a_scrolled_document() -> CullWorkload {
        CullWorkload {
            primitives: 40_000,
            offscreen_share: 0.95,
            ..CullWorkload::default()
        }
    }

    #[test]
    fn the_plan_needing_nothing_is_the_default_one() {
        assert_eq!(CullPlan::default(), CullPlan::PerPrimitive);
        assert_eq!(
            CullPlan::select(CullWorkload::default()),
            CullPlan::PerPrimitive,
            "a scene nobody described must not require a culling structure"
        );
        assert!(!CullPlan::PerPrimitive.dispatches_compute());
        assert!(!CullPlan::Chunked.dispatches_compute());
    }

    #[test]
    fn a_scrolled_canvas_escalates_to_chunks_without_any_capability() {
        let plan = CullPlan::select(a_scrolled_document());
        assert_eq!(plan, CullPlan::Chunked);
        assert!(
            !plan.dispatches_compute(),
            "the escalation that ships must not need a GPU feature"
        );
    }

    #[test]
    fn a_huge_offscreen_scene_is_what_gpu_culling_is_for() {
        assert_eq!(
            CullPlan::select(a_zoomed_out_million_node_graph()),
            CullPlan::GpuIndirect
        );
    }

    /// Each GPU condition alone falls back, so the conjunction cannot decay into a
    /// disjunction. Dropping scale falls all the way to per-primitive; dropping a
    /// capability or the measurement falls to the CPU chunk plan, which still fits
    /// the scene.
    #[test]
    fn every_gpu_condition_is_individually_necessary() {
        let base = a_zoomed_out_million_node_graph();
        for (missing, workload, expect) in [
            (
                "the backend cannot consume indirect arguments",
                CullWorkload {
                    indirect_draw: false,
                    ..base
                },
                CullPlan::Chunked,
            ),
            (
                "the backend cannot dispatch the kernel",
                CullWorkload {
                    compute_dispatch: false,
                    ..base
                },
                CullPlan::Chunked,
            ),
            (
                "nobody measured the cull cost",
                CullWorkload {
                    cull_share: 0.0,
                    ..base
                },
                CullPlan::Chunked,
            ),
            (
                "the scene is not huge",
                CullWorkload {
                    primitives: HUGE_SCENE,
                    ..base
                },
                CullPlan::Chunked,
            ),
            (
                "the scene is entirely visible",
                CullWorkload {
                    offscreen_share: 0.0,
                    ..base
                },
                CullPlan::PerPrimitive,
            ),
        ] {
            assert_eq!(
                CullPlan::select(workload),
                expect,
                "{missing}: the GPU plan must not be selected"
            );
        }
    }

    /// §20's hard rule, in the form it is stated: 50 UI nodes never reach a
    /// dispatch, under the most favorable conditions a backend could report.
    #[test]
    fn fifty_ui_nodes_are_never_gpu_driven() {
        for (label, primitives) in [
            ("fifty UI nodes", 50),
            ("a dense screen", 800),
            ("a busy screen with text", SMALL_SCENE),
        ] {
            let frame = CullWorkload {
                indirect_draw: true,
                compute_dispatch: true,
                primitives,
                offscreen_share: 1.0,
                cull_share: 1.0,
            };
            let plan = CullPlan::select(frame);
            assert_eq!(
                plan,
                CullPlan::PerPrimitive,
                "{label} must stay on the plain bounds test"
            );
            assert!(!plan.dispatches_compute());
        }
    }

    /// A large scene that is fully visible is fill-bound, not cull-bound: no plan
    /// can reject anything, so none is selected.
    #[test]
    fn a_large_but_fully_visible_scene_has_nothing_to_cull() {
        let visible = CullWorkload {
            offscreen_share: 0.1,
            ..a_scrolled_document()
        };
        assert!(visible.scene_is_large());
        assert!(!visible.mostly_offscreen());
        assert_eq!(CullPlan::select(visible), CullPlan::PerPrimitive);
    }

    fn rect(x: f32, y: f32) -> Rect {
        Rect {
            x,
            y,
            w: 10.0,
            h: 10.0,
        }
    }

    /// A scene laid out as a wide grid of cells, the shape a canvas/document takes.
    fn grid_scene(cols: u32, rows: u32) -> Vec<Rect> {
        (0..rows)
            .flat_map(|r| (0..cols).map(move |c| rect(c as f32 * 12.0, r as f32 * 12.0)))
            .collect()
    }

    /// The reference answer: the per-primitive walk the chunked index must agree
    /// with, exactly.
    fn brute_force(bounds: &[Rect], viewport: Rect) -> Vec<u32> {
        bounds
            .iter()
            .enumerate()
            .filter(|(_, b)| overlaps(**b, viewport))
            .map(|(i, _)| i as u32)
            .collect()
    }

    /// The contract that matters more than any speedup: the chunked cull returns
    /// exactly the per-primitive walk's answer, in paint order, for every viewport —
    /// inside, straddling a chunk seam, at a corner, covering everything, and
    /// missing everything.
    #[test]
    fn a_chunked_cull_agrees_with_the_per_primitive_walk() {
        let bounds = grid_scene(60, 40);
        let mut index = ChunkedCull::default();
        index.build(&bounds);

        for (label, viewport) in [
            (
                "a small interior window",
                Rect {
                    x: 100.0,
                    y: 100.0,
                    w: 90.0,
                    h: 70.0,
                },
            ),
            (
                "straddling chunk seams",
                Rect {
                    x: 37.0,
                    y: 41.0,
                    w: 200.0,
                    h: 150.0,
                },
            ),
            (
                "the top-left corner",
                Rect {
                    x: -50.0,
                    y: -50.0,
                    w: 80.0,
                    h: 80.0,
                },
            ),
            (
                "the whole scene",
                Rect {
                    x: -10.0,
                    y: -10.0,
                    w: 10_000.0,
                    h: 10_000.0,
                },
            ),
            (
                "far off the scene",
                Rect {
                    x: 50_000.0,
                    y: 50_000.0,
                    w: 100.0,
                    h: 100.0,
                },
            ),
            (
                "one primitive wide",
                Rect {
                    x: 12.0,
                    y: 12.0,
                    w: 1.0,
                    h: 1.0,
                },
            ),
        ] {
            let mut visible = Vec::new();
            let outcome = index.cull(&bounds, viewport, &mut visible);
            assert_eq!(
                visible,
                brute_force(&bounds, viewport),
                "{label}: the chunked answer must be the exact answer"
            );
            assert_eq!(outcome.primitives_visible as usize, visible.len());
            assert!(
                visible.windows(2).all(|w| w[0] < w[1]),
                "{label}: the visible set must stay in paint order"
            );
        }
    }

    /// The win, measured: a small window into a large scene tests a small fraction
    /// of it, because whole chunks are rejected untouched. This is the only reason
    /// the structure exists, so it is asserted rather than described.
    #[test]
    fn a_small_viewport_tests_a_small_fraction_of_a_large_scene() {
        let bounds = grid_scene(100, 100);
        let mut index = ChunkedCull::default();
        index.build(&bounds);

        let mut visible = Vec::new();
        let outcome = index.cull(
            &bounds,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 120.0,
                h: 120.0,
            },
            &mut visible,
        );
        assert_eq!(visible.len(), 100, "a 10x10 window of cells is visible");
        assert!(outcome.chunks_rejected > 0, "whole chunks were rejected");
        assert!(
            outcome.primitives_tested * 10 < bounds.len() as u32,
            "tested {} of {} primitives — a coarse reject that tests most of the \
             scene anyway is not worth building",
            outcome.primitives_tested,
            bounds.len()
        );
    }

    /// Degenerate scenes are handled by construction: nothing to bin, everything at
    /// one point, and a single primitive all produce a usable index rather than a
    /// panic or a wrong answer.
    #[test]
    fn degenerate_scenes_build_a_usable_index() {
        let mut index = ChunkedCull::default();
        let mut visible = Vec::new();

        index.build(&[]);
        assert!(index.grid().is_none());
        assert_eq!(
            index.cull(&[], Rect::INFINITE, &mut visible),
            CullOutcome::default()
        );
        assert!(visible.is_empty());

        // A thousand primitives stacked at one point: the extent is one cell wide, so
        // the grid collapses, and the cull degrades to the per-primitive walk.
        let stacked = vec![rect(5.0, 5.0); 1_000];
        index.build(&stacked);
        let outcome = index.cull(&stacked, rect(0.0, 0.0), &mut visible);
        assert_eq!(outcome.primitives_visible, 1_000);
        assert_eq!(visible.len(), 1_000);

        visible.clear();
        let one = [rect(0.0, 0.0)];
        index.build(&one);
        assert_eq!(index.grid().unwrap().len(), 1);
        assert_eq!(
            index
                .cull(&one, rect(0.0, 0.0), &mut visible)
                .primitives_visible,
            1
        );
    }

    /// An empty primitive covers no pixels, so it is culled everywhere — including
    /// against a viewport containing its origin. Keeping it would submit a draw that
    /// paints nothing.
    #[test]
    fn zero_area_primitives_are_culled_everywhere() {
        let bounds = [
            Rect {
                x: 10.0,
                y: 10.0,
                w: 0.0,
                h: 10.0,
            },
            rect(10.0, 10.0),
        ];
        let mut index = ChunkedCull::default();
        index.build(&bounds);
        let mut visible = Vec::new();
        index.cull(&bounds, Rect::INFINITE, &mut visible);
        assert_eq!(visible, vec![1], "only the primitive with area survives");
    }

    /// Rebuilding reuses the index's buffers: a scene that changes shape every frame
    /// must not re-allocate, and must not carry the previous scene's members.
    #[test]
    fn rebuilding_reuses_buffers_and_keeps_no_stale_members() {
        let mut index = ChunkedCull::default();
        let big = grid_scene(40, 40);
        index.build(&big);

        let small = grid_scene(3, 3);
        index.build(&small);
        let mut visible = Vec::new();
        let outcome = index.cull(&small, Rect::INFINITE, &mut visible);
        assert_eq!(outcome.primitives_visible, 9);
        assert_eq!(visible, (0..9).collect::<Vec<u32>>());
    }
}
