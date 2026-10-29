//! `viso profile`: the frames the app draws, sampled, summarized and kept as
//! a trace.
//!
//! The app is the one the project's `viso run` session runs, asked through
//! the session's control endpoint for its next frames or those of the next
//! seconds, or a headless run of it built for the capture, which samples
//! every frame it draws while it settles or plays a scenario. Either way the
//! samples come back as a [`Trace`]: the command reports each metric's p50,
//! p95 and maximum over its frames, writes the trace in Ende under
//! `target/viso/traces/` (or `--output`) and, with `--chrome`, as Chrome
//! trace-event JSON.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use viso_dev_protocol::headless::HeadlessSurface;
use viso_dev_protocol::profile::{Metric, Trace, Unit, chrome_trace, summarize};
use viso_ende::JsonWriter;
use viso_project::Target;

use super::build::relative_name;
use super::check;
use super::inspect;
use super::scenario::{self, Ended};
use crate::args::ProfileArgs;
use crate::context::Context;
use crate::dev::control;
use crate::dev::{ControlAsk, ControlReply};
use crate::error::{
    self, CLI_USAGE, ENV_NO_DEV_SESSION, INTERRUPTED, PROFILE_ANSWER_INVALID, PROFILE_APP_GONE,
    PROFILE_WRITE, SUCCESS, TEST_EXPECTATION,
};
use crate::interrupt;
use crate::output::{Artifact, Output};

/// How long a capture lasts when neither its frames nor its time is given.
const DEFAULT_SECONDS: f64 = 5.0;

/// How long past its time a capture's answer may take: the frames still in
/// flight, and the session's reply.
const ANSWER_GRACE: Duration = Duration::from_secs(10);

/// How many traces `target/viso/traces/` keeps: the newest.
const KEPT_TRACES: usize = 20;

pub fn run(ctx: &Context<'_>, args: &ProfileArgs, out: &mut Output) -> u8 {
    let target = args.target.map_or(Target::Host, |target| target.target());
    let headless = target == Target::Headless;
    if headless && args.device.is_some() {
        return out.failure(CLI_USAGE, "a headless run has no `--device`", &[]);
    }
    if headless && args.seconds.is_some() {
        return out.failure(
            CLI_USAGE,
            "a headless run's clock is its own: bound it with `--frames`, or a `--scenario`",
            &[],
        );
    }
    if headless && args.drive.is_some() {
        return out.failure(
            CLI_USAGE,
            "`--drive` changes a live window: profile the `viso run` session",
            &[],
        );
    }
    if !headless && args.scenario.is_some() {
        return out.failure(
            CLI_USAGE,
            "`--scenario` plays in a headless run: profile `headless`",
            &[],
        );
    }
    let project = match ctx.project(out) {
        Ok(project) => project,
        Err(code) => return code,
    };
    let root = project.root.clone();
    let app = project.manifest.package.name.as_ref().map_or_else(
        || {
            root.file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
        },
        |name| name.value.clone(),
    );
    let traced = if headless {
        self::headless(ctx, args, out)
    } else {
        session(ctx, target, args, out)
    };
    let mut trace = match traced {
        Ok(trace) => trace,
        Err(code) => return code,
    };
    trace.app = app;
    trace.device = args.device.clone();
    report(ctx, &root, &trace, args, out)
}

