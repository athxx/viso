//! The doctor's checks, and the tools they ask: each probe reads what is
//! installed and says what it found, running no tool longer than
//! [`TOOL_TIMEOUT`].

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use viso_ende::JsonValue;
use viso_package::android::{Abi, SdkOrigin};
use viso_package::{apple, web};
use viso_project::{ArtifactKind, DeviceStore, Platform, Target};

use super::{Fix, Found, Machine, Probe, shown};
use crate::build::cargo::{self, ProcessTree};
use crate::device::version_key;
use crate::toolchain::{android, ios};

/// How long one tool may run.
const TOOL_TIMEOUT: Duration = Duration::from_secs(15);

/// The oldest Rust the framework builds with.
const RUST_VERSION: &str = env!("CARGO_PKG_RUST_VERSION");

/// This CLI's version, which the framework a project links should match.
const VERSION: &str = env!("CARGO_PKG_VERSION");

const RUST: Probe = Probe {
    name: "rust",
    title: "Rust toolchain",
    run: rust,
};
const CARGO: Probe = Probe {
    name: "cargo",
    title: "Cargo",
    run: cargo_tool,
};
const RUSTUP: Probe = Probe {
    name: "rustup",
    title: "rustup",
    run: rustup,
};
const PROJECT: Probe = Probe {
    name: "project",
    title: "Viso project",
    run: project,
};
const FRAMEWORK: Probe = Probe {
    name: "framework",
    title: "Viso framework",
    run: framework,
};
const LINKER: Probe = Probe {
    name: "linker",
    title: "Host compiler and linker",
    run: linker,
};
const GPU: Probe = Probe {
    name: "gpu",
    title: "Host GPU backend",
    run: gpu,
};
const WASM: Probe = Probe {
    name: "wasm32",
    title: "WebAssembly target",
    run: wasm,
};
const BINDGEN: Probe = Probe {
    name: "wasm-bindgen",
    title: "wasm-bindgen",
    run: bindgen,
};
const WASM_OPT: Probe = Probe {
    name: "wasm-opt",
    title: "wasm-opt",
    run: wasm_opt,
};
const FILESYSTEM: Probe = Probe {
    name: "filesystem",
    title: "Build output",
    run: filesystem,
};
const GIT: Probe = Probe {
    name: "git",
    title: "Git",
    run: git,
};
const LSP: Probe = Probe {
    name: "lsp",
    title: "Language server",
    run: lsp,
};
const ANDROID: Probe = Probe {
    name: "android-sdk",
    title: "Android SDK",
    run: android_sdk,
};
const ANDROID_SET: Probe = Probe {
    name: "android-components",
    title: "Viso Android component set",
    run: android_set,
};
const ANDROID_ACCEL: Probe = Probe {
    name: "android-accel",
    title: "Emulator hardware acceleration",
    run: android_accel,
};
const JAVA: Probe = Probe {
    name: "java",
    title: "Java compiler",
    run: java,
};
const ANDROID_TOOLS: Probe = Probe {
    name: "android-tools",
    title: "Android emulator and adb",
    run: android_tools,
};
const ANDROID_RUST: Probe = Probe {
    name: "android-rust",
    title: "Rust Android target",
    run: android_rust,
};
const ANDROID_PROFILES: Probe = Probe {
    name: "android-profiles",
    title: "Android emulator profiles",
    run: android_profiles,
};
const IOS: Probe = Probe {
    name: "ios-runtime",
    title: "iOS Simulator runtime",
    run: ios_runtime,
};
const XCODE: Probe = Probe {
    name: "xcode",
    title: "Xcode",
    run: xcode,
};
const XCODE_LICENSE: Probe = Probe {
    name: "xcode-license",
    title: "Xcode license",
    run: xcode_license,
};
const IOS_DISK: Probe = Probe {
    name: "ios-disk",
    title: "Space for a Simulator runtime",
    run: ios_disk,
};
const IOS_RUST: Probe = Probe {
    name: "ios-rust",
    title: "Rust iOS Simulator target",
    run: ios_rust,
};
const IOS_PROFILES: Probe = Probe {
    name: "ios-profiles",
    title: "iOS Simulator profiles",
    run: ios_profiles,
};
const AVAILABLE: Probe = Probe {
    name: "target",
    title: "Target",
    run: available,
};

