//! `cargo xtask mcu-smoke`: the crates that keep the MCU-ready invariants
//! (Architecture §50A.6) checked for a Cortex-M4F/M7 microcontroller,
//! `thumbv7em-none-eabihf`, with no default features and so no `std`.
//!
//! Each crate is checked even after one fails, so one run reports them all.

use std::path::Path;
use std::process::{Command, ExitCode};

/// The microcontroller target.
const TARGET: &str = "thumbv7em-none-eabihf";

/// Each check: the package and the features it is checked with besides none.
const CHECKS: &[(&str, &[&str])] = &[
    ("viso-math", &[]),
    ("viso-ende", &[]),
    ("viso-schema", &[]),
    ("viso-behavior", &[]),
    // The bytecode interpreter, for an app whose behavior is not all
    // compiled ahead of time.
    ("viso-behavior", &["interpreter"]),
    // The baked glyph reader, without the rasterizer: what a fully baked
    // build draws its text from.
    ("viso-glyph-a8", &[]),
];

pub fn mcu_smoke(args: &[String]) -> ExitCode {
    if let Some(arg) = args.first() {
        eprintln!("unknown argument {arg:?}\nusage: cargo xtask mcu-smoke");
        return ExitCode::FAILURE;
    }
    if !installed() {
        eprintln!(
            "mcu-smoke: the Rust target {TARGET} is not installed; \
             run `rustup target add {TARGET}`"
        );
        return ExitCode::FAILURE;
    }
    let failed: Vec<String> = CHECKS
        .iter()
        .filter(|(package, features)| !check(package, features))
        .map(|(package, features)| named(package, features))
        .collect();
    if failed.is_empty() {
        println!("mcu-smoke: {} check(s) clean on {TARGET}", CHECKS.len());
        ExitCode::SUCCESS
    } else {
        eprintln!("mcu-smoke: failed on {TARGET}: {}", failed.join(", "));
        ExitCode::FAILURE
    }
}

/// Whether the toolchain has the target's standard library: `core` and
/// `alloc` in its sysroot.
fn installed() -> bool {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    Command::new(rustc)
        .args(["--print", "target-libdir", "--target", TARGET])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| {
            let dir = String::from_utf8_lossy(&output.stdout);
            Path::new(dir.trim()).is_dir()
        })
}

/// `cargo check` of `package` for the target with `features` only.
fn check(package: &str, features: &[&str]) -> bool {
    println!("mcu-smoke: {}", named(package, features));
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .args(["check", "-p", package, "--target", TARGET])
        .arg("--no-default-features");
    if !features.is_empty() {
        command.args(["--features", &features.join(",")]);
    }
    command.status().is_ok_and(|status| status.success())
}

/// A check as the summary names it.
fn named(package: &str, features: &[&str]) -> String {
    match features {
        [] => package.to_owned(),
        _ => format!("{package} (+{})", features.join(",")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_no_std_crate_is_checked_bare() {
        for package in ["viso-math", "viso-ende", "viso-schema", "viso-behavior"] {
            assert!(CHECKS.contains(&(package, &[][..])), "{package}");
        }
        assert_eq!(
            named("viso-behavior", &["interpreter"]),
            "viso-behavior (+interpreter)"
        );
    }
}
