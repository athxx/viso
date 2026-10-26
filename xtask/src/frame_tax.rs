//! `cargo xtask check-frame-tax`: the gate on what the dev layer costs the
//! frame loop.
//!
//! ```text
//! cargo xtask check-frame-tax [--rounds <n>] [--tolerance <percent>] [--floor <ns>]
//! ```
//!
//! Runs `viso`'s `frame_loop` bench in release as is and with `--features
//! hot-reload`, alternating, `rounds` times each (default 5), and reads each
//! run's median from criterion's estimates. A frame's cost is the 240-frame
//! run's less the startup run's, over 240, so the dev session's startup (its
//! mount records, the link that finds no host) is not counted against a
//! frame. Each build's best round is compared — what else the machine runs
//! only ever slows a round, by a quarter on a busy one — and the dev frame
//! may cost at most `tolerance` percent (default 10) more than the plain
//! one, plus `floor` nanoseconds (default 250); a release artifact compiles
//! neither path, which `check-release-absence` proves.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The frames of the bench's long run.
const FRAMES: f64 = 240.0;
/// Where criterion writes the bench's estimates, under the target directory.
const GROUP_DIR: &str = "criterion/frame_loop_view";

const USAGE: &str =
    "cargo xtask check-frame-tax [--rounds <n>] [--tolerance <percent>] [--floor <ns>]";

struct Options {
    rounds: u32,
    tolerance: f64,
    floor_ns: f64,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        rounds: 5,
        tolerance: 10.0,
        floor_ns: 250.0,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut number = |what: &str| -> Result<f64, String> {
            args.next()
                .and_then(|n| n.parse::<f64>().ok())
                .filter(|n| n.is_finite() && *n >= 0.0)
                .ok_or_else(|| format!("{arg} needs {what}\nusage: {USAGE}"))
        };
        match arg.as_str() {
            "--rounds" => {
                let rounds = number("a count")?;
                if rounds < 1.0 || rounds.fract() != 0.0 {
                    return Err(format!(
                        "--rounds needs a count of at least 1\nusage: {USAGE}"
                    ));
                }
                options.rounds = rounds as u32;
            }
            "--tolerance" => options.tolerance = number("a percentage")?,
            "--floor" => options.floor_ns = number("nanoseconds")?,
            other => return Err(format!("unknown argument {other:?}\nusage: {USAGE}")),
        }
    }
    Ok(options)
}

pub(crate) fn check_frame_tax(args: &[String]) -> ExitCode {
    match parse(args).and_then(|o| check(&o)) {
        Ok(()) => {
            println!("check-frame-tax: OK");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("check-frame-tax: {error}");
            ExitCode::FAILURE
        }
    }
}

fn check(o: &Options) -> Result<(), String> {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::workspace_root().join("target"));
    let (mut plain, mut dev) = (f64::INFINITY, f64::INFINITY);
    for round in 1..=o.rounds {
        let p = frame_ns(&target, "plain", false)?;
        let d = frame_ns(&target, "dev", true)?;
        println!("round {round}: {p:.0} ns as is, {d:.0} ns with hot-reload");
        plain = plain.min(p);
        dev = dev.min(d);
    }
    let allowed = plain * (1.0 + o.tolerance / 100.0) + o.floor_ns;
    println!(
        "best frame: {plain:.0} ns as is, {dev:.0} ns with hot-reload ({:+.1}%), at most {allowed:.0} ns",
        (dev / plain - 1.0) * 100.0
    );
    if dev > allowed {
        return Err(format!(
            "a frame with hot-reload costs {dev:.0} ns, over {allowed:.0} ns \
             ({plain:.0} ns as is, {}% and {} ns allowed)",
            o.tolerance, o.floor_ns
        ));
    }
    Ok(())
}

/// A frame's median cost in nanoseconds, the bench run as `baseline`.
fn frame_ns(target: &Path, baseline: &str, hot_reload: bool) -> Result<f64, String> {
    let mut bench = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    bench.args(["bench", "-p", "viso", "--bench", "frame_loop"]);
    if hot_reload {
        bench.args(["--features", "hot-reload"]);
    }
    bench.args(["--", "--save-baseline", baseline, "--noplot"]);
    let status = bench
        .current_dir(crate::workspace_root())
        .status()
        .map_err(|e| format!("cannot run cargo bench: {e}"))?;
    if !status.success() {
        return Err(format!("the {baseline} bench failed ({status})"));
    }
    let median = |run: &str| -> Result<f64, String> {
        let path = target
            .join(GROUP_DIR)
            .join(run)
            .join(baseline)
            .join("estimates.json");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        median_ns(&text).ok_or_else(|| format!("{} holds no median", path.display()))
    };
    let startup = median("startup")?;
    let long = median("240")?;
    Ok((long - startup) / FRAMES)
}

/// The median's point estimate of criterion's `estimates.json`.
pub(crate) fn median_ns(estimates: &str) -> Option<f64> {
    let median = &estimates[estimates.find("\"median\"")?..];
    let point = &median[median.find("\"point_estimate\"")? + "\"point_estimate\"".len()..];
    let point = point.trim_start().strip_prefix(':')?.trim_start();
    let end = point
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .unwrap_or(point.len());
    point[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_median_is_read_from_criterions_estimates() {
        let estimates = r#"{"mean":{"confidence_interval":{"confidence_level":0.95,"lower_bound":1.0,"upper_bound":2.0},"point_estimate":1.5,"standard_error":0.1},"median":{"confidence_interval":{"confidence_level":0.95,"lower_bound":905.5,"upper_bound":921.0},"point_estimate":912.25,"standard_error":3.2e0}}"#;
        assert_eq!(median_ns(estimates), Some(912.25));
        assert_eq!(median_ns(r#"{"mean":{"point_estimate":1.0}}"#), None);
    }

    #[test]
    fn parses_its_arguments() {
        let args = |a: &[&str]| parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let o = args(&[]).unwrap();
        assert_eq!((o.rounds, o.tolerance, o.floor_ns), (5, 10.0, 250.0));
        let o = args(&["--rounds", "5", "--tolerance", "5", "--floor", "100"]).unwrap();
        assert_eq!((o.rounds, o.tolerance, o.floor_ns), (5, 5.0, 100.0));
        assert!(args(&["--rounds", "0"]).is_err());
        assert!(args(&["--rounds", "1.5"]).is_err());
        assert!(args(&["--tolerance"]).is_err());
        assert!(args(&["--floor", "-1"]).is_err());
        assert!(args(&["--fast"]).is_err());
    }
}
