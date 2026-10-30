//! `viso export solid`'s lowering: a view as the JSX of one SolidJS
//! component, its states as signals, its computeds as memos, its inputs as
//! typed props, its events as callbacks, and what it cannot express
//! reported where it is written. The Counter of the CLI specification is a
//! golden under `tests/golden/export/`; set `BLESS=1` to regenerate it.

mod support;

use std::path::{Path, PathBuf};
use std::rc::Rc;

use support::origin;
use viso_dsl::assets::AssetCatalog;
use viso_dsl::export::{Need, SolidExport, SolidOptions, solid};
use viso_dsl::frontend::compile_file_for;
use viso_dsl::hir::TargetProfile;
use viso_dsl::schema::Natives;

fn options() -> SolidOptions {
    SolidOptions {
        package: "demo-solid".into(),
        version: "1.0.0".into(),
        title: "Demo".into(),
        lang: "en".into(),
    }
}

fn export_in(source: &str, assets: Option<&Path>) -> Result<SolidExport, Vec<(Need, String)>> {
    let profile = TargetProfile {
        assets: assets.map(|dir| Rc::new(AssetCatalog::scan(dir).unwrap())),
        ..TargetProfile::default()
    };
    let compiled = compile_file_for(source, &origin(), Natives::standard(), profile);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    solid(&compiled, &options()).map_err(|findings| {
        findings
            .into_iter()
            .map(|f| {
                (
                    f.need,
                    source[std::ops::Range::<usize>::from(f.at)].to_owned(),
                )
            })
            .collect()
    })
}

fn export(source: &str) -> SolidExport {
    export_in(source, None).unwrap()
}

fn file<'a>(project: &'a SolidExport, path: &str) -> &'a str {
    project
        .files
        .iter()
        .find(|(p, _)| p == path)
        .map(|(_, text)| text.as_str())
        .unwrap_or_else(|| panic!("no `{path}` among {:?}", paths(project)))
}

fn paths(project: &SolidExport) -> Vec<&str> {
    project.files.iter().map(|(p, _)| p.as_str()).collect()
}

fn golden(name: &str, text: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/export")
        .join(name);
    if std::env::var("BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}; run with BLESS=1 to generate it",
            path.display()
        )
    });
    assert_eq!(text, want, "{name} changed; run with BLESS=1 to accept");
}

/// The state example of the CLI specification.
const COUNTER: &str = r#"
export component Counter {
    state count = 0;

    view {
        Column {
            Text { text: "{count}"; }
            Button {
                text: "Add";
                on click { count += 1; }
            }
        }
    }
}
"#;

#[test]
fn the_counter_is_a_component_with_a_signal_and_a_handler() {
    let project = export(COUNTER);
    assert_eq!(
        paths(&project),
        [
            "index.html",
            "package.json",
            "src/App.tsx",
            "src/components/Counter.behavior.ts",
            "src/components/Counter.tsx",
            "src/index.tsx",
            "src/styles.css",
            "src/viso/runtime.d.ts",
            "src/viso/runtime.js",
            "tsconfig.json",
            "vite.config.ts",
        ]
    );
    let component = file(&project, "src/components/Counter.tsx");
    assert!(
        component.contains("const [count, setCount] = createSignal<Value>(0n, { equals: false });"),
        "{component}"
    );
    assert!(
        component.contains("onClick={(e) => show.reaches(e) && st.run(() => f.handler"),
        "{component}"
    );
    golden("solid_counter.tsx", component);
    golden(
        "solid_counter.behavior.ts",
        file(&project, "src/components/Counter.behavior.ts"),
    );

    let package = file(&project, "package.json");
    assert!(package.contains("\"name\": \"demo-solid\""), "{package}");
    assert!(package.contains("\"solid-js\": \"^1.9.0\""), "{package}");
    assert!(file(&project, "src/viso/runtime.js").starts_with("// Generated from Viso source."));
    // The same view exports to the same bytes.
    assert_eq!(export(COUNTER), project);
}

