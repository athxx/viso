//! Runtime acceptance (§154), from source through the mounted view: capture
//! before target before bubble, paint-only writes that lay nothing out, keyed
//! identity, preserved-branch eviction, stale loads, fault rollback and
//! environment changes touching only their dependents.

mod support;

use support::Rt;
use viso_behavior::FaultKind;
use viso_ui::adaptive::Environment;
use viso_ui::{DirtyClass, NodeId};

/// Whether some mounted node carries any of `class`.
fn any_dirty(rt: &Rt, class: DirtyClass) -> bool {
    rt.nodes
        .iter()
        .flatten()
        .any(|&id| rt.store.dirty(id).intersects(class))
}

#[test]
fn capture_handlers_run_before_the_target_and_bubble_after() {
    let mut rt = Rt::mount(
        "export component C {\n    state log = 0;\n    view {\n        Column {\n            width: 400dp; height: 100dp;\n            \
         on capture click { log = log * 10 + 1; }\n            on click { log = log * 10 + 3; }\n            \
         Text { width: 20dp; height: 20dp; on click { log = log * 10 + 2; } }\n        }\n    }\n}\n",
    );
    rt.click(0);
    assert_eq!(rt.int("log"), Some(123));
}

#[test]
fn capture_runs_first_from_the_release_package_too() {
    let mut rt = Rt::packaged(
        "export component C {\n    state log = 0;\n    view {\n        Column {\n            \
         width: 400dp; height: 100dp;\n            on capture click { log = log * 10 + 1; }\n            \
         on click { log = log * 10 + 3; }\n            \
         Text { width: 20dp; height: 20dp; on click { log = log * 10 + 2; } }\n        }\n    }\n}\n",
    );
    rt.click(0);
    assert_eq!(rt.int("log"), Some(123));
}

/// Two 20dp rows: the first writes a state only the `background` of the
/// panel reads, the second one its width reads.
const LOOK: &str = "export component C {\n    state lit = false;\n    state wide = false;\n    \
    view {\n        Column {\n            width: 400dp; height: 100dp;\n            \
    Text { width: 20dp; height: 20dp; on click { lit = !lit; } }\n            \
    Text { width: 20dp; height: 20dp; on click { wide = !wide; } }\n            \
    Column {\n                width: if wide { 300dp } else { 100dp }; height: 20dp;\n                \
    background: if lit { #ff0000 } else { #00ff00 };\n                Text { text: \"x\"; }\n            }\n        }\n    }\n}\n";

#[test]
fn a_paint_only_write_lays_nothing_out() {
    let mut rt = Rt::mount(LOOK);
    rt.store.clear_dirty();
    rt.press(0);
    rt.settle();
    assert_eq!(rt.int("lit"), Some(1), "the write landed");
    assert!(any_dirty(&rt, DirtyClass::PAINT));
    assert!(!any_dirty(&rt, DirtyClass::MEASURE | DirtyClass::LAYOUT));
    assert_eq!(rt.relayout(), (0, 0), "the background repaints alone");
    rt.store.clear_dirty();
    rt.press(1);
    rt.settle();
    let (measured, laid_out) = rt.relayout();
    assert!(measured > 0 && laid_out > 0, "a width write relays out");
}

/// Keyed rows, each an instance that starts a task on mount: the first
/// button reorders the keys, the second replaces key 2 with key 4.
const ROWS: &str = r#"
import viso::time;

component Row {
    input id: I64;
    state got = 0;
    task tick() -> I64 { await time::sleep(1s); 1 }
    effect load { start tick() { success(v) { got = v; } }; }
    view { Text { width: 20dp; height: 20dp; } }
}

export component C {
    state items = [1, 2, 3];
    view {
        Column {
            width: 400dp;
            height: 100dp;
            Text { width: 20dp; height: 20dp; on click { items = [3, 1, 2]; } }
            Text { width: 20dp; height: 20dp; on click { items = [3, 1, 4]; } }
            Column { width: 400dp; height: 60dp; for item in items key item { Row { id: item; } } }
        }
    }
}
"#;

