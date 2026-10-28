//! The iOS Simulator deploy against a fake Xcode: a shell script standing in
//! for `xcrun simctl`, playing a simulator that boots, installs, launches the
//! app attached to its console, and logs the records `runningboardd` and
//! SpringBoard write of its spawn and end. The session's whole device
//! sequence runs with no simulator: a boot that fails, an app stopped, one
//! that aborts, a simulator that shuts down under it, and no Xcode at all.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use viso_project::{DeviceProfile, Platform, Preset};

use super::*;
use crate::device::simulator::FAKE_TOOLS;

/// `xcrun`: the simulator's state lives under `$STATE`; its log is
/// `$STATE/syslog`, which `log stream` follows.
const XCRUN: &str = r#"#!/bin/sh
STATE="__STATE__"
[ "$1" = simctl ] || { echo "xcrun: unknown $*" >&2; exit 1; }
shift
echo "$*" >> "$STATE/calls"
case "$1" in
list)
    if [ -f "$STATE/gone" ]; then echo '{"devices":{}}'; exit 0; fi
    state=Shutdown; [ -f "$STATE/booted" ] && state=Booted
    printf '{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-27-0":[{"udid":"FAKE-UDID","state":"%s"}]}}\n' "$state"
    ;;
boot)
    if [ -f "$STATE/noboot" ]; then echo "Unable to boot device in current state: Shutdown" >&2; exit 149; fi
    touch "$STATE/booted"
    ;;
bootstatus) [ -f "$STATE/booted" ] || exit 1 ;;
shutdown) rm -f "$STATE/booted"; touch "$STATE/down" ;;
install) [ -d "$3" ] || { echo "no such app: $3" >&2; exit 1; } ;;
terminate) touch "$STATE/terminate" ;;
spawn)
    echo "Filtering the log data using \"process == runningboardd\""
    exec tail -n +1 -f "$STATE/syslog"
    ;;
launch)
    id="$5"
    env | grep '^SIMCTL_CHILD_' | sort > "$STATE/env"
    echo "x runningboardd[1:2] [com.apple.runningboard:process] [app<$id((null))>:$$] This process will be managed." >> "$STATE/syslog"
    echo "$id: $$"
    echo "probe launched"
    exited() { echo "x SpringBoard[3:4] [com.apple.FrontBoard:Process] [app<$id>:$$] Process exited: $1." >> "$STATE/syslog"; }
    if [ -f "$STATE/crash" ]; then
        echo "panicked at src/main.rs"
        exited "<RBSProcessExitContext| specific, status:<RBSProcessExitStatus| domain:signal(2) code:SIGABRT(6)>>"
        exit 0
    fi
    while :; do
        if [ -f "$STATE/terminate" ]; then
            rm -f "$STATE/terminate"
            exited "<RBSProcessExitContext| specific, status:<RBSProcessExitStatus| domain:frontboard(10) code:force-quit(0xfbfbfbfb)>>"
            exit 0
        fi
        if [ -f "$STATE/down" ]; then echo "The simulator shut down" >&2; exit 1; fi
        sleep 0.05
    done
    ;;
*) echo "simctl: unknown $*" >&2; exit 1 ;;
esac
"#;

/// A fake Xcode, the simulator's state directory, and an `.app` to install.
struct Fake {
    tools: PathBuf,
    state: PathBuf,
    app: PathBuf,
}

