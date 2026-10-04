//! Headless game scenarios: a package's systems run on a tape, each tick's
//! probes traced and checked against the scenario's expectations.

use viso_behavior::native::Natives;
use viso_dsl::behavior::Program;
use viso_dsl::frontend::{Origin, compile_file_in};
use viso_dsl::scenario::{RunError, Scenario, read_tape, run};

fn program(source: &str) -> Program {
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    };
    let compiled = compile_file_in(source, &origin, Natives::standard());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    compiled.behavior
}

const JUMPER: &str = r#"
import viso::game::{FixedUpdate, FixedFrame, InputAction};

export system Player implements FixedUpdate {
    @probe state jumps = 0;
    @probe state airborne = false;
    state rolls = 0;

    action fixed_update(frame: FixedFrame) {
        if frame.input.pressed(InputAction::jump) {
            jumps += 1;
            airborne = true;
        }
        if frame.input.released(InputAction::jump) { airborne = false; }
        rolls += frame.world.random_range(0, 100);
    }
}
"#;

const SCENARIO: &str = "\
seed 1234
2: press jump
3: expect Player.airborne == true
4: release jump   # lands
4: expect Player.airborne == false
6: tap jump
9: expect Player.jumps == 2
";

#[test]
fn a_scenario_runs_its_tape_and_checks_its_expectations() {
    let scenario = Scenario::parse(SCENARIO, 0, 60).expect("a scenario");
    assert_eq!(scenario.tape.seed, 1234);
    assert_eq!(scenario.expectations.len(), 3);
    assert_eq!(
        (scenario.expectations[2].line, scenario.expectations[2].tick),
        (7, 9)
    );
    let program = program(JUMPER);
    let mut trace = Vec::new();
    let run = run(&program, &scenario, 10, |tick, probes| {
        trace.push(format!("{tick} {probes}"));
    })
    .expect("runs");
    assert!(run.failures.is_empty(), "{:?}", run.failures);
    assert!(run.faults.is_empty());
    assert_eq!((run.ticks, run.seed), (10, 1234));
    assert_eq!(trace.len(), 10);
    assert_eq!(trace[1], r#"1 {"Player.jumps":0,"Player.airborne":false}"#);
    assert_eq!(trace[2], r#"2 {"Player.jumps":1,"Player.airborne":true}"#);

    let again = viso_dsl::scenario::run(&program, &scenario, 10, |_, _| {}).expect("runs");
    assert_eq!(again.snapshot_hash, run.snapshot_hash, "deterministic");
    let reseeded = Scenario::parse(&SCENARIO.replace("seed 1234", "seed 1"), 0, 60).unwrap();
    let other = viso_dsl::scenario::run(&program, &reseeded, 10, |_, _| {}).expect("runs");
    assert_ne!(
        other.snapshot_hash, run.snapshot_hash,
        "the seed moves the dice"
    );
}

#[test]
fn an_unmet_expectation_reports_what_the_probe_held() {
    let scenario = Scenario::parse(
        "2: press jump\n5: expect Player.jumps == 3\n50: expect Player.airborne == true",
        1,
        60,
    )
    .expect("a scenario");
    let run = run(&program(JUMPER), &scenario, 10, |_, _| {}).expect("runs");
    let failures: Vec<_> = run
        .failures
        .iter()
        .map(|f| (f.expectation.line, f.actual.as_deref()))
        .collect();
    assert_eq!(failures, [(2, Some("1")), (3, None)]);
}

#[test]
fn a_scenario_must_name_probes_and_parse() {
    let scenario = Scenario::parse("3: expect Player.rolls == 0", 1, 60).expect("a scenario");
    let error = run(&program(JUMPER), &scenario, 5, |_, _| {}).expect_err("not a probe");
    assert!(matches!(error, RunError::UnknownProbe(_)));
    assert_eq!(
        error.to_string(),
        "line 1: `Player.rolls` is not a `@probe` state"
    );
    let error = Scenario::parse("x: expect Player.jumps == 1", 1, 60).expect_err("a bad tick");
    assert_eq!(error.to_string(), "line 1: `x` is not a tick");
    let error = Scenario::parse("1: expect Player.jumps", 1, 60).expect_err("no value");
    assert_eq!(
        error.to_string(),
        "line 1: expected `expect System.state == value`, found `expect Player.jumps`"
    );
}

#[test]
fn a_tape_file_is_binary_or_text() {
    let text = read_tape(b"seed 3\n1: press jump", 0, 60).expect("a text tape");
    let binary = read_tape(&text.encode(), 0, 60).expect("a binary tape");
    assert_eq!(binary, text);
    assert!(read_tape(&[0xff, 0xfe], 0, 60).is_err());
}
