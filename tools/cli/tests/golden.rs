//! The CLI's surface held to golden files: every command's help page, the
//! usage errors of each kind, and `viso config show` of a fixed project.
//! `viso --version` is checked by shape, its commit being the build's.
//!
//! Set `BLESS=1` to (re)write the files under `tests/golden/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn viso(cwd: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_viso"));
    command.args(args).current_dir(cwd);
    for name in [
        "VISO_PROFILE",
        "VISO_OPT_LEVEL",
        "VISO_SOURCE_MAPS",
        "VISO_STRIP",
        "VISO_COMPLETE",
    ] {
        command.env_remove(name);
    }
    command.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Compares `actual` with the golden file `name`, or writes it under `BLESS`.
fn golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("BLESS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}; run with BLESS=1 to write it",
            path.display()
        )
    });
    if expected != actual {
        let line = expected
            .lines()
            .zip(actual.lines())
            .position(|(e, a)| e != a)
            .unwrap_or(expected.lines().count().min(actual.lines().count()));
        panic!(
            "{} differs from line {}; run with BLESS=1 if the change is meant:\n{actual}",
            path.display(),
            line + 1
        );
    }
}

fn scratch(label: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("golden-{label}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The commands a help page lists: the top level's grouped list, or a
/// command's `Commands:` section.
fn listed(page: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_list = false;
    for line in page.lines() {
        if line.is_empty() {
            in_list = false;
        } else if !line.starts_with(' ') {
            in_list = line == "Commands:"
                || (line
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || " /&".contains(c))
                    && !line.starts_with("Usage"));
        } else if in_list && let Some(name) = line.split_whitespace().next() {
            names.push(name.to_owned());
        }
    }
    names
}

/// Every help page, the top level first and then each command in the order
/// its parent lists it, reached through the pages themselves.
#[test]
fn every_help_page() {
    let cwd = scratch("help");
    let mut pages = String::new();
    let mut queue: Vec<Vec<String>> = vec![Vec::new()];
    let mut seen = 0;
    while let Some(path) = queue.pop() {
        let mut args: Vec<&str> = path.iter().map(String::as_str).collect();
        args.push("--help");
        let out = viso(&cwd, &args);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert!(out.stderr.is_empty(), "{args:?}");
        let page = text(&out.stdout);
        pages.push_str(&format!("$ viso {}\n{page}\n", args.join(" ")));
        seen += 1;
        // Depth first, in the listed order.
        for name in listed(&page).into_iter().rev() {
            let mut sub = path.clone();
            sub.push(name);
            queue.push(sub);
        }
    }
    assert!(seen > 20, "only {seen} pages reached");
    golden("help.txt", &pages);
}

/// One usage error of each kind: each is `CLI_USAGE` and exit 2, with the
/// command's `--help` to run next.
#[test]
fn usage_errors() {
    let cwd = scratch("usage");
    let mut report = String::new();
    for args in [
        &["chekc"][..],
        &[],
        &["config"],
        &["check", "--bogus"],
        &["run", "macos"],
        &["run", "--target", "ios"],
        &["config", "get"],
        &["config", "set", "a", "b"],
        &["-q", "-v", "check"],
        &["check", "--jobs", "0"],
        &["schema", "Button", "--search", "x"],
        &["completion", "tcsh"],
    ] {
        let out = viso(&cwd, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let stderr = text(&out.stderr);
        // With no command, the page of the command is the answer.
        if !matches!(args, [] | ["config"]) {
            assert!(
                stderr.starts_with("error[CLI_USAGE]: "),
                "{args:?}: {stderr}"
            );
        }
        report.push_str(&format!("$ viso {}\n{stderr}\n", args.join(" ")));
    }
    golden("usage.txt", &report);

    // In the event stream: one diagnostic with the next command as a fix,
    // then the summary.
    let out = viso(&cwd, &["check", "--bogus", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let stdout = text(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert!(lines[0].contains(r#""code":"CLI_USAGE""#), "{stdout}");
    assert!(
        lines[0].contains(
            r#""fixes":[{"title":"run `viso check --help`","applicability":"maybe-incorrect","edits":[]}]"#
        ),
        "{stdout}"
    );
    assert!(lines[1].contains(r#""exit_code":2"#), "{stdout}");
}

#[test]
fn version_names_the_build_and_what_it_speaks() {
    let cwd = scratch("version");
    let out = viso(&cwd, &["--version"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = text(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    let head = lines[0]
        .strip_prefix(&format!("viso {} (", env!("CARGO_PKG_VERSION")))
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert!(
        head == "unknown" || (head.len() == 12 && head.bytes().all(|b| b.is_ascii_hexdigit())),
        "{head}"
    );
    let named: Vec<&str> = lines[1..]
        .iter()
        .map(|line| line.rsplit_once(' ').unwrap().0)
        .collect();
    assert_eq!(
        named,
        [
            "language",
            "event schema",
            "diagnostic schema",
            "schema object",
            "dump",
            "snapshot",
            "dev protocol"
        ]
    );
    assert_eq!(text(&viso(&cwd, &["-V"]).stdout), stdout);

    let out = viso(&cwd, &["--version", "--json"]);
    let stdout = text(&out.stdout);
    for name in [
        "version",
        "commit",
        "language",
        "event_schema",
        "diagnostic_schema",
        "schema_object",
        "dump_version",
        "snapshot_version",
        "dev_protocol",
    ] {
        assert!(
            stdout.contains(&format!("\"{name}\":")),
            "{name} in {stdout}"
        );
    }
}

/// The payload of the `result` event in `stdout`.
fn payload(stdout: &str) -> String {
    let line = stdout
        .lines()
        .find(|l| l.contains(r#""type":"result""#))
        .unwrap_or_else(|| panic!("no result: {stdout}"));
    let start = line.find(r#""payload":"#).unwrap() + r#""payload":"#.len();
    format!("{}\n", &line[start..line.len() - 1])
}

const CARD: &str = r#"export component Card {
    @bindable(changed)
    input value: String;
    input title: String;
    input dense: Bool = false;
    event changed(value: String);
    slot footer: OptionalSlot<Node>;
    view {
        Column {
            Text { text: title; }
            if dense { Text { text: value; } }
        }
    }
}

export record Point {
    x: I64;
    y: I64 = 0;
}

enum Status {
    idle;
    ready(I64);
    failed { code: I64; };
}
"#;

/// The schema objects of a widget, a member, a native function and type and
/// a project component, record and enum, and each `--list`, as `--json`
/// gives them.
#[test]
fn schema_objects() {
    let dir = scratch("schema");
    fs::write(
        dir.join("Viso.toml"),
        "[package]\nname = \"demo\"\nversion = \"1.2.0\"\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/card.vs"), CARD).unwrap();
    let mut objects = String::new();
    for args in [
        &["Button"][..],
        &["Slider.value"],
        &["viso::clipboard::write_text"],
        &["Stopwatch"],
        &["Card"],
        &["Point"],
        &["Status"],
        &["--search", "card"],
        &["--list", "components"],
        &["--list", "types"],
    ] {
        let mut full = vec!["schema"];
        full.extend(args);
        full.push("--json");
        let out = viso(&dir, &full);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&out.stdout)
        );
        objects.push_str(&format!("$ viso schema {}\n", args.join(" ")));
        objects.push_str(&payload(&text(&out.stdout)));
    }
    golden("schema.json.txt", &objects);

    let mut human = String::new();
    for symbol in ["Card", "Point", "Status"] {
        let out = viso(&dir, &["schema", symbol]);
        assert_eq!(out.status.code(), Some(0));
        human.push_str(&format!("$ viso schema {symbol}\n{}", text(&out.stdout)));
    }
    golden("schema_card.txt", &human);
}

const DUMP_COUNTER: &str = r#"import viso::math::Vec2F32;

export component Counter {
    input label: String = "Count";
    state count = 0;
    state names: [String] = ["a", "b"];
    computed doubled: I64 = count * 2;
    event changed(value: I64);

    action increment() {
        count += 1;
        emit changed(count);
    }

    view {
        Column {
            gap: 8dp;
            node title: Text { text: label; }
            Button { text: "Add"; on click { increment(); } }
            for name in names key name { Text { text: name; } }
            if count > 3 { Text { text: "many"; } } else { Text { text: "few"; } }
        }
    }
}

shader Glow {
    uniform viewport_size: Vec2F32;
    instance rect_pos: Vec2F32;
    instance rect_size: Vec2F32;
    instance tint: ColorLinear;
    varying local: Vec2F32;

    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        local = unit;
        return VertexOutput {
            clip_position: to_clip(rect_pos + unit * rect_size, viewport_size),
        };
    }

    fragment() -> ColorLinear {
        let k = 1.0 - length(local - Vec2F32(0.5, 0.5));
        return ColorLinear(tint.r * k, tint.g * k, tint.b * k, 1.0);
    }
}
"#;

const DUMP_GAME: &str = r#"import viso::game::quick::{QuickGame, QuickStart, QuickFrame};
import viso::game::{InputAction, InputAxis};

export system Runner implements QuickGame {
    state lives = 0;
    state x = 0.0;

    action start(cx: QuickStart) {
        lives = 2;
    }

    action fixed(frame: QuickFrame) {
        x = x + frame.input.axis(InputAxis::move_x) * frame.dt();
        if frame.input.pressed(InputAction::jump) && lives > 0 {
            lives -= 1;
        }
    }
}
"#;

const DUMP_MODEL: &str = r#"export record Step {
    by: I64;
}

export fn twice(n: I64) -> I64 {
    n * 2
}
"#;

/// Every `viso dump` kind over a fixed project, and two of them as JSON.
#[test]
fn dumps() {
    let dir = scratch("dump");
    fs::write(
        dir.join("Viso.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/counter.vs"), DUMP_COUNTER).unwrap();
    fs::write(dir.join("src/game.vs"), DUMP_GAME).unwrap();
    fs::write(dir.join("src/model.vs"), DUMP_MODEL).unwrap();
    let mut report = String::new();
    for args in [
        &["module-graph"][..],
        &["ast", "src/model.vs"],
        &["hir"],
        &["ui-ir", "src/counter.vs"],
        &["reactive-ir", "Counter"],
        &["behavior-ir", "Counter.increment"],
        &["shader-ir", "Glow"],
        &["system-ir", "src/game.vs"],
    ] {
        let mut full = vec!["dump"];
        full.extend(args);
        let out = viso(&dir, &full);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&out.stderr)
        );
        report.push_str(&format!(
            "$ viso dump {}\n{}\n",
            args.join(" "),
            text(&out.stdout)
        ));
    }
    for args in [&["hir", "Counter"][..], &["reactive-ir", "Counter"]] {
        let mut full = vec!["dump"];
        full.extend(args);
        full.push("--json");
        let out = viso(&dir, &full);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        report.push_str(&format!("$ viso dump {} --json\n", args.join(" ")));
        report.push_str(&payload(&text(&out.stdout)));
    }
    golden("dump.txt", &report);
}

/// `viso explain` of a code of each producer, and the list.
#[test]
fn explanations() {
    let cwd = scratch("explain");
    let mut report = String::new();
    for args in [
        &["E3101"][..],
        &["E2302"],
        &["E5101"],
        &["C0004"],
        &["ENV_LOCK_HELD"],
        &["--list"],
    ] {
        let mut full = vec!["explain"];
        full.extend(args);
        let out = viso(&cwd, &full);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        report.push_str(&format!(
            "$ viso explain {}\n{}\n",
            args.join(" "),
            text(&out.stdout)
        ));
    }
    golden("explain.txt", &report);
}

#[test]
fn config_show() {
    let dir = scratch("config");
    fs::write(
        dir.join("Viso.toml"),
        "[package]\nname = \"demo\"\nversion = \"1.2.0\"\n\n[profile.release]\nsource_maps = true\n\n\
         [web.serve]\nport = 9000\n",
    )
    .unwrap();
    let out = viso(&dir, &["config", "show"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    golden("config_show.txt", &text(&out.stdout));
}
