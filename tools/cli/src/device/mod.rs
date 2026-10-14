//! The virtual devices `viso run ios|android` deploys to: the Apple Simulator
//! ([`simulator`]) and the Android Emulator ([`emulator`]), each driven
//! through its own toolchain, and the profiles that name them
//! ([`viso_project::DeviceStore`]).

pub(crate) mod emulator;
pub(crate) mod simulator;

use std::io::{BufRead, IsTerminal, Write};
use std::process::{Command, Stdio};

use viso_package::android::{Abi, Sdk};
use viso_project::{DeviceProfile, DeviceStore, Platform, Preset, Selection, device::valid_id};

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
/// A profile ID that is not one.
const DEVICE_ID_INVALID: &str = "DEVICE_ID_INVALID";
/// A profile of that ID exists.
const DEVICE_EXISTS: &str = "DEVICE_EXISTS";
/// No installed system image or runtime fits.
const DEVICE_SYSTEM: &str = "DEVICE_SYSTEM";

/// The profile a run creates when there is none and the user agrees.
const STANDARD_PROFILE: &str = "phone";

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

/// Creates the profile `id` of `preset` on `platform` with its virtual
/// device — on the Android system image of `api`, the newest when `None` —
/// and saves it; or reports why it cannot be and returns the exit code.
pub(crate) fn create(
    platform: Platform,
    store: &DeviceStore,
    id: &str,
    preset: Preset,
    api: Option<&str>,
    out: &mut Output,
) -> Result<DeviceProfile, u8> {
    if !valid_id(id) {
        out.failure(
            DEVICE_ID_INVALID,
            &format!("`{id}` is not a profile ID"),
            &["use lowercase letters, digits and `-`, at most 40".into()],
        );
        return Err(USAGE);
    }
    if store.get(platform, id).is_some() {
        out.failure(
            DEVICE_EXISTS,
            &format!("a {platform} profile is already named `{id}`"),
            &[],
        );
        return Err(USAGE);
    }
    let made = match platform {
        Platform::Ios => {
            if api.is_some() {
                out.failure(
                    crate::command::CLI_USAGE,
                    "`--api` is an Android option",
                    &[],
                );
                return Err(USAGE);
            }
            simulator::newest_runtime(preset)
                .and_then(|(runtime, device_type, model)| {
                    let udid =
                        simulator::create(&format!("Viso {id} ({model})"), &device_type, &runtime)?;
                    Ok((runtime, udid))
                })
                .map_err(|why| (ENV_DEVICE, why))
        }
        Platform::Android => create_avd(id, api, preset),
    };
    let (system, device) = match made {
        Ok(made) => made,
        Err((code, why)) => {
            out.failure(code, &why, &[]);
            return Err(ENVIRONMENT);
        }
    };
    let profile = DeviceProfile {
        id: id.to_owned(),
        platform,
        preset,
        system,
        device,
    };
    if let Err(error) = store.save(&profile) {
        out.failure(
            ENV_DEVICE,
            &format!("cannot write the profile: {error}"),
            &[],
        );
        return Err(ENVIRONMENT);
    }
    out.device(platform.as_str(), &profile.id, &profile.device, "created");
    Ok(profile)
}

/// Creates the AVD of the profile `id` on the image of `api` (the newest
/// when `None`): its API level and AVD name.
fn create_avd(
    id: &str,
    api: Option<&str>,
    preset: Preset,
) -> Result<(String, String), (&'static str, String)> {
    let sdk = Sdk::locate().map_err(|why| (ENV_DEVICE, why))?;
    let abi = Abi::host();
    let images = emulator::system_images(&sdk, abi);
    let image = images
        .iter()
        .find(|image| {
            api.is_none_or(|api| image.api == api || image.api.split('.').next() == Some(api))
        })
        .ok_or_else(|| {
            let installed: Vec<&str> = images.iter().map(|i| i.api.as_str()).collect();
            (
                DEVICE_SYSTEM,
                match api {
                    Some(api) => format!(
                        "no {} system image of API {api} is installed (installed: {})",
                        abi.as_str(),
                        if installed.is_empty() {
                            "none".into()
                        } else {
                            installed.join(", ")
                        }
                    ),
                    None => format!("no {} system image is installed", abi.as_str()),
                },
            )
        })?;
    let name = emulator::avd_name(id);
    emulator::create(&name, image, abi, preset).map_err(|why| (ENV_DEVICE, why))?;
    Ok((image.api.clone(), name))
}

/// Whether the user can be asked: human output, a terminal on stdin and
/// stderr, and no CI.
fn can_ask(out: &Output) -> bool {
    out.human()
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
        && std::env::var_os("CI").is_none()
}

/// Asks which of `profiles` to run on, until an answer names one: its
/// number or its ID. `None` when the input ends.
fn choose<'a>(
    input: &mut impl BufRead,
    prompt: &mut impl Write,
    platform: Platform,
    profiles: &'a [DeviceProfile],
) -> Option<&'a DeviceProfile> {
    let kind = platform.device_kind();
    let _ = writeln!(prompt, "Several {platform} {kind} profiles and no default:");
    for (n, profile) in profiles.iter().enumerate() {
        let _ = writeln!(
            prompt,
            "  {}. {} ({}, {})",
            n + 1,
            profile.id,
            profile.preset.as_str(),
            profile.system
        );
    }
    loop {
        let _ = write!(prompt, "Run on [1-{}]: ", profiles.len());
        let _ = prompt.flush();
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let answer = line.trim();
        let chosen = answer
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|n| profiles.get(n))
            .or_else(|| profiles.iter().find(|p| p.id == answer));
        if chosen.is_some() {
            return chosen;
        }
    }
}