#[test]
fn a_keyed_reorder_keeps_focus_and_a_key_change_cancels_the_row_task() {
    let mut rt = Rt::mount(ROWS);
    let rows = rt.region(2);
    assert_eq!(rows.len(), 3);
    assert_eq!(rt.store.task_count(), 3, "each row started its task");
    rt.store.set_focused(Some(rows[2]));

    rt.click(0);
    assert_eq!(rt.region(2), [rows[2], rows[0], rows[1]]);
    assert_eq!(
        rt.store.focused(),
        Some(rows[2]),
        "focus moves with the key"
    );
    assert_eq!(rt.store.task_count(), 3, "a move cancels nothing");
    assert_eq!(rt.fault(), None);

    rt.click(1);
    let after = rt.region(2);
    assert_eq!(&after[..2], [rows[2], rows[0]]);
    assert!(!rows.contains(&after[2]), "key 4 is a new row");
    assert_eq!(
        rt.store.task_count(),
        3,
        "key 2's task was cancelled and key 4's started"
    );
    assert_eq!(rt.store.focused(), Some(rows[2]));
    rt.ring();
    assert_eq!(rt.store.task_count(), 0);
    assert_eq!(rt.fault(), None);
}

/// A resource whose first key loads slower than the next.
const SEARCH: &str = r#"
import viso::time;

export component C {
    state q = 1;
    resource r: Resource<I64, String> {
        load = fetch(q);
        key = q;
    }
    task fetch(n: I64) -> Result<I64, String> {
        await time::sleep(if n == 1 { 2s } else { 1s });
        Ok(n * 10)
    }
    view {
        Column {
            width: 400dp;
            height: 100dp;
            Text { width: 20dp; height: 20dp; on click { q += 1; } }
        }
    }
}
"#;

/// The ready value of resource `r`, if it is ready.
fn ready(rt: &Rt) -> Option<i64> {
    let host = rt.view.as_ref()?.borrow();
    let slot = host.state_slot("r")?;
    match host.state(slot)? {
        viso_view::Value::Agg(agg) if agg.tag == 2 => agg.fields[0].as_int(),
        _ => None,
    }
}

#[test]
fn a_stale_load_finishing_after_the_newer_one_is_dropped() {
    let mut rt = Rt::mount(SEARCH);
    rt.click(0);
    assert_eq!(rt.store.task_count(), 2, "both loads in flight");
    assert_eq!(rt.ring_upto(std::time::Duration::from_secs(1)), 1);
    assert_eq!(ready(&rt), Some(20), "the newer key settles first");
    assert_eq!(rt.ring(), 1);
    assert_eq!(
        ready(&rt),
        Some(20),
        "the older one finishing later is dropped"
    );
    assert_eq!(rt.store.task_count(), 0);
    assert_eq!(rt.fault(), None);
}

/// Three 20dp rows: one copies through a clipboard that panics, one spins
/// past the instruction budget, one counts; the first two write before they
/// fault.
const FAULTS: &str = r#"
import viso::clipboard;

export component C {
    state copies = 0;
    state spins = 0;
    state plain = 0;
    view {
        Column {
            width: 400dp;
            height: 100dp;
            Text { width: 20dp; height: 20dp; on click { copies += 1; clipboard::write_text("x"); } }
            Text { width: 20dp; height: 20dp; on click { spins += 1; while true { spins += 1; } } }
            Text { width: 20dp; height: 20dp; on click { plain += 1; } }
        }
    }
}
"#;

/// A clipboard whose host side panics.
struct Broken;

impl viso_behavior::native::Clipboard for Broken {
    fn read_text(&mut self) -> Option<String> {
        panic!("the host clipboard is gone")
    }

    fn write_text(&mut self, _: &str) {
        panic!("the host clipboard is gone")
    }
}

