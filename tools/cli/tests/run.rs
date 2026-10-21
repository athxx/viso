//! `viso run` options over a stand-in app that prints what it was launched
//! with and exits: `--env` reaches it, `--cwd` is where it runs, and the
//! options a target does not take, and `headless`, are usage errors; a
//! device target's session runs on its board over a fake SSH, or emulated
//! on a Mac, beside this machine's app in one session.

mod stand_in;

use std::fs;

use std::path::Path;

use stand_in::{events, field, project, text, viso, viso_with};

#[test]
fn the_app_gets_its_variables_and_working_directory() {
    let root = project("run-env");
    fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    println!(\"greeting={}\", std::env::var(\"GREETING\").unwrap_or_default());\n    \
         println!(\"cwd={}\", std::env::current_dir().unwrap().display());\n}\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("work")).unwrap();
    let out = viso(
        &root,
        &[
            "run",
            "--json",
            "--verbose",
            "--env",
            "GREETING=hi there",
            "--env",
            "API_TOKEN=t0p-s3cret",
            "--cwd",
            "work",
        ],
    );
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}{}", text(&out.stderr));
    let said: Vec<&str> = events(&stdout, "log")
        .into_iter()
        .filter(|line| field(line, "source") == Some("app"))
        .filter_map(|line| field(line, "message"))
        .collect();
    let work = root.join("work").canonicalize().unwrap();
    assert!(said.contains(&"greeting=hi there"), "{said:?}");
    assert!(
        said.iter()
            .any(|line| line.strip_prefix("cwd=").is_some_and(|dir| {
                std::path::Path::new(dir)
                    .canonicalize()
                    .is_ok_and(|dir| dir == work)
            })),
        "{said:?}"
    );
    assert!(stdout.contains("API_TOKEN=<hidden>"), "{stdout}");
    assert!(!stdout.contains("t0p-s3cret"), "a secret is never printed");
    let artifact = events(&stdout, "artifact");
    assert_eq!(artifact.len(), 1, "{stdout}");
    assert_eq!(field(artifact[0], "kind"), Some("binary"));
}

#[test]
fn an_option_the_target_does_not_take_is_a_usage_error() {
    let root = project("run-usage");
    for (args, said) in [
        (&["run", "--open"][..], "`--open` is a web option"),
        (
            &["run", "ios", "--cwd", "."],
            "`--cwd` is the working directory",
        ),
        (
            &["run", "web-gpu", "--open", "--browser", "none"],
            "opposite",
        ),
        (&["run", "--cwd", "missing"], "is not a directory"),
        (
            &["run", "--env", "VISO_DEV_TOKEN=x"],
            "the dev session's own",
        ),
        (
            &["run", "--profile", "release"],
            "viso build --profile release",
        ),
        (&["run", "headless"], "viso test ui"),
        (&["run", "web-gpu", "web-gpu"], "named twice"),
        (&["run", "ios", "android"], "one simulator or emulator"),
        (&["run", "web-gpu", "web-dom"], "one web target"),
    ] {
        let out = viso(&root, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let stderr = text(&out.stderr);
        assert!(stderr.contains(said), "{args:?}: {stderr}");
    }
}

/// The app's `Cargo.toml` as `viso new` writes it: the facade's defaults
/// through the app's own.
const CARGO: &str = "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
                     [features]\ndefault = [\"viso/vm\", \"viso/desktop\", \"viso/native-vulkan\", \"viso/rasterizer\", \"viso/complex-line-break\"]\n\
                     [dependencies]\nviso = { path = \"viso\", default-features = false }\n[workspace]\n";

/// What the app logged, in order.
fn app_lines(stdout: &str) -> Vec<&str> {
    events(stdout, "log")
        .into_iter()
        .filter(|line| field(line, "source") == Some("app"))
        .filter_map(|line| field(line, "message"))
        .collect()
}

/// A fake `ssh` and `scp` in `dir`, playing a board whose home is
/// `board/home` beside them: `ssh` runs its command there, `scp` copies into
/// it. Each call is logged to `board/calls`.
#[cfg(unix)]
fn fake_ssh(dir: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let state = dir.join("board");
    fs::create_dir_all(state.join("home")).unwrap();
    let tools = dir.join("fake-bin");
    fs::create_dir_all(&tools).unwrap();
    let ssh = r#"#!/bin/sh
echo "ssh $*" >> "__STATE__/calls"
while [ "$#" -gt 0 ]; do
    case "$1" in
    -o|-p) shift 2 ;;
    --) shift; break ;;
    *) shift ;;
    esac
done
cd "__STATE__/home" || exit 255
SSH_CONNECTION="127.0.0.1 50022 127.0.0.1 22" exec sh -c "$*"
"#;
    let scp = r#"#!/bin/sh
echo "scp $*" >> "__STATE__/calls"
recursive=""
while [ "$#" -gt 2 ]; do
    case "$1" in
    -o|-P) shift 2 ;;
    -r) recursive="-R"; shift ;;
    *) shift ;;
    esac