impl Fake {
    fn new(name: &str) -> Fake {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("viso-ios-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (tools, state) = (dir.join("tools"), dir.join("state"));
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("syslog"), "").unwrap();
        for (name, script) in [
            (
                "xcrun",
                XCRUN.replace("__STATE__", &state.display().to_string()),
            ),
            ("open", "#!/bin/sh\nexit 0\n".to_owned()),
        ] {
            let path = tools.join(name);
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let app = dir.join("probe.app");
        std::fs::create_dir_all(&app).unwrap();
        FAKE_TOOLS.with(|dir| *dir.borrow_mut() = Some(tools.clone()));
        Fake { tools, state, app }
    }

    fn calls(&self) -> Vec<String> {
        read_lines(&self.state.join("calls"))
    }

    fn touch(&self, name: &str) {
        std::fs::write(self.state.join(name), b"").unwrap();
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        FAKE_TOOLS.with(|dir| *dir.borrow_mut() = None);
        if let Some(dir) = self.tools.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The lines of `path` once the fake writes it, waiting at most five
/// seconds.
fn arrived(path: &Path) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let lines = read_lines(path);
        if !lines.is_empty() || Instant::now() >= deadline {
            return lines;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn profile() -> DeviceProfile {
    DeviceProfile {
        id: "iphone-fake".into(),
        platform: Platform::Ios,
        preset: Preset::Phone,
        system: "com.apple.CoreSimulator.SimRuntime.iOS-27-0".into(),
        device: "FAKE-UDID".into(),
    }
}

fn vars() -> Vars<'static> {
    Vars {
        address: "127.0.0.1:4242".into(),
        token: "t0k",
        session: "5e55",
        build_id: "b111d",
        env: &[],
    }
}

/// How `app` ended, waiting at most five seconds.
fn ended(app: &mut App) -> Ended {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(ended) = app.try_wait().expect("the watch runs") {
            return ended;
        }
        assert!(Instant::now() < deadline, "the app never ended");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn launch(fake: &Fake) -> App {
    Deploy::Simulator { profile: profile() }
        .launch(&fake.app, &[], &vars(), true, None, true)
        .expect("it launches")
}

#[test]
fn a_session_boots_installs_launches_reads_and_stops_the_app() {
    let fake = Fake::new("session");
    simulator::boot(&profile()).expect("it boots");
    let calls = fake.calls();
    assert!(calls.iter().any(|c| c == "boot FAKE-UDID"), "{calls:?}");
    assert!(calls.iter().any(|c| c == "bootstatus FAKE-UDID -b"));

    let mut app = launch(&fake);
    // The dev variables reach the app through `simctl`'s child environment.
    let env = arrived(&fake.state.join("env"));
    for expected in [
        "SIMCTL_CHILD_VISO_DEV_RUNTIME=127.0.0.1:4242",
        "SIMCTL_CHILD_VISO_DEV_TOKEN=t0k",
        "SIMCTL_CHILD_VISO_DEV_SESSION=5e55",
        "SIMCTL_CHILD_VISO_DEV_BUILD=b111d",
        "SIMCTL_CHILD_VISO_DEV_RESTORE=1",
    ] {
        assert!(env.iter().any(|e| e == expected), "{env:?}");
    }
    let calls = fake.calls();
    assert!(
        calls
            .iter()
            .any(|c| c == &format!("install FAKE-UDID {}", fake.app.display()))
    );
    assert!(
        calls.iter().any(|c| c.starts_with(
            "launch --console-pty --terminate-running-process FAKE-UDID dev.viso.probe"
        )),
        "{calls:?}"
    );

    // The console's first line, `<id>: <pid>`, says nothing; the app's do.
    let mut outputs = app.outputs();
    let (console, lines) = outputs.remove(0);
    let said: Vec<_> = BufReader::new(console)
        .lines()
        .map_while(Result::ok)
        .take(2)
        .filter_map(|line| lines.read(line))
        .collect();
    assert_eq!(said, [("info", "probe launched".to_owned())]);

    // Stopped by the session: a clean end, as SpringBoard recorded it.
    assert!(app.try_wait().unwrap().is_none(), "it runs");
    app.close();
    let ended = ended(&mut app);
    assert!(ended.success(), "{ended}");
    assert_eq!(ended.to_string(), "terminated");
}

#[test]
fn an_app_that_aborts_ends_with_its_signal() {
    let fake = Fake::new("crash");
    fake.touch("booted");
    fake.touch("crash");
    simulator::boot(&profile()).expect("it runs already");
    assert!(
        !fake.calls().iter().any(|c| c.starts_with("boot ")),
        "a booted simulator is not booted again"
    );
    let mut app = launch(&fake);
    let ended = ended(&mut app);
    assert!(!ended.success());
    assert_eq!(ended.to_string(), "signal: 6 (SIGABRT)");
}

#[test]
fn a_simulator_that_shuts_down_mid_session_ends_it_as_a_failure() {
    let fake = Fake::new("down");
    fake.touch("booted");
    let mut app = launch(&fake);
    assert!(app.try_wait().unwrap().is_none(), "it runs");
    simulator::shutdown("FAKE-UDID").unwrap();
    let ended = ended(&mut app);
    assert!(!ended.success(), "{ended}");
    assert_eq!(
        simulator::state("FAKE-UDID"),
        Ok(simulator::State::Shutdown)
    );
}

#[test]
fn a_boot_that_fails_or_a_simulator_gone_says_why() {
    let fake = Fake::new("noboot");
    fake.touch("noboot");
    let why = simulator::boot(&profile()).unwrap_err();
    assert!(why.contains("Unable to boot device"), "{why}");

    fake.touch("gone");
    let why = simulator::boot(&profile()).unwrap_err();
    assert!(why.contains("`iphone-fake` is gone"), "{why}");
}

#[test]
fn without_xcode_nothing_runs_and_that_is_said() {
    let fake = Fake::new("noxcode");
    std::fs::remove_file(fake.tools.join("xcrun")).unwrap();
    let why = simulator::boot(&profile()).unwrap_err();
    assert!(why.ends_with("xcrun` is not installed"), "{why}");
    let why = Deploy::Simulator { profile: profile() }
        .launch(&fake.app, &[], &vars(), false, None, true)
        .err()
        .expect("nothing launches");
    assert!(why.contains("xcrun` is not installed"), "{why}");
}
