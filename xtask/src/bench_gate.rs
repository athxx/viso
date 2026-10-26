//! `cargo xtask check-benches`: the gate on the frame's hot paths, the work
//! tree against another commit on the same machine.
//!
//! ```text
//! cargo xtask check-benches --against <rev> [--rounds <n>] [--tolerance <percent>]
//!     [--floor <ns>] [--measure <s>]
//! ```
//!
//! The benches of [`BENCHES`] are run in release from a detached worktree of
//! `rev`, under a target directory of their own, and from the work tree:
//! each bench target on both sides back to back, the side that goes first
//! alternating from round to round, `rounds` times (default 4), each run
//! `measure` seconds per benchmark (default 2). A benchmark's change is the
//! median over the rounds of its work-tree median against `rev`'s from the
//! same round, so what else the machine runs, which drifts over minutes, is
//! met by both sides alike; the work tree may be at most `tolerance` percent
//! (default 15) slower, plus `floor` nanoseconds (default 20) of `rev`'s
//! best median.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::frame_tax::median_ns;
use crate::worktree::Worktree;

/// The benches held: the package and its bench targets.
const BENCHES: &[(&str, &[&str])] = &[
    ("viso", &["frame_loop", "list_scroll", "paint_scale"]),
    (
        "viso-ui",
        &["reactive_binding", "grid_layout", "large_list"],
    ),
];

const USAGE: &str = "cargo xtask check-benches --against <rev> [--rounds <n>] \
                     [--tolerance <percent>] [--floor <ns>] [--measure <s>]";

struct Options {
    against: String,
    rounds: u32,
    tolerance: f64,
    floor_ns: f64,
    measure_s: f64,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut against = None;
    let mut options = Options {
        against: String::new(),
        rounds: 4,
        tolerance: 15.0,
        floor_ns: 20.0,
        measure_s: 2.0,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value\nusage: {USAGE}"))
        };
        let number = |text: String, what: &str| {
            text.parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .ok_or_else(|| format!("{arg} needs {what}\nusage: {USAGE}"))
        };
        match arg.as_str() {
            "--against" => against = Some(value()?),
            "--rounds" => {
                let rounds = number(value()?, "a count")?;
                if rounds < 1.0 || rounds.fract() != 0.0 {
                    return Err(format!(
                        "--rounds needs a count of at least 1\nusage: {USAGE}"
                    ));
                }
                options.rounds = rounds as u32;
            }
            "--tolerance" => options.tolerance = number(value()?, "a percentage")?,
            "--floor" => options.floor_ns = number(value()?, "nanoseconds")?,
            "--measure" => {
                options.measure_s = number(value()?, "seconds")?;
                if options.measure_s == 0.0 {
                    return Err(format!("--measure needs some seconds\nusage: {USAGE}"));
                }
            }
            other => return Err(format!("unknown argument {other:?}\nusage: {USAGE}")),
        }
    }
    options.against =
        against.ok_or_else(|| format!("--against names the commit\nusage: {USAGE}"))?;
    Ok(options)
}

pub(crate) fn check_benches(args: &[String]) -> ExitCode {
    match parse(args).and_then(|o| check(&o)) {
        Ok(()) => {
            println!("check-benches: OK");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("check-benches: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Each benchmark's median in nanoseconds, by its full id.
type Medians = BTreeMap<String, f64>;

/// One side of the comparison: its workspace, its target directory and the
/// directory criterion writes to.
struct Side<'a> {
    root: &'a Path,
    target: PathBuf,
    home: PathBuf,
}

/// What the rounds gave: each benchmark's work-tree-to-`rev` ratio per
/// round, and each side's best median.
#[derive(Default)]
struct Rounds {
    ratios: BTreeMap<String, Vec<f64>>,
    base: Medians,
    head: Medians,
}

impl Rounds {
    /// Folds in one bench target's run on both sides.
    fn add(&mut self, base: Medians, head: Medians) {
        for (id, &now) in &head {
            if let Some(&then) = base.get(id) {
                self.ratios.entry(id.clone()).or_default().push(now / then);
            }
        }
        keep_best(&mut self.base, base);
        keep_best(&mut self.head, head);
    }
}

fn check(o: &Options) -> Result<(), String> {
    let root = crate::workspace_root();
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let dir = target.join("bench-against");
    let tree = Worktree::add(&root, &dir.join("tree"), &o.against)?;
    let sides = [
        Side {
            root: tree.path(),
            target: dir.join("target"),
            home: dir.join("criterion-base"),
        },
        Side {
            root: &root,
            target,
            home: dir.join("criterion-tree"),
        },
    ];
    let mut rounds = Rounds::default();
    for round in 0..o.rounds {
        let order = if round % 2 == 0 { [0, 1] } else { [1, 0] };
        for &(package, benches) in BENCHES {
            for bench in benches {
                eprintln!("check-benches: round {}, {package} {bench}", round + 1);
                let mut got = [Medians::new(), Medians::new()];
                for side in order {
                    got[side] = run(o, &sides[side], package, bench)?;
                }
                let [base, head] = got;
                rounds.add(base, head);
            }
        }
    }
    drop(sides);
    drop(tree);
    let worse = compare(o, &rounds);
    if worse.is_empty() {
        println!(
            "check-benches: {} benchmarks within {}% of {}",
            rounds.ratios.len(),
            o.tolerance,
            o.against
        );
        Ok(())
    } else {
        Err(format!(
            "{} benchmark(s) slower than {}:\n  {}",
            worse.len(),
            o.against,
            worse.join("\n  ")
        ))
    }
}

/// Runs `bench` of `package` on `side` and reads each of its benchmarks'
/// medians.
fn run(o: &Options, side: &Side<'_>, package: &str, bench: &str) -> Result<Medians, String> {
    let _ = std::fs::remove_dir_all(&side.home);
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    let measure = o.measure_s.to_string();
    let warm_up = (o.measure_s / 4.0).to_string();
    command
        .current_dir(side.root)
        .args(["bench", "-p", package, "--bench", bench])
        .args(["--", "--noplot", "--save-baseline", "gate"])
        .args(["--measurement-time", &measure, "--warm-up-time", &warm_up])
        .env("CARGO_TARGET_DIR", &side.target)
        .env("CRITERION_HOME", &side.home);
    let status = command
        .status()
        .map_err(|e| format!("cannot run cargo bench: {e}"))?;
    if !status.success() {
        return Err(format!(
            "{package}'s {bench} in {} failed ({status})",
            side.root.display()
        ));
    }
    let mut medians = Medians::new();
    read(&side.home, &mut medians)?;
    Ok(medians)
}

/// Every `gate` baseline under `dir`: its benchmark's full id and median.
fn read(dir: &Path, medians: &mut Medians) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().is_some_and(|name| name == "gate") {
            let text = |file: &str| {
                let at = path.join(file);
                std::fs::read_to_string(&at)
                    .map_err(|e| format!("cannot read {}: {e}", at.display()))
            };
            let id = full_id(&text("benchmark.json")?)
                .ok_or_else(|| format!("{} names no benchmark", path.display()))?;
            let median = median_ns(&text("estimates.json")?)
                .ok_or_else(|| format!("{} holds no median", path.display()))?;
            medians.insert(id, median);
        } else {
            read(&path, medians)?;
        }
    }
    Ok(())
}

/// The `full_id` of criterion's `benchmark.json`.
fn full_id(benchmark: &str) -> Option<String> {
    let rest = &benchmark[benchmark.find("\"full_id\"")? + "\"full_id\"".len()..];
    let rest = rest
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_owned())
}