/// The checks for `target`: the host's, or the toolchain, the project and
/// the target's own.
pub(super) fn plan(target: Option<Target>) -> Vec<Probe> {
    let core = [RUST, CARGO, PROJECT];
    let own: &[Probe] = match target {
        None => {
            return vec![
                RUST, CARGO, RUSTUP, PROJECT, FRAMEWORK, LINKER, GPU, WASM, BINDGEN, WASM_OPT,
                FILESYSTEM, GIT, LSP, ANDROID, IOS,
            ];
        }
        Some(Target::Host) => &[LINKER, GPU, FILESYSTEM],
        Some(Target::Headless) => &[LINKER, FILESYSTEM],
        Some(Target::Ios) => &[
            XCODE,
            XCODE_LICENSE,
            IOS,
            IOS_DISK,
            IOS_RUST,
            IOS_PROFILES,
            GPU,
        ],
        Some(Target::Android) => &[
            ANDROID,
            ANDROID_SET,
            ANDROID_TOOLS,
            ANDROID_ACCEL,
            JAVA,
            ANDROID_RUST,
            ANDROID_PROFILES,
        ],
        Some(Target::WebGpu) => &[WASM, BINDGEN, WASM_OPT],
        Some(Target::WebDom | Target::WebHybrid) => &[AVAILABLE],
    };
    core.into_iter().chain(own.iter().copied()).collect()
}

/// Whether the doctor asks about `target`, so a gap in its tools is one.
fn asked(machine: &Machine, target: Target) -> bool {
    machine.target == Some(target)
}

fn rust(_: &Machine) -> Option<Found> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    Some(match tool(&rustc, &["-vV"], TOOL_TIMEOUT) {
        Ok(text) => {
            let field = |name: &str| {
                text.lines()
                    .find_map(|line| line.strip_prefix(name))
                    .map(str::trim)
                    .unwrap_or("")
                    .to_owned()
            };
            let (release, host) = (field("release:"), field("host:"));
            if version_key(&release) < version_key(RUST_VERSION) {
                Found::error(
                    crate::error::ENV_RUST_TOOLCHAIN.name,
                    format!(
                        "rustc {release} is older than {RUST_VERSION}, the oldest Viso builds with"
                    ),
                )
                .suggest("rustup update")
            } else {
                Found::ok(format!("rustc {release} ({host})"))
            }
        }
        Err(why) => Found::missing(crate::error::ENV_RUST_TOOLCHAIN.name, why)
            .suggest("curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"),
    })
}

fn cargo_tool(_: &Machine) -> Option<Found> {
    Some(
        match tool(cargo::program().as_os_str(), &["-V"], TOOL_TIMEOUT) {
            Ok(text) => Found::ok(text.trim()),
            Err(why) => Found::missing(crate::error::ENV_CARGO.name, why),
        },
    )
}

fn rustup(_: &Machine) -> Option<Found> {
    Some(match tool("rustup", &["--version"], TOOL_TIMEOUT) {
        Ok(text) => Found::ok(text.lines().next().unwrap_or("").trim()),
        Err(why) => Found::warn(format!(
            "{why}; a Rust target is then installed by hand, and `--fix` cannot add one"
        )),
    })
}

