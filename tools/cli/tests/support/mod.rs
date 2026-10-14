//! A probe app built against this repository and `viso run --json` over it,
//! for the tests that drive a dev session end to end.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

/// The probe project `name`, written fresh under the tests' scratch directory
/// with `files` (path and bytes) and depending on this repository's crates by
/// path.
pub fn project(name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repo = repo.canonicalize().expect("the repository");
    let app = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(name)
        .join("app");
    let crate_path = |name: &str| repo.join("crates").join(name).display().to_string();
    let manifest = format!(
        "[package]\nname = \"{name}-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
         publish = false\n\n[dependencies]\nviso = {{ path = {:?} }}\n\
         viso-ui = {{ path = {:?} }}\nviso-view = {{ path = {:?} }}\n\n[workspace]\n",
        crate_path("viso"),
        crate_path("ui"),
        crate_path("view"),
        name = name.replace('_', "-"),
    );
    let viso_toml: &[u8] = b"[package]\nname = \"probe\"\n";
    // The repository's lock, so the probe builds against the versions the
    // repository does (and the `wasm-bindgen` its CLI matches).
    let lock = std::fs::read(repo.join("Cargo.lock")).expect("the repository's lock");
    for (path, bytes) in [
        ("Cargo.toml", manifest.as_bytes()),
        ("Viso.toml", viso_toml),
        ("Cargo.lock", &lock),
    ]
    .into_iter()
    .chain(files.iter().copied())
    {
        let path = app.join(path);
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the directory");
        std::fs::write(path, bytes).expect("the file");
    }
    app
}

/// `viso run --json` in a probe project, and the lines it writes.
pub struct Run {
    child: Child,
    pub lines: Receiver<String>,
}

impl Run {
    /// `viso run <args> --json` with `envs` set.
    pub fn start(app: &Path, args: &[&str], envs: &[(&str, &Path)]) -> Run {
        Run::command(app, "run", args, envs)
    }

    /// `viso <subcommand> <args> --json` with `envs` set.
    pub fn command(app: &Path, subcommand: &str, args: &[&str], envs: &[(&str, &Path)]) -> Run {
        let target = app.parent().expect("the scratch directory").join("target");
        let mut command = Command::new(env!("CARGO_BIN_EXE_viso"));
        command
            .arg(subcommand)
            .args(args)
            .arg("--json")
            .current_dir(app)
            .env("CARGO_TARGET_DIR", target);
        for (name, value) in envs {
            command.env(name, value);
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("viso runs");
        let stdout = child.stdout.take().expect("piped");
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                println!("{line}");
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Run { child, lines }
    }

    /// The next line `matches` accepts, within `within`.
    pub fn until(&self, within: Duration, what: &str, matches: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) if matches(&line) => return line,
                Ok(_) => {}
                Err(_) => panic!("no {what} within {within:?}"),
            }
        }
    }

    /// Waits for the app's first launch to connect to the session.
    pub fn connected(&self) {
        self.until(Duration::from_secs(900), "the dev channel", |line| {
            line.contains("dev runtime connected")
        });
    }

    /// Stops the session as Ctrl-C does; its exit code.
    pub fn stop(mut self) -> Option<i32> {
        let pid = libc::pid_t::try_from(self.child.id()).expect("a pid");
        // SAFETY: a plain call on the child not yet reaped.
        unsafe { libc::kill(pid, libc::SIGINT) };
        self.child.wait().ok()?.code()
    }
}

/// Whether `line` is a dev event of `stage` with `outcome`.
pub fn dev_event(line: &str, stage: &str, outcome: &str) -> bool {
    line.contains("\"type\":\"dev\"")
        && line.contains(&format!("\"stage\":\"{stage}\""))
        && line.contains(&format!("\"outcome\":\"{outcome}\""))
}
