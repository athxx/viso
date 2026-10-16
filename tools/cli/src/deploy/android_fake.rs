//! The Android deploy against a fake SDK: shell scripts standing in for
//! `emulator` and `adb`, playing a device that boots, installs, forwards the
//! dev port, starts the app as a process of its own, streams its log, and
//! records how it ended the way `dumpsys activity exit-info` does. The
//! session's whole device sequence runs on any Unix host, with no emulator.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use viso_package::android::Sdk;
use viso_project::{DeviceProfile, Platform, Preset};

use super::*;

/// `adb`: the device's state lives under `$STATE`. `am start` runs the app
/// as a background shell that logs as `viso` and runs until stopped, or, when
/// `$STATE/crash` exists, exits with 101 on its own; each end is recorded.
const ADB: &str = r#"#!/bin/sh
STATE="__STATE__"
echo "$*" >> "$STATE/calls"
[ "$1" = "-s" ] && shift 2
case "$1" in
devices)
    echo "List of devices attached"
    [ -f "$STATE/avd" ] && printf 'emulator-5554\tdevice\n'
    ;;
emu) cat "$STATE/avd"; echo OK ;;
install) [ -f "$4" ] || { echo "no such apk" >&2; exit 1; } ;;
reverse) ;;
logcat)
    if [ "$2" = "-d" ]; then cat "$STATE/log" 2>/dev/null; exit 0; fi
    pid=$(echo "$4" | sed 's/--pid=//')
    cat "$STATE/log"
    while kill -0 "$pid" 2>/dev/null; do sleep 0.05; done
    ;;
shell)
    shift
    case "$1" in
    getprop) echo 1 ;;
    pm) echo "package:/system/framework/framework-res.apk" ;;
    date) echo "10-14 12:00:00.000" ;;
    pidof) [ -f "$STATE/pid" ] && kill -0 "$(cat "$STATE/pid")" 2>/dev/null && cat "$STATE/pid" || exit 1 ;;
    am)
        if [ "$2" = "force-stop" ]; then
            pid=$(cat "$STATE/pid")
            echo "$pid 10 USER_REQUESTED 0" >> "$STATE/exits"
            kill "$pid"
            exit 0
        fi
        shift 6
        : > "$STATE/extras"
        while [ "$#" -gt 0 ]; do echo "$2=$3" >> "$STATE/extras"; shift 3; done
        if [ -f "$STATE/crash" ]; then
            sh -c 'echo "E/viso    ( $$): panicked at src/main.rs" >> "$0/log"; sleep 0.3; echo "$$ 1 EXIT_SELF 101" >> "$0/exits"; exit 101' "$STATE" </dev/null >/dev/null 2>&1 &
        else
            sh -c 'echo "I/viso    ( $$): probe launched" >> "$0/log"; trap "exit 0" TERM; while :; do sleep 0.05; done' "$STATE" </dev/null >/dev/null 2>&1 &
        fi
        echo $! > "$STATE/pid"
        ;;
    dumpsys)
        echo "  package: $4"
        [ -f "$STATE/exits" ] && while read -r pid reason name status; do
            echo "    ApplicationExitInfo:"
            echo "      timestamp=2026-10-14 12:00:00.000 pid=$pid realUid=10226 user=0"
            echo "      process=$4 reason=$reason ($name) subreason=0 (UNKNOWN) status=$status"
        done < "$STATE/exits"
        ;;
    *)
        # The pid watch: `while [ -d /proc/<pid> ]; ...`.
        pid=$(echo "$*" | sed 's|.*/proc/\([0-9]*\).*|\1|')
        while kill -0 "$pid" 2>/dev/null; do sleep 0.05; done
        ;;
    esac
    ;;
*) echo "adb: unknown $*" >&2; exit 1 ;;
esac
"#;

/// `emulator`: the AVD named after `-avd` shows up to adb a moment later.
const EMULATOR: &str = r#"#!/bin/sh
STATE="__STATE__"
echo "emulator $*" >> "$STATE/calls"
(sleep 0.2; echo "$2" > "$STATE/avd") </dev/null >/dev/null 2>&1 &
"#;

/// A fake SDK, its state directory, and an APK to install.
struct Fake {
    sdk: Sdk,
    state: PathBuf,
    apk: PathBuf,
}

