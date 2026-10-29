//! `cargo xtask check-profiles [--target <triple>]...`: hold each footprint
//! profile's release closure to what the profile must not link.
//!
//! The facade is resolved by `cargo tree` as `viso build --release` builds a
//! target of that profile: an embedded device target with its default
//! features off (`desktop`, the Linux desktop stack, among them), the
//! profile's on and `rasterizer`, which a build that rasterizes glyphs adds
//! back; a desktop one with the defaults alone. For each Linux triple
//! (`aarch64-unknown-linux-gnu` and `armv7-unknown-linux-gnueabihf` when none
//! is named); the mobile and web profiles on their own triples. Only normal edges count, and no proc-macro's subtree: the
//! closure is what the app links, not what compiles it.
//!
//! No release closure turns on the behavior interpreter, which only a dev
//! build adds (`viso/vm`). A forbidden crate is reported with the path the
//! closure reaches it by, a forbidden feature with the features that turn it
//! on, and every profile and triple is tried even after one fails, so one run
//! reports the whole matrix.

use std::process::{Command, ExitCode};

/// The triples a board is checked for when no `--target` names one: a
/// 64-bit and a 32-bit Arm Linux board.
const TARGETS: &[&str] = &["aarch64-unknown-linux-gnu", "armv7-unknown-linux-gnueabihf"];

/// What the DSL compiler and the hot reload watcher are: a release device
/// build compiles every view ahead of time and reloads nothing.
const COMPILER: &[&str] = &["viso-dsl", "viso-lsp", "notify*", "kqueue*", "inotify*"];

/// The Linux desktop stack a KMS display has none of: X11, Wayland, the
/// D-Bus portal and the AT-SPI accessibility bridge.
const DESKTOP: &[&str] = &["x11rb*", "wayland-*", "zbus*", "accesskit*"];

/// The desktop stack without accessibility, which a mobile app bridges too.
const DESKTOP_STACK: &[&str] = &["x11rb*", "wayland-*", "zbus*", "accesskit_unix"];

/// The crate features no release closure turns on: the behavior
/// interpreter, which a release build replaces with the Rust each view's
/// behavior compiles to.
const RELEASE_FEATURES: &[(&str, &str)] = &[("viso-behavior", "interpreter")];

/// One profile: its name, the facade features it turns on, whether it keeps
/// the facade's defaults, the triples it is built for (none: the Linux
/// board triples the run checks), and the crates its closure must not hold
/// (`name*` is every crate whose name starts so).
struct FootprintCheck {
    name: &'static str,
    features: &'static str,
    defaults: bool,
    targets: &'static [&'static str],
    forbidden: &'static [&'static [&'static str]],
}

