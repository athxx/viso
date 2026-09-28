//! `cargo xtask check-targets [--target <triple>]...`: Clippy with `-D warnings`
//! (which type-checks every crate it lints) over each supported target, with
//! every target kind except benches on wasm.
//!
//! The host target lints the whole workspace. A cross target leaves out the host
//! tooling crates, which run only on the developer machine. Every target is tried
//! even after one fails, so one run reports the whole matrix.

use std::process::{Command, ExitCode};

/// Every target a Viso app builds for.
pub const TARGETS: &[&str] = &[
    "aarch64-apple-darwin",
    "aarch64-apple-ios",
    "aarch64-apple-ios-sim",
    "aarch64-linux-android",
    "armv7-linux-androideabi",
    "wasm32-unknown-unknown",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
];

/// Workspace crates that run only on the developer machine.
const HOST_TOOLS: &[&str] = &["xtask", "viso-project", "viso-cli", "viso-lsp"];

pub fn check_targets(args: &[String]) -> ExitCode {
    let targets = match selected(args) {
        Ok(targets) => targets,
        Err(message) => {
            eprintln!("{message}\nusage: cargo xtask check-targets [--target <triple>]...");
            return ExitCode::FAILURE;
        }
    };
    let host = host_triple();
    let failed: Vec<&str> = targets
        .iter()
        .copied()
        .filter(|target| !clippy(target, host.as_deref() == Some(*target)))
        .collect();
    if failed.is_empty() {
        println!("check-targets: {} target(s) clean", targets.len());
        ExitCode::SUCCESS
    } else {
        eprintln!("check-targets: failed on {}", failed.join(", "));
        ExitCode::FAILURE
    }
}

/// The `--target` arguments, or every supported target when none is given.
fn selected(args: &[String]) -> Result<Vec<&'static str>, String> {
    let mut targets = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg != "--target" {
            return Err(format!("unknown argument {arg:?}"));
        }
        let name = args.next().ok_or("--target needs a triple")?;
        let target = TARGETS
            .iter()
            .copied()
            .find(|target| target == name)
            .ok_or_else(|| format!("unsupported target {name:?}"))?;
        targets.push(target);
    }
    Ok(if targets.is_empty() {
        TARGETS.to_vec()
    } else {
        targets
    })
}

fn clippy(target: &str, host: bool) -> bool {
    println!("check-targets: {target}");
    let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo
        .current_dir(crate::workspace_root())
        .args(["clippy", "--workspace", "--target", target]);
    // The benches measure native builds; their harness does not build for wasm.
    if target.starts_with("wasm32") {
        cargo.args(["--lib", "--bins", "--examples", "--tests"]);
    } else {
        cargo.arg("--all-targets");
    }
    if !host {
        for tool in HOST_TOOLS {
            cargo.args(["--exclude", tool]);
        }
    }
    cargo.args(["--", "-D", "warnings"]);
    // `thread_local!` expands differently on emulated-TLS targets, and this lint
    // then flags initializers that are already `const`.
    if target == "armv7-linux-androideabi" {
        cargo.args(["-A", "clippy::missing_const_for_thread_local"]);
    }
    cargo.status().is_ok_and(|status| status.success())
}

fn host_triple() -> Option<String> {
    let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .arg("-vV")
        .output()
        .ok()?;
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn no_target_selects_the_whole_matrix() {
        assert_eq!(selected(&[]).unwrap(), TARGETS);
    }

    #[test]
    fn a_named_target_selects_only_itself() {
        let picked = selected(&args(&["--target", "wasm32-unknown-unknown"])).unwrap();
        assert_eq!(picked, ["wasm32-unknown-unknown"]);
    }

    #[test]
    fn an_unknown_target_or_argument_is_refused() {
        assert!(selected(&args(&["--target", "riscv64gc-unknown-linux-gnu"])).is_err());
        assert!(selected(&args(&["--target"])).is_err());
        assert!(selected(&args(&["--all"])).is_err());
    }
}
