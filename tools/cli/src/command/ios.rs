//! `viso ios list|use|doctor|simulator`: the iOS Simulator development
//! environment, on macOS only.
//!
//! `list` shows the runtimes installed, the one Xcode's SDK pairs with when
//! it is not, and which new simulators are made on; `use` makes a runtime the
//! default, having Xcode download it first — only when asked to — and
//! checking it is there afterwards; `doctor` is `viso doctor ios`.

use std::io::{BufRead as _, Write as _};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use viso_project::{Platform, Preset};

use crate::args::{
    BuildTarget, DoctorArgs, IosCommand, IosListArgs, IosUseArgs, PlatformDoctorArgs,
};
use crate::context::Context;
use crate::device::simulator;
use crate::error::{
    ENV_DEVICE, ENV_DOWNLOAD, ENV_INSTALL, ENV_IOS_HOST, ENV_OFFLINE, ENV_XCODE, ENV_XCODE_LICENSE,
    INTERRUPTED, IOS_RUNTIME_INVALID, IOS_RUNTIME_MISSING, SUCCESS,
};
use crate::interrupt;
use crate::output::{Output, human::size};
use crate::process::ProcessTree;
use crate::toolchain::ios::{self, Home, Runtime};

pub fn run(ctx: &Context<'_>, command: &IosCommand, out: &mut Output) -> u8 {
    if !cfg!(target_os = "macos") {
        return out.failure(
            ENV_IOS_HOST,
            &format!(
                "the iOS Simulator is on macOS only; this host is {}",
                std::env::consts::OS
            ),
            &[],
        );
    }
    match command {
        IosCommand::List(args) => list(args, out),
        IosCommand::Use(args) => use_runtime(args, out),
        IosCommand::Doctor(args) => doctor(ctx, args, out),
        IosCommand::Simulator(command) => super::device::run(Platform::Ios, command, out),
    }
}

fn no_home(out: &mut Output) -> u8 {
    out.failure(
        ENV_DEVICE,
        "no home directory for `~/.viso` (set `VISO_HOME`)",
        &[],
    )
}

/// The runtimes, or the failure of a Mac without Xcode's `simctl`.
fn installed(out: &mut Output) -> Result<Vec<Runtime>, u8> {
    ios::runtimes().map_err(|why| {
        out.failure_next(
            ENV_XCODE,
            &format!("cannot list the Simulator runtimes: {why}"),
            &[],
            &["viso ios doctor".to_owned()],
        )
    })
}

/// A row of `list`.
struct Row<'a> {
    version: &'a str,
    runtime: Option<&'a Runtime>,
    status: &'static str,
}

fn list(args: &IosListArgs, out: &mut Output) -> u8 {
    let Some(home) = Home::current() else {
        return no_home(out);
    };
    let runtimes = match installed(out) {
        Ok(runtimes) => runtimes,
        Err(code) => return code,
    };
    let xcode = ios::xcode_runtime();
    let default = home.default_runtime();
    let chosen = ios::chosen(&runtimes, Some(&home)).map(|r| r.identifier.as_str());
    let mut rows: Vec<Row<'_>> = runtimes
        .iter()
        .map(|r| Row {
            version: &r.version,
            runtime: Some(r),
            status: if r.usable() { "installed" } else { "unusable" },
        })
        .collect();
    if let Some(xcode) = &xcode
        && !args.offline
        && ios::find(&runtimes, xcode).is_none()
    {
        rows.insert(
            0,
            Row {
                version: xcode,
                runtime: None,
                status: "available",
            },
        );
    }
    let is_chosen = |row: &Row<'_>| {
        row.runtime
            .is_some_and(|r| Some(r.identifier.as_str()) == chosen)
    };
    out.result(
        |w| {
            w.begin_object();
            w.name("xcode");
            match &xcode {
                Some(version) => w.string(version),
                None => w.null(),
            }
            w.name("default");
            match &default {
                Some(version) => w.string(version),
                None => w.null(),
            }
            w.name("runtimes");
            w.begin_array();
            for row in &rows {
                w.begin_object();
                w.name("version");
                w.string(row.version);
                w.name("status");
                w.string(row.status);
                let field = |w: &mut viso_ende::JsonWriter, name: &str, value: Option<&str>| {
                    w.name(name);
                    match value {
                        Some(value) => w.string(value),
                        None => w.null(),
                    }
                };
                field(w, "build", row.runtime.map(|r| r.build.as_str()));
                field(w, "identifier", row.runtime.map(|r| r.identifier.as_str()));
                field(w, "unusable", row.runtime.and_then(|r| r.unusable.as_deref()));
                for (name, preset) in [("phone", Preset::Phone), ("tablet", Preset::Tablet)] {
                    let model = row
                        .runtime
                        .and_then(|r| simulator::model(r, preset))
                        .map(|(name, _)| name);
                    field(w, name, model);
                }
                w.name("default");
                w.bool(is_chosen(row));
                w.end_object();
            }
            w.end_array();
            w.end_object();
        },
        || {
            if rows.is_empty() {
                return "no iOS Simulator runtime is installed; `viso ios list` shows the one Xcode can install\n"
                    .to_owned();
            }
            let mut text = String::from("RUNTIME       STATUS       DEFAULT\n");
            for row in &rows {
                let mark = if is_chosen(row) { "*" } else { "" };
                text.push_str(&format!(
                    "{:<13} {:<12} {mark}\n",
                    format!("iOS {}", row.version),
                    row.status
                ));
            }
            for row in &rows {
                if let Some(why) = row.runtime.and_then(|r| r.unusable.as_deref()) {
                    text.push_str(&format!("\niOS {} cannot run: {why}\n", row.version));
                }
            }
            text
        },
    );
    SUCCESS
}

