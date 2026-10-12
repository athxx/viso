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

mod audio {
    use std::cell::Cell;
    use std::rc::Rc;

    use viso::ui::{
        BindingTable, BuildCx, NodeStore, SemanticProjector, StateStore, TextEdits, VirtualLists,
    };
    use viso_behavior::Module;
    use viso_behavior::game::AudioHost;
    use viso_behavior::native::Natives;
    use viso_dsl::frontend::{Origin, compile_file_for};
    use viso_dsl::hir::TargetProfile;
    use viso_view::{GameAudioDevice, GameSession, StartedAudio, game_session, install_game_audio};

    thread_local! {
        static STARTS: Cell<u32> = const { Cell::new(0) };
        static RELOADS: Cell<u32> = const { Cell::new(0) };
    }

    /// A device that renders nothing, counting the swaps it is handed.
    struct Quiet(AudioHost);

    impl GameAudioDevice for Quiet {
        fn reload(&mut self, module: &Module, natives: &Natives) -> Result<(), String> {
            RELOADS.with(|n| n.set(n.get() + 1));
            self.0.swapper().swap(&module.encode(), natives)
        }

        fn collect(&mut self) {}
    }

    fn start(module: &Module, natives: &Natives) -> Result<Option<StartedAudio>, String> {
        STARTS.with(|n| n.set(n.get() + 1));
        let built = AudioHost::new(&module.encode(), natives, 2, 64, 48_000.0)?;
        Ok(built.map(|(host, link)| (Box::new(Quiet(host)) as Box<dyn GameAudioDevice>, link)))
    }

    const SYNTH: &str = "
system Synth implements AudioProcess {
    action audio_process(block: AudioBlock) {}
}
";

    /// The game fixture's module, with an audio system when `synth`.
    fn module(synth: bool) -> Rc<Module> {
        let source = include_str!("fixtures/game.vs").replacen(
            "import viso::math::Vec3F32;",
            "import viso::math::Vec3F32;\nimport viso::game::{AudioProcess, AudioBlock};",
            1,
        );
        let source = if synth { source + SYNTH } else { source };
        let compiled = compile_file_for(
            &source,
            &Origin {
                package: "app".into(),
                module: vec!["game".into()],
                language: None,
            },
            Natives::standard(),
            TargetProfile::default(),
        );
        assert!(!compiled.has_errors(), "{:#?}", compiled.diagnostics);
        Rc::new(compiled.behavior.bytecode().expect("bytecode"))
    }

    #[test]
    fn a_game_starts_its_audio_when_a_reload_adds_it_and_swaps_it_after() {
        install_game_audio(Some(start));
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
        let session = game_session(&store, root).expect("a game");
        assert_eq!(STARTS.with(Cell::get), 1, "asked once");
        assert!(!session.borrow().has_audio(), "no audio system, no device");

        let swap = |session: &Rc<std::cell::RefCell<GameSession>>, synth| {
            let module = module(synth);
            let vm = GameSession::stage(&module).expect("links");
            session
                .borrow_mut()
                .reload(module, vm, None)
                .expect("swaps");
        };
        swap(&session, true);
        assert_eq!(STARTS.with(Cell::get), 2, "the reload added a synth");
        assert!(session.borrow().has_audio());
        assert!(
            session.borrow_mut().scheduler_mut().audio().is_some(),
            "linked"
        );

        swap(&session, true);
        assert_eq!(RELOADS.with(Cell::get), 1, "swapped on the running device");
        assert_eq!(STARTS.with(Cell::get), 2);
        install_game_audio(None);
    }
}
