//! Package identity for a macro invocation: the crate's manifest directory is the
//! package root, the nearest `Viso.toml` at or above it names the package and may
//! pin the language version, and the source file's path below the root is its
//! module path ([`Origin::for_file`]). Without a `Viso.toml` the Cargo package
//! name stands in and nothing is granted. The message catalogs lie in `i18n/`
//! beside the `Viso.toml`, or below the crate root without one.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use proc_macro2::Span;
use viso_dsl::frontend::Origin;
use viso_dsl::hir::{CapabilitySet, TargetProfile};
use viso_dsl::i18n::{self, CatalogIssue, Messages};

const MANIFEST: &str = "Viso.toml";

/// The crate root the invocation compiles in.
pub fn root() -> syn::Result<PathBuf> {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| syn::Error::new(Span::call_site(), "CARGO_MANIFEST_DIR is not set"))
}

/// The Rust file the macro is invoked from, as an absolute path, when the
/// compiler reports one.
pub fn invoking_file() -> Option<PathBuf> {
    let file = proc_macro::Span::call_site().local_file()?;
    if file.is_absolute() {
        return Some(file);
    }
    Some(std::env::current_dir().ok()?.join(file))
}

/// The origin of `file`, a source file of the crate rooted at `root`.
pub fn origin(root: &Path, file: Option<&Path>) -> syn::Result<Origin> {
    let (package, language) = identity(root)?;
    Ok(match file {
        Some(file) => Origin::for_file(&package, root, file, language),
        None => Origin {
            package,
            module: Vec::new(),
            language,
        },
    })
}

/// The nearest `Viso.toml` at or above `root`, parsed, with its path.
fn manifest(root: &Path) -> syn::Result<Option<(PathBuf, toml_edit::DocumentMut)>> {
    let Some(manifest) = root
        .ancestors()
        .map(|dir| dir.join(MANIFEST))
        .find(|path| path.is_file())
    else {
        return Ok(None);
    };
    let fail = |reason: String| {
        syn::Error::new(
            Span::call_site(),
            format!("`{}`: {reason}", manifest.display()),
        )
    };
    let text = std::fs::read_to_string(&manifest).map_err(|error| fail(error.to_string()))?;
    let document: toml_edit::DocumentMut = text
        .parse()
        .map_err(|error: toml_edit::TomlError| fail(error.message().to_owned()))?;
    Ok(Some((manifest, document)))
}

/// The package name and pinned language version: from the nearest `Viso.toml`
/// at or above `root`, else the Cargo package name and no pin.
fn identity(root: &Path) -> syn::Result<(String, Option<String>)> {
    let cargo_name = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    let Some((_, document)) = manifest(root)? else {
        return Ok((cargo_name, None));
    };
    let key = |name: &str| {
        document
            .get("package")
            .and_then(|package| package.get(name))
            .and_then(|value| value.as_str())
            .map(str::to_owned)
    };
    Ok((key("name").unwrap_or(cargo_name), key("language")))
}

/// A crate's build profile and the message catalogs it was read with.
pub struct Profile {
    pub target: TargetProfile,
    pub catalogs: Option<Catalogs>,
}

/// A package's message catalogs: its source locale, the directory holding
/// them, and each file, which an expansion makes a compile dependency.
pub struct Catalogs {
    pub source: String,
    pub dir: String,
    pub files: Vec<String>,
}

