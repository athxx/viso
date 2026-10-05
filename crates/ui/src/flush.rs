//! The frame's state flush: the pending writes fanned through every reactor
//! downstream of state, repeated until no reactor writes more.
//!
//! One round drains the pending set once and runs, in order, the memo-gated
//! derivations, the direct bindings, the structure hooks, the semantic-state
//! projections and the effects. A structure hook may write a cell while it
//! mounts (a region's instance state), so the flush runs another round over
//! those writes in the same frame; the frame settles when a round leaves
//! nothing pending. A round whose writes keep causing writes is a reactive
//! cycle: past [`SETTLE_ROUNDS`] the flush stops and drops what is still
//! pending, so a cycle costs a bounded amount of one frame rather than every
//! frame after it.
//!
//! A layout then settles the adaptive environment against the boxes it placed:
//! a value that moved wakes its readers, the states they write settle, and the
//! tree lays out again, until no anchor moves. A structure that keeps moving
//! the constraints that select it is an adaptive cycle: past
//! [`ADAPTIVE_ROUNDS`] the frame keeps the last structure.

use crate::binding::BindingTable;
use crate::component::NodeStore;
use crate::node::NodeId;
use crate::reactive::{ComputedStore, EffectStore, SemanticProjector};
use crate::state::{StateId, StateStore};
use crate::structure::run_structure_hooks;

/// How many rounds one frame's flush runs before it stops as a reactive cycle.
pub const SETTLE_ROUNDS: u32 = 16;

/// How many layouts one frame's adaptive settle runs before it stops as an
/// adaptive cycle.
pub const ADAPTIVE_ROUNDS: u32 = 8;

/// A frame whose flush did not settle within [`SETTLE_ROUNDS`] rounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactiveCycle {
    /// How many written cells were still pending, and were dropped, when the
    /// flush stopped. Their values stay written; only the reactions to them
    /// do not run.
    pub dropped: usize,
    /// The dropped cells, the chain's last writes.
    pub cells: Vec<StateId>,
    /// The nodes of the effects those cells would have run again: the effects
    /// on the chain.
    pub effects: Vec<NodeId>,
}

impl ReactiveCycle {
    /// The diagnostic code of a reactive cycle.
    pub const CODE: &'static str = "E4202";
}

impl std::fmt::Display for ReactiveCycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: state writes did not settle within {SETTLE_ROUNDS} rounds; \
             dropped the changes of {} cell(s) {:?}",
            Self::CODE,
            self.dropped,
            self.cells
        )?;
        if !self.effects.is_empty() {
            write!(f, ", which re-run the effects of nodes {:?}", self.effects)?;
        }
        Ok(())
    }
}

/// Flushes the pending state writes, and runs the effects builds registered,
/// until they settle and returns how many rounds ran: `0` for a frame with
/// nothing pending, which touches nothing.
/// `changed` is reused scratch for each round's drained set, left empty.
///
/// Each round runs, over the cells written since the last:
///
/// 1. the derivations, first, so a memo-gated re-evaluation dirties its node
///    only when its value changed, before Measure/Layout;
/// 2. the direct bindings, turning each changed cell into targeted node
///    dirtying through the compiled static and dynamic-script edges;
/// 3. the structure hooks, after the bindings so a node a hook frees has taken
///    its marks;
/// 4. the semantic-state projections, writing each node's `semantic_state`;
/// 5. the effects: first runs of those mounted since the last round, then
///    re-runs of those whose dependencies changed. An effect's writes are
///    the next round's.
///
/// # Errors
///
/// A [`ReactiveCycle`] when round [`SETTLE_ROUNDS`] still leaves writes
/// pending; they are dropped.
pub fn settle_states(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    computeds: &mut ComputedStore,
    projectors: &mut SemanticProjector,
    effects: &mut EffectStore,
    changed: &mut Vec<StateId>,
) -> Result<u32, ReactiveCycle> {
    let mut rounds = 0;
    store.sync_interactions(states);
    while states.has_pending() || store.has_pending_effects() {
        changed.clear();
        states.take_pending(changed);
        if rounds == SETTLE_ROUNDS {
            let cells = std::mem::take(changed);
            let mut chain = Vec::new();
            effects.woken_by(&cells, &mut chain);
            return Err(ReactiveCycle {
                dropped: cells.len(),
                cells,
                effects: chain,
            });
        }
        rounds += 1;
        computeds.wake_computed(changed, states, store);
        store.flush_state_transactions(changed, bindings);
        run_structure_hooks(store, states, bindings, effects, changed);
        projectors.wake(changed, states, store);
        // The effects the commit mounted run first, then those it woke.
        effects.adopt(store, states);
        effects.wake(changed, states);
        let tasks = effects.take_task_ops();
        if !tasks.is_empty() {
            store.apply_owned_task_ops(tasks);
        }
    }
    changed.clear();
    Ok(rounds)
}

