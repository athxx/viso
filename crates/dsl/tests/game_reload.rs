//! The reload tier of a game edit: the compiler compares the running build's
//! Behavior IR with the candidate's by stable identity and picks the least
//! disruptive layer every change allows, and a swap applies it to the running
//! game.

use std::rc::Rc;

use viso_behavior::game::{Rebuild, Scheduler};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Value, Vm};
use viso_dsl::behavior::Program;
use viso_dsl::diag::Severity;
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::TargetProfile;
use viso_dsl::hotreload::game::{GameChangeKind, ReloadTier, SwapError, classify, swap};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

fn program_at(source: &str, tick_rate: u32) -> Program {
    let profile = TargetProfile {
        tick_rate,
        ..TargetProfile::default()
    };
    let compiled = compile_file_for(source, &origin(), Natives::standard(), profile);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    compiled.behavior
}

fn program(source: &str) -> Program {
    program_at(source, 60)
}

const GAME: &str = r#"
import viso::game::{Startup, GameStart, FixedUpdate, FixedFrame, FrameUpdate, RenderFrame,
    SpawnDesc, InputAction};
import viso::math::Vec3F32;

fn speed() -> F64 { 2.0 }

fn glow(x: F32) -> F32 { x * 0.5f32 }

system Setup implements Startup {
    action startup(cx: GameStart) {
        cx.spawn(SpawnDesc::player().at(Vec3F32::new(0.0f32, 1.0f32, 0.0f32)));
    }
}

system Mover implements FixedUpdate + FrameUpdate {
    state steps = 0;
    state best: I64 = 0;
    @local state shown = 0;

    action fixed_update(frame: FixedFrame) {
        steps += 1;
        for id in frame.world.entities() {
            frame.world.walk(id, speed(), 0.0f32);
        }
    }

    action frame_update(frame: RenderFrame) {
        shown = steps;
        let _ = glow(frame.alpha());
    }
}
"#;

/// The tier of editing `GAME` by replacing `from` with `to`.
fn edit(from: &str, to: &str) -> (ReloadTier, Vec<(GameChangeKind, String)>) {
    assert!(GAME.contains(from), "{from}");
    let reload = classify(&program(GAME), &program(&GAME.replace(from, to)));
    let changes = reload
        .changes
        .iter()
        .map(|c| (c.kind, c.name.clone()))
        .collect();
    (reload.tier, changes)
}

#[test]
fn moving_code_or_editing_comments_changes_nothing() {
    let moved = format!(
        "// a comment\n\n{}",
        GAME.replace("    state steps = 0;", "\n\n    state steps = 0;")
    );
    let reload = classify(&program(GAME), &program(&moved));
    assert_eq!(reload.tier, ReloadTier::Unchanged);
    assert!(reload.changes.is_empty());
    assert_eq!(reload.diagnostic(), None);
}

#[test]
fn a_fixed_hook_or_what_it_calls_is_a_logic_reload() {
    assert_eq!(
        edit("steps += 1;", "steps += 2;"),
        (
            ReloadTier::Logic,
            vec![(GameChangeKind::Simulation, "Mover.fixed_update".to_owned())]
        )
    );
    assert_eq!(
        edit("F64 { 2.0 }", "F64 { 3.0 }"),
        (
            ReloadTier::Logic,
            vec![(GameChangeKind::Simulation, "speed".to_owned())]
        )
    );
}

#[test]
fn frame_code_and_local_states_are_presentation_only() {
    assert_eq!(
        edit("x * 0.5f32", "x * 0.25f32"),
        (
            ReloadTier::Presentation,
            vec![(GameChangeKind::Presentation, "glow".to_owned())]
        )
    );
    assert_eq!(
        edit("@local state shown = 0;", "@local state shown = 1;"),
        (
            ReloadTier::Presentation,
            vec![(GameChangeKind::LocalState, "Mover.shown".to_owned())]
        )
    );
}

