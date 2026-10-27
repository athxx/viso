//! The `ui!` proc-macro end to end through the facade (DSL source forms; declarative
//! syntax is not rebuild semantics).
//!
//! `ui!` runs the *shared* Viso DSL frontend at Rust compile time and expands to a
//! static `BuildCx` builder closure — no runtime parse, no per-frame rebuild. These
//! tests exercise the expansion the only way that proves it: by taking the emitted
//! `|cx: &mut BuildCx| -> Handle` closure and mounting it into a real `NodeStore`,
//! then asserting on the retained tree and on the values its nodes declare. A
//! fragment is a stateless view: one that reads state is a `component!`.
//!
//! The macro lives in the compile-time-only `viso-ui-macros` crate and emits
//! `::viso_ui::…` paths; it resolves here because the facade re-exports both `ui!`
//! and the `viso_ui` builder types. A normal app reaches all of this through
//! `use viso::prelude::*;`.

use viso::ui::{
    BindingTable, BuildCx, NodeStore, SemanticProjector, StateStore, TextEdits, VirtualLists,
};

/// A static-only fragment expands to a builder closure that mounts a real retained
/// tree: a Column flex with one Text leaf child. No bindings, no reactive stores.
#[test]
fn static_fragment_mounts_a_retained_tree() {
    let build = viso::ui! {
        Column {
            Text { }
        }
    };

    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        let root = build(&mut cx);
        // The closure returned the Column's handle, and it is the build root.
        assert_eq!(cx.root(), Some(root.id()), "the Column is the mounted root");
        root.id()
    };

    // Column (flex) with exactly one child (the Text leaf), and no siblings.
    let links = store.arena().links(root).expect("root is live");
    let child = links.first_child.expect("Column mounted its Text child");
    assert!(
        store.arena().links(child).unwrap().next_sibling.is_none(),
        "Column mounted exactly one child"
    );
}

/// A fragment's constant control values — a string, a color, an enum variant
/// of the prelude, an option — reach the node's text request: the fragment is
/// checked as a component's view, so `TextAlign::center` names the prelude's
/// variant, not a Rust variable.
#[test]
fn constant_control_values_reach_the_text_request() {
    use std::num::NonZeroU32;
    use viso::ui::{FieldSeed, FontSize, NodeId, TextAlign, TextOverflow, TextRequest};

    let build = viso::ui! {
        Column {
            Text {
                text: "Title";
                color: #ff0000;
                align: TextAlign::center;
                max_lines: Option::Some(2);
                overflow: TextOverflow::ellipsis;
            }
        }
    };
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        build(&mut cx).id()
    };
    let text = store.arena().links(root).unwrap().first_child.unwrap();
    let mut requests: Vec<(NodeId, Box<TextRequest>)> = Vec::new();
    store.take_text_requests(&mut requests);
    let mut seeds: Vec<(NodeId, FieldSeed)> = Vec::new();
    store.take_text_seeds(&mut seeds);
    let (id, request) = requests
        .into_iter()
        .map(|(id, r)| (id, *r))
        .chain(seeds.into_iter().map(|(id, seed)| (id, seed.request)))
        .find(|(_, r)| r.text == "Title")
        .expect("the text is declared");
    assert_eq!(id, text);
    let c = request.color;
    assert_eq!((c.r, c.g, c.b, c.a), (1.0, 0.0, 0.0, 1.0));
    assert_eq!(request.font_size, FontSize::Inherited);
    assert_eq!(request.style.align, TextAlign::Center);
    assert_eq!(request.style.max_lines, NonZeroU32::new(2));
    assert_eq!(request.style.overflow, TextOverflow::Ellipsis);
    assert_eq!(
        bindings.footprint().absolute_edges,
        0,
        "a constant value binds nothing"
    );
}