impl Fake {
    fn new(name: &str) -> Fake {
        let dir = std::env::temp_dir().join(format!("viso-android-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = dir.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let tool = |name: &str, script: &str| {
            let path = dir.join(name);
            std::fs::write(
                &path,
                script.replace("__STATE__", &state.display().to_string()),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let sdk = Sdk {
            root: dir.clone(),
            origin: viso_package::android::SdkOrigin::Viso,
            ndk_bin: dir.clone(),
            build_tools: dir.clone(),
            platform_jar: dir.join("android.jar"),
            adb: tool("adb", ADB),
            emulator: tool("emulator", EMULATOR),
        };
        let apk = dir.join("probe.apk");
        std::fs::write(&apk, b"apk").unwrap();
        Fake { sdk, state, apk }
    }

    /// What the device was asked, one line per call.
    fn calls(&self) -> Vec<String> {
        read_lines(&self.state.join("calls"))
    }

    /// The deploy onto the device booted for `profile`.
    fn deploy(&self, serial: String) -> Deploy {
        Deploy::Emulator {
            sdk: self.sdk.clone(),
            device: Device {
                adb: self.sdk.adb.clone(),
                serial,
            },
            shell: android::Shell {
                java: Vec::new(),
                manifest: PathBuf::new(),
            },
            reversed: None,
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        if let Some(dir) = self.state.parent() {
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

fn profile() -> DeviceProfile {
    DeviceProfile {
        id: "pixel-fake".into(),
        platform: Platform::Android,
        preset: Preset::Phone,
        system: "36".into(),
        device: "viso_pixel_fake".into(),
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

#[test]
fn a_session_boots_installs_forwards_launches_reads_and_stops_the_app() {
    let fake = Fake::new("session");
    let serial = emulator::boot(&fake.sdk, &profile(), false).expect("it boots");
    assert_eq!(serial, "emulator-5554");
    let calls = fake.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("emulator -avd viso_pixel_fake") && c.contains("-no-window")),
        "{calls:?}"
    );

    let mut deploy = fake.deploy(serial);
    let mut app = deploy
        .launch(&fake.apk, &[], &vars(), true, None, true)
        .expect("it launches");
    // The dev variables arrive as the launch intent's extras, the dev port
    // forwarded to the host's.
    let extras = read_lines(&fake.state.join("extras"));
    for expected in [
        "VISO_DEV_RUNTIME=127.0.0.1:4242",
        "VISO_DEV_TOKEN=t0k",
        "VISO_DEV_SESSION=5e55",
        "VISO_DEV_BUILD=b111d",
        "VISO_DEV_RESTORE=1",
    ] {
        assert!(extras.iter().any(|e| e == expected), "{extras:?}");
    }
    let calls = fake.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.ends_with(&format!("install -r -t {}", fake.apk.display())))
    );
    assert!(
        calls
            .iter()
            .any(|c| c.ends_with("reverse tcp:4242 tcp:4242"))
    );
    assert!(
        calls
            .iter()
            .any(|c| c.contains("am start -W -S -n dev.viso.probe/dev.viso.VisoActivity")),
        "{calls:?}"
    );

    // Its log comes through logcat, at each line's priority.
    let mut logs = app.outputs();
    let (log, lines) = logs.pop().expect("the logcat");
    assert_eq!(lines, Lines::Logcat);
    let first = BufReader::new(log).lines().next().unwrap().unwrap();
    assert_eq!(lines.read(first), Some(("info", "probe launched".into())));

    // Stopped by the session: a clean end, as the device recorded it.
    assert!(app.try_wait().unwrap().is_none(), "it runs");
    app.close();
    let ended = ended(&mut app);
    assert!(ended.success(), "{ended}");
    assert_eq!(ended.to_string(), "user requested");

    // A relaunch on the same port forwards nothing again; the session's end
    // takes the forward back.
    let mut again = deploy
        .launch(&fake.apk, &[], &vars(), false, None, true)
        .expect("it relaunches");
    let reverses = fake
        .calls()
        .iter()
        .filter(|c| c.contains(" reverse tcp:"))
        .count();
    assert_eq!(reverses, 1);
    again.kill();
    deploy.ended();
    assert!(
        fake.calls()
            .iter()
            .any(|c| c.ends_with("reverse --remove tcp:4242"))
    );
}

#[test]
fn an_app_that_panics_ends_with_its_exit_status() {
    let fake = Fake::new("crash");
    std::fs::write(fake.state.join("crash"), b"").unwrap();
    std::fs::write(fake.state.join("avd"), b"viso_pixel_fake\n").unwrap();
    let serial = emulator::boot(&fake.sdk, &profile(), false).expect("it runs already");
    assert!(
        !fake.calls().iter().any(|c| c.starts_with("emulator ")),
        "a running emulator is not started again"
    );
    let mut deploy = fake.deploy(serial);
    let mut app = deploy
        .launch(&fake.apk, &[], &vars(), false, None, true)
        .expect("it launches");
    let ended = ended(&mut app);
    assert!(!ended.success());
    assert_eq!(ended.to_string(), "exit status: 101");
}
