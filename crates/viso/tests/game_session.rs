//! A view whose module declares systems runs a game on its root: the frame
//! loop advances it by the frame's wall delta, the keys no UI node takes
//! reach it, focus moving into the UI releases them, and freeing the root
//! ends it.

use viso::ui::input::Key;
use viso::ui::{
    BindingTable, BuildCx, EffectStore, NodeStore, SemanticProjector, StateStore, TextEdits,
    VirtualLists,
};
use viso_behavior::Value;
use viso_view::game_session;

const TICK: f64 = 1.0 / 60.0;

#[test]
fn a_mounted_game_runs_on_the_frame_loop_until_its_root_goes() {
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
        viso::view!("fixtures/game.vs")(&mut cx).id()
    };
    assert!(viso_view::take_game_faults().is_empty());
    let session = game_session(&store, root).expect("the root runs the game");
    assert!(store.frame_clients_want_frame());
    let steps = || session.borrow().state("Runner", "steps");
    assert_eq!(steps(), Some(Value::Int(0)), "started, no tick yet");

    // Three frames of one tick each.
    for _ in 0..3 {
        store.run_frame_clients(TICK);
    }
    assert_eq!(steps(), Some(Value::Int(3)));
    let world = session.borrow().scheduler().world().entities().len();
    assert_eq!(world, 1, "the start spawned the player");

    // Space is the jump; it reaches the game while nothing is focused.
    assert!(store.frame_clients_key(Key::Space, true));
    store.run_frame_clients(TICK);
    store.frame_clients_key(Key::Space, false);
    store.run_frame_clients(TICK);
    assert_eq!(
        session.borrow().state("Runner", "jumps"),
        Some(Value::Int(1))
    );

    // A key held as the window loses focus is released, not stuck.
    store.frame_clients_key(Key::Space, true);
    store.release_client_input();
    store.run_frame_clients(TICK);
    store.run_frame_clients(TICK);
    assert_eq!(
        session.borrow().state("Runner", "jumps"),
        Some(Value::Int(2)),
        "one press, then released"
    );

    let mut effects = EffectStore::new();
    store.free_tree(root, &mut effects, &mut Vec::new());
    store.run_frame_clients(TICK);
    assert_eq!(store.frame_client_count(), 0, "the game ends with its root");
    assert_eq!(
        std::rc::Rc::strong_count(&session),
        1,
        "only the test holds it"
    );
}

mod facade_loop {
    use std::cell::Cell;
    use std::time::Duration;

    use viso::prelude::*;
    use viso::ui::NodeId;
    use viso_behavior::Value;
    use viso_platform::{KeyCode, Modifiers, RawEvent, RawKey, WindowId};
    use viso_view::game_session;

    thread_local! {
        static ROOT: Cell<Option<NodeId>> = const { Cell::new(None) };
    }

    struct Game;

    impl Application for Game {
        fn new(_cx: &mut AppCx) -> Self {
            Game
        }

        fn build(&mut self, cx: &mut BuildCx<'_>) {
            let root = viso::view!("fixtures/game.vs")(cx).id();
            ROOT.with(|r| r.set(Some(root)));
        }
    }

    fn key(pressed: bool) -> RawEvent {
        RawEvent::Key(RawKey {
            window: WindowId(1),
            code: KeyCode::Space,
            pressed,
            repeat: false,
            modifiers: Modifiers::default(),
        })
    }

    #[test]
    fn the_frame_loop_ticks_the_game_and_hands_it_the_unfocused_keys() {
        let step = Duration::from_secs_f64(1.0 / 60.0);
        let app = viso::__test_support::drive_scripted_frames::<Game>(
            vec![key(true), key(false)],
            step,
            12,
        );
        let root = ROOT.with(Cell::get).expect("built");
        let session = game_session(app.store(), root).expect("the view runs its game");
        let session = session.borrow();
        let Some(Value::Int(steps)) = session.state("Runner", "steps") else {
            panic!("a step count");
        };
        assert!((10..=12).contains(&steps), "one tick a frame: {steps}");
        assert_eq!(
            session.state("Runner", "jumps"),
            Some(Value::Int(1)),
            "the key reached the game"
        );
        assert!(
            app.wants_animation(),
            "a running game keeps the frames coming"
        );
    }
}