/// The profiles: the embedded ones, the desktop's, the mobile one and the
/// web's.
const PROFILES: &[FootprintCheck] = &[
    // GLES 3 on a KMS display: no other platform's graphics or system API,
    // and no Vulkan.
    FootprintCheck {
        name: "embedded-gpu",
        features: "viso/gles,viso/kms,viso/rasterizer",
        defaults: false,
        targets: &[],
        forbidden: &[
            &["windows*", "objc2*", "metal", "wgpu*", "ash"],
            DESKTOP,
            COMPILER,
        ],
    },
    // Vulkan on a KMS display: the same, GLES aside.
    FootprintCheck {
        name: "embedded-gpu (vulkan)",
        features: "viso/native-vulkan,viso/kms,viso/rasterizer",
        defaults: false,
        targets: &[],
        forbidden: &[
            &[
                "windows*",
                "objc2*",
                "metal",
                "wgpu*",
                "glow",
                "khronos-egl",
            ],
            DESKTOP,
            COMPILER,
        ],
    },
    // The software raster on a KMS display: no GPU API at all.
    FootprintCheck {
        name: "embedded-cpu",
        features: "viso/cpu,viso/kms,viso/rasterizer",
        defaults: false,
        targets: &[],
        forbidden: &[
            &[
                "ash",
                "wgpu*",
                "metal",
                "objc2*",
                "windows*",
                "d3d12",
                "glow",
                "khronos-egl",
            ],
            DESKTOP,
            COMPILER,
        ],
    },
    // A desktop app on the facade's defaults: the desktop stack, and still
    // no compiler.
    FootprintCheck {
        name: "desktop",
        features: "",
        defaults: true,
        targets: &[],
        forbidden: &[COMPILER],
    },
    // An iOS or Android app: its own platform's APIs, no desktop stack and
    // no compiler.
    FootprintCheck {
        name: "mobile",
        features: "",
        defaults: true,
        targets: &["aarch64-apple-ios", "aarch64-linux-android"],
        forbidden: &[&["windows*", "d3d12", "wgpu*"], DESKTOP_STACK, COMPILER],
    },
    // A web app: the browser's APIs alone.
    FootprintCheck {
        name: "web",
        features: "",
        defaults: true,
        targets: &["wasm32-unknown-unknown"],
        forbidden: &[
            &[
                "windows*",
                "d3d12",
                "wgpu*",
                "objc2*",
                "metal",
                "ash",
                "glow",
                "khronos-egl",
            ],
            DESKTOP,
            COMPILER,
        ],
    },
];

pub fn check_profiles(args: &[String]) -> ExitCode {
    let targets = match selected(args) {
        Ok(targets) => targets,
        Err(message) => {
            eprintln!("{message}\nusage: cargo xtask check-profiles [--target <triple>]...");
            return ExitCode::FAILURE;
        }
    };
    let mut violations = Vec::new();
    let mut tried = 0;
    for profile in PROFILES {
        let own: Vec<String>;
        let targets = if profile.targets.is_empty() {
            &targets
        } else {
            own = profile.targets.iter().map(|t| (*t).to_owned()).collect();
            &own
        };
        for target in targets {
            tried += 1;
            match closure(profile, target) {
                Ok(tree) => {
                    let found = forbidden(profile, &tree);
                    let features = forbidden_features(&tree);
                    if found.is_empty() && features.is_empty() {
                        println!(
                            "check-profiles: {} on {target}: clean ({} crates)",
                            profile.name,
                            tree.len()
                        );
                    }
                    violations.extend(found.into_iter().map(|(name, path)| {
                        format!(
                            "{} on {target} links `{name}` ({})",
                            profile.name,
                            path.join(" -> ")
                        )
                    }));
                    for (krate, feature) in features {
                        let by = enabled_by(profile, target, krate, feature)
                            .unwrap_or_else(|why| vec![why]);
                        violations.push(format!(
                            "{} on {target} turns on `{krate}/{feature}` ({})",
                            profile.name,
                            by.join(" <- ")
                        ));
                    }
                }
                Err(why) => violations.push(format!("{} on {target}: {why}", profile.name)),
            }
        }
    }
    if violations.is_empty() {
        println!(
            "check-profiles: OK — {} profile(s), {tried} closure(s)",
            PROFILES.len()
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("check-profiles: {} violation(s):", violations.len());
        for violation in &violations {
            eprintln!("  - {violation}");
        }
        ExitCode::FAILURE
    }
}

/// The `--target` arguments, or [`TARGETS`] when none is given.
fn selected(args: &[String]) -> Result<Vec<String>, String> {
    let mut targets = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg != "--target" {
            return Err(format!("unknown argument {arg:?}"));
        }
        let triple = args.next().ok_or("--target needs a triple")?;
        if !triple.contains("-linux") {
            return Err(format!(
                "{triple} is not a Linux triple; an embedded profile runs on Linux"
            ));
        }
        targets.push(triple.clone());
    }
    if targets.is_empty() {
        targets = TARGETS.iter().map(|t| (*t).to_owned()).collect();
    }
    Ok(targets)
}

/// One crate of a closure: its depth below the facade, its name and the
/// features it is built with.
struct Node {
    depth: usize,
    name: String,
    features: Vec<String>,
}

