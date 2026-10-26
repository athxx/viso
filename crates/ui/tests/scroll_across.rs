//! A viewport scrolling both ways: its content takes its natural extent
//! along and across, at least filling it; a wheel moves both offsets and
//! hands what it cannot take to the viewport around it; a row read right to
//! left starts at its right edge; classic bars keep room beside and under
//! the content and stop short of the corner; a slanted pan moves both ways
//! while one begun along an axis keeps to it.

use viso_ui::adaptive::LayoutDirection;
use viso_ui::{
    Axis, BindingTable, BuildCx, LeafStyle, Modifiers, NodeId, NodeStore, PointerButtons,
    PointerContact, PointerEvent, PointerId, PointerPhase, PointerRouter, Rect, ScrollBarStyle,
    ScrollBars, ScrollEvent, ScrollPhase, ScrollStyle, Sizing, StateStore, Vec2, route_scroll,
};

fn surface(w: f32, h: f32) -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w,
        h,
    }
}

/// A 100×100 viewport scrolling both ways over content `w`×`h`.
fn scene(store: &mut NodeStore, w: f32, h: f32) -> (NodeId, NodeId) {
    let mut content = None;
    let root = {
        let mut cx = BuildCx::new(store);
        cx.scroll(
            ScrollStyle {
                across: true,
                size: Sizing::fixed(100.0, 100.0),
                bars: ScrollBars::Hidden,
                ..Default::default()
            },
            |cx| {
                content = Some(
                    cx.leaf(LeafStyle {
                        size: Sizing::fixed(w, h),
                        ..Default::default()
                    })
                    .id(),
                );
            },
        );
        cx.root().unwrap()
    };
    store.layout(root, surface(100.0, 100.0), &mut Vec::new());
    (root, content.unwrap())
}

fn wheel(store: &mut NodeStore, root: NodeId, dx: f32, dy: f32) -> bool {
    route_scroll(
        store,
        root,
        ScrollEvent {
            x: 50.0,
            y: 50.0,
            delta_x: dx,
            delta_y: dy,
            modifiers: Modifiers::default(),
            phase: ScrollPhase::None,
        },
    )
}

#[test]
fn the_content_takes_its_extent_both_ways_and_at_least_fills_the_viewport() {
    let mut store = NodeStore::new();
    let (root, content) = scene(&mut store, 300.0, 400.0);
    assert_eq!(
        store.bounds(content),
        Rect {
            x: 0.0,
            y: 0.0,
            w: 300.0,
            h: 400.0
        }
    );
    assert_eq!(store.scroll_range(root, Axis::Row), 200.0);
    assert_eq!(store.scroll_range(root, Axis::Column), 300.0);
    assert!(store.scrolls_on(root, Axis::Row) && store.scrolls_on(root, Axis::Column));

    let mut store = NodeStore::new();
    let (root, content) = scene(&mut store, 40.0, 400.0);
    assert_eq!(store.bounds(content).w, 100.0, "narrower content fills it");
    assert_eq!(store.scroll_range(root, Axis::Row), 0.0);
}

#[test]
fn a_wheel_moves_both_offsets_and_hands_on_what_it_cannot_take() {
    let mut store = NodeStore::new();
    let (root, content) = scene(&mut store, 300.0, 400.0);
    assert!(wheel(&mut store, root, 30.0, 50.0));
    assert_eq!(store.scroll(root), Vec2 { x: 30.0, y: 50.0 });
    let b = store.bounds(content);
    assert_eq!(
        store.world(content),
        Rect {
            x: b.x - 30.0,
            y: b.y - 50.0,
            ..b
        }
    );
    wheel(&mut store, root, 500.0, 0.0);
    assert_eq!(store.scroll(root).x, 200.0, "clamped across");

    // Inside a column viewport, a wheel the inner one has no room for down
    // goes on to the outer one; across it stays with the inner one.
    let mut store = NodeStore::new();
    let mut inner = None;
    let outer = {
        let mut cx = BuildCx::new(&mut store);
        cx.scroll(
            ScrollStyle {
                size: Sizing::fixed(100.0, 100.0),
                bars: ScrollBars::Hidden,
                ..Default::default()
            },
            |cx| {
                cx.flex(
                    viso_ui::FlexStyle {
                        axis: Axis::Column,
                        size: Sizing::fixed(100.0, 300.0),
                        ..Default::default()
                    },
                    |cx| {
                        inner = Some(
                            cx.scroll(
                                ScrollStyle {
                                    across: true,
                                    size: Sizing::fixed(100.0, 100.0),
                                    bars: ScrollBars::Hidden,
                                    ..Default::default()
                                },
                                |cx| {
                                    cx.leaf(LeafStyle {
                                        size: Sizing::fixed(300.0, 150.0),
                                        ..Default::default()
                                    });
                                },
                            )
                            .id(),
                        );
                    },
                );
            },
        );
        cx.root().unwrap()
    };
    let inner = inner.unwrap();
    store.layout(outer, surface(100.0, 100.0), &mut Vec::new());
    wheel(&mut store, outer, 20.0, 80.0);
    assert_eq!(store.scroll(inner), Vec2 { x: 20.0, y: 50.0 });
    assert_eq!(store.scroll(outer), Vec2::ZERO, "a notch ends at the end");
    wheel(&mut store, outer, 10.0, 30.0);
    assert_eq!(store.scroll(inner), Vec2 { x: 30.0, y: 50.0 });
    assert_eq!(store.scroll(outer), Vec2 { x: 0.0, y: 30.0 });
}

