//! Viso build/CI automation.
//!
//! `cargo xtask check-deps` enforces the crate dependency DAG from
//! `Viso_Architecture_and_Migration.md` section 10. It is dependency-free (no
//! third-party crates) so it stays a trivial leaf of the workspace.
//!
//! The check is *allowlist-based*: each crate declares exactly which internal
//! `viso-*` crates it may depend on. Any edge not in the allowlist — including
//! every forbidden edge in section 10.1 (platform→ui, gpu→ui, ui→widgets, …) — is a
//! failure. This is stricter than a blocklist and cannot silently rot.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Allowed internal dependency edges. Key = crate, value = crates it MAY
/// depend on. Anything outside this set fails. This encodes the section 10 DAG.
fn allowed_edges() -> BTreeMap<&'static str, &'static [&'static str]> {
    use std::iter::FromIterator;
    BTreeMap::from_iter([
        // Facade may depend on everything below it.
        (
            "viso",
            &[
                "viso-runtime",
                "viso-ui",
                "viso-ui-macros",
                "viso-widgets",
                "viso-dsl",
                "viso-services",
                "viso-render",
                "viso-gpu",
                "viso-platform",
                "viso-handle",
                "viso-text",
                "viso-shader",
                "viso-macros",
            ][..],
        ),
        ("viso-macros", &[][..]),
        // The `ui!` proc-macro crate. A proc-macro crate is a compile-time dylib,
        // so it MAY carry an ordinary library dependency (unlike the leaf
        // `viso-macros`): it drives the shared DSL frontend via `viso-dsl` and
        // emits `viso_ui::` builder tokens. The `viso-dsl -> viso-ui` edge above
        // keeps the emitted paths within the allowed DAG.
        ("viso-ui-macros", &["viso-dsl"][..]),
        // Two owned foundations, both DAG leaves (section 10.1 forbidden edges):
        // math is the numeric/geometry base, ende is the encode/decode base.
        // Neither may depend on any framework crate, and they do not depend on
        // each other.
        ("viso-math", &[][..]),
        ("viso-ende", &[][..]),
        ("viso-widgets", &["viso-ui"][..]),
        // dsl works against a schema/registry, NOT concrete widgets (section 10.1).
        ("viso-dsl", &["viso-ui"][..]),
        ("viso-services", &["viso-runtime"][..]),
        ("viso-ui", &["viso-render", "viso-runtime"][..]),
        ("viso-render", &["viso-text", "viso-shader", "viso-gpu"][..]),
        ("viso-text", &["viso-gpu"][..]),
        ("viso-shader", &["viso-gpu"][..]),
        (
            "viso-gpu",
            &["viso-runtime", "viso-handle", "viso-macros"][..],
        ),
        ("viso-runtime", &["viso-platform"][..]),
        ("viso-platform", &["viso-handle"][..]),
        ("viso-handle", &[][..]),
    ])
}

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    match cmd.as_str() {
        "check-deps" => check_deps(),
        other => {
            eprintln!("unknown xtask: {other:?}\nusage: cargo xtask check-deps");
            ExitCode::FAILURE
        }
    }
}

fn check_deps() -> ExitCode {
    let root = workspace_root();
    let crates_dir = root.join("crates");
    let allowed = allowed_edges();
    let mut violations: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for (name, allowed_deps) in &allowed {
        // Crate directory names: `viso` -> crates/viso, `viso-ui` -> crates/ui.
        let dir_name = name.strip_prefix("viso-").unwrap_or(name);
        let toml_path = crates_dir.join(dir_name).join("Cargo.toml");
        let Ok(text) = fs::read_to_string(&toml_path) else {
            violations.push(format!("missing manifest: {}", toml_path.display()));
            continue;
        };
        checked += 1;

        for dep in internal_deps(&text) {
            if !allowed_deps.contains(&dep.as_str()) {
                violations.push(format!(
                    "FORBIDDEN EDGE: `{name}` depends on `{dep}` (not in the section 10 allowlist)"
                ));
            }
        }
    }

    if violations.is_empty() {
        println!("check-deps: OK — {checked} crates, all edges within the section 10 DAG");
        ExitCode::SUCCESS
    } else {
        eprintln!("check-deps: {} violation(s):", violations.len());
        for v in &violations {
            eprintln!("  - {v}");
        }
        ExitCode::FAILURE
    }
}

/// Extract `viso-*` dependency names from a Cargo.toml. Minimal parser: scans
/// lines inside any `[dependencies]`-family table for keys/entries that name a
/// `viso-*` crate. Good enough for our own manifests, which we control.
fn internal_deps(toml: &str) -> Vec<String> {
    let mut deps = Vec::new();
    let mut in_deps_table = false;
    for line in toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            // Any table whose name ends in `dependencies` counts (including
            // target-specific `[target.'..'.dependencies]`).
            in_deps_table = trimmed.trim_end_matches(']').ends_with("dependencies");
            continue;
        }
        if !in_deps_table || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Key is the crate name before `=`.
        if let Some((key, _)) = trimmed.split_once('=') {
            let key = key.trim();
            if key.starts_with("viso") {
                deps.push(key.to_string());
            }
        }
    }
    deps
}

fn workspace_root() -> PathBuf {
    // xtask lives at <root>/xtask, so the manifest dir's parent is the root.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap());
    parent_or_self(&manifest_dir)
}

fn parent_or_self(p: &Path) -> PathBuf {
    p.parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| p.to_path_buf())
}
