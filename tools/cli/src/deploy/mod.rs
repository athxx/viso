//! Where `viso run` runs the dev artifact, and how it is started, asked to
//! close, stopped and heard from there: only the
//! deploy differs between the desktop, the simulator and
//! the emulator; the dev session and its patches are the same.
//!
//! - The desktop: the executable runs as a child process with the dev
//!   variables in its environment; a close is a termination request.
//! - The iOS Simulator: the executable is wrapped in an `.app`, installed,
//!   and launched by `simctl`, which hands it the dev variables as its
//!   environment and stays attached to its console until it ends.
//! - The Android Emulator: the executable is linked as the Java shell's
//!   library and packed into an APK, installed, and launched with the dev
//!   variables as string extras of its intent; `adb reverse` gives it the
//!   host's loopback dev port as its own, a watcher on the device ends when
//!   its process does, and its log comes through logcat.
//!
//! - A browser: the executable is built for `wasm32-unknown-unknown` with
//!   its glue, and the web dev server ([`web`]) serves it with the dev
//!   variables in the page and carries its dev channel; a restart serves the
//!   new bundle and has the open pages load it.
//!
//! A restart installs the new artifact only once it is built and packaged
//! and the old app has handed over its snapshot, so the old app runs on
//! through the build.

pub(crate) mod web;

#[cfg(all(test, unix))]
mod android_fake;
#[cfg(all(test, unix))]
mod ios_fake;

use std::cell::Cell;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use viso_package::android::{self, Abi, Sdk, Shell};
use viso_package::{apple, web as bundle};
use viso_project::{DeviceProfile, Profile, Target};
use viso_view::dev::wire::{
    DEV_BUILD_ENV, DEV_PREWARM_ENV, DEV_RESTORE_ENV, DEV_RUNTIME_ENV, DEV_SESSION_ENV,
    DEV_TOKEN_ENV, DEV_WINDOW_ENV, WindowGeometry,
};

use crate::build::cargo::Package;
use crate::build::{RustPlan, rust_plan};
use crate::dev::build::{Stamp, stamp};
use crate::device::emulator::{self, Device};
use crate::device::simulator;
use crate::process::Launch;

/// The dev variables an app is launched with.
pub(crate) struct Vars<'a> {
    pub(crate) address: String,
    pub(crate) token: &'a str,
    pub(crate) session: &'a str,
    pub(crate) build_id: &'a str,
    /// The variables `--env` names, before the dev variables.
    pub(crate) env: &'a [(String, String)],
}

impl<'a> Vars<'a> {
    /// The variables of a launch that restores the app before it when
    /// `restore`, its window at `window`.
    fn of(&self, restore: bool, window: Option<WindowGeometry>) -> Vec<(&'a str, String)> {
        let mut vars: Vec<(&'a str, String)> = self
            .env
            .iter()
            .map(|(name, value)| (name.as_str(), value.clone()))
            .collect();
        vars.extend([
            (DEV_RUNTIME_ENV, self.address.clone()),
            (DEV_TOKEN_ENV, self.token.to_owned()),
            (DEV_SESSION_ENV, self.session.to_owned()),
            (DEV_BUILD_ENV, self.build_id.to_owned()),
        ]);
        if restore {
            vars.push((DEV_RESTORE_ENV, "1".to_owned()));
        }
        if let Some(window) = window {
            vars.push((DEV_WINDOW_ENV, window.to_env()));
        }
        vars
    }
}

/// How the dev artifact is built for a deploy.
pub(crate) struct BuildPlan {
    /// The Rust target, compiler arguments and environment.
    pub(crate) rust: RustPlan,
    /// What wraps the executable.
    pub(crate) package: Option<Package>,
}

/// Where the app runs.
pub(crate) enum Deploy {
    /// This machine; the app runs in `cwd`.
    Host { cwd: PathBuf },
    /// The simulator of `profile`.
    Simulator { profile: DeviceProfile },
    /// The emulator `device`; `reversed` is the port forwarded to the host.
    Emulator {
        sdk: Box<Sdk>,
        device: Device,
        shell: Shell,
        reversed: Option<u16>,
    },
    /// The pages `server` serves; the first launch opens `browser` (the
    /// default one when `None`) unless it is not to `open` one.
    Browser {
        server: web::WebServer,
        browser: Option<String>,
        open: bool,
        launched: bool,
    },
}

/// How the lines of one of the app's outputs become log lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lines {
    /// Each line as it is, at `info`.
    Out,
    /// A simulator app's console, at `info`, without the `<id>: <pid>` line
    /// `simctl` starts it with; the prefix of that line.
    Console(String),
    /// Each line as it is, at `warn`.
    Err,
    /// logcat's brief format, each line at its own priority.
    Logcat,
}

