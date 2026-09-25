//! Package identity for a macro invocation: the crate's manifest directory is the
//! package root, the nearest `Viso.toml` at or above it names the package and may
//! pin the language version, and the source file's path below the root is its
//! module path ([`Origin::for_file`]). Without a `Viso.toml` the Cargo package
//! name stands in.

use std::path::{Path, PathBuf};

use proc_macro2::Span;
use viso_dsl::frontend::Origin;

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

/// The package name and pinned language version: from the nearest `Viso.toml`
/// at or above `root`, else the Cargo package name and no pin.
fn identity(root: &Path) -> syn::Result<(String, Option<String>)> {
    let cargo_name = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    let Some(manifest) = root
        .ancestors()
        .map(|dir| dir.join(MANIFEST))
        .find(|path| path.is_file())
    else {
        return Ok((cargo_name, None));
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
    let key = |name: &str| {
        document
            .get("package")
            .and_then(|package| package.get(name))
            .and_then(|value| value.as_str())
            .map(str::to_owned)
    };
    Ok((key("name").unwrap_or(cargo_name), key("language")))
}
