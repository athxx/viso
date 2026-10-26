//! The Build Service: how the app is compiled and packaged for a target and a
//! profile, shared by `viso build` and `viso run`.
//!
//! - [`rust_plan`]: the Rust target, the compiler arguments and the
//!   environment of a target — the simulator triple, the emulator ABI linked
//!   as the Java shell's library with the NDK, `wasm32-unknown-unknown`, a
//!   device target's triple with its footprint profile's features and
//!   variables.
//! - [`cargo`]: one `cargo build` with the profile's flags, killable with
//!   every process under it.
//! - [`fonts`]: the static text an embedded build subsets its packaged fonts
//!   to.
//! - [`images`]: the images a release build converts to the texel format its
//!   target samples.
//! - [`produce`]: a build packaged for its target into
//!   `target/viso/build/<build-id>/out/` — the executable, the simulator
//!   `.app` signed ad hoc, the APK, or the web bundle as the profile's web
//!   settings shape it ([`web`]); or, for `viso package`, the package its
//!   [`Plan`](crate::delivery::Plan) describes, every ABI it carries
//!   compiled in turn.
//! - [`publish`]: the packaged output copied into `dist/<platform>/` whole,
//!   with its `artifact.json`, replacing what was there in one rename, so
//!   `dist/` never holds part of a build; each file with its size and
//!   SHA-256.

pub(crate) mod cargo;
pub(crate) mod fonts;
pub(crate) mod images;
pub(crate) mod web;

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use viso_package::android::{self, Abi, Sdk, Shell};
use viso_package::{apple, web as bundle};
use viso_project::{DeviceTarget, Layout, Profile, Target, Toolchain, WebSettings};

use cargo::{BuildCommand, BuildFailure, Sink, is_cancelled};

use crate::delivery::{self, Plan, Platform};

/// How the app is compiled for a target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RustPlan {
    /// The Rust target Cargo is given: none for the host, the host's own for
    /// a headless build, so its output lies apart from the host build's.
    pub(crate) target: Option<String>,
    /// The features of the app's dependencies the target turns on.
    pub(crate) features: Vec<&'static str>,
    /// Whether the app builds with its default features off: an embedded
    /// profile's, which has its own features alone.
    pub(crate) no_default_features: bool,
    /// The arguments after `cargo rustc --`; `cargo build` when empty.
    pub(crate) rustc_args: Vec<String>,
    /// The variables Cargo runs with.
    pub(crate) env: Vec<(String, OsString)>,
}

/// How the app is compiled for `target` with `profile`; an Android build
/// links with the NDK of `sdk`, a device target builds as `device`, its
/// `[target.<name>]` table, says.
pub(crate) fn rust_plan(
    target: Target,
    device: Option<&DeviceTarget>,
    profile: Profile,
    sdk: Option<&Sdk>,
) -> RustPlan {
    with_profile(target_plan(target, device, profile, sdk), profile)
}

/// `plan` for `profile`: a dev build runs behavior on the interpreter, which
/// no release or shipping build links.
fn with_profile(mut plan: RustPlan, profile: Profile) -> RustPlan {
    if profile == Profile::Dev {
        plan.features.push(cargo::VM);
    }
    plan
}

fn target_plan(
    target: Target,
    device: Option<&DeviceTarget>,
    profile: Profile,
    sdk: Option<&Sdk>,
) -> RustPlan {
    match target {
        Target::Device(_) => device.map_or_else(RustPlan::default, device_plan),
        Target::Host => RustPlan::default(),
        Target::Headless => RustPlan {
            target: host_triple(),
            features: vec![cargo::HEADLESS],
            ..RustPlan::default()
        },
        Target::Ios => RustPlan {
            target: Some(apple::simulator_triple().to_owned()),
            env: vec![(
                "IPHONEOS_DEPLOYMENT_TARGET".to_owned(),
                apple::MIN_IOS.into(),
            )],
            ..RustPlan::default()
        },
        Target::Android => android_plan(Abi::host(), profile, sdk),
        Target::WebGpu | Target::WebDom | Target::WebHybrid => RustPlan {
            target: Some(bundle::TRIPLE.to_owned()),
            ..RustPlan::default()
        },
    }
}

