//! `viso config` end to end: every layer in play at once, each key reporting
//! the one it won from, and validation reporting every problem.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

/// A scratch project directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str, manifest: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "viso-cli-config-{label}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Viso.toml"), manifest).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn viso(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_viso"));
    command.args(args).current_dir(cwd);
    for name in [
        "VISO_PROFILE",
        "VISO_OPT_LEVEL",
        "VISO_SOURCE_MAPS",
        "VISO_STRIP",
    ] {
        command.env_remove(name);
    }
    command.envs(env.iter().copied()).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

const LAYERED: &str = "\
[package]
name = \"demo\"

[profile.release]
source_maps = true
strip = true
";

/// The flag beats the environment, the environment the profile table, the
/// table the default — and each key says which spoke, with its line.
#[test]
fn show_reports_the_layer_each_value_won_from() {
    let s = Scratch::new("show", LAYERED);
    let env = [("VISO_PROFILE", "shipping"), ("VISO_STRIP", "0")];
    let out = viso(
        &s.0,
        &["config", "show", "--json", "--profile", "release"],
        &env,
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    let result = stdout
        .lines()
        .find(|line| line.contains(r#""type":"result""#))
        .expect("a result");
    for expected in [
        r#""profile":{"value":"release","origin":"flag"}"#,
        r#""strip":{"value":false,"origin":"env","from":"env VISO_STRIP"}"#,
        r#""source_maps":{"value":true,"origin":"profile","from":"Viso.toml [profile.release]:5:15"}"#,
        r#""opt_level":{"value":"3","origin":"default"}"#,
        r#""package.name":{"value":"demo","origin":"manifest","from":"Viso.toml:2:8"}"#,
        r#""package.bundle_id":{"value":null,"origin":"default"}"#,
    ] {
        assert!(result.contains(expected), "{expected} in {result}");
    }

    // Without the flag the environment names the profile.
    let out = viso(&s.0, &["config", "get", "profile"], &env);
    assert_eq!(text(&out.stdout), "profile = shipping (env VISO_PROFILE)\n");
}

#[test]
fn get_names_the_key_it_does_not_know() {
    let s = Scratch::new("get", LAYERED);
    let out = viso(&s.0, &["config", "get", "profile.relase.strip"], &[]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    assert!(stderr.starts_with("error[CLI_USAGE]: "), "{stderr}");
    assert!(stderr.contains("`profile.release.strip`"), "{stderr}");
}

#[test]
fn path_is_the_manifest_in_use() {
    let s = Scratch::new("path", LAYERED);
    let nested = s.0.join("src/deep");
    fs::create_dir_all(&nested).unwrap();
    let out = viso(&nested, &["config", "path"], &[]);
    assert_eq!(out.status.code(), Some(0));
    let path = PathBuf::from(text(&out.stdout).trim_end());
    assert_eq!(
        path.canonicalize().unwrap(),
        s.0.join("Viso.toml").canonicalize().unwrap()
    );
}

/// Every problem at once: two errors in the manifest and one in the
/// environment, each a diagnostic, and the exit of the first.
#[test]
fn validate_reports_every_problem() {
    let s = Scratch::new(
        "validate",
        "[package]\nname = \"demo\"\nbundle_id = \"demo\"\n\n[web]\ndefault_target = \"ios\"\n",
    );
    let out = viso(&s.0, &["config", "validate", "--json"], &[]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = text(&out.stdout);
    let codes: Vec<&str> = stdout
        .lines()
        .filter(|line| line.contains(r#""type":"diagnostic""#))
        .map(|line| {
            let at = line.find(r#""code":""#).unwrap() + 8;
            &line[at..at + 5]
        })
        .collect();
    assert_eq!(codes, ["C0007", "C0007"], "{stdout}");

    let s = Scratch::new("valid", LAYERED);
    let out = viso(&s.0, &["config", "validate"], &[("VISO_OPT_LEVEL", "fast")]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).starts_with("error[C0010]: VISO_OPT_LEVEL"));
    let out = viso(&s.0, &["config", "validate"], &[]);
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out.stdout).ends_with(": valid\n"));
}