/// A frame whose layout did not settle the adaptive environment within
/// [`ADAPTIVE_ROUNDS`] layouts: the structure the environment selects keeps
/// moving the constraints that select it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptiveCycle {
    /// How many environment cells the last settle raised, and were dropped.
    /// The anchors hold their new values; only the reactions to them do not
    /// run, so the frame keeps its last structure.
    pub dropped: usize,
}

impl AdaptiveCycle {
    /// The diagnostic code of an adaptive cycle.
    pub const CODE: &'static str = "E4204";
}

impl std::fmt::Display for AdaptiveCycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: the adaptive environment did not settle within {ADAPTIVE_ROUNDS} \
             layouts; kept the last structure and dropped the changes of {} cell(s)",
            Self::CODE,
            self.dropped
        )
    }
}

/// Why a frame's layout stopped before it settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsettled {
    /// A state flush inside the settle did not settle.
    Reactive(ReactiveCycle),
    /// The environment did not settle.
    Adaptive(AdaptiveCycle),
}

impl std::fmt::Display for Unsettled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unsettled::Reactive(cycle) => cycle.fmt(f),
            Unsettled::Adaptive(cycle) => cycle.fmt(f),
        }
    }
}

/// Settles the adaptive environment against the layout that just ran: pads
/// every avoiding region by what the system now covers of it, then resolves
/// every anchor, and while either moves something, settles the states it
/// wakes and lays out again through `relayout`, which returns whether it
/// placed anything. Anchors resolve only once the regions hold still, so they
/// read the boxes the frame presents. `laid_out` is whether the layout before
/// the call placed anything; a frame that placed nothing and changed no
/// environment input resolves nothing. Returns how many layouts it ran.
#[allow(clippy::too_many_arguments)]
pub fn settle_adaptive(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    computeds: &mut ComputedStore,
    projectors: &mut SemanticProjector,
    effects: &mut EffectStore,
    changed: &mut Vec<StateId>,
    mut laid_out: bool,
    mut relayout: impl FnMut(&mut NodeStore) -> bool,
) -> Result<u32, Unsettled> {
    let mut rounds = 0;
    while laid_out || states.env().unsettled() {
        let padded = states.pad_avoiding(store);
        let moved = !padded && states.settle_env(store);
        if !padded && !moved {
            break;
        }
        if rounds == ADAPTIVE_ROUNDS {
            changed.clear();
            states.take_pending(changed);
            let dropped = changed.len();
            changed.clear();
            return Err(Unsettled::Adaptive(AdaptiveCycle { dropped }));
        }
        rounds += 1;
        if moved {
            settle_states(
                store, states, bindings, computeds, projectors, effects, changed,
            )
            .map_err(Unsettled::Reactive)?;
        }
        laid_out = relayout(store);
    }
    Ok(rounds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adaptive::{AnchorId, Avoid, EnvField, SizeClass};
    use crate::component::{BuildCx, FlexStyle, LeafStyle};
    use crate::dirty::DirtyClass;
    use crate::layout::{Axis, Inset, Length, Size};
    use crate::node::NodeId;
    use crate::state::StateValue;
    use viso_render::Rect;

    #[derive(Default)]
    struct Stores {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        computeds: ComputedStore,
        projectors: SemanticProjector,
        effects: EffectStore,
        changed: Vec<StateId>,
    }

    impl Stores {
        fn settle(&mut self) -> Result<u32, ReactiveCycle> {
            settle_states(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.computeds,
                &mut self.projectors,
                &mut self.effects,
                &mut self.changed,
            )
        }
    }

    fn int(states: &StateStore, id: StateId) -> i32 {
        match states.get(id) {
            Some(StateValue::Int(n)) => n,
            other => panic!("an int cell, not {other:?}"),
        }
    }

    #[test]
    fn a_registered_effect_first_runs_in_the_next_settle_and_its_writes_settle_too() {
        let mut stores = Stores::default();
        let count = stores.states.alloc(StateValue::Int(3));
        let doubled = stores.states.alloc(StateValue::Int(0));
        let root = BuildCx::new(&mut stores.store)
            .leaf(LeafStyle::default())
            .id();
        let mut last = None;
        stores.store.add_effect(
            root,
            move |cx| {
                let now = cx.get(count);
                last.replace(now) != Some(now)
            },
            move |cx| {
                let Some(StateValue::Int(n)) = cx.get(count) else {
                    return None;
                };
                cx.set(doubled, StateValue::Int(n * 2));
                None
            },
        );
        assert!(stores.store.has_pending_effects());
        // Nothing is written, yet the mount runs, and its write settles.
        assert_eq!(stores.settle(), Ok(2));
        assert_eq!(int(&stores.states, doubled), 6);
        assert!(!stores.store.has_pending_effects());
        stores.states.set(count, StateValue::Int(4));
        assert_eq!(stores.settle(), Ok(2));
        assert_eq!(int(&stores.states, doubled), 8);
    }

    #[test]
    fn an_effect_feeding_itself_stops_at_the_settle_cap() {
        let mut stores = Stores::default();
        let count = stores.states.alloc(StateValue::Int(0));
        let root = BuildCx::new(&mut stores.store)
            .leaf(LeafStyle::default())
            .id();
        stores.store.add_effect(
            root,
            move |cx| cx.get(count).is_some(),
            move |cx| {
                if let Some(StateValue::Int(n)) = cx.get(count) {
                    cx.set(count, StateValue::Int(n + 1));
                }
                None
            },
        );
        let cycle = stores.settle().expect_err("a cycle");
        assert_eq!(ReactiveCycle::CODE, "E4202");
        assert_eq!(cycle.dropped, 1);
        assert_eq!(
            (&*cycle.cells, &*cycle.effects),
            (&[count][..], &[root][..])
        );
        assert!(
            cycle.to_string().contains("re-run the effects of nodes"),
            "{cycle}"
        );
        assert_eq!(int(&stores.states, count), SETTLE_ROUNDS as i32);
    }

    #[test]
    fn an_effect_of_a_node_freed_before_the_flush_never_runs() {
        let mut stores = Stores::default();
        let root = BuildCx::new(&mut stores.store)
            .leaf(LeafStyle::default())
            .id();
        let ran = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = std::rc::Rc::clone(&ran);
        stores.store.add_effect(
            root,
            |_| true,
            move |_| {
                seen.set(true);
                None
            },
        );
        stores
            .store
            .free_tree(root, &mut stores.effects, &mut Vec::new());
        assert_eq!(stores.settle(), Ok(1));
        assert!(!ran.get());
    }

    /// A 1000dp row holding a filling column with an adaptive scope inside,
    /// and a content-sized column whose 10dp-high leaf a hook resizes by the
    /// scope's class: `width` maps each class to the leaf's width.
    struct Squeeze {
        root: NodeId,
        surface: Rect,
        scratch: Vec<u32>,
        redo: Vec<NodeId>,
        anchor: AnchorId,
    }

    impl Squeeze {
        fn new(stores: &mut Stores, start: f32, width: fn(SizeClass) -> f32) -> Squeeze {
            let column = |size| FlexStyle {
                axis: Axis::Column,
                size,
                ..Default::default()
            };
            let (mut scope, mut inner, mut leaf) = (None, None, None);
            let root = {
                let mut cx = BuildCx::new(&mut stores.store);
                cx.flex(
                    FlexStyle {
                        size: Size::fixed(1000.0, 100.0),
                        ..Default::default()
                    },
                    |cx| {
                        cx.flex(column(Size::fill()), |cx| {
                            scope = Some(
                                cx.flex(column(Size::fill()), |cx| {
                                    inner = Some(
                                        cx.leaf(LeafStyle {
                                            size: Size::fixed(10.0, 10.0),
                                            ..Default::default()
                                        })
                                        .id(),
                                    );
                                })
                                .id(),
                            );
                        });
                        cx.flex(
                            column(Size {
                                width: Length::Fit,
                                height: Length::Fill { weight: 1.0 },
                            }),
                            |cx| {
                                leaf = Some(
                                    cx.leaf(LeafStyle {
                                        size: Size::fixed(start, 10.0),
                                        ..Default::default()
                                    })
                                    .id(),
                                );
                            },
                        );
                    },
                );
                cx.root().unwrap()
            };
            let leaf = leaf.unwrap();
            stores.states.mark_adaptive_scope(scope.unwrap(), None);
            let anchor = stores.states.anchor_env(inner.unwrap(), None);
            let class = stores
                .states
                .anchor_cell(anchor, EnvField::SizeClass)
                .unwrap();
            stores.store.add_structure_hook([class], move |cx, _| {
                let class = cx.states.env().size_class(anchor).unwrap();
                cx.store
                    .set_fixed_size(leaf, Size::fixed(width(class), 10.0));
                cx.store.mark_dirty(leaf, DirtyClass::MEASURE);
            });
            let surface = Rect {
                x: 0.0,
                y: 0.0,
                w: 1000.0,
                h: 100.0,
            };
            let mut scratch = Vec::new();
            stores.store.layout(root, surface, &mut scratch);
            Squeeze {
                root,
                surface,
                scratch,
                redo: Vec::new(),
                anchor,
            }
        }

        fn frame(&mut self, stores: &mut Stores) -> Result<u32, Unsettled> {
            let Squeeze {
                root,
                surface,
                scratch,
                redo,
                ..
            } = self;
            let (measured, laid_out) = stores.store.relayout_dirty(*root, *surface, scratch, redo);
            settle_adaptive(
                &mut stores.store,
                &mut stores.states,
                &mut stores.bindings,
                &mut stores.computeds,
                &mut stores.projectors,
                &mut stores.effects,
                &mut stores.changed,
                measured + laid_out > 0,
                |store| {
                    let (measured, laid_out) = store.relayout_dirty(*root, *surface, scratch, redo);
                    measured + laid_out > 0
                },
            )
        }
    }

    #[test]
    fn a_structure_the_environment_selects_converges_in_one_frame() {
        let mut stores = Stores::default();
        let mut tree = Squeeze::new(&mut stores, 100.0, |class| match class {
            SizeClass::Expanded => 500.0,
            _ => 300.0,
        });
        assert_eq!(
            tree.frame(&mut stores),
            Ok(3),
            "expanded at 900dp, compact at 500dp, medium at 700dp"
        );
        assert_eq!(
            stores.states.env().size_class(tree.anchor),
            Some(SizeClass::Medium)
        );
        assert_eq!(
            tree.frame(&mut stores),
            Ok(0),
            "a settled frame resolves nothing"
        );
    }

    #[test]
    fn a_structure_that_moves_its_own_class_stops_as_an_adaptive_cycle() {
        let mut stores = Stores::default();
        let mut tree = Squeeze::new(&mut stores, 100.0, |class| match class {
            SizeClass::Compact => 100.0,
            _ => 600.0,
        });
        let Err(Unsettled::Adaptive(cycle)) = tree.frame(&mut stores) else {
            panic!("the class flips every layout");
        };
        assert_eq!(cycle, AdaptiveCycle { dropped: 1 });
        assert_eq!(AdaptiveCycle::CODE, "E4204");
        assert!(!stores.states.has_pending(), "its changes are dropped");
        assert_eq!(tree.frame(&mut stores), Ok(0), "the last structure holds");
    }

    #[test]
    fn an_anchor_inside_an_avoiding_region_reads_the_padded_box() {
        let mut stores = Stores::default();
        let fill = FlexStyle {
            axis: Axis::Column,
            size: Size::fill(),
            ..Default::default()
        };
        let mut inner = None;
        let mut region = None;
        let root = {
            let mut cx = BuildCx::new(&mut stores.store);
            cx.flex(
                FlexStyle {
                    size: Size::fixed(1000.0, 100.0),
                    ..Default::default()
                },
                |cx| {
                    let boxed = FlexStyle {
                        size: Size::fixed(1000.0, 100.0),
                        ..fill
                    };
                    let handle = cx.avoiding(Avoid::SafeArea, boxed, |cx| {
                        inner = Some(cx.flex(fill, |_| {}).id());
                    });
                    region = Some(handle.id());
                },
            );
            cx.root().unwrap()
        };
        stores
            .states
            .mark_avoiding(region.unwrap(), Avoid::SafeArea, Inset::default());
        let anchor = stores.states.anchor_env(inner.unwrap(), None);
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 100.0,
        };
        let mut scratch = Vec::new();
        stores.store.layout(root, surface, &mut scratch);
        let mut tree = Squeeze {
            root,
            surface,
            scratch,
            redo: Vec::new(),
            anchor,
        };
        tree.frame(&mut stores).unwrap();
        let height = |stores: &Stores| stores.states.env().constraints(anchor).unwrap().max_height;
        assert_eq!(height(&stores), Some(100.0));
        stores.states.update_env(|env| env.safe_area.top = 30.0);
        assert_eq!(
            tree.frame(&mut stores),
            Ok(2),
            "one layout pads the region, one resolves the anchor in it"
        );
        assert_eq!(height(&stores), Some(70.0));
        assert_eq!(tree.frame(&mut stores), Ok(0));
    }

    #[test]
    fn nothing_pending_runs_no_round() {
        let mut stores = Stores::default();
        assert_eq!(stores.settle(), Ok(0));
    }

    #[test]
    fn a_write_made_while_settling_flushes_in_the_same_frame() {
        let mut stores = Stores::default();
        let source = stores.states.alloc(StateValue::Int(0));
        let derived = stores.states.alloc(StateValue::Int(0));
        let seen = stores.states.alloc(StateValue::Int(0));
        stores.store.add_structure_hook([source], move |cx, _| {
            let next = int(cx.states, source) * 10;
            cx.states.set(derived, StateValue::Int(next));
        });
        stores.store.add_structure_hook([derived], move |cx, _| {
            let next = int(cx.states, derived) + 1;
            cx.states.set(seen, StateValue::Int(next));
        });
        stores.states.set(source, StateValue::Int(4));
        assert_eq!(stores.settle(), Ok(3), "source, derived, then seen");
        assert_eq!(int(&stores.states, seen), 41);
        assert!(!stores.states.has_pending());
        assert!(stores.changed.is_empty());
    }

    #[test]
    fn a_hook_feeding_itself_stops_as_a_reactive_cycle() {
        let mut stores = Stores::default();
        let cell = stores.states.alloc(StateValue::Int(0));
        stores.store.add_structure_hook([cell], move |cx, _| {
            let next = int(cx.states, cell) + 1;
            cx.states.set(cell, StateValue::Int(next));
        });
        stores.states.set(cell, StateValue::Int(1));
        let cycle = stores.settle().expect_err("the hook never settles");
        assert_eq!(
            cycle,
            ReactiveCycle {
                dropped: 1,
                cells: vec![cell],
                effects: Vec::new(),
            }
        );
        assert_eq!(ReactiveCycle::CODE, "E4202");
        assert_eq!(int(&stores.states, cell), 1 + SETTLE_ROUNDS as i32);
        assert!(
            !stores.states.has_pending(),
            "the cycle's writes are dropped"
        );
        assert_eq!(stores.settle(), Ok(0), "the next frame is quiet");
    }
}
