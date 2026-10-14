//! The Apple Simulator through `xcrun simctl`: the simulators Viso creates
//! for its profiles, their boot state, and the app installed, launched and
//! stopped on one (`Viso_CLI.md`, `Viso_Hot_Reload.md`).
//!
//! A simulator shares the host's network, so the app reaches the dev channel
//! on the host's loopback address as a desktop app does; `simctl launch`
//! hands it the dev variables as its environment (each `SIMCTL_CHILD_` name
//! with the prefix taken off). Nothing is signed beyond ad-hoc and nothing is
//! provisioned.
//!
//! `simctl launch` ends with 0 however the app ended, and the simulator keeps
//! no exit code of an app that exits on its own; it records only how the app
//! was ended — by a signal, by a force quit, or on its own. [`ExitWatch`]
//! reads that record from the simulator's log, so an app that aborts — a
//! panic does — ends the session as a failure.

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use viso_ende::JsonValue;
use viso_project::{DeviceProfile, Preset};

use super::{capture, quiet, version_key};

/// Where `simctl` says a simulator is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Booted,
    Shutdown,
    /// Booting or shutting down.
    Changing,
    /// The UDID names no simulator any more.
    Gone,
}

fn simctl() -> Command {
    let mut command = Command::new("xcrun");
    command.arg("simctl");
    command
}

/// The newest available iOS runtime and, for `preset`, the device type of
/// its plain current model: `(runtime, device type, device name)`.
pub(crate) fn newest_runtime(preset: Preset) -> Result<(String, String, String), String> {
    let text = capture(simctl().args(["list", "runtimes", "-j"]))?;
    let json = JsonValue::parse(&text).map_err(|e| format!("`simctl` wrote no JSON: {e}"))?;
    let runtimes = json
        .get("runtimes")
        .and_then(JsonValue::as_array)
        .unwrap_or_default();
    let runtime = runtimes
        .iter()
        .filter(|r| r.get("isAvailable").and_then(JsonValue::as_bool) == Some(true))
        .filter(|r| r.get("platform").and_then(JsonValue::as_str) == Some("iOS"))
        .max_by_key(|r| version_key(r.get("version").and_then(JsonValue::as_str).unwrap_or("")))
        .ok_or(
            "no iOS Simulator runtime is installed (install one in Xcode › Settings › Components)",
        )?;
    let id = runtime
        .get("identifier")
        .and_then(JsonValue::as_str)
        .ok_or("`simctl` names no runtime identifier")?;
    let types: Vec<(&str, &str)> = runtime
        .get("supportedDeviceTypes")
        .and_then(JsonValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(|t| {
            Some((
                t.get("name")?.as_str()?,
                t.get("identifier")?.as_str()?,
                t.get("productFamily")?.as_str()?,
            ))
        })
        .filter(|(_, _, family)| {
            *family
                == match preset {
                    Preset::Phone => "iPhone",
                    Preset::Tablet => "iPad",
                }
        })
        .map(|(name, identifier, _)| (name, identifier))
        .collect();
    let (name, device_type) = pick_model(preset, &types)
        .ok_or_else(|| format!("the runtime `{id}` has no {} model", preset.as_str()))?;
    Ok((id.to_owned(), device_type.to_owned(), name.to_owned()))
}

/// The model a preset makes: the newest plain `iPhone N`, or an 11-inch iPad
/// Pro; otherwise the last of the family.
fn pick_model<'a>(preset: Preset, types: &[(&'a str, &'a str)]) -> Option<(&'a str, &'a str)> {
    let plain = match preset {
        Preset::Phone => types
            .iter()
            .filter_map(|&(name, id)| {
                let number: u32 = name.strip_prefix("iPhone ")?.parse().ok()?;
                Some((number, (name, id)))
            })
            .max_by_key(|(number, _)| *number)
            .map(|(_, model)| model),
        Preset::Tablet => types
            .iter()
            .rev()
            .find(|(name, _)| name.starts_with("iPad Pro 11"))
            .copied(),
    };
    plain.or_else(|| types.last().copied())
}

/// Creates a simulator named `name` of `device_type` on `runtime`; its UDID.
pub(crate) fn create(name: &str, device_type: &str, runtime: &str) -> Result<String, String> {
    let udid = capture(simctl().args(["create", name, device_type, runtime]))?;
    Ok(udid.trim().to_owned())
}

/// Where the simulator `udid` is.
pub(crate) fn state(udid: &str) -> Result<State, String> {
    let text = capture(simctl().args(["list", "devices", "-j"]))?;
    let json = JsonValue::parse(&text).map_err(|e| format!("`simctl` wrote no JSON: {e}"))?;
    let Some(JsonValue::Object(runtimes)) = json.get("devices") else {
        return Err("`simctl` lists no devices".into());
    };
    let device = runtimes
        .iter()
        .filter_map(|(_, devices)| devices.as_array())
        .flatten()
        .find(|d| d.get("udid").and_then(JsonValue::as_str) == Some(udid));
    Ok(match device.and_then(|d| d.get("state")?.as_str()) {
        None => State::Gone,
        Some("Booted") => State::Booted,
        Some("Shutdown") => State::Shutdown,
        Some(_) => State::Changing,
    })
}

