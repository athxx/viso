//! Viso build/CI automation.
//!
//! The check is *allowlist-based*: each crate declares exactly which internal
//! `viso-*` crates it may depend on. Any edge not in the allowlist — including
//! every forbidden edge (platform→ui, gpu→ui, ui→widgets, …) — is a
//! failure. This is stricter than a blocklist and cannot silently rot.
//!
//! Each entry also carries its directory. Deriving it from the name (`viso-ui` →
//! `crates/ui`) stopped working once tooling crates appeared under `tools/`, and a
//! name-to-path convention that holds for most crates is worse than no convention:
//! the one crate it does not cover is silently skipped rather than checked.

mod absence;
mod bench_gate;
mod bundle;
mod footprint;
mod frame_tax;
mod imports;
mod mcu;
mod profiles;
mod targets;
mod worktree;

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
    /// Internal crates its tests, benches and examples may also name, as
    /// dev-dependencies only: they never reach the crate's own closure.
    dev: &'static [&'static str],
}

/// Whether a crate is tooling rather than framework, from its directory.
///
/// The `tools/` prefix is the whole rule: tooling may depend on the
/// framework, the framework may never depend on tooling.
fn is_tool(dir: &str) -> bool {
    dir.starts_with("tools/")
}

/// Allowed internal dependency edges. Key = crate, value = its directory and the
/// crates it MAY depend on. Anything outside this set fails. This encodes the
/// crate DAG plus the tooling direction ([`is_tool`]).
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
                    "viso-view",
                    "viso-dom",
                    "viso-behavior",
                    "viso-services",
                    "viso-render",
                    "viso-image",
                    "viso-media",
                    "viso-gpu",
                    "viso-platform",
                    "viso-text",
                    "viso-shader",
                    "viso-macros",
                    "viso-ende",
                    "viso-math",
                    "viso-pdf",
                ],
                // The DSL tests compile `.vs` sources themselves and reload
                // them with the patch the dev link carries; an app links
                // neither.
                dev: &["viso-dsl", "viso-dev-protocol"],
            },
        ),
        // PDF documents of the renderer's primitives, the facade's `pdf`
        // feature: it reads the primitive types and nothing of the runtime.
        (
            "viso-pdf",
            Allowed {
                dir: "crates/pdf",
                deps: &["viso-render", "viso-math"],
                dev: &[],
            },
        ),
        // The proc macro bakes glyphs with the rasterizer the text runtime
        // runs, so it carries the one leaf both share and no runtime.
        (
            "viso-macros",
            Allowed {
                dir: "crates/macros",
                deps: &["viso-glyph-a8"],
                dev: &[],
            },
        ),
        // A8 glyph coverage, rasterized or read from baked pages: a leaf that
        // the text runtime and the font baker in `viso-macros` both depend on.
        (
            "viso-glyph-a8",
            Allowed {
                dir: "crates/glyph-a8",
                deps: &[],
                dev: &[],
            },
        ),
        // The `ui!` proc-macro crate. A proc-macro crate is a compile-time dylib,
        // so it MAY carry an ordinary library dependency: it drives the shared
        // DSL frontend via `viso-dsl` and
        // emits `viso_ui::` builder tokens, which resolve in the app that
        // expands them; it links no runtime itself. It writes a shader's
        // Rust for the CPU raster with the shader crate's generator.
        (
            "viso-ui-macros",
            Allowed {
                dir: "crates/ui-macros",
                deps: &["viso-dsl", "viso-shader"],
                dev: &[],
            },
        ),
        // Two owned foundations, both DAG leaves:
        // math is the numeric/geometry base, ende is the encode/decode base.
        // Neither may depend on any framework crate, and they do not depend on
        // each other.
        (
            "viso-math",
            Allowed {
                dir: "crates/math",
                deps: &[],
                dev: &[],
            },
        ),
        (
            "viso-ende",
            Allowed {
                dir: "crates/ende",
                deps: &[],
                dev: &[],
            },
        ),
        // The data crossing the compile boundary: the compiler
        // builds against it and the runtime re-exports it. Pure data over the
        // encode/decode base, nothing of the runtime.
        (
            "viso-schema",
            Allowed {
                dir: "crates/schema",
                deps: &["viso-ende"],
                dev: &[],
            },
        ),
        // The dev link between the CLI and a dev runtime: framed
        // messages, the patch bundle, and what a running app reports to a
        // tool. Both sides build against it, so the CLI links no runtime.
        (
            "viso-dev-protocol",
            Allowed {
                dir: "crates/dev-protocol",
                deps: &[
                    "viso-ende",
                    "viso-macros",
                    "viso-math",
                    "viso-schema",
                    "viso-gpu-abi",
                ],
                dev: &[],
            },
        ),
        (
            "viso-widgets",
            Allowed {
                dir: "crates/widgets",
                deps: &["viso-ui", "viso-math"],
                dev: &[],
            },
        ),
        // Charts are an optional extra over the widgets, drawn by the UI's
        // plot content; their tests drive the facade headless.
        (
            "viso-charts",
            Allowed {
                dir: "extras/charts",
                deps: &["viso-ui", "viso-widgets", "viso-math"],
                dev: &["viso"],
            },
        ),
        // The map view is an optional extra over the widgets: retained
        // drawings and transform roots of the UI, meshes of the renderer,
        // labels shaped by the text runtime, the services' replies for
        // asynchronous tiles and the JSON reader for GeoJSON. Its tests
        // drive the facade headless.
        (
            "viso-map",
            Allowed {
                dir: "extras/map",
                deps: &[
                    "viso-ui",
                    "viso-widgets",
                    "viso-render",
                    "viso-text",
                    "viso-math",
                    "viso-services",
                    "viso-ende",
                ],
                dev: &["viso"],
            },
        ),
        // The code editor is an optional extra over the widgets; its tests
        // drive the facade headless.
        (
            "viso-code-editor",
            Allowed {
                dir: "extras/code-editor",
                deps: &["viso-ui", "viso-lexer", "viso-ende"],
                dev: &["viso", "viso-lsp"],
            },
        ),
        // Source spans and the one diagnostic shape: a leaf the `.vs`
        // frontend and the shader compiler both report in.
        (
            "viso-diag",
            Allowed {
                dir: "crates/diag",
                deps: &[],
                dev: &[],
            },
        ),
        // The `.vs` tokenizer the compiler and the code editor's highlighter
        // share, its spans from the diagnostics leaf.
        (
            "viso-lexer",
            Allowed {
                dir: "crates/lexer",
                deps: &["viso-diag"],
                dev: &[],
            },
        ),
        // dsl works against a schema/registry, NOT concrete widgets or the
        // runtime: it emits schema data (`viso-schema`) and dev-link patches
        // (`viso-dev-protocol`), encodes them with `viso-ende`, lowers behavior
        // to `viso-behavior` and shaders to `viso-shader`, reports in
        // `viso-diag`'s shape, and samples `viso-math`'s easing curves for
        // export. Its integration tests mount what it compiles, so they link
        // the runtime as dev-dependencies.
        (
            "viso-dsl",
            Allowed {
                dir: "crates/dsl",
                deps: &[
                    "viso-diag",
                    "viso-lexer",
                    "viso-schema",
                    "viso-dev-protocol",
                    "viso-ende",
                    "viso-behavior",
                    "viso-shader",
                    "viso-math",
                ],
                dev: &["viso-ui", "viso-view"],
            },
        ),
        // The runtime a compiled view's behavior runs in: a component instance on
        // the behavior VM, mirrored into UI state cells, and the node handlers
        // that dispatch into it. `viso-ui` never depends on it.
        (
            "viso-view",
            Allowed {
                dir: "crates/view",
                deps: &[
                    "viso-ui",
                    "viso-schema",
                    "viso-dev-protocol",
                    "viso-behavior",
                    "viso-ende",
                ],
                dev: &[],
            },
        ),
        // The `web-dom` runtime: a view's behavior on the behavior VM in a page
        // the browser lays out. It knows no UI node, layout or GPU.
        (
            "viso-dom",
            Allowed {
                dir: "crates/dom",
                deps: &["viso-behavior"],
                dev: &["viso-dsl"],
            },
        ),
        // The behavior bytecode and interpreter the DSL compiler lowers `fn`/`action`
        // bodies to. `viso-runtime` and `viso-ui` never depend on it, so a pure Rust
        // app links no VM; its binary module form is encoded through
        // `viso-ende`.
        (
            "viso-behavior",
            Allowed {
                dir: "crates/behavior",
                deps: &["viso-ende", "viso-math", "viso-schema"],
                dev: &[],
            },
        ),
        // The formatter / language-server tooling for `.vs`. A pure
        // analysis engine plus a thin stdio JSON-RPC bin, both reusing the
        // `viso-dsl` frontend (CST / resolver / LineIndex). A clean new leaf edge;
        // formatter/LSP are long-term standalone tools for `.vs`, so this earns its
        // own crate (an independent executable + protocol deps that must not
        // pollute the compiler library).
        (
            "viso-lsp",
            Allowed {
                dir: "crates/lsp",
                deps: &["viso-dsl"],
                dev: &[],
            },
        ),
        // Services reach platform only on Android, for the activity their JNI
        // calls go through; platform never sees services.
        (
            "viso-services",
            Allowed {
                dir: "crates/services",
                deps: &["viso-platform"],
                dev: &[],
            },
        ),
        // `viso-ende` is the AOT package codec: the release loader decodes a
        // compact package and instantiates it through `BuildCx`, so the release path
        // carries no DSL compiler. Leaf edge, no cycle. `viso-text` supplies the
        // editing model (positions, caret, hit test, selection, IME) text controls
        // edit through; it already sits below `viso-render`, so no cycle.
        (
            "viso-ui",
            Allowed {
                dir: "crates/ui",
                deps: &[
                    "viso-render",
                    "viso-runtime",
                    "viso-ende",
                    "viso-text",
                    "viso-math",
                    "viso-schema",
                ],
                dev: &[],
            },
        ),
        (
            "viso-render",
            Allowed {
                dir: "crates/render",
                deps: &[
                    "viso-math",
                    "viso-ende",
                    "viso-schema",
                    "viso-text",
                    "viso-image",
                    "viso-svg",
                    "viso-shader",
                    "viso-gpu",
                ],
                // The CPU raster bench compiles a `.vs` shader and the Rust
                // built from it.
                dev: &["viso-dsl", "viso-ui-macros"],
            },
        ),
        // The SVG input lane: the dependency-free parser to a normalized
        // document, over `viso-math` geometry. The renderer lowers the document
        // (`viso_render::svg`); tooling checks an SVG with this crate alone.
        (
            "viso-svg",
            Allowed {
                dir: "crates/svg",
                deps: &["viso-math", "viso-ende"],
                dev: &[],
            },
        ),
        // Raster images on the CPU: asset decode, the build-time conversion to
        // a target's texel format (stored in the `gpu-abi` formats, a leaf; SVG
        // rasterized from the parsed document) and the snapshot image, shared
        // by the renderer and the CLI.
        (
            "viso-image",
            Allowed {
                dir: "crates/image",
                deps: &["viso-schema", "viso-gpu-abi", "viso-svg", "viso-math"],
                dev: &[],
            },
        ),
        (
            "viso-text",
            Allowed {
                dir: "crates/text",
                deps: &["viso-glyph-a8"],
                dev: &[],
            },
        ),
        // `viso-ende` is the codec a view package carries a brush's layout
        // and code in. A program is laid out and coded for the GPU ABI, never a
        // backend.
        (
            "viso-shader",
            Allowed {
                dir: "crates/shader",
                deps: &["viso-gpu-abi", "viso-ende", "viso-diag"],
                dev: &[],
            },
        ),
        // The GPU ABI description: layouts, program ABI, descriptors, handles.
        // A leaf, so the compiler side describes GPU data without a backend.
        (
            "viso-gpu-abi",
            Allowed {
                dir: "crates/gpu-abi",
                deps: &[],
                dev: &[],
            },
        ),
        (
            "viso-gpu",
            Allowed {
                dir: "crates/gpu",
                deps: &["viso-handle", "viso-macros", "viso-gpu-abi"],
                dev: &[],
            },
        ),
        (
            "viso-runtime",
            Allowed {
                dir: "crates/runtime",
                deps: &["viso-platform"],
                dev: &[],
            },
        ),
        (
            "viso-platform",
            Allowed {
                dir: "crates/platform",
                deps: &["viso-handle"],
                dev: &[],
            },
        ),
        (
            "viso-handle",
            Allowed {
                dir: "crates/handle",
                deps: &[],
                dev: &[],
            },
        ),
        // Media through the OS players: a leaf, its target-specific media
        // stacks kept out of the window/input layer.
        (
            "viso-media",
            Allowed {
                dir: "crates/media",
                deps: &["viso-handle"],
                dev: &[],
            },
        ),
        // Tooling. The project/config model answers "what
        // project am I in, and what is the resolved configuration" as plain data, so
        // that `viso --help` and `viso config show` need no framework initialization
        // It therefore depends on nothing internal — not as an
        // accident of being new, but so the rule stays visible here.
        (
            "viso-project",
            Allowed {
                dir: "tools/project",
                deps: &[],
                dev: &[],
            },
        ),
        // The packager: an iOS `.app`, an Android APK, a web bundle around a
        // built app. It runs the platform toolchains and depends on nothing
        // internal.
        (
            "viso-package",
            Allowed {
                dir: "tools/package",
                deps: &[],
                dev: &[],
            },
        ),
        // The services the tools share: build orchestration, the dev session
        // and its control endpoint, the inspector client, devices and
        // toolchains, delivery and deployment. The dev session talks to a dev
        // runtime over `viso-dev-protocol`, and decodes each changed asset as
        // the app would before it sends it: a raster through `viso-image`, an
        // SVG through its parser, a font through the text runtime's sfnt
        // parser. It links no runtime.
        (
            "viso-tooling",
            Allowed {
                dir: "tools/tooling",
                deps: &[
                    "viso-project",
                    "viso-package",
                    "viso-dsl",
                    "viso-dev-protocol",
                    "viso-schema",
                    "viso-ende",
                    "viso-image",
                    "viso-svg",
                    "viso-text",
                ],
                dev: &[],
            },
        ),
        // The `viso` command, a thin consumer of the services: argument
        // parsing, output and exit codes over `viso-tooling`, the project
        // model and the compiler. It renders the data a running app reports
        // and compares a snapshot with its golden image, and decodes no asset
        // itself.
        (
            "viso-cli",
            Allowed {
                dir: "tools/cli",
                deps: &[
                    "viso-tooling",
                    "viso-project",
                    "viso-package",
                    "viso-dsl",
                    "viso-dev-protocol",
                    "viso-schema",
                    "viso-ende",
                    "viso-image",
                ],
                dev: &[],
            },
        ),
    ])
}

