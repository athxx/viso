//! Sizing defaults (U3.1): each axis a node does not size takes its type's
//! default — the view's root fills, a row, column or stack fits its content,
//! a wrap fills the width and fits the height, a grid, absolute container or
//! scroll view fills, a leaf fits — the same in the dev and packaged paths.

mod support;

use support::{Rt, SURFACE, origin};
use viso_dsl::frontend::compile_file;
use viso_dsl::ir::{LengthIr, UiItem};

const SIZED: &str = r#"
export component C {
    state n = 0
    view {
        Column {
            on click { n += 1 }
            Row {
                Text { width: 30dp; height: 10dp }
                Text { width: 20dp; height: 10dp }
            }
            Wrap { Text { width: 30dp; height: 10dp } }
            Stack { Text { width: 15dp; height: 5dp } }
            Row { height: 40dp; Text { width: 10dp; height: 10dp } }
            Scroll { Text { width: 10dp; height: 300dp } }
        }
    }
}
"#;

#[test]
fn each_unsized_axis_takes_its_types_default() {
    for packaged in [false, true] {
        let mut rt = if packaged {
            Rt::packaged(SIZED)
        } else {
            Rt::mount(SIZED)
        };
        assert_eq!(rt.fault(), None);
        rt.relayout();
        let root = rt.root.expect("mounted");
        let size = |n| {
            let b = rt.store.bounds(n);
            (b.w, b.h)
        };
        assert_eq!(size(root), (SURFACE.w, SURFACE.h), "the root fills");
        let parts = rt.children(root);
        assert_eq!(size(parts[0]), (50.0, 10.0), "a row fits its content");
        assert_eq!(size(parts[1]), (SURFACE.w, 10.0), "a wrap fills the width");
        assert_eq!(size(parts[2]), (15.0, 5.0), "a stack fits its content");
        assert_eq!(size(parts[3]), (10.0, 40.0), "the axis not written fits");
        let scroll = size(parts[4]);
        assert_eq!(scroll.0, SURFACE.w, "a scroll view fills");
        assert_eq!(
            scroll.1,
            SURFACE.h - 10.0 - 10.0 - 5.0 - 40.0,
            "the room left"
        );
    }
}

#[test]
fn the_views_root_fills_and_a_fragments_root_sizes_as_any_node() {
    let compiled = compile_file(SIZED, &origin());
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    let UiItem::Node(root) = &compiled.tree.items[0] else {
        panic!("a node");
    };
    let fill = Some(LengthIr::Fill { weight: 1.0 });
    assert_eq!((root.style.width, root.style.height), (fill, fill));
    let UiItem::Node(row) = &root.children[0] else {
        panic!("a node");
    };
    assert_eq!(
        (row.style.width, row.style.height),
        (Some(LengthIr::Fit), Some(LengthIr::Fit))
    );
    // A component's root sizes by its own type where it is placed.
    let placed = compile_file(
        "component Card { view { Column { Text(\"a\") } } }\n\
         export component C { view { Column { Card {} } } }",
        &origin(),
    );
    let UiItem::Node(root) = &placed.tree.items[0] else {
        panic!("a node");
    };
    let UiItem::Node(card) = &root.children[0] else {
        panic!("a node");
    };
    assert_eq!(card.style.width, Some(LengthIr::Fit));
}
