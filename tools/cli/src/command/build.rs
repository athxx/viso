//! `viso build [<target>]`: check the project's sources, compile the app for
//! the target with the profile (`--profile`, `--release`, `VISO_PROFILE`,
//! `dev` when none names one), package it for the target, and publish it to
//! `dist/<platform>/` with its `artifact.json`, without running it. A device
//! target of `Viso.toml` builds for its triple with its footprint profile and
//! is published to `dist/<name>/`, for its board.
//!
//! The build cache's lock of the target is held throughout; a `viso run` of
//! the target waits for it before each of its own builds. Ctrl-C kills the
//! build with every process under it, leaves `dist/` as it was and exits 130.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use viso_ende::JsonWriter;
use viso_project::{
    ArtifactKind, Layout, LockGuard, LockKind, Profile, Project, ProjectFingerprint, Target,
};

use super::check;
use crate::args::{BuildArgs, BuildTarget};
use crate::build::cargo::{self, BuildCommand, BuildFailure, CargoEvent, Sink};
use crate::build::{self, Job, Staged, Written};
use crate::context::{Context, Loaded, in_its_file};
use crate::delivery::fragments::{self, Fragments};
use crate::delivery::{Plan, Platform, package};
use crate::deploy;
use crate::dev::cargo::RustScope;
use crate::error::{
    BUILD_FAILED, BUILD_PACKAGE, CLI_USAGE, ENV_BUILD_WRITE, ENV_CARGO, ENV_CARGO_MANIFEST,
    ENV_DEVICE, ENV_LOCK_HELD, ENV_NO_EXECUTABLE, ENV_TOOL, INTERRUPTED, PACKAGE_ARCHIVE,
    PACKAGE_SIGNING, SUCCESS,
};
use crate::interrupt;
use crate::output::sayln;
use crate::output::{Artifact, Output, human};

/// How long the command waits for the build between checks for Ctrl-C.
const POLL: Duration = Duration::from_millis(50);

/// What the build's thread hands the command.
enum Message {
    Cargo(CargoEvent),
    Done(Result<Staged, BuildFailure>),
}

pub fn run(ctx: &Context<'_>, args: &BuildArgs, out: &mut Output) -> u8 {
    let target = args
        .target
        .as_ref()
        .map_or(Target::Host, |target| target.target(BuildTarget::target));
    let named = ctx.global.profile.map(|p| p.profile());
    let profile = match (args.release, named) {
        (true, Some(other)) if other != Profile::Release => {
            return out.failure_next(
                CLI_USAGE,
                &format!("`--release` and `--profile {other}` name different profiles"),
                &["`--release` is `--profile release`".to_owned()],
                &[command_line(target, other)],
            );
        }
        (true, _) => Some(Profile::Release),
        (false, _) => None,
    };
    let Built {
        staged,
        project,
        layout,
        profile,
        build_id,
        toolchain,
        started,
        _lock,
    } = match stage(ctx, target, profile, Check::Run, out) {
        Ok(built) => built,
        Err(code) => return code,
    };
    let root = project.root.clone();

    let dist = layout.dist(target);
    let platform = build::platform(&layout, target);
    let publishing = Instant::now();
    let name = project.name().unwrap_or_default().to_owned();
    let version = project
        .manifest
        .package
        .version
        .as_ref()
        .map(|version| version.value.clone());
    let published = match build::publish(&staged, &dist, |artifacts| {
        manifest_json(&Described {
            project: &name,
            version: version.as_deref(),
            target: &platform,
            profile: profile.as_str(),
            build_id: &build_id,
            build: None,
            toolchain: &toolchain,
            signing: Signed {
                policy: None,
                status: staged.signing,
                identity: None,
            },
            dist: &dist,
            artifacts,
        })
    }) {
        Ok(published) => published,
        Err(why) => return out.failure(ENV_BUILD_WRITE, &why, &[]),
    };
    let written = &published.written;
    let elapsed = started.elapsed();
    out.verbose(|| {
        let mut stages: Vec<String> = staged
            .stages
            .iter()
            .map(|(stage, took)| format!("{stage} {}", seconds(*took)))
            .collect();
        stages.push(format!("publish {}", seconds(publishing.elapsed())));
        stages.join(", ")
    });

    if out.human() {
        sayln!(
            "Built {target} ({profile}) in {}{}",
            seconds(elapsed),
            if published.cached { ", unchanged" } else { "" }
        );
    }
    let paths: Vec<String> = written
        .iter()
        .map(|artifact| relative_name(&root, &artifact.path, artifact.kind == "app-bundle"))
        .collect();
    let width = paths.iter().map(String::len).max().unwrap_or(0);
    for (artifact, path) in written.iter().zip(&paths) {
        out.artifact(
            &Artifact {
                kind: artifact.kind,
                path: path.trim_end_matches('/'),
                target: &platform,
                profile: Some(profile.as_str()),
                build_id: &build_id,
                size: artifact.size,
                hash: &artifact.hash,
                cached: published.cached,
            },
            || {
                format!(
                    "  {:<10} {path:<width$}  {}\n",
                    artifact.kind,
                    human::size(artifact.size)
                )
            },
        );
    }
    SUCCESS
}

