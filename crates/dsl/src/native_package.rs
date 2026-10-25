//! Native packages: native libraries a Cargo dependency of the app carries,
//! which its `.vs` code imports as it imports the standard ones.
//!
//! Such a crate declares them in its manifest:
//!
//! ```toml
//! [package.metadata.viso.natives]
//! schema = "natives.json"   # the libraries' schema, beside the manifest
//! library = "NATIVES"       # the `&'static [&'static NativeLibrary]` they are, in the crate
//! feature = "dsl"           # the feature that builds them, where one does
//! ```
//!
//! The schema is the libraries as [`schema::write`] writes them, lossless
//! but for each function's implementation; the crate's own tests check it
//! against the Rust libraries ([`schema::check`]), so it never drifts from
//! them. A tool that compiles the app's `.vs` without building its Rust
//! reads it with [`schema::read`]: the schemas enter the registry as stubs,
//! and the host links the module to the Rust libraries, where another
//! signature is `E6101` again.
//!
//! [`discover`] finds the packages among the app's direct dependencies
//! through `cargo metadata`, once per manifest and lock file for the
//! process; [`registry`] is the standard libraries and theirs, and
//! [`compile_linked`] compiles against them once a source needs them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use viso_behavior::native::schema;
use viso_behavior::native::{NativeLibrary, Natives, SchemaConflict};
use viso_ende::JsonValue;

use crate::diag::{Diagnostic, Severity};
use crate::frontend::Compiled;

/// A dependency's native libraries.
#[derive(Debug, Clone)]
pub struct NativePackage {
    /// The Cargo package's name.
    pub name: String,
    /// The Rust path of its libraries, as the app's crate names them:
    /// `::viso_net::NATIVES`.
    pub library: String,
    /// Their schemas.
    pub libraries: Vec<&'static NativeLibrary>,
}

impl NativePackage {
    /// Whether one of its libraries holds the native at `path`.
    pub fn holds(&self, path: &str) -> bool {
        self.libraries.iter().any(|library| {
            path.strip_prefix(library.path)
                .is_some_and(|rest| rest.starts_with("::"))
        })
    }
}

/// What a manifest's discovery was made from, so a change to either file
/// makes it again.
type Stamp = (Option<SystemTime>, Option<SystemTime>);

/// Each manifest's packages, with what they were found from.
type Found = HashMap<PathBuf, (Stamp, Arc<[NativePackage]>)>;

