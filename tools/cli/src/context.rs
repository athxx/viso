//! What one invocation knows before any command runs: the global options and
//! the `VISO_*` environment, captured once, and the project they lead to.
//!
//! Every command reaches the project, its configuration and its paths through
//! a [`Context`], so `--project`, `--profile`, `--target-dir` and the
//! environment mean the same thing to each of them, and a diagnostic on the way
//! is reported once, in its file.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use viso_project::{
    ArtifactKind, ConfigDiagnostic, Env, Layout, Overrides, Profile, Project, Resolved, Target,
};

use crate::args::{ColorChoice, Global};
use crate::error::{CLI_USAGE, DIAGNOSTICS, ENV_CURRENT_DIR};
use crate::output::{Output, Source};

/// One invocation's options and environment.
pub struct Context<'a> {
    /// The global options.
    pub global: &'a Global,
    env: Env,
}

/// A located project and its resolved configuration.
pub struct Loaded {
    pub project: Project,
    pub resolved: Resolved,
    /// Where its output goes.
    pub layout: Layout,
}

impl<'a> Context<'a> {
    /// The context of a command run with `global`, in this process's
    /// environment.
    pub fn new(global: &'a Global) -> Self {
        Self {
            global,
            env: Env::from_process(),
        }
    }

    /// The `VISO_*` environment.
    pub fn env(&self) -> &Env {
        &self.env
    }

    /// The current directory.
    pub fn cwd(&self, out: &mut Output) -> Result<PathBuf, u8> {
        std::env::current_dir().map_err(|error| {
            out.failure(
                ENV_CURRENT_DIR,
                &format!("cannot read the current directory: {error}"),
                &[],
            )
        })
    }

    /// The project `--project` names or the current directory lies in, its
    /// warnings reported.
    pub fn project(&self, out: &mut Output) -> Result<Project, u8> {
        let cwd = self.cwd(out)?;
        let project = Project::locate(self.global.project.as_deref(), &cwd)
            .map_err(|diagnostics| report_all(out, &diagnostics))?;
        for warning in &project.warnings {
            in_its_file(out, warning);
        }
        Ok(project)
    }

    /// The project and its configuration for a build for `target`, as
    /// `--profile` and the environment ask.
    pub fn load(
        &self,
        target: Target,
        artifact: ArtifactKind,
        out: &mut Output,
    ) -> Result<Loaded, u8> {
        let flags = Overrides {
            profile: self.global.profile.map(|p| p.profile()),
        };
        self.load_with(target, artifact, &flags, out)
    }

    /// As [`load`](Self::load), for a command that only ever builds the dev
    /// profile: another `--profile` is a usage error, and the environment's
    /// is not consulted.
    pub fn load_dev(&self, command: &str, target: Target, out: &mut Output) -> Result<Loaded, u8> {
        self.dev_only(command, out)?;
        let flags = Overrides {
            profile: Some(Profile::Dev),
        };
        self.load_with(target, ArtifactKind::Build, &flags, out)
    }

    /// Fails unless `--profile` is absent or `dev`, for a command that builds
    /// only the dev profile.
    pub fn dev_only(&self, command: &str, out: &mut Output) -> Result<(), u8> {
        match self.global.profile.map(|p| p.profile()) {
            None | Some(Profile::Dev) => Ok(()),
            Some(profile) => Err(out.failure(
                CLI_USAGE,
                &format!("`viso {command}` builds the dev profile, not `{profile}`"),
                &[format!(
                    "a {profile} artifact is built by `viso build --profile {profile}`"
                )],
            )),
        }
    }

    fn load_with(
        &self,
        target: Target,
        artifact: ArtifactKind,
        flags: &Overrides,
        out: &mut Output,
    ) -> Result<Loaded, u8> {
        let cwd = self.cwd(out)?;
        let project = Project::locate(self.global.project.as_deref(), &cwd)
            .map_err(|diagnostics| report_all(out, &diagnostics))?;
        let resolved = viso_project::resolve(&project, target, artifact, flags, &self.env)
            .map_err(|diagnostics| report_all(out, &diagnostics))?;
        for warning in &resolved.warnings {
            in_its_file(out, warning);
        }
        let config = &resolved.config;
        out.verbose(|| {
            let s = &config.settings;
            format!(
                "{target}: profile {} ({}), opt_level {} ({}), source_maps {} ({}), strip {} ({})",
                config.profile.value,
                config.profile.origin,
                s.opt_level.value,
                s.opt_level.origin,
                s.source_maps.value,
                s.source_maps.origin,
                s.strip.value,
                s.strip.origin,
            )
        });
        let layout = self.layout(&project.root);
        Ok(Loaded {
            project,
            resolved,
            layout,
        })
    }

    /// The paths of the project at `root`, `target/` where `--target-dir`
    /// puts it.
    pub fn layout(&self, root: &Path) -> Layout {
        let layout = Layout::new(root);
        match self.target_dir() {
            Some(dir) => layout.with_target_dir(dir),
            None => layout,
        }
    }

    /// `--target-dir`, absolute: a relative one is taken from the current
    /// directory, as Cargo takes it.
    pub fn target_dir(&self) -> Option<PathBuf> {
        let dir = self.global.target_dir.as_deref()?;
        Some(match std::env::current_dir() {
            Ok(cwd) if dir.is_relative() => cwd.join(dir),
            _ => dir.to_owned(),
        })
    }

    /// The options every Cargo command takes that the global options ask
    /// for: `--offline` and `--locked`.
    pub fn cargo_args(&self) -> Vec<OsString> {
        let g = self.global;
        let mut args: Vec<OsString> = Vec::new();
        if g.offline {
            args.push("--offline".into());
        }
        if g.locked {
            args.push("--locked".into());
        }
        args
    }

    /// The options of `cargo build` that the global options ask for besides
    /// [`cargo_args`](Self::cargo_args): `--jobs` and `--color`.
    pub fn cargo_build_args(&self) -> Vec<OsString> {
        let g = self.global;
        let mut args: Vec<OsString> = Vec::new();
        if let Some(jobs) = g.jobs {
            args.push("--jobs".into());
            args.push(jobs.to_string().into());
        }
        // Cargo's own output goes to the terminal in human mode; under
        // `--json` it is read as lines, which carry no color.
        let color = match (g.json, g.color) {
            (true, _) | (_, ColorChoice::Never) => "never",
            (false, ColorChoice::Always) => "always",
            (false, ColorChoice::Auto) => "auto",
        };
        args.push("--color".into());
        args.push(color.into());
        args
    }
}

/// Reports every diagnostic of a project that did not load, and returns the
/// exit code of the first error.
fn report_all(out: &mut Output, diagnostics: &[ConfigDiagnostic]) -> u8 {
    for diagnostic in diagnostics {
        in_its_file(out, diagnostic);
    }
    failure_code(diagnostics)
}

/// Reports a project diagnostic, reading the file it points into so the
/// report can show the line.
pub fn in_its_file(out: &mut Output, diagnostic: &ConfigDiagnostic) {
    if let (Some(path), Some(_)) = (&diagnostic.path, diagnostic.span)
        && let Ok(text) = fs::read_to_string(path)
    {
        let root = path.parent().unwrap_or(Path::new(""));
        out.config(Some(&Source::new(path, root, &text)), diagnostic);
    } else {
        out.config(None, diagnostic);
    }
}

/// The exit code of the first error that stopped the command.
pub fn failure_code(diagnostics: &[ConfigDiagnostic]) -> u8 {
    diagnostics
        .iter()
        .find(|d| d.is_error())
        .map_or(DIAGNOSTICS, |d| d.code.exit_code())
}
