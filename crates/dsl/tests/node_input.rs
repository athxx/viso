//! A node's `enabled` and `autofocus`: a disabled node and its
//! subtree take no pointer event, no focus and read as disabled until it is
//! enabled again, and the first `autofocus` node takes the focus as it
//! mounts, a reload giving it none back — under hot reload and from the
//! release package.

mod support;

use support::Rt;
use viso_ui::NodeId;

const PANEL: &str = r#"
export component Panel {
    state on = true;
    state taps = 0;
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { on = !on; } }
            Text { width: 20dp; height: 20dp; enabled: on; on click { taps += 1; } }
            Column {
                width: 20dp;
                height: 20dp;
                enabled: on;
                Text { width: 20dp; height: 20dp; focusable: true; on click { taps += 10; } }
            }
            TextInput { width: 20dp; height: 20dp; autofocus: true; }
            TextInput { width: 20dp; height: 20dp; autofocus: true; }
        }
    }
}
"#;

/// The root's children: the toggle, the guarded text, the guarded column,
/// and the two fields asking for the focus.
fn nodes(rt: &Rt) -> [NodeId; 5] {
    let root = rt.root.expect("mounted");
    rt.children(root).try_into().expect("five children")
}

#[test]
fn a_disabled_node_and_its_subtree_take_no_input_in_every_target() {
    for mut rt in [Rt::mount(PANEL), Rt::packaged(PANEL)] {
        assert_eq!(rt.fault(), None);
        rt.relayout();
        let [_, text, column, field, _] = nodes(&rt);
        let inner = rt.children(column)[0];
        assert_eq!(rt.store.focused(), Some(field), "the first autofocus wins");

        rt.click(1);
        rt.click(2);
        assert_eq!(rt.int("taps"), Some(11));

        rt.click(0);
        assert!(rt.store.inert(text) && rt.store.inert(inner));
        assert!(rt.store.enabled(inner), "its own `enabled` holds");
        rt.click(1);
        rt.click(2);
        assert_eq!(rt.int("taps"), Some(11), "neither click lands");
        assert!(!viso_ui::focus_node(&mut rt.store, inner));
        let root = rt.root.expect("mounted");
        let tree = rt.store.derive_semantics(root);
        let disabled = |id| tree.get(id).is_some_and(|n| n.disabled);
        assert!(disabled(text) && disabled(inner) && !disabled(field));

        rt.click(0);
        rt.click(1);
        assert_eq!(rt.int("taps"), Some(12));
        assert!(viso_ui::focus_node(&mut rt.store, inner));
        assert_eq!(rt.fault(), None);
    }
}

#[test]
fn a_reload_gives_autofocus_no_second_turn() {
    let mut rt = Rt::mount(PANEL);
    rt.relayout();
    rt.store.set_focused(None);
    rt.reload(&PANEL.replace("taps += 1;", "taps += 2;"));
    assert_eq!(rt.store.focused(), None);
    rt.click(1);
    assert_eq!(rt.int("taps"), Some(2), "the reload took");
}
