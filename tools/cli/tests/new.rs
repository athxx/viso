//! `viso new` end to end: the files each template writes, the directories it
//! refuses and the ones it fills without replacing a file, the name it takes,
//! the framework it depends on, Git, and a failed write leaving nothing
//! behind. The acceptance test builds and tests each template against the
//! workspace's framework through `--framework`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        // Outside the workspace, so Cargo does not take a new package for
        // one of its members.
        let dir =
            std::env::temp_dir().join(format!("viso-cli-new-{label}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn viso(cwd: &Path, args: &[&str]) -> Output {
    viso_with(cwd, args, &[])
}

fn viso_with(cwd: &Path, args: &[&str], env: &[(&str, &std::ffi::OsStr)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_viso"));
    command.args(args).current_dir(cwd);
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().unwrap()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

const TEMPLATES: [&str; 4] = ["app", "game", "web", "library"];

#[test]
fn an_app_has_its_files_and_says_what_to_run_next() {
    let s = Scratch::new("app");
    let out = viso(&s.0, &["new", "hello", "--no-git"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Created the app `hello` in hello\n\nNext:\n  cd hello\n  viso run\n  viso test\n"
    );
    let root = s.0.join("hello");
    let cargo = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(cargo.contains("name = \"hello\""), "{cargo}");
    assert!(cargo.contains("edition = \"2024\""), "{cargo}");
    assert!(
        cargo.contains(&format!(
            "viso = {{ version = \"{}\", default-features = false }}",
            env!("CARGO_PKG_VERSION")
        )),
        "{cargo}"
    );
    assert!(
        cargo.contains("default = [\"viso/vm\", \"viso/desktop\", \"viso/native-vulkan\", \"viso/rasterizer\", \"viso/complex-line-break\"]"),
        "the app turns the facade's defaults on through its own: {cargo}"
    );
    let manifest = fs::read_to_string(root.join("Viso.toml")).unwrap();
    assert!(manifest.contains("name = \"hello\""), "{manifest}");
    assert!(manifest.contains("language = \"1.0\""), "{manifest}");
    assert!(root.join("assets").is_dir());
    let font = fs::read(root.join("assets/fonts/NotoSans-Regular.ttf")).unwrap();
    assert_eq!(font[..4], [0, 1, 0, 0], "a TrueType face, as it was");
    assert!(root.join("assets/fonts/OFL.txt").is_file());
    assert!(!root.join(".git").exists(), "`--no-git`");
    // Nothing is left beside it.
    let beside: Vec<_> = fs::read_dir(&s.0).unwrap().collect();
    assert_eq!(beside.len(), 1);

    let out = viso(
        &s.0,
        &[
            "new",
            "lib",
            "--template",
            "library",
            "--edition",
            "2021",
            "--no-git",
            "--json",
        ],
    );
    assert_eq!(code(&out), 0, "{}", stdout(&out));
    let json = stdout(&out);
    let result = json
        .lines()
        .find(|line| line.contains("\"type\":\"result\""))
        .unwrap();
    assert!(result.contains("\"template\":\"library\""), "{result}");
    assert!(
        result.contains(
            "\"files\":[\".gitignore\",\"Cargo.toml\",\"Viso.toml\",\"i18n/en.toml\",\
             \"src/counter.vs\",\"src/lib.rs\"]"
        ),
        "{result}"
    );
    assert!(json.lines().any(|l| l.contains("\"type\":\"progress\"")));
    assert!(
        fs::read_to_string(s.0.join("lib/Cargo.toml"))
            .unwrap()
            .contains("edition = \"2021\"")
    );
}

#[test]
fn every_template_passes_strict_checks_as_created() {
    let s = Scratch::new("check");
    for template in TEMPLATES {
        let out = viso(&s.0, &["new", template, "--template", template, "--no-git"]);
        assert_eq!(code(&out), 0, "{template}: {}", stderr(&out));
        let out = viso(
            &s.0.join(template),
            &["check", "--a11y", "strict", "--i18n", "strict"],
        );
        assert_eq!(code(&out), 0, "{template}: {}", stderr(&out));
        assert_eq!(stderr(&out), "", "{template}");
    }
}

#[test]
fn a_directory_with_files_is_refused_and_forcing_replaces_none() {
    let s = Scratch::new("taken");
    let root = s.0.join("taken");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("notes.txt"), "mine").unwrap();
    fs::write(root.join("src/other.rs"), "mine").unwrap();

    let out = viso(&s.0, &["new", "taken", "--no-git", "--json"]);
    assert_eq!(code(&out), 2);
    let json = stdout(&out);
    assert!(json.contains("\"code\":\"NEW_PATH_TAKEN\""), "{json}");
    assert!(json.contains("it holds `notes.txt`, `src`"), "{json}");
    assert!(
        json.contains("viso new taken --force-empty-dir"),
        "the fix: {json}"
    );
    assert!(!root.join("Cargo.toml").exists());

    let out = viso(&s.0, &["new", "taken", "--no-git", "--force-empty-dir"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(fs::read_to_string(root.join("notes.txt")).unwrap(), "mine");
    assert_eq!(
        fs::read_to_string(root.join("src/other.rs")).unwrap(),
        "mine"
    );
    assert!(root.join("src/app.vs").is_file());
    let staged: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(staged.is_empty(), "the staging directory is gone");

    // A second time every file is there already.
    fs::write(root.join("src/app.vs"), "edited").unwrap();
    let out = viso(&s.0, &["new", "taken", "--no-git", "--force-empty-dir"]);
    assert_eq!(code(&out), 2);
    assert!(
        stderr(&out).contains("`taken` holds files the project would write"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        fs::read_to_string(root.join("src/app.vs")).unwrap(),
        "edited"
    );

    // A file where the directory would be.
    fs::write(s.0.join("plain"), "").unwrap();
    let out = viso(&s.0, &["new", "plain/app", "--no-git"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(stderr(&out).contains("is a file, not a directory"));
}

#[test]
fn the_current_directory_gives_its_name_which_must_be_a_package_name() {
    let s = Scratch::new("here");
    let here = s.0.join("my-game");
    fs::create_dir_all(&here).unwrap();
    let out = viso(&here, &["new", ".", "--template", "game", "--no-git"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Created the game `my-game` here\n\nNext:\n  viso run\n  viso test game\n"
    );
    assert!(
        fs::read_to_string(here.join("Cargo.toml"))
            .unwrap()
            .contains("name = \"my-game\"")
    );

    let odd = s.0.join("My App");
    fs::create_dir_all(&odd).unwrap();
    let out = viso(&odd, &["new", ".", "--no-git", "--json"]);
    assert_eq!(code(&out), 2);
    let json = stdout(&out);
    assert!(json.contains("\"code\":\"NEW_NAME_INVALID\""), "{json}");
    assert!(json.contains("viso new . --name my_app"), "{json}");
    assert_eq!(fs::read_dir(&odd).unwrap().count(), 0);

    let out = viso(&odd, &["new", ".", "--no-git", "--name", "my_app"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    let out = viso(&s.0, &["new", "x", "--name", "fn"]);
    assert_eq!(code(&out), 2);
    assert!(
        stderr(&out).contains("it is a Rust keyword"),
        "{}",
        stderr(&out)
    );
    assert!(!s.0.join("x").exists());
}

#[test]
fn git_makes_it_a_repository_and_its_absence_is_a_warning() {
    let s = Scratch::new("git");
    let has_git = Command::new("git").arg("--version").output().is_ok();
    if has_git {
        let out = viso(&s.0, &["new", "tracked"]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        assert!(s.0.join("tracked/.git").is_dir());
        // Inside a repository the project is that repository's.
        let out = viso(&s.0.join("tracked"), &["new", "inner"]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        assert!(!s.0.join("tracked/inner/.git").exists());
    }

    let empty = s.0.join("no-tools");
    fs::create_dir_all(&empty).unwrap();
    let out = viso_with(&s.0, &["new", "untracked"], &[("PATH", empty.as_os_str())]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("warning[NEW_GIT]"), "{err}");
    assert!(err.contains("`--no-git`"), "{err}");
    assert!(s.0.join("untracked/Cargo.toml").is_file());
}

#[cfg(unix)]
#[test]
fn a_failed_write_leaves_nothing_behind() {
    use std::os::unix::fs::PermissionsExt;

    let s = Scratch::new("readonly");
    let locked = s.0.join("locked");
    fs::create_dir_all(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    // Root writes anywhere; the test means nothing then.
    if fs::write(locked.join("probe"), "").is_ok() {
        let _ = fs::remove_file(locked.join("probe"));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let out = viso(&s.0, &["new", "locked/deep/app", "--no-git"]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(code(&out), 7, "{}", stderr(&out));
    assert!(
        stderr(&out).contains("error[NEW_WRITE]"),
        "{}",
        stderr(&out)
    );
    assert_eq!(fs::read_dir(&locked).unwrap().count(), 0);
}

#[test]
fn a_framework_path_is_the_dependency_and_must_be_the_viso_crate() {
    let s = Scratch::new("framework");
    let checkout = s.0.join("viso");
    fs::create_dir_all(checkout.join("crates/viso")).unwrap();
    fs::write(checkout.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    fs::write(
        checkout.join("crates/viso/Cargo.toml"),
        "[package]\nname = \"viso\"\n",
    )
    .unwrap();
    let out = viso(
        &s.0,
        &["new", "apps/one", "--no-git", "--framework", "viso"],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let cargo = fs::read_to_string(s.0.join("apps/one/Cargo.toml")).unwrap();
    assert!(
        cargo.contains("viso = { path = \"../../viso/crates/viso\", default-features = false }"),
        "a checkout names its crate, from the project: {cargo}"
    );

    fs::write(s.0.join("Cargo.toml"), "[package]\nname = \"other\"\n").unwrap();
    let out = viso(
        &s.0,
        &["new", "two", "--no-git", "--framework", ".", "--json"],
    );
    assert_eq!(code(&out), 2);
    let json = stdout(&out);
    assert!(json.contains("\"code\":\"NEW_FRAMEWORK\""), "{json}");
    assert!(json.contains("the package `other`"), "{json}");
    assert!(!s.0.join("two").exists(), "nothing is written");

    let out = viso(&s.0, &["new", "three", "--no-git", "--framework", "apps"]);
    assert_eq!(code(&out), 2);
    assert!(
        stderr(&out).contains("it holds no `Cargo.toml`"),
        "{}",
        stderr(&out)
    );
}

/// `new → check → build headless → test` for every template, against the
/// workspace's framework through `--framework`, offline.
#[test]
fn every_template_builds_and_passes_its_tests() {
    let s = Scratch::new("accept");
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("new-acceptance");
    let env = [("CARGO_TARGET_DIR", target.as_os_str())];
    for template in TEMPLATES {
        let framework = checkout.to_str().unwrap();
        let args = [
            "new",
            template,
            "--template",
            template,
            "--no-git",
            "--framework",
            framework,
        ];
        let out = viso(&s.0, &args);
        assert_eq!(code(&out), 0, "{template}: {}", stderr(&out));
        let root = s.0.join(template);
        let out = viso_with(&root, &["--offline", "check"], &env);
        assert_eq!(code(&out), 0, "{template} check: {}", stderr(&out));
        if template != "library" {
            let out = viso_with(&root, &["--offline", "build", "headless"], &env);
            assert_eq!(code(&out), 0, "{template} build: {}", stderr(&out));
        }
        let out = viso_with(&root, &["--offline", "test", "--json"], &env);
        let json = stdout(&out);
        assert_eq!(code(&out), 0, "{template} test: {json}\n{}", stderr(&out));
        let passed = json
            .lines()
            .filter(|l| l.contains("\"type\":\"test\"") && l.contains("\"status\":\"pass\""))
            .count();
        let expected = match template {
            "game" => 2,
            "library" => 0,
            _ => 1,
        };
        assert_eq!(passed, expected, "{template}: {json}");
    }
}