impl Lines {
    /// The level and text of `line`, `None` for a line that says nothing.
    pub(crate) fn read(&self, line: String) -> Option<(&'static str, String)> {
        match self {
            Lines::Out => Some(("info", line)),
            Lines::Console(banner) => {
                let pid = line.strip_prefix(banner.as_str());
                if pid.is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
                {
                    return None;
                }
                Some(("info", line))
            }
            Lines::Err => Some(("warn", line)),
            Lines::Logcat => {
                let (level, message) = emulator::log_line(&line)?;
                Some((level, message.to_owned()))
            }
        }
    }
}

/// How a running app is asked to close.
enum Stop {
    /// A termination request to the process.
    Signal,
    /// `simctl terminate`; the simulator's record of how the app ended,
    /// when its log streams.
    Simulator {
        udid: String,
        id: String,
        exits: Option<simulator::ExitWatch>,
    },
    /// `am force-stop`; the app's pid, to read how it ended.
    Emulator {
        device: Device,
        package: String,
        pid: u32,
    },
    /// Nothing: a page is not stopped from here.
    Page,
}

/// How an app ended.
pub(crate) enum Ended {
    /// Its process's status.
    Status(ExitStatus),
    /// What the device recorded: whether that is a clean end, and how it
    /// reads.
    Recorded { clean: bool, how: String },
}

impl Ended {
    /// Whether the app ended cleanly.
    pub(crate) fn success(&self) -> bool {
        match self {
            Ended::Status(status) => status.success(),
            Ended::Recorded { clean, .. } => *clean,
        }
    }
}

impl std::fmt::Display for Ended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ended::Status(status) => status.fmt(f),
            Ended::Recorded { how, .. } => f.write_str(how),
        }
    }
}

/// How long the record of an emulator app's end may take to appear after
/// its process is gone.
const RECORD_WAIT: Duration = Duration::from_secs(1);

/// A running app.
pub(crate) struct App {
    /// The process whose end is the app's: the app itself, `simctl`
    /// attached to its console, or a watcher on the emulator; none for a
    /// page, which ends when it is asked to.
    process: Option<Child>,
    /// Processes that live as long as it does: its logcat.
    companions: Vec<Child>,
    stop: Stop,
    /// Whether a page was asked to end.
    ended: Cell<bool>,
}

impl App {
    fn of(process: Child, companions: Vec<Child>, stop: Stop) -> App {
        App {
            process: Some(process),
            companions,
            stop,
            ended: Cell::new(false),
        }
    }

