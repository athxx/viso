//! `viso android list|use|doctor|adb`: the Android development environment.
//!
//! `list` shows the API levels Viso has a tested component set for, what
//! each installs and which this machine uses by default; `use` installs one
//! (after its licenses are accepted) and makes it the default, downloading
//! nothing that is installed already; `doctor` is `viso doctor android`;
//! `adb` runs the SDK's `adb`, on a profile's running emulator when one is
//! named, with its arguments as they are.

use std::io::Write as _;
use std::process::Command;
use std::time::{Duration, Instant};

use viso_ende::JsonWriter;
use viso_package::android::{Abi, SdkOrigin};
use viso_project::{DeviceStore, Platform};

use crate::args::{
    AdbArgs, AndroidListArgs, AndroidUseArgs, BuildTarget, DoctorArgs, PlatformDoctorArgs,
};
use crate::context::Context;
use crate::device::emulator;
use crate::error::{
    ANDROID_ADB_FAILED, ANDROID_API_UNSUPPORTED, DEVICE_UNKNOWN, ENV_ANDROID_LICENSE,
    ENV_ANDROID_SDK, ENV_DEVICE, ENV_DOWNLOAD, ENV_DOWNLOAD_INTEGRITY, ENV_INSTALL, ENV_OFFLINE,
    INTERRUPTED, SUCCESS,
};
use crate::output::{Output, human::size};
use crate::toolchain::android::components::{self, APIS, Api, Component, Host, License};
use crate::toolchain::android::{self as toolchain, Home, InstallError, installed};
use crate::toolchain::download::{self, Curl, Failure};

/// Runs `adb` with no input, each line it writes a `log` event from the
/// device — its errors at `warn` — until it exits; a failure carries its
/// status. The first interrupt ends it.
fn relay_adb(mut command: Command, out: &mut Output) -> u8 {
    use std::io::{BufRead as _, BufReader};
    use std::process::Stdio;
    use std::sync::mpsc;

    let mut child = match command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return out.failure(ENV_ANDROID_SDK, &format!("cannot run `adb`: {error}"), &[]);
        }
    };
    crate::interrupt::catch();
    let (tx, rx) = mpsc::channel::<(&'static str, String)>();
    let streams: [(Option<Box<dyn std::io::Read + Send>>, &'static str); 2] = [
        (
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            "info",
        ),
        (
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            "warn",
        ),
    ];
    let readers: Vec<_> = streams
        .into_iter()
        .filter_map(|(stream, level)| Some((stream?, level)))
        .map(|(stream, level)| {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    if tx.send((level, line)).is_err() {
                        break;
                    }
                }
            })
        })
        .collect();
    drop(tx);
    let mut interrupted = false;
    loop {
        if !interrupted && crate::interrupt::count() > 0 {
            interrupted = true;
            let _ = child.kill();
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok((level, line)) => out.log(level, "device", &line),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    for reader in readers {
        let _ = reader.join();
    }
    let status = child.wait();
    if interrupted {
        return INTERRUPTED;
    }
    match status {
        Ok(status) if status.success() => SUCCESS,
        Ok(status) => out.failure(
            ANDROID_ADB_FAILED,
            &match status.code() {
                Some(code) => format!("`adb` exited with status {code}"),
                None => "`adb` was ended by a signal".to_owned(),
            },
            &[],
        ),
        Err(error) => out.failure(
            ENV_ANDROID_SDK,
            &format!("cannot wait for `adb`: {error}"),
            &[],
        ),
    }
}

/// How a level stands on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Installed,
    Partial,
    Available,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Installed => "installed",
            Status::Partial => "partial",
            Status::Available => "available",
        }
    }
}

fn status(home: &Home, api: &Api, abi: Abi) -> Status {
    let components = api.components(abi);
    let have = components
        .iter()
        .filter(|c| installed(&home.sdk(), c))
        .count();
    match have {
        n if n == components.len() => Status::Installed,
        0 => Status::Available,
        _ => Status::Partial,
    }
}

fn no_home(out: &mut Output) -> u8 {
    out.failure(
        ENV_DEVICE,
        "no home directory for `~/.viso` (set `VISO_HOME`)",
        &[],
    )
}