#[test]
fn a_simulation_state_change_migrates_states() {
    assert_eq!(
        edit("state best: I64 = 0;", "state best: I64 = 5;"),
        (
            ReloadTier::LogicMigration,
            vec![(GameChangeKind::StateInitializer, "Mover.best".to_owned())]
        )
    );
    assert_eq!(
        edit("state best: I64 = 0;", "state best: F64 = 0.0;").1,
        [(GameChangeKind::StateRetyped, "Mover.best".to_owned())]
    );
    assert_eq!(
        edit(
            "state best: I64 = 0;",
            "state best: I64 = 0;\n    state lives = 3;"
        )
        .1,
        [(GameChangeKind::StateAdded, "Mover.lives".to_owned())]
    );
    assert_eq!(
        edit("state best: I64 = 0;", "").1,
        [(GameChangeKind::StateRemoved, "Mover.best".to_owned())]
    );
}

#[test]
fn a_start_change_rebuilds_the_world_and_outranks_the_rest() {
    let (tier, changes) = edit("0.0f32, 1.0f32, 0.0f32", "0.0f32, 2.0f32, 0.0f32");
    assert_eq!(tier, ReloadTier::WorldRebuild);
    assert_eq!(
        changes,
        [(GameChangeKind::Start, "Setup.startup".to_owned())]
    );

    let both = GAME
        .replace("steps += 1;", "steps += 2;")
        .replace("0.0f32, 1.0f32, 0.0f32", "0.0f32, 2.0f32, 0.0f32");
    let reload = classify(&program(GAME), &program(&both));
    assert_eq!(reload.tier, ReloadTier::WorldRebuild);
    let kinds: Vec<_> = reload.changes.iter().map(|c| c.kind).collect();
    assert_eq!(kinds, [GameChangeKind::Start, GameChangeKind::Simulation]);
}

#[test]
fn hooks_and_systems_added_or_removed_take_their_phase() {
    let added = GAME.replace(
        "system Setup implements Startup {",
        "system Extra implements FixedUpdate {\n    action fixed_update(frame: FixedFrame) {}\n}\n\nsystem Setup implements Startup {",
    );
    let reload = classify(&program(GAME), &program(&added));
    assert_eq!(reload.tier, ReloadTier::Logic);
    assert_eq!(reload.changes[0].kind, GameChangeKind::Simulation);
    assert_eq!(reload.changes[0].name, "Extra.fixed_update");

    let reload = classify(&program(&added), &program(GAME));
    assert_eq!(reload.tier, ReloadTier::Logic);
    assert_eq!(reload.changes[0].name, "Extra.fixed_update");

    let (tier, changes) = edit(
        "system Mover implements FixedUpdate + FrameUpdate {",
        "@before(Setup)\nsystem Mover implements FixedUpdate + FrameUpdate {",
    );
    assert_eq!(tier, ReloadTier::Logic);
    assert_eq!(
        changes,
        [(GameChangeKind::SystemOrder, "systems".to_owned())]
    );
}

#[test]
fn the_input_map_and_the_tick_rate_are_logic_reloads() {
    let mapped = GAME.replace(
        "fn speed()",
        "import viso::game::{InputMap, Key};\n\
         const KEYS: InputMap<InputAction> = InputMap::new().key(Key::K, InputAction::jump);\n\
         fn speed()",
    );
    let reload = classify(&program(GAME), &program(&mapped));
    assert_eq!(reload.tier, ReloadTier::Logic);
    assert_eq!(reload.changes[0].kind, GameChangeKind::InputMap);

    let reload = classify(&program(GAME), &program_at(GAME, 30));
    assert_eq!(reload.tier, ReloadTier::Logic);
    assert_eq!(reload.changes[0].kind, GameChangeKind::TickRate);
}