/// Boots the simulator of `profile` unless it runs, waits until it is ready,
/// and brings the Simulator app up on it.
pub(crate) fn boot(profile: &DeviceProfile) -> Result<(), String> {
    let udid = profile.device.as_str();
    match state(udid)? {
        State::Gone => {
            return Err(format!(
                "the simulator of `{}` is gone; create the profile again",
                profile.id
            ));
        }
        State::Shutdown => quiet(simctl().args(["boot", udid]))?,
        State::Booted | State::Changing => {}
    }
    quiet(simctl().args(["bootstatus", udid, "-b"]))?;
    // The window shows the device; the app runs whether or not it is open.
    let _ = Command::new("open")
        .args([
            "-g",
            "-a",
            "Simulator",
            "--args",
            "-CurrentDeviceUDID",
            udid,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(())
}

pub(crate) fn shutdown(udid: &str) -> Result<(), String> {
    match state(udid)? {
        State::Booted | State::Changing => quiet(simctl().args(["shutdown", udid])),
        State::Shutdown | State::Gone => Ok(()),
    }
}

pub(crate) fn delete(udid: &str) -> Result<(), String> {
    if state(udid)? == State::Gone {
        return Ok(());
    }
    quiet(simctl().args(["delete", udid]))
}

/// Installs the `.app` at `app`, replacing the one with its bundle
/// identifier (which stops it).
pub(crate) fn install(udid: &str, app: &Path) -> Result<(), String> {
    quiet(simctl().args(["install", udid]).arg(app))
}

/// Launches the app `id` with `args` and the variables `env`, stopping it
/// first if it runs. The child is `simctl` attached to the app's console: it
/// writes the app's stdout and stderr, and ends when the app does.
pub(crate) fn launch(
    udid: &str,
    id: &str,
    args: &[OsString],
    env: &[(&str, String)],
    piped: bool,
) -> std::io::Result<Child> {
    let mut command = simctl();
    command
        .args([
            "launch",
            "--console-pty",
            "--terminate-running-process",
            udid,
            id,
        ])
        .args(args)
        .stdin(Stdio::null());
    for (name, value) in env {
        command.env(format!("SIMCTL_CHILD_{name}"), value);
    }
    if piped {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    command.spawn()
}

/// Stops the app `id`.
pub(crate) fn terminate(udid: &str, id: &str) {
    let _ = simctl()
        .args(["terminate", udid, id])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// How long the simulator's log has to start streaming.
const WATCH_START: Duration = Duration::from_secs(10);

/// What the simulator records of how the app ends: of the instance spawned
/// last since the watch began, so a stale instance a launch stops is not
/// taken for it.
pub(crate) struct ExitWatch {
    stream: Child,
    record: Arc<Mutex<Option<(bool, String)>>>,
}

impl ExitWatch {
    /// Watches the app `id` on the simulator `udid`, once the log streams.
    pub(crate) fn start(udid: &str, id: &str) -> Result<ExitWatch, String> {
        let predicate = format!(
            "(process == \"runningboardd\" AND eventMessage CONTAINS \"[app<{id}(\" \
             AND eventMessage ENDSWITH \"This process will be managed.\") \
             OR (process == \"SpringBoard\" AND eventMessage CONTAINS \"[app<{id}>:\" \
             AND eventMessage CONTAINS \"Process exited:\")"
        );
        let mut stream = simctl()
            .args([
                "spawn",
                udid,
                "log",
                "stream",
                "--style",
                "compact",
                "--predicate",
            ])
            .arg(predicate)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot read the simulator's log: {e}"))?;
        let stdout = stream.stdout.take().expect("piped");
        let record = Arc::new(Mutex::new(None));
        let (started, ready) = std::sync::mpsc::channel();
        let shared = Arc::clone(&record);
        let id = id.to_owned();
        std::thread::Builder::new()
            .name("viso-simulator-exits".into())
            .spawn(move || {
                let mut spawned = None;
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    // The stream's header comes first.
                    let _ = started.send(());
                    if let Some(pid) = spawned_pid(&line, &id) {
                        spawned = Some(pid);
                    } else if let Some((pid, ended)) = exit_record(&line, &id)
                        && Some(pid) == spawned
                    {
                        *shared.lock().unwrap_or_else(|e| e.into_inner()) = Some(ended);
                    }
                }
            })
            .map_err(|e| format!("cannot read the simulator's log: {e}"))?;
        if ready.recv_timeout(WATCH_START).is_err() {
            let _ = stream.kill();
            let _ = stream.wait();
            return Err("the simulator's log does not stream".into());
        }
        Ok(ExitWatch { stream, record })
    }

    /// How the app ended — whether cleanly, and how that reads — once the
    /// simulator recorded it.
    pub(crate) fn record(&self) -> Option<(bool, String)> {
        self.record
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Drop for ExitWatch {
    fn drop(&mut self) {
        let _ = self.stream.kill();
        let _ = self.stream.wait();
    }
}

/// The pid of `runningboardd`'s record that it manages the app `id`, a
/// process just spawned.
fn spawned_pid(line: &str, id: &str) -> Option<u32> {
    if !line.ends_with("This process will be managed.") {
        return None;
    }
    let rest = &line[line.find(&format!("[app<{id}("))?..];
    rest[rest.find(">:")? + 2..].split(']').next()?.parse().ok()
}

/// The pid and end of SpringBoard's record that the app `id` exited: on its
/// own, or force quit (as `simctl terminate` does), is clean; a signal or
/// any other status is not.
fn exit_record(line: &str, id: &str) -> Option<(u32, (bool, String))> {
    let marker = format!("[app<{id}>:");
    let rest = &line[line.find(&marker)? + marker.len()..];
    let (pid, rest) = rest.split_once(']')?;
    let pid = pid.parse().ok()?;
    let context = rest
        .trim_start()
        .strip_prefix("Process exited: <RBSProcessExitContext| ")?;
    if context.starts_with("voluntary") {
        return Some((pid, (true, "exited".into())));
    }
    let status = context.split("RBSProcessExitStatus| ").nth(1)?;
    let status = status.split('>').next()?.trim();
    let field = |name: &str| {
        status
            .split_whitespace()
            .find_map(|word| word.strip_prefix(name))
            .map(str::to_owned)
    };
    let domain = field("domain:")?;
    let code = field("code:").unwrap_or_default();
    let ended = if domain.starts_with("signal(") {
        // `SIGABRT(6)` reads as the host's `signal: 6 (SIGABRT)`.
        let (name, number) = code.split_once('(').unwrap_or((&code, ""));
        let number = number.trim_end_matches(')');
        (false, format!("signal: {number} ({name})"))
    } else if domain.starts_with("frontboard(") && code.starts_with("force-quit") {
        (true, "terminated".into())
    } else {
        (false, format!("{domain} {code}"))
    };
    Some((pid, ended))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_simulator_log_names_the_spawn_and_how_the_app_ended() {
        let spawn = "2026-10-15 00:30:03.687 Df runningboardd[36419:dc169f] \
                     [com.apple.runningboard:process] [app<dev.viso.probe((null))>:38774] \
                     This process will be managed.";
        assert_eq!(spawned_pid(spawn, "dev.viso.probe"), Some(38774));
        assert_eq!(spawned_pid(spawn, "dev.viso.other"), None);
        assert_eq!(
            spawned_pid(
                &spawn.replace("will be managed.", "is gone."),
                "dev.viso.probe"
            ),
            None
        );

        let exit = |context: &str| {
            format!(
                "2026-10-15 00:30:03.735 Df SpringBoard[36416:dc16a8] \
                 [com.apple.FrontBoard:Process] [app<dev.viso.probe>:36931] \
                 Process exited: {context}."
            )
        };
        let read = |context: &str| exit_record(&exit(context), "dev.viso.probe");
        assert_eq!(
            read("<RBSProcessExitContext| voluntary>"),
            Some((36931, (true, "exited".into())))
        );
        assert_eq!(
            read(
                "<RBSProcessExitContext| specific, status:<RBSProcessExitStatus| \
                 domain:signal(2) code:SIGABRT(6)>>"
            ),
            Some((36931, (false, "signal: 6 (SIGABRT)".into())))
        );
        assert_eq!(
            read(
                "<RBSProcessExitContext| specific, status:<RBSProcessExitStatus| \
                 domain:frontboard(10) code:force-quit(0xfbfbfbfb)>>"
            ),
            Some((36931, (true, "terminated".into())))
        );
        assert_eq!(
            read(
                "<RBSProcessExitContext| specific, status:<RBSProcessExitStatus| \
                 domain:jetsam(6) code:per-process-limit(7)>>"
            ),
            Some((36931, (false, "jetsam(6) per-process-limit(7)".into())))
        );
        assert_eq!(
            exit_record(
                &exit("<RBSProcessExitContext| voluntary>"),
                "dev.viso.other"
            ),
            None
        );
    }

    #[test]
    fn a_phone_is_the_newest_plain_iphone_and_a_tablet_an_ipad_pro() {
        let phones = [
            ("iPhone 17 Pro", "p17p"),
            ("iPhone 17", "p17"),
            ("iPhone 16", "p16"),
            ("iPhone 17e", "p17e"),
        ];
        assert_eq!(
            pick_model(Preset::Phone, &phones),
            Some(("iPhone 17", "p17"))
        );
        let pads = [
            ("iPad Pro 11-inch (M4)", "pro11m4"),
            ("iPad Pro 11-inch (M5)", "pro11m5"),
            ("iPad (A16)", "a16"),
        ];
        assert_eq!(
            pick_model(Preset::Tablet, &pads),
            Some(("iPad Pro 11-inch (M5)", "pro11m5"))
        );
        assert_eq!(
            pick_model(Preset::Phone, &[("iPhone Air", "air")]),
            Some(("iPhone Air", "air"))
        );
        assert_eq!(pick_model(Preset::Phone, &[]), None);
    }
}