/// Cuts an embedded `device`'s build of `project` to the text it ships: the
/// static text its packaged fonts are subset to, as [`fonts::FONT_TEXT_ENV`]
/// written under the generated sources of `layout`, and the line-break
/// models of Thai, Lao, Khmer and Myanmar ([`fonts::COMPLEX_LINE_BREAK`])
/// only when its locales or its text need them. Nothing for any other build,
/// which keeps the facade's defaults.
///
/// # Errors
///
/// A source that cannot be read, or the text that cannot be written.
pub(crate) fn add_text_data(
    plan: &mut RustPlan,
    device: Option<&DeviceTarget>,
    project: &viso_project::Project,
    layout: &viso_project::Layout,
) -> Result<(), String> {
    let Some(device) = device.filter(|device| device.profile.value.is_embedded()) else {
        return Ok(());
    };
    let shipped = fonts::Shipped::of(project)?;
    let path = fonts::write(&shipped, layout, &device.name.value)?;
    plan.env
        .push((fonts::FONT_TEXT_ENV.to_owned(), path.into_os_string()));
    if shipped.complex_line_break(device) {
        plan.features.push(fonts::COMPLEX_LINE_BREAK);
    }
    Ok(())
}

/// How the app is compiled for the board `device` describes with `profile`:
/// for its triple, the host's when it names none, so its output lies apart
/// from the host build's; with its footprint profile's features, and for an
/// embedded one with those alone; and with the variables the facade's build
/// script and the KMS backend read its profile, budget, workers and display
/// from.
fn device_plan(device: &DeviceTarget) -> RustPlan {
    let features = device.features();
    let no_default_features = !device.profile.value.keeps_default_features();
    RustPlan {
        target: device
            .triple
            .as_ref()
            .map(|triple| triple.value.clone())
            .or_else(host_triple),
        features,
        no_default_features,
        rustc_args: Vec::new(),
        env: device
            .build_env()
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.into()))
            .collect(),
    }
}

/// How the app is compiled as the shell's library for `abi`, linked with
/// the NDK of `sdk`.
fn android_plan(abi: Abi, profile: Profile, sdk: Option<&Sdk>) -> RustPlan {
    let mut env: Vec<(String, OsString)> = sdk
        .map(|sdk| sdk.cargo_env(abi))
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k, v.into_os_string()))
        .collect();
    if profile == Profile::Dev {
        // The debug info stays on the host: the APK carries the code
        // only, so it installs in seconds.
        env.push(("CARGO_PROFILE_DEV_STRIP".to_owned(), "debuginfo".into()));
    }
    RustPlan {
        target: Some(abi.triple().to_owned()),
        rustc_args: android::RUSTC_ARGS.iter().map(|&a| a.to_owned()).collect(),
        env,
        ..RustPlan::default()
    }
}

/// How the app is compiled for the package `plan` describes: for devices
/// on iOS, for the first of its ABIs on Android.
pub(crate) fn package_plan(plan: &Plan, profile: Profile, sdk: Option<&Sdk>) -> RustPlan {
    match plan.platform {
        Platform::Ios => with_profile(
            RustPlan {
                target: Some(apple::DEVICE_TRIPLE.to_owned()),
                env: vec![(
                    "IPHONEOS_DEPLOYMENT_TARGET".to_owned(),
                    plan.metadata.minimum_os.clone().into(),
                )],
                ..RustPlan::default()
            },
            profile,
        ),
        Platform::Android => with_profile(android_plan(plan.abis[0], profile, sdk), profile),
        _ => rust_plan(plan.target, None, profile, sdk),
    }
}

/// The Rust target of this machine, as `rustc -vV` names it.
pub(crate) fn host_triple() -> Option<String> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let output =
        crate::process::capture(Command::new(rustc).arg("-vV"), Some(crate::process::QUICK))
            .ok()?;
    output
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(|host| host.trim().to_owned())
}

/// The compiler identity of the build: `rustc -V`, or `unknown` when it does
/// not run.
pub(crate) fn toolchain() -> Toolchain {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let version = crate::process::run(Command::new(rustc).arg("-V"), Some(crate::process::QUICK))
        .ok()
        .map_or_else(|| "unknown".to_string(), |v| v.trim().to_string());
    Toolchain::new(version, Toolchain::current_host())
}

/// What an artifact's `target` names: the platform the host resolves to,
/// as its `dist/` directory does, or the target.
pub(crate) fn platform(layout: &Layout, target: Target) -> String {
    match target {
        Target::Host => layout
            .dist(target)
            .file_name()
            .map_or("host".to_owned(), |name| {
                name.to_string_lossy().into_owned()
            }),
        other => other.as_str().to_owned(),
    }
}

