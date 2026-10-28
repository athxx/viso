//! `viso check` end to end: the built binary against scratch projects, asserting
//! the exit code and what reaches stdout and stderr.

mod stand_in;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

/// A scratch project directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("viso-cli-{label}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, relative: &str, text: &str) -> &Self {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
        self
    }
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

const MANIFEST: &str = "[package]\nname = \"demo\"\nlanguage = \"1.0\"\n";

#[test]
fn a_clean_project_checks_from_a_nested_directory() {
    let s = Scratch::new("clean");
    s.write("Viso.toml", MANIFEST)
        .write(
            "src/widgets/badge.vs",
            "export record Badge { text: String; }",
        )
        .write(
            "src/main.vs",
            "import widgets::badge::{Badge};\ncomponent App { input b: Badge; view { } }",
        );
    let out = viso(&s.0.join("src/widgets"), &["check"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), "checked `demo`: 2 files, no problems\n");
    assert_eq!(stderr(&out), "");
}

#[test]
fn a_source_error_points_at_its_file_line_and_column() {
    let s = Scratch::new("error");
    s.write("Viso.toml", MANIFEST).write(
        "src/app.vs",
        "component App {\n    input b: Badgee;\n    view { }\n}\n",
    );
    let out = viso(&s.0, &["check"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(err.starts_with("error[E2001]: "), "{err}");
    assert!(
        err.contains(
            " --> src/app.vs:2:14\n  |\n2 |     input b: Badgee;\n  |              ^^^^^^\n"
        ),
        "{err}"
    );
    assert_eq!(stdout(&out), "checked `demo`: 1 file, 1 error\n");
}

#[test]
fn a_shader_compiler_diagnostic_renders_like_the_frontends() {
    // The brush profile is checked by the shader compiler: its diagnostic
    // carries the primary span, the view that paints with the shader and the
    // explanation pointer as any other.
    let s = Scratch::new("brush");
    s.write("Viso.toml", MANIFEST).write(
        "src/app.vs",
        "shader Glow {
    uniform viewport_size: Vec2F32;
    instance rect_pos: Vec2F32;
    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        return VertexOutput { clip_position: to_clip(rect_pos + unit, viewport_size) };
    }
    fragment() -> Vec4F32 {
        return Vec4F32(1.0, 0.0, 0.0, 1.0);
    }
}
export component Card {
    view {
        ShaderView { shader: Glow {}; width: 120dp; height: 80dp; }
    }
}
",
    );
    let out = viso(&s.0, &["check"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(err.starts_with("error[E8107]: "), "{err}");
    assert!(err.contains(" --> src/app.vs:1:8\n"), "{err}");
    assert!(err.contains("`Glow` paints this view"), "{err}");
    assert!(err.contains("viso explain E8107"), "{err}");
    assert_eq!(stdout(&out), "checked `demo`: 1 file, 1 error\n");
}

#[test]
fn a_view_that_does_not_mount_is_an_error_its_file_points_at() {
    let s = Scratch::new("unmounted");
    s.write("Viso.toml", MANIFEST)
        .write(
            "src/app.vs",
            "export component App {\n    state at: String = \"a\";\n    view {\n        Grid { Text { grid.area: at; } }\n    }\n}\n",
        )
        .write(
            "src/parts.vs",
            "record Size { w: I32; }\nconst GAP: MixedLength = 8dp;\n",
        );
    let out = viso(&s.0, &["check"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.starts_with("error[E3711]: "), "{err}");
    assert!(
        err.contains("a `grid.area` that reads a value does not lay out yet"),
        "{err}"
    );
    assert!(err.contains(" --> src/app.vs:4:34\n"), "{err}");
    assert_eq!(stdout(&out), "checked `demo`: 2 files, 1 error\n");
}

#[test]
fn an_unsupported_language_version_points_into_the_manifest() {
    let s = Scratch::new("language");
    s.write(
        "Viso.toml",
        "[package]\nname = \"demo\"\nlanguage = \"9.9\"\n",
    )
    .write("src/app.vs", "component App { view { } }");
    let out = viso(&s.0, &["check"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(err.starts_with("error[E1001]: "), "{err}");
    assert!(
        err.contains(" --> Viso.toml:3:12\n  |\n3 | language = \"9.9\"\n  |            ^^^^^\n"),
        "{err}"
    );
    assert!(err.contains("= note: set `language = \"1.0\"`"), "{err}");
}

#[test]
fn project_may_come_before_or_after_the_subcommand() {
    let s = Scratch::new("explicit");
    s.write("app/Viso.toml", MANIFEST)
        .write("app/src/app.vs", "component App { view { } }");
    for args in [["--project", "app", "check"], ["check", "--project", "app"]] {
        let out = viso(&s.0, &args);
        assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
        assert_eq!(stdout(&out), "checked `demo`: 1 file, no problems\n");
    }
}

#[test]
fn no_manifest_is_a_config_diagnostic() {
    let s = Scratch::new("missing");
    let out = viso(&s.0, &["check", "--project", "."]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).starts_with("error[C0001]: "),
        "{}",
        stderr(&out)
    );
    assert_eq!(stdout(&out), "");
}

#[test]
fn quiet_drops_warnings_but_keeps_the_summary() {
    let s = Scratch::new("quiet");
    s.write(
        "Viso.toml",
        "[package]\nname = \"demo\"\n\n[profile.dev]\nhot_reload = true\n",
    )
    .write("src/app.vs", "component App { view { } }");
    let loud = viso(&s.0, &["check"]);
    assert_eq!(code(&loud), 0);
    assert!(
        stderr(&loud).starts_with("warning[C0004]: "),
        "{}",
        stderr(&loud)
    );
    assert!(
        stderr(&loud).contains("5 | hot_reload = true\n"),
        "{}",
        stderr(&loud)
    );
    assert_eq!(stdout(&loud), "checked `demo`: 1 file, 1 warning\n");

    let quiet = viso(&s.0, &["-q", "check"]);
    assert_eq!(code(&quiet), 0);
    assert_eq!(stderr(&quiet), "");
    assert_eq!(stdout(&quiet), "checked `demo`: 1 file, 1 warning\n");
}

#[test]
fn usage_errors_exit_2_and_help_and_version_exit_0() {
    let cwd = std::env::temp_dir();
    assert_eq!(code(&viso(&cwd, &["check", "--no-such-flag"])), 2);
    assert_eq!(code(&viso(&cwd, &["no-such-command"])), 2);
    assert_eq!(code(&viso(&cwd, &[])), 2);

    let help = viso(&cwd, &["--help"]);
    assert_eq!(code(&help), 0);
    assert!(stdout(&help).contains("check"), "{}", stdout(&help));
    let version = viso(&cwd, &["--version"]);
    assert_eq!(code(&version), 0);
    assert!(
        stdout(&version).starts_with(&format!("viso {} (", env!("CARGO_PKG_VERSION"))),
        "{}",
        stdout(&version)
    );
}

/// The event lines of a `--json` run, each checked to open with the envelope and
/// its sequence number; returns the lines.
fn events(output: &Output) -> Vec<String> {
    let lines: Vec<String> = stdout(output).lines().map(str::to_string).collect();
    for (seq, line) in lines.iter().enumerate() {
        let envelope = format!(r#"{{"schema":"viso.cli.event","schema_version":1,"seq":{seq},"#);
        assert!(line.starts_with(&envelope), "{line}");
        assert!(line.ends_with("}}"), "{line}");
    }
    lines
}

#[test]
fn json_streams_one_event_per_line_and_ends_with_the_summary() {
    let s = Scratch::new("json");
    s.write("Viso.toml", MANIFEST).write(
        "src/app.vs",
        "component App {\n    input b: Badgee;\n    view { }\n}\n",
    );
    let out = viso(&s.0, &["check", "--json"]);
    assert_eq!(code(&out), 1);
    assert_eq!(stderr(&out), "");
    let lines = events(&out);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].contains(r#""type":"diagnostic","#), "{}", lines[0]);
    assert!(lines[0].contains(r#""command":"check","#), "{}", lines[0]);
    assert!(
        lines[0].contains(concat!(
            r#""severity":"error","code":"E2001","#,
            r#""message":"type does not exist: `Badgee`","#,
            r#""primary":{"file":"src/app.vs","byte_start":29,"byte_end":35,"line":2,"#,
            r#""column_utf16":14,"end_line":2,"end_column_utf16":20}"#,
        )),
        "{}",
        lines[0]
    );
    assert!(lines[1].contains(r#""type":"summary","#), "{}", lines[1]);
    assert!(
        lines[1].contains(r#""status":"failure","exit_code":1,"#),
        "{}",
        lines[1]
    );
    assert!(
        lines[1].contains(
            r#""warning_count":0,"error_count":1,"artifact_count":0,"package":"demo","file_count":1}}"#
        ),
        "{}",
        lines[1]
    );
}

#[test]
fn json_names_what_a_mismatch_expected_and_found() {
    let s = Scratch::new("json-mismatch");
    s.write("Viso.toml", MANIFEST)
        .write("src/app.vs", "fn f() -> I64 {\n    \"a\"\n}\n");
    let out = viso(&s.0, &["check", "--json"]);
    assert_eq!(code(&out), 1);
    let lines = events(&out);
    assert!(
        lines[0].contains(r#""code":"E2103","#)
            && lines[0].contains(r#""expected":["I64"],"actual":"String","#),
        "{}",
        lines[0]
    );
}

#[test]
fn an_import_of_a_private_record_points_into_its_file_and_fixes_it_there() {
    let s = Scratch::new("private-import");
    s.write("Viso.toml", MANIFEST)
        .write("src/shapes.vs", "record Point { x: I64; }\n")
        .write(
            "src/main.vs",
            "import shapes::{ Point };\ncomponent App { input p: Point; view { } }\n",
        );
    let out = viso(&s.0, &["check"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains(
            " ::: src/shapes.vs:1:8\n  |\n1 | record Point { x: I64; }\n  |        ----- declared here without `export`\n"
        ),
        "{}",
        stderr(&out)
    );
    let out = viso(&s.0, &["check", "--json"]);
    let lines = events(&out);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].contains(r#""related":[{"file":"src/shapes.vs","byte_start":7,"byte_end":12,"#)
            && lines[0].contains(
                r#""edits":[{"file":"src/shapes.vs","byte_start":0,"byte_end":0,"replacement":"export "}]"#
            ),
        "{}",
        lines[0]
    );
}

#[test]
fn json_quiet_still_streams_warnings() {
    let s = Scratch::new("json-quiet");
    s.write(
        "Viso.toml",
        "[package]\nname = \"demo\"\n\n[profile.dev]\nhot_reload = true\n",
    )
    .write("src/app.vs", "component App { view { } }");
    let out = viso(&s.0, &["--json", "-q", "check"]);
    assert_eq!(code(&out), 0);
    let lines = events(&out);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].contains(r#""code":"C0004","#)
            && lines[0].contains(r#""primary":{"file":"Viso.toml","#),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains(r#""status":"success","exit_code":0,"#),
        "{}",
        lines[1]
    );
}

#[test]
fn json_reports_a_usage_error_as_events() {
    let cwd = std::env::temp_dir();
    let out = viso(&cwd, &["--json", "check", "--no-such-flag"]);
    assert_eq!(code(&out), 2);
    assert_eq!(stderr(&out), "");
    let lines = events(&out);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].contains(r#""code":"CLI_USAGE","#) && lines[0].contains(r#""primary":null"#),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains(r#""status":"failure","exit_code":2,"#),
        "{}",
        lines[1]
    );

    // Without `--json` the same diagnostic goes to stderr as text.
    let plain = viso(&cwd, &["check", "--no-such-flag"]);
    assert_eq!(stdout(&plain), "");
    assert!(
        stderr(&plain).starts_with("error[CLI_USAGE]: unexpected argument '--no-such-flag'"),
        "{}",
        stderr(&plain)
    );
}

#[test]
fn json_without_a_project_still_ends_with_a_summary() {
    let s = Scratch::new("json-missing");
    let out = viso(&s.0, &["check", "--json", "--project", "."]);
    assert_eq!(code(&out), 1);
    let lines = events(&out);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].contains(r#""code":"C0001","#), "{}", lines[0]);
    assert!(
        lines[1].contains(r#""exit_code":1,"#) && !lines[1].contains("package"),
        "{}",
        lines[1]
    );
}

#[test]
fn a_manifest_syntax_error_shows_its_line() {
    let s = Scratch::new("syntax");
    s.write("Viso.toml", "[package\n");
    let out = viso(&s.0, &["check"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(
        err.contains(" --> Viso.toml:1:9\n  |\n1 | [package\n"),
        "{err}"
    );
}

#[test]
fn tr_checks_against_the_catalogs_and_a_catalog_error_points_into_its_file() {
    let s = Scratch::new("i18n");
    s.write("Viso.toml", &format!("{MANIFEST}[i18n]\nsource = \"en\"\n"))
        .write("i18n/en.toml", "[inbox]\ntitle = \"Inbox\"\n")
        .write(
            "src/app.vs",
            "import viso::i18n::tr;\ncomponent App { view { Text { text: tr(\"inbox.title\"); } } }",
        );
    let out = viso(&s.0, &["check"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    s.write(
        "src/app.vs",
        "import viso::i18n::tr;\ncomponent App { view { Text { text: tr(\"inbox.titel\"); } } }",
    );
    let out = viso(&s.0, &["check"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("E3706"), "{}", stderr(&out));
    assert!(stderr(&out).contains("inbox.title"), "{}", stderr(&out));

    s.write(
        "i18n/de.toml",
        "[inbox]\ntitle = \"{n, plural, one {x}}\"\n",
    );
    let out = viso(&s.0, &["check"]);
    let err = stderr(&out);
    assert!(err.contains("i18n/de.toml:2:9"), "{err}");
    assert!(err.contains("`other`"), "{err}");
}

/// A game whose one action is bound to a key alone.
const KEYS_ONLY: &str = "import viso::game::{InputAction, InputMap, Key};\n\
     @derive(InputAction) enum Act { Jump; }\n\
     system Host {}\n\
     const C: InputMap<Act> = InputMap::new().key(Key::Space, Act::Jump);\n";

/// The codes of the diagnostics of a `--json` run.
fn codes(output: &Output) -> Vec<String> {
    events(output)
        .iter()
        .filter(|line| line.contains("\"type\":\"diagnostic\""))
        .filter_map(|line| {
            let at = line.find("\"code\":\"")? + 8;
            Some(line[at..].split('"').next()?.to_owned())
        })
        .collect()
}

#[test]
fn a_target_is_checked_for_the_input_its_devices_have() {
    let s = Scratch::new("target");
    s.write("Viso.toml", MANIFEST)
        .write("src/game.vs", KEYS_ONLY);
    let host = viso(&s.0, &["check", "--json"]);
    assert_eq!(code(&host), 0, "{}", stdout(&host));
    assert_eq!(codes(&host), ["E9107"], "a desktop has gamepads");
    let headless = viso(&s.0, &["check", "headless", "--json"]);
    assert_eq!(code(&headless), 0);
    assert!(codes(&headless).is_empty(), "{}", stdout(&headless));
    let phone = viso(&s.0, &["check", "ios", "--json"]);
    assert_eq!(codes(&phone), ["E9107", "E9107"], "gamepads and touch");

    // A page mounts the project's `.vs` view, which a game has not.
    let dom = viso(&s.0, &["check", "web-dom"]);
    assert_ne!(code(&dom), 0);
    assert!(
        stderr(&dom).contains("error[WEB_DOM_NO_VIEW]"),
        "{}",
        stderr(&dom)
    );
    let hybrid = viso(&s.0, &["check", "web-hybrid"]);
    assert_eq!(code(&hybrid), 3);
    assert!(
        stderr(&hybrid).contains("error[C0011]"),
        "{}",
        stderr(&hybrid)
    );
    let flag = viso(&s.0, &["check", "--target", "ios"]);
    assert_eq!(code(&flag), 2);
    assert!(
        stderr(&flag).contains("`viso check` names its target as an argument"),
        "{}",
        stderr(&flag)
    );
}

#[test]
fn strict_makes_accessibility_and_localization_findings_errors() {
    let s = Scratch::new("strict");
    s.write("Viso.toml", MANIFEST).write(
        "src/app.vs",
        "component App {\n    state n = 0;\n    view { Row { on click { n += 1; } } }\n}\n",
    );
    let lenient = viso(&s.0, &["check"]);
    assert_eq!(code(&lenient), 0, "{}", stderr(&lenient));
    assert!(
        stderr(&lenient).contains("warning[E3704]"),
        "{}",
        stderr(&lenient)
    );
    let strict = viso(&s.0, &["check", "--a11y", "strict"]);
    assert_eq!(code(&strict), 1);
    assert!(
        stderr(&strict).contains("error[E3704]"),
        "{}",
        stderr(&strict)
    );

    s.write(
        "src/app.vs",
        "component App {\n    view { Text { text: \"Hello\"; } }\n}\n",
    );
    assert_eq!(code(&viso(&s.0, &["check"])), 0);
    let strict = viso(&s.0, &["check", "--i18n", "strict"]);
    assert_eq!(code(&strict), 1);
    assert!(
        stderr(&strict).contains("error[E3705]"),
        "{}",
        stderr(&strict)
    );
}

#[test]
fn a_native_packages_rust_widget_checks_from_its_schema() {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/11-native-widget");
    let out = viso(&example, &["check"]);
    assert_eq!(code(&out), 0, "{}{}", stdout(&out), stderr(&out));
    assert!(stdout(&out).contains("no problems"), "{}", stdout(&out));
}

#[test]
fn cargo_checks_the_rust_code_too() {
    let root = stand_in::project("check-cargo");
    fs::write(root.join("Viso.toml"), MANIFEST).unwrap();
    let out = stand_in::viso(&root, &["check", "--cargo", "--json"]);
    assert_eq!(code(&out), 0, "{}", stdout(&out));
    fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    let x: u8 = \"no\";\n}\n",
    )
    .unwrap();
    let out = stand_in::viso(&root, &["check", "--cargo", "--json"]);
    assert_eq!(code(&out), 4, "{}", stdout(&out));
    let codes = codes(&out);
    assert!(codes.contains(&"E0308".to_owned()), "{codes:?}");
    assert!(codes.contains(&"BUILD_FAILED".to_owned()), "{codes:?}");
    assert!(
        !root.join("target/debug/probe").exists(),
        "nothing is built"
    );
    // Without `--cargo` the Rust code is not looked at.
    assert_eq!(code(&stand_in::viso(&root, &["check"])), 0);
}

#[cfg(unix)]
#[test]
fn watch_checks_again_after_each_change_until_interrupted() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;

    let s = Scratch::new("watch");
    s.write("Viso.toml", MANIFEST)
        .write("src/app.vs", "component App { view { } }");
    let mut child = Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(["check", "--watch", "--json"])
        .current_dir(&s.0)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    // The lines of a round, through its `idle`.
    let round = || {
        let mut lines = Vec::new();
        loop {
            let line = rx
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_else(|_| panic!("no idle after {lines:#?}"));
            let idle = line.contains("\"phase\":\"idle\"");
            lines.push(line);
            if idle {
                return lines;
            }
        }
    };
    let first = round();
    assert_eq!(first.len(), 1, "a clean round: {first:#?}");

    s.write("src/app.vs", "component App { view { Text { txet: 1; } } }");
    let second = round();
    assert!(
        second.iter().any(|l| l.contains("\"severity\":\"error\"")),
        "{second:#?}"
    );
    s.write("src/app.vs", "component App { view { } }");
    let third = round();
    assert!(
        !third.iter().any(|l| l.contains("\"type\":\"diagnostic\"")),
        "{third:#?}"
    );

    let pid = child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-INT", &pid])
            .status()
            .unwrap()
            .success()
    );
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130));
    let rest: Vec<String> = rx.try_iter().collect();
    let summary = rest.last().expect("a summary");
    assert!(summary.contains("\"type\":\"summary\""), "{rest:#?}");
    assert!(summary.contains("\"status\":\"cancelled\""), "{summary}");
    assert!(
        summary.contains("\"error_count\":0"),
        "the last round's: {summary}"
    );
}
