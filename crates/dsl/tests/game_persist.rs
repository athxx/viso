//! `@persist` state: loaded before the start, written at tick boundaries once
//! an interval when it changed, made durable on suspend and drop, carried
//! across a type change by the conversion matrix or a `@migrate` function,
//! and across a process restart; `E9106` at compile time, `E6103` and
//! `E9111` at run time.

use std::cell::Cell;
use std::rc::Rc;

use viso_behavior::game::{
    MemoryStore, PERSIST_CAPABILITY, Persist, PersistStore, Rebuild, Scheduler,
};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::{CapabilitySet, TargetProfile};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

fn profile(granted: bool) -> TargetProfile {
    let mut capabilities = CapabilitySet::new();
    if granted {
        capabilities.insert(PERSIST_CAPABILITY);
    }
    TargetProfile {
        tick_rate: 4,
        capabilities,
        ..TargetProfile::default()
    }
}

const IMPORTS: &str = "import viso::game::{Startup, GameStart, FixedUpdate, FixedFrame};\n";

fn compiled(source: &str, granted: bool) -> viso_dsl::frontend::Compiled {
    compile_file_for(
        &format!("{IMPORTS}{source}"),
        &origin(),
        Natives::standard(),
        profile(granted),
    )
}

fn errors(source: &str, granted: bool) -> Vec<(String, String)> {
    compiled(source, granted)
        .errors()
        .map(|d| (d.code.to_string(), d.message.clone()))
        .collect()
}

/// A VM of `source` linked with `storage.persist` granted or not, persisting
/// through `store`.
fn vm(source: &str, store: impl PersistStore + 'static, grant: bool) -> Vm {
    let compiled = compiled(source, true);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = compiled.behavior.bytecode().expect("verified bytecode");
    let mut vm = Vm::new(Rc::new(module), Budget::default());
    let grants: &[&str] = if grant { &[PERSIST_CAPABILITY] } else { &[] };
    vm.link(&Natives::standard(), grants).expect("link");
    vm.services_mut().insert(Persist::new(store));
    vm
}

fn start(source: &str, store: impl PersistStore + 'static) -> Scheduler {
    Scheduler::new(vm(source, store, true)).expect("a started game")
}

fn state(game: &Scheduler, name: &str) -> Value {
    let module = game.vm().module();
    let slot = module
        .layout(module.systems()[0].component)
        .state(name)
        .unwrap_or_else(|| panic!("no state {name}"));
    game.instance(0).states()[slot].clone()
}

/// A store counting the blobs it is given.
#[derive(Clone, Default)]
struct Counting {
    inner: MemoryStore,
    stores: Rc<Cell<u32>>,
}

impl PersistStore for Counting {
    fn load(&mut self, key: &str) -> Result<Option<Vec<u8>>, String> {
        self.inner.load(key)
    }

    fn store(&mut self, key: &str, blob: Vec<u8>) {
        self.stores.set(self.stores.get() + 1);
        self.inner.store(key, blob);
    }

    fn flush(&mut self) -> Result<(), String> {
        self.inner.flush()
    }
}

const PROGRESS: &str = r#"
export system Progress implements Startup + FixedUpdate {
    @persist("best_score")
    state best: I64 = 0;
    state seen: I64 = -1;

    action startup(cx: GameStart) { seen = best; }
    action fixed_update(frame: FixedFrame) { best += 1; }
}
"#;

#[test]
fn a_persisted_state_loads_before_the_start_and_writes_once_an_interval() {
    let store = Counting::default();
    let mut game = start(PROGRESS, store.clone());
    assert_eq!(state(&game, "seen"), Value::Int(0));
    assert_eq!(store.stores.get(), 0, "an unchanged state is not written");

    // The first boundary writes; then one write per interval of 4 ticks.
    game.step(1);
    assert_eq!(store.stores.get(), 1);
    game.step(3);
    assert_eq!(store.stores.get(), 1, "within the interval");
    game.step(1);
    assert_eq!(store.stores.get(), 2);
    let next = start(PROGRESS, store.inner.clone());
    assert_eq!(state(&next, "seen"), Value::Int(5), "the last write");
    drop(next);

    // Suspend writes what changed now; dropping the game does too.
    game.step(2);
    game.suspend();
    assert_eq!(store.stores.get(), 3);
    let next = start(PROGRESS, store.inner.clone());
    assert_eq!(state(&next, "seen"), Value::Int(7));
    drop(next);
    game.step(1);
    drop(game);
    let next = start(PROGRESS, store.inner.clone());
    assert_eq!(state(&next, "seen"), Value::Int(8));
    assert_eq!(store.inner.keys(), ["best_score"]);
}