#[test]
fn a_native_panic_or_budget_fault_rolls_back_and_the_view_keeps_running() {
    let mut rt = Rt::mount_granted(FAULTS, &["clipboard.write"]);
    rt.view
        .as_ref()
        .expect("a view")
        .borrow_mut()
        .services_mut()
        .insert::<Box<dyn viso_behavior::native::Clipboard>>(Box::new(Broken));
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    rt.click(0);
    std::panic::set_hook(quiet);
    let fault = rt.fault().expect("the panic faulted");
    assert_eq!(fault.kind, FaultKind::NativeFailure, "{fault:?}");
    assert_eq!(rt.int("copies"), Some(0), "the write before it rolled back");

    rt.click(1);
    let fault = rt.fault().expect("the spin faulted");
    assert_eq!(fault.kind, FaultKind::InstructionBudget, "{fault:?}");
    assert_eq!(rt.int("spins"), Some(0));

    rt.click(2);
    rt.click(2);
    assert_eq!(
        rt.ints(["plain", "copies", "spins"]),
        [Some(2), Some(0), Some(0)]
    );
}

/// One row per environment reader, and a row that reads nothing.
const ENV: &str = r#"
export component C {
    view {
        Column {
            width: 400dp;
            height: 100dp;
            Row {
                width: 400dp;
                height: 10dp;
                if env.size_class == SizeClass::Compact {
                    Text { width: 10dp; height: 10dp; }
                } else {
                    Text { width: 10dp; height: 10dp; }
                    Text { width: 10dp; height: 10dp; }
                }
            }
            Row {
                width: 400dp;
                height: 10dp;
                if env.keyboard_inset.height > 0dp { Text { width: 10dp; height: 10dp; } }
            }
            Row {
                width: 400dp;
                height: 10dp;
                if env.safe_area.top > 0dp { Text { width: 10dp; height: 10dp; } }
            }
            Row {
                width: 400dp;
                height: 40dp;
                Text { width: 10dp; height: 10sp; }
                if env.text_scale > 1.5 { Text { width: 10dp; height: 10dp; } }
            }
            Row { width: 400dp; height: 10dp; Text { width: 10dp; height: 10dp; } }
        }
    }
}
"#;

impl Rt {
    /// The root's `row`th child and everything beneath it.
    fn row_subtree(&self, row: usize) -> Vec<NodeId> {
        let mut out = vec![self.children(self.root.expect("mounted"))[row]];
        let mut at = 0;
        while at < out.len() {
            out.extend(self.children(out[at]));
            at += 1;
        }
        out
    }

    /// The rows holding a dirty node, the root aside.
    fn dirty_rows(&self) -> Vec<usize> {
        let rows = self.children(self.root.expect("mounted")).len();
        (0..rows)
            .filter(|&row| {
                self.row_subtree(row)
                    .iter()
                    .any(|&id| !self.store.dirty(id).is_empty())
            })
            .collect()
    }

    /// Applies `change`, lays out what it dirtied and returns the rows it
    /// touched, then lays out and cleans for the next change.
    fn env_change(&mut self, change: impl FnOnce(&mut Environment)) -> Vec<usize> {
        self.store.clear_dirty();
        self.update_env(change);
        let rows = self.dirty_rows();
        self.relayout();
        self.frame();
        rows
    }
}

#[test]
fn an_environment_change_touches_only_its_dependents() {
    let mut rt = Rt::mount(ENV);
    let quiet: Vec<Vec<NodeId>> = (0..5).map(|row| rt.row_subtree(row)).collect();
    assert_eq!(rt.region(0).len(), 1, "a zero-width window is compact");

    assert_eq!(rt.env_change(|e| e.window.width = 700.0), [0]);
    assert_eq!(rt.region(0).len(), 2, "medium");
    let medium = rt.region(0);
    assert_eq!(
        rt.env_change(|e| e.window.width = 800.0),
        Vec::<usize>::new(),
        "a resize within the class patches nothing"
    );
    assert_eq!(rt.region(0), medium);

    assert_eq!(rt.env_change(|e| e.keyboard_inset = 300.0), [1]);
    assert_eq!(rt.region(1).len(), 1);
    assert_eq!(rt.env_change(|e| e.safe_area.top = 40.0), [2]);
    assert_eq!(rt.region(2).len(), 1);
    assert_eq!(
        rt.env_change(|e| e.safe_area.bottom = 20.0),
        Vec::<usize>::new(),
        "no reader of the bottom inset"
    );

    let scaled = rt.region(3)[0];
    assert_eq!(rt.store.bounds(scaled).h, 10.0);
    rt.store.clear_dirty();
    rt.update_env(|e| e.text_scale = 2.0);
    let (measured, laid_out) = rt.relayout();
    assert!(measured > 0 && laid_out > 0, "the sp height re-measures");
    assert!(
        rt.store
            .dirty(scaled)
            .contains(DirtyClass::MEASURE | DirtyClass::LAYOUT),
        "the moved box republishes its semantics bounds"
    );
    assert_eq!(rt.dirty_rows(), [3], "only the scaled row");
    assert_eq!(rt.store.bounds(scaled).h, 20.0);
    assert_eq!(rt.region(3).len(), 2, "the text-scale branch");
    let semantics = rt.store.derive_semantics(rt.root.expect("mounted"));
    assert_eq!(semantics.get(scaled).map(|n| n.bounds.h), Some(20.0));

    // The row that reads nothing kept its nodes through every change.
    assert_eq!(rt.row_subtree(4), quiet[4]);
    assert!(!rt.descendants().is_empty());
}