fn project(machine: &Machine) -> Option<Found> {
    let project = match &machine.project {
        Ok(project) => project,
        Err(diagnostics) => {
            let first = diagnostics.first();
            let message = first.map_or_else(|| "no project".to_owned(), |d| d.message.clone());
            let not_found =
                first.is_some_and(|d| d.code == viso_project::ConfigCode::ManifestNotFound);
            return Some(if not_found && !machine.named {
                Found::warn(format!("{message}; the project checks need one"))
                    .suggest("viso new <name>")
            } else {
                Found::error(first.map_or("C0001", |d| d.code.as_str()), message)
            });
        }
    };
    let name = match project.name() {
        Ok(name) => name.to_owned(),
        Err(diagnostic) => {
            return Some(Found::error(diagnostic.code.as_str(), diagnostic.message));
        }
    };
    if let Some(language) = &project.manifest.package.language
        && language.value != viso_dsl::frontend::LANGUAGE_VERSION
    {
        return Some(Found::error(
            "E1001",
            format!(
                "`Viso.toml` pins language `{}`; this Viso implements `{}`",
                language.value,
                viso_dsl::frontend::LANGUAGE_VERSION
            ),
        ));
    }
    let target = machine.target.unwrap_or(Target::Host);
    let resolved = viso_project::resolve(
        project,
        target,
        ArtifactKind::Build,
        &viso_project::Overrides::default(),
        &viso_project::Env::from_process(),
    );
    let at = shown(&project.root);
    Some(match resolved {
        Err(diagnostics) => {
            let first = &diagnostics[0];
            Found::error(first.code.as_str(), first.message.clone())
        }
        Ok(resolved) => {
            let warnings = project.warnings.len() + resolved.warnings.len();
            if !project.root.join("Cargo.toml").is_file() {
                Found::warn(format!(
                    "`{name}` at {at} has no `Cargo.toml`, which `viso build` builds"
                ))
            } else if warnings > 0 {
                Found::warn(format!(
                    "`{name}` at {at}: `Viso.toml` has {warnings} warning(s)"
                ))
                .suggest("viso config check")
            } else {
                Found::ok(format!("`{name}` at {at}"))
            }
        }
    })
}

fn framework(machine: &Machine) -> Option<Found> {
    machine.project.as_ref().ok()?;
    let Some(lock) = machine.lock() else {
        return Some(Found::ok(
            "not locked yet: the first build resolves the framework",
        ));
    };
    let Some(version) = locked(&lock, "viso") else {
        return Some(Found::warn(
            "`Cargo.lock` has no `viso` package: the app does not link the framework",
        ));
    };
    Some(if version == VERSION {
        Found::ok(format!("viso {version}, as this CLI"))
    } else {
        Found::warn(format!(
            "the project links viso {version}, this CLI is {VERSION}: a dev session \
             needs both to speak the same protocol"
        ))
        .suggest(format!("cargo update -p viso --precise {VERSION}"))
    })
}

/// The version `lock` (a `Cargo.lock`) resolves package `name` to.
pub(super) fn locked(lock: &str, name: &str) -> Option<String> {
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line.trim() == format!("name = \"{name}\"") {
            let version = lines.next()?.trim().strip_prefix("version = \"")?;
            return Some(version.trim_end_matches('"').to_owned());
        }
    }
    None
}

fn linker(_: &Machine) -> Option<Found> {
    if cfg!(windows) {
        let vswhere = std::env::var_os("ProgramFiles(x86)")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("Microsoft Visual Studio/Installer/vswhere.exe");
        return Some(
            match tool(
                vswhere.as_os_str(),
                &["-latest", "-property", "displayName"],
                TOOL_TIMEOUT,
            ) {
                Ok(name) if !name.trim().is_empty() => Found::ok(name.trim()),
                _ => Found::missing(
                    crate::error::ENV_TOOL.name,
                    "no Visual Studio Build Tools, whose linker Rust uses",
                ),
            },
        );
    }
    Some(match tool("cc", &["--version"], TOOL_TIMEOUT) {
        Ok(text) => Found::ok(text.lines().next().unwrap_or("").trim()),
        Err(why) => {
            let gap = Found::missing(
                crate::error::ENV_TOOL.name,
                format!("{why}; Rust links with it"),
            );
            if cfg!(target_os = "macos") {
                gap.suggest("xcode-select --install")
            } else {
                gap
            }
        }
    })
}

