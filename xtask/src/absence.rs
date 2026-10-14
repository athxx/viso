//! `cargo xtask check-release-absence`: prove a release artifact carries no
//! development layer (`Viso_Hot_Reload.md` §1, §58, §64).
//!
//! ```text
//! cargo xtask check-release-absence [-p <package>] [--target host|ios-sim|android|web]
//!     [--no-launch]
//! ```
//!
//! The package (default `viso-example-i18n`, an app that mounts a `view!`) is
//! built for the target (default the host) three ways, each checked against
//! the same marker list — the dev layer's env names, its thread name, its
//! patch checks and the failure overlay's text; on the web, where the dev
//! link runs on the loop's thread and the module keeps no symbols, there is
//! no link thread or type to name:
//!
//! 1. a dev artifact (`--release --features viso/hot-reload`, `VISO_PROFILE=dev`)
//!    must contain every marker, so a marker that stopped matching fails here
//!    instead of passing the release scan vacuously; and, since the host
//!    compiles every patch, none of the `.vs` compiler's symbols;
//! 2. the same build with `VISO_PROFILE=shipping` must be refused by the
//!    facade's build script;
//! 3. the release artifact (`VISO_PROFILE=release`, no feature) must contain
//!    none.
//!
//! Then, for the host target unless `--no-launch`, both binaries are
//! launched with the dev channel's environment, `VISO_DEV_RUNTIME` pointing
//! at a loopback listener: the dev artifact must connect (the control, which
//! needs a machine that can open a window), the release artifact must stay
//! up three times as long and never connect. Other targets are scanned only:
//! their artifact is what the packager wraps — the iOS simulator executable,
//! the Android shared library, the web's wasm module.
//!
//! Symbols are read from the artifact itself, so the scan needs a platform
//! that keeps them there (macOS, Linux, and every cross target); a Windows
//! executable keeps them in its PDB.

use std::io::ErrorKind;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use viso_package::{android, apple, web};

/// The dev link's reader and writer thread, which the web's link has none
/// of, and its type, whose name a wasm module keeps no symbol for.
const LINK_THREAD: &str = "viso-dev-link";
const LINK_TYPE: &str = "DevLink";

/// What only the dev layer puts in a binary.
const MARKERS: &[&str] = &[
    // The dev channel's environment, read only by the dev link.
    "VISO_DEV_RUNTIME",
    "VISO_DEV_TOKEN",
    "VISO_DEV_SESSION",
    "VISO_DEV_BUILD",
    // The runtime's patch checks, and its stage of a candidate view.
    "NACK_REVISION_MISMATCH",
    "NACK_UNLOADABLE_VIEW",
    // The dev link's thread and type.
    LINK_THREAD,
    LINK_TYPE,
    // The failure overlay.
    "Hot reload failed",
    // The snapshot endpoint: the restore launch's environment and window,
    // its refusal, and its wait.
    "VISO_DEV_RESTORE",
    "VISO_DEV_WINDOW",
    "VISO_DEV_PREWARM",
    "NACK_UNRESTORABLE_SNAPSHOT",
    "no restore arrived in time",
];

/// The `.vs` compiler, which no artifact links: `view!` expands at build
/// time, and a dev artifact applies the patches the host compiled.
const COMPILER: &[&str] = &["viso_dsl"];

/// How long the dev artifact may take to come up and connect.
const CONNECT_LIMIT: Duration = Duration::from_secs(60);

/// The least time the release artifact must run without connecting.
const QUIET_FLOOR: Duration = Duration::from_secs(5);

const USAGE: &str = "cargo xtask check-release-absence [-p <package>] \
[--target host|ios-sim|android|web] [--no-launch]";

/// What the artifact is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Host,
    IosSim,
    Android,
    Web,
}

impl Target {
    fn parse(name: &str) -> Option<Target> {
        Some(match name {
            "host" => Target::Host,
            "ios-sim" => Target::IosSim,
            "android" => Target::Android,
            "web" => Target::Web,
            _ => return None,
        })
    }

    /// The markers a dev artifact for it must carry.
    fn markers(self) -> Vec<&'static str> {
        MARKERS
            .iter()
            .copied()
            .filter(|&m| self != Target::Web || (m != LINK_THREAD && m != LINK_TYPE))
            .collect()
    }
}

struct Options {
    package: String,
    target: Target,
    launch: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        package: "viso-example-i18n".to_owned(),
        target: Target::Host,
        launch: true,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-p" | "--package" => {
                options.package = args.next().ok_or("-p needs a package")?.clone();
            }
            "--target" => {
                let name = args.next().ok_or("--target needs a target")?;
                options.target = Target::parse(name)
                    .ok_or_else(|| format!("unknown target {name:?}\nusage: {USAGE}"))?;
            }
            "--no-launch" => options.launch = false,
            other => return Err(format!("unknown argument {other:?}\nusage: {USAGE}")),
        }
    }
    Ok(options)
}