/// Two preserved branches, each toggled by its own row.
const PRESERVED: &str = r#"
export component C {
    state a = false;
    state b = false;
    view {
        Column {
            width: 400dp;
            height: 100dp;
            Text { width: 20dp; height: 20dp; on click { a = !a; } }
            Text { width: 20dp; height: 20dp; on click { b = !b; } }
            Column {
                width: 400dp;
                height: 20dp;
                if a preserve "a" { Text { width: 10dp; height: 10dp; } }
            }
            Column {
                width: 400dp;
                height: 20dp;
                if b preserve "b" { Text { width: 10dp; height: 10dp; } }
            }
        }
    }
}
"#;

impl Rt {
    fn host(&self) -> std::cell::RefMut<'_, viso_view::ViewHost> {
        self.view.as_ref().expect("a view").borrow_mut()
    }

    /// Shows then hides both branches, `a` first; returns their nodes.
    fn leave_both(&mut self) -> [NodeId; 2] {
        self.click(0);
        let a = self.region(2)[0];
        self.click(0);
        self.click(1);
        let b = self.region(3)[0];
        self.click(1);
        [a, b]
    }
}

fn preserved() -> [Rt; 2] {
    [Rt::mount(PRESERVED), Rt::packaged(PRESERVED)]
}

#[test]
fn a_preserved_branch_comes_back_with_its_nodes_until_evicted() {
    for mut rt in preserved() {
        let [a, b] = rt.leave_both();
        assert_eq!(rt.host().kept_branches(), 2, "each keeps its last instance");
        rt.click(0);
        assert_eq!(rt.region(2), [a], "the same node comes back");
        rt.click(1);
        assert_eq!(rt.region(3), [b]);
    }
}

#[test]
fn a_preserve_budget_evicts_the_least_recently_left_branch() {
    for mut rt in preserved() {
        rt.host().set_preserve_budget(Some(1));
        let [a, b] = rt.leave_both();
        assert_eq!(rt.host().kept_branches(), 1);
        assert!(!rt.store.arena().is_live(a), "a, left first, was freed");
        rt.click(1);
        assert_eq!(rt.region(3), [b], "b, left last, is kept");
        rt.click(0);
        let fresh = rt.region(2);
        assert_eq!(fresh.len(), 1);
        assert_ne!(fresh, [a], "a mounts afresh");
        assert_eq!(rt.fault(), None);
    }
}

#[test]
fn a_memory_trim_frees_every_kept_branch() {
    for mut rt in preserved() {
        let [a, b] = rt.leave_both();
        assert!(rt.store.request_memory_trim(&mut rt.states));
        rt.frame();
        assert_eq!(rt.host().kept_branches(), 0);
        assert!(!rt.store.arena().is_live(a) && !rt.store.arena().is_live(b));
        rt.click(0);
        assert_eq!(rt.region(2).len(), 1, "a mounts afresh");
        rt.click(0);
        assert_eq!(rt.host().kept_branches(), 1, "and is kept again");
    }
}