fn gpu(_: &Machine) -> Option<Found> {
    let none = |why: String| Found::error(crate::error::ENV_GPU.name, why);
    if cfg!(target_os = "macos") {
        let text = match tool(
            "system_profiler",
            &["SPDisplaysDataType", "-json"],
            TOOL_TIMEOUT,
        ) {
            Ok(text) => text,
            Err(why) => return Some(none(why)),
        };
        let json = JsonValue::parse(&text).ok();
        let gpus = json
            .as_ref()
            .and_then(|j| j.get("SPDisplaysDataType"))
            .and_then(JsonValue::as_array)
            .unwrap_or_default();
        let metal = gpus.iter().find_map(|gpu| {
            let family = gpu.get("spdisplays_mtlgpufamilysupport")?.as_str()?;
            let name = gpu
                .get("sppci_model")
                .or_else(|| gpu.get("_name"))?
                .as_str()?;
            Some((
                name.to_owned(),
                family.trim_start_matches("spdisplays_").to_owned(),
            ))
        });
        return Some(match metal {
            Some((name, family)) => Found::ok(format!("Metal on {name} ({family})")),
            None => none("no GPU supports Metal".to_owned()),
        });
    }
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_default();
        return Some(if root.join("System32/d3d12.dll").is_file() {
            Found::ok("Direct3D 12")
        } else {
            none("no Direct3D 12 runtime".to_owned())
        });
    }
    // Vulkan: a loader and an installable client driver.
    let loader = [
        "/usr/lib",
        "/usr/lib64",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
    ]
    .iter()
    .any(|dir| Path::new(dir).join("libvulkan.so.1").exists());
    let drivers: Vec<String> = ["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d"]
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .trim_end_matches(".json")
                .to_owned()
        })
        .collect();
    Some(match (loader, drivers.is_empty()) {
        (true, false) => Found::ok(format!("Vulkan ({})", drivers.join(", "))),
        (false, _) => none("no Vulkan loader (`libvulkan.so.1`)".to_owned()),
        (true, true) => none("a Vulkan loader but no driver in `icd.d`".to_owned()),
    })
}

/// Whether the Rust standard library of `triple` is installed.
fn rust_target(triple: &'static str) -> Found {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let sysroot = match tool(&rustc, &["--print", "sysroot"], TOOL_TIMEOUT) {
        Ok(text) => PathBuf::from(text.trim()),
        Err(why) => return Found::error(crate::error::ENV_RUST_TOOLCHAIN.name, why),
    };
    if sysroot.join("lib/rustlib").join(triple).is_dir() {
        Found::ok(format!("`{triple}` installed"))
    } else {
        Found::missing(
            crate::error::ENV_RUST_TARGET.name,
            format!("`{triple}` is not installed"),
        )
        .fixed_by(Fix::RustTarget(triple))
    }
}

fn wasm(machine: &Machine) -> Option<Found> {
    Some(rust_target(web::TRIPLE).required(asked(machine, Target::WebGpu)))
}

fn bindgen(machine: &Machine) -> Option<Found> {
    let required = asked(machine, Target::WebGpu);
    let pinned = machine
        .lock()
        .and_then(|lock| locked(&lock, "wasm-bindgen"));
    let found = match tool("wasm-bindgen", &["--version"], TOOL_TIMEOUT) {
        Ok(text) => {
            let version = text.trim().rsplit(' ').next().unwrap_or("").to_owned();
            match pinned {
                Some(pinned) if pinned != version => Found::missing(
                    crate::error::ENV_TOOL.name,
                    format!("wasm-bindgen {version}; the project's `Cargo.lock` pins {pinned}"),
                )
                .suggest(format!("cargo install wasm-bindgen-cli --version {pinned}")),
                _ => Found::ok(format!("wasm-bindgen {version}")),
            }
        }
        Err(why) => Found::missing(
            crate::error::ENV_TOOL.name,
            format!("{why}; a web build generates its glue with it"),
        )
        .suggest(match pinned {
            Some(pinned) => format!("{} --version {pinned}", web::BINDGEN_INSTALL),
            None => web::BINDGEN_INSTALL.to_owned(),
        }),
    };
    Some(found.required(required))
}

