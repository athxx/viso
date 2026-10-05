//! `viso check` (`Viso_CLI.md` section 17): locate the project, read its manifest,
//! and compile the package's `.vs` sources through parse, resolution and type
//! checking, with no build output.

use std::fs;
use std::path::Path;

use viso_dsl::hir::{CapabilitySet, Determinism, InputDevices, TargetProfile};
use viso_dsl::package::{LoadedPackage, PackageManifest, load_package};
use viso_dsl::{TextRange, TextSize};
use viso_project::{ConfigDiagnostic, GameDeterminism, Project, Span};

use super::{DIAGNOSTICS, ENV_CURRENT_DIR, ENV_SOURCE_UNREADABLE, ENVIRONMENT, SUCCESS};
use crate::args::Global;
use crate::output::{Output, Source};

pub fn run(global: &Global, out: &mut Output) -> u8 {
    let checked = match load(global, out) {
        Ok(checked) => checked,
        Err(code) => return code,
    };
    out.checked(&checked.name, checked.package.files.len());
    checked.code(out)
}

/// A located project and its package, loaded and checked.
pub(super) struct Checked {
    pub project: Project,
    pub name: String,
    pub package: LoadedPackage,
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
pub(super) fn load(global: &Global, out: &mut Output) -> Result<Checked, u8> {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            out.failure(
                ENV_CURRENT_DIR,
                &format!("cannot read the current directory: {error}"),
                &[],
            );
            return Err(ENVIRONMENT);
        }
    };
    let project = match Project::locate(global.project.as_deref(), &cwd) {
        Ok(project) => project,
        Err(diagnostics) => {
            for diagnostic in &diagnostics {
                config_in_its_file(out, diagnostic);
            }
            return Err(failure_code(&diagnostics));
        }
    };

    // The manifest parsed a moment ago; its text is read again only to show the
    // lines its diagnostics point at, so a failed read just drops the snippets.
    let manifest_path = project.manifest_path();
    let manifest_text = fs::read_to_string(&manifest_path).ok();
    let manifest = manifest_text
        .as_deref()
        .map(|text| Source::new(&manifest_path, &project.root, text));
    for warning in &project.warnings {
        out.config(manifest.as_ref(), warning);
    }
    let name = match project.name() {
        Ok(name) => name.to_owned(),
        Err(diagnostic) => {
            out.config(manifest.as_ref(), &diagnostic);
            return Err(diagnostic.code.exit_code());
        }
    };

    let package = load_package(
        &project.root,
        PackageManifest {
            name: &name,
            profile: profile(&project),
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
    })
}

/// What the project's targets are and how it is built.
pub(super) fn profile(project: &Project) -> TargetProfile {
    TargetProfile {
        // The host is a desktop, which takes gamepads; a mobile target takes
        // touch.
        devices: InputDevices {
            gamepad: true,
            touch: project.manifest.targets != Default::default(),
        },
        determinism: match project.manifest.game.determinism.map(|d| d.value) {
            Some(GameDeterminism::CrossPlatform) => Determinism::CrossPlatform,
            Some(GameDeterminism::SameBinary) | None => Determinism::SameBinary,
        },
        tick_rate: project
            .manifest
            .game
            .tick_rate
            .map_or(TargetProfile::default().tick_rate, |rate| rate.value),
        release: false,
        capabilities: {
            let mut granted = CapabilitySet::new();
            for capability in &project.manifest.package.capabilities {
                granted.insert(capability.value.as_str());
            }
            granted
        },
        // `--a11y strict` and `--i18n strict` arrive with the CLI's check
        // options.
        a11y_strict: false,
        i18n_strict: false,
    }
}

/// Reports a diagnostic from a project that failed to load, reading the file it
/// points into so the report can show the line.
pub(super) fn config_in_its_file(out: &mut Output, diagnostic: &ConfigDiagnostic) {
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
pub(super) fn failure_code(diagnostics: &[ConfigDiagnostic]) -> u8 {
    diagnostics
        .iter()
        .find(|d| d.is_error())
        .map_or(DIAGNOSTICS, |d| d.code.exit_code())
}

/// A manifest span as the compiler's range into `Viso.toml`.
fn text_range(span: Span) -> TextRange {
    TextRange::new(TextSize::new(span.start), TextSize::new(span.end))
}

fn relative<'a>(root: &Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}