pub fn list(args: &AndroidListArgs, out: &mut Output) -> u8 {
    let Some(home) = Home::current() else {
        return no_home(out);
    };
    let abi = Abi::host();
    let host = Host::current();
    let default = home.default_api();
    let sdk = toolchain::sdk().ok();
    let rows: Vec<(&Api, Status)> = APIS
        .iter()
        .map(|api| (api, status(&home, api, abi)))
        .filter(|(_, status)| !args.offline || *status == Status::Installed)
        .collect();
    out.result(
        |w| {
            w.begin_object();
            w.name("sdk");
            match &sdk {
                Some(sdk) => {
                    w.begin_object();
                    w.name("root");
                    w.string(&sdk.root.display().to_string());
                    w.name("origin");
                    w.string(origin_name(sdk.origin));
                    w.end_object();
                }
                None => w.null(),
            }
            w.name("default");
            match default {
                Some(level) => w.uint(u64::from(level)),
                None => w.null(),
            }
            w.name("abi");
            w.string(abi.as_str());
            w.name("apis");
            w.begin_array();
            for (api, status) in &rows {
                w.begin_object();
                w.name("api");
                w.uint(u64::from(api.level));
                w.name("android");
                w.string(api.android);
                w.name("status");
                w.string(status.as_str());
                w.name("system_image");
                w.string(&api.image_name(abi));
                w.name("default");
                w.bool(default == Some(api.level));
                w.name("components");
                w.begin_array();
                for component in api.components(abi) {
                    write_component(w, &home, component, host);
                }
                w.end_array();
                w.end_object();
            }
            w.end_array();
            w.end_object();
        },
        || {
            if rows.is_empty() {
                return "no Android API level is installed; `viso android list` shows those that can be\n"
                    .to_owned();
            }
            let mut text = String::from("API   ANDROID   STATUS      SYSTEM IMAGE              DEFAULT\n");
            for (api, status) in &rows {
                let mark = if default == Some(api.level) { "*" } else { "" };
                text.push_str(&format!(
                    "{:<5} {:<9} {:<11} {:<25} {mark}\n",
                    api.level,
                    api.android,
                    status.as_str(),
                    api.image_name(abi),
                ));
            }
            if let Some(sdk) = &sdk {
                text.push_str(&format!(
                    "\nSDK {} ({})\n",
                    sdk.root.display(),
                    sdk.origin.describe()
                ));
            }
            text
        },
    );
    SUCCESS
}

fn origin_name(origin: SdkOrigin) -> &'static str {
    match origin {
        SdkOrigin::Variable(name) => name,
        SdkOrigin::Viso => "viso",
        SdkOrigin::Default => "default",
    }
}

fn write_component(w: &mut JsonWriter, home: &Home, component: &Component, host: Option<Host>) {
    w.begin_object();
    w.name("path");
    w.string(component.path);
    w.name("revision");
    w.string(component.revision);
    w.name("title");
    w.string(component.title);
    w.name("installed");
    w.bool(installed(&home.sdk(), component));
    w.name("download_bytes");
    match host.and_then(|host| component.archive(host)) {
        Some(archive) => w.uint(archive.size),
        None => w.null(),
    }
    w.end_object();
}