    /// How the app ended, once it did.
    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<Ended>> {
        let Some(process) = &mut self.process else {
            return Ok(self
                .ended
                .get()
                .then(|| Ended::Status(ExitStatus::default())));
        };
        let Some(status) = process.try_wait()? else {
            return Ok(None);
        };
        let recorded: Box<dyn Fn() -> Option<(bool, String)>> = match &self.stop {
            Stop::Emulator {
                device,
                package,
                pid,
            } => Box::new(|| device.exit(package, *pid)),
            Stop::Simulator {
                exits: Some(exits), ..
            } => Box::new(|| exits.record()),
            _ => return Ok(Some(Ended::Status(status))),
        };
        let since = Instant::now();
        loop {
            if let Some((clean, how)) = recorded() {
                return Ok(Some(Ended::Recorded { clean, how }));
            }
            if since.elapsed() >= RECORD_WAIT {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(Some(Ended::Status(status)))
    }

    /// Asks the app to close.
    pub(crate) fn close(&self) {
        match &self.stop {
            Stop::Signal => {
                if let Some(process) = &self.process {
                    signal(process);
                }
            }
            Stop::Simulator { udid, id, .. } => simulator::terminate(udid, id),
            Stop::Emulator {
                device, package, ..
            } => device.force_stop(package),
            Stop::Page => self.ended.set(true),
        }
    }

    /// Stops the app.
    pub(crate) fn kill(&mut self) {
        match &self.stop {
            Stop::Signal => {}
            Stop::Simulator { udid, id, .. } => simulator::terminate(udid, id),
            Stop::Emulator {
                device, package, ..
            } => device.force_stop(package),
            Stop::Page => self.ended.set(true),
        }
        if let Some(process) = &mut self.process {
            let _ = process.kill();
        }
    }

    /// The app's piped outputs, each with how its lines read.
    pub(crate) fn outputs(&mut self) -> Vec<(Box<dyn Read + Send>, Lines)> {
        let mut outputs: Vec<(Box<dyn Read + Send>, Lines)> = Vec::new();
        if let Some(process) = &mut self.process {
            if let Some(stdout) = process.stdout.take() {
                let lines = match &self.stop {
                    Stop::Simulator { id, .. } => Lines::Console(format!("{id}: ")),
                    _ => Lines::Out,
                };
                outputs.push((Box::new(stdout), lines));
            }
            if let Some(stderr) = process.stderr.take() {
                outputs.push((Box::new(stderr), Lines::Err));
            }
        }
        for companion in &mut self.companions {
            if let Some(stdout) = companion.stdout.take() {
                outputs.push((Box::new(stdout), Lines::Logcat));
            }
        }
        outputs
    }
}

impl Drop for App {
    /// The companions end with the app.
    fn drop(&mut self) {
        for companion in &mut self.companions {
            let _ = companion.kill();
            let _ = companion.wait();
        }
    }
}

/// Asks the process to close: a termination request, which an app handles
/// as a close of its windows.
fn signal(child: &Child) {
    #[cfg(unix)]
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: a plain call; the child is not yet reaped, so its id names
        // it.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    // The console's Ctrl-C reaches the app itself.
    #[cfg(not(unix))]
    let _ = child;
}

impl Deploy {
    /// How the dev artifact is built for this deploy.
    pub(crate) fn plan(&self) -> BuildPlan {
        match self {
            Deploy::Host { .. } => BuildPlan {
                rust: rust_plan(Target::Host, Profile::Dev, None),
                package: None,
            },
            Deploy::Simulator { .. } => BuildPlan {
                rust: rust_plan(Target::Ios, Profile::Dev, None),
                package: Some(once_per_executable(|exe, bin, dir| {
                    let app = apple::bundle(exe, dir, &apple::Info::dev(bin), true)?;
                    apple::sign(&app.path, None, None, &dir.join("sign"))?;
                    Ok(app.path)
                })),
            },
            Deploy::Emulator { sdk, shell, .. } => {
                let abi = Abi::host();
                let (sdk, shell) = (sdk.clone(), shell.clone());
                BuildPlan {
                    rust: rust_plan(Target::Android, Profile::Dev, Some(&sdk)),
                    package: Some(once_per_executable(move |exe, bin, dir| {
                        sdk.package(&android::Packing::dev(
                            exe,
                            abi,
                            dir,
                            bin,
                            &shell,
                            android::Flavor::Dev,
                        ))
                        .map(|apk| apk.path)
                    })),
                }
            }
            Deploy::Browser { .. } => {
                // Two bundles in turn: the one the open pages run stays whole
                // while the next is written.
                let turn = std::sync::atomic::AtomicBool::new(false);
                BuildPlan {
                    rust: rust_plan(Target::WebGpu, Profile::Dev, None),
                    package: Some(once_per_executable(move |exe, bin, dir| {
                        let slot = if turn.fetch_xor(true, std::sync::atomic::Ordering::Relaxed) {
                            "web-b"
                        } else {
                            "web-a"
                        };
                        let out = dir.join(slot).join(bin);
                        bundle::bindgen(exe, &out, bin)?;
                        bundle::write_index(&out, bin)?;
                        Ok(out)
                    })),
                }
            }
        }
    }

    /// Runs the new executable `artifact` once to exit as it starts, while
    /// the old app runs on: the desktop checks a newly linked executable at
    /// its first launch. `None` where there is nothing to warm.
    pub(crate) fn prewarm(
        &self,
        artifact: &Path,
        args: &[OsString],
    ) -> Option<std::io::Result<Child>> {
        let Deploy::Host { cwd } = self else {
            return None;
        };
        Some(
            Command::new(artifact)
                .args(args)
                .current_dir(cwd)
                .env(DEV_PREWARM_ENV, "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .launch(),
        )
    }

    /// Installs and starts `artifact` with `args` and the dev variables of
    /// `vars`, restoring the app before it when `restore`, its window at
    /// `window`; its outputs piped when `piped`.
    pub(crate) fn launch(
        &mut self,
        artifact: &Path,
        args: &[OsString],
        vars: &Vars<'_>,
        restore: bool,
        window: Option<WindowGeometry>,
        piped: bool,
    ) -> Result<App, String> {
        match self {
            Deploy::Host { cwd } => {
                let mut launch = Command::new(artifact);
                launch.args(args).current_dir(&*cwd);
                launch.envs(vars.of(restore, window));
                if piped {
                    launch.stdout(Stdio::piped()).stderr(Stdio::piped());
                }
                let process = launch
                    .launch()
                    .map_err(|e| format!("cannot launch `{}`: {e}", artifact.display()))?;
                Ok(App::of(process, Vec::new(), Stop::Signal))
            }
            Deploy::Simulator { profile } => {
                let udid = profile.device.clone();
                let id = apple::bundle_id(&artifact_name(artifact, ".app")?);
                simulator::install(&udid, artifact)?;
                // Without its log the session still runs; the app's end then
                // reads as `simctl`'s.
                let exits = simulator::ExitWatch::start(&udid, &id).ok();
                let process = simulator::launch(&udid, &id, args, &vars.of(restore, None), piped)
                    .map_err(|e| format!("cannot run `xcrun simctl launch`: {e}"))?;
                Ok(App::of(
                    process,
                    Vec::new(),
                    Stop::Simulator { udid, id, exits },
                ))
            }
            Deploy::Emulator {
                device, reversed, ..
            } => {
                let bin = artifact_name(artifact, ".apk")?;
                let package = android::package_name(&bin);
                device.install(artifact)?;
                let port = vars
                    .address
                    .rsplit(':')
                    .next()
                    .and_then(|p| p.parse::<u16>().ok())
                    .ok_or("the dev channel has no port")?;
                if *reversed != Some(port) {
                    device.reverse(port)?;
                    *reversed = Some(port);
                }
                let component = format!("{package}/{}", android::ACTIVITY);
                let pid = device.launch(&component, &package, &vars.of(restore, None))?;
                let process = device
                    .watch(pid)
                    .map_err(|e| format!("cannot watch the app: {e}"))?;
                let logcat = device
                    .logcat(pid)
                    .map_err(|e| format!("cannot read the app's log: {e}"))?;
                Ok(App::of(
                    process,
                    vec![logcat],
                    Stop::Emulator {
                        device: device.clone(),
                        package,
                        pid,
                    },
                ))
            }
            Deploy::Browser {
                server,
                browser,
                open,
                launched,
            } => {
                let page = vars
                    .of(false, None)
                    .into_iter()
                    .map(|(name, value)| (name.to_owned(), value))
                    .collect();
                server.serve(artifact.to_owned(), page, restore);
                if std::mem::replace(launched, true) {
                    // The open pages load the new build.
                    server.reload_pages();
                } else if *open {
                    web::open_browser(&server.url(), browser.as_deref())?;
                }
                Ok(App {
                    process: None,
                    companions: Vec::new(),
                    stop: Stop::Page,
                    ended: Cell::new(false),
                })
            }
        }
    }

    /// Undoes what the session set up on the device: the forwarded port.
    pub(crate) fn ended(&mut self) {
        if let Deploy::Emulator {
            device, reversed, ..
        } = self
            && let Some(port) = reversed.take()
        {
            device.unreverse(port);
        }
    }
}

/// The app's name from its artifact `path`: its file name without
/// `suffix`.
fn artifact_name(path: &Path, suffix: &str) -> Result<String, String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(suffix))
        .map(str::to_owned)
        .ok_or_else(|| format!("`{}` is not a packaged app", path.display()))
}

/// A [`Package`] that wraps an executable with `wrap(exe, bin, dir)` into
/// `dir`, beside it, and leaves the artifact of an executable it already
/// wrapped as it is, so an unchanged build launches nothing new.
fn once_per_executable(
    wrap: impl Fn(&Path, &str, &Path) -> Result<PathBuf, String> + Send + Sync + 'static,
) -> Package {
    let last: Mutex<Option<(PathBuf, Stamp, PathBuf)>> = Mutex::new(None);
    Arc::new(move |exe: &Path| {
        let stamp = stamp(exe);
        let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((path, at, artifact)) = &*last
            && path == exe
            && *at == stamp
            && stamp.is_some()
        {
            return Ok(artifact.clone());
        }
        let bin = exe
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("`{}` has no name", exe.display()))?;
        let dir = exe
            .parent()
            .ok_or_else(|| format!("`{}` has no directory", exe.display()))?
            .join("viso-dev");
        let artifact = wrap(exe, bin, &dir)?;
        *last = Some((exe.to_owned(), stamp, artifact.clone()));
        Ok(artifact)
    })
}