/// A build to compile and package.
pub(crate) struct Job {
    /// The compile; it packages nothing itself.
    pub(crate) cargo: BuildCommand,
    pub(crate) target: Target,
    pub(crate) profile: Profile,
    /// How a web bundle is shaped.
    pub(crate) web: WebSettings,
    /// The build's own directory, `target/viso/build/<build-id>/`.
    pub(crate) dir: PathBuf,
    /// The SDK and Java shell an Android build is packed with.
    pub(crate) android: Option<(Sdk, Shell)>,
    /// What the app asks the user for and the URLs that open it: declared
    /// by a mobile build.
    pub(crate) declared: crate::delivery::metadata::Declared,
    /// The package `viso package` makes of it, in place of the build's own
    /// output.
    pub(crate) delivery: Option<Plan>,
}

/// A build compiled and packaged, not yet published.
#[derive(Debug)]
pub(crate) struct Staged {
    /// The packaged output, `<build dir>/out/`.
    pub(crate) out: PathBuf,
    /// Each artifact: its kind and its path under `out`.
    pub(crate) artifacts: Vec<(&'static str, PathBuf)>,
    /// How the output is signed: `none`, `ad-hoc`, `debug-keystore` or
    /// `signed`.
    pub(crate) signing: &'static str,
    /// Each stage and how long it took.
    pub(crate) stages: Vec<(&'static str, Duration)>,
    /// Whether `out` is the one an earlier build staged from the same
    /// inputs, packaged again by nothing.
    pub(crate) reused: bool,
}

/// Beside `out/`, what it was staged from and holds: its key, its signing,
/// then each artifact as `<kind>\t<path>`.
const STAGED: &str = "out.staged";

/// Every kind a build of its own stages; a record naming another is not
/// one this CLI wrote.
const STAGED_KINDS: &[&str] = &[
    "binary",
    "app-bundle",
    "apk",
    "wasm",
    "js",
    "html",
    "brotli",
    "gzip",
];

/// What staging `job` from the executable `exe` depends on: the executable
/// as Cargo left it, how it is packaged, what the app and the packages it
/// links declare, the SDK and the Java shell (each source of every linked
/// package) packing it, and the CLI and tools doing so. The same key stages
/// the same `out/`.
fn staging_key(job: &Job, exe: &Path) -> String {
    use std::fmt::Write as _;
    let stamp = |path: &Path| {
        fs::metadata(path).ok().map(|meta| {
            let modified = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .unwrap_or_default();
            (meta.len(), modified.as_nanos())
        })
    };
    let mut key = String::new();
    let _ = write!(
        key,
        "{exe:?} {:?}\n{:?} {:?}\n{:?}\n{:?}\n{:?}\n",
        stamp(exe),
        job.target,
        job.profile,
        job.web,
        job.declared,
        job.android
    );
    if let Some((_, shell)) = &job.android {
        let _ = writeln!(key, "{:?}", shell.stamp());
    }
    if let Ok(cli) = std::env::current_exe() {
        let _ = writeln!(key, "{cli:?} {:?}", stamp(&cli));
    }
    if matches!(
        job.target,
        Target::WebGpu | Target::WebDom | Target::WebHybrid
    ) {
        for tool in ["wasm-bindgen", "wasm-opt"] {
            let found = std::env::var_os("PATH").and_then(|paths| {
                std::env::split_paths(&paths)
                    .map(|dir| dir.join(tool))
                    .find(|path| path.is_file())
            });
            let _ = writeln!(
                key,
                "{tool} {found:?} {:?}",
                found.as_deref().and_then(stamp)
            );
        }
    }
    let sum = Sha256::digest(key.as_bytes());
    sum.iter().map(|b| format!("{b:02x}")).collect()
}

/// The signing and artifacts of the `out/` of `dir` when it was staged
/// with `key` and is still there.
fn staged_before(dir: &Path, key: &str) -> Option<(&'static str, Vec<(&'static str, PathBuf)>)> {
    let record = fs::read_to_string(dir.join(STAGED)).ok()?;
    let mut lines = record.lines();
    if lines.next()? != key {
        return None;
    }
    let signing = match lines.next()? {
        "none" => "none",
        "ad-hoc" => "ad-hoc",
        "debug-keystore" => "debug-keystore",
        _ => return None,
    };
    let out = dir.join("out");
    let mut artifacts = Vec::new();
    for line in lines {
        let (kind, path) = line.split_once('\t')?;
        let kind = STAGED_KINDS.iter().find(|k| **k == kind)?;
        let path = PathBuf::from(path);
        fs::symlink_metadata(out.join(&path)).ok()?;
        artifacts.push((*kind, path));
    }
    Some((signing, artifacts))
}

/// Records that `staged` was made with `key`, for the next build.
fn record_staged(dir: &Path, key: &str, staged: &Staged) {
    let mut record = format!("{key}\n{}\n", staged.signing);
    for (kind, path) in &staged.artifacts {
        record.push_str(&format!("{kind}\t{}\n", path.display()));
    }
    let _ = crate::atomic::write(&dir.join(STAGED), record.as_bytes());
}

/// Compiles `job` and packages it for its target, until it is done or
/// `cancelled` fires; what Cargo and the packaging say goes to `sink`.
pub(crate) fn produce(
    job: &Job,
    cancelled: &mpsc::Receiver<()>,
    sink: &Sink,
) -> Result<Staged, BuildFailure> {
    let mut stages = Vec::new();
    let started = Instant::now();
    let exe = cargo::build(&job.cargo, cancelled, sink)?;
    stages.push(("compile", started.elapsed()));
    if is_cancelled(cancelled) {
        return Err(BuildFailure::Cancelled);
    }

    let started = Instant::now();
    let out = job.dir.join("out");
    // A build of its own whose inputs did not change has nothing to stage
    // again; a package is always made afresh.
    let key = job.delivery.is_none().then(|| staging_key(job, &exe));
    if let Some(key) = &key
        && let Some((signing, artifacts)) = staged_before(&job.dir, key)
    {
        stages.push(("package", started.elapsed()));
        return Ok(Staged {
            out,
            artifacts,
            signing,
            stages,
            reused: true,
        });
    }
    let _ = fs::remove_file(job.dir.join(STAGED));
    let written = |error: io::Error, path: &Path| {
        BuildFailure::Write(format!("cannot write `{}`: {error}", path.display()))
    };
    if let Err(error) = fs::remove_dir_all(&out)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(written(error, &out));
    }
    fs::create_dir_all(&out).map_err(|e| written(e, &out))?;
    let file = exe
        .file_name()
        .ok_or_else(|| BuildFailure::Package(format!("`{}` has no name", exe.display())))?;
    let bin = exe
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| BuildFailure::Package(format!("`{}` has no name", exe.display())))?;
    let (artifacts, signing) = match &job.delivery {
        Some(plan) => {
            // Each further ABI is its own compile, with its own NDK, and
            // counts as compiling.
            let mut more = Vec::new();
            for &abi in plan.abis.iter().skip(1) {
                let rust = android_plan(abi, job.profile, job.android.as_ref().map(|(sdk, _)| sdk));
                let mut cargo = job.cargo.clone();
                cargo.target = rust.target;
                cargo.env = rust.env;
                more.push((abi, cargo::build(&cargo, cancelled, sink)?));
                if is_cancelled(cancelled) {
                    return Err(BuildFailure::Cancelled);
                }
            }
            stages[0].1 += started.elapsed();
            let started = Instant::now();
            let built = delivery::package::Built {
                exe: &exe,
                more: &more,
                bin,
                dir: &job.dir,
                profile: job.profile,
                web: &job.web,
                android: job.android.as_ref(),
                fragments: &job.declared.fragments,
            };
            let artifacts = delivery::package::make(plan, &built, &out, cancelled, sink)?;
            stages.push(("package", started.elapsed()));
            return Ok(Staged {
                artifacts: relative(artifacts, &out),
                out,
                signing: plan.signing.signer.status(),
                stages,
                reused: false,
            });
        }
        None => match job.target {
            Target::Host | Target::Headless | Target::Device(_) => {
                let path = out.join(file);
                fs::copy(&exe, &path).map_err(|e| written(e, &path))?;
                (vec![("binary", path)], "none")
            }
            Target::Ios => {
                let declared = &job.declared;
                let info = apple::Info {
                    usage: declared.usage.clone(),
                    links: declared.links.clone(),
                    fragments: declared.fragments.info.clone(),
                    ..apple::Info::dev(bin)
                };
                let app = apple::bundle(&exe, &out, &info, true).map_err(BuildFailure::Package)?;
                let entitlements = apple::Entitlements {
                    hosts: &declared.links.hosts,
                    fragments: &declared.fragments.entitlements,
                };
                apple::sign(&app.path, None, None, &entitlements, &job.dir.join("sign"))
                    .map_err(BuildFailure::Package)?;
                (vec![("app-bundle", app.path)], "ad-hoc")
            }
            Target::Android => {
                let (sdk, shell) = job
                    .android
                    .as_ref()
                    .expect("an Android build has its SDK and shell");
                let flavor = match job.profile {
                    Profile::Dev => android::Flavor::Dev,
                    Profile::Release | Profile::Shipping => android::Flavor::Release,
                };
                let dir = job.dir.join("android");
                let apk = sdk
                    .package(&android::Packing::dev(
                        &exe,
                        Abi::host(),
                        &dir,
                        bin,
                        shell,
                        flavor,
                        &job.declared.access(),
                        &job.declared.links,
                    ))
                    .map_err(BuildFailure::Package)?;
                let path = out.join(format!("{bin}.apk"));
                fs::copy(&apk.path, &path).map_err(|e| written(e, &path))?;
                (vec![("apk", path)], "debug-keystore")
            }
            Target::WebGpu | Target::WebDom | Target::WebHybrid => {
                let worker = viso_package::fragment::worker(&job.declared.fragments.worker);
                let files = web::bundle(
                    &exe,
                    &out,
                    bin,
                    job.profile,
                    &job.web,
                    worker.as_deref(),
                    cancelled,
                    sink,
                )?;
                (files, "none")
            }
        },
    };
    stages.push(("package", started.elapsed()));
    let staged = Staged {
        artifacts: relative(artifacts, &out),
        out,
        signing,
        stages,
        reused: false,
    };
    if let Some(key) = &key {
        record_staged(&job.dir, key, &staged);
    }
    Ok(staged)
}

