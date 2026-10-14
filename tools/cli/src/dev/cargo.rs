//! What Cargo says changes the dev artifact: the Rust half of the watch
//! scope.
//!
//! `cargo metadata` resolves the app package's dependency graph; every path
//! package the app links — itself, its workspace's crates, a path dependency
//! outside the project — is source the artifact is built from, and so are the
//! files Cargo reads beside them: the lock file, the Cargo configuration and
//! the toolchain pin. A registry or git dependency changes only through the
//! lock file. Development dependencies do not reach the artifact.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use viso_ende::JsonValue;

/// The Rust sources and files whose change changes the dev artifact.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RustScope {
    /// The directory of each path package the app links, the app's first.
    pub(crate) packages: Vec<PathBuf>,
    /// The directory of each other path package Cargo knows of — a workspace
    /// member the app does not link, nested in or beside one it does — whose
    /// sources are not the app's.
    pub(crate) unlinked: Vec<PathBuf>,
    /// The files beside them Cargo reads: the workspace's lock file, and each
    /// configuration and toolchain pin it looks for, existing or not.
    pub(crate) files: Vec<PathBuf>,
    /// The directory of every package the app links, a registry or git one
    /// as well: where a platform shell the app is packaged with is found.
    pub(crate) linked: Vec<PathBuf>,
}

impl RustScope {
    /// The scope of the package whose manifest is `manifest`, a canonical
    /// path, as `cargo metadata` resolves it for the Rust `target` (the host
    /// when `None`).
    ///
    /// # Errors
    ///
    /// Cargo or `rustc` not running, or Cargo's answer not being metadata.
    pub(crate) fn read(
        manifest: &Path,
        target: Option<&str>,
        cargo_args: &[OsString],
    ) -> Result<RustScope, String> {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
        let mut command = Command::new(&cargo);
        command
            .args(["metadata", "--format-version", "1"])
            .args(cargo_args)
            .arg("--manifest-path")
            .arg(manifest);
        if let Some(platform) = target.map(str::to_owned).or_else(host_triple) {
            command.args(["--filter-platform", &platform]);
        }
        let output = command
            .output()
            .map_err(|error| format!("cannot run `cargo metadata`: {error}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("`cargo metadata` failed: {}", stderr.trim()));
        }
        let text = String::from_utf8(output.stdout)
            .map_err(|_| "`cargo metadata` wrote no UTF-8".to_owned())?;
        let metadata = JsonValue::parse(&text)
            .map_err(|error| format!("`cargo metadata` wrote no JSON: {error}"))?;
        RustScope::of(&metadata, manifest)
            .ok_or_else(|| "`cargo metadata` does not describe the package".to_owned())
    }

    /// The scope `metadata` gives the package of `manifest`.
    fn of(metadata: &JsonValue, manifest: &Path) -> Option<RustScope> {
        let packages = metadata.get("packages")?.as_array()?;
        fn manifest_of(package: &JsonValue) -> Option<&str> {
            package.get("manifest_path")?.as_str()
        }
        // A path package has no source; a registry or git one does.
        fn path_package(package: &JsonValue) -> Option<&Path> {
            if !package.get("source").is_none_or(JsonValue::is_null) {
                return None;
            }
            Path::new(manifest_of(package)?).parent()
        }
        let root = packages
            .iter()
            .find(|p| manifest_of(p).is_some_and(|m| Path::new(m) == manifest))?
            .get("id")?
            .as_str()?;
        let nodes = metadata.get("resolve")?.get("nodes")?.as_array()?;
        let node = |id: &str| {
            nodes
                .iter()
                .find(|n| n.get("id").and_then(JsonValue::as_str) == Some(id))
        };
        // The packages the app links: a dependency of a normal or build kind,
        // from the root out.
        let mut linked = vec![root];
        let mut at = 0;
        while let Some(&id) = linked.get(at) {
            at += 1;
            let Some(deps) = node(id).and_then(|n| n.get("deps")?.as_array()) else {
                continue;
            };
            for dep in deps {
                let Some(pkg) = dep.get("pkg").and_then(JsonValue::as_str) else {
                    continue;
                };
                let links = dep
                    .get("dep_kinds")
                    .and_then(JsonValue::as_array)
                    .is_none_or(|kinds| {
                        kinds.iter().any(|kind| {
                            kind.get("kind")
                                .is_none_or(|k| k.is_null() || k.as_str() == Some("build"))
                        })
                    });
                if links && !linked.contains(&pkg) {
                    linked.push(pkg);
                }
            }
        }
        let mut scope = RustScope::default();
        for id in &linked {
            let Some(package) = packages
                .iter()
                .find(|p| p.get("id").and_then(JsonValue::as_str) == Some(*id))
            else {
                continue;
            };
            if let Some(dir) = path_package(package) {
                scope.packages.push(dir.to_owned());
            }
            if let Some(dir) = manifest_of(package).and_then(|m| Path::new(m).parent()) {
                scope.linked.push(dir.to_owned());
            }
        }
        for package in packages {
            let id = package.get("id").and_then(JsonValue::as_str);
            if id.is_some_and(|id| !linked.contains(&id))
                && let Some(dir) = path_package(package)
            {
                scope.unlinked.push(dir.to_owned());
            }
        }
        // The lock file is the workspace's; Cargo looks for its configuration
        // and the toolchain pin from the app's directory up.
        let workspace = metadata.get("workspace_root")?.as_str()?;
        scope.files.push(Path::new(workspace).join("Cargo.lock"));
        for dir in manifest.parent()?.ancestors() {
            for file in [
                ".cargo/config.toml",
                ".cargo/config",
                "rust-toolchain.toml",
                "rust-toolchain",
            ] {
                scope.files.push(dir.join(file));
            }
        }
        if let Some(home) = cargo_home() {
            for file in ["config.toml", "config"] {
                let path = home.join(file);
                if !scope.files.contains(&path) {
                    scope.files.push(path);
                }
            }
        }
        Some(scope)
    }

    /// Whether a change of `path` changes the artifact: a Rust source,
    /// manifest or build script of a linked package — the innermost package
    /// holding it — or one of the files Cargo reads beside them.
    pub(crate) fn contains(&self, path: &Path) -> bool {
        if self.files.iter().any(|file| file == path) {
            return true;
        }
        let rust = path.extension().is_some_and(|ext| ext == "rs")
            || path.file_name().is_some_and(|name| name == "Cargo.toml");
        if !rust {
            return false;
        }
        let innermost = |dirs: &[PathBuf]| {
            dirs.iter()
                .filter(|dir| path.starts_with(dir))
                .map(|dir| dir.components().count())
                .max()
        };
        innermost(&self.packages) > innermost(&self.unlinked)
    }
}

/// Cargo's home, whose configuration every build reads: `$CARGO_HOME`, or
/// `.cargo` in the user's home.
fn cargo_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        return Some(PathBuf::from(home));
    }
    let user = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    Some(PathBuf::from(user).join(".cargo"))
}