/// Asks whether to create the standard profile; an empty answer agrees.
fn agree(input: &mut impl BufRead, prompt: &mut impl Write, question: &str) -> bool {
    let _ = write!(prompt, "{question} [Y/n]: ");
    let _ = prompt.flush();
    let mut line = String::new();
    match input.read_line(&mut line) {
        Ok(0) | Err(_) => false,
        Ok(_) => matches!(line.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes"),
    }
}

/// The profile a run of the project at `root` on `platform` uses, or the
/// exit code of the diagnostic that says why there is none. Where the choice
/// is the user's and they can be asked — at a terminal, without `--json` or
/// CI — it asks which profile to run on, or whether to create the standard
/// one when there is none; otherwise it never waits on stdin.
pub(crate) fn select(
    out: &mut Output,
    store: &DeviceStore,
    platform: Platform,
    requested: Option<&str>,
    root: &std::path::Path,
) -> Result<DeviceProfile, u8> {
    let kind = platform.device_kind();
    let create_hint = format!("viso {platform} {kind} create <id>");
    match store.select(platform, requested, root) {
        Selection::Profile(profile) => Ok(profile),
        Selection::Missing if can_ask(out) => {
            let question = format!(
                "There is no {platform} {kind} profile. Create `{STANDARD_PROFILE}`, a phone on \
                 the newest installed system?"
            );
            if agree(
                &mut std::io::stdin().lock(),
                &mut std::io::stderr(),
                &question,
            ) {
                create(platform, store, STANDARD_PROFILE, Preset::Phone, None, out)
            } else {
                missing(out, platform, &create_hint)
            }
        }
        Selection::Missing => missing(out, platform, &create_hint),
        Selection::Ambiguous(ids) if can_ask(out) => {
            let profiles: Vec<DeviceProfile> = ids
                .iter()
                .filter_map(|id| store.get(platform, id))
                .collect();
            match choose(
                &mut std::io::stdin().lock(),
                &mut std::io::stderr(),
                platform,
                &profiles,
            ) {
                Some(profile) => {
                    eprintln!(
                        "[viso] set `[{platform}] device = \"{}\"` in `.viso/local.toml` to \
                         run on it without asking",
                        profile.id
                    );
                    Ok(profile.clone())
                }
                None => ambiguous(out, platform, &ids),
            }
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
        Selection::Ambiguous(ids) => ambiguous(out, platform, &ids),
    }
}

/// `DEVICE_MISSING`, with the command that creates a profile.
fn missing(out: &mut Output, platform: Platform, create: &str) -> Result<DeviceProfile, u8> {
    let kind = platform.device_kind();
    out.failure(
        DEVICE_MISSING,
        &format!("there is no {platform} {kind} profile"),
        &[format!("create one: `{create}`")],
    );
    Err(ENVIRONMENT)
}

/// `DEVICE_AMBIGUOUS`, with the commands that choose among `ids`.
fn ambiguous(out: &mut Output, platform: Platform, ids: &[String]) -> Result<DeviceProfile, u8> {
    let kind = platform.device_kind();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str) -> DeviceProfile {
        DeviceProfile {
            id: id.into(),
            platform: Platform::Ios,
            preset: Preset::Phone,
            system: "26.0".into(),
            device: format!("udid-{id}"),
        }
    }

    #[test]
    fn a_choice_is_a_number_or_an_id_asked_again_until_it_names_one() {
        let profiles = [profile("mini"), profile("max")];
        let mut asked = Vec::new();
        let chosen = choose(
            &mut &b"0\nnope\n2\n"[..],
            &mut asked,
            Platform::Ios,
            &profiles,
        );
        assert_eq!(chosen.map(|p| p.id.as_str()), Some("max"));
        let asked = String::from_utf8(asked).unwrap();
        assert!(asked.contains("  1. mini (phone, 26.0)"), "{asked}");
        assert_eq!(asked.matches("Run on [1-2]: ").count(), 3);

        let by_id = choose(
            &mut &b"mini\n"[..],
            &mut Vec::new(),
            Platform::Ios,
            &profiles,
        );
        assert_eq!(by_id.map(|p| p.id.as_str()), Some("mini"));
        assert!(choose(&mut &b"7\n"[..], &mut Vec::new(), Platform::Ios, &profiles).is_none());
    }

    #[test]
    fn creating_the_standard_profile_is_agreed_by_default_and_not_on_eof() {
        for (answer, agreed) in [
            (&b"\n"[..], true),
            (b"Y\n", true),
            (b"yes\n", true),
            (b"n\n", false),
            (b"later\n", false),
            (b"", false),
        ] {
            assert_eq!(
                agree(&mut &answer[..], &mut Vec::new(), "Create?"),
                agreed,
                "{answer:?}"
            );
        }
    }

    #[test]
    fn versions_order_by_their_numbers() {
        assert!(version_key("26.3.1") > version_key("26.3"));
        assert!(version_key("27.0") > version_key("26.10"));
        assert!(version_key("37.0") > version_key("36"));
    }
}