/// Each artifact's path relative to `out`.
fn relative(artifacts: Vec<(&'static str, PathBuf)>, out: &Path) -> Vec<(&'static str, PathBuf)> {
    artifacts
        .into_iter()
        .map(|(kind, path)| {
            let relative = path.strip_prefix(out).map(Path::to_owned).unwrap_or(path);
            (kind, relative)
        })
        .collect()
}

/// An artifact as published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Written {
    pub(crate) kind: &'static str,
    /// Where it is, under `dist/<platform>/`.
    pub(crate) path: PathBuf,
    /// Its bytes; a directory's, all its files'.
    pub(crate) size: u64,
    /// `sha256:<hex>`; a directory's over its files ([`hash`]).
    pub(crate) hash: String,
}

/// The name of the manifest beside a published build.
pub(crate) const MANIFEST: &str = "artifact.json";

/// What publishing wrote: the artifacts, the manifest last, and whether
/// `dist/` held them already.
pub(crate) struct Published {
    pub(crate) written: Vec<Written>,
    pub(crate) cached: bool,
}

/// Publishes `staged` as `dist`, `dist/<platform>/`, with the manifest
/// `manifest` writes of its artifacts: copied beside it, then swapped in
/// with one rename, the old output removed after. A reused `staged` that
/// `dist` holds already, artifacts and manifest the same, is left as it is.
pub(crate) fn publish(
    staged: &Staged,
    dist: &Path,
    manifest: impl Fn(&[Written]) -> String,
) -> Result<Published, String> {
    if staged.reused
        && let Some(written) = published_already(staged, dist, &manifest)
    {
        return Ok(Published {
            written,
            cached: true,
        });
    }
    let parent = dist
        .parent()
        .ok_or_else(|| format!("`{}` has no parent", dist.display()))?;
    let name = dist
        .file_name()
        .ok_or_else(|| format!("`{}` has no name", dist.display()))?
        .to_string_lossy();
    let fresh = parent.join(format!(".{name}.{}.new", std::process::id()));
    let failed =
        |error: io::Error, path: &Path| format!("cannot write `{}`: {error}", path.display());
    fs::create_dir_all(parent).map_err(|e| failed(e, parent))?;
    let _ = fs::remove_dir_all(&fresh);
    let result = (|| {
        copy_tree(&staged.out, &fresh).map_err(|e| failed(e, &fresh))?;
        let mut written = Vec::with_capacity(staged.artifacts.len() + 1);
        for (kind, relative) in &staged.artifacts {
            let (size, hash) = hash(&fresh.join(relative)).map_err(|e| failed(e, relative))?;
            written.push(Written {
                kind,
                path: dist.join(relative),
                size,
                hash,
            });
        }
        let path = fresh.join(MANIFEST);
        fs::write(&path, manifest(&written)).map_err(|e| failed(e, &path))?;
        let (size, hash) = hash(&path).map_err(|e| failed(e, &path))?;
        written.push(Written {
            kind: "manifest",
            path: dist.join(MANIFEST),
            size,
            hash,
        });
        replace(&fresh, dist, parent, &name).map_err(|e| failed(e, dist))?;
        Ok(Published {
            written,
            cached: false,
        })
    })();
    // After a swap it holds the old output; after a failure, part of the new.
    let _ = fs::remove_dir_all(&fresh);
    result
}