/// Asks the session of `target` for the frames `args` bounds.
fn session(
    ctx: &Context<'_>,
    target: Target,
    args: &ProfileArgs,
    out: &mut Output,
) -> Result<Trace, u8> {
    let endpoint = inspect::endpoint(ctx, target, args.device.as_deref(), out)?;
    let seconds = match (args.frames, args.seconds) {
        (None, None) => Some(DEFAULT_SECONDS),
        (_, seconds) => seconds,
    };
    let millis = seconds.map_or(0, |s| (s * 1000.0).round() as u64);
    let frames = args.frames.unwrap_or(0);
    let wait = seconds.map(|s| Duration::from_secs_f64(s) + ANSWER_GRACE);
    let ask = ControlAsk::Profile {
        frames,
        millis,
        drive: args.drive.map(|d| d.drive()),
    };
    interrupt::catch();
    let (answered, answer) = mpsc::channel();
    thread::spawn(move || {
        let _ = answered.send(control::call(&endpoint, ask, wait));
    });
    let started = Instant::now();
    let what = match (args.frames, seconds) {
        (Some(n), Some(s)) => format!("{n} frames or {s} s, whichever ends first"),
        (Some(n), None) => format!("{n} frames"),
        (None, Some(s)) => format!("{s} s"),
        (None, None) => unreachable!("a capture is bounded"),
    };
    out.step("profile", &format!("sampling the app's frames: {what}"));
    let reply = loop {
        match answer.recv_timeout(Duration::from_millis(250)) {
            Ok(reply) => break reply,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if interrupt::count() > 0 {
                    return Err(INTERRUPTED);
                }
                let elapsed = started.elapsed().as_secs_f64();
                if elapsed.fract() < 0.25 && elapsed >= 1.0 {
                    out.progress("profile", &format!("sampled for {elapsed:.0} s"));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(out.failure(ENV_NO_DEV_SESSION, "`viso run` did not answer", &[]));
            }
        }
    };
    match reply {
        Ok(ControlReply::Profiled { trace }) => Trace::from_bytes(&trace).map_err(|error| {
            out.failure(
                PROFILE_ANSWER_INVALID,
                &format!("the session's answer does not decode: {error}"),
                &[],
            )
        }),
        Ok(ControlReply::Refused {
            code,
            message,
            notes,
        }) => Err(out.failure(
            error::by_name(&code).unwrap_or(PROFILE_ANSWER_INVALID),
            &message,
            &notes,
        )),
        Ok(_) => Err(out.failure(
            PROFILE_ANSWER_INVALID,
            "`viso run` answered another question",
            &[],
        )),
        Err(message) => Err(out.failure(ENV_NO_DEV_SESSION, &message, &[])),
    }
}

/// Builds the app headless and samples every frame it draws while it
/// settles or plays `--scenario`, the first `--frames` of them kept.
fn headless(ctx: &Context<'_>, args: &ProfileArgs, out: &mut Output) -> Result<Trace, u8> {
    let checked = check::load(ctx, out)?;
    let code = checked.code(out);
    if code != SUCCESS {
        return Err(code);
    }
    let root = checked.project.root.clone();
    let (width, height, scale) = checked.project.manifest.test.surface.resolved();
    let surface = HeadlessSurface {
        width,
        height,
        scale: scale.get(),
    };
    let mut case = inspect::case(&root, "profile", args.scenario.as_deref(), surface, out)?;
    case.profile = true;
    let build = scenario::build(ctx, Some(checked.sources()), out)?;
    let started_ms = now_ms();
    let started = Instant::now();
    let mut profile = None;
    let code = scenario::play(
        ctx,
        &build,
        vec![case],
        out,
        |_| false,
        |out, case, played| {
            let code = match &played.ended {
                Ended::Passed | Ended::Stopped => SUCCESS,
                Ended::Failed { line, message } => out.failure(
                    TEST_EXPECTATION,
                    &format!(
                        "the scenario `{}` failed at line {line}: {message}",
                        case.name
                    ),
                    &[],
                ),
                Ended::Crashed(why) => {
                    let mut notes = vec![why.clone()];
                    notes.extend(played.output.lines().map(str::to_owned));
                    out.failure(PROFILE_APP_GONE, "the headless run crashed", &notes)
                }
            };
            if code == SUCCESS {
                profile = Some(std::fs::read(played.profile()));
            }
            played.discard();
            code
        },
    );
    if code != SUCCESS {
        return Err(code);
    }
    let bytes = match profile {
        Some(Ok(bytes)) => bytes,
        Some(Err(error)) => {
            return Err(out.failure(
                PROFILE_ANSWER_INVALID,
                &format!("the run wrote no profile: {error}"),
                &[],
            ));
        }
        None => return Err(code),
    };
    let mut trace = Trace::from_bytes(&bytes).map_err(|error| {
        out.failure(
            PROFILE_ANSWER_INVALID,
            &format!("the run's profile does not decode: {error}"),
            &[],
        )
    })?;
    if let Some(frames) = args.frames {
        trace.frames.truncate(frames as usize);
    }
    trace.started_ms = started_ms;
    trace.nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    Ok(trace)
}