done
exec cp $recursive "$1" "__STATE__/home/${2#*:}"
"#;
    for (name, script) in [("ssh", ssh), ("scp", scp)] {
        let path = tools.join(name);
        fs::write(
            &path,
            script.replace("__STATE__", &state.display().to_string()),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    format!(
        "{}:{}",
        tools.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[cfg(unix)]
#[test]
fn a_board_with_deploy_runs_the_session_over_ssh() {
    let root = project("run-board");
    fs::write(root.join("Cargo.toml"), CARGO).unwrap();
    fs::write(
        root.join("Viso.toml"),
        "[package]\nname = \"probe\"\nversion = \"1.2.0\"\n\n\
         [target.kiosk]\nprofile = \"embedded-cpu\"\ndeploy = \"tester@127.0.0.1:2222\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("assets")).unwrap();
    fs::write(root.join("assets/logo.txt"), "logo").unwrap();
    fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    let var = |name: &str| std::env::var(name).unwrap_or_default();\n    \
         println!(\"runtime={}\", var(\"VISO_DEV_RUNTIME\"));\n    \
         println!(\"cwd={}\", std::env::current_dir().unwrap().file_name().unwrap().to_string_lossy());\n    \
         println!(\"logo={}\", std::fs::read_to_string(\"assets/logo.txt\").unwrap_or_default());\n    \
         println!(\"{} {} {}\", viso::CPU, viso::KMS, viso::HOT_RELOAD);\n}\n",
    )
    .unwrap();
    let path = fake_ssh(&root);
    let out = viso_with(&root, &["run", "kiosk", "--json"], &[("PATH", path)]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}{}", text(&out.stderr));
    let said = app_lines(&stdout);
    assert!(
        said.iter()
            .any(|line| line.starts_with("runtime=127.0.0.1:")),
        "the dev channel on the route to the board: {said:?}"
    );
    assert!(said.contains(&"cwd=probe"), "{said:?}");
    assert!(
        said.contains(&"logo=logo"),
        "the assets beside it: {said:?}"
    );
    assert!(said.contains(&"true true true"), "{said:?}");
    assert!(
        stdout.contains("unencrypted on the local network"),
        "{stdout}"
    );
    let calls = fs::read_to_string(root.join("board/calls")).unwrap();
    assert!(calls.contains("-p 2222 tester@127.0.0.1 --"), "{calls}");
    assert!(
        root.join("board/home/viso-dev/probe/probe").is_file(),
        "{calls}"
    );

    // `--cwd` is this machine's.
    let out = viso(&root, &["run", "kiosk", "--cwd", "."]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
}

#[test]
fn an_embedded_cpu_target_is_emulated_on_a_mac() {
    let root = project("run-emulate");
    fs::write(root.join("Cargo.toml"), CARGO).unwrap();
    fs::write(
        root.join("Viso.toml"),
        "[package]\nname = \"probe\"\nversion = \"1.2.0\"\n\n\
         [target.kiosk]\nprofile = \"embedded-cpu\"\ntriple = \"aarch64-unknown-linux-gnu\"\n\
         display = { width = 480, height = 272, format = \"rgb565\" }\n\n\
         [target.panel]\nprofile = \"embedded-gpu\"\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    println!(\"{} {} {} {} {} {}\", option_env!(\"VISO_FOOTPRINT\").unwrap_or(\"-\"), \
         option_env!(\"VISO_KMS_MODE\").unwrap_or(\"-\"), viso::CPU, viso::EMULATE, viso::HOT_RELOAD, \
         viso::DESKTOP);\n}\n",
    )
    .unwrap();
    let out = viso(&root, &["run", "kiosk", "--emulate", "--json"]);
    let stdout = text(&out.stdout);
    if cfg!(target_os = "macos") {
        assert_eq!(out.status.code(), Some(0), "{stdout}{}", text(&out.stderr));
        assert_eq!(
            app_lines(&stdout),
            ["embedded-cpu 480x272 true true true false"],
            "the board's profile and panel, on this machine, emulated and reloading"
        );
    } else {
        assert_eq!(out.status.code(), Some(3), "{stdout}");
        assert!(stdout.contains("ENV_EMULATE"), "{stdout}");
    }

    // A GPU board's driver is not on a desktop.
    let out = viso(&root, &["run", "panel", "--emulate"]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("error[ENV_EMULATE]"),
        "{}",
        text(&out.stderr)
    );
    // `--emulate` is a device target's.
    let out = viso(&root, &["run", "--emulate"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
}

/// This machine and an emulated board run side by side in one session:
/// each lane builds its own artifact, under its own build id, and its app's
/// lines are named by its target.
#[test]
fn a_session_runs_this_machine_beside_an_emulated_board() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let root = project("run-lanes");
    fs::write(root.join("Cargo.toml"), CARGO).unwrap();
    fs::write(
        root.join("Viso.toml"),
        "[package]\nname = \"probe\"\nversion = \"1.2.0\"\n\n\
         [target.kiosk]\nprofile = \"embedded-cpu\"\n\
         display = { width = 480, height = 272, format = \"rgb565\" }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    println!(\"{} {}\", viso::EMULATE, viso::HOT_RELOAD);\n}\n",
    )
    .unwrap();
    let out = viso(&root, &["run", "kiosk", "--emulate", "--desktop", "--json"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}{}", text(&out.stderr));
    let mut lines = app_lines(&stdout);
    lines.sort_unstable();
    assert_eq!(
        lines,
        ["[host] false true", "[kiosk] true true"],
        "{stdout}"
    );
    let artifacts = events(&stdout, "artifact");
    assert_eq!(artifacts.len(), 2, "{stdout}");
    assert_ne!(
        field(artifacts[0], "build_id"),
        field(artifacts[1], "build_id"),
        "each lane runs a build of its own"
    );
}