/// What `dist` holds of `staged` when it holds every artifact as `staged`
/// has it, the manifest `manifest` writes of them, and nothing else.
fn published_already(
    staged: &Staged,
    dist: &Path,
    manifest: &impl Fn(&[Written]) -> String,
) -> Option<Vec<Written>> {
    let mut expected: Vec<OsString> = staged
        .artifacts
        .iter()
        .filter_map(|(_, relative)| relative.iter().next().map(ToOwned::to_owned))
        .chain([MANIFEST.into()])
        .collect();
    expected.sort();
    expected.dedup();
    let mut held: Vec<OsString> = fs::read_dir(dist)
        .ok()?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<_>>()
        .ok()?;
    held.sort();
    if held != expected {
        return None;
    }
    let mut written = Vec::with_capacity(staged.artifacts.len() + 1);
    for (kind, relative) in &staged.artifacts {
        let (size, hash) = hash(&dist.join(relative)).ok()?;
        if hash != self::hash(&staged.out.join(relative)).ok()?.1 {
            return None;
        }
        written.push(Written {
            kind,
            path: dist.join(relative),
            size,
            hash,
        });
    }
    let path = dist.join(MANIFEST);
    if fs::read_to_string(&path).ok()? != manifest(&written) {
        return None;
    }
    let (size, hash) = hash(&path).ok()?;
    written.push(Written {
        kind: "manifest",
        path,
        size,
        hash,
    });
    Some(written)
}

