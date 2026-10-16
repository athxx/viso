//! `viso package` end to end, short of building: the plan `--dry-run`
//! shows, the metadata checked where it is written, and the signing the
//! policy asks for refused before anything compiles.

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
            "viso-cli-package-{label}-{}-{n}",
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

fn viso(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .current_dir(cwd)
        .env_remove("VISO_PROFILE")
        .env_remove("VISO_ANDROID_KEYSTORE_PASSWORD")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

const APP: &str = "[package]\nname = \"notes\"\ndisplay_name = \"Notes\"\nversion = \"2.1.0\"\n\
                   bundle_id = \"com.example.notes\"\nbuild = 42\n";

/// The plan names the metadata, the signing, each step and each file, and
/// builds nothing.
#[test]
fn a_dry_run_shows_the_package_and_builds_nothing() {
    let s = Scratch::new("dry", APP);
    let out = viso(&s.0, &["package", "web-gpu", "--dry-run", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
    let stdout = text(&out.stdout);
    let result = stdout
        .lines()
        .find(|line| line.contains(r#""type":"result""#))
        .expect("a result");
    for expected in [
        r#""target":"web-gpu""#,
        r#""profile":"shipping""#,
        r#""display_name":"Notes""#,
        r#""version":"2.1.0","build":42"#,
        r#""signing":{"policy":"auto","status":"none","identity":null}"#,
        r#""output":"dist/web""#,
        r#"{"kind":"archive","path":"dist/web/notes-2.1.0-web-gpu.zip"}"#,
    ] {
        assert!(result.contains(expected), "{expected} in {result}");
    }
    assert!(!s.0.join("dist").exists());
    assert!(!s.0.join("target").exists());

    // The release profile is the other a package is built with.
    let out = viso(
        &s.0,
        &["package", "web-gpu", "--dry-run", "--profile", "release"],
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out.stdout).starts_with("Package web-gpu (release)\n"));
}

#[test]
fn a_package_never_holds_the_dev_runtime() {
    let s = Scratch::new("dev", APP);
    let out = viso(&s.0, &["package", "--profile", "dev", "--dry-run"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    assert!(stderr.starts_with("error[CLI_USAGE]: "), "{stderr}");
}

/// What the package needs is checked where it is written, every problem
/// at once.
#[test]
fn missing_metadata_is_reported_at_its_place() {
    let s = Scratch::new(
        "meta",
        "[package]\nname = \"notes\"\n\n[package.icons]\nsource = \"none.png\"\n",
    );
    let out = viso(&s.0, &["package", "android", "--dry-run", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = text(&out.stdout);
    let codes: Vec<&str> = stdout
        .lines()
        .filter(|line| line.contains(r#""type":"diagnostic""#))
        .filter_map(|line| line.split(r#""code":""#).nth(1)?.split('"').next())
        .collect();
    assert_eq!(codes, ["C0006", "C0006", "C0007"], "{stdout}");
}

/// `required` refuses what has no signature to make, and a keystore whose
/// password is not in the environment, before anything is built.
#[test]
fn signing_is_refused_before_the_build() {
    let s = Scratch::new("web", APP);
    let out = viso(&s.0, &["package", "web-gpu", "--signing", "required"]);
    assert_eq!(out.status.code(), Some(6));
    assert!(
        text(&out.stderr)
            .starts_with("error[PACKAGE_SIGNING]: a web bundle has no signature to make")
    );

    let s = Scratch::new(
        "keystore",
        &format!("{APP}\n[package.android]\nkeystore = \"release.jks\"\nkey_alias = \"upload\"\n"),
    );
    fs::write(s.0.join("release.jks"), "").unwrap();
    let out = viso(&s.0, &["package", "android", "--dry-run"]);
    assert_eq!(out.status.code(), Some(6));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("VISO_ANDROID_KEYSTORE_PASSWORD"),
        "{stderr}"
    );
    assert!(!s.0.join("target").exists());
}
