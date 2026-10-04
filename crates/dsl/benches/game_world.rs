//! Game world step cost: one fixed tick of a tile level with characters and
//! sensors, through the scheduler. `idle` characters only fall onto the
//! tiles; `walk` characters also walk, one `walk` command each a tick,
//! back and forth so the scene stays on the level.
//!
//! Run release (`cargo bench -p viso-dsl --bench game_world`); criterion
//! defaults to a release profile. Debug timing is not a perf result.

use std::rc::Rc;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_behavior::game::Scheduler;
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Vm};
use viso_dsl::frontend::{Origin, compile_file_in};

/// A level of `tiles` 1 m tiles in a square, `characters` characters and
/// `sensors` sensors over it.
fn source(characters: i64, tiles: i64, sensors: i64, walk: bool) -> String {
    let side = (tiles as f64).sqrt().ceil() as i64;
    let walk = if walk {
        "let speed = if frame.tick() % 120 < 60 { 1.5 } else { -1.5 };
        for id in frame.world.query(GameTag::enemy) { frame.world.walk(id, speed, speed * 0.5); }"
    } else {
        ""
    };
    format!(
        r#"
import viso::game::{{Startup, GameStart, FixedUpdate, FixedFrame, SpawnDesc, GameTag}};
import viso::math::Vec3F32;

export system Level implements Startup + FixedUpdate {{
    action startup(cx: GameStart) {{
        let tile = Vec3F32::new(1.0f32, 1.0f32, 1.0f32);
        for i in 0..{tiles} {{
            let at = Vec3F32::new((i % {side}) as F32, 0.0f32, (i / {side}) as F32);
            cx.spawn(SpawnDesc::block(tile).at(at));
        }}
        for i in 0..{characters} {{
            let at = Vec3F32::new(((i * 37) % {side}) as F32 + 0.25f32, 1.4f32, ((i * 101) % {side}) as F32);
            cx.spawn(SpawnDesc::player().at(at).tag(GameTag::enemy));
        }}
        for i in 0..{sensors} {{
            let at = Vec3F32::new(((i * 53) % {side}) as F32, 1.5f32, ((i * 29) % {side}) as F32 + 0.5f32);
            cx.spawn(SpawnDesc::sensor(tile).at(at).tag(GameTag::coin));
        }}
    }}

    action fixed_update(frame: FixedFrame) {{
        {walk}
    }}
}}
"#
    )
}

fn game(source: &str) -> Scheduler {
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    };
    let compiled = compile_file_in(source, &origin, Natives::standard());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = compiled.behavior.bytecode().expect("verified bytecode");
    let module = Rc::new(module.with_tick_rate(60).expect("a tick rate"));
    let budget = Budget {
        instructions: 1 << 40,
        memory: 1 << 32,
        depth: 256,
        native_calls: u32::MAX,
    };
    let mut vm = Vm::new(module, budget);
    vm.link(&Natives::standard(), &[]).expect("link");
    let mut game = Scheduler::new(vm).expect("a started game");
    // Settle onto the tiles.
    game.step(30);
    assert!(game.faults().is_empty(), "{:?}", game.faults());
    game
}

fn step(c: &mut Criterion) {
    let scenes = [
        ("small", 10, 100, 10),
        ("medium", 100, 2_500, 100),
        ("large", 1_000, 10_000, 1_000),
    ];
    let mut group = c.benchmark_group("game_world");
    group.sample_size(10);
    for (name, characters, tiles, sensors) in scenes {
        for walk in [false, true] {
            let source = source(characters, tiles, sensors, walk);
            let mode = if walk { "walk" } else { "idle" };
            // The hash after a fixed run identifies the outcome, so an
            // optimization that changes what the world computes shows here.
            let mut run = game(&source);
            run.step(150);
            println!("{mode}/{name}: hash {:016x}", run.snapshot().hash());
            let mut game = game(&source);
            group.bench_function(format!("{mode}/{name}"), |b| b.iter(|| game.step(1)));
        }
    }
    group.finish();
}

criterion_group!(benches, step);
criterion_main!(benches);