/// Puts `fresh` where `dist` is: swapped in one rename where the system
/// can, else the old moved aside first. `fresh` then holds the old output.
fn replace(fresh: &Path, dist: &Path, parent: &Path, name: &str) -> io::Result<()> {
    if fs::symlink_metadata(dist).is_err() {
        return fs::rename(fresh, dist);
    }
    match exchange(fresh, dist) {
        Ok(()) => return Ok(()),
        Err(error) if !unsupported(&error) => return Err(error),
        Err(_) => {}
    }
    let old = parent.join(format!(".{name}.{}.old", std::process::id()));
    let _ = fs::remove_dir_all(&old);
    fs::rename(dist, &old)?;
    if let Err(error) = fs::rename(fresh, dist) {
        let _ = fs::rename(&old, dist);
        return Err(error);
    }
    fs::rename(&old, fresh)
}

/// Whether a swap failed only because the system or filesystem has none.
fn unsupported(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
    ) || {
        #[cfg(unix)]
        {
            matches!(
                error.raw_os_error(),
                Some(libc::ENOTSUP | libc::ENOSYS | libc::EINVAL | libc::EXDEV)
            )
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

/// Swaps the entries at `a` and `b` atomically.
#[cfg(target_vendor = "apple")]
fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let a = CString::new(a.as_os_str().as_bytes())?;
    let b = CString::new(b.as_os_str().as_bytes())?;
    // SAFETY: both are NUL-terminated paths that live across the call.
    if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Swaps the entries at `a` and `b` atomically.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let a = CString::new(a.as_os_str().as_bytes())?;
    let b = CString::new(b.as_os_str().as_bytes())?;
    // SAFETY: both are NUL-terminated paths that live across the call, each
    // taken from the current directory as `AT_FDCWD` asks.
    let swapped = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if swapped == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Swaps the entries at `a` and `b` atomically, where the system can.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
fn exchange(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Copies the tree at `from` to `to`, which does not exist: each file
/// cloned where the filesystem can, links recreated as links.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(fs::read_link(&source)?, &target)?;
            #[cfg(not(unix))]
            fs::copy(&source, &target).map(drop)?;
        } else {
            fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// The size and `sha256:<hex>` of the file or directory at `path`. A
/// directory's hash is over its files in path order: each one's path below
/// it, `/`-separated, a NUL, and the SHA-256 of its bytes; its size is
/// theirs together.
pub(crate) fn hash(path: &Path) -> io::Result<(u64, String)> {
    let digest = |bytes: &[u8]| -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() };
    if !fs::symlink_metadata(path)?.is_dir() {
        let (size, sum) = file_hash(path)?;
        return Ok((size, format!("sha256:{}", digest(&sum))));
    }
    let mut files = Vec::new();
    collect(path, path, &mut files)?;
    files.sort();
    let mut tree = Sha256::new();
    let mut total = 0;
    for name in files {
        let (size, sum) = file_hash(&path.join(&name))?;
        total += size;
        tree.update(name.as_bytes());
        tree.update([0]);
        tree.update(sum);
    }
    Ok((total, format!("sha256:{}", digest(&tree.finalize()))))
}

/// The SHA-256 of a file's bytes, and how many there are.
pub(crate) fn file_hash(path: &Path) -> io::Result<(u64, [u8; 32])> {
    let mut hasher = Sha256::new();
    let size = io::copy(&mut fs::File::open(path)?, &mut hasher)?;
    Ok((size, hasher.finalize().into()))
}

/// The files under `dir`, as paths below `root` with `/` separators.
fn collect(root: &Path, dir: &Path, files: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect(root, &path, files)?;
        } else {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let name: Vec<String> = relative
                .iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect();
            files.push(name.join("/"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("viso-build-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_target_compiles_for_its_device() {
        assert_eq!(
            rust_plan(Target::Host, None, Profile::Release, None),
            RustPlan::default()
        );
        // Headless builds for this machine, apart from its host build.
        let headless = rust_plan(Target::Headless, None, Profile::Dev, None);
        let host = headless.target.expect("rustc names its host");
        assert!(host.contains(std::env::consts::ARCH), "{host}");
        assert_eq!(headless.features, [cargo::HEADLESS, cargo::VM]);
        let web = rust_plan(Target::WebGpu, None, Profile::Dev, None);
        assert_eq!(web.target.as_deref(), Some("wasm32-unknown-unknown"));
        let ios = rust_plan(Target::Ios, None, Profile::Dev, None);
        assert_eq!(ios.target.as_deref(), Some(apple::simulator_triple()));
        let android = rust_plan(Target::Android, None, Profile::Release, None);
        assert!(!android.rustc_args.is_empty());
        assert!(
            android
                .env
                .iter()
                .all(|(k, _)| k != "CARGO_PROFILE_DEV_STRIP")
        );
        let android = rust_plan(Target::Android, None, Profile::Dev, None);
        assert!(
            android
                .env
                .iter()
                .any(|(k, _)| k == "CARGO_PROFILE_DEV_STRIP")
        );
    }

    /// A device target builds for its triple with its profile's features, and
    /// hands its table to the facade's build script and the KMS backend.
    #[test]
    fn a_device_target_compiles_as_its_table_says() {
        let manifest = viso_project::Manifest::parse(
            "Viso.toml",
            "[target.panel]\nprofile = \"embedded-gpu\"\ntriple = \"aarch64-unknown-linux-gnu\"\n\
             display = { width = 1024, height = 600, rotation = 90 }\nbudget = { heap = \"48MiB\" }\n\
             workers = 1\n\n[target.kiosk]\nprofile = \"embedded-cpu\"\n",
        )
        .ok()
        .unwrap();
        let panel = manifest.targets.device("panel");
        let plan = rust_plan(Target::device("panel"), panel, Profile::Release, None);
        assert_eq!(plan.target.as_deref(), Some("aarch64-unknown-linux-gnu"));
        // An embedded profile has its own features alone, and the glyph
        // rasterizer its defaults would have given it.
        assert_eq!(plan.features, ["viso/gles", "viso/kms", "viso/rasterizer"]);
        assert!(plan.no_default_features);
        let env: Vec<(&str, &str)> = plan
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.to_str().unwrap()))
            .collect();
        assert_eq!(
            env,
            [
                ("VISO_FOOTPRINT", "embedded-gpu"),
                ("VISO_BUDGET", "heap=50331648"),
                ("VISO_WORKERS", "1"),
                ("VISO_KMS_MODE", "1024x600"),
                ("VISO_KMS_ROTATION", "90"),
            ]
        );
        // No triple is the host's, named, so the output lies apart.
        let kiosk = manifest.targets.device("kiosk");
        let plan = rust_plan(Target::device("kiosk"), kiosk, Profile::Dev, None);
        assert_eq!(plan.target, host_triple());
        // A debug build runs behavior on the interpreter.
        assert_eq!(
            plan.features,
            ["viso/cpu", "viso/kms", "viso/rasterizer", "viso/vm"]
        );
        assert!(plan.no_default_features);
        let host = rust_plan(Target::Host, None, Profile::Dev, None);
        assert!(!host.no_default_features);
        // Only a dev build links the interpreter.
        assert_eq!(host.features, [cargo::VM]);
        for profile in [Profile::Release, Profile::Shipping] {
            let plan = rust_plan(Target::Host, None, profile, None);
            assert!(!plan.features.contains(&cargo::VM), "{profile:?}");
        }
    }

    #[test]
    fn a_file_and_a_tree_hash_as_their_bytes_say() {
        let dir = scratch("hash");
        fs::write(dir.join("a"), b"abc").unwrap();
        assert_eq!(
            hash(&dir.join("a")).unwrap(),
            (
                3,
                "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into()
            )
        );
        let app = dir.join("demo.app");
        fs::create_dir_all(app.join("res")).unwrap();
        fs::write(app.join("demo"), b"exe").unwrap();
        fs::write(app.join("res/x"), b"xy").unwrap();
        let (size, first) = hash(&app).unwrap();
        assert_eq!(size, 5);
        fs::write(app.join("res/x"), b"xz").unwrap();
        assert_ne!(hash(&app).unwrap().1, first, "a file's bytes count");
        fs::write(app.join("res/x"), b"xy").unwrap();
        assert_eq!(hash(&app).unwrap().1, first);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn publishing_replaces_the_output_whole() {
        let dir = scratch("publish");
        let out = dir.join("build/out");
        fs::create_dir_all(out.join("demo.app")).unwrap();
        fs::write(out.join("demo.app/demo"), b"new").unwrap();
        let dist = dir.join("dist/ios");
        fs::create_dir_all(&dist).unwrap();
        fs::write(dist.join("stale"), b"old").unwrap();
        let staged = Staged {
            out: out.clone(),
            artifacts: vec![("app-bundle", PathBuf::from("demo.app"))],
            signing: "ad-hoc",
            stages: Vec::new(),
            reused: false,
        };
        let written = publish(&staged, &dist, |artifacts| {
            format!("{{\"count\":{}}}\n", artifacts.len())
        })
        .unwrap()
        .written;
        let kinds: Vec<&str> = written.iter().map(|w| w.kind).collect();
        assert_eq!(kinds, ["app-bundle", "manifest"]);
        assert_eq!(written[0].path, dist.join("demo.app"));
        assert_eq!(written[0].size, 3);
        assert_eq!(
            fs::read_to_string(dist.join(MANIFEST)).unwrap(),
            "{\"count\":1}\n"
        );
        assert!(!dist.join("stale").exists(), "the old output is gone");
        assert_eq!(fs::read(dist.join("demo.app/demo")).unwrap(), b"new");
        assert!(
            out.join("demo.app/demo").exists(),
            "the build keeps its own"
        );
        let left: Vec<_> = fs::read_dir(dir.join("dist"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["ios"], "nothing is left beside it");

        // A reused staging `dist/` holds already is left as it is; one it
        // does not hold, or holds beside something else, is published.
        let reused = Staged {
            reused: true,
            ..staged
        };
        let count = |artifacts: &[Written]| format!("{{\"count\":{}}}\n", artifacts.len());
        assert!(publish(&reused, &dist, count).unwrap().cached);
        fs::write(dist.join("demo.app/demo"), b"edited").unwrap();
        assert!(!publish(&reused, &dist, count).unwrap().cached);
        assert_eq!(fs::read(dist.join("demo.app/demo")).unwrap(), b"new");
        fs::write(dist.join("extra"), b"x").unwrap();
        assert!(!publish(&reused, &dist, count).unwrap().cached);
        assert!(!dist.join("extra").exists());
        let staged = reused;

        // Into a `dist/` that does not exist yet.
        let fresh = dir.join("other/dist/web");
        publish(&staged, &fresh, |_| String::new()).unwrap();
        assert!(fresh.join("demo.app/demo").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
