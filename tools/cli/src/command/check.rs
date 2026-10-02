//! `viso check` (`Viso_CLI.md` section 17): locate the project, read its manifest,
//! and compile the package's `.vs` sources through parse, resolution and type
//! checking, with no build output.

use std::fs;
use std::path::Path;

use viso_dsl::hir::InputDevices;
use viso_dsl::package::{PackageManifest, load_package};
use viso_dsl::{TextRange, TextSize};
use viso_project::{ConfigDiagnostic, Project, Span};

use super::{DIAGNOSTICS, ENV_CURRENT_DIR, ENV_SOURCE_UNREADABLE, ENVIRONMENT, SUCCESS};
use crate::args::Global;
use crate::output::{Output, Source};

pub fn run(global: &Global, out: &mut Output) -> u8 {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            out.failure(
                ENV_CURRENT_DIR,
                &format!("cannot read the current directory: {error}"),
                &[],
            );
            return ENVIRONMENT;
        }
    };
    let project = match Project::locate(global.project.as_deref(), &cwd) {
        Ok(project) => project,
        Err(diagnostics) => {
            for diagnostic in &diagnostics {
                config_in_its_file(out, diagnostic);
            }
            return failure_code(&diagnostics);
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
        Ok(name) => name,
        Err(diagnostic) => {
            out.config(manifest.as_ref(), &diagnostic);
            return diagnostic.code.exit_code();
        }
    };

    let package = load_package(
        &project.root,
        PackageManifest {
            name,
            // The host is a desktop, which takes gamepads; a mobile target
            // takes touch.
            devices: InputDevices {
                gamepad: true,
                touch: project.manifest.targets != Default::default(),
            },
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
    out.checked(name, package.files.len());

    // A source that could not be read leaves the check incomplete, which is an
    // environment failure whatever the rest of the package says.
    if !package.unreadable.is_empty() {
        ENVIRONMENT
    } else if out.errors() > 0 {
        DIAGNOSTICS
    } else {
        SUCCESS
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
