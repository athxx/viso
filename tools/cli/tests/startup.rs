//! What starts with nothing behind it: `--version`, `--help`, `completion`
//! and `explain` read no project, run no tool and use no network, so they
//! answer in a directory whose `Viso.toml` is broken, with no program on
//! `PATH` and no home, and answer fast.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// A directory of the test's own, so tests running at once never share one.
fn scratch(test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("startup")
        .join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Viso.toml"), "[package\nthis is not toml").unwrap();
    dir
}

fn bare(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", dir.join("no-programs"))
        .env("VISO_HOME", dir.join("no-home"))
        .output()
        .unwrap()
}

#[test]
fn the_constant_commands_need_nothing_around_them() {
    let dir = scratch("constant");
    for args in [
        &["--version"][..],
        &["--version", "--json"],
        &["--help"],
        &["build", "--help"],
        &["completion", "zsh"],
        &["explain", "ENV_LOCK_HELD"],
        &["explain", "E2001", "--json"],
    ] {
        let out = bare(&dir, args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "`viso {}`: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.stdout.is_empty(), "`viso {}`", args.join(" "));
    }
    assert!(!dir.join("no-home").exists(), "no home was made");
    assert!(!dir.join("target").exists(), "no project state was made");
}

/// The budget `viso --version` starts within, the best of a few runs. A
/// hypothesis for CI hardware, generous enough for a debug build; a release
/// build on a laptop takes a few milliseconds.
const VERSION_BUDGET: Duration = Duration::from_millis(250);

#[test]
fn version_answers_within_its_budget() {
    let dir = scratch("version");
    let best = (0..5)
        .map(|_| {
            let started = Instant::now();
            assert!(bare(&dir, &["--version"]).status.success());
            started.elapsed()
        })
        .min()
        .unwrap();
    assert!(best < VERSION_BUDGET, "`viso --version` took {best:?}");
}
