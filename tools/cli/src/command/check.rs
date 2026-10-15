//! `viso check [<target>]`: locate the project, read its manifest, and
//! compile the package's `.vs` sources through parse, resolution and type
//! checking, with no build output.
//!
//! A named target is checked as a build of it would be: its configuration
//! resolved, its availability required, and the sources held to the input
//! its devices have. `--cargo` checks the Cargo package's Rust code for the
//! target too, with `cargo check`. `--watch` checks again after every
//! change to the sources (and to the Rust code under `--cargo`), each round
//! reporting every diagnostic and ending in `progress{phase:"idle"}`; Ctrl-C
//! ends it, and the summary, with the last round's counts.

use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use viso_dsl::hir::{CapabilitySet, CollisionDelivery, Determinism, InputDevices, TargetProfile};
use viso_dsl::i18n::{CatalogIssue, Messages};
use viso_dsl::package::{LoadedPackage, PackageManifest, load_package};
use viso_dsl::{Diagnostic, TextRange, TextSize};
use viso_package::android::Sdk;
use viso_project::{
    ArtifactKind, DevRuntime, GameDeterminism, Project, ResolvedConfig, Span, Target,
};

use crate::args::{BuildTarget, CheckArgs, Strictness};
use crate::build;
use crate::build::cargo::{self, BuildCommand, BuildFailure, CargoEvent, Sink};
use crate::context::{Context, Loaded};
use crate::dev::cargo::RustScope;
use crate::dev::watch::{WatchScope, Watcher};
use crate::error::{
    BUILD_FAILED, DIAGNOSTICS, ENV_CARGO, ENV_CARGO_MANIFEST, ENV_SOURCE_UNREADABLE, ENVIRONMENT,
    INTERRUPTED, SUCCESS,
};
use crate::interrupt;
use crate::output::{Output, Source};

/// How often a wait checks for Ctrl-C.
const POLL: Duration = Duration::from_millis(50);

/// How long the changes of one edit may keep arriving before the sources
/// are checked again: a save that writes several files checks once.
const SETTLE: Duration = Duration::from_millis(80);

/// What the sources are checked for.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Options {
    /// The target, `None` for the host and the targets `Viso.toml`
    /// configures.
    pub target: Option<Target>,
    /// Whether accessibility findings are errors.
    pub a11y_strict: bool,
    /// Whether localization findings are errors, literal text included.
    pub i18n_strict: bool,
}

impl Options {
    /// The sources checked for a build of `target`.
    pub fn target(target: Target) -> Options {
        Options {
            target: Some(target),
            ..Options::default()
        }
    }
}

pub fn run(ctx: &Context<'_>, args: &CheckArgs, out: &mut Output) -> u8 {
    let options = Options {
        target: args.target.map(BuildTarget::target),
        a11y_strict: args.a11y == Some(Strictness::Strict),
        i18n_strict: args.i18n == Some(Strictness::Strict),
    };
    if args.watch {
        watch(ctx, args.cargo, &options, out)
    } else {
        once(ctx, args.cargo, &options, out)
    }
}

/// One check, with the Rust code under `cargo`; its exit code.
fn once(ctx: &Context<'_>, cargo: bool, options: &Options, out: &mut Output) -> u8 {
    // A named target, or the Rust code, is checked as a build of it would
    // be configured.
    let (project, config) = if cargo || options.target.is_some() {
        let target = options.target.unwrap_or(Target::Host);
        match ctx.load_as(target, ArtifactKind::Build, None, out) {
            Ok(Loaded {
                project, resolved, ..
            }) => (project, Some(resolved.config)),
            Err(code) => return code,
        }
    } else {
        match ctx.project(out) {
            Ok(project) => (project, None),
            Err(code) => return code,
        }
    };
    let checked = match package_for(project, options, out) {
        Ok(checked) => checked,
        Err(code) => return code,
    };
    out.checked(&checked.name, checked.package.files.len());
    let code = checked.code(out);
    match config {
        Some(config) if cargo => code.max(rust(ctx, &checked.project, &config, out)),
        _ => code,
    }
}

