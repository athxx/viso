//! `viso ios simulator …` and `viso android emulator …` (`Viso_CLI.md`):
//! the profiles `viso run ios|android --device` takes, each with the virtual
//! device it names.

use viso_package::android::{Abi, Sdk};
use viso_project::{DeviceProfile, DeviceStore, Platform, Preset, device::valid_id};

use super::{ENVIRONMENT, RUNTIME, SUCCESS, USAGE};
use crate::args::{CreateArgs, DeviceCommand, PresetArg};
use crate::device::{self, DEVICE_UNKNOWN, ENV_DEVICE, emulator, simulator};
use crate::output::Output;

/// A profile ID that is not one.
const DEVICE_ID_INVALID: &str = "DEVICE_ID_INVALID";
/// A profile of that ID exists.
const DEVICE_EXISTS: &str = "DEVICE_EXISTS";
/// No installed system image or runtime fits.
const DEVICE_SYSTEM: &str = "DEVICE_SYSTEM";

pub fn run(platform: Platform, command: &DeviceCommand, out: &mut Output) -> u8 {
    if platform == Platform::Ios && !cfg!(target_os = "macos") {
        out.failure(ENV_DEVICE, "the iOS Simulator runs on macOS only", &[]);
        return ENVIRONMENT;
    }
    let store = match device::store(out) {
        Ok(store) => store,
        Err(code) => return code,
    };
    match command {
        DeviceCommand::List => list(platform, &store, out),
        DeviceCommand::Create(args) => create(platform, &store, args, out),
        DeviceCommand::Start(args) => {
            with_profile(platform, &store, &args.id, out, |profile, out| {
                start(profile, !args.no_window, out)
            })
        }
        DeviceCommand::Stop(args) => with_profile(platform, &store, &args.id, out, stop),
        DeviceCommand::Delete(args) => {
            with_profile(platform, &store, &args.id, out, |profile, out| {
                let code = stop(profile, out);
                if code != SUCCESS {
                    return code;
                }
                let deleted = match platform {
                    Platform::Ios => simulator::delete(&profile.device),
                    Platform::Android => emulator::delete(&profile.device),
                };
                if let Err(why) = deleted.and_then(|()| {
                    store
                        .remove(platform, &profile.id)
                        .map(drop)
                        .map_err(|e| e.to_string())
                }) {
                    out.failure(ENV_DEVICE, &why, &[]);
                    return ENVIRONMENT;
                }
                out.device(platform.as_str(), &profile.id, &profile.device, "deleted");
                SUCCESS
            })
        }
    }
}

/// Runs `action` on the profile `id`, or reports that there is none.
fn with_profile(
    platform: Platform,
    store: &DeviceStore,
    id: &str,
    out: &mut Output,
    action: impl FnOnce(&DeviceProfile, &mut Output) -> u8,
) -> u8 {
    match store.get(platform, id) {
        Some(profile) => action(&profile, out),
        None => {
            let kind = platform.device_kind();
            out.failure(
                DEVICE_UNKNOWN,
                &format!("no {platform} {kind} profile is named `{id}`"),
                &[format!("list them: `viso {platform} {kind} list`")],
            );
            USAGE
        }
    }
}

/// Where the device of `profile` is: `booted`, `stopped` or `missing`.
fn state(profile: &DeviceProfile, sdk: Option<&Sdk>) -> &'static str {
    match profile.platform {
        Platform::Ios => match simulator::state(&profile.device) {
            Ok(simulator::State::Booted) => "booted",
            Ok(simulator::State::Changing) => "booting",
            Ok(simulator::State::Shutdown) => "stopped",
            Ok(simulator::State::Gone) | Err(_) => "missing",
        },
        Platform::Android => match sdk.and_then(|sdk| emulator::serial(sdk, &profile.device)) {
            Some(_) => "booted",
            None => "stopped",
        },
    }
}

