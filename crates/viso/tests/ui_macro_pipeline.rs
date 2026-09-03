//! `ui!` driven end to end through the full facade pipeline — the honest proof that
//! the compile-time expansion is not just well-typed tokens but a real retained tree
//! that layout, paint, the renderer, and the reactive flush all accept.
//!
//! Two complementary slices:
//!
//!   * the *structural* pipeline — a sized `ui!` fragment mounts, `store.layout` gives
//!     it real world geometry, `paint_tree` + `Renderer::upload/submit` run through the
//!     headless backend without panic, and `frame_stats` is observable. The emitter
//!     folds only the axis/size seam this slice (color/background folding is a
//!     consuming-slice concern), so the tree is intentionally invisible and the claim
//!     is on geometry, not pixels — asserting fills here would require styling the
//!     emitter does not yet produce.
//!
//!   * the *reactive* contract — a `text: count;` property compiled to a static binding
//!     edge, driven the way the facade's flush phase drives it (`set` -> `take_pending`
//!     -> `flush_state_transactions`), dirties exactly the bound node and class and
//!     walks exactly one *static* edge with zero dynamic fallback — the strict typed
//!     path a compiler-known binding must take.
//!
//! Everything is reached through the facade because the macro emits `::viso_ui::…`
//! paths and cannot itself depend on `viso-ui`; a normal app sees all of it via
//! `use viso::prelude::*;`.

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso::render::{Rect, Renderer};
use viso::ui::{
    BindingTable, BuildCx, DirtyClass, NodeStore, StateStore, StateValue, VirtualLists, paint_tree,
};

const W: u32 = 160;
const H: u32 = 100;

/// A sized `ui!` fragment mounts, lays out to real world bounds, and drives the
/// renderer through the headless backend without panic. The tree carries no fill
/// (the emitter folds only the axis/size seam), so it paints nothing visible — the
/// assertion is on the geometry layout assigned, the stage `ui!` actually owns.
#[test]
fn ui_fragment_lays_out_and_drives_the_renderer() {
    // A root sized in device pixels so layout has a concrete box to place, holding a
    // fixed-size child leaf. `width`/`height` fold to `Length::Fixed` in the emitter.
    let build = viso::ui! {
        Column {
            width: 120px;
            height: 60px;
            Leaf { width: 40px; height: 24px; }
        }
    };

    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        build(&mut cx).id()
    };

    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: W as f32,
        h: H as f32,
    };
    let mut scratch = Vec::new();
    store.layout(root, surface, &mut scratch);

    // The root is the layout entry: the surface rect is its imposed box, so it fills
    // the surface — that is the layout contract for the root, not its own `width`.
    let world = store.world(root);
    assert_eq!(
        (world.w, world.h),
        (W as f32, H as f32),
        "the root fills the surface it is laid out into"
    );

    // The child leaf, laid out *inside* the root, honors its own folded fixed size —
    // proof the `ui!`-emitted `width`/`height` reached the layout contract as real
    // `Length::Fixed` and were respected where the parent does not impose the box.
    let child = store
        .arena()
        .links(root)
        .unwrap()
        .first_child
        .expect("Column mounted its Leaf child");
    let child_world = store.world(child);
    assert_eq!(
        (child_world.w, child_world.h),
        (40.0, 24.0),
        "child leaf took its folded fixed size"
    );

    // The full render path accepts the tree. It is colorless, so `paint_tree` emits no
    // visible quad and the frame is empty draw work — correct, not a failure.
    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);
    assert!(
        primitives.is_empty(),
        "an unstyled ui! tree paints nothing visible this slice"
    );

    let mut gpu = HeadlessRaster::new();
    let surf = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surf);
    let mut renderer = Renderer::new(&mut gpu, format);
    renderer.upload(&mut gpu, &primitives);
    let stats = renderer.frame_stats();
    assert_eq!(stats.draw_calls, 0, "no visible quad, no draw call");
    assert_eq!(stats.instances, 0, "no visible quad, no instance");
    renderer.submit(&mut gpu, surf, [0.0, 0.0, 0.0, 1.0], [W as f32, H as f32]);
}

/// The reactive contract driven end to end: a `text: count;` property that the macro
/// compiled to a static binding edge dirties exactly the bound leaf and class when
/// `count` is written and flushed, and the flush walks exactly one *static* edge with
/// zero dynamic fallback — the path a compiler-known typed binding must take.
#[test]
fn ui_fragment_reactive_flush_hits_only_the_static_edge() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();

    let count = states.alloc(StateValue::Int(0));

    let build = viso::ui! {
        Column {
            Text { text: count; }
        }
    };

    let root = {
        let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
        build(&mut cx).id()
    };
    let text_leaf = store
        .arena()
        .links(root)
        .unwrap()
        .first_child
        .expect("the Column mounted its Text leaf");

    // Drive the flush phase the way the facade does: a write goes to the pending set,
    // `take_pending` drains it, and `flush_state_transactions` fans it through the
    // compiled binding edges once.
    store.clear_dirty();
    assert!(
        states.set(count, StateValue::Int(7)),
        "the write is a change"
    );
    let mut changed = Vec::new();
    states.take_pending(&mut changed);
    assert_eq!(changed.as_slice(), &[count], "exactly `count` is pending");
    let applied = store.flush_state_transactions(&changed, &bindings);

    // Exactly the one compiled edge fired, dirtying precisely the text-content class on
    // exactly the bound leaf.
    let text_class =
        DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT | DirtyClass::SEMANTICS;
    assert_eq!(
        applied, 1,
        "the write reached exactly the one compiled edge"
    );
    assert_eq!(
        store.dirty(text_leaf),
        text_class,
        "the text binding dirties precisely its dirty-class set"
    );

    // Strict typed path: the flush walked one static edge and never fell back to a
    // dynamic subscription (the compiler-known binding is never silently dynamic).
    let counters = bindings.counters();
    assert_eq!(
        counters.static_binding_eval(),
        1,
        "exactly one static edge walked"
    );
    assert_eq!(
        counters.dynamic_binding_eval(),
        0,
        "no dynamic edge walked for a typed binding"
    );
    assert_eq!(
        counters.dynamic_fallback_nodes(),
        0,
        "a compiler-known binding never falls back to dynamic"
    );
}