fn wasm_opt(_: &Machine) -> Option<Found> {
    Some(match tool("wasm-opt", &["--version"], TOOL_TIMEOUT) {
        Ok(text) => Found::ok(text.trim()),
        Err(why) => Found::warn(format!(
            "{why} (Binaryen): a release web bundle is left unoptimized, and a shipping one does not build"
        )),
    })
}

fn filesystem(machine: &Machine) -> Option<Found> {
    let mut places: Vec<PathBuf> = Vec::new();
    if let Ok(project) = &machine.project {
        let target = project.root.join("target");
        places.push(if target.is_dir() {
            target
        } else {
            project.root.clone()
        });
    }
    if let Some(store) = DeviceStore::home() {
        let home = store.root().to_owned();
        places.push(if home.is_dir() {
            home
        } else {
            home.parent().map(Path::to_owned).unwrap_or(home)
        });
    }
    if places.is_empty() {
        return None;
    }
    for place in &places {
        let probe = place.join(format!(".viso-doctor-{}", std::process::id()));
        let written = std::fs::write(&probe, b"").and_then(|()| std::fs::remove_file(&probe));
        if let Err(error) = written {
            return Some(Found::error(
                crate::error::ENV_BUILD_WRITE.name,
                format!("cannot write in {}: {error}", shown(place)),
            ));
        }
    }
    let places: Vec<String> = places.iter().map(|p| shown(p)).collect();
    Some(Found::ok(format!("writable: {}", places.join(", "))))
}

fn git(_: &Machine) -> Option<Found> {
    Some(match tool("git", &["--version"], TOOL_TIMEOUT) {
        Ok(text) => Found::ok(text.trim()),
        Err(why) => Found::warn(format!("{why}; `viso new` then starts no repository")),
    })
}

fn lsp(_: &Machine) -> Option<Found> {
    let Some(server) = crate::command::lsp::locate() else {
        return Some(
            Found::warn(
                "`viso-lsp` is not next to `viso` or on `PATH`: editors get no `.vs` \
                 diagnostics, completion or formatting",
            )
            .suggest(crate::command::lsp::install()),
        );
    };
    let at = shown(&server);
    Some(match tool(&server, &["--version"], TOOL_TIMEOUT) {
        Ok(text) => match text.trim().strip_prefix("viso-lsp ") {
            Some(version) if version == VERSION => Found::ok(format!("viso-lsp {version} ({at})")),
            said => Found::warn(format!(
                "{at} is viso-lsp {}, this CLI is {VERSION}: an editor would check `.vs` \
                 by another compiler than `viso check`",
                said.unwrap_or("of no version")
            ))
            .suggest(crate::command::lsp::install()),
        },
        Err(why) => Found::warn(why).suggest(crate::command::lsp::install()),
    })
}

/// The command that installs this machine's Android component set.
fn android_use() -> String {
    let level = android::Home::current()
        .and_then(|home| home.default_api())
        .unwrap_or(android::components::DEFAULT_API);
    format!("viso android use {level}")
}

fn android_sdk(machine: &Machine) -> Option<Found> {
    let required = asked(machine, Target::Android);
    Some(
        match android::sdk() {
            Ok(sdk) => Found::ok(format!(
                "SDK at {}, {}",
                shown(&sdk.root),
                sdk.origin.describe()
            )),
            Err(why) => {
                Found::missing(crate::error::ENV_ANDROID_SDK.name, why).suggest(android_use())
            }
        }
        .required(required),
    )
}

