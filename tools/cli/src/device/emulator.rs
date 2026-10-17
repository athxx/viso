//! The Android Emulator through the SDK's `emulator` and `adb`: the virtual
//! devices (AVDs) Viso creates for its profiles, the adb serial of the one
//! running a profile, and the app installed, launched and stopped on it.
//!
//! The app reaches the dev channel on its own loopback address: `adb
//! reverse` forwards the emulator's port to the host's loopback listener, so
//! nothing listens beyond the host's loopback interface. The dev variables
//! reach it as string extras of the launch intent, which the platform hands
//! the app as its launch variables.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use viso_package::android::{Abi, Sdk};
use viso_project::{DeviceProfile, Preset};

use super::{capture, quiet, version_key};

/// How long a booting emulator may take to show up to adb, then to finish
/// booting.
const APPEAR_WAIT: Duration = Duration::from_secs(120);
const BOOT_WAIT: Duration = Duration::from_secs(300);

/// How often a wait polls.
const POLL: Duration = Duration::from_millis(500);

/// The AVD name of the profile `id`.
pub(crate) fn avd_name(id: &str) -> String {
    format!("viso_{}", id.replace('-', "_"))
}

/// Where AVDs live: `$ANDROID_AVD_HOME`, `$ANDROID_USER_HOME/avd` or
/// `~/.android/avd`.
fn avd_home() -> Option<PathBuf> {
    std::env::var_os("ANDROID_AVD_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("ANDROID_USER_HOME").map(|h| PathBuf::from(h).join("avd")))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".android/avd")))
}

/// An installed system image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SystemImage {
    /// `android-36`, `android-37.0`.
    pub(crate) platform: String,
    /// `google_apis`, `google_apis_playstore_ps16k`.
    pub(crate) tag: String,
    pub(crate) api: String,
}

impl SystemImage {
    fn sysdir(&self, abi: Abi) -> String {
        format!(
            "system-images/{}/{}/{}/",
            self.platform,
            self.tag,
            abi.as_str()
        )
    }
}

/// The installed system images for `abi`, newest API first, the plainest
/// tag first within one.
pub(crate) fn system_images(sdk: &Sdk, abi: Abi) -> Vec<SystemImage> {
    let root = sdk.root.join("system-images");
    let mut images = Vec::new();
    for platform in read_names(&root) {
        let Some(api) = platform.strip_prefix("android-") else {
            continue;
        };
        for tag in read_names(&root.join(&platform)) {
            if root.join(&platform).join(&tag).join(abi.as_str()).is_dir() {
                images.push(SystemImage {
                    api: api.to_owned(),
                    platform: platform.clone(),
                    tag,
                });
            }
        }
    }
    images.sort_by(|a, b| {
        version_key(&b.api)
            .cmp(&version_key(&a.api))
            .then_with(|| tag_rank(&a.tag).cmp(&tag_rank(&b.tag)))
    });
    images
}

/// How plain a tag is: Google APIs first, the Play Store ones after.
fn tag_rank(tag: &str) -> (usize, usize) {
    let rank = ["google_apis", "default", "aosp_atd", "google_atd"]
        .iter()
        .position(|t| *t == tag)
        .unwrap_or(4);
    (rank, tag.len())
}

