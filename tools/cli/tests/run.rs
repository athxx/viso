//! `viso run` options over a stand-in app that prints what it was launched
//! with and exits: `--env` reaches it, `--cwd` is where it runs, and the
//! options a target does not take are usage errors.

mod stand_in;

use std::fs;

use stand_in::{events, field, project, text, viso};

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
    ] {
        let out = viso(&root, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let stderr = text(&out.stderr);
        assert!(stderr.contains(said), "{args:?}: {stderr}");
    }
}