/// The level `viso android use` chose and whether its whole set is
/// installed; it does not apply to an SDK an environment variable names.
fn android_set(_: &Machine) -> Option<Found> {
    if android::sdk().is_ok_and(|sdk| matches!(sdk.origin, SdkOrigin::Variable(_))) {
        return None;
    }
    let home = android::Home::current()?;
    let Some(level) = home.default_api() else {
        return Some(
            Found::warn(
                "no API level chosen: Viso installs its tested set with `viso android use`",
            )
            .suggest(android_use()),
        );
    };
    let Some(api) = android::components::api(level) else {
        return Some(
            Found::missing(
                crate::error::ANDROID_API_UNSUPPORTED.name,
                format!("the chosen API {level} has no tested component set any more"),
            )
            .suggest(format!(
                "viso android use {}",
                android::components::DEFAULT_API
            )),
        );
    };
    let missing: Vec<&str> = api
        .components(Abi::host())
        .into_iter()
        .filter(|c| !android::installed(&home.sdk(), c))
        .map(|c| c.path)
        .collect();
    Some(if missing.is_empty() {
        Found::ok(format!(
            "API {level} (Android {}), every component installed",
            api.android
        ))
    } else {
        Found::missing(
            crate::error::ENV_ANDROID_SDK.name,
            format!("API {level} lacks {}", missing.join(", ")),
        )
        .suggest(format!("viso android use {level}"))
    })
}

fn android_tools(_: &Machine) -> Option<Found> {
    let sdk = android::sdk().ok()?;
    let absent: Vec<String> = [(&sdk.emulator, "emulator"), (&sdk.adb, "platform-tools")]
        .into_iter()
        .filter(|(path, _)| !path.is_file())
        .map(|(_, package)| format!("`{package}`"))
        .collect();
    Some(if absent.is_empty() {
        Found::ok("emulator and adb installed")
    } else {
        Found::missing(
            crate::error::ENV_DEVICE.name,
            format!("the SDK lacks {}", absent.join(" and ")),
        )
        .suggest(android_use())
    })
}

/// Whether the emulator can use the host's hypervisor (HVF, KVM, WHPX):
/// without it an emulator is too slow to develop on.
fn android_accel(_: &Machine) -> Option<Found> {
    let sdk = android::sdk().ok().filter(|sdk| sdk.emulator.is_file())?;
    Some(match tool(&sdk.emulator, &["-accel-check"], TOOL_TIMEOUT) {
        Ok(text) => {
            let said = text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with("accel"))
                .find(|line| line.parse::<i32>().is_err())
                .unwrap_or("usable")
                .to_owned();
            Found::ok(said)
        }
        Err(why) => Found::missing(
            crate::error::ENV_DEVICE.name,
            format!("the emulator cannot use hardware acceleration: {why}"),
        ),
    })
}

/// `javac`, which compiles the APK's Java shell.
fn java(_: &Machine) -> Option<Found> {
    Some(match tool("javac", &["-version"], TOOL_TIMEOUT) {
        Ok(text) if !text.trim().is_empty() => Found::ok(text.trim()),
        Ok(_) => Found::ok("javac"),
        Err(why) => Found::missing(
            crate::error::ENV_TOOL.name,
            format!("{why}; an APK's Java shell is compiled with it (a JDK 17 or newer)"),
        ),
    })
}

fn android_rust(_: &Machine) -> Option<Found> {
    Some(rust_target(Abi::host().triple()))
}

fn android_profiles(_: &Machine) -> Option<Found> {
    profiles(Platform::Android, "viso android emulator create phone")
}

fn ios_profiles(_: &Machine) -> Option<Found> {
    profiles(Platform::Ios, "viso ios simulator create phone")
}

/// The profiles of `platform`, and how one is made when there is none.
fn profiles(platform: Platform, create: &str) -> Option<Found> {
    let store = DeviceStore::home()?;
    let list = store.list(platform);
    Some(match list.len() {
        0 => Found::warn("none yet: the first `viso run` offers to create one").suggest(create),
        n => {
            let ids: Vec<&str> = list.iter().map(|p| p.id.as_str()).collect();
            Found::ok(format!("{n}: {}", ids.join(", ")))
        }
    })
}