fn use_runtime(args: &IosUseArgs, out: &mut Output) -> u8 {
    let Some(wanted) = ios::version(&args.runtime) else {
        return out.failure_next(
            IOS_RUNTIME_INVALID,
            &format!("`{}` is not an iOS runtime version", args.runtime),
            &[],
            &["viso ios list".to_owned()],
        );
    };
    let Some(home) = Home::current() else {
        return no_home(out);
    };
    let runtimes = match installed(out) {
        Ok(runtimes) => runtimes,
        Err(code) => return code,
    };
    let mut downloaded = false;
    let runtime = match ios::find(&runtimes, &wanted) {
        Some(runtime) => runtime.clone(),
        None => {
            if args.offline {
                return out.failure(
                    ENV_OFFLINE,
                    &format!("no iOS {wanted} Simulator runtime is installed"),
                    &[],
                );
            }
            let xcode = ios::xcode_runtime();
            if let Err(code) = consent(out, &wanted, args.download) {
                return code;
            }
            if let Err(code) = download(out, &wanted, xcode.as_deref()) {
                return code;
            }
            downloaded = true;
            let runtimes = match installed(out) {
                Ok(runtimes) => runtimes,
                Err(code) => return code,
            };
            match ios::find(&runtimes, &wanted) {
                Some(runtime) => runtime.clone(),
                None => {
                    return out.failure(
                        ENV_INSTALL,
                        &format!(
                            "Xcode finished downloading, but `simctl` lists no usable iOS {wanted} runtime"
                        ),
                        &[],
                    );
                }
            }
        }
    };
    if let Err(error) = home.set_default_runtime(&wanted) {
        return out.failure(
            ENV_INSTALL,
            &format!("cannot record the default runtime: {error}"),
            &[],
        );
    }
    out.result(
        |w| {
            w.begin_object();
            w.name("runtime");
            w.string(&wanted);
            w.name("version");
            w.string(&runtime.version);
            w.name("build");
            w.string(&runtime.build);
            w.name("identifier");
            w.string(&runtime.identifier);
            w.name("installed");
            w.bool(downloaded);
            w.name("default");
            w.bool(true);
            w.end_object();
        },
        || {
            format!(
                "{} iOS {} ({}); new simulators use it\n",
                if downloaded { "installed" } else { "using" },
                runtime.version,
                runtime.build
            )
        },
    );
    SUCCESS
}

/// Has the download agreed to: by `--download`, or at a terminal.
fn consent(out: &mut Output, wanted: &str, given: bool) -> Result<(), u8> {
    if given {
        return Ok(());
    }
    if !crate::device::can_ask(out) {
        return Err(out.failure_next(
            IOS_RUNTIME_MISSING,
            &format!("no iOS {wanted} Simulator runtime is installed"),
            &["pass `--download` to have Xcode download it (several GB)".to_owned()],
            &[format!("viso ios use {wanted} --download")],
        ));
    }
    let mut stderr = std::io::stderr().lock();
    let _ = write!(
        stderr,
        "Have Xcode download the iOS {wanted} Simulator runtime (about {})? [y/N]: ",
        size(ios::RUNTIME_BYTES)
    );
    let _ = stderr.flush();
    drop(stderr);
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    if matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(out.failure(
            IOS_RUNTIME_MISSING,
            &format!("the download of iOS {wanted} was declined; nothing changed"),
            &[],
        ))
    }
}