/// Whether [`stage`] checks the project's sources first.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Check {
    Run,
    /// The command checked them already.
    Done,
}

/// A build compiled and packaged into its build directory, with the build
/// cache's lock still held.
pub(super) struct Built {
    pub(super) staged: Staged,
    pub(super) project: Project,
    pub(super) layout: Layout,
    pub(super) profile: Profile,
    pub(super) build_id: String,
    /// `rustc -V`.
    pub(super) toolchain: String,
    pub(super) started: Instant,
    pub(super) _lock: LockGuard,
}

/// Compiles and packages the project for `target` with `profile` (the
/// configured one when `None`) into its build directory, reporting what the
/// build says as it says it.
pub(super) fn stage(
    ctx: &Context<'_>,
    target: Target,
    profile: Option<Profile>,
    check: Check,
    out: &mut Output,
) -> Result<Built, u8> {
    available(target, out)?;
    let loaded = ctx.load_as(target, ArtifactKind::Build, profile, out)?;
    stage_loaded(ctx, target, loaded, check, None, out)
}

/// Fails unless this build of `viso` can build for `target` here.
pub(super) fn available(target: Target, out: &mut Output) -> Result<(), u8> {
    if let Err(diagnostic) = target.require_available() {
        out.config(None, &diagnostic);
        return Err(diagnostic.code.exit_code());
    }
    if target == Target::Ios && !cfg!(target_os = "macos") {
        return Err(out.failure(ENV_DEVICE, "the iOS SDK is on macOS only", &[]));
    }
    Ok(())
}

