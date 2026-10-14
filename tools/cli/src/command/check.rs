//! `viso check`: locate the project, read its manifest,
//! and compile the package's `.vs` sources through parse, resolution and type
//! checking, with no build output.

use std::fs;
use std::path::Path;
use std::rc::Rc;

use viso_dsl::hir::{CapabilitySet, CollisionDelivery, Determinism, InputDevices, TargetProfile};
use viso_dsl::i18n::{CatalogIssue, Messages};
use viso_dsl::package::{LoadedPackage, PackageManifest, load_package};
use viso_dsl::{Diagnostic, TextRange, TextSize};
use viso_project::{GameDeterminism, Project, Span};

use crate::context::Context;
use crate::error::{DIAGNOSTICS, ENV_SOURCE_UNREADABLE, ENVIRONMENT, SUCCESS};
use crate::output::{Output, Source};

pub fn run(ctx: &Context<'_>, out: &mut Output) -> u8 {
    let checked = match load(ctx, out) {
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
pub(super) fn load(ctx: &Context<'_>, out: &mut Output) -> Result<Checked, u8> {
    package(ctx.project(out)?, out)
}

/// Loads and checks the package of `project`, located already, reporting
/// every diagnostic on the way; the exit code when it did not load.
pub(super) fn package(project: Project, out: &mut Output) -> Result<Checked, u8> {
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
    let mut profile = profile(&project);
    profile.messages = messages.map(Rc::new);
    profile.assets = Some(Rc::new(assets));

    let package = load_package(
        &project.root,
        PackageManifest {
            name: &name,
            profile,
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
        // `--a11y strict` and `--i18n strict` arrive with the CLI's check
        // options.
        a11y_strict: false,
        i18n_strict: false,
        messages: None,
        assets: None,
        shader_langs: TargetProfile::default().shader_langs,
    }
}

/// A manifest span as the compiler's range into `Viso.toml`.
fn text_range(span: Span) -> TextRange {
    TextRange::new(TextSize::new(span.start), TextSize::new(span.end))
}

fn relative<'a>(root: &Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}