/// `px` and `sp` lengths compile to bound term sums the store folds at layout
/// against the window's scale factor and text scale, and re-fold when the
/// environment moves. (Rust's lexer reads `1.5em` as a malformed exponent, so an
/// `em` length is authored in `.vs`.)
#[test]
fn environment_lengths_fold_at_layout_and_follow_the_environment() {
    use viso::render::Rect;
    use viso::ui::LengthEnv;

    let build = viso::ui! {
        Row {
            Column {
                width: 100dp;
                Text { font_size: 20dp; width: 50% - 20dp; height: 2sp + 4px; }
            }
        }
    };
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        build(&mut cx).id()
    };
    let column = store.arena().links(root).unwrap().first_child.unwrap();
    let text = store.arena().links(column).unwrap().first_child.unwrap();
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };
    store.layout(root, surface, &mut Vec::new());
    assert_eq!(
        store.bounds(text).w,
        30.0,
        "50% of the column's 100dp, less 20dp"
    );
    assert_eq!(store.bounds(text).h, 6.0, "2sp + 4px at scale 1");
    assert_eq!(store.resolved_font_size(text), Some(20.0));

    store.set_length_env(LengthEnv {
        scale_factor: 2.0,
        text_scale: 1.5,
        ..LengthEnv::default()
    });
    store.layout(root, surface, &mut Vec::new());
    assert_eq!(
        store.bounds(text).w,
        30.0,
        "a dp-and-percent width reads no environment"
    );
    assert_eq!(
        store.bounds(text).h,
        5.0,
        "2sp + 4px at text scale 1.5, scale 2"
    );
}

/// A constant padding pads the container's content: plain `dp` as the built
/// style, an `sp` edge or one whose `start` and `end` differ as bound terms the
/// store folds at layout against the text scale and the layout direction.
#[test]
fn padding_pads_the_content_and_follows_the_environment() {
    use viso::render::Rect;
    use viso::ui::LengthEnv;
    use viso::ui::adaptive::LayoutDirection;

    let build = viso::ui! {
        Column {
            padding: 8dp;
            Column {
                width: 100dp;
                padding: EdgeInsets { start: 6dp, top: 1sp };
                Text { width: 10dp; height: 10dp; }
            }
            Grid {
                padding: EdgeInsets::all(3dp);
                Text { width: 10dp; height: 10dp; }
            }
        }
    };
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        build(&mut cx).id()
    };
    let inner = store.arena().links(root).unwrap().first_child.unwrap();
    let text = store.arena().links(inner).unwrap().first_child.unwrap();
    let grid = store.arena().links(inner).unwrap().next_sibling.unwrap();
    let cell = store.arena().links(grid).unwrap().first_child.unwrap();
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };
    store.layout(root, surface, &mut Vec::new());
    assert_eq!((store.bounds(inner).x, store.bounds(inner).y), (8.0, 8.0));
    assert_eq!((store.bounds(text).x, store.bounds(text).y), (14.0, 9.0));
    let (g, c) = (store.bounds(grid), store.bounds(cell));
    assert_eq!(
        (c.x - g.x, c.y - g.y),
        (3.0, 3.0),
        "the grid pads its cells"
    );

    store.set_length_env(LengthEnv {
        text_scale: 2.0,
        direction: LayoutDirection::Rtl,
        ..LengthEnv::default()
    });
    store.layout(root, surface, &mut Vec::new());
    assert_eq!(
        store.bounds(inner).x,
        400.0 - 8.0 - 100.0,
        "a column aligns its children's start to its right"
    );
    assert_eq!(
        (store.bounds(text).x, store.bounds(text).y),
        (400.0 - 8.0 - 6.0 - 10.0, 10.0),
        "`start` is the right edge right to left; `top` follows the text scale"
    );
}