#[test]
fn the_reload_note_names_the_deciding_change_first() {
    let both = GAME
        .replace("x * 0.5f32", "x * 0.25f32")
        .replace("steps += 1;", "steps += 2;");
    let reload = classify(&program(GAME), &program(&both));
    let note = reload.diagnostic().expect("a note");
    assert_eq!(note.code, "E5103");
    assert_eq!(note.severity, Severity::Note);
    assert_eq!(
        note.message,
        "game reload: logic-only (simulation code changed: `Mover.fixed_update`)"
    );
    let primary =
        &both[note.primary.start().to_u32() as usize..note.primary.end().to_u32() as usize];
    assert!(primary.contains("steps += 2;"), "{primary}");
    assert_eq!(note.related.len(), 1);
    assert_eq!(note.related[0].label, "presentation code changed: `glow`");
}

fn running(program: &Program) -> Scheduler {
    let module = Rc::new(program.bytecode().expect("verified bytecode"));
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    Scheduler::new(vm).expect("a started game")
}

fn steps(game: &Scheduler) -> Value {
    let module = game.vm().module();
    let index = module
        .systems()
        .iter()
        .position(|s| module.components()[s.component as usize].name.as_ref() == "Mover")
        .expect("Mover");
    let slot = module
        .layout(module.systems()[index].component)
        .state("steps")
        .expect("steps");
    game.instance(index).states()[slot].clone()
}

#[test]
fn a_swap_applies_each_tier_to_the_running_game() {
    let last_good = program(GAME);
    let mut game = running(&last_good);
    game.step(5);
    let build = game.build();

    let same = swap(
        &mut game,
        &last_good,
        &program(GAME),
        &Natives::standard(),
        &[],
        Rebuild::Fresh,
    )
    .expect("swaps");
    assert_eq!(
        (same.reload.tier, same.restored, same.carried),
        (ReloadTier::Unchanged, None, 0)
    );
    assert_eq!(game.build(), build, "nothing swapped");

    let faster = program(&GAME.replace("steps += 1;", "steps += 2;"));
    let swapped = swap(
        &mut game,
        &last_good,
        &faster,
        &Natives::standard(),
        &[],
        Rebuild::Fresh,
    )
    .expect("swaps");
    assert_eq!(swapped.reload.tier, ReloadTier::Logic);
    assert_eq!(swapped.restored.map(|r| r.states), Some(2));
    assert_eq!(game.clock().tick(), 5);
    game.step(1);
    assert_eq!(
        steps(&game),
        Value::Int(7),
        "the new logic runs on the kept state"
    );

    let moved = program(
        &GAME
            .replace("steps += 1;", "steps += 2;")
            .replace("0.0f32, 1.0f32, 0.0f32", "0.0f32, 2.0f32, 0.0f32"),
    );
    let walked = game.world().entities()[0];
    let at = game.world().position(walked).expect("alive").to_array();
    let swapped = swap(
        &mut game,
        &faster,
        &moved,
        &Natives::standard(),
        &[],
        Rebuild::KeepCharacters,
    )
    .expect("swaps");
    assert_eq!(
        (swapped.reload.tier, swapped.carried),
        (ReloadTier::WorldRebuild, 1)
    );
    assert_eq!(game.clock().tick(), 0);
    let player = game.world().entities()[0];
    assert_eq!(game.world().position(player).expect("alive").to_array(), at);

    let broken = program(&GAME.replace(
        "cx.spawn(SpawnDesc::player()",
        "let xs = [1]; let _ = xs[2]; cx.spawn(SpawnDesc::player()",
    ));
    game.step(3);
    let before = game.snapshot();
    let error = swap(
        &mut game,
        &moved,
        &broken,
        &Natives::standard(),
        &[],
        Rebuild::Fresh,
    )
    .expect_err("the start faults");
    assert!(
        matches!(error, SwapError::Fault(ref f) if f.code == "E7104"),
        "{error}"
    );
    assert_eq!(game.snapshot(), before, "the last good game runs on");
}
