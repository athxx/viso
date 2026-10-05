//! Package identity for a macro invocation: the crate's manifest directory is the
//! package root, the nearest `Viso.toml` at or above it names the package and may
//! pin the language version, and the source file's path below the root is its
//! module path ([`Origin::for_file`]). Without a `Viso.toml` the Cargo package
//! name stands in and nothing is granted.

use std::path::{Path, PathBuf};

use proc_macro2::Span;
use viso_dsl::frontend::Origin;
use viso_dsl::hir::{CapabilitySet, TargetProfile};

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

/// The build profile of the crate rooted at `root`: the capabilities its
/// `Viso.toml` grants (`[package] capabilities`), which the module it embeds
/// carries to the host that links it.
pub fn profile(root: &Path) -> syn::Result<TargetProfile> {
    let mut capabilities = CapabilitySet::new();
    if let Some((path, document)) = manifest(root)?
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
    Ok(TargetProfile {
        capabilities,
        ..TargetProfile::default()
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
            .capabilities
            .iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(granted, ["clipboard.write", "network.http"]);

        let bad = package(Some("[package]\ncapabilities = \"clipboard.write\"\n"));
        assert!(profile(&bad).is_err());
    }
}