pub fn use_level(args: &AndroidUseArgs, out: &mut Output) -> u8 {
    let Some(api) = args.api.parse().ok().and_then(components::api) else {
        let levels: Vec<String> = APIS.iter().map(|a| a.level.to_string()).collect();
        return out.failure_next(
            ANDROID_API_UNSUPPORTED,
            &format!(
                "Viso has no tested component set for Android API `{}`; it has {}",
                args.api,
                levels.join(", ")
            ),
            &[],
            &["viso android list".to_owned()],
        );
    };
    let Some(home) = Home::current() else {
        return no_home(out);
    };
    let Some(host) = Host::current() else {
        return out.failure(
            ENV_ANDROID_SDK,
            &format!(
                "the Android SDK publishes no tools for this host ({} on {})",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
            &[],
        );
    };
    let abi = Abi::host();
    let missing: Vec<&Component> = api
        .components(abi)
        .into_iter()
        .filter(|c| !installed(&home.sdk(), c))
        .collect();
    let mut fresh = Vec::new();
    if !missing.is_empty() {
        if args.offline {
            let paths: Vec<&str> = missing.iter().map(|c| c.path).collect();
            return out.failure(
                ENV_OFFLINE,
                &format!(
                    "API {} needs {} downloaded: {}",
                    api.level,
                    if missing.len() == 1 {
                        "one component"
                    } else {
                        "components"
                    },
                    paths.join(", ")
                ),
                &[],
            );
        }
        let mut licenses: Vec<License> = Vec::new();
        for component in &missing {
            if !home.accepted(&component.license) && !licenses.contains(&component.license) {
                licenses.push(component.license);
            }
        }
        if let Err(code) = consent(out, &home, &licenses, args.accept_licenses) {
            return code;
        }
        let total: u64 = missing
            .iter()
            .filter_map(|c| c.archive(host))
            .map(|a| a.size)
            .sum();
        out.step(
            "download",
            &format!(
                "installing Android API {} (Android {}): {} to download",
                api.level,
                api.android,
                size(total)
            ),
        );
        for component in &missing {
            if let Err(code) = install_one(out, &home, component, host) {
                return code;
            }
            fresh.push(component.path);
        }
    }
    if let Err(error) = home.set_default_api(api.level) {
        return out.failure(
            ENV_INSTALL,
            &format!("cannot record the default level: {error}"),
            &[],
        );
    }
    let sdk = toolchain::sdk().ok();
    let active = sdk
        .as_ref()
        .is_some_and(|sdk| sdk.origin == SdkOrigin::Viso);
    let shadowed = sdk.as_ref().and_then(|sdk| match sdk.origin {
        SdkOrigin::Variable(name) => Some((name, sdk.root.display().to_string())),
        _ => None,
    });
    out.result(
        |w| {
            w.begin_object();
            w.name("api");
            w.uint(u64::from(api.level));
            w.name("android");
            w.string(api.android);
            w.name("installed");
            w.begin_array();
            for path in &fresh {
                w.string(path);
            }
            w.end_array();
            w.name("default");
            w.bool(true);
            w.name("sdk");
            w.string(&home.sdk().display().to_string());
            w.name("active");
            w.bool(active);
            w.end_object();
        },
        || {
            let mut text = if fresh.is_empty() {
                format!(
                    "Android API {} (Android {}) is installed; it is the default now\n",
                    api.level, api.android
                )
            } else {
                format!(
                    "installed Android API {} (Android {}): {}; it is the default now\n",
                    api.level,
                    api.android,
                    fresh.join(", ")
                )
            };
            if let Some((name, root)) = &shadowed {
                text.push_str(&format!(
                    "note: `{name}` is set, so builds use the SDK at {root}; unset it to use this one\n"
                ));
            }
            text
        },
    );
    SUCCESS
}

/// Has each of `licenses` accepted: by `--accept-licenses`, or by the user
/// at a terminal after reading it. Nothing is downloaded without.
fn consent(out: &mut Output, home: &Home, licenses: &[License], accept: bool) -> Result<(), u8> {
    for license in licenses {
        if !accept && !crate::device::can_ask(out) {
            return Err(out.failure_next(
                ENV_ANDROID_LICENSE,
                &format!(
                    "the license `{}` is not accepted on this machine",
                    license.id
                ),
                &[
                    "run at a terminal to read and accept it, or pass `--accept-licenses`"
                        .to_owned(),
                ],
                &[],
            ));
        }
        let text = download::text(license.manifest, &Curl)
            .ok()
            .and_then(|xml| toolchain::license_text(&xml, license));
        let Some(text) = text else {
            return Err(out.failure(
                ENV_DOWNLOAD,
                &format!(
                    "cannot read the license `{}` from {}",
                    license.id, license.manifest
                ),
                &[],
            ));
        };
        if !accept {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "\n{}\n", text.trim());
            let _ = write!(stderr, "Accept the license `{}`? [y/N]: ", license.id);
            let _ = stderr.flush();
            drop(stderr);
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                return Err(out.failure(
                    ENV_ANDROID_LICENSE,
                    &format!(
                        "the license `{}` was declined; nothing was installed",
                        license.id
                    ),
                    &[],
                ));
            }
        }
        if let Err(error) = home.accept(license, &text) {
            return Err(out.failure(
                ENV_INSTALL,
                &format!("cannot record the license `{}`: {error}", license.id),
                &[],
            ));
        }
    }
    Ok(())
}