/// The build profile of the crate rooted at `root`: the capabilities its
/// `Viso.toml` grants (`[package] capabilities`), which the module it embeds
/// carries to the host that links it, and the message catalogs beside it
/// (`i18n/`, the source locale `[i18n] source`), which `tr` checks against.
pub fn profile(root: &Path) -> syn::Result<Profile> {
    let mut capabilities = CapabilitySet::new();
    let manifest = manifest(root)?;
    if let Some((path, document)) = &manifest
        && let Some(granted) = document
            .get("package")
            .and_then(|package| package.get("capabilities"))
    {
        let fail = || {
            syn::Error::new(
                Span::call_site(),
                format!(
                    "`{}`: `[package] capabilities` is not an array of strings",
                    path.display()
                ),
            )
        };
        for capability in granted.as_array().ok_or_else(fail)? {
            capabilities.insert(capability.as_str().ok_or_else(fail)?);
        }
    }
    let source = match &manifest {
        Some((path, document)) => match document.get("i18n").and_then(|t| t.get("source")) {
            Some(source) => source.as_str().map(str::to_owned).ok_or_else(|| {
                syn::Error::new(
                    Span::call_site(),
                    format!("`{}`: `[i18n] source` is not a string", path.display()),
                )
            })?,
            None => i18n::DEFAULT_SOURCE.to_owned(),
        },
        None => i18n::DEFAULT_SOURCE.to_owned(),
    };
    let base = manifest
        .as_ref()
        .and_then(|(path, _)| path.parent())
        .unwrap_or(root);
    let dir = base.join(i18n::CATALOG_DIR);
    let files = Messages::read_dir(&dir).map_err(|error| {
        syn::Error::new(
            Span::call_site(),
            format!("cannot read `{}`: {error}", dir.display()),
        )
    })?;
    let (messages, catalogs) = if files.is_empty() {
        (None, None)
    } else {
        let messages = Messages::compile(&source, &files);
        if let Some(issue) = messages.issues().iter().find(|i| i.error) {
            let file = &files[issue.file];
            let before = &file.text[..issue.range.start.min(file.text.len())];
            let line = before.matches('\n').count() + 1;
            let column = before.len() - before.rfind('\n').map_or(0, |at| at + 1) + 1;
            return Err(syn::Error::new(
                Span::call_site(),
                format!(
                    "error[{}]: {}:{line}:{column}: {}",
                    CatalogIssue::CODE,
                    file.path,
                    issue.message
                ),
            ));
        }
        let catalogs = Catalogs {
            source,
            dir: dir.display().to_string(),
            files: files.iter().map(|f| f.path.clone()).collect(),
        };
        (Some(Rc::new(messages)), Some(catalogs))
    };
    Ok(Profile {
        target: TargetProfile {
            capabilities,
            messages,
            ..TargetProfile::default()
        },
        catalogs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(manifest: Option<&str>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "viso-ui-macros-profile-{}-{}",
            std::process::id(),
            manifest.map_or(0, str::len)
        ));
        let crate_dir = dir.join("app");
        std::fs::create_dir_all(&crate_dir).unwrap();
        if let Some(text) = manifest {
            std::fs::write(dir.join(MANIFEST), text).unwrap();
        }
        crate_dir
    }

    #[test]
    fn the_profile_grants_the_nearest_manifests_capabilities() {
        let root = package(Some(
            "[package]\nname = \"app\"\ncapabilities = [\"clipboard.write\", \"network.http\"]\n",
        ));
        let granted: Vec<String> = profile(&root)
            .unwrap()
            .target
            .capabilities
            .iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(granted, ["clipboard.write", "network.http"]);

        let bad = package(Some("[package]\ncapabilities = \"clipboard.write\"\n"));
        assert!(profile(&bad).is_err());
    }

    #[test]
    fn the_profile_compiles_the_catalogs_beside_the_manifest() {
        let root = package(Some("[package]\nname = \"app\"\n[i18n]\nsource = \"fr\"\n"));
        let dir = root.parent().unwrap().join(i18n::CATALOG_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fr.toml"), "titre = \"Boîte\"\n").unwrap();
        let profile = profile(&root).unwrap();
        let messages = profile.target.messages.expect("catalogs");
        assert_eq!(messages.source(), "fr");
        assert!(messages.message("titre").is_some());
        let catalogs = profile.catalogs.expect("tracked");
        assert_eq!(catalogs.files.len(), 1);

        std::fs::write(dir.join("fr.toml"), "titre = \"{n, plural, one {x}}\"\n").unwrap();
        let error = profile_error(&root);
        assert!(
            error.contains("E3706") && error.contains("fr.toml:1:9"),
            "{error}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn profile_error(root: &Path) -> String {
        match profile(root) {
            Ok(_) => panic!("a catalog error is a compile error"),
            Err(error) => error.to_string(),
        }
    }
}