/// Edges that stay forbidden however the allowlist evolves. The allowlist
/// already rejects them; listing them here also rejects an allowlist entry that
/// would admit one, so widening the DAG cannot quietly reopen a closed boundary.
const NEVER: &[(&str, &str)] = &[
    ("viso-platform", "viso-ui"),
    ("viso-platform", "viso-widgets"),
    ("viso-platform", "viso-dsl"),
    ("viso-ui", "viso-widgets"),
    ("viso-ui", "viso-dsl"),
    ("viso-ui", "viso-behavior"),
    ("viso-runtime", "viso-behavior"),
    ("viso-widgets", "viso-behavior"),
    ("viso-render", "viso-widgets"),
    ("viso-gpu", "viso-ui"),
    ("viso-gpu", "viso-runtime"),
    ("viso-gpu", "viso-platform"),
    ("viso-text", "viso-platform"),
    ("viso-text", "viso-gpu"),
    ("viso-text", "viso-render"),
    ("viso-shader", "viso-gpu"),
    ("viso-svg", "viso-render"),
    ("viso-dsl", "viso-ui"),
    ("viso-dsl", "viso-view"),
    ("viso-cli", "viso-view"),
    ("viso-tooling", "viso-view"),
    // The command leaves decoding a changed asset to `viso-tooling`: it
    // parses no SVG or font itself.
    ("viso-cli", "viso-svg"),
    ("viso-cli", "viso-text"),
    ("viso-schema", "viso-ui"),
    ("viso-dev-protocol", "viso-ui"),
    // A `web-dom` page's DOM is the browser's.
    ("viso-dom", "viso-ui"),
    ("viso-dom", "viso-render"),
    ("viso-dom", "viso-gpu"),
    ("viso-dom", "viso-text"),
];

