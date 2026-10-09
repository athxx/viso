//! A view's control-flow regions under hot reload and the release package: an
//! `if` switches arms and keeps a `preserve` arm's nodes, a `match` switches on
//! its scrutinee, a keyed `for` moves its retained nodes on a reorder and hands
//! each item to its handlers, and a repeated key faults without re-shaping;
//! across a reload that rebuilds the view, the nodes a region mounts carry
//! their focus and scroll offset to the nodes built for the same items.

use std::cell::RefCell;
use std::rc::Rc;

use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::{CandidatePlan, HotReloadReport, LiveRuntime, hot_reload_view};
use viso_ui::Rect;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, SemanticProjector, StateStore, TextEdits, run_structure_hooks,
};
use viso_view::{Value, ViewHost, load_view};

const SOURCE: &str = r#"
component Regions {
    state open = false;
    state mode = 0;
    state picked = 0;
    state items = [1, 2, 3];
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Row {
                width: 400dp;
                height: 20dp;
                Text { width: 20dp; height: 20dp; on click { open = !open; } }
                Text { width: 20dp; height: 20dp; on click { mode += 1; } }
                Text { width: 20dp; height: 20dp; on click { items = [3, 1, 2]; } }
                Text { width: 20dp; height: 20dp; on click { items = [1, 1]; } }
            }
            Column {
                width: 400dp;
                height: 60dp;
                if open preserve "panel" {
                    Text { width: 10dp; height: 10dp; }
                } else {
                    Text { width: 10dp; height: 10dp; }
                    Text { width: 10dp; height: 10dp; }
                }
            }
            Column {
                width: 400dp;
                height: 60dp;
                match mode {
                    0 => { Text { width: 10dp; height: 10dp; } },
                    1 => { Row { width: 10dp; height: 10dp; Text { width: 5dp; height: 5dp; } } },
                    _ => { },
                }
            }
            Row {
                width: 400dp;
                height: 20dp;
                for item in items key item {
                    Text { width: 20dp; height: 20dp; on click { picked = item; } }
                }
            }
        }
    }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["regions".into()],
        language: None,
    }
}

/// A mounted view and the stores it runs over.
#[derive(Default)]
struct Rt {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    text_edits: TextEdits,
    projectors: SemanticProjector,
    root: Option<NodeId>,
    scratch: Vec<NodeId>,
    nodes: Vec<Option<NodeId>>,
    view: Option<Rc<RefCell<ViewHost>>>,
    last_good: CandidatePlan,
}

impl Rt {
    /// The view mounted by a hot reload.
    fn reloaded(source: &str) -> Self {
        let mut rt = Rt::default();
        rt.reload(source);
        rt
    }

    /// The view loaded from its release package.
    fn packaged(source: &str) -> Self {
        let blob = build_view_package(source, &origin()).expect("packages");
        let mut rt = Rt::default();
        let view = load_view(
            &blob,
            &mut rt.store,
            &mut rt.states,
            &mut rt.bindings,
            &mut rt.lists,
        )
        .expect("loads");
        rt.root = view.root;
        rt.view = view.host;
        rt.layout();
        rt
    }

    fn reload(&mut self, source: &str) -> HotReloadReport {
        let mut live = LiveRuntime {
            store: &mut self.store,
            states: &mut self.states,
            bindings: &mut self.bindings,
            effects: &mut self.effects,
            lists: &mut self.lists,
            text_edits: &mut self.text_edits,
            projectors: &mut self.projectors,
            root: self.root,
            scratch: &mut self.scratch,
            nodes: &mut self.nodes,
            view: &mut self.view,
        };
        let done = hot_reload_view(&mut live, &self.last_good, source, &origin()).expect("reloads");
        self.root = live.root;
        self.last_good = done.candidate;
        self.layout();
        done.report
    }