fn list(platform: Platform, store: &DeviceStore, out: &mut Output) -> u8 {
    let sdk = Sdk::locate().ok();
    let rows: Vec<(DeviceProfile, &'static str)> = store
        .list(platform)
        .into_iter()
        .map(|profile| {
            let state = state(&profile, sdk.as_ref());
            (profile, state)
        })
        .collect();
    out.result(
        |w| {
            w.begin_object();
            w.name("profiles");
            w.begin_array();
            for (profile, state) in &rows {
                w.begin_object();
                w.name("id");
                w.string(&profile.id);
                w.name("preset");
                w.string(profile.preset.as_str());
                w.name("system");
                w.string(&profile.system);
                w.name("device");
                w.string(&profile.device);
                w.name("state");
                w.string(state);
                w.end_object();
            }
            w.end_array();
            w.end_object();
        },
        || {
            let mut text = format!(
                "{:<16}{:<10}{:<12}{}\n",
                "NAME", "PROFILE", "STATUS", "SYSTEM"
            );
            for (profile, state) in &rows {
                text.push_str(&format!(
                    "{:<16}{:<10}{:<12}{}\n",
                    profile.id,
                    profile.preset.as_str(),
                    state,
                    profile.system
                ));
            }
            text
        },
    );
    SUCCESS
}

fn create(platform: Platform, store: &DeviceStore, args: &CreateArgs, out: &mut Output) -> u8 {
    if !valid_id(&args.id) {
        out.failure(
            DEVICE_ID_INVALID,
            &format!("`{}` is not a profile ID", args.id),
            &["use lowercase letters, digits and `-`, at most 40".into()],
        );
        return USAGE;
    }
    if store.get(platform, &args.id).is_some() {
        out.failure(
            DEVICE_EXISTS,
            &format!("a {platform} profile is already named `{}`", args.id),
            &[],
        );
        return USAGE;
    }
    let preset = match args.preset {
        PresetArg::Phone => Preset::Phone,
        PresetArg::Tablet => Preset::Tablet,
    };
    let made = match platform {
        Platform::Ios => {
            if args.api.is_some() {
                out.failure(super::CLI_USAGE, "`--api` is an Android option", &[]);
                return USAGE;
            }
            simulator::newest_runtime(preset)
                .and_then(|(runtime, device_type, model)| {
                    let udid = simulator::create(
                        &format!("Viso {} ({model})", args.id),
                        &device_type,
                        &runtime,
                    )?;
                    Ok((runtime, udid))
                })
                .map_err(|why| (ENV_DEVICE, why))
        }
        Platform::Android => create_avd(&args.id, args.api.as_deref(), preset),
    };
    let (system, device) = match made {
        Ok(made) => made,
        Err((code, why)) => {
            out.failure(code, &why, &[]);
            return ENVIRONMENT;
        }
    };
    let profile = DeviceProfile {
        id: args.id.clone(),
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
        return ENVIRONMENT;
    }
    out.device(platform.as_str(), &profile.id, &profile.device, "created");
    SUCCESS
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

fn start(profile: &DeviceProfile, window: bool, out: &mut Output) -> u8 {
    let platform = profile.platform.as_str();
    out.device(platform, &profile.id, &profile.device, "booting");
    let booted = match profile.platform {
        Platform::Ios => simulator::boot(profile),
        Platform::Android => {
            Sdk::locate().and_then(|sdk| emulator::boot(&sdk, profile, window).map(drop))
        }
    };
    match booted {
        Ok(()) => {
            out.device(platform, &profile.id, &profile.device, "ready");
            SUCCESS
        }
        Err(why) => {
            out.failure(ENV_DEVICE, &why, &[]);
            RUNTIME
        }
    }
}

fn stop(profile: &DeviceProfile, out: &mut Output) -> u8 {
    let stopped = match profile.platform {
        Platform::Ios => simulator::shutdown(&profile.device),
        Platform::Android => {
            Sdk::locate().and_then(|sdk| match emulator::serial(&sdk, &profile.device) {
                Some(serial) => emulator::kill(&sdk, &serial),
                None => Ok(()),
            })
        }
    };
    match stopped {
        Ok(()) => {
            out.device(
                profile.platform.as_str(),
                &profile.id,
                &profile.device,
                "stopped",
            );
            SUCCESS
        }
        Err(why) => {
            out.failure(ENV_DEVICE, &why, &[]);
            ENVIRONMENT
        }
    }
}
