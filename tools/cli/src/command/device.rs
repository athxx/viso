//! `viso ios simulator …` and `viso android emulator …`:
//! the profiles `viso run ios|android --device` takes, each with the virtual
//! device it names.

use viso_package::android::Sdk;
use viso_project::{DeviceProfile, DeviceStore, Platform, Preset};

use crate::args::{CreateArgs, DeviceCommand, PresetArg};
use crate::device::{self, emulator, simulator};
use crate::error::{DEVICE_UNKNOWN, ENV_DEVICE, SUCCESS};
use crate::output::Output;

pub fn run(platform: Platform, command: &DeviceCommand, out: &mut Output) -> u8 {
    if platform == Platform::Ios && !cfg!(target_os = "macos") {
        return out.failure(ENV_DEVICE, "the iOS Simulator runs on macOS only", &[]);
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
                    return out.failure(ENV_DEVICE, &why, &[]);
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
            )
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

/// Creates the profile `create` names, with its virtual device.
fn create(platform: Platform, store: &DeviceStore, args: &CreateArgs, out: &mut Output) -> u8 {
    let preset = match args.preset {
        PresetArg::Phone => Preset::Phone,
        PresetArg::Tablet => Preset::Tablet,
    };
    match device::create(platform, store, &args.id, preset, args.api.as_deref(), out) {
        Ok(_) => SUCCESS,
        Err(code) => code,
    }
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
        Err(why) => out.failure(ENV_DEVICE, &why, &[]),
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
        Err(why) => out.failure(ENV_DEVICE, &why, &[]),
    }
}
