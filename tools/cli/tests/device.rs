//! `viso run ios|android|web-gpu` end to end: the probe launches on a
//! simulator, an emulator or a browser page, connects, takes a `.vs` patch,
//! and restarts across a Rust edit with its state back.
//!
//! Each test builds a probe app for its target and needs that target's
//! toolchain, so they are ignored by default:
//!
//! ```text
//! cargo test -p viso-cli --test device -- --ignored --nocapture ios
//! cargo test -p viso-cli --test device -- --ignored --nocapture android
//! cargo test -p viso-cli --test device -- --ignored --nocapture web
//! ```
//!
//! The device tests create a profile of their own under a scratch
//! `VISO_HOME`, and delete it, with its simulator or emulator, at the end.
//! The web test opens the page in a headless Chrome: `$VISO_E2E_CHROME`, or
//! the default install.
#![cfg(unix)]

mod support;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use support::{Run, dev_event};

const VIEW: &str = r#"component Probe {
    state count = 0;
    view {
        Column {
            width: 200dp;
            height: 40dp;
            Text("{count}") { width: 200dp; height: 40dp }
        }
    }
}
"#;

/// The app: it says which process it is at launch, and keeps that across a
/// restart as its own state.
fn main_rs(edit: &str) -> String {
    format!(
        r#"use viso::prelude::*;

struct Probe;

impl Application for Probe {{
    fn new(cx: &mut AppCx) -> Self {{
        // A page has no process id.
        #[cfg(not(target_arch = "wasm32"))]
        let pid = std::process::id();
        #[cfg(target_arch = "wasm32")]
        let pid = 0;
        eprintln!("probe launched {{pid}}");
        cx.dev_state(
            "probe",
            move || pid.to_string().into_bytes(),
            |bytes| eprintln!("probe restored {{}}", String::from_utf8_lossy(bytes)),
        );
        Probe
    }}

    fn build(&mut self, cx: &mut BuildCx<'_>) {{
        viso::view!("probe.vs")(cx);
    }}
}}

fn main() {{
    viso::run::<Probe>();
}}
{edit}
"#
    )
}

/// The probe project `name`.
fn project(name: &str) -> PathBuf {
    support::project(
        name,
        &[
            ("src/probe.vs", VIEW.as_bytes()),
            ("src/main.rs", main_rs("").as_bytes()),
        ],
    )
}

/// `viso <args>` with `VISO_HOME` at `home`, which must succeed.
fn viso(app: &Path, home: &Path, args: &[&str]) {
    let status = Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .current_dir(app)
        .env("VISO_HOME", home)
        .stdin(Stdio::null())
        .status()
        .expect("viso runs");
    assert!(status.success(), "`viso {}`: {status}", args.join(" "));
}

/// A profile of `platform` made for the test, deleted with its device when
/// the test ends.
struct Profile {
    app: PathBuf,
    home: PathBuf,
    platform: &'static str,
    kind: &'static str,
}

impl Profile {
    const ID: &'static str = "viso-e2e";

    fn create(app: &Path, platform: &'static str, kind: &'static str) -> Profile {
        let home = app.parent().expect("the scratch directory").join("home");
        let _ = std::fs::remove_dir_all(&home);
        viso(app, &home, &[platform, kind, "create", Self::ID]);
        Profile {
            app: app.to_owned(),
            home,
            platform,
            kind,
        }
    }

    fn run(&self) -> Run {
        let args = [self.platform, "--device", Self::ID];
        Run::start(&self.app, &args, &[("VISO_HOME", &self.home)])
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        for command in ["stop", "delete"] {
            let _ = Command::new(env!("CARGO_BIN_EXE_viso"))
                .args([self.platform, self.kind, command, Self::ID])
                .current_dir(&self.app)
                .env("VISO_HOME", &self.home)
                .stdin(Stdio::null())
                .status();
        }
    }
}

/// Whether `line` is the device event of `state`.
fn device_event(line: &str, state: &str) -> bool {
    line.contains("\"type\":\"device\"") && line.contains(&format!("\"state\":\"{state}\""))
}

/// The pid in `line` if it is the probe's log saying `said`.
fn probe_pid(line: &str, said: &str) -> Option<u32> {
    let marker = format!("\"message\":\"probe {said} ");
    let at = line.find(&marker)? + marker.len();
    line[at..].split('"').next()?.parse().ok()
}