#[test]
fn read_right_to_left_the_content_starts_at_its_right_edge() {
    let mut store = NodeStore::new();
    let (root, content) = scene(&mut store, 300.0, 400.0);
    store.set_layout_direction(root, Some(LayoutDirection::Rtl));
    store.relayout_dirty(
        root,
        surface(100.0, 100.0),
        &mut Vec::new(),
        &mut Vec::new(),
    );
    assert_eq!(store.bounds(content).x, -200.0);
    assert_eq!(store.scroll_span(root, Axis::Row), (-200.0, 0.0));
    wheel(&mut store, root, -50.0, 10.0);
    assert_eq!(store.scroll(root), Vec2 { x: -50.0, y: 10.0 });
}

#[test]
fn classic_bars_keep_room_beside_and_under_and_stop_short_of_the_corner() {
    let mut store = NodeStore::new();
    let style = ScrollBarStyle::classic(false);
    store.set_scroll_bar_style(style);
    let t = style.track;
    let mut content = None;
    let root = {
        let mut cx = BuildCx::new(&mut store);
        cx.scroll(
            ScrollStyle {
                across: true,
                size: Sizing::fixed(100.0, 100.0),
                ..Default::default()
            },
            |cx| {
                content = Some(
                    cx.leaf(LeafStyle {
                        size: Sizing::fixed(300.0, 400.0),
                        ..Default::default()
                    })
                    .id(),
                );
            },
        );
        cx.root().unwrap()
    };
    store.layout(root, surface(100.0, 100.0), &mut Vec::new());
    let track = |axis| {
        let thumb = store.scroll_thumb_on(root, axis).expect("a bar each way");
        store.bounds(store.arena().parent(thumb).unwrap())
    };
    assert_eq!(
        track(Axis::Column),
        Rect {
            x: 100.0 - t,
            y: 0.0,
            w: t,
            h: 100.0 - t
        }
    );
    assert_eq!(
        track(Axis::Row),
        Rect {
            x: 0.0,
            y: 100.0 - t,
            w: 100.0 - t,
            h: t
        }
    );
    // The ranges reach the content's far edges past the bars.
    assert_eq!(store.scroll_range(root, Axis::Row), 300.0 - (100.0 - t));
    assert_eq!(store.scroll_range(root, Axis::Column), 400.0 - (100.0 - t));
    assert_eq!(store.bounds(content.unwrap()).h, 400.0);
}

struct Touch {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    chain: Vec<NodeId>,
}

fn touch(t: &mut Touch, at: (f32, f32), phase: PointerPhase) {
    PointerRouter::route_contact(
        &mut t.store,
        &mut t.states,
        &t.bindings,
        t.root,
        PointerContact {
            id: PointerId(1),
            direct: true,
        },
        PointerEvent {
            x: at.0,
            y: at.1,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Modifiers::default(),
        },
        &mut t.chain,
    );
}

/// A finger dragged from (80, 80) by `by` in steps, still down.
fn pan(by: (f32, f32)) -> Touch {
    let mut store = NodeStore::new();
    let (root, _) = scene(&mut store, 300.0, 400.0);
    let mut t = Touch {
        store,
        states: StateStore::new(),
        bindings: BindingTable::new(),
        root,
        chain: Vec::new(),
    };
    touch(&mut t, (80.0, 80.0), PointerPhase::Down);
    for k in 1..=5 {
        let f = k as f32 / 5.0;
        touch(
            &mut t,
            (80.0 + by.0 * f, 80.0 + by.1 * f),
            PointerPhase::Move,
        );
    }
    t
}

#[test]
fn a_slanted_pan_moves_both_ways_one_along_an_axis_keeps_to_it() {
    let t = pan((-40.0, -40.0));
    let at = t.store.scroll(t.root);
    assert!(at.x > 0.0 && at.y > 0.0, "both ways: {at:?}");
    let t = pan((-6.0, -50.0));
    let at = t.store.scroll(t.root);
    assert!(at.y > 0.0 && at.x == 0.0, "down only: {at:?}");
}
