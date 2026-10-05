//! The adaptive environment in the compiler: `env` is a typed, read-only
//! `Environment` a view reads, each field it reads is a state slot the runtime
//! fills, and the view package names the node each reader's anchored fields
//! resolve at.

use viso_dsl::aot::emit_view_package;
use viso_dsl::frontend::{Origin, compile_file};
use viso_dsl::hotreload::plan_view;
use viso_dsl::ir::binding_ir::NodeKey;
use viso_dsl::view_behavior::view_behavior;
use viso_ui::adaptive::EnvField;
use viso_view::ViewEnv;

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

/// The error codes `source` compiles with.
fn codes(source: &str) -> Vec<String> {
    compile_file(source, &origin())
        .errors()
        .map(|d| d.code.to_string())
        .collect()
}

#[test]
fn env_in_a_state_initializer_is_e2111() {
    let codes =
        codes("export component A { state scale = env.text_scale; view { Row { width: 10dp; } } }");
    assert!(codes.contains(&"E2111".to_owned()), "{codes:?}");
}

#[test]
fn env_in_a_member_function_is_e2111() {
    let codes = codes(
        "export component A { fn scale() -> F32 { env.text_scale } view { Row { width: 10dp; } } }",
    );
    assert!(codes.contains(&"E2111".to_owned()), "{codes:?}");
}

#[test]
fn a_local_named_env_shadows_it() {
    let codes = codes(
        "fn twice(env: I64) -> I64 { env * 2 }\n\
         export component A { view { Row { width: 10dp; } } }",
    );
    assert!(codes.is_empty(), "{codes:?}");
}

const TYPED: &str = r#"
fn scale(e: Environment) -> F32 { e.text_scale }

export component A {
    state taps = 0;
    view {
        Column {
            width: 400dp;
            match env.size_class {
                SizeClass::Compact => { Row { width: 10dp; } },
                SizeClass::Medium => { Row { width: 20dp; } },
                SizeClass::Expanded => { Row { width: 30dp; } },
            }
            if env.reduced_motion && scale(env) > 1.0 {
                Row { width: 40dp; }
            }
            Text {
                width: 20dp;
                height: 20dp;
                on click { if env.text_scale > 1.0 { taps += 1; } }
            }
        }
    }
}
"#;

#[test]
fn env_fields_are_typed() {
    let codes = codes(TYPED);
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn a_mistyped_env_read_is_a_type_error() {
    let codes = codes(
        "export component A { view { Column { width: 10dp; if env.text_scale { Row { width: 10dp; } } } } }",
    );
    assert!(!codes.is_empty());
}

#[test]
fn env_is_not_writable() {
    let codes = codes(
        "export component A { view { Text { width: 10dp; on click { env.text_scale = 2.0; } } } }",
    );
    assert_eq!(codes, ["E2110"]);
}

#[test]
fn each_field_read_is_one_state_slot() {
    let compiled = compile_file(TYPED, &origin());
    let layout = compiled.behavior.component("A").expect("layout");
    let env: Vec<&str> = layout
        .states
        .iter()
        .filter_map(|s| s.strip_prefix("env."))
        .collect();
    let all: Vec<&str> = EnvField::ENVIRONMENT.iter().map(|f| f.name()).collect();
    let mut sorted = env.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), env.len(), "a field read twice is one slot");
    for name in &all {
        assert!(
            env.contains(name),
            "`scale(env)` reads every field: {env:?}"
        );
    }
    for read in &layout.env {
        assert_eq!(read.instance, 0);
        assert_eq!(
            layout.states[read.slot as usize],
            format!("env.{}", read.field.name())
        );
        assert_eq!(layout.state_inits[read.slot as usize], None);
    }
    let view = view_behavior(&compiled).unwrap().expect("behavior");
    assert_eq!(view.env.len(), EnvField::ENVIRONMENT.len());
    assert!(view.env.iter().all(|read| read.anchor == NodeKey(0)));
}

const PANELS: &str = r#"
component Panel {
    view {
        Column {
            width: 100dp;
            if env.size_class == SizeClass::Compact {
                Row { width: 10dp; }
            }
        }
    }
}

export component App {
    view {
        Row {
            width: 400dp;
            Panel {}
            Panel {}
        }
    }
}
"#;

#[test]
fn each_inlined_instance_reads_its_own_slot_at_its_root() {
    let compiled = compile_file(PANELS, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let layout = compiled.behavior.component("App").expect("layout");
    let reads: Vec<(u32, EnvField)> = layout.env.iter().map(|e| (e.instance, e.field)).collect();
    assert_eq!(
        reads,
        [(1, EnvField::SizeClass), (2, EnvField::SizeClass)],
        "the mounted view reads nothing itself"
    );
    assert_ne!(layout.env[0].slot, layout.env[1].slot);

    let view = view_behavior(&compiled).expect("mounts").expect("behavior");
    let anchors: Vec<NodeKey> = view.env.iter().map(|read| read.anchor).collect();
    assert_eq!(anchors, [NodeKey(1), NodeKey(3)], "each Panel's Column");

    let plan = plan_view(PANELS, &origin()).expect("plans");
    let package = emit_view_package(&plan);
    assert_eq!(
        package.env,
        [
            ViewEnv {
                slot: layout.env[0].slot,
                field: EnvField::SizeClass,
                anchor: 1,
            },
            ViewEnv {
                slot: layout.env[1].slot,
                field: EnvField::SizeClass,
                anchor: 2,
            },
        ],
        "a region's nodes take no static index"
    );
}
