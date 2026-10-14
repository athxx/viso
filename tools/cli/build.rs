//! Records the commit `viso` is built from, for `viso --version`:
//! `VISO_GIT_COMMIT`, its first twelve hex digits, or `unknown` outside a git
//! checkout.

use std::path::Path;
use std::process::Command;

fn main() {
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    let commit = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=VISO_GIT_COMMIT={commit}");

    // Rebuilt when the checked-out commit moves.
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        let dir = Path::new(&dir);
        for file in ["HEAD", "packed-refs"] {
            println!("cargo:rerun-if-changed={}", dir.join(file).display());
        }
        if let Some(head) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={}", dir.join(head).display());
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