/// Folds a round's medians into the best so far.
fn keep_best(best: &mut Medians, round: Medians) {
    for (id, median) in round {
        best.entry(id)
            .and_modify(|kept| *kept = kept.min(median))
            .or_insert(median);
    }
}

/// Each benchmark whose median ratio over the rounds is beyond the
/// tolerance and the floor.
fn compare(o: &Options, rounds: &Rounds) -> Vec<String> {
    let mut worse = Vec::new();
    for (id, ratios) in &rounds.ratios {
        let mut ratios = ratios.clone();
        let Some(ratio) = crate::footprint::median(&mut ratios) else {
            continue;
        };
        let (then, now) = (rounds.base[id], rounds.head[id]);
        let limit = 1.0 + o.tolerance / 100.0 + o.floor_ns / then;
        let change = (ratio - 1.0) * 100.0;
        println!(
            "check-benches: {id}: {then:.0} ns -> {now:.0} ns at best, {change:+.1}% by round"
        );
        if ratio > limit {
            worse.push(format!(
                "{id}: {change:+.1}% by round, {now:.0} ns against {then:.0} ns at best \
                 (at most {:+.1}%)",
                (limit - 1.0) * 100.0
            ));
        }
    }
    worse
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Result<Options, String> {
        parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_its_arguments() {
        let o = args(&["--against", "origin/nnn"]).unwrap();
        assert_eq!(o.against, "origin/nnn");
        assert_eq!(
            (o.rounds, o.tolerance, o.floor_ns, o.measure_s),
            (4, 15.0, 20.0, 2.0)
        );
        let o = args(&["--against", "HEAD~1", "--rounds", "5", "--tolerance", "5"]).unwrap();
        assert_eq!((o.rounds, o.tolerance), (5, 5.0));
        assert!(args(&[]).is_err(), "the commit is named");
        assert!(args(&["--against", "HEAD", "--rounds", "0"]).is_err());
        assert!(args(&["--against", "HEAD", "--measure", "0"]).is_err());
        assert!(args(&["--against"]).is_err());
        assert!(args(&["--fast"]).is_err());
    }

    #[test]
    fn the_id_is_read_from_criterions_benchmark() {
        let benchmark = r#"{"group_id":"frame_loop/view","function_id":"240","value_str":null,"throughput":null,"full_id":"frame_loop/view/240","directory_name":"frame_loop_view/240","title":"frame_loop/view/240"}"#;
        assert_eq!(full_id(benchmark).as_deref(), Some("frame_loop/view/240"));
        assert_eq!(full_id("{}"), None);
    }

    #[test]
    fn a_benchmark_slower_by_round_than_the_tolerance_fails() {
        let o = args(&["--against", "HEAD"]).unwrap();
        let medians = |a: f64, b: f64| Medians::from([("a".into(), a), ("b".into(), b)]);
        let mut rounds = Rounds::default();
        // `a` drifts with the machine on both sides; `b` is a fifth slower
        // in every round.
        rounds.add(medians(1000.0, 1000.0), medians(1050.0, 1200.0));
        rounds.add(medians(1500.0, 1500.0), medians(1450.0, 1800.0));
        rounds.add(medians(900.0, 1000.0), medians(950.0, 1210.0));
        assert_eq!(rounds.base["a"], 900.0, "the best median is kept");
        assert_eq!(rounds.ratios["b"].len(), 3);
        let worse = compare(&o, &rounds);
        assert_eq!(worse.len(), 1, "{worse:?}");
        assert!(worse[0].starts_with("b:"), "{worse:?}");
        // A benchmark only one side has is not compared.
        rounds.add(Medians::new(), Medians::from([("new".into(), 5.0)]));
        assert!(!rounds.ratios.contains_key("new"));
    }
}