fn xcode(_: &Machine) -> Option<Found> {
    if !cfg!(target_os = "macos") {
        return Some(Found::error(
            crate::error::ENV_IOS_HOST.name,
            "the iOS Simulator SDK is on macOS only",
        ));
    }
    Some(match tool("xcode-select", &["-p"], TOOL_TIMEOUT) {
        Ok(path) if path.trim().ends_with("CommandLineTools") => Found::missing(
            crate::error::ENV_XCODE.name,
            "only the Command Line Tools are selected; the Simulator needs Xcode",
        )
        .suggest("sudo xcode-select -s /Applications/Xcode.app"),
        Ok(path) => match tool("xcodebuild", &["-version"], TOOL_TIMEOUT) {
            Ok(version) => Found::ok(format!(
                "{} at {}",
                version.lines().next().unwrap_or("Xcode").trim(),
                path.trim()
            )),
            Err(_) => Found::ok(format!("at {}", path.trim())),
        },
        Err(why) => Found::missing(crate::error::ENV_XCODE.name, why)
            .suggest("sudo xcode-select -s /Applications/Xcode.app"),
    })
}

fn xcode_license(_: &Machine) -> Option<Found> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    Some(
        match tool("xcodebuild", &["-license", "check"], TOOL_TIMEOUT) {
            Ok(_) => Found::ok("accepted"),
            Err(_) => Found::missing(
                crate::error::ENV_XCODE_LICENSE.name,
                "not accepted: Xcode's tools refuse to run until it is",
            )
            .suggest("sudo xcodebuild -license"),
        },
    )
}

/// How to install a runtime: the one Xcode's SDK pairs with.
fn ios_use() -> String {
    let version = ios::xcode_runtime().unwrap_or_else(|| "<version>".to_owned());
    format!("viso ios use {version}")
}

fn ios_runtime(machine: &Machine) -> Option<Found> {
    let required = asked(machine, Target::Ios);
    if !cfg!(target_os = "macos") {
        return Some(
            Found::missing(
                crate::error::ENV_IOS_HOST.name,
                "the iOS Simulator runs on macOS only",
            )
            .required(required),
        );
    }
    let found = match ios::runtimes() {
        Ok(runtimes) => {
            let home = ios::Home::current();
            let default = home.as_ref().and_then(ios::Home::default_runtime);
            match (&default, ios::chosen(&runtimes, home.as_ref())) {
                (Some(wanted), _) if ios::find(&runtimes, wanted).is_none() => Found::missing(
                    crate::error::DEVICE_SYSTEM.name,
                    format!("the default, iOS {wanted}, is not installed"),
                )
                .suggest(format!("viso ios use {wanted} --download")),
                (Some(_), Some(runtime)) => Found::ok(format!(
                    "iOS {} ({}), the default",
                    runtime.version, runtime.build
                )),
                (None, Some(runtime)) => Found::ok(format!(
                    "iOS {} ({}), the newest; `viso ios use` chooses another",
                    runtime.version, runtime.build
                )),
                (_, None) => Found::missing(
                    crate::error::DEVICE_SYSTEM.name,
                    "no iOS Simulator runtime is installed",
                )
                .suggest(ios_use()),
            }
        }
        Err(why) => Found::missing(crate::error::ENV_XCODE.name, why),
    };
    Some(found.required(required))
}

fn ios_disk(_: &Machine) -> Option<Found> {
    let free = ios::free_bytes()?;
    let shown = crate::output::human::size(free);
    Some(if free < ios::RUNTIME_BYTES {
        Found::warn(format!(
            "{shown} free; a runtime needs about {}",
            crate::output::human::size(ios::RUNTIME_BYTES)
        ))
    } else {
        Found::ok(format!("{shown} free"))
    })
}