/// Sizing bounds, a margin and a `%` padding read the parent's content box: the
/// constant ones as the node's box rules, a margin in `sp` as bound terms that
/// follow the text scale.
#[test]
fn box_rules_read_the_parent_content_box_and_follow_the_environment() {
    use viso::render::Rect;
    use viso::ui::LengthEnv;

    let build = viso::ui! {
        Column {
            Column {
                width: 200dp;
                height: 100dp;
                padding: 10%;
                Text { width: 10dp; height: 10dp; }
            }
            Text { width: 500dp; max_width: 50%; height: 10dp; }
            Text { width: 10dp; min_width: 30dp; height: 10dp; margin: EdgeInsets { start: 6dp, top: 4dp }; }
            Text { width: 10dp; height: 10dp; margin: EdgeInsets { start: 2sp }; }
        }
    };
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        build(&mut cx).id()
    };
    let mut children = Vec::new();
    let mut at = store.arena().links(root).unwrap().first_child;
    while let Some(child) = at {
        children.push(child);
        at = store.arena().links(child).unwrap().next_sibling;
    }
    let padded = store
        .arena()
        .links(children[0])
        .unwrap()
        .first_child
        .unwrap();
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };
    store.layout(root, surface, &mut Vec::new());
    let b = |store: &NodeStore, node| {
        let b = store.bounds(node);
        (b.x, b.y, b.w)
    };
    assert_eq!(
        b(&store, padded),
        (40.0, 30.0, 10.0),
        "10% of the 400×300 content box"
    );
    assert_eq!(
        b(&store, children[1]),
        (0.0, 100.0, 200.0),
        "at most half of 400dp"
    );
    assert_eq!(
        b(&store, children[2]),
        (6.0, 114.0, 30.0),
        "at least 30dp, past its margin"
    );
    assert_eq!(b(&store, children[3]).0, 2.0);

    store.set_length_env(LengthEnv {
        text_scale: 2.0,
        ..LengthEnv::default()
    });
    store.layout(root, surface, &mut Vec::new());
    assert_eq!(b(&store, children[3]).0, 4.0, "2sp at text scale 2");
}

/// A grid's tracks, gaps, cell alignment and areas, and its children's
/// cells, lay out from the macro as from the package; a row gap in `sp`
/// follows the text scale.
#[test]
fn a_grid_places_its_children_by_tracks_areas_and_cells() {
    use viso::render::Rect;
    use viso::ui::LengthEnv;

    let build = viso::ui! {
        Grid {
            width: 300dp;
            height: 200dp;
            columns: [Track::fixed(100dp), Track::fr(1.0), Track::fr(2.0)];
            rows: tracks(2, Track::fixed(40dp));
            column_gap: 10dp;
            row_gap: 14sp;
            align_items: GridAlign::center;
            areas: ["head head head", "side main main", "side main main"];
            Text { width: 10dp; height: 10dp; grid.area: "main"; }
            Text { width: 10dp; height: 10dp; grid.column: 0; grid.row: 2; }
            Text { width: 10dp; height: 10dp; grid.column: 2; }
        }
    };
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        build(&mut cx).id()
    };
    let mut children = Vec::new();
    let mut at = store.arena().links(root).unwrap().first_child;
    while let Some(child) = at {
        children.push(child);
        at = store.arena().links(child).unwrap().next_sibling;
    }
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 300.0,
        h: 200.0,
    };
    store.layout(root, surface, &mut Vec::new());
    let at = |store: &NodeStore, node| {
        let b = store.bounds(node);
        (b.x, b.y)
    };
    assert_eq!(at(&store, children[0]), (110.0, 81.0));
    assert_eq!(at(&store, children[1]), (0.0, 108.0));
    assert_eq!(at(&store, children[2]), (180.0, 15.0));

    store.set_length_env(LengthEnv {
        text_scale: 2.0,
        ..LengthEnv::default()
    });
    store.layout(root, surface, &mut Vec::new());
    assert_eq!(
        at(&store, children[1]),
        (0.0, 136.0),
        "14sp at text scale 2"
    );
}