/// As [`stage`], for the project `loaded`; with a `delivery` plan, the
/// package it describes in place of the build's own output, its failures
/// the delivery's.
pub(super) fn stage_loaded(
    ctx: &Context<'_>,
    target: Target,
    loaded: Loaded,
    check: Check,
    delivery: Option<Plan>,
    out: &mut Output,
) -> Result<Built, u8> {
    let Loaded {
        project,
        resolved,
        layout,
    } = loaded;
    let config = resolved.config;
    let profile = config.profile.value;

    // Nothing is compiled from sources that do not check.
    let project = match check {
        Check::Run => {
            let checked = check::package_for(project, &check::Options::target(target), out)?;
            let code = checked.code(out);
            if code != SUCCESS {
                return Err(code);
            }
            checked.project
        }
        Check::Done => project,
    };
    let root = project.root.clone();
    if !root.join("Cargo.toml").is_file() {
        return Err(out.failure(
            ENV_CARGO_MANIFEST,
            &format!(
                "no `Cargo.toml` next to `{}`",
                project.manifest_path().display()
            ),
            &["`viso build` builds the Cargo package at the project root".to_owned()],
        ));
    }
    let sdk = match target {
        Target::Android => match crate::toolchain::android::sdk() {
            Ok(sdk) => Some(sdk),
            Err(why) => return Err(crate::toolchain::android::no_sdk(out, &why)),
        },
        _ => None,
    };

    let lock = ctx.lock(&layout, LockKind::BuildCache { target }, out)?;
    let fingerprint = match ProjectFingerprint::compute(&root) {
        Ok(fingerprint) => fingerprint,
        Err(diagnostic) => {
            in_its_file(out, &diagnostic);
            return Err(diagnostic.code.exit_code());
        }
    };
    let toolchain = build::toolchain();
    let identity = config.build_id(fingerprint, &toolchain);
    let build_id = identity.to_hex();
    let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
    let manifest = canonical.join("Cargo.toml");
    let mut plan = match &delivery {
        Some(delivery) => build::package_plan(delivery, profile, sdk.as_ref()),
        None => build::rust_plan(target, config.device.as_ref(), profile, sdk.as_ref()),
    };
    if delivery.is_none()
        && let Err(why) = build::add_text_data(&mut plan, config.device.as_ref(), &project, &layout)
    {
        return Err(out.failure(ENV_BUILD_WRITE, &why, &[]));
    }
    crate::delivery::metadata::add_schemes(&mut plan, &project);
    let image_target = delivery.as_ref().map_or(target, |delivery| delivery.target);
    match build::images::add_converted_images(
        &mut plan,
        image_target,
        config.device.as_ref(),
        profile,
        &project,
        &layout,
    ) {
        Ok(Some(converted)) if converted.images > 0 => out.progress(
            "images",
            &format!(
                "{} images converted ({} from cache): {} bytes as written, {} converted{}",
                converted.images,
                converted.cached,
                converted.source_bytes,
                converted.converted_bytes,
                match converted.animated {
                    0 => String::new(),
                    n => format!("; {n} animated, as written"),
                }
            ),
        ),
        Ok(_) => {}
        Err(why) => return Err(out.failure(ENV_BUILD_WRITE, &why, &[])),
    }
    if let Some(delivery) = &delivery
        && delivery.platform == Platform::Windows
    {
        let triple = build::host_triple().unwrap_or_default();
        match package::link_resources(delivery, profile, &triple, &layout.build(identity)) {
            Ok(args) => plan.rustc_args.extend(args),
            Err(why) => return Err(out.failure(PACKAGE_ARCHIVE, &why, &[])),
        }
    }
    let packaging = delivery.is_some();
    // What the packages the app links declare for its platform is read, and
    // checked, before anything is compiled.
    let mut declared = crate::delivery::metadata::declared(&project);
    let fragment_platform = fragments::platform(target, delivery.as_ref().map(|d| d.platform));
    let linked = match fragment_platform {
        Some(_) => match RustScope::read(&manifest, plan.target.as_deref(), &ctx.cargo_args()) {
            Ok(scope) => scope.linked,
            Err(why) => return Err(out.failure(ENV_CARGO, &why, &[])),
        },
        None => Vec::new(),
    };
    if let Some(platform) = fragment_platform {
        declared.fragments = match Fragments::read(&linked, platform) {
            Ok(fragments) => fragments,
            Err(diagnostic) => {
                fragments::report(out, &diagnostic);
                return Err(diagnostic.code.exit_code());
            }
        };
    }
    let android = match sdk {
        Some(sdk) => match deploy::android_shell(&linked, &declared.fragments.android) {
            Ok(shell) => Some((sdk, shell)),
            Err(why) => return Err(out.failure(ENV_DEVICE, &why, &[])),
        },
        None => None,
    };
    if let Some(platform) = fragment_platform {
        let name = project.name().unwrap_or("app");
        let info = match &delivery {
            Some(delivery) => delivery.metadata.apple(name),
            None => viso_package::apple::Info {
                usage: declared.usage.clone(),
                links: declared.links.clone(),
                ..viso_package::apple::Info::dev(name)
            },
        };
        let shell = android.as_ref().map(|(_, shell)| shell);
        if let Err(diagnostic) = declared.fragments.check(platform, shell, &info) {
            fragments::report(out, &diagnostic);
            return Err(diagnostic.code.exit_code());
        }
    }
    // A simulator app's entitlements are linked into its executable.
    if target == Target::Ios && delivery.is_none() {
        match fragments::link_simulated(&declared, &layout.generated()) {
            Ok(args) => plan.rustc_args.extend(args),
            Err(why) => return Err(out.failure(ENV_BUILD_WRITE, &why, &[])),
        }
    }
    warn_kept_defaults(plan.no_default_features, &manifest, ctx, out);
    let mut features = plan.features;
    if config.dev_runtime() == viso_project::DevRuntime::Present && cargo::reloads(target) {
        features.push(cargo::HOT_RELOAD);
    }
    let job = Job {
        cargo: BuildCommand {
            cargo: cargo::program(),
            manifest,
            quiet: ctx.global.quiet,
            piped: ctx.global.json,
            target_dir: ctx.target_dir(),
            cargo_args: ctx.cargo_args(),
            build_args: ctx.cargo_build_args(),
            profile,
            profile_args: cargo::profile_args(profile, &config.settings, target),
            features,
            no_default_features: plan.no_default_features,
            target: plan.target,
            rustc_args: plan.rustc_args,
            env: plan.env,
            package: None,
            lock: None,
        },
        target,
        profile,
        web: config.settings.web,
        dir: layout.build(identity),
        android,
        declared,
        delivery,
    };
    let doing = if packaging { "packaging" } else { "building" };
    out.progress("build", &format!("{doing} {target} ({profile})"));

    interrupt::catch();
    let started = Instant::now();
    let staging = job.dir.join("out");
    let staged = match produce(job, &canonical, out) {
        Ok(staged) => staged,
        Err(failure) => {
            // A failed or cancelled build leaves no output that looks whole.
            let _ = std::fs::remove_dir_all(&staging);
            return Err(failed(out, &failure, packaging));
        }
    };
    if interrupt::count() > 0 {
        return Err(INTERRUPTED);
    }
    Ok(Built {
        staged,
        project,
        layout,
        profile,
        build_id,
        toolchain: toolchain.rustc,
        started,
        _lock: lock,
    })
}