fn read_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// Creates the AVD `name` on `image` for `abi` with the screen of `preset`.
pub(crate) fn create(
    name: &str,
    image: &SystemImage,
    abi: Abi,
    preset: Preset,
) -> Result<(), String> {
    let home = avd_home().ok_or("no home directory for AVDs (set ANDROID_AVD_HOME)")?;
    let dir = home.join(format!("{name}.avd"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let target = format!("android-{}", image.api);
    let ini = format!(
        "avd.ini.encoding=UTF-8\npath={}\npath.rel=avd/{name}.avd\ntarget={target}\n",
        dir.display()
    );
    let ini_path = home.join(format!("{name}.ini"));
    std::fs::write(&ini_path, ini).map_err(|e| format!("{}: {e}", ini_path.display()))?;
    let config = dir.join("config.ini");
    std::fs::write(&config, avd_config(name, image, abi, preset))
        .map_err(|e| format!("{}: {e}", config.display()))
}

/// The hardware of an AVD: a phone or tablet screen, the host's GPU where it
/// has one and SwiftShader where not (Vulkan through either), and enough
/// memory and storage for a dev app.
fn avd_config(name: &str, image: &SystemImage, abi: Abi, preset: Preset) -> String {
    let (width, height, density) = match preset {
        Preset::Phone => (1080, 2400, 420),
        Preset::Tablet => (1600, 2560, 320),
    };
    let arch = match abi {
        Abi::Arm64 => "arm64",
        Abi::Armv7 => "arm",
        Abi::X86_64 => "x86_64",
        Abi::X86 => "x86",
    };
    let play_store = image.tag.contains("playstore");
    format!(
        "AvdId={name}\n\
         avd.ini.displayname={name}\n\
         avd.ini.encoding=UTF-8\n\
         abi.type={abi}\n\
         hw.cpu.arch={arch}\n\
         hw.cpu.ncore=4\n\
         hw.ramSize=2048\n\
         vm.heapSize=256\n\
         disk.dataPartition.size=6G\n\
         hw.gpu.enabled=yes\n\
         hw.gpu.mode=auto\n\
         hw.keyboard=yes\n\
         hw.mainKeys=no\n\
         hw.lcd.width={width}\n\
         hw.lcd.height={height}\n\
         hw.lcd.density={density}\n\
         hw.initialOrientation=portrait\n\
         image.sysdir.1={sysdir}\n\
         tag.id={tag}\n\
         PlayStore.enabled={play_store}\n\
         target=android-{api}\n\
         fastboot.forceColdBoot=no\n\
         showDeviceFrame=no\n",
        abi = abi.as_str(),
        sysdir = image.sysdir(abi),
        tag = image.tag,
        api = image.api,
    )
}

/// Removes the AVD `name`.
pub(crate) fn delete(name: &str) -> Result<(), String> {
    let home = avd_home().ok_or("no home directory for AVDs")?;
    let _ = std::fs::remove_file(home.join(format!("{name}.ini")));
    match std::fs::remove_dir_all(home.join(format!("{name}.avd"))) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// The adb serial of the running emulator of the AVD `name`.
pub(crate) fn serial(sdk: &Sdk, name: &str) -> Option<String> {
    let devices = capture(Command::new(&sdk.adb).arg("devices")).ok()?;
    devices
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (serial, state) = line.split_once('\t')?;
            (serial.starts_with("emulator-") && state.trim() == "device").then_some(serial)
        })
        .find(|serial| {
            capture(Command::new(&sdk.adb).args(["-s", serial, "emu", "avd", "name"]))
                .is_ok_and(|out| out.lines().next().map(str::trim) == Some(name))
        })
        .map(str::to_owned)
}

/// Starts the emulator of `profile` unless it runs, waits until it has
/// booted, and returns its serial. Its window opens when `window` and the
/// host has a display. The emulator runs on in a process group of its own,
/// so the session's end and its Ctrl-C leave it booted.
pub(crate) fn boot(sdk: &Sdk, profile: &DeviceProfile, window: bool) -> Result<String, String> {
    let name = profile.device.as_str();
    let window = window && has_display();
    let serial = match serial(sdk, name) {
        Some(serial) => serial,
        None => {
            let mut emulator = Command::new(&sdk.emulator);
            emulator
                .args(["-avd", name, "-no-snapshot-save", "-no-boot-anim"])
                .args((!window).then_some("-no-window"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            detach(&mut emulator);
            crate::process::spawn(&mut emulator)
                .map_err(|e| format!("cannot run `{}`: {e}", sdk.emulator.display()))?;
            let since = Instant::now();
            loop {
                if let Some(serial) = serial(sdk, name) {
                    break serial;
                }
                if since.elapsed() >= APPEAR_WAIT {
                    return Err(format!("the emulator of `{}` did not start", profile.id));
                }
                thread::sleep(POLL);
            }
        }
    };
    let since = Instant::now();
    while !booted(sdk, &serial) {
        if since.elapsed() >= BOOT_WAIT {
            return Err(format!(
                "the emulator of `{}` did not finish booting",
                profile.id
            ));
        }
        thread::sleep(POLL);
    }
    Ok(serial)
}

/// Whether the emulator `serial` finished booting and its package manager
/// answers.
fn booted(sdk: &Sdk, serial: &str) -> bool {
    let adb = |args: &[&str]| capture(Command::new(&sdk.adb).args(["-s", serial]).args(args));
    adb(&["shell", "getprop", "sys.boot_completed"]).is_ok_and(|v| v.trim() == "1")
        && adb(&["shell", "pm", "path", "android"]).is_ok_and(|v| v.contains("package:"))
}

/// Puts `command` in a process group of its own.
fn detach(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
}

/// Stops the emulator `serial`.
pub(crate) fn kill(sdk: &Sdk, serial: &str) -> Result<(), String> {
    quiet(Command::new(&sdk.adb).args(["-s", serial, "emu", "kill"]))
}

/// The device side of one app on one running emulator.
#[derive(Debug, Clone)]
pub(crate) struct Device {
    pub(crate) adb: PathBuf,
    pub(crate) serial: String,
}

impl Device {
    fn adb(&self) -> Command {
        let mut command = Command::new(&self.adb);
        command.args(["-s", &self.serial]);
        command
    }

    /// Installs `apk`, replacing the app with its id (which stops it).
    pub(crate) fn install(&self, apk: &Path) -> Result<(), String> {
        quiet(self.adb().args(["install", "-r", "-t"]).arg(apk))
    }

    /// Forwards the emulator's loopback `port` to the host's.
    pub(crate) fn reverse(&self, port: u16) -> Result<(), String> {
        let port = format!("tcp:{port}");
        quiet(self.adb().args(["reverse", &port, &port]))
    }

    /// Stops forwarding `port`.
    pub(crate) fn unreverse(&self, port: u16) {
        let _ = crate::process::status(
            self.adb()
                .args(["reverse", "--remove", &format!("tcp:{port}")]),
            crate::process::QUICK,
        );
    }

    /// Starts `component` with `extras` as string extras, stopping the app
    /// first if it runs, and returns the pid of its process; when it ends
    /// before it is seen, the error carries what it logged.
    pub(crate) fn launch(
        &self,
        component: &str,
        package: &str,
        extras: &[(&str, String)],
    ) -> Result<u32, String> {
        let started = capture(self.adb().args(["shell", "date", "'+%m-%d %H:%M:%S.000'"]))
            .map(|time| time.trim().to_owned());
        let mut command = self.adb();
        command.args(["shell", "am", "start", "-W", "-S", "-n", component]);
        for (name, value) in extras {
            command.args(["--es", name, &shell_word(value)]);
        }
        quiet(&mut command)?;
        let since = Instant::now();
        loop {
            if let Ok(out) = capture(self.adb().args(["shell", "pidof", package]))
                && let Some(pid) = out.split_whitespace().next().and_then(|p| p.parse().ok())
            {
                return Ok(pid);
            }
            if since.elapsed() >= Duration::from_secs(10) {
                let mut why = format!("`{package}` did not start");
                if let Ok(time) = &started
                    && let Ok(log) = capture(
                        self.adb()
                            .args(["logcat", "-d", "-v", "brief", "-T", time, "viso:V", "*:S"]),
                    )
                {
                    for line in log.lines().filter_map(|line| log_line(line)) {
                        why.push_str("\n  ");
                        why.push_str(line.1);
                    }
                }
                return Err(why);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// The process that ends when `pid` does.
    pub(crate) fn watch(&self, pid: u32) -> std::io::Result<Child> {
        crate::process::spawn(
            self.adb()
                .args([
                    "shell",
                    &format!("while [ -d /proc/{pid} ]; do sleep 0.2; done"),
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
    }

    /// The log of `pid`: the app's own lines (the `viso` tag) and every
    /// warning and error of its process, in logcat's brief format.
    pub(crate) fn logcat(&self, pid: u32) -> std::io::Result<Child> {
        crate::process::spawn(
            self.adb()
                .args([
                    "logcat",
                    "-v",
                    "brief",
                    &format!("--pid={pid}"),
                    "viso:V",
                    "*:W",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
    }

    /// How the process `pid` of `package` ended, as Android recorded it:
    /// whether that is a clean end, and how it reads. `None` while nothing
    /// is recorded for it.
    pub(crate) fn exit(&self, package: &str, pid: u32) -> Option<(bool, String)> {
        let dump = capture(
            self.adb()
                .args(["shell", "dumpsys", "activity", "exit-info", package]),
        )
        .ok()?;
        let (reason, name, status) = exit_info(&dump, pid)?;
        Some(match reason {
            EXIT_SELF => (status == 0, format!("exit status: {status}")),
            SIGNALED => (false, format!("signal: {status}")),
            USER_REQUESTED | USER_STOPPED => (true, name.to_lowercase().replace('_', " ")),
            _ => (false, name.to_lowercase().replace('_', " ")),
        })
    }

    /// Stops `package`.
    pub(crate) fn force_stop(&self, package: &str) {
        let _ = crate::process::status(
            self.adb().args(["shell", "am", "force-stop", package]),
            crate::process::QUICK,
        );
    }
}

/// The reasons of `ApplicationExitInfo` that say how to read its status:
/// the code the process exited with, the signal that ended it; and the ones
/// a user's stop records.
const EXIT_SELF: u32 = 1;
const SIGNALED: u32 = 2;
const USER_REQUESTED: u32 = 10;
const USER_STOPPED: u32 = 11;

/// The reason, its name and the status `dumpsys activity exit-info` records
/// for the process `pid`: `pid=123 …` then `reason=1 (EXIT_SELF) … status=0`.
fn exit_info(dump: &str, pid: u32) -> Option<(u32, String, i32)> {
    let mut lines = dump.lines();
    let pid = format!("pid={pid}");
    lines.find(|line| line.split_whitespace().any(|word| word == pid))?;
    let line = lines.next()?;
    let field = |key: &str| {
        line.split_whitespace()
            .find_map(|word| word.strip_prefix(key))
            .map(str::to_owned)
    };
    let reason = field("reason=")?.parse().ok()?;
    let name = line
        .split_once("reason=")?
        .1
        .split_once('(')?
        .1
        .split_once(')')?
        .0
        .to_owned();
    let status = field("status=")?.parse().ok()?;
    Some((reason, name, status))
}

/// Whether a window can open: always on macOS and Windows, with an X11 or
/// Wayland display elsewhere.
fn has_display() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
        || ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// `value` as one word of the device's shell: `adb shell` joins its
/// arguments into one command line.
fn shell_word(value: &str) -> String {
    if value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b".:-_/,=".contains(&b))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// A line of logcat's brief format as its level and message: `I/viso (
/// 123): text`. Other lines are `None`.
pub(crate) fn log_line(line: &str) -> Option<(&'static str, &str)> {
    let (head, message) = line.split_once("): ")?;
    let (priority, tag) = head.split_once('/')?;
    let level = match priority {
        "V" | "D" | "I" => "info",
        "W" => "warn",
        "E" | "F" | "A" => "error",
        _ => return None,
    };
    let tag = tag.split('(').next().unwrap_or("").trim();
    Some((level, if tag == "viso" { message } else { line }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exit_reads_from_the_record_of_its_pid() {
        let dump = "  package: dev.viso.probe\n\
            \x20   ApplicationExitInfo #0:\n\
            \x20     timestamp=2026-10-14 15:24:11.962 pid=8794 realUid=10226 user=0\n\
            \x20     process=dev.viso.probe reason=1 (EXIT_SELF) subreason=0 (UNKNOWN) status=101\n\
            \x20   ApplicationExitInfo #1:\n\
            \x20     timestamp=2026-10-14 15:20:01.000 pid=8701 realUid=10226 user=0\n\
            \x20     process=dev.viso.probe reason=10 (USER_REQUESTED) subreason=23 (FORCE STOP) status=0\n";
        assert_eq!(exit_info(dump, 8794), Some((1, "EXIT_SELF".into(), 101)));
        assert_eq!(
            exit_info(dump, 8701),
            Some((10, "USER_REQUESTED".into(), 0))
        );
        assert_eq!(exit_info(dump, 87), None);
    }

    #[test]
    fn profiles_name_their_avd() {
        assert_eq!(avd_name("pixel-local"), "viso_pixel_local");
    }

    #[test]
    fn an_avd_runs_its_image_on_the_gpu_the_host_has() {
        let image = SystemImage {
            platform: "android-37.0".into(),
            tag: "google_apis_playstore_ps16k".into(),
            api: "37.0".into(),
        };
        let config = avd_config("viso_a", &image, Abi::Arm64, Preset::Tablet);
        assert!(config.contains(
            "image.sysdir.1=system-images/android-37.0/google_apis_playstore_ps16k/arm64-v8a/\n"
        ));
        assert!(config.contains("hw.gpu.mode=auto\n"));
        assert!(config.contains("hw.lcd.width=1600\n"));
        assert!(config.contains("abi.type=arm64-v8a\nhw.cpu.arch=arm64\n"));
        assert!(config.contains("PlayStore.enabled=true\n"));
    }

    #[test]
    fn plain_images_sort_before_play_store_ones() {
        let mut tags = ["google_apis_playstore", "google_apis", "default"];
        tags.sort_by_key(|t| tag_rank(t));
        assert_eq!(tags, ["google_apis", "default", "google_apis_playstore"]);
    }

    #[test]
    fn logcat_lines_carry_their_level() {
        assert_eq!(
            log_line("I/viso    ( 4242): probe launched 1"),
            Some(("info", "probe launched 1"))
        );
        assert_eq!(
            log_line("E/AndroidRuntime( 4242): FATAL EXCEPTION: main"),
            Some(("error", "E/AndroidRuntime( 4242): FATAL EXCEPTION: main"))
        );
        assert_eq!(log_line("--------- beginning of main"), None);
    }

    #[test]
    fn shell_words_are_quoted_when_they_must_be() {
        assert_eq!(shell_word("127.0.0.1:4242"), "127.0.0.1:4242");
        assert_eq!(shell_word("a b"), "'a b'");
        assert_eq!(shell_word("it's"), "'it'\\''s'");
    }
}
