//! A node's `cursor`: it names the pointer shape over the node and its
//! subtree, follows the state it reads, and leaves fields their text beam —
//! under hot reload and from the release package.

mod support;

use support::Rt;
use viso_ui::CursorIcon;

const PANEL: &str = r#"
export component Panel {
    state busy = false;
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { busy = !busy; } }
            Column {
                width: 20dp;
                height: 20dp;
                cursor: if busy { Cursor::wait } else { Cursor::pointer };
                Text { width: 20dp; height: 20dp; }
            }
            TextInput { width: 20dp; height: 20dp; }
        }
    }
}
"#;

#[test]
fn a_cursor_names_the_shape_over_the_subtree_in_every_target() {
    for mut rt in [Rt::mount(PANEL), Rt::packaged(PANEL)] {
        assert_eq!(rt.fault(), None);
        rt.relayout();
        let root = rt.root.expect("mounted");
        let [_, column, field]: [_; 3] = rt.children(root).try_into().expect("three children");
        assert_eq!(rt.store.cursor(column), Some(CursorIcon::Pointer));
        assert_eq!(rt.store.cursor(field), None, "a field keeps its beam");

        rt.click(0);
        assert_eq!(rt.store.cursor(column), Some(CursorIcon::Wait));
        assert_eq!(rt.fault(), None);
    }
}