#[test]
fn a_world_rebuild_and_a_logic_reload_keep_the_persisted_value() {
    let store = MemoryStore::default();
    let mut game = start(PROGRESS, store.clone());
    game.step(6);
    game.rebuild_world(Rebuild::Fresh).expect("rebuild");
    assert_eq!(state(&game, "seen"), Value::Int(6), "the start reads it");
    assert_eq!(state(&game, "best"), Value::Int(6));

    let widened = PROGRESS
        .replace("state best: I64 = 0;", "state best: F64 = 0.0;")
        .replace("best += 1;", "best += 0.5;")
        .replace("state seen: I64 = -1;", "state seen: F64 = -1.0;");
    game.reload(vm(&widened, store.clone(), true))
        .expect("reload");
    assert!(game.take_persist_reports().is_empty());
    assert_eq!(state(&game, "best"), Value::Float(6.0), "converted");
    game.step(1);
    game.suspend();
    let next = start(&widened, store.clone());
    assert_eq!(state(&next, "seen"), Value::Float(6.5));
}

#[test]
fn a_stored_value_of_an_older_type_converts_or_migrates() {
    let store = MemoryStore::default();
    let mut game = start(PROGRESS, store.clone());
    game.step(42);
    drop(game);

    // `I64` widens to `F64`.
    let widened = PROGRESS
        .replace("state best: I64 = 0;", "state best: F64 = 0.0;")
        .replace("best += 1;", "best += 1.0;")
        .replace("state seen: I64 = -1;", "state seen: F64 = -1.0;");
    let mut game = start(&widened, MemoryStore::default());
    assert_eq!(state(&game, "seen"), Value::Float(0.0));
    drop(game);
    game = start(&widened, store.clone());
    assert_eq!(state(&game, "seen"), Value::Float(42.0));
    drop(game);

    // `I64` does not convert into a record; the `@migrate` function does,
    // the record's new field taking its default.
    let lifted = r#"
record Score { points: I64; stars: I64 = 3; }

@migrate(from: "I64")
fn lift(points: I64) -> Score { Score { points: points * 10 } }

export system Progress implements Startup + FixedUpdate {
    @persist("best_score")
    state best: Score = Score { points: 0 };
    state seen: I64 = -1;

    action startup(cx: GameStart) { seen = best.points + best.stars; }
    action fixed_update(frame: FixedFrame) {}
}
"#;
    let mut game = start(lifted, store.clone());
    assert!(game.take_persist_reports().is_empty());
    assert_eq!(state(&game, "seen"), Value::Int(423));

    // A later `Score` with another field keeps `points` and `stars` by name.
    game.suspend();
    let grown = lifted
        .replace(
            "record Score { points: I64; stars: I64 = 3; }",
            "record Score { level: I64 = 7; points: I64; stars: I64 = 3; }",
        )
        .replace(
            "best.points + best.stars",
            "best.points + best.stars + best.level",
        );
    drop(game);
    let mut game = start(&grown, store.clone());
    assert!(game.take_persist_reports().is_empty());
    assert_eq!(state(&game, "seen"), Value::Int(430));
}

#[test]
fn a_value_that_does_not_load_takes_the_initializer_and_reports() {
    let store = MemoryStore::default();
    let mut writer = store.clone();
    writer.store("best_score", b"not a blob".to_vec());
    let mut game = start(PROGRESS, store.clone());
    assert_eq!(state(&game, "seen"), Value::Int(0));
    let reports = game.take_persist_reports();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!((&*reports[0].key, reports[0].code), ("best_score", "E9111"));
    // The unreadable blob stays until the state changes.
    drop(game);
    assert_eq!(store.get("best_score").as_deref(), Some(&b"not a blob"[..]));

    // A stored type nothing converts from.
    let mut game = start(PROGRESS, store.clone());
    game.step(1);
    drop(game);
    let text = PROGRESS
        .replace("state best: I64 = 0;", "state best: String = \"\";")
        .replace("best += 1;", "best = \"x\";")
        .replace("seen = best;", "seen = 1;");
    let mut game = start(&text, store.clone());
    let reports = game.take_persist_reports();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert!(
        reports[0].message.contains("does not convert"),
        "{reports:?}"
    );
    assert_eq!(state(&game, "best"), Value::Str(Rc::new(String::new())));
}