/// `viso build` of `target` with `profile`, as typed.
/// Warns when the build turns the app's default features off but the app's
/// `viso` dependency keeps the facade's on, so the embedded build links the
/// desktop stack after all.
pub(crate) fn warn_kept_defaults(
    no_default_features: bool,
    manifest: &Path,
    ctx: &Context<'_>,
    out: &mut Output,
) {
    if no_default_features
        && crate::dev::cargo::facade_defaults(manifest, &ctx.cargo_args()) == Some(true)
    {
        out.log(
            "warn",
            "tool",
            "the app's `viso` dependency keeps its default features, so this embedded \
             build links the desktop stack; depend on it with `default-features = false` \
             and give the app `default = [\"viso/desktop\", \"viso/native-vulkan\", \"viso/rasterizer\", \"viso/complex-line-break\"]`",
        );
    }
}

fn command_line(target: Target, profile: Profile) -> String {
    match target {
        Target::Host => format!("viso build --profile {profile}"),
        target => format!("viso build {target} --profile {profile}"),
    }
}

/// Runs `job` on its own thread, reporting what it says as it says it,
/// and cancels it on Ctrl-C.
fn produce(job: Job, root: &Path, out: &mut Output) -> Result<Staged, BuildFailure> {
    let (tx, rx) = mpsc::channel();
    let (cancel, cancelled) = mpsc::channel();
    let events = tx.clone();
    let worker = thread::Builder::new()
        .name("viso-build".into())
        .spawn(move || {
            let sink: Sink = Arc::new(move |event| events.send(Message::Cargo(event)).is_ok());
            let result = build::produce(&job, &cancelled, &sink);
            let _ = tx.send(Message::Done(result));
        })
        .expect("a build thread");
    let mut cancelling = false;
    let result = loop {
        if !cancelling && interrupt::count() > 0 {
            cancelling = true;
            let _ = cancel.send(());
        }
        match rx.recv_timeout(POLL) {
            Ok(Message::Cargo(CargoEvent::Log { level, line })) => out.log(level, "tool", &line),
            Ok(Message::Cargo(CargoEvent::Diagnostic(diagnostic))) => out.rust(root, &diagnostic),
            Ok(Message::Done(result)) => break result,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Err(BuildFailure::Cancelled),
        }
    };
    let _ = worker.join();
    match result {
        Err(_) if cancelling => Err(BuildFailure::Cancelled),
        result => result,
    }
}

/// Reports why the build gave nothing, and returns the exit code; a
/// package's own steps fail as delivery does when `packaging`.
fn failed(out: &mut Output, failure: &BuildFailure, packaging: bool) -> u8 {
    let code = match failure {
        BuildFailure::Package(_) if packaging => PACKAGE_ARCHIVE,
        BuildFailure::Sign(_) => PACKAGE_SIGNING,
        BuildFailure::Cancelled => return INTERRUPTED,
        BuildFailure::Spawn(_) => ENV_CARGO,
        BuildFailure::Failed { .. } => BUILD_FAILED,
        BuildFailure::Executables(_) => ENV_NO_EXECUTABLE,
        BuildFailure::Package(_) => BUILD_PACKAGE,
        BuildFailure::Tool(_) => ENV_TOOL,
        BuildFailure::Write(_) => ENV_BUILD_WRITE,
        BuildFailure::Lock(_) => ENV_LOCK_HELD,
    };
    let notes: Vec<String> = match failure {
        BuildFailure::Executables(executables) => executables
            .iter()
            .map(|exe| format!("built `{}`", exe.display()))
            .collect(),
        _ => Vec::new(),
    };
    out.failure(code, &failure.message(), &notes)
}

