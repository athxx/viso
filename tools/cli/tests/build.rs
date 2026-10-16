//! `viso build` end to end over a small Cargo package whose `viso` is a
//! stand-in: each profile's artifact in `dist/` with its manifest, the
//! profile reaching the build, compile errors as diagnostics with their
//! spans, and what stops a build before Cargo runs.

mod stand_in;

use std::fs;
use std::path::Path;
use std::process::Command;

use stand_in::{events, field, project, text, viso};

/// Runs the built app at `path` under `root`, and what it printed.
fn ran(root: &Path, path: &str) -> String {
    let out = Command::new(root.join(path)).output().unwrap();
    assert!(out.status.success());
    text(&out.stdout).trim().to_owned()
}

#[test]
fn a_dev_build_lands_in_dist_with_its_manifest() {
    let root = project("dev");
    let out = viso(&root, &["build", "--json"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}{}", text(&out.stderr));
    let artifacts = events(&stdout, "artifact");
    assert_eq!(artifacts.len(), 2, "{stdout}");
    let binary = artifacts[0];
    assert_eq!(field(binary, "kind"), Some("binary"));
    assert_eq!(field(binary, "profile"), Some("dev"));
    let path = field(binary, "path").unwrap();
    let platform = field(binary, "target").unwrap();
    assert!(
        path.starts_with(&format!("dist/{platform}/probe")),
        "{path}"
    );
    assert!(field(binary, "hash").unwrap().starts_with("sha256:"));
    assert_eq!(field(artifacts[1], "kind"), Some("manifest"));
    assert!(stdout.contains("\"artifact_count\":2"), "{stdout}");

    assert_eq!(
        ran(&root, path),
        "dev true",
        "a dev build has the Dev Runtime"
    );
    let build_id = field(binary, "build_id").unwrap();
    assert_eq!(build_id.len(), 32);
    assert!(
        root.join("target/viso/build")
            .join(build_id)
            .join("out")
            .is_dir()
    );
    let manifest = fs::read_to_string(root.join(format!("dist/{platform}/artifact.json"))).unwrap();
    for part in [
        "\"project\":\"probe\"".to_owned(),
        "\"version\":\"1.2.0\"".to_owned(),
        format!("\"build_id\":\"{build_id}\""),
        format!("\"hash\":\"{}\"", field(binary, "hash").unwrap()),
        "\"path\":\"probe".to_owned(),
    ] {
        assert!(manifest.contains(&part), "{part} in {manifest}");
    }
}

#[test]
fn each_profile_builds_as_itself() {
    let root = project("profiles");
    let out = viso(&root, &["build", "--release"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}{}", text(&out.stderr));
    assert!(stdout.starts_with("Built host (release) in "), "{stdout}");
    let row = stdout.lines().nth(1).unwrap();
    let path = row.split_whitespace().nth(1).unwrap();
    assert!(row.trim_start().starts_with("binary "), "{stdout}");
    assert_eq!(ran(&root, path), "release false");

    let out = viso(&root, &["build", "--profile", "shipping"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        ran(&root, path),
        "shipping false",
        "the output was replaced"
    );
    assert!(
        root.join("target/shipping").is_dir(),
        "Viso's own Cargo profile"
    );

    let out = viso(&root, &["build", "--release", "--profile", "dev"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("viso build --profile dev"), "{stderr}");
}

#[test]
fn a_compile_error_is_a_diagnostic_with_its_span() {
    let root = project("broken");
    fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    let x: u8 = \"no\";\n}\n",
    )
    .unwrap();
    let out = viso(&root, &["build", "--json"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(4), "{stdout}");
    let error = events(&stdout, "diagnostic")
        .into_iter()
        .find(|line| field(line, "code") == Some("E0308"))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert!(error.contains("\"file\":\"src/main.rs\""), "{error}");
    assert!(error.contains("\"line\":2"), "{error}");
    assert!(stdout.contains("\"code\":\"BUILD_FAILED\""), "{stdout}");
    assert!(!root.join("dist").exists(), "nothing is published");
}

#[test]
fn what_stops_a_build_before_cargo_runs() {
    // Sources that do not check.
    let root = project("unchecked");
    fs::write(
        root.join("src/app.vs"),
        "component App {\n    view { Text { text: ; } }\n}\n",
    )
    .unwrap();
    let out = viso(&root, &["build", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    assert!(!root.join("target").exists(), "Cargo never ran");

    // The build cache's lock held by another live command.
    let root = project("locked");
    let locks = root.join("target/viso/locks");
    fs::create_dir_all(&locks).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::write(
        locks.join("build-host.lock"),
        format!("1\n{now}\nbuild cache for `host`\n"),
    )
    .unwrap();
    let out = viso(&root, &["build"]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("build cache for `host` is already in use"),
        "{stderr}"
    );

    // A target this build cannot produce.
    let out = viso(&root, &["build", "web-dom"]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
}

#[test]
fn a_headless_build_is_apart_from_the_host_build() {
    let root = project("headless");
    fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    println!(\"{} {} {}\", option_env!(\"VISO_PROFILE\").unwrap_or(\"-\"), \
         viso::HOT_RELOAD, viso::HEADLESS);\n}\n",
    )
    .unwrap();
    let out = viso(&root, &["build", "headless", "--json"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}{}", text(&out.stderr));
    let binary = events(&stdout, "artifact")[0];
    assert_eq!(field(binary, "target"), Some("headless"));
    let path = field(binary, "path").unwrap();
    assert_eq!(path, "dist/headless/probe");
    assert_eq!(
        ran(&root, path),
        "dev false true",
        "headless, and with nothing to reload"
    );

    let out = viso(&root, &["build"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let host = fs::read_dir(root.join("dist"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|name| name != "headless")
        .unwrap();
    assert_eq!(ran(&root, &format!("dist/{host}/probe")), "dev true false");
    assert_eq!(
        ran(&root, path),
        "dev false true",
        "the host build left it as it was"
    );
}

/// Whether `program` runs here.
fn installed(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The name of the file in `dir` that starts with `prefix` and ends with
/// `suffix`, its fingerprint between.
fn bundled(dir: &Path, prefix: &str, suffix: &str) -> Option<String> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|name| {
            name.strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(suffix))
                .is_some_and(|hash| !hash.contains('.'))
        })
}

#[test]
fn a_web_build_is_a_bundle_shaped_by_its_profile() {
    let wasm32 = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .is_ok_and(|o| {
            text(&o.stdout)
                .lines()
                .any(|t| t == "wasm32-unknown-unknown")
        });
    if !wasm32 || !installed("wasm-bindgen") {
        eprintln!("skipped: no wasm32-unknown-unknown target or wasm-bindgen CLI");
        return;
    }
    // A module `wasm-bindgen` takes: the stand-in exports a function
    // through the version of the crate the CLI is.
    let version = text(
        &Command::new("wasm-bindgen")
            .arg("--version")
            .output()
            .unwrap()
            .stdout,
    );
    let version = version.trim().rsplit(' ').next().unwrap().to_owned();
    let root = project("web");
    fs::write(
        root.join("viso/Cargo.toml"),
        format!(
            "[package]\nname = \"viso\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
             [features]\nhot-reload = []\n[dependencies]\nwasm-bindgen = \"={version}\"\n"
        ),
    )
    .unwrap();
    fs::write(
        root.join("viso/src/lib.rs"),
        "#[wasm_bindgen::prelude::wasm_bindgen]\npub fn answer() -> u32 {\n    42\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    let _ = viso::answer();\n}\n",
    )
    .unwrap();
    // The repository's lock holds the versions its registry cache has.
    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    fs::copy(lock, root.join("Cargo.lock")).unwrap();
    let out = viso(&root, &["build", "web-gpu", "--json", "--offline"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    let kinds: Vec<&str> = events(&stdout, "artifact")
        .iter()
        .filter_map(|line| field(line, "kind"))
        .collect();
    assert_eq!(kinds, ["html", "js", "wasm", "manifest"], "{stdout}");
    assert!(root.join("dist/web/index.html").is_file());
    // The module and its glue are named by their content, and the page by
    // the names.
    let web = root.join("dist/web");
    let module = bundled(&web, "probe_bg.", ".wasm").expect("the module");
    let glue = bundled(&web, "probe.", ".js").expect("the glue");
    let page = fs::read_to_string(web.join("index.html")).unwrap();
    assert!(
        page.contains(&format!(
            r#"import init from "./{glue}"; init({{ module_or_path: "./{module}" }});"#
        )),
        "{page}"
    );
    assert_ne!(module, "probe_bg.wasm");

    // Shipping optimizes for size and precompresses; without `wasm-opt` it
    // cannot ship.
    let out = viso(
        &root,
        &[
            "build",
            "web-gpu",
            "--profile",
            "shipping",
            "--json",
            "--offline",
        ],
    );
    let stdout = text(&out.stdout);
    if installed("wasm-opt") {
        assert_eq!(out.status.code(), Some(0), "{stdout}");
        assert!(bundled(&web, "probe_bg.", ".wasm.br").is_some());
        assert!(bundled(&web, "probe_bg.", ".wasm.gz").is_some());
    } else {
        assert_eq!(out.status.code(), Some(3), "{stdout}");
        assert!(stdout.contains("\"code\":\"ENV_TOOL\""), "{stdout}");
        assert!(
            bundled(&web, "probe_bg.", ".wasm").is_some(),
            "the dev bundle stays"
        );
        assert!(bundled(&web, "probe_bg.", ".wasm.br").is_none());
    }

    // Release without `wasm-opt` builds, unoptimized, and says so.
    if !installed("wasm-opt") {
        let out = viso(
            &root,
            &["build", "web-gpu", "--release", "--json", "--offline"],
        );
        let stdout = text(&out.stdout);
        assert_eq!(out.status.code(), Some(0), "{stdout}");
        assert!(stdout.contains("the module is not optimized"), "{stdout}");
    }
}