#[test]
fn a_game_not_granted_the_capability_neither_loads_nor_stores() {
    let store = MemoryStore::default();
    let mut game = start(PROGRESS, store.clone());
    game.step(3);
    drop(game);
    let mut game = Scheduler::new(vm(PROGRESS, store.clone(), false)).expect("started");
    assert_eq!(state(&game, "seen"), Value::Int(0));
    let reports = game.take_persist_reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].code, "E6103");
    game.step(10);
    drop(game);
    let game = start(PROGRESS, store);
    assert_eq!(state(&game, "seen"), Value::Int(3), "nothing was stored");
}

#[test]
fn persist_is_checked_at_compile_time() {
    let system = |attrs: &str, ty: &str, init: &str| {
        format!(
            "export system P implements FixedUpdate {{
    {attrs}
    state best: {ty} = {init};
    action fixed_update(frame: FixedFrame) {{}}
}}"
        )
    };
    let code = |source: &str, granted| -> Vec<String> {
        errors(source, granted)
            .into_iter()
            .map(|(c, _)| c)
            .collect()
    };
    assert!(code(&system("@persist(\"k\")", "I64", "0"), true).is_empty());
    assert_eq!(
        code(&system("@persist(\"k\")", "I64", "0"), false),
        ["E9106"]
    );
    assert_eq!(code(&system("@persist", "I64", "0"), true), ["E9106"]);
    assert_eq!(
        code(&system("@persist(k: \"k\")", "I64", "0"), true),
        ["E9106"]
    );
    let handle = errors(
        &format!(
            "import viso::time::Stopwatch;\n{}",
            system(
                "@local @persist(\"k\")",
                "Option<Stopwatch>",
                "Option::None"
            )
        ),
        true,
    );
    assert_eq!(handle.len(), 1, "{handle:?}");
    assert!(handle[0].1.contains("cannot persist"), "{handle:?}");

    let twice = format!(
        "{}\n{}",
        system("@persist(\"k\")", "I64", "0"),
        system("@persist(\"k\")", "I64", "0").replace("system P", "system Q")
    );
    let twice = errors(&twice, true);
    assert_eq!(twice.len(), 1, "{twice:?}");
    assert!(twice[0].1.contains("second state"), "{twice:?}");

    assert_eq!(
        code(
            "export system P implements FixedUpdate {
    @persist(\"k\")
    action fixed_update(frame: FixedFrame) {}
}",
            true
        ),
        ["E9106"]
    );
    assert!(
        errors(
            "export component C { @persist(\"k\") state n = 0; view { } }",
            true
        )
        .is_empty(),
        "a component's state persists"
    );
    assert_eq!(
        code(
            "export component C { @persist(\"k\") action a() {} view { } }",
            true
        ),
        ["E9106"]
    );
}

/// A store on disk across a process restart; the web has neither.
#[cfg(not(target_family = "wasm"))]
mod restart {
    use std::path::PathBuf;
    use std::process::Command;

    use viso_behavior::game::DirStore;

    use super::*;

    /// The child half of the restart test: run the game in `dir` and let the
    /// process end.
    const CHILD: &str = "VISO_PERSIST_CHILD_DIR";

    #[test]
    fn a_persisted_state_survives_a_process_restart() {
        if let Ok(dir) = std::env::var(CHILD) {
            let mut game = start(PROGRESS, DirStore::open(dir).expect("store"));
            game.step(30);
            // Dropping the game at exit makes the last value durable.
            drop(game);
            return;
        }
        let dir: PathBuf =
            std::env::temp_dir().join(format!("viso-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let status = Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "restart::a_persisted_state_survives_a_process_restart",
                "--test-threads=1",
            ])
            .env(CHILD, &dir)
            .status()
            .expect("the child runs");
        assert!(status.success());
        let lifted = PROGRESS
            .replace("state best: I64 = 0;", "state best: F64 = 0.0;")
            .replace("best += 1;", "best += 1.0;")
            .replace("state seen: I64 = -1;", "state seen: F64 = -1.0;");
        let mut game = start(&lifted, DirStore::open(&dir).expect("store"));
        assert!(game.take_persist_reports().is_empty());
        assert_eq!(state(&game, "seen"), Value::Float(30.0));
        drop(game);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