/// The Java shell of an app that links the packages in `linked`: the Java
/// under each one's `android/java`, and the manifest template one of them
/// carries.
pub(crate) fn android_shell(linked: &[PathBuf]) -> Result<Shell, String> {
    let java: Vec<PathBuf> = linked
        .iter()
        .map(|dir| dir.join("android/java"))
        .filter(|dir| dir.is_dir())
        .collect();
    let manifest = linked
        .iter()
        .map(|dir| dir.join("android/AndroidManifest.xml"))
        .find(|path| path.is_file())
        .ok_or("no package the app links carries the Android shell (it comes with `viso`)")?;
    Ok(Shell { java, manifest })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restore_and_a_window_are_launch_variables() {
        let vars = Vars {
            address: "127.0.0.1:4242".into(),
            token: "t",
            session: "s",
            build_id: "b",
            env: &[],
        };
        let plain = vars.of(false, None);
        assert_eq!(plain[0], (DEV_RUNTIME_ENV, "127.0.0.1:4242".into()));
        assert_eq!(plain.len(), 4);
        let window = WindowGeometry {
            size: (800.0, 600.0),
            position: None,
        };
        let restart = vars.of(true, Some(window));
        assert!(restart.contains(&(DEV_RESTORE_ENV, "1".into())));
        assert!(restart.contains(&(DEV_WINDOW_ENV, "800,600".into())));
    }

    #[test]
    fn an_artifact_names_its_app() {
        assert_eq!(
            artifact_name(Path::new("/t/viso-dev/demo.app"), ".app"),
            Ok("demo".into())
        );
        assert!(artifact_name(Path::new("/t/demo"), ".apk").is_err());
    }

    #[test]
    fn an_executable_is_wrapped_once() {
        let dir = std::env::temp_dir().join(format!("viso-deploy-once-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("demo");
        std::fs::write(&exe, b"one").unwrap();
        let wraps = Arc::new(Mutex::new(0));
        let counted = Arc::clone(&wraps);
        let package = once_per_executable(move |_, bin, dir| {
            *counted.lock().unwrap() += 1;
            Ok(dir.join(format!("{bin}.app")))
        });
        assert_eq!(package(&exe), Ok(dir.join("viso-dev/demo.app")));
        assert_eq!(package(&exe), Ok(dir.join("viso-dev/demo.app")));
        assert_eq!(*wraps.lock().unwrap(), 1);
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&exe, b"two!").unwrap();
        assert!(package(&exe).is_ok());
        assert_eq!(*wraps.lock().unwrap(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_shell_comes_from_the_linked_packages() {
        let dir = std::env::temp_dir().join(format!("viso-deploy-shell-{}", std::process::id()));
        let platform = dir.join("platform");
        let services = dir.join("services");
        std::fs::create_dir_all(platform.join("android/java")).unwrap();
        std::fs::create_dir_all(services.join("android/java")).unwrap();
        std::fs::write(platform.join("android/AndroidManifest.xml"), "<manifest/>").unwrap();
        let shell = android_shell(&[dir.join("app"), platform.clone(), services.clone()]).unwrap();
        assert_eq!(
            shell.java,
            vec![platform.join("android/java"), services.join("android/java")]
        );
        assert_eq!(shell.manifest, platform.join("android/AndroidManifest.xml"));
        assert!(android_shell(&[dir.join("app")]).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_simulator_console_drops_the_launch_line_only() {
        let console = Lines::Console("dev.viso.probe: ".into());
        assert_eq!(console.read("dev.viso.probe: 89311".into()), None);
        assert_eq!(
            console.read("dev.viso.probe: ready".into()),
            Some(("info", "dev.viso.probe: ready".into()))
        );
        assert_eq!(
            console.read("probe launched".into()),
            Some(("info", "probe launched".into()))
        );
    }
}