/// `cargo tree` over the facade's normal edges, built as `profile` builds it
/// for `target`, with `extra` arguments.
fn tree(profile: &FootprintCheck, target: &str, extra: &[&str]) -> Result<String, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.args(["tree", "-p", "viso"]).args(extra);
    if !profile.defaults {
        command.arg("--no-default-features");
    }
    if !profile.features.is_empty() {
        command.args(["--features", profile.features]);
    }
    let output = command
        .args(["--target", target, "--prefix", "depth"])
        .output()
        .map_err(|error| format!("cannot run `cargo tree`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`cargo tree` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The facade's normal closure as `profile` builds it for `target`, in
/// `cargo tree` order, each crate at its depth.
fn closure(profile: &FootprintCheck, target: &str) -> Result<Vec<Node>, String> {
    let args = ["-e", "normal,no-proc-macro", "--format", "{p} [{f}]"];
    Ok(parse_tree(&tree(profile, target, &args)?))
}

/// `cargo tree --prefix depth --format "{p} [{f}]"` lines: the depth, then
/// the crate's name and version and its features in brackets.
fn parse_tree(text: &str) -> Vec<Node> {
    text.lines()
        .filter_map(|line| {
            let digits = line.find(|c: char| !c.is_ascii_digit())?;
            let depth = line[..digits].parse().ok()?;
            let name = line[digits..].split_whitespace().next()?.to_owned();
            let features = line
                .rfind('[')
                .and_then(|open| {
                    let list = &line[open + 1..];
                    Some(&list[..list.find(']')?])
                })
                .map(|list| {
                    list.split(',')
                        .filter(|f| !f.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            Some(Node {
                depth,
                name,
                features,
            })
        })
        .collect()
}

/// Every [`RELEASE_FEATURES`] entry `tree` turns on.
fn forbidden_features(tree: &[Node]) -> Vec<(&'static str, &'static str)> {
    RELEASE_FEATURES
        .iter()
        .copied()
        .filter(|&(krate, feature)| {
            tree.iter()
                .any(|node| node.name == krate && node.features.iter().any(|f| f == feature))
        })
        .collect()
}

/// The features that turn on `krate`'s `feature` as `profile` builds the
/// facade for `target`: the first chain of the inverted feature tree, from
/// the feature down to the one the build names.
fn enabled_by(
    profile: &FootprintCheck,
    target: &str,
    krate: &str,
    feature: &str,
) -> Result<Vec<String>, String> {
    let args = ["-e", "features,normal,no-proc-macro", "-i", krate];
    Ok(feature_chain(
        &tree(profile, target, &args)?,
        krate,
        feature,
    ))
}

/// In an inverted `cargo tree -e features --prefix depth` listing, the chain
/// from `krate feature "feature"` down its first dependents.
fn feature_chain(text: &str, krate: &str, feature: &str) -> Vec<String> {
    let head = format!("{krate} feature \"{feature}\"");
    let mut chain = Vec::new();
    let mut depth = None;
    for line in text.lines() {
        let digits = line.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
        let Ok(at) = line[..digits].parse::<usize>() else {
            continue;
        };
        let entry = line[digits..].trim_end_matches(" (*)");
        match depth {
            None if entry == head => depth = Some(at),
            Some(d) if at == d + 1 => {
                chain.push(entry.to_owned());
                depth = Some(at);
            }
            Some(d) if at <= d => break,
            _ => {}
        }
    }
    chain.insert(0, format!("{krate}/{feature}"));
    chain
}

/// Every crate of `tree` that `profile` forbids, once, with the path from the
/// facade to where the tree first reaches it.
fn forbidden(profile: &FootprintCheck, tree: &[Node]) -> Vec<(String, Vec<String>)> {
    let banned = |name: &str| {
        profile
            .forbidden
            .iter()
            .flat_map(|set| set.iter())
            .any(|pattern| match pattern.strip_suffix('*') {
                Some(prefix) => name.starts_with(prefix),
                None => name == *pattern,
            })
    };
    let mut found: Vec<(String, Vec<String>)> = Vec::new();
    let mut path: Vec<&str> = Vec::new();
    for node in tree {
        path.truncate(node.depth);
        path.push(&node.name);
        if banned(&node.name) && !found.iter().any(|(name, _)| *name == node.name) {
            found.push((
                node.name.clone(),
                path.iter().map(|s| (*s).to_owned()).collect(),
            ));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forbidden_crate_is_named_with_the_path_that_reaches_it() {
        let tree = parse_tree(
            "0viso v0.1.0 (/w/crates/viso) [cpu,kms]\n\
             1viso-gpu v0.1.0 (/w/crates/gpu) []\n\
             2ash v0.38.0 [default]\n\
             2libc v0.2.0 []\n\
             1viso-platform v0.1.0 (/w/crates/platform) []\n\
             2accesskit_unix v0.17.0 []\n\
             3accesskit v0.21.0 []\n\
             1viso-gpu v0.1.0 (/w/crates/gpu) [] (*)\n",
        );
        assert_eq!(tree.len(), 8);
        let cpu = &PROFILES[2];
        let found = forbidden(cpu, &tree);
        let names: Vec<&str> = found.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["ash", "accesskit_unix", "accesskit"]);
        assert_eq!(found[0].1, ["viso", "viso-gpu", "ash"]);
        assert_eq!(
            found[2].1,
            ["viso", "viso-platform", "accesskit_unix", "accesskit"]
        );
        // The desktop stack is no embedded profile's, and Vulkan only the
        // Vulkan board's.
        let gles = forbidden(&PROFILES[0], &tree);
        let names: Vec<&str> = gles.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["ash", "accesskit_unix", "accesskit"]);
        let vulkan = forbidden(&PROFILES[1], &tree);
        let names: Vec<&str> = vulkan.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["accesskit_unix", "accesskit"]);
    }

    #[test]
    fn the_interpreter_is_reported_with_the_features_that_turn_it_on() {
        let tree = parse_tree(
            "0viso v0.1.0 (/w/crates/viso) [cpu,vm]\n\
             1viso-behavior v0.1.0 (/w/crates/behavior) [interpreter,std]\n\
             1viso-view v0.1.0 (/w/crates/view) [vm]\n\
             2viso-behavior v0.1.0 (/w/crates/behavior) [interpreter,std] (*)\n",
        );
        assert_eq!(tree[1].features, ["interpreter", "std"]);
        assert_eq!(
            forbidden_features(&tree),
            [("viso-behavior", "interpreter")]
        );
        assert!(forbidden_features(&parse_tree("0viso-behavior v0.1.0 [std]\n")).is_empty());
        let inverted = "0viso-behavior v0.0.0 (/w/crates/behavior)\n\
                        1viso-behavior feature \"interpreter\"\n\
                        2viso-view feature \"vm\"\n\
                        3viso feature \"vm\" (command-line)\n\
                        1viso-behavior feature \"std\"\n\
                        2viso v0.0.0 (/w/crates/viso)\n";
        assert_eq!(
            feature_chain(inverted, "viso-behavior", "interpreter"),
            [
                "viso-behavior/interpreter",
                "viso-view feature \"vm\"",
                "viso feature \"vm\" (command-line)"
            ]
        );
    }

    #[test]
    fn only_linux_triples_are_checked() {
        assert_eq!(selected(&[]).unwrap().len(), TARGETS.len());
        let one = [
            "--target".to_owned(),
            "riscv64gc-unknown-linux-gnu".to_owned(),
        ];
        assert_eq!(selected(&one).unwrap(), ["riscv64gc-unknown-linux-gnu"]);
        let mac = ["--target".to_owned(), "aarch64-apple-darwin".to_owned()];
        assert!(selected(&mac).is_err());
    }
}
