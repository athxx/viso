//! `viso inspect query` end to end. A selector that does not parse, and a
//! project with no session, fail before anything is built. Querying a
//! headless run builds a probe app, so that test is ignored by default:
//!
//! `cargo test -p viso-cli --test inspect -- --ignored --nocapture`.

#[allow(
    dead_code,
    reason = "the dev-session half of the support is not used here"
)]
mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

use viso_ende::Encoder;
use viso_view::inspect::{
    Census, CensusMount, CensusNode, FileSources, InspectKind, NodeOrigin, NodeSemantics,
    NodeSource, Rect, Role, SemanticState, SourceTable, WindowCensus,
};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "viso-cli-inspect-{label}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("Viso.toml"),
            "[package]\nname = \"demo\"\nlanguage = \"1.0\"\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/main.vs"),
            "export component App { view { Text { text: \"Hi\"; } } }\n",
        )
        .unwrap();
        Self(dir)
    }
}

impl Scratch {
    /// Writes the lock of a dev session for `name` (`host`, `ios-<device>`),
    /// naming `endpoint`.
    fn lock(&self, name: &str, endpoint: &str) {
        let dir = self.0.join("target/viso/locks");
        fs::create_dir_all(&dir).unwrap();
        let text = format!(
            "{}\n0\ndev session for `{name}`\n{endpoint}\n",
            std::process::id()
        );
        fs::write(dir.join(format!("dev-{name}.lock")), text).unwrap();
    }
}