#[test]
fn inputs_are_props_events_are_callbacks_computeds_are_memos() {
    let project = export(
        r#"
export component Badge {
    input title: String = "Hi";
    input wide: Bool = false;
    input count: I64 = 3;
    state taps = 0;
    computed doubled = taps * 2;
    event tapped(value: I64);

    view {
        Row {
            Text { width: 80dp; height: 20dp; text: title; }
            Text { width: 80dp; height: 20dp; text: "{doubled + count}"; }
            Button {
                width: 80dp;
                height: 20dp;
                text: "Tap";
                on click { taps += 1; emit tapped(taps); }
            }
        }
    }
}
"#,
    );
    let component = file(&project, "src/components/Badge.tsx");
    for part in [
        "export interface BadgeProps {",
        "  title?: string;",
        "  wide?: boolean;",
        "  count?: bigint | number;",
        "  onTapped?: (...args: Value[]) => void;",
        "[() => props.title, \"string\", \"Hi\"]",
        "[() => props.wide, \"bool\", 0n]",
        "events: [() => props.onTapped]",
    ] {
        assert!(component.contains(part), "{part} in {component}");
    }
    let behavior = file(&project, "src/components/Badge.behavior.ts");
    assert!(behavior.contains("function doubled(): any {"), "{behavior}");
    assert!(
        behavior.contains("const doubled$memo = st.memo(doubled);"),
        "{behavior}"
    );
    assert!(
        behavior.contains("st.work ? doubled() : doubled$memo()"),
        "{behavior}"
    );
    assert!(behavior.contains("st.emit(0, ["), "{behavior}");
    assert!(behavior.contains("st.input(0)"), "{behavior}");
}

#[test]
fn regions_are_show_switch_and_keyed_each() {
    let project = export(
        r#"
enum Mode { list; empty; busy; }

export component Todo {
    state items: List<String> = ["a", "b"];
    state open = true;
    state mode = Mode::list;
    view {
        Column {
            if open {
                Text { width: 100dp; height: 20dp; text: "Open"; }
            } else {
                Text { width: 100dp; height: 20dp; text: "Closed"; }
            }
            match mode {
                Mode::list => { Text { width: 100dp; height: 20dp; text: "listing"; } },
                Mode::empty => { Text { width: 100dp; height: 20dp; text: "nothing"; } },
                Mode::busy => { Text { width: 100dp; height: 20dp; text: "busy"; } },
            }
            for item in items key item {
                Text { width: 100dp; height: 20dp; text: item; }
            }
        }
    }
}
"#,
    );
    let component = file(&project, "src/components/Todo.tsx");
    for part in [
        "import { createSignal, Match, Show, Switch } from \"solid-js\";",
        "<Show",
        "fallback={",
        "{\"Closed\"}",
        "<With value={() => f.entry",
        "<Match when={f.entry",
        "(value1()) === 2n}>",
        "<Each each={() => f.entry",
        "by={(v) => f.entry",
        "{(item) => (",
        "{show.text(f.entry",
        "(item())",
    ] {
        assert!(component.contains(part), "{part} in {component}");
    }
}

#[test]
fn a_shader_has_no_solid_form() {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/10-custom-shader");
    let source = std::fs::read_to_string(example.join("src/ripples.vs")).unwrap();
    let findings = export_in(&source, Some(&example.join("assets"))).unwrap_err();
    assert!(
        findings
            .iter()
            .any(|(need, at)| *need == Need::GpuOnly && at.starts_with("ShaderView")),
        "{findings:#?}"
    );
}

/// The view of `tests/export_html.rs` that runs tasks, effects, a resource
/// and components a region mounts.
const RUNNING: &str = include_str!("sources/running.vs");

#[test]
fn the_component_runs_its_tasks_effects_and_resources() {
    let project = export(RUNNING);
    let behavior = file(&project, "src/components/Demo.behavior.ts");
    assert!(behavior.contains("async function"), "{behavior}");
    assert!(behavior.contains("st.start("), "{behavior}");
    assert!(behavior.contains("await $.sleep(tk, "), "{behavior}");
    let component = file(&project, "src/components/Demo.tsx");
    assert!(component.contains("st.fx(["), "{component}");
    assert!(
        component.contains("<>{st.mount(null, ["),
        "a region mounting a component keeps its states: {component}"
    );
    assert!(component.contains("st.at(m, () => "), "{component}");
    write(&project, "solid-running");
}

/// The view of `tests/export_html.rs` whose components read where they sit.
const ADAPTIVE: &str = include_str!("sources/adaptive.vs");

#[test]
fn the_component_follows_the_browsers_environment() {
    let project = export(ADAPTIVE);
    let component = file(&project, "src/components/Shell.tsx");
    assert!(component.contains("envField("), "{component}");
    assert!(component.contains("st.anchor(el, ["), "{component}");
    assert!(component.contains("data-scope=\"700\""), "{component}");
    write(&project, "solid-adaptive");
}

/// Writes `project` under the test's scratch directory, keeping what a
/// package manager installed there, so a reader can build and run it.
fn write(project: &SolidExport, name: &str) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(dir.join("src"));
    for (path, text) in &project.files {
        let at = dir.join(path);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, text).unwrap();
    }
}