/// Drives the session on `app`: a `.vs` edit is applied, a Rust edit
/// restarts the app with its state, and Ctrl-C ends the session. `logs`
/// says whether the app's own lines reach the session.
fn hot_reload(run: Run, app: &Path, logs: bool) {
    run.connected();

    std::fs::write(
        app.join("src/probe.vs"),
        VIEW.replace("\"{count}\"", "\"n={count}\""),
    )
    .expect("probe.vs");
    let applied = run.until(Duration::from_secs(60), "the patch", |line| {
        dev_event(line, "runtime-commit", "applied")
            || dev_event(line, "runtime-commit", "rejected")
    });
    assert!(
        dev_event(&applied, "runtime-commit", "applied"),
        "{applied}"
    );

    std::fs::write(app.join("src/main.rs"), main_rs("// edited")).expect("main.rs");
    let (mut first, mut restored, mut restarted) = (None, None, None);
    let deadline = Instant::now() + Duration::from_secs(900);
    while restarted.is_none() || (logs && restored.is_none()) {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = run
            .lines
            .recv_timeout(left)
            .unwrap_or_else(|_| panic!("the restart did not land: {restored:?} {restarted:?}"));
        first = first.or(probe_pid(&line, "launched"));
        restored = restored.or(probe_pid(&line, "restored"));
        if dev_event(&line, "warm-restart", "warm_restarted")
            || dev_event(&line, "warm-restart", "rejected")
        {
            restarted = Some(line);
        }
    }
    let restarted = restarted.expect("seen");
    assert!(
        dev_event(&restarted, "warm-restart", "warm_restarted"),
        "{restarted}"
    );
    assert!(restarted.contains("\"restored\":true"), "{restarted}");
    if logs {
        assert!(restored.is_some(), "the new app took the old one's state");
    }

    assert_eq!(run.stop(), Some(130));
}

#[test]
#[cfg(target_os = "macos")]
#[ignore = "builds a probe app for the iOS simulator and boots one"]
fn ios_simulator() {
    let app = project("device_ios");
    let profile = Profile::create(&app, "ios", "simulator");
    let run = profile.run();
    run.until(Duration::from_secs(300), "the booted simulator", |line| {
        device_event(line, "ready")
    });
    run.until(Duration::from_secs(1800), "the launch", |line| {
        device_event(line, "launched")
    });
    hot_reload(run, &app, true);
}

#[test]
#[ignore = "builds a probe app for the Android emulator and boots one"]
fn android_emulator() {
    let app = project("device_android");
    let profile = Profile::create(&app, "android", "emulator");
    let run = profile.run();
    run.until(Duration::from_secs(600), "the booted emulator", |line| {
        device_event(line, "ready")
    });
    run.until(Duration::from_secs(1800), "the launch", |line| {
        device_event(line, "launched")
    });
    hot_reload(run, &app, true);
}

/// A headless Chrome on `url`, ended when dropped.
struct Page(Child);

impl Page {
    fn open(url: &str, profile: &Path, args: &[String]) -> Page {
        let chrome = std::env::var_os("VISO_E2E_CHROME").map_or_else(
            || {
                if cfg!(target_os = "macos") {
                    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into()
                } else {
                    "google-chrome".into()
                }
            },
            PathBuf::from,
        );
        let child = Command::new(chrome)
            .args(["--headless=new", "--enable-unsafe-webgpu", "--no-first-run"])
            .arg(format!("--user-data-dir={}", profile.display()))
            .args(args)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("Chrome runs");
        Page(child)
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "builds a probe app for the web and opens it in a headless Chrome"]
fn web() {
    let app = project("device_web");
    let run = Run::start(&app, &["web-gpu", "--browser", "none"], &[]);
    // The server names its page once the first bundle is served.
    let server = run.until(Duration::from_secs(1800), "the dev server", |line| {
        line.contains("\"type\":\"server\"")
    });
    let at = server.find("\"url\":\"").expect("a url") + "\"url\":\"".len();
    let url = server[at..].split('"').next().expect("the url").to_owned();
    let scratch = app.parent().expect("the scratch directory").join("chrome");
    let _page = Page::open(&url, &scratch, &[]);
    hot_reload(run, &app, false);
}

/// `viso serve web-gpu --lan`: a page loaded at this machine's network
/// address, as another device would, connects and takes the patch. Chrome
/// treats that origin as secure, which plain HTTP is not, for WebGPU.
#[test]
#[ignore = "builds for the browser and drives Chrome over the machine's network address"]
fn web_lan() {
    let app = project("device_web_lan");
    let run = Run::command(&app, "serve", &["web-gpu", "--lan"], &[]);
    let server = run.until(Duration::from_secs(1800), "the dev server", |line| {
        line.contains("\"type\":\"server\"")
    });
    assert!(server.contains("\"lan\":true"), "{server}");
    let at = server.find("\"url\":\"").expect("a url") + "\"url\":\"".len();
    let url = server[at..].split('"').next().expect("the url").to_owned();
    assert!(!url.contains("127.0.0.1"), "the network address: {url}");
    let scratch = app.parent().expect("the scratch directory").join("chrome");
    let origin = url.trim_end_matches('/');
    let _page = Page::open(
        &url,
        &scratch,
        &[format!(
            "--unsafely-treat-insecure-origin-as-secure={origin}"
        )],
    );
    hot_reload(run, &app, false);
}