/// Crates on the compiler and tooling side whose dependency
/// closure holds no runtime, platform or GPU backend, so the compiler, the
/// language server and `viso check` build without any GPU or windowing code.
const COMPILER_SIDE: &[&str] = &[
    "viso-dsl",
    "viso-lsp",
    "viso-ui-macros",
    "viso-tooling",
    "viso-cli",
];

/// What [`COMPILER_SIDE`] closures must not reach: the runtime stack below
/// `viso-ui`. Everything else of the runtime (`viso-ui`, `viso-render`,
/// `viso-view`, …) reaches one of these, so listing them closes it all.
const RUNTIME_STACK: &[&str] = &["viso-runtime", "viso-platform", "viso-gpu"];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or_default();
    match cmd {
        "check-deps" => check_deps(),
        "bundle" => bundle::bundle(&args[1..]),
        "check-targets" => targets::check_targets(&args[1..]),
        "check-release-absence" => absence::check_release_absence(&args[1..]),
        "check-frame-tax" => frame_tax::check_frame_tax(&args[1..]),
        "check-benches" => bench_gate::check_benches(&args[1..]),
        "check-profiles" => profiles::check_profiles(&args[1..]),
        "mcu-smoke" => mcu::mcu_smoke(&args[1..]),
        "footprint" => footprint::footprint(&args[1..]),
        other => {
            eprintln!(
                "unknown xtask: {other:?}\nusage: cargo xtask check-deps | check-targets | check-release-absence | check-frame-tax | check-benches | check-profiles | mcu-smoke | footprint | bundle"
            );
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
    // Each crate's normal internal dependencies, as its manifest declares them.
    let mut normal: BTreeMap<&str, Vec<String>> = BTreeMap::new();

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

        let deps = internal_deps(&text);
        normal.insert(
            name,
            deps.iter()
                .filter(|(_, dev)| !dev)
                .map(|(dep, _)| dep.clone())
                .collect(),
        );
        for (dep, dev) in deps {
            // The tooling direction gets its own message. The allowlist already
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
            let admitted =
                entry.deps.contains(&dep.as_str()) || (dev && entry.dev.contains(&dep.as_str()));
            if !admitted {
                let kind = if dev { "dev-depends" } else { "depends" };
                violations.push(format!(
                    "FORBIDDEN EDGE: `{name}` {kind} on `{dep}` (not in the allowlist)"
                ));
            }
        }
    }

    for &(from, to) in NEVER {
        if allowed
            .get(from)
            .is_some_and(|entry| entry.deps.contains(&to))
        {
            violations.push(format!(
                "FORBIDDEN EDGE: the allowlist admits `{from}` -> `{to}`, which is a forbidden edge"
            ));
        }
    }

    violations.extend(runtime_in_closure(&normal));

    // Every workspace member must appear above, or the allowlist checks a shrinking
    // fraction of the repository while continuing to report OK.
    violations.extend(unlisted_members(&root, &allowed));
    violations.extend(duplicate_geometry(&root, &allowed));
    match imports::scan(&root, NEVER) {
        Ok(found) => violations.extend(found.into_iter().map(|f| format!("FORBIDDEN IMPORT: {f}"))),
        Err(why) => violations.push(format!("import scan: {why}")),
    }

    if violations.is_empty() {
        println!(
            "check-deps: OK — {checked} crates ({tools} tooling), all edges within the allowed DAG, \
             {} module rule(s) over {} crate(s)",
            imports::RULES.len(),
            imports::SCANNED.len()
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

/// A [`COMPILER_SIDE`] crate whose normal-dependency closure reaches the
/// [`RUNTIME_STACK`], with the path it takes.
fn runtime_in_closure(normal: &BTreeMap<&str, Vec<String>>) -> Vec<String> {
    let mut out = Vec::new();
    for &root in COMPILER_SIDE {
        // Breadth first, each crate's path from `root` kept for the message.
        let mut paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
        paths.insert(root.to_owned(), vec![root.to_owned()]);
        let mut queue = vec![root.to_owned()];
        while let Some(at) = queue.pop() {
            let path = paths[&at].clone();
            for dep in normal.get(at.as_str()).into_iter().flatten() {
                if paths.contains_key(dep) {
                    continue;
                }
                let mut next = path.clone();
                next.push(dep.clone());
                if RUNTIME_STACK.contains(&dep.as_str()) {
                    out.push(format!(
                        "RUNTIME IN CLOSURE: `{root}` reaches `{dep}` ({}) — ADR 0042 keeps the \
                         compiler and tooling off the runtime stack",
                        next.join(" -> ")
                    ));
                }
                paths.insert(dep.clone(), next);
                queue.push(dep.clone());
            }
        }
    }
    out
}

/// Geometry vocabulary owned by `viso-math`: no other
/// crate defines a public type of the same name, so a rect is one type from
/// layout to the GPU upload.
const MATH_OWNED: &[&str] = &[
    "Rect",
    "Point",
    "Size",
    "Insets",
    "Vec2",
    "Vec3",
    "Vec4",
    "Affine2",
    "Mat2",
    "Mat3",
    "Mat4",
    "Quat",
    "Transform3",
    "Aabb",
    "Ray",
    "Plane",
];

/// Public geometry types declared outside `crates/math`.
fn duplicate_geometry(root: &Path, allowed: &BTreeMap<&'static str, Allowed>) -> Vec<String> {
    let mut out = Vec::new();
    for entry in allowed.values() {
        if entry.dir == "crates/math" {
            continue;
        }
        let mut files = Vec::new();
        rust_files(&root.join(entry.dir).join("src"), &mut files);
        for file in files {
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            for (line_no, line) in text.lines().enumerate() {
                let Some(rest) = line.trim_start().strip_prefix("pub struct ") else {
                    continue;
                };
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if MATH_OWNED.contains(&name.as_str()) {
                    out.push(format!(
                        "DUPLICATE GEOMETRY: `{name}` declared at {}:{} — use `viso_math::{name}`",
                        file.strip_prefix(root).unwrap_or(&file).display(),
                        line_no + 1
                    ));
                }
            }
        }
    }
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Extract `viso-*` dependency names from a Cargo.toml, each with whether it
/// is a dev-dependency. Minimal parser: scans lines inside any
/// `[dependencies]`-family table for keys/entries that name a `viso-*` crate.
/// Good enough for our own manifests, which we control.
fn internal_deps(toml: &str) -> Vec<(String, bool)> {
    let mut deps = Vec::new();
    let mut in_deps_table = false;
    let mut dev_table = false;
    for line in toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            // Any table whose name ends in `dependencies` counts (including
            // target-specific `[target.'..'.dependencies]`).
            let table = trimmed.trim_end_matches(']');
            in_deps_table = table.ends_with("dependencies");
            dev_table = table.ends_with("dev-dependencies");
            continue;
        }
        if !in_deps_table || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Key is the crate name before `=`.
        if let Some((key, _)) = trimmed.split_once('=') {
            let key = key.trim();
            if key.starts_with("viso") {
                deps.push((key.to_string(), dev_table));
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
/// Examples and the apps under `benches/` are applications: they consume the
/// facade and nothing consumes them, so an allowlist entry would only
/// restate that. `libs/` holds standalone libraries with no framework dependency
/// of their own, and `xtask` is the checker itself. None of them can weaken the
/// DAG, because a framework crate that named one would still fail the allowlist
/// above.
fn outside_the_dag(dir: &str) -> bool {
    dir == "xtask"
        || dir.starts_with("examples/")
        || dir.starts_with("benches/")
        || dir.starts_with("libs/")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dev_dependency_is_told_apart_and_stays_out_of_the_closure() {
        let manifest = "[dependencies]\nviso-schema = { workspace = true }\n\n\
                        [dev-dependencies]\nviso-view = { workspace = true }\n";
        assert_eq!(
            internal_deps(manifest),
            [
                ("viso-schema".to_owned(), false),
                ("viso-view".to_owned(), true)
            ]
        );
        let normal = BTreeMap::from([
            ("viso-dsl", vec!["viso-schema".to_owned()]),
            ("viso-lsp", vec!["viso-dsl".to_owned()]),
        ]);
        assert!(runtime_in_closure(&normal).is_empty());
    }

    #[test]
    fn a_runtime_crate_reached_through_another_is_named_with_its_path() {
        let normal = BTreeMap::from([
            ("viso-lsp", vec!["viso-dsl".to_owned()]),
            ("viso-dsl", vec!["viso-ui".to_owned()]),
            ("viso-ui", vec!["viso-runtime".to_owned()]),
        ]);
        let found = runtime_in_closure(&normal);
        assert!(
            found
                .iter()
                .any(|v| v.contains("`viso-lsp` reaches `viso-runtime` (viso-lsp -> viso-dsl -> viso-ui -> viso-runtime)")),
            "{found:?}"
        );
    }
}