/// Writes `trace` where `args` asks and reports it: the `profile` event, an
/// artifact per file written.
fn report(
    ctx: &Context<'_>,
    root: &Path,
    trace: &Trace,
    args: &ProfileArgs,
    out: &mut Output,
) -> u8 {
    let traces = ctx.layout(root).traces();
    let ende = match &args.output {
        Some(path) => path.clone(),
        None => traces.join(format!("{}.ende", stamp(trace.started_ms))),
    };
    let mut written = vec![("trace", ende, trace.to_bytes())];
    if let Some(chrome) = &args.chrome {
        written.push((
            "chrome-trace",
            chrome.clone(),
            chrome_trace(trace).into_bytes(),
        ));
    }
    for (_, path, bytes) in &written {
        if let Err(error) = crate::atomic::write(path, bytes) {
            return out.failure(
                PROFILE_WRITE,
                &format!("cannot write `{}`: {error}", path.display()),
                &[],
            );
        }
    }
    if args.output.is_none() {
        prune(&traces);
    }

    let (metrics, missing) = summarize(&trace.frames);
    let unavailable: Vec<&str> = missing;
    let caveats = caveats(&trace.target);
    let duration_ms = trace.nanos as f64 / 1e6;
    out.profile(
        |w| {
            w.begin_object();
            w.name("target");
            w.string(&trace.target);
            w.name("device");
            match &trace.device {
                Some(device) => w.string(device),
                None => w.null(),
            }
            w.name("drive");
            match args.drive {
                Some(drive) => w.string(drive.drive().as_str()),
                None => w.null(),
            }
            w.name("frames");
            w.uint(trace.frames.len() as u64);
            w.name("duration_ms");
            w.number(round(duration_ms));
            w.name("dropped");
            w.uint(u64::from(trace.dropped));
            w.name("metrics");
            write_metrics(w, &metrics);
            w.name("unavailable");
            w.begin_array();
            for name in &unavailable {
                w.string(name);
            }
            w.end_array();
            w.name("caveats");
            w.begin_array();
            for caveat in &caveats {
                w.string(caveat);
            }
            w.end_array();
            w.end_object();
        },
        || Some(table(trace, args, &metrics, &unavailable, &caveats)),
    );
    for (kind, path, bytes) in &written {
        let shown = if path.starts_with(root) {
            relative_name(root, path, false)
        } else {
            path.display().to_string()
        };
        let hash = format!("sha256:{}", hex(&Sha256::digest(bytes)));
        let build_id: String = hash
            .trim_start_matches("sha256:")
            .chars()
            .take(32)
            .collect();
        out.artifact(
            &Artifact {
                kind,
                path: &shown,
                target: &trace.target,
                profile: None,
                build_id: &build_id,
                size: bytes.len() as u64,
                hash: &hash,
                cached: false,
            },
            || format!("  {kind} {shown}\n"),
        );
    }
    SUCCESS
}

fn write_metrics(w: &mut JsonWriter, metrics: &[Metric]) {
    w.begin_object();
    for metric in metrics {
        w.name(metric.name);
        w.begin_object();
        for (name, value) in [
            ("p50", metric.stat.p50),
            ("p95", metric.stat.p95),
            ("max", metric.stat.max),
        ] {
            w.name(name);
            match metric.unit {
                Unit::Millis => w.number(round(value)),
                Unit::Count => w.uint(value as u64),
            }
        }
        w.end_object();
    }
    w.end_object();
}

