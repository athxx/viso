//! `viso build [<target>]`: check the project's sources, compile the app for
//! the target with the profile (`--profile`, `--release`, `VISO_PROFILE`,
//! `dev` when none names one), package it for the target, and publish it to
//! `dist/<platform>/` with its `artifact.json`, without running it.
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
use viso_package::android::Sdk;
use viso_project::{ArtifactKind, LockKind, Profile, ProjectFingerprint, Target};

use super::check;
use crate::args::{BuildArgs, BuildTarget};
use crate::build::cargo::{self, BuildCommand, BuildFailure, CargoEvent, Sink};
use crate::build::{self, Job, Staged, Written};
use crate::context::{Context, Loaded, in_its_file};
use crate::deploy;
use crate::dev::cargo::RustScope;
use crate::error::{
    BUILD_FAILED, BUILD_PACKAGE, CLI_USAGE, ENV_BUILD_WRITE, ENV_CARGO, ENV_CARGO_MANIFEST,
    ENV_DEVICE, ENV_LOCK_HELD, ENV_NO_EXECUTABLE, ENV_TOOL, INTERRUPTED, SUCCESS,
};
use crate::interrupt;
use crate::output::{Artifact, Output, human};

/// How long the command waits for the build between checks for Ctrl-C.
const POLL: Duration = Duration::from_millis(50);

/// What the build's thread hands the command.
enum Message {
    Cargo(CargoEvent),
    Done(Result<Staged, BuildFailure>),
}

pub fn run(ctx: &Context<'_>, args: &BuildArgs, out: &mut Output) -> u8 {
    let target = args.target.map_or(Target::Host, BuildTarget::target);
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
    if let Err(diagnostic) = target.require_available() {
        out.config(None, &diagnostic);
        return diagnostic.code.exit_code();
    }
    if target == Target::Ios && !cfg!(target_os = "macos") {
        return out.failure(ENV_DEVICE, "the iOS Simulator SDK is on macOS only", &[]);
    }
    let Loaded {
        project,
        resolved,
        layout,
    } = match ctx.load_as(target, ArtifactKind::Build, profile, out) {
        Ok(loaded) => loaded,
        Err(code) => return code,
    };
    let config = resolved.config;
    let profile = config.profile.value;

    // Nothing is compiled from sources that do not check.
    let checked = match check::package(project, out) {
        Ok(checked) => checked,
        Err(code) => return code,
    };
    let code = checked.code(out);
    if code != SUCCESS {
        return code;
    }
    let project = checked.project;
    let root = project.root.clone();
    if !root.join("Cargo.toml").is_file() {
        return out.failure(
            ENV_CARGO_MANIFEST,
            &format!(
                "no `Cargo.toml` next to `{}`",
                project.manifest_path().display()
            ),
            &["`viso build` builds the Cargo package at the project root".to_owned()],
        );
    }
    let sdk = match target {
        Target::Android => match Sdk::locate() {
            Ok(sdk) => Some(sdk),
            Err(why) => return out.failure(ENV_DEVICE, &why, &[]),
        },
        _ => None,
    };

    let _lock = match ctx.lock(&layout, LockKind::BuildCache { target }, out) {
        Ok(lock) => lock,
        Err(code) => return code,
    };
    let fingerprint = match ProjectFingerprint::compute(&root) {
        Ok(fingerprint) => fingerprint,
        Err(diagnostic) => {
            in_its_file(out, &diagnostic);
            return diagnostic.code.exit_code();
        }
    };
    let toolchain = build::toolchain();
    let identity = config.build_id(fingerprint, &toolchain);
    let build_id = identity.to_hex();
    let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
    let manifest = canonical.join("Cargo.toml");
    let plan = build::rust_plan(target, profile, sdk.as_ref());
    let android = match sdk {
        Some(sdk) => {
            let scope = match RustScope::read(&manifest, plan.target.as_deref(), &ctx.cargo_args())
            {
                Ok(scope) => scope,
                Err(why) => return out.failure(ENV_CARGO, &why, &[]),
            };
            match deploy::android_shell(&scope.linked) {
                Ok(shell) => Some((sdk, shell)),
                Err(why) => return out.failure(ENV_DEVICE, &why, &[]),
            }
        }
        None => None,
    };
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
            profile_args: cargo::profile_args(profile, &config.settings, target.is_web()),
            hot_reload: config.dev_runtime() == viso_project::DevRuntime::Present,
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
    };
    out.verbose(|| job.cargo.describe());
    out.progress("build", &format!("building {target} ({profile})"));

    interrupt::catch();
    let started = Instant::now();
    let staging = job.dir.join("out");
    let staged = match produce(job, &canonical, out) {
        Ok(staged) => staged,
        Err(failure) => {
            // A failed or cancelled build leaves no output that looks whole.
            let _ = std::fs::remove_dir_all(&staging);
            return failed(out, &failure);
        }
    };
    if interrupt::count() > 0 {
        return INTERRUPTED;
    }

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
    let written = match build::publish(&staged, &dist, |artifacts| {
        manifest_json(&Described {
            project: &name,
            version: version.as_deref(),
            target: &platform,
            profile: profile.as_str(),
            build_id: &build_id,
            toolchain: &toolchain.rustc,
            signing: staged.signing,
            dist: &dist,
            artifacts,
        })
    }) {
        Ok(written) => written,
        Err(why) => return out.failure(ENV_BUILD_WRITE, &why, &[]),
    };
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
        println!("Built {target} ({profile}) in {}", seconds(elapsed));
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
                profile: profile.as_str(),
                build_id: &build_id,
                size: artifact.size,
                hash: &artifact.hash,
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

/// `viso build` of `target` with `profile`, as typed.
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

/// Reports why the build gave nothing, and returns the exit code.
fn failed(out: &mut Output, failure: &BuildFailure) -> u8 {
    let code = match failure {
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

/// What `artifact.json` records of a build.
struct Described<'a> {
    project: &'a str,
    version: Option<&'a str>,
    target: &'a str,
    profile: &'a str,
    build_id: &'a str,
    toolchain: &'a str,
    signing: &'a str,
    /// The directory it lies in, which its paths are relative to.
    dist: &'a Path,
    artifacts: &'a [Written],
}

/// `artifact.json`: the build, and each artifact beside it with its size
/// and hash, by its path relative to the manifest's directory.
fn manifest_json(described: &Described<'_>) -> String {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.name("project");
    w.string(described.project);
    w.name("version");
    match described.version {
        Some(version) => w.string(version),
        None => w.null(),
    }
    for (name, value) in [
        ("target", described.target),
        ("profile", described.profile),
        ("build_id", described.build_id),
        ("toolchain", described.toolchain),
        ("signing", described.signing),
    ] {
        w.name(name);
        w.string(value);
    }
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
fn relative_name(base: &Path, path: &Path, directory: bool) -> String {
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
fn seconds(duration: Duration) -> String {
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
            toolchain: "rustc 1.90.0",
            signing: "none",
            dist: &dist,
            artifacts: &artifacts,
        });
        assert_eq!(
            text,
            "{\"project\":\"app\",\"version\":null,\"target\":\"web-gpu\",\"profile\":\"shipping\",\
             \"build_id\":\"ab\",\"toolchain\":\"rustc 1.90.0\",\"signing\":\"none\",\"artifacts\":\
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
