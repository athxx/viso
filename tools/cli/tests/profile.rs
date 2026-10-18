//! `viso profile` end to end. What fails before anything runs — no session,
//! a flag the target does not take — runs on scratch projects. Profiling a
//! headless run builds a probe app, so that test is ignored by default:
//!
//! `cargo test -p viso-cli --test profile -- --ignored --nocapture`.

#[allow(
    dead_code,
    reason = "the dev-session half of the support is not used here"
)]
mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

use viso_dev_protocol::profile::Trace;
use viso_ende::JsonValue;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "viso-cli-profile-{label}-{}-{n}",
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

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// The payloads of the `kind` events of a `--json` stream.
fn payloads(output: &Output, kind: &str) -> Vec<JsonValue> {
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| JsonValue::parse(line).unwrap())
        .filter(|event| event.get("type").and_then(JsonValue::as_str) == Some(kind))
        .map(|event| event.get("payload").unwrap().clone())
        .collect()
}

#[test]
fn without_a_session_there_is_nothing_to_profile() {
    let s = Scratch::new("none");
    let out = viso(&s.0, &["profile"]);
    assert_eq!(code(&out), 3, "{}", stderr(&out));
    let text = stderr(&out);
    assert!(text.contains("ENV_NO_DEV_SESSION"), "{text}");
    assert!(text.contains("viso run"), "{text}");
}

#[test]
fn each_target_takes_its_own_bounds() {
    let s = Scratch::new("usage");
    for (args, says) in [
        (&["profile", "headless", "--seconds", "2"][..], "own"),
        (
            &["profile", "headless", "--device", "x"][..],
            "no `--device`",
        ),
        (&["profile", "--scenario", "a"][..], "headless"),
        (&["profile", "--frames", "0"][..], "0"),
        (&["profile", "--seconds", "0"][..], "seconds"),
    ] {
        let out = viso(&s.0, args);
        assert_eq!(code(&out), 2, "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains(says), "{args:?}: {}", stderr(&out));
    }
}

#[test]
#[ignore = "builds a probe app headless"]
fn a_headless_run_is_profiled_frame_by_frame() {
    let app = support::project(
        "profile",
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
                b"export component App {\n    state taps = 0;\n    view {\n        Column {\n            \
                  Text { text: format(\"{} taps\", taps); }\n            \
                  Button { width: 120dp; height: 40dp; text: \"Tap\"; on click { taps += 1; } }\n        }\n    }\n}\n",
            ),
            (
                "tests/ui/taps.tape",
                b"click \"Tap\"\nclick \"Tap\"\nexpect \"Tap\"\n",
            ),
        ],
    );
    let target = app.parent().unwrap().join("target");
    let traces = app.join("target/viso/traces");
    let _ = fs::remove_dir_all(&traces);
    let chrome = app.join("trace.json");
    let out = Command::new(env!("CARGO_BIN_EXE_viso"))
        .args([
            "profile",
            "headless",
            "--scenario",
            "taps",
            "--json",
            "--chrome",
        ])
        .arg(&chrome)
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    let profile = payloads(&out, "profile");
    assert_eq!(profile.len(), 1);
    let profile = &profile[0];
    assert_eq!(
        profile.get("target").and_then(JsonValue::as_str),
        Some("headless")
    );
    let frames = profile.get("frames").and_then(JsonValue::as_f64).unwrap();
    assert!(frames >= 3.0, "a frame a step at least: {frames}");
    let metrics = profile.get("metrics").unwrap();
    for name in [
        "frame",
        "input",
        "layout",
        "paint",
        "submit",
        "gpu",
        "nodes",
        "draw_calls",
    ] {
        let metric = metrics
            .get(name)
            .unwrap_or_else(|| panic!("no `{name}` metric"));
        let p50 = metric.get("p50").and_then(JsonValue::as_f64).unwrap();
        let max = metric.get("max").and_then(JsonValue::as_f64).unwrap();
        assert!(p50 <= max, "{name}");
    }
    assert!(
        metrics
            .get("input")
            .unwrap()
            .get("max")
            .and_then(JsonValue::as_f64)
            > Some(0.0),
        "the clicks are input"
    );
    let unavailable = profile
        .get("unavailable")
        .and_then(JsonValue::as_array)
        .unwrap();
    assert_eq!(
        unavailable
            .iter()
            .filter_map(JsonValue::as_str)
            .collect::<Vec<_>>(),
        ["allocations", "allocated_bytes"],
        "the probe installs no counting allocator"
    );

    let artifacts = payloads(&out, "artifact");
    let kinds: Vec<&str> = artifacts
        .iter()
        .filter_map(|a| a.get("kind")?.as_str())
        .collect();
    assert_eq!(kinds, ["trace", "chrome-trace"]);
    let path = artifacts[0]
        .get("path")
        .and_then(JsonValue::as_str)
        .unwrap();
    assert!(path.starts_with("target/viso/traces/profile-"), "{path}");
    let trace = Trace::from_bytes(&fs::read(app.join(path)).unwrap()).unwrap();
    assert_eq!(trace.app, "probe");
    assert_eq!(trace.frames.len() as f64, frames);

    let chrome = JsonValue::parse(&fs::read_to_string(&chrome).unwrap()).unwrap();
    let events = chrome
        .get("traceEvents")
        .and_then(JsonValue::as_array)
        .unwrap();
    let frame_events = events
        .iter()
        .filter(|e| e.get("name").and_then(JsonValue::as_str) == Some("frame"))
        .count();
    assert_eq!(frame_events as f64, frames);

    // `--frames` keeps the first frames of the run.
    let out = Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(["profile", "headless", "--scenario", "taps", "--frames", "2"])
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("Profiled 2 frames"), "{text}");
    assert!(text.contains("note: a headless run draws through the software raster"));

    // With a counting allocator the frames' allocations are measured.
    let main = fs::read_to_string(app.join("src/main.rs")).unwrap();
    fs::write(
        app.join("src/main.rs"),
        format!(
            "#[global_allocator]\nstatic ALLOCATOR: viso::alloc::Counting = \
             viso::alloc::Counting::system();\n{main}"
        ),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(["profile", "headless", "--scenario", "taps", "--json"])
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .unwrap();
    fs::write(app.join("src/main.rs"), main).unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let profile = &payloads(&out, "profile")[0];
    let metrics = profile.get("metrics").unwrap();
    let max = |name: &str| {
        metrics
            .get(name)
            .and_then(|m| m.get("max"))
            .and_then(JsonValue::as_f64)
            .unwrap_or_else(|| panic!("no `{name}` metric"))
    };
    assert!(max("allocations") > 0.0, "a tap formats a new label");
    assert!(max("allocated_bytes") >= max("allocations"));
    let unavailable = profile
        .get("unavailable")
        .and_then(JsonValue::as_array)
        .unwrap();
    assert!(unavailable.is_empty(), "{unavailable:?}");
}
