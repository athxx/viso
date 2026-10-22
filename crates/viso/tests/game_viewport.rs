//! A `GameViewport` draws the World of the game its view runs: the frame
//! loop fills its scene after the game's frame, commits a new revision only
//! when what it draws moved, and hands each committed frame to the renderer
//! once; freeing the viewport lets its slot go.

use viso::ui::content::Content;
use viso::ui::input::Key;
use viso::ui::{
    BindingTable, BuildCx, EffectStore, NodeId, NodeStore, SceneSync, SemanticProjector,
    StateStore, TextEdits, VirtualLists,
};

const TICK: f64 = 1.0 / 60.0;

fn revision(store: &NodeStore, node: NodeId) -> u64 {
    match store.content_payload(node) {
        Some(Content::Scene { revision, .. }) => *revision,
        other => panic!("a scene, not {other:?}"),
    }
}

/// The frames a sync hands over, as their instance counts, and the slots it
/// releases.
fn sync(store: &mut NodeStore) -> (Vec<usize>, usize) {
    let (mut frames, mut released) = (Vec::new(), 0);
    store.scenes_mut().sync(|s| match s {
        SceneSync::Frame(_, frame) => frames.push(frame.instance_count()),
        SceneSync::Released(_) => released += 1,
    });
    (frames, released)
}

#[test]
fn a_viewport_draws_its_game_and_redraws_only_what_moved() {
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
        viso::view!("fixtures/viewport.vs")(&mut cx).id()
    };
    assert!(viso_view::take_game_faults().is_empty());
    let viewport = store.arena().children(root).next().expect("the viewport");
    assert!(store.focusable(viewport), "a press focuses it");
    assert_eq!(revision(&store, viewport), 0, "nothing drawn yet");

    store.run_frame_clients(TICK);
    let first = revision(&store, viewport);
    assert_eq!(first, 1, "the first frame draws");
    let (frames, released) = sync(&mut store);
    assert_eq!(released, 0);
    assert_eq!(frames.len(), 1, "one committed frame for the renderer");
    assert_eq!(frames, [2], "the ground and the player");
    assert_eq!(sync(&mut store).0.len(), 0, "handed over once");

    // A World standing still under a still camera draws nothing new.
    for _ in 0..3 {
        store.run_frame_clients(TICK);
    }
    let still = revision(&store, viewport);
    sync(&mut store);
    for _ in 0..3 {
        store.run_frame_clients(TICK);
    }
    assert_eq!(revision(&store, viewport), still, "the player rests");
    assert!(sync(&mut store).0.is_empty());

    // A moving player does.
    assert!(store.frame_clients_key(Key::Space, true));
    store.run_frame_clients(TICK);
    store.run_frame_clients(TICK);
    assert!(revision(&store, viewport) > still);
    assert_eq!(
        sync(&mut store).0.len(),
        1,
        "one frame however many commits"
    );

    let mut effects = EffectStore::new();
    store.free_tree(root, &mut effects, &mut Vec::new());
    assert_eq!(sync(&mut store).1, 1, "the freed viewport lets its slot go");
    assert!(store.scenes().is_empty());
}
