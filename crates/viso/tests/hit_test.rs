//! Hit testing through the public facade: a point maps to the topmost hittable
//! node. Like `reactive_wiring`, this drives the ui store directly (all reachable
//! through `viso::ui`) rather than the live scheduler, because pointer routing
//! that consumes the hit test lands in the next slice. Here we prove the query
//! itself over a real laid-out tree built through the facade path.

use viso::render::{Rect, Rgba};
use viso::ui::{Axis, BoxStyle, BuildCx, FlexStyle, Inset, LeafStyle, NodeStore, Size, hit_test};

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 200.0,
    h: 200.0,
};

const SOLID: Rgba = Rgba {
    r: 0.4,
    g: 0.4,
    b: 0.4,
    a: 1.0,
};

#[test]
fn facade_hit_test_resolves_leaf_padding_and_miss() {
    let mut store = NodeStore::new();
    let (root, leaf) = {
        let mut cx = BuildCx::new(&mut store);
        let mut leaf = None;
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                padding: Inset::all(20.0),
                size: Size::fixed(100.0, 100.0),
                style: BoxStyle::solid(SOLID),
                ..Default::default()
            },
            |cx| {
                leaf = Some(cx.leaf(LeafStyle {
                    size: Size::fixed(40.0, 40.0),
                    style: BoxStyle::solid(SOLID),
                }));
            },
        );
        (cx.root().unwrap(), leaf.unwrap())
    };
    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);

    // A point inside the leaf returns the leaf (it sits atop the container).
    assert_eq!(hit_test(&store, root, 30.0, 30.0), Some(leaf.id()));
    // A point in the container's padding returns the container.
    assert_eq!(hit_test(&store, root, 5.0, 5.0), Some(root));
    // A point beyond the surface (and so outside the root box) misses entirely.
    assert_eq!(hit_test(&store, root, 250.0, 250.0), None);
}