/// The host's target triple, as `rustc -vV` reports it.
fn host_triple() -> Option<String> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let output = Command::new(rustc).arg("-vV").output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Metadata of an app linking a workspace crate and a registry crate, with
    /// a path crate it only tests with, one its build script uses, and a
    /// workspace member nested in it that it does not link.
    const METADATA: &str = r#"{
        "packages": [
            {"id": "app 0.1.0 (path+file:///w/app)", "source": null,
             "manifest_path": "/w/app/Cargo.toml"},
            {"id": "core 0.1.0 (path+file:///w/core)", "source": null,
             "manifest_path": "/w/core/Cargo.toml"},
            {"id": "gen 0.1.0 (path+file:///w/gen)", "source": null,
             "manifest_path": "/w/gen/Cargo.toml"},
            {"id": "probe 0.1.0 (path+file:///w/probe)", "source": null,
             "manifest_path": "/w/probe/Cargo.toml"},
            {"id": "tool 0.1.0 (path+file:///w/app/tool)", "source": null,
             "manifest_path": "/w/app/tool/Cargo.toml"},
            {"id": "libc 0.2.0", "source": "registry+https://github.com/rust-lang/crates.io-index",
             "manifest_path": "/home/.cargo/registry/libc/Cargo.toml"}
        ],
        "resolve": {"root": null, "nodes": [
            {"id": "app 0.1.0 (path+file:///w/app)", "deps": [
                {"pkg": "core 0.1.0 (path+file:///w/core)", "dep_kinds": [{"kind": null}]},
                {"pkg": "gen 0.1.0 (path+file:///w/gen)", "dep_kinds": [{"kind": "build"}]},
                {"pkg": "probe 0.1.0 (path+file:///w/probe)", "dep_kinds": [{"kind": "dev"}]}
            ]},
            {"id": "core 0.1.0 (path+file:///w/core)", "deps": [
                {"pkg": "libc 0.2.0", "dep_kinds": [{"kind": null}]}
            ]},
            {"id": "gen 0.1.0 (path+file:///w/gen)", "deps": []},
            {"id": "probe 0.1.0 (path+file:///w/probe)", "deps": []},
            {"id": "tool 0.1.0 (path+file:///w/app/tool)", "deps": []},
            {"id": "libc 0.2.0", "deps": []}
        ]},
        "workspace_root": "/w"
    }"#;

    #[test]
    fn the_scope_is_the_path_packages_the_app_links_and_what_cargo_reads() {
        let metadata = JsonValue::parse(METADATA).unwrap();
        let scope = RustScope::of(&metadata, Path::new("/w/app/Cargo.toml")).unwrap();
        assert_eq!(
            scope.packages,
            [
                PathBuf::from("/w/app"),
                PathBuf::from("/w/core"),
                PathBuf::from("/w/gen")
            ]
        );
        assert_eq!(
            scope.unlinked,
            [PathBuf::from("/w/probe"), PathBuf::from("/w/app/tool")]
        );
        for inside in [
            "/w/app/src/main.rs",
            "/w/core/src/deep/mod.rs",
            "/w/gen/Cargo.toml",
            "/w/app/build.rs",
            "/w/Cargo.lock",
            "/w/.cargo/config.toml",
            "/w/app/rust-toolchain.toml",
            "/.cargo/config.toml",
        ] {
            assert!(scope.contains(Path::new(inside)), "{inside}");
        }
        for outside in [
            "/w/probe/src/lib.rs",
            "/w/app/src/view.vs",
            "/w/app/README.md",
            "/home/.cargo/registry/libc/src/lib.rs",
            "/w/app/Cargo.lock",
            "/w/core/.cargo/config.toml",
            "/w/app/tool/src/main.rs",
            "/w/app/tool/Cargo.toml",
        ] {
            assert!(!scope.contains(Path::new(outside)), "{outside}");
        }
        assert_eq!(
            RustScope::of(&metadata, Path::new("/elsewhere/Cargo.toml")),
            None
        );
    }

    #[test]
    fn this_package_reads_its_scope_from_cargo() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("Cargo.toml")
            .canonicalize()
            .unwrap();
        let scope = RustScope::read(&manifest, None, &[]).unwrap();
        assert_eq!(scope.packages[0], manifest.parent().unwrap());
        let dsl = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/dsl")
            .canonicalize()
            .unwrap();
        assert!(scope.packages.contains(&dsl), "{:?}", scope.packages);
        assert!(scope.contains(&dsl.join("src/lib.rs")));
    }
}