/// Has Xcode download and install the runtime `wanted`: the one its SDK
/// pairs with when `wanted` names it, else that build version.
fn download(out: &mut Output, wanted: &str, xcode: Option<&str>) -> Result<(), u8> {
    match ios::free_bytes() {
        Some(free) if free < ios::RUNTIME_BYTES => {
            return Err(out.failure(
                ENV_INSTALL,
                &format!(
                    "a Simulator runtime needs about {} free; {} are",
                    size(ios::RUNTIME_BYTES),
                    size(free)
                ),
                &[],
            ));
        }
        _ => {}
    }
    match crate::process::status(
        Command::new("xcodebuild").args(["-license", "check"]),
        crate::process::QUICK,
    ) {
        Ok(status) if status.success() => {}
        Ok(_) => {
            return Err(out.failure(
                ENV_XCODE_LICENSE,
                "the Xcode license is not accepted, so Xcode downloads nothing",
                &["run `sudo xcodebuild -license` and accept it".to_owned()],
            ));
        }
        Err(error) => {
            return Err(out.failure(ENV_XCODE, &error.to_string(), &[]));
        }
    }
    let mut command = Command::new("xcodebuild");
    command.args(["-downloadPlatform", "iOS"]);
    if !xcode.is_some_and(|xcode| ios::names(wanted, xcode)) {
        command.args(["-buildVersion", wanted]);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    out.step(
        "download",
        &format!("having Xcode download the iOS {wanted} Simulator runtime"),
    );
    let mut tree = ProcessTree::spawn(&mut command).map_err(|error| {
        out.failure(ENV_XCODE, &format!("cannot run `xcodebuild`: {error}"), &[])
    })?;
    let (send, lines) = mpsc::channel::<String>();
    let mut readers = Vec::new();
    for stream in [
        tree.child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        tree.child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let send = send.clone();
        readers.push(std::thread::spawn(move || {
            // Progress is redrawn with `\r`; each redraw is a line.
            let mut reader = std::io::BufReader::new(stream);
            let mut chunk = Vec::new();
            loop {
                chunk.clear();
                match reader.read_until(b'\r', &mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        for line in String::from_utf8_lossy(&chunk).split(['\r', '\n']) {
                            if !line.trim().is_empty() && send.send(line.trim().to_owned()).is_err()
                            {
                                return;
                            }
                        }
                    }
                }
            }
        }));
    }
    drop(send);
    let live = out.live();
    let mut said = Vec::new();
    let mut last = Instant::now() - Duration::from_secs(1);
    let status = loop {
        if interrupt::count() > 0 {
            tree.kill();
            if live {
                eprint!("\r\x1b[K");
            }
            return Err(INTERRUPTED);
        }
        match lines.recv_timeout(Duration::from_millis(250)) {
            Ok(line) => {
                if let Some(progress) = ios::download_progress(&line) {
                    if last.elapsed() >= Duration::from_millis(if live { 200 } else { 1000 }) {
                        last = Instant::now();
                        if live {
                            eprint!("\r[viso] downloading {progress}\x1b[K");
                        } else {
                            out.progress("download", &format!("downloading {progress}"));
                        }
                    }
                } else {
                    out.verbose(|| format!("xcodebuild: {line}"));
                    said.push(line);
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        match tree.child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                tree.kill();
                return Err(out.failure(
                    ENV_XCODE,
                    &format!("cannot wait for `xcodebuild`: {error}"),
                    &[],
                ));
            }
        }
    };
    for reader in readers {
        let _ = reader.join();
    }
    while let Ok(line) = lines.try_recv() {
        if ios::download_progress(&line).is_none() {
            said.push(line);
        }
    }
    if live {
        eprint!("\r\x1b[K");
    }
    if status.success() {
        out.step(
            "install",
            &format!("Xcode installed the iOS {wanted} runtime"),
        );
        return Ok(());
    }
    let why = said.last().cloned().unwrap_or_else(|| status.to_string());
    if why.to_ascii_lowercase().contains("license") {
        return Err(out.failure(
            ENV_XCODE_LICENSE,
            &format!("Xcode would not download: {why}"),
            &["run `sudo xcodebuild -license` and accept it".to_owned()],
        ));
    }
    let next: Vec<String> = xcode
        .filter(|xcode| !ios::names(wanted, xcode))
        .map(|xcode| format!("viso ios use {xcode} --download"))
        .into_iter()
        .collect();
    Err(out.failure_next(
        ENV_DOWNLOAD,
        &format!("Xcode cannot download iOS {wanted}: {why}"),
        &[],
        &next,
    ))
}

fn doctor(ctx: &Context<'_>, args: &PlatformDoctorArgs, out: &mut Output) -> u8 {
    super::doctor::run(
        ctx,
        &DoctorArgs {
            target: Some(BuildTarget::Ios),
            fix: args.fix,
        },
        out,
    )
}