pub(crate) fn check_release_absence(args: &[String]) -> ExitCode {
    let result = parse(args).and_then(|o| check(&o));
    match result {
        Ok(()) => {
            println!("check-release-absence: OK");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("check-release-absence: {error}");
            ExitCode::FAILURE
        }
    }
}

fn check(o: &Options) -> Result<(), String> {
    let markers = o.target.markers();
    let dev = build(&o.package, "dev", true, o.target)?;
    let missing = scan_for(&dev.executable, &markers)?
        .into_iter()
        .filter(|(_, found)| !found)
        .map(|(m, _)| m)
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "the dev artifact lacks {missing:?}: a marker no longer names the dev layer"
        ));
    }
    println!("dev artifact: all {} markers present", markers.len());
    let linked = scan_for(&dev.executable, COMPILER)?
        .into_iter()
        .filter(|(_, found)| *found)
        .map(|(m, _)| m)
        .collect::<Vec<_>>();
    if !linked.is_empty() {
        return Err(format!("the dev artifact links the compiler: {linked:?}"));
    }
    println!("dev artifact: no compiler linked");
    let dev = keep(&dev.executable, "dev")?;

    match build(&o.package, "shipping", true, o.target) {
        Ok(_) => return Err("a shipping build accepted `viso/hot-reload`".to_owned()),
        Err(error) if error.contains("development-only") => {
            println!("shipping build: `viso/hot-reload` refused");
        }
        Err(error) => return Err(format!("the shipping build failed otherwise: {error}")),
    }

    let release = build(&o.package, "release", false, o.target)?;
    let present = scan_for(&release.executable, &[MARKERS, COMPILER].concat())?
        .into_iter()
        .filter(|(_, found)| *found)
        .map(|(m, _)| m)
        .collect::<Vec<_>>();
    if !present.is_empty() {
        return Err(format!("the release artifact contains {present:?}"));
    }
    println!("release artifact: no marker present");

    if o.launch && o.target == Target::Host {
        // The app runs from its package root, as `viso run` starts it.
        let root = &release.package_dir;
        let connected_after = launch(&dev, root, Wait::Connect)?;
        println!("dev artifact: connected after {connected_after:.1?}");
        let quiet = launch(
            &release.executable,
            root,
            Wait::Quiet((connected_after * 3).max(QUIET_FLOOR)),
        )?;
        println!("release artifact: ran {quiet:.1?} without connecting");
    }
    Ok(())
}

/// A built executable and the directory of the package it belongs to.
struct Artifact {
    executable: PathBuf,
    package_dir: PathBuf,
}

/// Builds `package` in release for `target` as a `profile` artifact, with
/// or without the dev layer: an Android one linked as the shell's library.
fn build(
    package: &str,
    profile: &str,
    hot_reload: bool,
    target: Target,
) -> Result<Artifact, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    let rustc = target == Target::Android;
    command
        .arg(if rustc { "rustc" } else { "build" })
        .args(["--release", "--message-format=json", "-p", package])
        .env("VISO_PROFILE", profile)
        .stderr(Stdio::piped());
    match target {
        Target::Host => {}
        Target::IosSim => {
            command.args(["--target", apple::simulator_triple()]);
        }
        Target::Web => {
            command.args(["--target", web::TRIPLE]);
        }
        Target::Android => {
            let abi = android::Abi::Arm64;
            let sdk = android::Sdk::locate()?;
            command
                .args(["--target", abi.triple()])
                .envs(sdk.cargo_env(abi));
        }
    }
    if hot_reload {
        command.args(["--features", "viso/hot-reload"]);
    }
    if rustc {
        command.arg("--").args(android::RUSTC_ARGS);
    }
    let out = command.output().map_err(|e| format!("cargo: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        // The build script's refusal is a JSON message on stdout.
        return Err(format!(
            "the {profile} build failed: {}{}",
            String::from_utf8_lossy(&out.stderr),
            stdout
        ));
    }
    artifact(&stdout).ok_or_else(|| format!("{package} has no executable"))
}

/// The last executable in cargo's JSON messages, with its package directory.
fn artifact(messages: &str) -> Option<Artifact> {
    let field = |line: &str, key: &str| {
        let key = format!("\"{key}\":\"");
        let at = line.find(&key)? + key.len();
        let end = line[at..].find('"')?;
        Some(PathBuf::from(line[at..at + end].replace("\\\\", "\\")))
    };
    messages
        .lines()
        .filter(|l| l.contains("\"reason\":\"compiler-artifact\""))
        .filter_map(|l| {
            Some(Artifact {
                executable: field(l, "executable")?,
                package_dir: field(l, "manifest_path")?.parent()?.to_path_buf(),
            })
        })
        .next_back()
}