/// The native packages among the direct dependencies of the Cargo package
/// whose manifest is `manifest`, as `cargo metadata` resolves them, the
/// answer kept for the process until the manifest or the lock file changes.
///
/// # Errors
///
/// Cargo not running or failing, or a package's schema missing or not one.
pub fn discover(manifest: &Path) -> Result<Arc<[NativePackage]>, String> {
    let stamp = stamp(manifest);
    if let Some(packages) = known(manifest, stamp) {
        return Ok(packages);
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = std::process::Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(manifest)
        .output()
        .map_err(|error| format!("cannot run `cargo metadata`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`cargo metadata` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let metadata = JsonValue::parse(&String::from_utf8_lossy(&output.stdout))
        .map_err(|error| format!("`cargo metadata` wrote no JSON: {error}"))?;
    let packages: Arc<[NativePackage]> = packages(&metadata, |path| {
        std::fs::read_to_string(path).map_err(|e| format!("`{}`: {e}", path.display()))
    })?
    .into();
    found()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(manifest.to_owned(), (stamp, packages.clone()));
    Ok(packages)
}

/// What [`discover`] found for each manifest, and when.
fn found() -> &'static Mutex<Found> {
    static FOUND: OnceLock<Mutex<Found>> = OnceLock::new();
    FOUND.get_or_init(Mutex::default)
}

/// When `manifest` and the lock file beside it or above it last changed.
fn stamp(manifest: &Path) -> Stamp {
    let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let lock = manifest
        .ancestors()
        .skip(1)
        .map(|dir| dir.join("Cargo.lock"))
        .find(|lock| lock.is_file());
    (modified(manifest), lock.as_deref().and_then(modified))
}

/// What [`discover`] found for `manifest` as it is at `stamp`, if it looked.
fn known(manifest: &Path, stamp: Stamp) -> Option<Arc<[NativePackage]>> {
    let found = found().lock().unwrap_or_else(PoisonError::into_inner);
    let (was, packages) = found.get(manifest)?;
    (*was == stamp).then(|| packages.clone())
}

/// The manifest of the Cargo package `file` belongs to: the nearest
/// `Cargo.toml` above it.
pub fn manifest_of(file: &Path) -> Option<PathBuf> {
    file.ancestors()
        .skip(1)
        .map(|dir| dir.join("Cargo.toml"))
        .find(|manifest| manifest.is_file())
}

/// What was made of `.vs` code against the native packages of its Cargo
/// package.
pub struct Linked<T> {
    /// A compile, a loaded package.
    pub made: T,
    /// The packages it was made against: none when the standard libraries
    /// were enough.
    pub packages: Arc<[NativePackage]>,
    /// Why the packages could not be made against, when they were needed:
    /// their discovery failed or two of them conflict.
    pub unlinked: Option<String>,
}

/// What `compile` makes of `.vs` code of the Cargo package whose manifest
/// is `manifest`, as [`link_with`] makes it.
pub fn compile_linked(
    manifest: Option<&Path>,
    compile: impl Fn(Arc<Natives>) -> Compiled,
) -> Linked<Compiled> {
    link_with(manifest, compile, |compiled, visit| {
        compiled.diagnostics.iter_mut().for_each(visit);
    })
}

/// What `make` makes of `.vs` code of the Cargo package whose manifest is
/// `manifest`: against the standard libraries, or, when a path resolves to
/// nothing there (an `E2001` among the diagnostics `each` visits in what it
/// made),
/// against the package's native packages as well. Cargo runs only then,
/// and once per manifest and lock file for the process: later calls make it
/// against the packages found at once. Why they could not be found is a
/// note on each `E2001` it keeps.
pub fn link_with<T>(
    manifest: Option<&Path>,
    make: impl Fn(Arc<Natives>) -> T,
    each: impl Fn(&mut T, &mut dyn FnMut(&mut Diagnostic)),
) -> Linked<T> {
    let none = || Arc::from([]);
    let alone = |made| Linked {
        made,
        packages: none(),
        unlinked: None,
    };
    let Some(manifest) = manifest else {
        return alone(make(Natives::standard()));
    };
    let unresolved = |made: &mut T| {
        let mut found = false;
        each(made, &mut |d| {
            found |= d.code == "E2001" && d.severity == Severity::Error
        });
        found
    };
    let unlinked = |mut made: T, why: String| {
        let note = format!("the native packages are not known: {why}");
        each(&mut made, &mut |d| {
            if d.code == "E2001" {
                d.notes.push(note.clone());
            }
        });
        Linked {
            made,
            packages: none(),
            unlinked: Some(why),
        }
    };
    let packages = match known(manifest, stamp(manifest)) {
        Some(packages) => packages,
        None => {
            let mut made = make(Natives::standard());
            if !unresolved(&mut made) {
                return alone(made);
            }
            match discover(manifest) {
                Ok(packages) => packages,
                Err(why) => return unlinked(made, why),
            }
        }
    };
    if packages.is_empty() {
        return alone(make(Natives::standard()));
    }
    match registry(&packages) {
        Ok(natives) => Linked {
            made: make(natives),
            packages,
            unlinked: None,
        },
        Err(conflict) => unlinked(make(Natives::standard()), format!("E6101: {conflict}")),
    }
}

/// The native packages `metadata` says the root package depends on
/// directly, each schema read through `read_file`.
fn packages(
    metadata: &JsonValue,
    read_file: impl Fn(&Path) -> Result<String, String>,
) -> Result<Vec<NativePackage>, String> {
    let Some(resolve) = metadata.get("resolve").filter(|r| !r.is_null()) else {
        return Ok(Vec::new());
    };
    let Some(root) = resolve.get("root").and_then(JsonValue::as_str) else {
        return Ok(Vec::new());
    };
    let nodes = resolve
        .get("nodes")
        .and_then(JsonValue::as_array)
        .unwrap_or(&[]);
    let node = |id: &str| {
        nodes
            .iter()
            .find(|n| n.get("id").and_then(JsonValue::as_str) == Some(id))
    };
    let all = metadata
        .get("packages")
        .and_then(JsonValue::as_array)
        .unwrap_or(&[]);
    let package = |id: &str| {
        all.iter()
            .find(|p| p.get("id").and_then(JsonValue::as_str) == Some(id))
    };
    let deps = node(root)
        .and_then(|n| n.get("deps"))
        .and_then(JsonValue::as_array)
        .unwrap_or(&[]);
    let mut found = Vec::new();
    for dep in deps {
        // A normal dependency: a kind of `null`, for any platform.
        let normal = dep
            .get("dep_kinds")
            .and_then(JsonValue::as_array)
            .is_some_and(|kinds| {
                kinds
                    .iter()
                    .any(|k| k.get("kind").is_none_or(JsonValue::is_null))
            });
        let (Some(id), Some(extern_name)) = (
            dep.get("pkg").and_then(JsonValue::as_str),
            dep.get("name").and_then(JsonValue::as_str),
        ) else {
            continue;
        };
        let Some(declared) = package(id)
            .and_then(|p| p.get("metadata"))
            .and_then(|m| m.get("viso"))
            .and_then(|v| v.get("natives"))
        else {
            continue;
        };
        if !normal {
            continue;
        }
        let name = package(id)
            .and_then(|p| p.get("name"))
            .and_then(JsonValue::as_str)
            .unwrap_or(extern_name);
        if let Some(feature) = declared.get("feature").and_then(JsonValue::as_str) {
            let enabled = node(id)
                .and_then(|n| n.get("features"))
                .and_then(JsonValue::as_array)
                .is_some_and(|f| f.iter().any(|f| f.as_str() == Some(feature)));
            if !enabled {
                continue;
            }
        }
        let manifest = package(id)
            .and_then(|p| p.get("manifest_path"))
            .and_then(JsonValue::as_str)
            .ok_or_else(|| format!("`{name}` has no manifest path"))?;
        let dir = Path::new(manifest).parent().unwrap_or(Path::new("."));
        let member = |member: &str| {
            declared
                .get(member)
                .and_then(JsonValue::as_str)
                .ok_or_else(|| format!("`{name}`: `{member}` is not a string"))
        };
        let file = dir.join(member("schema")?);
        let library = member("library")?;
        let libraries =
            schema::read(&read_file(&file)?).map_err(|e| format!("`{}`: {e}", file.display()))?;
        found.push(NativePackage {
            name: name.to_owned(),
            library: format!("::{extern_name}::{library}"),
            libraries,
        });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// The standard libraries and those of `packages`.
///
/// # Errors
///
/// Two of them declaring one path (`E6101`).
pub fn registry(packages: &[NativePackage]) -> Result<Arc<Natives>, SchemaConflict> {
    if packages.is_empty() {
        return Ok(Natives::standard());
    }
    let libraries: Vec<&'static NativeLibrary> = packages
        .iter()
        .flat_map(|p| p.libraries.iter().copied())
        .collect();
    Natives::shared(&libraries)
}

#[cfg(test)]
mod tests {
    use viso_behavior::native::schema::write;

    use super::*;

    static LIBRARY: NativeLibrary = NativeLibrary {
        path: "net",
        version: 1,
        functions: &[
            viso_behavior::native!(task "get" |_cx, url: String| -> i64 { Ok(url.len() as i64) })
                .requires(&["network.http"]),
        ],
        types: &[],
        traits: &[],
        derives: &[],
        widgets: &[],
    };

    #[test]
    fn the_packages_are_the_roots_direct_normal_dependencies_with_natives() {
        let metadata = JsonValue::parse(
            r#"{
              "packages": [
                {"id": "app", "name": "app", "manifest_path": "/w/app/Cargo.toml", "metadata": null},
                {"id": "net", "name": "viso-net", "manifest_path": "/w/net/Cargo.toml",
                 "metadata": {"viso": {"natives": {"schema": "natives.json", "library": "NATIVES", "feature": "dsl"}}}},
                {"id": "off", "name": "off", "manifest_path": "/w/off/Cargo.toml",
                 "metadata": {"viso": {"natives": {"schema": "n.json", "library": "N", "feature": "dsl"}}}},
                {"id": "dev", "name": "dev", "manifest_path": "/w/dev/Cargo.toml",
                 "metadata": {"viso": {"natives": {"schema": "n.json", "library": "N"}}}}
              ],
              "resolve": {"root": "app", "nodes": [
                {"id": "app", "deps": [
                  {"name": "net2", "pkg": "net", "dep_kinds": [{"kind": null, "target": null}]},
                  {"name": "off", "pkg": "off", "dep_kinds": [{"kind": null, "target": null}]},
                  {"name": "dev", "pkg": "dev", "dep_kinds": [{"kind": "dev", "target": null}]}
                ], "features": []},
                {"id": "net", "deps": [], "features": ["default", "dsl"]},
                {"id": "off", "deps": [], "features": []},
                {"id": "dev", "deps": [], "features": []}
              ]}
            }"#,
        )
        .unwrap();
        let schema = write(&[&LIBRARY]).unwrap();
        let found = packages(&metadata, |path| {
            assert_eq!(path, Path::new("/w/net/natives.json"));
            Ok(schema.clone())
        })
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "viso-net");
        assert_eq!(found[0].library, "::net2::NATIVES");
        assert!(found[0].holds("net::get") && !found[0].holds("network::get"));
        let natives = registry(&found).unwrap();
        assert!(
            natives.function("net::get").is_some()
                && natives.function("viso::text::upper").is_some()
        );
    }
}