    fn layout(&mut self) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };
        if let Some(root) = self.root {
            self.store.layout(root, surface, &mut Vec::new());
        }
    }

    /// A primary click at `(x, y)`, then the frame's flush and structure hooks.
    fn click(&mut self, x: f32, y: f32) {
        let root = self.root.expect("mounted");
        let mut chain = Vec::new();
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let event = PointerEvent {
                x,
                y,
                phase,
                buttons: PointerButtons::PRIMARY,
                modifiers: Default::default(),
            };
            PointerRouter::route(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                root,
                event,
                &mut chain,
            );
        }
        let mut changed = Vec::new();
        self.states.take_pending(&mut changed);
        self.store
            .flush_state_transactions(&changed, &self.bindings);
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
            &changed,
        );
        self.layout();
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        let arena = self.store.arena();
        let mut out = Vec::new();
        let mut child = arena.links(node).and_then(|l| l.first_child);
        while let Some(c) = child {
            out.push(c);
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        out
    }

    /// The children of the root's `index`th child.
    fn region(&self, index: usize) -> Vec<NodeId> {
        let root = self.root.expect("mounted");
        self.children(self.children(root)[index])
    }

    fn state(&self, name: &str) -> Option<Value> {
        let host = self.view.as_ref()?.borrow();
        let slot = host.state_slot(name)?;
        host.state(slot).cloned()
    }

    fn faulted(&self) -> bool {
        let host = self.view.as_ref().expect("a host");
        host.borrow().last_fault().is_some()
    }
}

fn targets() -> [Rt; 2] {
    [Rt::reloaded(SOURCE), Rt::packaged(SOURCE)]
}

#[test]
fn an_if_switches_arms_and_keeps_a_preserved_arm() {
    for mut rt in targets() {
        assert_eq!(rt.region(1).len(), 2, "the else arm");
        rt.click(5.0, 5.0);
        let panel = rt.region(1);
        assert_eq!(panel.len(), 1, "the then arm");
        rt.click(5.0, 5.0);
        assert_eq!(rt.region(1).len(), 2, "back to the else arm");
        rt.click(5.0, 5.0);
        assert_eq!(rt.region(1), panel, "the preserved node comes back");
    }
}

#[test]
fn a_match_switches_on_its_scrutinee() {
    for mut rt in targets() {
        let region = rt.region(2);
        assert_eq!(region.len(), 1);
        assert!(rt.children(region[0]).is_empty(), "arm 0 is a leaf");
        rt.click(25.0, 5.0);
        let region = rt.region(2);
        assert_eq!(region.len(), 1);
        assert_eq!(rt.children(region[0]).len(), 1, "arm 1 is a row");
        rt.click(25.0, 5.0);
        assert!(rt.region(2).is_empty(), "the wildcard arm is empty");
    }
}

#[test]
fn a_keyed_for_moves_its_nodes_on_a_reorder() {
    for mut rt in targets() {
        let before = rt.region(3);
        assert_eq!(before.len(), 3);
        rt.click(45.0, 5.0);
        let after = rt.region(3);
        assert_eq!(after, vec![before[2], before[0], before[1]]);
        assert!(!rt.faulted());
    }
}

#[test]
fn a_for_item_reaches_its_handler() {
    for mut rt in targets() {
        rt.click(25.0, 150.0);
        assert_eq!(rt.state("picked"), Some(Value::Int(2)));
        rt.click(45.0, 5.0);
        rt.click(5.0, 150.0);
        assert_eq!(rt.state("picked"), Some(Value::Int(3)));
    }
}

#[test]
fn a_repeated_key_faults_and_keeps_the_items() {
    for mut rt in targets() {
        let before = rt.region(3);
        rt.click(65.0, 5.0);
        assert!(rt.faulted());
        assert_eq!(rt.region(3), before);
    }
}

#[test]
fn a_reload_keeps_the_state_the_regions_read() {
    let mut rt = Rt::reloaded(SOURCE);
    rt.click(5.0, 5.0);
    rt.click(25.0, 5.0);
    rt.reload(&SOURCE.replace("width: 5dp", "width: 6dp"));
    assert_eq!(rt.region(1).len(), 1, "open survives");
    assert_eq!(rt.children(rt.region(2)[0]).len(), 1, "mode survives");
    rt.click(5.0, 5.0);
    assert_eq!(rt.region(1).len(), 2, "the reloaded hook runs");
    assert_eq!(rt.store.structure_hook_count(), 1);
}

#[test]
fn a_reload_out_of_regions_drops_the_hook() {
    let mut rt = Rt::reloaded(SOURCE);
    rt.reload(
        "component Regions {
            state open = false;
            view { Column { width: 10dp; height: 10dp; } }
        }",
    );
    assert_eq!(rt.store.structure_hook_count(), 0);
    assert!(rt.view.is_none());
}

