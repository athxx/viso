//! Viso build/CI automation.
//!
//! The check is *allowlist-based*: each crate declares exactly which internal
//! `viso-*` crates it may depend on. Any edge not in the allowlist — including
//! every forbidden edge in section 10.1 (platform→ui, gpu→ui, ui→widgets, …) — is a
//! failure. This is stricter than a blocklist and cannot silently rot.
//!
//! Each entry also carries its directory. Deriving it from the name (`viso-ui` →
//! `crates/ui`) stopped working once tooling crates appeared under `tools/`, and a
//! name-to-path convention that holds for most crates is worse than no convention:
//! the one crate it does not cover is silently skipped rather than checked.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// One crate: where it lives, and which internal crates it MAY depend on.
struct Allowed {
    /// Path relative to the workspace root.
    dir: &'static str,
    /// Internal crates this one may name. Anything else fails.
    deps: &'static [&'static str],
}

/// Whether a crate is tooling rather than framework, from its directory.
///
/// The `tools/` prefix is the whole rule (section 41): tooling may depend on the
/// framework, the framework may never depend on tooling.
fn is_tool(dir: &str) -> bool {
    dir.starts_with("tools/")
}

/// Allowed internal dependency edges. Key = crate, value = its directory and the
/// crates it MAY depend on. Anything outside this set fails. This encodes the
/// section 10 DAG plus the section 41 tooling direction.
fn allowed_edges() -> BTreeMap<&'static str, Allowed> {
    use std::iter::FromIterator;
    BTreeMap::from_iter([
        // Facade may depend on everything below it.
        (
            "viso",
            Allowed {
                dir: "crates/viso",
                deps: &[
                    "viso-runtime",
                    "viso-ui",
                    "viso-ui-macros",
                    "viso-widgets",
                    "viso-dsl",
                    "viso-services",
                    "viso-render",
                    "viso-svg",
                    "viso-gpu",
                    "viso-platform",
                    "viso-handle",
                    "viso-text",
                    "viso-shader",
                    "viso-macros",
                ],
            },
        ),
        (
            "viso-macros",
            Allowed {
                dir: "crates/macros",
                deps: &[],
            },
        ),
        // The `ui!` proc-macro crate. A proc-macro crate is a compile-time dylib,
        // so it MAY carry an ordinary library dependency (unlike the leaf
        // `viso-macros`): it drives the shared DSL frontend via `viso-dsl` and
        // emits `viso_ui::` builder tokens. The `viso-dsl -> viso-ui` edge above
        // keeps the emitted paths within the allowed DAG.
        (
            "viso-ui-macros",
            Allowed {
                dir: "crates/ui-macros",
                deps: &["viso-dsl"],
            },
        ),
        // Two owned foundations, both DAG leaves (section 10.1 forbidden edges):
        // math is the numeric/geometry base, ende is the encode/decode base.
        // Neither may depend on any framework crate, and they do not depend on
        // each other.
        (
            "viso-math",
            Allowed {
                dir: "crates/math",
                deps: &[],
            },
        ),
        (
            "viso-ende",
            Allowed {
                dir: "crates/ende",
                deps: &[],
            },
        ),
        (
            "viso-widgets",
            Allowed {
                dir: "crates/widgets",
                deps: &["viso-ui"],
            },
        ),
        // dsl works against a schema/registry, NOT concrete widgets (section 10.1).
        // `viso-ende` is the AOT package codec: the release emitter (Slice P) turns a
        // compiled fragment into a compact `viso_ende`-serialized package. Leaf edge,
        // no cycle.
        (
            "viso-dsl",
            Allowed {
                dir: "crates/dsl",
                deps: &["viso-ui", "viso-ende"],
            },
        ),
        // The formatter / language-server tooling for `.vs` (Slice R). A pure
        // analysis engine plus a thin stdio JSON-RPC bin, both reusing the
        // `viso-dsl` frontend (CST / resolver / LineIndex). A clean new leaf edge;
        // `.vs` (section 32) lists formatter/LSP as long-term standalone tools, so
        // this earns its own crate (AGENTS 3.3: independent executable + protocol
        // deps that must not pollute the compiler library).
        (
            "viso-lsp",
            Allowed {
                dir: "crates/lsp",
                deps: &["viso-dsl"],
            },
        ),
        (
            "viso-services",
            Allowed {
                dir: "crates/services",
                deps: &["viso-runtime"],
            },
        ),
        // `viso-ende` is the AOT package codec: the release loader (Slice P) decodes a
        // compact package and instantiates it through `BuildCx`, so the release path
        // carries no DSL compiler. Leaf edge, no cycle.
        (
            "viso-ui",
            Allowed {
                dir: "crates/ui",
                deps: &["viso-render", "viso-runtime", "viso-ende"],
            },
        ),
        (
            "viso-render",
            Allowed {
                dir: "crates/render",
                deps: &["viso-math", "viso-text", "viso-shader", "viso-gpu"],
            },
        ),
        // The SVG input lane (§13) sits above render: it lowers parsed SVG into
        // `viso_render::Path`/`Stroke`/`Primitive`, converting colors through
        // `viso-math`. Its heavy usvg dependency tree is isolated here (§3.3),
        // out of render/ui.
        (
            "viso-svg",
            Allowed {
                dir: "crates/svg",
                deps: &["viso-render", "viso-math"],
            },
        ),
        (
            "viso-text",
            Allowed {
                dir: "crates/text",
                deps: &["viso-gpu"],
            },
        ),
        (
            "viso-shader",
            Allowed {
                dir: "crates/shader",
                deps: &["viso-gpu"],
            },
        ),
        (
            "viso-gpu",
            Allowed {
                dir: "crates/gpu",
                deps: &["viso-runtime", "viso-handle", "viso-macros"],
            },
        ),
        (
            "viso-runtime",
            Allowed {
                dir: "crates/runtime",
                deps: &["viso-platform"],
            },
        ),
        (
            "viso-platform",
            Allowed {
                dir: "crates/platform",
                deps: &["viso-handle"],
            },
        ),
        (
            "viso-handle",
            Allowed {
                dir: "crates/handle",
                deps: &[],
            },
        ),
        // Tooling (`Viso_CLI.md` section 41). The project/config model answers "what
        // project am I in, and what is the resolved configuration" as plain data, so
        // that `viso --help` and `viso config show` need no framework initialization
        // (CLI section 64). It therefore depends on nothing internal — not as an
        // accident of being new, but so the rule stays visible here.
        (
            "viso-project",
            Allowed {
                dir: "tools/project",
                deps: &[],
            },
        ),
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
    let allowed = allowed_edges();
    let mut violations: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let mut tools = 0usize;

    for (name, entry) in &allowed {
        let toml_path = root.join(entry.dir).join("Cargo.toml");
        let Ok(text) = fs::read_to_string(&toml_path) else {
            violations.push(format!("missing manifest: {}", toml_path.display()));
            continue;
        };
        checked += 1;
        if is_tool(entry.dir) {
            tools += 1;
        }

        for dep in internal_deps(&text) {
            // The section 41 direction gets its own message. The allowlist already
            // rejects the edge, but "framework depends on tooling" is a different
            // mistake from "this edge skips a layer", and the two are fixed
            // differently — one by moving code, the other by widening the allowlist.
            if !is_tool(entry.dir)
                && allowed
                    .get(dep.as_str())
                    .is_some_and(|target| is_tool(target.dir))
            {
                violations.push(format!(
                    "FORBIDDEN EDGE: framework crate `{name}` depends on tooling crate `{dep}` \
                     (CLI section 41: tools/* -> framework only)"
                ));
                continue;
            }
            if !entry.deps.contains(&dep.as_str()) {
                violations.push(format!(
                    "FORBIDDEN EDGE: `{name}` depends on `{dep}` (not in the section 10 allowlist)"
                ));
            }
        }
    }

    // Every workspace member must appear above, or the allowlist checks a shrinking
    // fraction of the repository while continuing to report OK.
    violations.extend(unlisted_members(&root, &allowed));

    if violations.is_empty() {
        println!(
            "check-deps: OK — {checked} crates ({tools} tooling), all edges within the section 10 DAG"
        );
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

/// Workspace members that name a `viso*` crate the allowlist does not cover.
///
/// Without this, adding a crate and forgetting to list it above is invisible: the
/// checker keeps printing OK while covering a shrinking fraction of the repository.
/// A mismatched directory is reported too, since a listed-but-moved crate reads as
/// "missing manifest" and is easy to misdiagnose.
fn unlisted_members(root: &Path, allowed: &BTreeMap<&'static str, Allowed>) -> Vec<String> {
    let Ok(text) = fs::read_to_string(root.join("Cargo.toml")) else {
        return vec![format!("missing workspace manifest at {}", root.display())];
    };
    let mut out = Vec::new();
    for dir in workspace_members(&text) {
        let Ok(member) = fs::read_to_string(root.join(&dir).join("Cargo.toml")) else {
            out.push(format!("workspace member `{dir}` has no Cargo.toml"));
            continue;
        };
        let Some(name) = package_name(&member) else {
            continue;
        };
        if !name.starts_with("viso") || outside_the_dag(&dir) {
            continue;
        }
        match allowed.get(name.as_str()) {
            Some(entry) if entry.dir == dir => {}
            Some(entry) => out.push(format!(
                "`{name}` is listed at `{}` but the workspace has it at `{dir}`",
                entry.dir
            )),
            None => out.push(format!(
                "workspace member `{dir}` (`{name}`) is missing from the check-deps allowlist"
            )),
        }
    }
    out
}

/// Whether a member sits outside the dependency DAG entirely.
///
/// Examples are applications: they consume the facade and nothing consumes them
/// (AGENTS 4), so an allowlist entry would only restate that. `libs/` holds
/// standalone libraries with no framework dependency of their own, and `xtask` is the
/// checker itself. None of them can weaken the DAG, because a framework crate that
/// named one would still fail the allowlist above.
fn outside_the_dag(dir: &str) -> bool {
    dir == "xtask" || dir.starts_with("examples/") || dir.starts_with("libs/")
}

/// Member paths from `[workspace] members = [...]`.
fn workspace_members(toml: &str) -> Vec<String> {
    let mut members = Vec::new();
    let mut in_workspace = false;
    let mut in_members = false;
    for line in toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && !in_members {
            in_workspace = trimmed == "[workspace]";
            continue;
        }
        if in_workspace && trimmed.starts_with("members") {
            in_members = true;
        }
        if !in_members {
            continue;
        }
        members.extend(quoted(trimmed));
        if trimmed.ends_with(']') {
            in_members = false;
            in_workspace = false;
        }
    }
    members
}

/// `name` from a `[package]` table.
fn package_name(toml: &str) -> Option<String> {
    let mut in_package = false;
    for line in toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package
            && let Some((key, value)) = trimmed.split_once('=')
            && key.trim() == "name"
        {
            return quoted(value).into_iter().next();
        }
    }
    None
}

/// Every double-quoted string in a line. Enough for our own manifests, which have no
/// escapes in paths or crate names.
fn quoted(line: &str) -> Vec<String> {
    line.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
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