/// A `ui!` stack aligns and layers its children and an absolute container
/// pins its children by their insets, an inset of `sp` following the text
/// scale.
#[test]
fn a_stack_and_an_absolute_place_their_children() {
    use viso::render::Rect;
    use viso::ui::LengthEnv;

    let build = viso::ui! {
        Column {
            Stack {
                content_align: Alignment2D::center;
                padding: 10dp;
                Text { width: 100dp; height: 60dp; stack.layer: 1; }
                Text { width: 40dp; height: 20dp; stack.align: Alignment2D::bottom_end; }
            }
            Absolute {
                width: 200dp;
                height: 100dp;
                Text { width: 20dp; height: 10dp; absolute.end: 10%; absolute.bottom: 4sp; }
            }
        }
    };
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        build(&mut cx).id()
    };
    let children = |store: &NodeStore, node| {
        let mut out = Vec::new();
        let mut at = store.arena().links(node).unwrap().first_child;
        while let Some(child) = at {
            out.push(child);
            at = store.arena().links(child).unwrap().next_sibling;
        }
        out
    };
    let [stack, pinned] = children(&store, root)[..] else {
        panic!("two containers");
    };
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 300.0,
        h: 200.0,
    };
    store.layout(root, surface, &mut Vec::new());
    let at = |store: &NodeStore, node| {
        let b = store.bounds(node);
        (b.x, b.y, b.w, b.h)
    };
    assert_eq!(at(&store, stack), (0.0, 0.0, 120.0, 80.0));
    let stacked = children(&store, stack);
    assert_eq!(at(&store, stacked[0]), (10.0, 10.0, 100.0, 60.0));
    assert_eq!(at(&store, stacked[1]), (70.0, 50.0, 40.0, 20.0));
    let text = children(&store, pinned)[0];
    assert_eq!(at(&store, text), (160.0, 166.0, 20.0, 10.0));

    store.set_length_env(LengthEnv {
        text_scale: 2.0,
        ..LengthEnv::default()
    });
    store.layout(root, surface, &mut Vec::new());
    assert_eq!(at(&store, text).1, 162.0, "4sp at text scale 2");
}

#[test]
fn a_drag_source_and_a_drop_target_mount_with_its_preview_put_away() {
    let build = viso::ui! {
        Column {
            width: 300dp;
            height: 300dp;
            Stack {
                width: 100dp;
                height: 40dp;
                drag: "7";
                drag_kind: "card";
                DragPreview { Column { width: 100dp; height: 40dp; } }
            }
            Column { width: 300dp; height: 100dp; drop_accepts: ["card"]; }
        }
    };
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        build(&mut cx).id()
    };
    let mut kids = store.arena().children(root);
    let (card, zone) = (kids.next().unwrap(), kids.next().unwrap());
    let preview = store.arena().children(card).next().expect("its preview");
    assert!(store.is_drag_source(card) && store.is_drop_target(zone));
    assert!(store.is_layer(preview) && store.hidden(preview));
}

#[test]
fn a_sheet_mounts_against_its_edge_resting_at_its_detents() {
    use viso::render::Rect;

    let build = viso::ui! {
        Column {
            width: 400dp;
            height: 300dp;
            Sheet {
                edge: SheetEdge::top;
                detents: [50%, 200dp];
                dismissible: false;
                Column { width: 400dp; height: 240dp; }
            }
        }
    };
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        build(&mut cx).id()
    };
    let sheet = store.arena().children(root).next().expect("the sheet");
    let panel = store.arena().children(sheet).next().expect("its panel");
    assert!(store.is_sheet(sheet) && store.is_sheet_panel(panel) && store.hidden(sheet));
    store.set_reduced_motion(true);
    store.set_sheet_detent(sheet, 1);
    store.present_sheet(sheet, true);
    let surface = Rect::new(0.0, 0.0, 400.0, 300.0);
    store.layout(root, surface, &mut Vec::new());
    assert_eq!(store.sheet_shown(sheet), Some(200.0));
    assert_eq!(
        store.world(panel).y,
        -40.0,
        "against the top, 200 of its 240 showing"
    );
}

