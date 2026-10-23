//! `cargo xtask check-profiles [--target <triple>]...`: hold each embedded
//! footprint profile's dependency closure to what the profile must not link.
//!
//! The facade is resolved by `cargo tree` as `viso build` builds a device
//! target of that profile: its default features off (`desktop`, the Linux
//! desktop stack, among them), the profile's on, and `rasterizer` and `vm`,
//! which a build that rasterizes glyphs and a dev build add back; for each Linux board triple (`aarch64-unknown-linux-gnu` and
//! `armv7-unknown-linux-gnueabihf` when none is named). Only normal edges
//! count, and no proc-macro's subtree: the closure is what the app links, not
//! what compiles it.
//!
//! A forbidden crate is reported with the path the closure reaches it by,
//! and every profile and triple is tried even after one fails, so one run
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

/// One profile: its name, the facade features it turns on, and the crates
/// its closure must not hold (`name*` is every crate whose name starts so).
struct FootprintCheck {
    name: &'static str,
    features: &'static str,
    forbidden: &'static [&'static [&'static str]],
}

/// The embedded profiles.
const PROFILES: &[FootprintCheck] = &[
    // GLES 3 on a KMS display: no other platform's graphics or system API,
    // and no Vulkan.
    FootprintCheck {
        name: "embedded-gpu",
        features: "viso/gles,viso/kms,viso/rasterizer,viso/vm",
        forbidden: &[
            &["windows*", "objc2*", "metal", "wgpu*", "ash"],
            DESKTOP,
            COMPILER,
        ],
    },
    // Vulkan on a KMS display: the same, GLES aside.
    FootprintCheck {
        name: "embedded-gpu (vulkan)",
        features: "viso/native-vulkan,viso/kms,viso/rasterizer,viso/vm",
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
        features: "viso/cpu,viso/kms,viso/rasterizer,viso/vm",
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
    for profile in PROFILES {
        for target in &targets {
            match closure(profile.features, target) {
                Ok(tree) => {
                    let found = forbidden(profile, &tree);
                    if found.is_empty() {
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
                }
                Err(why) => violations.push(format!("{} on {target}: {why}", profile.name)),
            }
        }
    }
    if violations.is_empty() {
        println!(
            "check-profiles: OK — {} profile(s) on {} target(s)",
            PROFILES.len(),
            targets.len()
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

/// One crate of a closure: its depth below the facade and its name.
struct Node {
    depth: usize,
    name: String,
}

/// The facade's normal closure with `features` alone for `target`, in
/// `cargo tree` order, each crate at its depth.
fn closure(features: &str, target: &str) -> Result<Vec<Node>, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["tree", "-p", "viso", "-e", "normal,no-proc-macro"])
        .arg("--no-default-features")
        .args(["--features", features, "--target", target])
        .args(["--prefix", "depth", "--format", "{p}"])
        .output()
        .map_err(|error| format!("cannot run `cargo tree`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`cargo tree` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(parse_tree(&String::from_utf8_lossy(&output.stdout)))
}

/// `cargo tree --prefix depth` lines: the depth, then the crate's name and
/// version.
fn parse_tree(text: &str) -> Vec<Node> {
    text.lines()
        .filter_map(|line| {
            let digits = line.find(|c: char| !c.is_ascii_digit())?;
            let depth = line[..digits].parse().ok()?;
            let name = line[digits..].split_whitespace().next()?.to_owned();
            Some(Node { depth, name })
        })
        .collect()
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
            "0viso v0.1.0 (/w/crates/viso)\n\
             1viso-gpu v0.1.0 (/w/crates/gpu)\n\
             2ash v0.38.0\n\
             2libc v0.2.0\n\
             1viso-platform v0.1.0 (/w/crates/platform)\n\
             2accesskit_unix v0.17.0\n\
             3accesskit v0.21.0\n\
             1viso-gpu v0.1.0 (/w/crates/gpu) (*)\n",
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
