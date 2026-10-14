//! The virtual devices `viso run ios|android` deploys to: the Apple Simulator
//! ([`simulator`]) and the Android Emulator ([`emulator`]), each driven
//! through its own toolchain, and the profiles that name them
//! ([`viso_project::DeviceStore`]).

pub(crate) mod emulator;
pub(crate) mod simulator;

use std::process::{Command, Stdio};

use viso_project::{DeviceProfile, DeviceStore, Platform, Selection};

use crate::command::{ENVIRONMENT, USAGE};
use crate::output::Output;

/// No simulator or emulator profile exists.
pub(crate) const DEVICE_MISSING: &str = "DEVICE_MISSING";
/// `--device` or a configured default names no profile.
pub(crate) const DEVICE_UNKNOWN: &str = "DEVICE_UNKNOWN";
/// Several profiles exist and none is the default.
pub(crate) const DEVICE_AMBIGUOUS: &str = "DEVICE_AMBIGUOUS";
/// The simulator or emulator toolchain is missing or failed.
pub(crate) const ENV_DEVICE: &str = "ENV_DEVICE";

/// Runs `command` and returns its stdout, or what it said when it failed.
pub(crate) fn capture(command: &mut Command) -> Result<String, String> {
    let out = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {:?}: {e}", command.get_program()))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        let said = if said.trim().is_empty() {
            String::from_utf8_lossy(&out.stdout)
        } else {
            said
        };
        return Err(format!(
            "{:?} failed ({}): {}",
            command.get_program(),
            out.status,
            said.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs `command` for its effect.
pub(crate) fn quiet(command: &mut Command) -> Result<(), String> {
    capture(command).map(drop)
}

/// A version's numeric parts, for ordering: `26.3.1` is `[26, 3, 1]`.
pub(crate) fn version_key(version: &str) -> Vec<u64> {
    version
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|part| part.parse().ok())
        .collect()
}

/// The profile store, or the diagnostic for a machine without a home.
pub(crate) fn store(out: &mut Output) -> Result<DeviceStore, u8> {
    DeviceStore::home().ok_or_else(|| {
        out.failure(
            ENV_DEVICE,
            "no home directory for the device profiles (set VISO_HOME)",
            &[],
        );
        ENVIRONMENT
    })
}

/// The profile a run of the project at `root` on `platform` uses, or the
/// exit code of the diagnostic that says why there is none. It never
/// prompts.
pub(crate) fn select(
    out: &mut Output,
    store: &DeviceStore,
    platform: Platform,
    requested: Option<&str>,
    root: &std::path::Path,
) -> Result<DeviceProfile, u8> {
    let kind = platform.device_kind();
    let create = format!("viso {platform} {kind} create <id>");
    match store.select(platform, requested, root) {
        Selection::Profile(profile) => Ok(profile),
        Selection::Missing => {
            out.failure(
                DEVICE_MISSING,
                &format!("there is no {platform} {kind} profile"),
                &[format!("create one: `{create}`")],
            );
            Err(ENVIRONMENT)
        }
        Selection::Unknown(id) => {
            out.failure(
                DEVICE_UNKNOWN,
                &format!("no {platform} {kind} profile is named `{id}`"),
                &[
                    format!("list them: `viso {platform} {kind} list`"),
                    format!("or create it: `viso {platform} {kind} create {id}`"),
                ],
            );
            Err(USAGE)
        }
        Selection::Ambiguous(ids) => {
            out.failure(
                DEVICE_AMBIGUOUS,
                &format!("several {platform} {kind} profiles and no default"),
                &[
                    format!("profiles: {}", ids.join(", ")),
                    format!("choose one: `viso run {platform} --device {}`", ids[0]),
                    format!(
                        "or set a default: `[{platform}] device = \"{}\"` in `.viso/local.toml`",
                        ids[0]
                    ),
                ],
            );
            Err(USAGE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_by_their_numbers() {
        assert!(version_key("26.3.1") > version_key("26.3"));
        assert!(version_key("27.0") > version_key("26.10"));
        assert!(version_key("37.0") > version_key("36"));
    }
}