/// Downloads and installs `component`, its progress on a live line at a
/// terminal and as `progress` events at most once a second.
fn install_one(out: &mut Output, home: &Home, component: &Component, host: Host) -> Result<(), u8> {
    let Some(archive) = component.archive(host) else {
        return Err(out.failure(
            ENV_ANDROID_SDK,
            &format!("`{}` has no archive for this host", component.path),
            &[],
        ));
    };
    let name = format!("{} {}", component.title, component.revision);
    let live = out.live();
    if !live {
        out.step(
            "download",
            &format!("downloading {name} ({})", size(archive.size)),
        );
    }
    let mut last = Instant::now();
    let mut progress = |got: u64| {
        if last.elapsed() < Duration::from_millis(if live { 200 } else { 1000 }) {
            return;
        }
        last = Instant::now();
        let percent = got.saturating_mul(100) / archive.size.max(1);
        let line = format!(
            "{name}: {percent}% ({} of {})",
            size(got),
            size(archive.size)
        );
        if live {
            eprint!("\r[viso] downloading {line}\x1b[K");
        } else {
            out.progress("download", &format!("downloading {line}"));
        }
    };
    let result = toolchain::install(home, component, host, &Curl, &mut progress);
    if live {
        eprint!("\r\x1b[K");
    }
    match result {
        Ok(()) => {
            out.step("install", &format!("installed {name} ({})", component.path));
            Ok(())
        }
        Err(InstallError::Download(Failure::Interrupted)) => Err(INTERRUPTED),
        Err(InstallError::Download(Failure::Transfer(why))) => Err(out.failure(
            ENV_DOWNLOAD,
            &format!("cannot download {name}: {why}"),
            &["what arrived is kept; running the command again resumes it".to_owned()],
        )),
        Err(InstallError::Download(Failure::Integrity(why))) => Err(out.failure(
            ENV_DOWNLOAD_INTEGRITY,
            &format!("the download of {name} is not the pinned archive: {why}"),
            &[],
        )),
        Err(InstallError::Unpack(why)) => {
            Err(out.failure(ENV_INSTALL, &format!("cannot install {name}: {why}"), &[]))
        }
        Err(InstallError::Host) => Err(out.failure(
            ENV_ANDROID_SDK,
            &format!("`{}` has no archive for this host", component.path),
            &[],
        )),
    }
}

pub fn doctor(ctx: &Context<'_>, args: &PlatformDoctorArgs, out: &mut Output) -> u8 {
    super::doctor::run(
        ctx,
        &DoctorArgs {
            target: Some(BuildTarget::Android),
            fix: args.fix,
        },
        out,
    )
}

pub fn adb(ctx: &Context<'_>, args: &AdbArgs, out: &mut Output) -> u8 {
    let sdk = match toolchain::sdk() {
        Ok(sdk) => sdk,
        Err(why) => return toolchain::no_sdk(out, &why),
    };
    if !sdk.adb.is_file() {
        return toolchain::no_sdk(
            out,
            &format!("the SDK at {} has no `adb`", sdk.root.display()),
        );
    }
    let mut command = Command::new(&sdk.adb);
    if let Some(id) = &args.device {
        let Some(profile) = DeviceStore::home().and_then(|store| store.get(Platform::Android, id))
        else {
            return out.failure_next(
                DEVICE_UNKNOWN,
                &format!("no Android emulator profile `{id}`"),
                &[],
                &["viso android emulator list".to_owned()],
            );
        };
        let Some(serial) = emulator::serial(&sdk, &profile.device) else {
            return out.failure_next(
                ENV_DEVICE,
                &format!("the emulator of `{id}` is not running"),
                &[],
                &[format!("viso android emulator start {id}")],
            );
        };
        command.args(["-s", &serial]);
    }
    command.args(&args.args);
    out.verbose(|| format!("running {}", sdk.adb.display()));
    if ctx.global.json {
        return relay_adb(command, out);
    }
    let error = super::lsp::replace(command);
    out.failure(
        ENV_ANDROID_SDK,
        &format!("cannot run `{}`: {error}", sdk.adb.display()),
        &[],
    )
}
