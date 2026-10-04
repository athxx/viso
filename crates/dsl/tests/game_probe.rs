//! `@probe` states: the compiler records each with the shape of its type, so
//! a game test writes its values to the JSON trace by type.

use std::rc::Rc;

use viso_behavior::game::Scheduler;
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Vm};
use viso_dsl::behavior::probe::ProbeShape;
use viso_dsl::frontend::{Compiled, Origin, compile_file_in};
use viso_ende::JsonWriter;

fn compiled(source: &str) -> Compiled {
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    };
    compile_file_in(source, &origin, Natives::standard())
}

fn codes(source: &str) -> Vec<String> {
    compiled(source)
        .errors()
        .map(|d| d.code.to_string())
        .collect()
}

const PROBED: &str = r#"
import viso::game::{FixedUpdate, FixedFrame};
import viso::math::Vec3F32;

enum Mode { Idle; Run; Hurt(I64); }
record Stats { hp: I64; alive: Bool; }

export system Player implements FixedUpdate {
    @probe state alive = true;
    @probe state mode = Mode::Idle;
    @probe state stats = Stats { hp: 3, alive: true };
    @probe state target: Option<I64> = Option::None;
    @probe state at = Vec3F32::new(1.0f32, 2.0f32, 0.5f32);
    @probe state speed: F32 = 0.0f32;
    @probe state name = "p1";
    state hidden = 0;

    action fixed_update(frame: FixedFrame) {
        speed += 0.5f32;
        mode = if speed > 0.75f32 { Mode::Hurt(2) } else { Mode::Run };
    }
}
"#;

#[test]
fn probes_are_recorded_with_the_shape_of_their_type() {
    let compiled = compiled(PROBED);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let system = &compiled.behavior.systems[0];
    let names: Vec<_> = system.probes.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        ["alive", "mode", "stats", "target", "at", "speed", "name"]
    );
    assert_eq!(system.probes[0].shape, ProbeShape::Bool);
    assert_eq!(system.probes[4].shape, ProbeShape::Value);

    let module = Rc::new(compiled.behavior.bytecode().expect("verified bytecode"));
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    let mut game = Scheduler::new(vm).expect("a started game");
    let trace = |game: &Scheduler| {
        let mut json = JsonWriter::new();
        json.begin_object();
        for probe in &system.probes {
            json.name(&probe.name);
            probe
                .shape
                .write(&game.instance(0).states()[probe.slot as usize], &mut json);
        }
        json.end_object();
        json.into_string()
    };
    game.step(1);
    assert_eq!(
        trace(&game),
        r#"{"alive":true,"mode":"Run","stats":{"hp":3,"alive":true},"target":null,"at":[1,2,0.5],"speed":0.5,"name":"p1"}"#
    );
    game.step(1);
    assert!(
        trace(&game).contains(r#""mode":{"Hurt":[2]}"#),
        "{}",
        trace(&game)
    );
}

#[test]
fn a_probe_marks_only_a_simulation_state_of_a_system() {
    let head = "import viso::game::{FixedUpdate, FixedFrame};\n";
    assert_eq!(
        codes(&format!(
            "{head}system S implements FixedUpdate {{ @local @probe state x = 0; action fixed_update(frame: FixedFrame) {{}} }}"
        )),
        ["E9110"]
    );
    assert_eq!(
        codes(&format!(
            "{head}system S implements FixedUpdate {{ @probe action fixed_update(frame: FixedFrame) {{}} }}"
        )),
        ["E9110"]
    );
    assert_eq!(
        codes("component C { @probe state x = 0; view { } }"),
        ["E9110"]
    );
}