/// What `artifact.json` records of a build or a package.
pub(super) struct Described<'a> {
    pub(super) project: &'a str,
    pub(super) version: Option<&'a str>,
    pub(super) target: &'a str,
    pub(super) profile: &'a str,
    pub(super) build_id: &'a str,
    /// A package's build number.
    pub(super) build: Option<u32>,
    pub(super) toolchain: &'a str,
    pub(super) signing: Signed<'a>,
    /// The directory it lies in, which its paths are relative to.
    pub(super) dist: &'a Path,
    pub(super) artifacts: &'a [Written],
}

/// How it is signed: a package's policy and identity, and the status.
pub(super) struct Signed<'a> {
    pub(super) policy: Option<&'a str>,
    pub(super) status: &'a str,
    pub(super) identity: Option<String>,
}

/// `artifact.json`: the build, and each artifact beside it with its size
/// and hash, by its path relative to the manifest's directory.
pub(super) fn manifest_json(described: &Described<'_>) -> String {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.name("project");
    w.string(described.project);
    w.name("version");
    match described.version {
        Some(version) => w.string(version),
        None => w.null(),
    }
    if let Some(build) = described.build {
        w.name("build");
        w.uint(u64::from(build));
    }
    for (name, value) in [
        ("target", described.target),
        ("profile", described.profile),
        ("build_id", described.build_id),
        ("toolchain", described.toolchain),
    ] {
        w.name(name);
        w.string(value);
    }
    let signed = &described.signing;
    w.name("signing");
    w.begin_object();
    if let Some(policy) = signed.policy {
        w.name("policy");
        w.string(policy);
    }
    w.name("status");
    w.string(signed.status);
    if let Some(identity) = &signed.identity {
        w.name("identity");
        w.string(identity);
    }
    w.end_object();
    w.name("artifacts");
    w.begin_array();
    for artifact in described.artifacts {
        w.begin_object();
        w.name("kind");
        w.string(artifact.kind);
        w.name("path");
        w.string(&relative_name(described.dist, &artifact.path, false));
        w.name("size");
        w.uint(artifact.size);
        w.name("hash");
        w.string(&artifact.hash);
        w.end_object();
    }
    w.end_array();
    w.end_object();
    let mut text = w.into_string();
    text.push('\n');
    text
}

/// `path` relative to `base`, `/`-separated, with a trailing `/` for a
/// directory when `directory`.
pub(super) fn relative_name(base: &Path, path: &Path, directory: bool) -> String {
    let relative = path.strip_prefix(base).unwrap_or(path);
    let mut name = relative
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if directory {
        name.push('/');
    }
    name
}

/// A duration as seconds to a tenth.
pub(super) fn seconds(duration: Duration) -> String {
    format!("{:.1} s", duration.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn the_manifest_names_the_build_and_its_artifacts() {
        let dist = PathBuf::from("/p/dist/web");
        let artifacts = [Written {
            kind: "wasm",
            path: dist.join("app_bg.wasm"),
            size: 12,
            hash: "sha256:00".into(),
        }];
        let text = manifest_json(&Described {
            project: "app",
            version: None,
            target: "web-gpu",
            profile: "shipping",
            build_id: "ab",
            build: None,
            toolchain: "rustc 1.90.0",
            signing: Signed {
                policy: None,
                status: "none",
                identity: None,
            },
            dist: &dist,
            artifacts: &artifacts,
        });
        assert_eq!(
            text,
            "{\"project\":\"app\",\"version\":null,\"target\":\"web-gpu\",\"profile\":\"shipping\",\
             \"build_id\":\"ab\",\"toolchain\":\"rustc 1.90.0\",\"signing\":{\"status\":\"none\"},\"artifacts\":\
             [{\"kind\":\"wasm\",\"path\":\"app_bg.wasm\",\"size\":12,\"hash\":\"sha256:00\"}]}\n"
        );
    }

    #[test]
    fn a_conflicting_profile_names_the_command_it_meant() {
        assert_eq!(
            command_line(Target::Host, Profile::Shipping),
            "viso build --profile shipping"
        );
        assert_eq!(
            command_line(Target::WebGpu, Profile::Dev),
            "viso build web-gpu --profile dev"
        );
    }
}
