//! The Apple Simulator through `xcrun simctl`: the simulators Viso creates
//! for its profiles, their boot state, and the app installed, launched and
//! stopped on one (`Viso_CLI.md`, `Viso_Hot_Reload.md`).
//!
//! A simulator shares the host's network, so the app reaches the dev channel
//! on the host's loopback address as a desktop app does; `simctl launch`
//! hands it the dev variables as its environment (each `SIMCTL_CHILD_` name
//! with the prefix taken off). Nothing is signed beyond ad-hoc and nothing is
//! provisioned.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Child, Command, Stdio};

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

#[cfg(test)]
mod tests {
    use super::*;

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