/// The summary as a person reads it: a line on the capture, a row per
/// metric, then what it could not measure and what its times mean.
fn table(
    trace: &Trace,
    args: &ProfileArgs,
    metrics: &[Metric],
    unavailable: &[&str],
    caveats: &[String],
) -> String {
    let mut text = format!(
        "Profiled {} frames in {:.1} s ({}{}{})\n",
        trace.frames.len(),
        trace.nanos as f64 / 1e9,
        trace.target,
        trace
            .device
            .as_deref()
            .map(|d| format!(", {d}"))
            .unwrap_or_default(),
        args.drive
            .map(|d| format!(", the window driven: {}", d.drive().as_str()))
            .unwrap_or_default(),
    );
    if trace.frames.is_empty() {
        text.push_str(
            "  the app drew no frame: it was idle; interact with it while it is profiled\n",
        );
        return text;
    }
    text.push_str(&format!(
        "  {:<20} {:>10} {:>10} {:>10}\n",
        "", "p50", "p95", "max"
    ));
    for metric in metrics {
        let cell = |v: f64| match metric.unit {
            Unit::Millis => format!("{v:.3} ms"),
            Unit::Count => format!("{}", v as u64),
        };
        text.push_str(&format!(
            "  {:<20} {:>10} {:>10} {:>10}\n",
            metric.name,
            cell(metric.stat.p50),
            cell(metric.stat.p95),
            cell(metric.stat.max)
        ));
    }
    if trace.dropped > 0 {
        text.push_str(&format!(
            "  {} frames were sampled but not handed over in time\n",
            trace.dropped
        ));
    }
    if !unavailable.is_empty() {
        text.push_str(&format!("  not measured: {}\n", unavailable.join(", ")));
    }
    for caveat in caveats {
        text.push_str(&format!("  note: {caveat}\n"));
    }
    text
}

/// What the times of a capture on `target` say, and what they do not.
fn caveats(target: &str) -> Vec<String> {
    match target {
        "ios-simulator" => vec![
            "the iOS Simulator runs the app on this Mac's CPU, and its GPU through the \
             Simulator's Metal: its times do not predict an iPhone's; profile on a device for those"
                .to_owned(),
        ],
        "android-emulator" => vec![
            "the Android Emulator runs the app on this machine's CPU, and its GPU through the \
             emulator's host-GPU translation: its times do not predict a phone's; profile on a \
             device for those"
                .to_owned(),
        ],
        "web" => vec![
            "a page's frames are paced by the browser, and its GPU time is measured only where \
             the browser offers timestamp queries, which it may round"
                .to_owned(),
        ],
        "headless" => vec![
            "a headless run draws through the software raster: its `gpu` is that raster's time \
             on the CPU, and its frames come as fast as the app settles"
                .to_owned(),
        ],
        _ => Vec::new(),
    }
}

/// Removes all but the newest [`KEPT_TRACES`] traces of `dir`.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut traces: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "ende"))
        .collect();
    // The names are timestamps: they sort as the traces were taken.
    traces.sort();
    let excess = traces.len().saturating_sub(KEPT_TRACES);
    for path in &traces[..excess] {
        let _ = std::fs::remove_file(path);
    }
}

/// `ms` since the epoch as a trace's name: `profile-YYYYMMDD-HHMMSS-mmm`, UTC.
fn stamp(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil(days as i64);
    format!(
        "profile-{y:04}{m:02}{d:02}-{:02}{:02}{:02}-{:03}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        ms % 1000
    )
}

/// The civil date of `days` since 1970-01-01.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// `value` to the microsecond, as metrics report milliseconds.
fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trace_is_named_by_when_it_was_taken() {
        assert_eq!(stamp(0), "profile-19700101-000000-000");
        assert_eq!(stamp(1_792_240_496_789), "profile-20261017-123456-789");
        assert_eq!(stamp(1_709_251_199_000), "profile-20240229-235959-000");
    }
}