/// Copies the dev executable aside, since the release build overwrites it.
fn keep(binary: &Path, tag: &str) -> Result<PathBuf, String> {
    let name = binary.file_name().ok_or("an executable has a name")?;
    let kept = binary.with_file_name(format!("{tag}-{}", name.to_string_lossy()));
    std::fs::copy(binary, &kept).map_err(|e| format!("{}: {e}", kept.display()))?;
    Ok(kept)
}

/// Each of `markers` and whether `binary` contains it.
fn scan_for(binary: &Path, markers: &[&'static str]) -> Result<Vec<(&'static str, bool)>, String> {
    let bytes = std::fs::read(binary).map_err(|e| format!("{}: {e}", binary.display()))?;
    Ok(markers
        .iter()
        .map(|m| (*m, contains(&bytes, m.as_bytes())))
        .collect())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

enum Wait {
    /// The app must connect; returns how long it took.
    Connect,
    /// The app must run this long without connecting.
    Quiet(Duration),
}

/// Launches `binary` from `dir` with the dev channel's environment pointing at
/// a fresh loopback listener, and checks it against `wait`.
fn launch(binary: &Path, dir: &Path, wait: Wait) -> Result<Duration, String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let mut child = Command::new(binary)
        .current_dir(dir)
        .env("VISO_DEV_RUNTIME", addr.to_string())
        .env("VISO_DEV_TOKEN", "00000000000000000000000000000000")
        .env("VISO_DEV_SESSION", "00000000000000000000000000000001")
        .env("VISO_DEV_BUILD", "00000000000000000000000000000002")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", binary.display()))?;
    let start = Instant::now();
    let limit = match wait {
        Wait::Connect => CONNECT_LIMIT,
        Wait::Quiet(quiet) => quiet,
    };
    let outcome = loop {
        let connected = match listener.accept() {
            Ok(_) => true,
            Err(e) if e.kind() == ErrorKind::WouldBlock => false,
            Err(e) => break Err(e.to_string()),
        };
        match (&wait, connected) {
            (Wait::Connect, true) => break Ok(start.elapsed()),
            (Wait::Quiet(_), true) => {
                break Err(format!("{} connected to the dev channel", binary.display()));
            }
            _ => {}
        }
        if let Ok(Some(status)) = child.try_wait() {
            break Err(format!(
                "{} exited ({status}) before the check finished; on a machine that \
                 cannot open a window, pass --no-launch",
                binary.display()
            ));
        }
        if start.elapsed() >= limit {
            break match wait {
                Wait::Connect => Err(format!(
                    "{} did not connect within {limit:?}",
                    binary.display()
                )),
                Wait::Quiet(_) => Ok(start.elapsed()),
            };
        }
        thread::sleep(Duration::from_millis(20));
    };
    let _ = child.kill();
    let _ = child.wait();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_its_arguments() {
        let o = parse(&[]).unwrap();
        assert_eq!(
            (o.package.as_str(), o.target, o.launch),
            ("viso-example-i18n", Target::Host, true)
        );
        let args = ["-p", "app", "--target", "web", "--no-launch"].map(String::from);
        let o = parse(&args).unwrap();
        assert_eq!(
            (o.package.as_str(), o.target, o.launch),
            ("app", Target::Web, false)
        );
        assert!(parse(&["--release".to_owned()]).is_err());
        assert!(parse(&["--target", "ios"].map(String::from)).is_err());
    }

    #[test]
    fn the_web_names_no_link_thread_or_type() {
        assert!(Target::Host.markers().contains(&LINK_THREAD));
        let web = Target::Web.markers();
        assert!(!web.contains(&LINK_THREAD) && !web.contains(&LINK_TYPE));
        assert_eq!(web.len(), MARKERS.len() - 2);
    }

    #[test]
    fn reads_the_executable_and_its_package() {
        let messages = concat!(
            "{\"reason\":\"compiler-artifact\",\"manifest_path\":\"/w/crates/viso/Cargo.toml\",\"executable\":null}\n",
            "{\"reason\":\"compiler-artifact\",\"manifest_path\":\"/w/examples/i18n/Cargo.toml\",\"executable\":\"/t/release/app\"}\n",
            "{\"reason\":\"build-finished\",\"success\":true}\n",
        );
        let found = artifact(messages).expect("an executable");
        assert_eq!(found.executable, Path::new("/t/release/app"));
        assert_eq!(found.package_dir, Path::new("/w/examples/i18n"));
    }

    #[test]
    fn finds_a_marker_anywhere() {
        assert!(contains(b"\0\0VISO_DEV_TOKEN\0", b"VISO_DEV_TOKEN"));
        assert!(!contains(b"VISO_DEV_TOKE", b"VISO_DEV_TOKEN"));
    }
}