/// A session's control endpoint answering one request with `reply`, a
/// reply body; the request body it read.
fn session(reply: Vec<u8>) -> (String, thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let endpoint = format!("{} secret", listener.local_addr().unwrap());
    let served = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut len = [0; 4];
        stream.read_exact(&mut len).unwrap();
        let mut request = vec![0; u32::from_le_bytes(len) as usize];
        stream.read_exact(&mut request).unwrap();
        stream
            .write_all(&(reply.len() as u32).to_le_bytes())
            .unwrap();
        stream.write_all(&reply).unwrap();
        request
    });
    (endpoint, served)
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn viso(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn a_selector_that_does_not_parse_exits_2_before_anything_runs() {
    let s = Scratch::new("syntax");
    let out = viso(&s.0, &["inspect", "query", "Button > [colour=red]"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("INSPECT_SELECTOR"), "{err}");
    assert!(err.contains("no attribute `colour`"), "{err}");

    let out = viso(&s.0, &["inspect", "query", "Button >", "--json"]);
    assert_eq!(code(&out), 2);
    assert!(stdout(&out).contains(r#""code":"INSPECT_SELECTOR""#));
}

#[test]
fn without_a_session_a_query_exits_3() {
    let s = Scratch::new("session");
    let out = viso(&s.0, &["inspect", "query", "#save"]);
    assert_eq!(code(&out), 3, "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("no `viso run` session"), "{err}");
    assert!(err.contains("--target headless"), "{err}");

    let out = viso(&s.0, &["inspect", "query", "#save", "--target", "ios"]);
    assert_eq!(code(&out), 3);
    assert!(stderr(&out).contains("no `viso run ios` session"));

    let out = viso(&s.0, &["inspect", "query", "#save", "--scenario", "login"]);
    assert_eq!(code(&out), 2, "a scenario plays only headless");
}

#[test]
fn a_session_answers_with_its_census_and_the_source_of_its_views() {
    let s = Scratch::new("answer");
    let node = |parent, origin, semantics: Option<NodeSemantics>| CensusNode {
        index: 4,
        generation: 2,
        parent,
        kind: InspectKind::Container,
        bounds: Rect {
            x: 10.0,
            y: 20.0,
            w: 120.0,
            h: 40.0,
        },
        shown: true,
        focusable: semantics.is_some(),
        focused: false,
        semantics,
        origin,
    };
    let census = Census {
        windows: vec![WindowCensus {
            mounts: vec![CensusMount {
                file: "/p/src/home.vs".into(),
                source_hash: 1,
            }],
            nodes: vec![
                node(None, None, None),
                node(
                    Some(0),
                    Some(NodeOrigin::Static { mount: 0, index: 0 }),
                    Some(NodeSemantics {
                        role: Role::Button,
                        label: Some("Save".into()),
                        hint: None,
                        value: None,
                        heading_level: None,
                        state: SemanticState::default(),
                    }),
                ),
            ],
        }],
    };
    let sources = SourceTable {
        files: vec![FileSources {
            file: "/p/src/home.vs".into(),
            path: "src/home.vs".into(),
            module: vec!["home".into()],
            statics: vec![NodeSource {
                type_name: "Button".into(),
                name: Some("save".into()),
                styles: vec!["Primary".into()],
                components: Vec::new(),
                owner: "Home".into(),
                bytes: (40, 90),
                start: (3, 9),
                end: (3, 59),
            }],
            regional: Vec::new(),
        }],
    };
    let mut reply = Encoder::new();
    reply.write_u8(3);
    reply.write_bytes(&census.to_bytes());
    reply.write_bytes(&sources.to_bytes());
    let (endpoint, served) = session(reply.into_bytes());
    s.lock("host", &endpoint);
    let out = viso(&s.0, &["inspect", "query", "Button.Primary", "--json"]);
    assert_eq!(code(&out), 0, "{}", stdout(&out));
    let json = stdout(&out);
    assert!(
        json.contains(
            r#""payload":{"selector":"Button.Primary","matches":[{"node_id":"4:2","window":0,"type":"Button","name":"save","styles":["Primary"],"components":[],"symbol":"home::Home::save","source_span":{"file":"src/home.vs","byte_start":40,"byte_end":90,"line":3,"column_utf16":9,"end_line":3,"end_column_utf16":59},"bounds":{"x":10,"y":20,"width":120,"height":40},"kind":"container","shown":true,"focusable":true,"focused":false,"semantics":{"role":"button","label":"Save","hint":null,"value":null,"heading_level":null,"checked":null,"selected":null,"expanded":null,"range_value":null,"range":null}}]}"#
        ),
        "{json}"
    );
    let request = served.join().unwrap();
    assert!(String::from_utf8_lossy(&request).contains("secret"));
}

#[test]
fn several_sessions_of_a_target_are_told_apart_by_device() {
    let s = Scratch::new("devices");
    s.lock("ios-phone", "127.0.0.1:1 a");
    s.lock("ios-tablet", "127.0.0.1:1 b");
    let out = viso(&s.0, &["inspect", "query", "*", "--target", "ios"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("`phone`") && err.contains("`tablet`"), "{err}");
}

fn events<'a>(stdout: &'a str, kind: &str) -> Vec<&'a str> {
    let tag = format!(r#""type":"{kind}""#);
    stdout.lines().filter(|l| l.contains(&tag)).collect()
}

#[test]
#[ignore = "builds a probe app headless"]
fn a_headless_run_is_queried_and_scenarios_select_by_the_same_selectors() {
    let app = support::project(
        "inspect",
        &[
            (
                "src/main.rs",
                b"use viso::prelude::*;\nstruct App;\nimpl Application for App {\n    \
                  fn new(_cx: &mut AppCx) -> Self { App }\n    \
                  fn build(&mut self, cx: &mut BuildCx<'_>) { viso::view!(\"app.vs\")(cx); }\n}\n\
                  fn main() { viso::run::<App>(); }\n",
            ),
            (
                "src/app.vs",
                b"style Primary for Button {\n    opacity: 0.9f32;\n}\n\n\
                  export component App {\n    state on = false;\n    view {\n        Column {\n            \
                  node title: Text { width: 60dp; height: 20dp; text: \"Hi\"; }\n            \
                  node flip: Button { width: 120dp; height: 40dp; text: \"Flip\"; styles: [Primary]; on click { on = !on; } }\n            \
                  if on {\n                node done: Text { width: 60dp; height: 20dp; text: \"Done\"; }\n            }\n        }\n    }\n}\n",
            ),
            (
                "tests/ui/press.tape",
                b"surface 320x200@1\nclick (Column > #flip)\nexpect \"Done\"\nexpect ([label=Done])\n",
            ),
        ],
    );
    let target = app.parent().unwrap().join("target");
    let viso = |args: &[&str]| -> Output {
        Command::new(env!("CARGO_BIN_EXE_viso"))
            .args(args)
            .current_dir(&app)
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .unwrap()
    };

    let out = viso(&[
        "inspect", "query", "#flip", "--target", "headless", "--json",
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let json = stdout(&out);
    let result = events(&json, "result");
    let [result] = &result[..] else {
        panic!("not one result: {json}");
    };
    assert!(result.contains(r##""selector":"#flip""##), "{result}");
    assert!(result.contains(r#""type":"Button""#), "{result}");
    assert!(result.contains(r#""name":"flip""#), "{result}");
    assert!(result.contains(r#""styles":["Primary"]"#), "{result}");
    assert!(result.contains(r#""symbol":"app::App::flip""#), "{result}");
    assert!(
        result.contains(r#""file":"src/app.vs""#) && result.contains(r#""line":10"#),
        "{result}"
    );
    assert!(result.contains(r#""role":"button""#), "{result}");
    assert!(result.contains(r#""width":120"#), "{result}");

    // Nothing selected is not an error.
    let out = viso(&["inspect", "query", "#done", "--target", "headless"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(
        stdout(&out).contains("no node matches `#done`"),
        "{}",
        stdout(&out)
    );

    // After the scenario the region's node is there, and found by name.
    let out = viso(&[
        "inspect",
        "query",
        "Column > Text",
        "--target",
        "headless",
        "--scenario",
        "press",
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("Text#title  src/app.vs:9:"), "{text}");
    assert!(text.contains("Text#done  src/app.vs:12:"), "{text}");
    assert!(text.contains("2 nodes match `Column > Text`"), "{text}");

    // A scenario finds its nodes by the same selectors.
    let out = viso(&["test", "ui"]);
    assert_eq!(code(&out), 0, "{}{}", stdout(&out), stderr(&out));
    let tape = "click (#missing)\n";
    fs::write(app.join("tests/ui/press.tape"), tape).unwrap();
    let out = viso(&["test", "ui"]);
    assert_eq!(code(&out), 1);
    let said = format!("{}{}", stdout(&out), stderr(&out));
    assert!(said.contains("no (#missing) is shown"), "{said}");
}