/// The diagnostic codes `source` fails to package with.
fn codes(source: &str) -> Vec<String> {
    build_view_package(source, &origin())
        .expect_err("does not package")
        .into_iter()
        .map(|d| d.code.to_string())
        .collect()
}

#[test]
fn a_repeated_preserve_identity_is_e3301() {
    let codes = codes(
        r#"component Twice {
            state open = false;
            view {
                Column {
                    if open preserve "panel" { Text {} }
                    if !open preserve "panel" { Text {} }
                }
            }
        }"#,
    );
    assert_eq!(codes, ["E3301"]);
}

#[test]
fn a_region_at_the_root_does_not_mount() {
    let codes = codes(
        "component Bare {
            state open = false;
            view { if open { Text {} } }
        }",
    );
    assert!(
        !codes.is_empty() && codes.iter().all(|c| c == "E3711"),
        "{codes:?}"
    );
}

#[test]
fn a_region_rolls_back_when_its_reload_does_not_mount() {
    let mut rt = Rt::reloaded(SOURCE);
    let before = rt.region(3);
    let mut live = LiveRuntime {
        store: &mut rt.store,
        states: &mut rt.states,
        bindings: &mut rt.bindings,
        effects: &mut rt.effects,
        lists: &mut rt.lists,
        text_edits: &mut rt.text_edits,
        projectors: &mut rt.projectors,
        root: rt.root,
        scratch: &mut rt.scratch,
        nodes: &mut rt.nodes,
        view: &mut rt.view,
    };
    let broken = SOURCE.replace("key item", "key missing");
    assert!(hot_reload_view(&mut live, &rt.last_good, &broken, &origin()).is_err());
    assert_eq!(rt.region(3), before);
    rt.click(45.0, 5.0);
    assert_eq!(
        rt.region(3),
        vec![before[2], before[0], before[1]],
        "the last-good hook runs"
    );
}

const SCROLLERS: &str = r#"
component Scrollers {
    state items = [1, 2, 3];
    state open = true;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Column {
                width: 400dp;
                height: 100dp;
                for item in items key item {
                    Scroll {
                        width: 100dp;
                        height: 20dp;
                        Text { width: 100dp; height: 200dp; }
                    }
                }
            }
            Column {
                width: 400dp;
                height: 100dp;
                if open {
                    Text { width: 10dp; height: 10dp; }
                }
            }
        }
    }
}
"#;

#[test]
fn nodes_a_region_mounts_carry_their_state_across_a_rebuild() {
    let mut rt = Rt::reloaded(SCROLLERS);
    let rows = rt.region(0);
    assert_eq!(rows.len(), 3);
    rt.store.scroll_by(rows[1], viso_ui::Vec2::new(0.0, 30.0));
    rt.store.scroll_by(rows[2], viso_ui::Vec2::new(0.0, 60.0));
    let [panel] = rt.region(1)[..] else {
        panic!("the then arm");
    };
    rt.store.set_focused(Some(panel));

    // A sibling ahead of both regions shifts every key and rebuilds the view.
    let shifted = SCROLLERS.replacen(
        "height: 300dp;",
        "height: 300dp;\n            Text { width: 10dp; height: 10dp; }",
        1,
    );
    let report = rt.reload(&shifted);
    assert!(!report.focus_lost);
    assert_eq!(report.scroll_lost, 0);
    let rebuilt = rt.region(1);
    assert!(
        rebuilt.iter().all(|row| !rows.contains(row)),
        "rebuilt rows"
    );
    let rows = rebuilt;
    let offsets: Vec<f32> = rows.iter().map(|&row| rt.store.scroll(row).y).collect();
    assert_eq!(offsets, [0.0, 30.0, 60.0], "each item keeps its offset");
    let [panel] = rt.region(2)[..] else {
        panic!("the then arm");
    };
    assert_eq!(
        rt.store.focused(),
        Some(panel),
        "the arm's node keeps the focus"
    );

    // A row no longer a scroll container loses its offset.
    let report = rt.reload(&shifted.replacen("Scroll {", "Column {", 1));
    assert_eq!(report.scroll_lost, 2);
    assert!(!report.focus_lost);
}
