//! Scroll routing through the public facade: a synthetic wheel sample reaches the
//! innermost scroll viewport under the pointer, moves its offset (clamped to the
//! scrollable range), and dirties only transform/hit-test/paint — never layout —
//! so a scroll re-derives world rects and repaints without a relayout. The full
//! Slice H chain (hit-test → viewport select → clamp → targeted invalidation),
//! end to end.
//!
//! Like `pointer_routing`, this drives the ui stores directly (all reachable
//! through `viso::ui`) rather than standing up the live scheduler and a real
//! window: the facade's `on_input` lowers a platform scroll sample and calls the
//! exact same `ScrollRouter` this test drives, so exercising the router over a
//! facade-built tree proves the same path without a platform surface.

use viso::render::Rect;
use viso::ui::{
    Axis, BuildCx, DirtyClass, LeafStyle, NodeStore, ScrollEvent, ScrollRouter, ScrollStyle, Size,
    Vec2,
};

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 100.0,
    h: 100.0,
};

fn wheel(x: f32, y: f32, dx: f32, dy: f32) -> ScrollEvent {
    ScrollEvent {
        x,
        y,
        delta_x: dx,
        delta_y: dy,
        modifiers: Default::default(),
    }
}

/// A 100×100 vertical viewport over a 100×300 content child (200px of range).
fn scroll_scene() -> (NodeStore, viso::ui::NodeId, viso::ui::NodeId) {
    let mut store = NodeStore::new();
    let mut content = None;
    let root = {
        let mut cx = BuildCx::new(&mut store);
        cx.scroll(
            ScrollStyle {
                axis: Axis::Column,
                size: Size::fixed(100.0, 100.0),
                ..Default::default()
            },
            |cx| {
                content = Some(
                    cx.leaf(LeafStyle {
                        size: Size::fixed(100.0, 300.0),
                        ..Default::default()
                    })
                    .id(),
                );
            },
        );
        cx.root().unwrap()
    };
    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);
    (store, root, content.unwrap())
}

#[test]
fn wheel_scrolls_the_viewport_and_dirties_only_transform_paint() {
    let (mut store, viewport, content) = scroll_scene();
    store.clear_dirty();

    let consumed = ScrollRouter::route(&mut store, viewport, wheel(50.0, 50.0, 0.0, 60.0));
    assert!(consumed, "the wheel landed on the viewport");
    assert_eq!(store.scroll(viewport), Vec2 { x: 0.0, y: 60.0 });

    // A scroll marks transform/hit-test/paint but never relayouts.
    let d = store.dirty(viewport);
    assert!(d.contains(DirtyClass::TRANSFORM));
    assert!(d.contains(DirtyClass::PAINT));
    assert!(!d.intersects(DirtyClass::LAYOUT | DirtyClass::MEASURE));

    // Re-deriving world rects shifts the content up by the offset; bounds (the
    // unscrolled layout truth) are untouched.
    store.resolve_transforms(viewport);
    let b = store.bounds(content);
    assert_eq!(store.world(content).y, b.y - 60.0);
    assert_eq!(store.bounds(content).y, b.y);
}

#[test]
fn wheel_clamps_at_the_end_of_range() {
    let (mut store, viewport, _content) = scroll_scene();
    ScrollRouter::route(&mut store, viewport, wheel(50.0, 50.0, 0.0, 10_000.0));
    assert_eq!(
        store.scroll(viewport),
        Vec2 { x: 0.0, y: 200.0 },
        "clamped to content(300) − viewport(100)"
    );
}

#[test]
fn wheel_off_the_viewport_scrolls_nothing() {
    let (mut store, viewport, _content) = scroll_scene();
    let consumed = ScrollRouter::route(&mut store, viewport, wheel(500.0, 500.0, 0.0, 60.0));
    assert!(!consumed);
    assert_eq!(store.scroll(viewport), Vec2::ZERO);
}