fn ios_rust(_: &Machine) -> Option<Found> {
    Some(rust_target(apple::simulator_triple()))
}

fn available(machine: &Machine) -> Option<Found> {
    let target = machine.target?;
    Some(match target.require_available() {
        Ok(_) => Found::ok(format!("`{target}` is available")),
        Err(diagnostic) => Found::error(diagnostic.code.as_str(), diagnostic.message),
    })
}

/// Runs `program` with `args` and returns what it wrote to stdout; why not,
/// when it is not installed, fails, or runs past `timeout` (then killed).
pub(super) fn tool(
    program: impl AsRef<OsStr>,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    let program = program.as_ref();
    let name = Path::new(program)
        .file_name()
        .unwrap_or(program)
        .to_string_lossy()
        .into_owned();
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut tree = match ProcessTree::spawn(&mut command) {
        Ok(tree) => tree,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("`{name}` is not installed"));
        }
        Err(error) => return Err(format!("cannot run `{name}`: {error}")),
    };
    let readers: Vec<_> = [
        tree.child
            .stdout
            .take()
            .map(|o| Box::new(o) as Box<dyn Read + Send>),
        tree.child
            .stderr
            .take()
            .map(|e| Box::new(e) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .map(|stream| {
        thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_string(&mut text);
            }
            text
        })
    })
    .collect();
    let until = Instant::now() + timeout;
    let status = loop {
        match tree.child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= until => {
                tree.kill();
                return Err(format!(
                    "`{name}` did not answer in {} s",
                    timeout.as_secs()
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                tree.kill();
                return Err(format!("cannot wait for `{name}`: {error}"));
            }
        }
    };
    let mut texts = readers.into_iter().map(|r| r.join().unwrap_or_default());
    let (stdout, stderr) = (
        texts.next().unwrap_or_default(),
        texts.next().unwrap_or_default(),
    );
    if status.success() {
        Ok(stdout)
    } else {
        let said = if stderr.trim().is_empty() {
            &stdout
        } else {
            &stderr
        };
        Err(format!("`{name}` failed ({status}): {}", said.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_names_each_package_version() {
        let lock = "version = 4\n\n[[package]]\nname = \"viso\"\nversion = \"0.3.1\"\n\n\
                    [[package]]\nname = \"wasm-bindgen\"\nversion = \"0.2.100\"\nsource = \"registry\"\n";
        assert_eq!(locked(lock, "viso").as_deref(), Some("0.3.1"));
        assert_eq!(locked(lock, "wasm-bindgen").as_deref(), Some("0.2.100"));
        assert_eq!(locked(lock, "wasm"), None);
    }

    #[test]
    fn a_tool_that_is_absent_or_hangs_says_so() {
        let absent = tool("viso-no-such-tool", &[], TOOL_TIMEOUT).unwrap_err();
        assert!(absent.contains("is not installed"), "{absent}");
        if cfg!(unix) {
            let hung = tool("sleep", &["5"], Duration::from_millis(100)).unwrap_err();
            assert!(hung.contains("did not answer"), "{hung}");
            assert_eq!(tool("echo", &["hi"], TOOL_TIMEOUT).as_deref(), Ok("hi\n"));
        }
    }

    #[test]
    fn a_target_adds_its_checks_to_the_toolchain_and_the_project() {
        let names = |target| -> Vec<&str> { plan(target).iter().map(|p| p.name).collect() };
        assert_eq!(
            names(Some(Target::WebGpu)),
            [
                "rust",
                "cargo",
                "project",
                "wasm32",
                "wasm-bindgen",
                "wasm-opt"
            ]
        );
        assert!(names(None).contains(&"android-sdk"));
        assert!(names(None).contains(&"lsp"));
        assert!(!names(Some(Target::Host)).contains(&"lsp"));
        assert!(!names(Some(Target::Ios)).contains(&"android-sdk"));
    }
}