/// A hand-written component: what a fragment places by its type name, built
/// from its `Default`.
#[derive(Default)]
struct Swatch;

impl viso::ui::Component for Swatch {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let leaf = cx.leaf(viso::ui::LeafStyle::default());
        cx.semantics(
            leaf,
            viso::ui::Semantics {
                role: viso::ui::Role::Image,
                label: Some("swatch".into()),
                ..Default::default()
            },
        );
    }
}

viso::component! {
    Tally {
        state count = 0;
        view { Button { text: "Tally"; on click { count += 1; } } }
    }
}

/// A fragment mounts a hand-written `Component` and a `component!` alike:
/// each by its type name, each the root it builds.
#[test]
fn a_fragment_mounts_hand_written_and_declared_components_alike() {
    let build = viso::ui! {
        Column {
            Swatch {}
            Tally {}
        }
    };
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        build(&mut cx).id()
    };
    let kids: Vec<_> = store.arena().children(root).collect();
    assert_eq!(
        kids.len(),
        2,
        "each component built its root under the Column"
    );
    let said = |id| {
        let s = store.semantics(id).expect("speaks");
        (s.role, s.label.clone())
    };
    assert_eq!(
        said(kids[0]),
        (viso::ui::Role::Image, Some("swatch".into()))
    );
    assert_eq!(
        said(kids[1]),
        (viso::ui::Role::Button, Some("Tally".into()))
    );
}

/// A fragment whose root is a hand-written component returns its root.
#[test]
fn a_hand_written_component_is_a_fragments_root() {
    let build = viso::ui! { Swatch {} };
    let mut store = NodeStore::new();
    let mut cx = BuildCx::new(&mut store);
    let root = build(&mut cx);
    assert_eq!(cx.root(), Some(root.id()));
}

/// A wrap breaks its children into lines at its width, its gaps between
/// them and between its lines; a row gap in `sp` follows the text scale.
#[test]
fn a_wrap_breaks_its_children_into_lines() {
    use viso::render::Rect;
    use viso::ui::LengthEnv;

    let build = viso::ui! {
        Column {
            width: 100dp;
            height: 200dp;
            Wrap {
                gap: 10dp;
                row_gap: 5sp;
                Text { width: 30dp; height: 10dp; }
                Text { width: 30dp; height: 20dp; }
                Text { width: 30dp; height: 10dp; }
                Text { width: 50dp; height: 10dp; }
            }
            Text { width: 10dp; height: 10dp; }
        }
    };
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        build(&mut cx).id()
    };
    let children = |store: &NodeStore, node| {
        let mut out = Vec::new();
        let mut at = store.arena().links(node).unwrap().first_child;
        while let Some(child) = at {
            out.push(child);
            at = store.arena().links(child).unwrap().next_sibling;
        }
        out
    };
    let (wrap, below) = match children(&store, root)[..] {
        [wrap, below] => (wrap, below),
        _ => panic!("a wrap and a text"),
    };
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 200.0,
    };
    store.layout(root, surface, &mut Vec::new());
    let at = |store: &NodeStore, node| {
        let b = store.bounds(node);
        (b.x, b.y)
    };
    let placed: Vec<_> = children(&store, wrap)
        .iter()
        .map(|&k| at(&store, k))
        .collect();
    assert_eq!(placed, [(0.0, 0.0), (40.0, 0.0), (0.0, 25.0), (40.0, 25.0)]);
    assert_eq!(
        at(&store, below),
        (0.0, 35.0),
        "the next sibling follows its lines"
    );

    store.set_length_env(LengthEnv {
        text_scale: 2.0,
        ..LengthEnv::default()
    });
    store.layout(root, surface, &mut Vec::new());
    assert_eq!(at(&store, below), (0.0, 40.0), "10sp between the lines");
}