/// Checks the Cargo package's Rust code with `cargo check`, for the target
/// and with the features `config` builds it with; what the compiler says is
/// reported as it says it. `BUILD_FAILED` when it does not check.
fn rust(ctx: &Context<'_>, project: &Project, config: &ResolvedConfig, out: &mut Output) -> u8 {
    let root = project
        .root
        .canonicalize()
        .unwrap_or_else(|_| project.root.clone());
    let manifest = root.join("Cargo.toml");
    if !manifest.is_file() {
        return out.failure(
            ENV_CARGO_MANIFEST,
            "no `Cargo.toml` at the project root",
            &["`viso check --cargo` checks the Cargo package at the project root".to_owned()],
        );
    }
    let target = config.target;
    let profile = config.profile.value;
    // The NDK's compilers serve a build script that compiles C; without
    // them only such a script fails.
    let sdk = (target == Target::Android)
        .then(|| Sdk::locate().ok())
        .flatten();
    let plan = build::rust_plan(target, profile, sdk.as_ref());
    let mut features = plan.features;
    if config.dev_runtime() == DevRuntime::Present && target != Target::Headless {
        features.push(cargo::HOT_RELOAD);
    }
    let command = BuildCommand {
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
        target: plan.target,
        rustc_args: Vec::new(),
        env: plan.env,
        package: None,
        lock: None,
    };
    out.verbose(|| command.describe());
    out.progress("check", "checking the Cargo package");

    interrupt::catch();
    let (tx, rx) = mpsc::channel();
    let (cancel, cancelled) = mpsc::channel();
    let events = tx.clone();
    let worker = thread::Builder::new()
        .name("viso-check".into())
        .spawn(move || {
            let sink: Sink = Arc::new(move |event| events.send(Some(event)).is_ok());
            let result = cargo::check(&command, &cancelled, &sink);
            let _ = tx.send(None);
            result
        })
        .expect("a check thread");
    let mut cancelling = false;
    loop {
        if !cancelling && interrupt::count() > 0 {
            cancelling = true;
            let _ = cancel.send(());
        }
        match rx.recv_timeout(POLL) {
            Ok(Some(CargoEvent::Log { level, line })) => out.log(level, "tool", &line),
            Ok(Some(CargoEvent::Diagnostic(diagnostic))) => out.rust(&root, &diagnostic),
            Ok(None) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
    let result = worker.join().unwrap_or(Err(BuildFailure::Cancelled));
    match result {
        _ if cancelling => INTERRUPTED,
        Ok(()) => SUCCESS,
        Err(BuildFailure::Spawn(why)) => out.failure(ENV_CARGO, &why, &[]),
        Err(BuildFailure::Cancelled) => INTERRUPTED,
        Err(BuildFailure::Failed { status, .. }) => out.failure(
            BUILD_FAILED,
            &format!("the Cargo package does not check (`cargo check` {status})"),
            &[],
        ),
        Err(failure) => out.failure(BUILD_FAILED, &failure.message(), &[]),
    }
}

/// Checks, then checks again after every change to what a check reads,
/// until Ctrl-C: 130, with the summary of the last round.
fn watch(ctx: &Context<'_>, cargo: bool, options: &Options, out: &mut Output) -> u8 {
    let project = match ctx.project(out) {
        Ok(project) => project,
        Err(code) => return code,
    };
    let root = project
        .root
        .canonicalize()
        .unwrap_or_else(|_| project.root.clone());
    let rust = if cargo {
        let target = options.target.unwrap_or(Target::Host);
        let triple = build::rust_plan(target, viso_project::Profile::Dev, None).target;
        RustScope::read(
            &root.join("Cargo.toml"),
            triple.as_deref(),
            &ctx.cargo_args(),
        )
        .unwrap_or_default()
    } else {
        RustScope::default()
    };
    interrupt::catch();
    let (tx, rx) = mpsc::channel();
    let _watcher = Watcher::spawn(
        WatchScope {
            project: root.clone(),
            rust,
        },
        move |change| tx.send(change.path).is_ok(),
    );
    // The watcher first hands over every file as it finds it; the first
    // round reads them as they are.
    if settled(&rx, SETTLE).is_err() {
        return INTERRUPTED;
    }
    let mut code;
    loop {
        out.round();
        code = once(ctx, cargo, options, out);
        if interrupt::count() > 0 {
            return INTERRUPTED;
        }
        out.idle("watching for changes; Ctrl-C to stop");
        // The next change, and every other of the same edit.
        let first = loop {
            if interrupt::count() > 0 {
                return INTERRUPTED;
            }
            match rx.recv_timeout(POLL) {
                Ok(path) => break path,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return code,
            }
        };
        let more = match settled(&rx, SETTLE) {
            Ok(more) => more,
            Err(()) => return INTERRUPTED,
        };
        let changed = first.strip_prefix(&root).unwrap_or(&first).display();
        let others = match more {
            0 => String::new(),
            1 => " and 1 other file".to_owned(),
            n => format!(" and {n} other files"),
        };
        out.progress("check", &format!("`{changed}`{others} changed"));
        if out.human() && !ctx.global.quiet {
            eprintln!("`{changed}`{others} changed; checking again");
        }
    }
}

/// Takes the changes arriving until none has for `quiet`, and how many;
/// `Err` on Ctrl-C.
fn settled(rx: &mpsc::Receiver<std::path::PathBuf>, quiet: Duration) -> Result<usize, ()> {
    let mut taken = 0;
    let mut until = Instant::now() + quiet;
    loop {
        if interrupt::count() > 0 {
            return Err(());
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(taken);
        }
        match rx.recv_timeout(left.min(POLL)) {
            Ok(_) => {
                taken += 1;
                until = Instant::now() + quiet;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(taken),
        }
    }
}

/// A located project and its package, loaded and checked.
pub(super) struct Checked {
    pub project: Project,
    pub name: String,
    pub package: LoadedPackage,
    /// The build the package was checked for.
    pub profile: TargetProfile,
}

impl Checked {
    /// The exit code of the check: a source that could not be read leaves it
    /// incomplete, an environment failure whatever the rest of the package
    /// says.
    pub fn code(&self, out: &Output) -> u8 {
        if !self.package.unreadable.is_empty() {
            ENVIRONMENT
        } else if out.errors() > 0 {
            DIAGNOSTICS
        } else {
            SUCCESS
        }
    }
}

/// Locates the project, reads its manifest and loads its package, reporting
/// every diagnostic on the way; the exit code when the project did not load.
pub(super) fn load(ctx: &Context<'_>, out: &mut Output) -> Result<Checked, u8> {
    package_for(ctx.project(out)?, &Options::default(), out)
}

/// Loads and checks the package of `project`, located already, for
/// `options`, reporting every diagnostic on the way; the exit code when it
/// did not load.
pub(super) fn package_for(
    project: Project,
    options: &Options,
    out: &mut Output,
) -> Result<Checked, u8> {
    // The manifest parsed a moment ago; its text is read again only to show the
    // lines its diagnostics point at, so a failed read just drops the snippets.
    let manifest_path = project.manifest_path();
    let manifest_text = fs::read_to_string(&manifest_path).ok();
    let manifest = manifest_text
        .as_deref()
        .map(|text| Source::new(&manifest_path, &project.root, text));
    let name = match project.name() {
        Ok(name) => name.to_owned(),
        Err(diagnostic) => {
            out.config(manifest.as_ref(), &diagnostic);
            return Err(diagnostic.code.exit_code());
        }
    };

    // The message catalogs, which `tr` and `MessageKey` check against.
    let source_locale = project
        .manifest
        .i18n
        .source
        .as_ref()
        .map_or(viso_dsl::i18n::DEFAULT_SOURCE, |source| {
            source.value.as_str()
        });
    let catalog_dir = project.root.join(viso_dsl::i18n::CATALOG_DIR);
    let catalogs = match Messages::read_dir(&catalog_dir) {
        Ok(files) => files,
        Err(error) => {
            return Err(out.failure(
                ENV_SOURCE_UNREADABLE,
                &format!(
                    "cannot read `{}`: {error}",
                    relative(&project.root, &catalog_dir).display()
                ),
                &[],
            ));
        }
    };
    let messages = (!catalogs.is_empty()).then(|| Messages::compile(source_locale, &catalogs));
    if let Some(messages) = &messages {
        for issue in messages.issues() {
            let file = &catalogs[issue.file];
            let path = Path::new(&file.path);
            let source = Source::new(path, &project.root, &file.text);
            let range = TextRange::new(
                TextSize::from(issue.range.start as u32),
                TextSize::from(issue.range.end as u32),
            );
            let diagnostic = if issue.error {
                Diagnostic::error(CatalogIssue::CODE, range, issue.message.clone())
            } else {
                Diagnostic::warning(CatalogIssue::CODE, range, issue.message.clone())
            };
            out.source(Some(&source), &[], &diagnostic);
        }
    }
    // The assets, which `asset("…")` checks against.
    let asset_dir = project.root.join(viso_dsl::assets::ASSET_DIR);
    let assets = match viso_dsl::assets::AssetCatalog::scan(&asset_dir) {
        Ok(assets) => assets,
        Err(error) => {
            return Err(out.failure(
                ENV_SOURCE_UNREADABLE,
                &format!(
                    "cannot read `{}`: {error}",
                    relative(&project.root, &asset_dir).display()
                ),
                &[],
            ));
        }
    };
    let mut profile = profile(&project, options);
    profile.messages = messages.map(Rc::new);
    profile.assets = Some(Rc::new(assets));

    let package = load_package(
        &project.root,
        PackageManifest {
            name: &name,
            profile: profile.clone(),
            language: project
                .manifest
                .package
                .language
                .as_ref()
                .map(|language| (language.value.as_str(), text_range(language.span))),
        },
    );
    for diagnostic in &package.manifest_diagnostics {
        out.source(manifest.as_ref(), &[], diagnostic);
    }
    for diagnostic in &package.graph_diagnostics {
        out.source(None, &[], diagnostic);
    }
    let sources: Vec<Source<'_>> = package
        .files
        .iter()
        .map(|file| {
            Source::new(&file.path, &project.root, &file.source).of_module(file.module.join("::"))
        })
        .collect();
    for (file, source) in package.files.iter().zip(&sources) {
        for diagnostic in &file.diagnostics {
            out.source(Some(source), &sources, diagnostic);
        }
    }
    for (path, error) in &package.unreadable {
        out.failure(
            ENV_SOURCE_UNREADABLE,
            &format!(
                "cannot read `{}`: {error}",
                relative(&project.root, path).display()
            ),
            &[],
        );
    }
    Ok(Checked {
        project,
        name,
        package,
        profile,
    })
}

/// What the project's targets are and how it is built, checked for
/// `options`.
pub(super) fn profile(project: &Project, options: &Options) -> TargetProfile {
    TargetProfile {
        devices: devices(project, options.target),
        determinism: match project.manifest.game.determinism.map(|d| d.value) {
            Some(GameDeterminism::CrossPlatform) => Determinism::CrossPlatform,
            Some(GameDeterminism::SameBinary) | None => Determinism::SameBinary,
        },
        tick_rate: project
            .manifest
            .game
            .tick_rate
            .map_or(TargetProfile::default().tick_rate, |rate| rate.value),
        collision_delivery: match project.manifest.game.collision_delivery.map(|d| d.value) {
            Some(viso_project::CollisionDelivery::ListenerMajor) => {
                CollisionDelivery::ListenerMajor
            }
            Some(viso_project::CollisionDelivery::EventMajor) | None => {
                CollisionDelivery::EventMajor
            }
        },
        release: false,
        capabilities: {
            let mut granted = CapabilitySet::new();
            for capability in &project.manifest.package.capabilities {
                granted.insert(capability.value.as_str());
            }
            granted
        },
        a11y_strict: options.a11y_strict,
        i18n_strict: options.i18n_strict,
        messages: None,
        assets: None,
        shader_langs: TargetProfile::default().shader_langs,
    }
}

/// The input devices of `target`, which each game input action needs a
/// binding for: a desktop host's keyboard, mouse and gamepads, a phone's or
/// a browser's touch besides, and a headless run's keyboard and pointer
/// alone. Without a target, the host's and those of the mobile targets
/// `Viso.toml` configures.
fn devices(project: &Project, target: Option<Target>) -> InputDevices {
    match target {
        None => InputDevices {
            gamepad: true,
            touch: project.manifest.targets != Default::default(),
        },
        Some(Target::Host) => InputDevices {
            gamepad: true,
            touch: false,
        },
        Some(Target::Headless) => InputDevices {
            gamepad: false,
            touch: false,
        },
        Some(
            Target::Ios | Target::Android | Target::WebGpu | Target::WebDom | Target::WebHybrid,
        ) => InputDevices {
            gamepad: true,
            touch: true,
        },
    }
}

/// A manifest span as the compiler's range into `Viso.toml`.
fn text_range(span: Span) -> TextRange {
    TextRange::new(TextSize::new(span.start), TextSize::new(span.end))
}

fn relative<'a>(root: &Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}
