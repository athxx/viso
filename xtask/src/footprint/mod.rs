//! `cargo xtask footprint`: what the reference app costs in each footprint
//! profile, beside Makepad's equivalent apps.
//!
//! ```text
//! cargo xtask footprint [--check | --record | --against <rev>] [--runs <n>]
//!     [--makepad <dir> | --no-makepad] [--only <subject>]...
//! ```
//!
//! Every subject is built under the shared target directory, then run
//! `runs` times (default 3, after one unrecorded warm-up) and each metric's
//! median kept. The desktop and headless subjects and Makepad's are built in
//! release with the same settings ([`build::PROFILE`]: opt-level 3, thin LTO,
//! one codegen unit, stripped). The embedded window and board subjects are
//! the artifact `viso build --profile shipping` makes for a device target:
//! opt-level `s`, fat LTO, and every raster converted at build time.
//!
//! - A window subject opens its window: the time from spawn to the first
//!   frame it submits, its RSS and memory footprint at the peak and after it
//!   settles idle, the CPU time and idle wakeups of three idle seconds, the
//!   GPU memory the OS charges to it (`GPU (OS)`), and the counted heap
//!   ([`window`]).
//! - A board subject is cross-built for a board (`--board <ssh destination>`,
//!   `--board-triple`, `aarch64-unknown-linux-gnu` by default) and watched
//!   there on its display: the startup, the idle CPU and wakeups and the
//!   resident set from `/proc`, and the counted heap ([`board`]).
//!   `--board-shell` is the command running a script there (`sh -s`), and
//!   each `--board-env NAME=value` a variable the app runs with.
//! - A headless subject plays scenarios (button clicks that change the
//!   count's text; wheel scrolls of the list) and reports each frame's
//!   UI-thread time, its heap and the device bytes its memory budget counts
//!   (`GPU (budget)`; on the software raster these are heap bytes too)
//!   ([`headless`]).
//!
//! The Viso subjects are `benches/footprint` built as `viso build` builds
//! each profile; the Makepad ones are `benches/footprint/makepad`, built
//! against a Makepad checkout (`--makepad`, else `$MAKEPAD_DIR`, else
//! `../makepad` beside the workspace) in its own crate, and Makepad's own
//! `examples/counter` for its size.
//!
//! The table is printed. `--record` writes it as the baseline
//! (`benches/footprint/baseline.json` and `baseline.md`); with `--only` it
//! replaces just the rows it measured and keeps the rest; `--check` measures
//! the Viso subjects and fails on a metric worse than the baseline by more
//! than its tolerance ([`METRICS`]), as a trend on another machine.
//!
//! `--against <rev>` is the gate CI runs: the Viso subjects of the commit
//! `rev` are built from a worktree of it, under a target directory of their
//! own, and measured on the same machine with the same toolchain; the work
//! tree fails on a metric worse than `rev`'s by more than its paired
//! tolerance, which holds sizes and the counted heap tighter than a baseline
//! taken elsewhere can.

mod baseline;
mod board;
mod build;
mod headless;
mod window;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "cargo xtask footprint [--check | --record | --against <rev>] [--runs <n>] \
                     [--makepad <dir> | --no-makepad] [--only <subject>]... \
                     [--board <ssh destination> [--board-triple <triple>] \
                     [--board-shell <command>] [--board-env NAME=value]...]";

/// What a metric counts, which decides how it is printed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Bytes,
    Millis,
    Micros,
    PerSecond,
}

/// One measured quantity: its key in the baseline, its column title, and how
/// much worse than the baseline `--check` lets it be: `tolerance` of the
/// baseline value plus `floor`, in its unit; and `--against`, which measures
/// both sides on one machine: `paired` (a tolerance and a floor) of the
/// other commit's value.
struct Metric {
    key: &'static str,
    title: &'static str,
    unit: Unit,
    tolerance: f64,
    floor: f64,
    paired: (f64, f64),
}

const KIB: f64 = 1024.0;
const MIB: f64 = 1024.0 * 1024.0;

const fn metric(
    key: &'static str,
    title: &'static str,
    unit: Unit,
    tolerance: f64,
    floor: f64,
) -> Metric {
    Metric {
        key,
        title,
        unit,
        tolerance,
        floor,
        paired: (tolerance, floor),
    }
}

impl Metric {
    /// The metric with `tolerance` and `floor` for a paired run: what one
    /// toolchain on one machine holds steady.
    const fn paired(self, tolerance: f64, floor: f64) -> Metric {
        Metric {
            paired: (tolerance, floor),
            ..self
        }
    }
}

/// Every metric, in the order the tables list them. Sizes are exact for a
/// toolchain and so held tightly, tighter when both sides share it; times
/// and memory vary from run to run, the memory footprint most (the system's
/// share of it, around a third of a window app's, moves by tens of MiB
/// between runs while the heap holds).
const METRICS: &[Metric] = &[
    metric("binary_bytes", "binary", Unit::Bytes, 0.03, 16.0 * KIB).paired(0.01, 4.0 * KIB),
    metric("text_bytes", "text", Unit::Bytes, 0.03, 16.0 * KIB).paired(0.01, 4.0 * KIB),
    metric("data_bytes", "data", Unit::Bytes, 0.05, 4.0 * KIB).paired(0.02, KIB),
    metric("bss_bytes", "bss", Unit::Bytes, 0.05, 4.0 * KIB).paired(0.02, KIB),
    metric("startup_ms", "first frame (ms)", Unit::Millis, 0.5, 20.0),
    metric("peak_rss_bytes", "peak RSS", Unit::Bytes, 0.2, 4.0 * MIB),
    metric("idle_rss_bytes", "idle RSS", Unit::Bytes, 0.2, 4.0 * MIB),
    metric(
        "peak_footprint_bytes",
        "peak footprint",
        Unit::Bytes,
        0.5,
        8.0 * MIB,
    ),
    metric(
        "idle_footprint_bytes",
        "idle footprint",
        Unit::Bytes,
        0.5,
        8.0 * MIB,
    ),
    metric("os_gpu_bytes", "GPU (OS)", Unit::Bytes, 0.2, MIB),
    metric("gpu_bytes", "GPU (budget)", Unit::Bytes, 0.2, MIB),
    metric(
        "heap_live_bytes",
        "heap live",
        Unit::Bytes,
        0.15,
        256.0 * KIB,
    )
    .paired(0.05, 64.0 * KIB),
    metric(
        "heap_peak_bytes",
        "heap peak",
        Unit::Bytes,
        0.15,
        256.0 * KIB,
    )
    .paired(0.05, 64.0 * KIB),
    metric(
        "idle_cpu_ms_per_s",
        "idle CPU (ms/s)",
        Unit::Millis,
        1.0,
        2.0,
    ),
    metric(
        "idle_wakeups_per_s",
        "idle wakeups (/s)",
        Unit::PerSecond,
        1.0,
        2.0,
    ),
    metric(
        "click_frame_us_p50",
        "click frame p50 (µs)",
        Unit::Micros,
        0.5,
        50.0,
    ),
    metric(
        "click_frame_us_p95",
        "click frame p95 (µs)",
        Unit::Micros,
        0.5,
        100.0,
    ),
    metric(
        "scroll_frame_us_p50",
        "scroll frame p50 (µs)",
        Unit::Micros,
        0.5,
        100.0,
    ),
    metric(
        "scroll_frame_us_p95",
        "scroll frame p95 (µs)",
        Unit::Micros,
        0.5,
        200.0,
    ),
];

/// The index of the metric `key` in [`METRICS`].
fn metric_index(key: &str) -> Option<usize> {
    METRICS.iter().position(|m| m.key == key)
}

/// Each metric's value, by [`METRICS`] index; `None` where it was not taken.
type Values = Vec<Option<f64>>;

/// One measured app: whose, in which profile, which scene, and its values.
#[derive(Clone)]
struct Row {
    framework: String,
    profile: String,
    scene: String,
    values: Values,
}

impl Row {
    fn new(framework: &str, profile: &str, scene: &str) -> Row {
        Row {
            framework: framework.to_owned(),
            profile: profile.to_owned(),
            scene: scene.to_owned(),
            values: vec![None; METRICS.len()],
        }
    }

    fn set(&mut self, key: &str, value: Option<f64>) {
        if let (Some(at), Some(value)) = (metric_index(key), value) {
            self.values[at] = Some(value);
        }
    }

    #[cfg(test)]
    fn get(&self, key: &str) -> Option<f64> {
        metric_index(key).and_then(|at| self.values[at])
    }

    /// `framework/profile/scene`, as `--only` and `--check` name it.
    fn id(&self) -> String {
        format!("{}/{}/{}", self.framework, self.profile, self.scene)
    }
}

/// The two scenes: a label and a button, and the same with a 10k-row list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scene {
    Counter,
    List,
}

impl Scene {
    const ALL: [Scene; 2] = [Scene::Counter, Scene::List];

    fn name(self) -> &'static str {
        match self {
            Scene::Counter => "counter",
            Scene::List => "list",
        }
    }
}

/// How a subject is run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Opens its window and is watched from outside.
    Window,
    /// Plays the scenario without a display.
    Headless,
    /// Only built: its size is all that is taken.
    Size,
    /// Runs on the board's display and is watched there.
    Board,
}

/// One build of an app to measure.
struct Subject {
    framework: &'static str,
    profile: &'static str,
    mode: Mode,
    build: build::Build,
}

/// The facade features of the embedded-cpu profile, as `viso build` turns
/// them on for a release build of a target that bakes no glyphs
/// (`viso-project` `DeviceTarget::features`).
const EMBEDDED_CPU: &[&str] = &["viso/cpu", "viso/kms", "viso/rasterizer"];

/// The variables `viso build` hands an embedded-cpu release build: its
/// profile, and the charset its packaged font is subset to.
const EMBEDDED_CPU_ENV: &[(&str, &str)] = &[
    ("VISO_PROFILE", "release"),
    ("VISO_FOOTPRINT", "embedded-cpu"),
    ("VISO_FONT_CHARSET", "ascii"),
];

/// The variables `viso build --profile shipping` hands an embedded-cpu
/// device target: the shipped artifact, every raster converted at build
/// time, so it links no raster decoder.
const EMBEDDED_CPU_SHIPPING: &[(&str, &str)] = &[
    ("VISO_PROFILE", "shipping"),
    ("VISO_FOOTPRINT", "embedded-cpu"),
    ("VISO_FONT_CHARSET", "ascii"),
    ("VISO_IMAGES_CONVERTED", "raster"),
];

const RELEASE_ENV: &[(&str, &str)] = &[("VISO_PROFILE", "release")];

/// The facade features of the embedded-gpu profile on GLES.
const EMBEDDED_GPU: &[&str] = &["viso/gles", "viso/kms", "viso/rasterizer"];

/// The variables of an embedded-gpu shipping build, its font subset to the
/// embedded-cpu one's.
const EMBEDDED_GPU_SHIPPING: &[(&str, &str)] = &[
    ("VISO_PROFILE", "shipping"),
    ("VISO_FOOTPRINT", "embedded-gpu"),
    ("VISO_FONT_CHARSET", "ascii"),
    ("VISO_IMAGES_CONVERTED", "raster"),
];

/// The Viso subjects: each profile `viso build` makes for this host, with a
/// window and headless.
fn viso_subjects() -> Vec<Subject> {
    let app = |features: &'static [&'static str], defaults: bool, env| build::Build::Viso {
        features,
        default_features: defaults,
        env,
        triple: None,
    };
    vec![
        Subject {
            framework: "viso",
            profile: "desktop",
            mode: Mode::Window,
            build: app(&[], true, RELEASE_ENV),
        },
        Subject {
            framework: "viso",
            profile: "embedded-cpu",
            mode: Mode::Window,
            build: app(EMBEDDED_CPU, false, EMBEDDED_CPU_SHIPPING),
        },
        Subject {
            framework: "viso",
            profile: "headless",
            mode: Mode::Headless,
            build: app(&["viso/headless"], true, RELEASE_ENV),
        },
        Subject {
            framework: "viso",
            profile: "embedded-cpu-headless",
            mode: Mode::Headless,
            build: app(
                &["viso/cpu", "viso/kms", "viso/rasterizer", "viso/headless"],
                false,
                EMBEDDED_CPU_ENV,
            ),
        },
    ]
}

/// The board subjects: each embedded profile, cross-built for `triple`.
fn board_subjects(triple: &str) -> Vec<Subject> {
    let app = |features, env| build::Build::Viso {
        features,
        default_features: false,
        env,
        triple: Some(triple.to_owned()),
    };
    vec![
        Subject {
            framework: "viso",
            profile: "embedded-cpu-board",
            mode: Mode::Board,
            build: app(EMBEDDED_CPU, EMBEDDED_CPU_SHIPPING),
        },
        Subject {
            framework: "viso",
            profile: "embedded-gpu-board",
            mode: Mode::Board,
            build: app(EMBEDDED_GPU, EMBEDDED_GPU_SHIPPING),
        },
    ]
}

/// The Makepad subjects, against the checkout at `dir`.
fn makepad_subjects(dir: &Path) -> Vec<Subject> {
    vec![
        Subject {
            framework: "makepad",
            profile: "desktop",
            mode: Mode::Window,
            build: build::Build::Makepad {
                checkout: dir.to_owned(),
            },
        },
        Subject {
            framework: "makepad",
            profile: "desktop-stock",
            mode: Mode::Size,
            build: build::Build::MakepadExample {
                checkout: dir.to_owned(),
                package: "makepad-example-counter",
            },
        },
    ]
}

struct Options {
    check: bool,
    record: bool,
    against: Option<String>,
    runs: u32,
    makepad: Option<PathBuf>,
    only: Vec<String>,
    board: Option<board::Board>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let root = crate::workspace_root();
    let mut options = Options {
        check: false,
        record: false,
        against: None,
        runs: 3,
        makepad: std::env::var_os("MAKEPAD_DIR")
            .map(PathBuf::from)
            .or_else(|| Some(root.join("../makepad"))),
        only: Vec::new(),
        board: None,
    };
    let mut explicit_makepad = false;
    let (mut host, mut triple, mut shell, mut env) = (None, None, None, Vec::new());
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value\nusage: {USAGE}"))
        };
        match arg.as_str() {
            "--check" => options.check = true,
            "--record" => options.record = true,
            "--against" => options.against = Some(value()?),
            "--runs" => {
                options.runs =
                    value()?.parse().ok().filter(|n| *n >= 1).ok_or_else(|| {
                        format!("--runs needs a count of at least 1\nusage: {USAGE}")
                    })?;
            }
            "--makepad" => {
                options.makepad = Some(PathBuf::from(value()?));
                explicit_makepad = true;
            }
            "--no-makepad" => options.makepad = None,
            "--only" => options.only.push(value()?),
            "--board" => host = Some(value()?),
            "--board-triple" => triple = Some(value()?),
            "--board-shell" => shell = Some(value()?),
            "--board-env" => {
                let pair = value()?;
                if !pair.contains('=') {
                    return Err(format!("--board-env takes NAME=value\nusage: {USAGE}"));
                }
                env.push(pair);
            }
            other => return Err(format!("unknown argument {other:?}\nusage: {USAGE}")),
        }
    }
    match host {
        Some(host) => {
            options.board = Some(board::Board {
                host,
                shell: shell.unwrap_or_else(|| "sh -s".to_owned()),
                triple: triple.unwrap_or_else(|| "aarch64-unknown-linux-gnu".to_owned()),
                env,
            });
        }
        None if triple.is_some() || shell.is_some() || !env.is_empty() => {
            return Err(format!(
                "the --board-* options need --board\nusage: {USAGE}"
            ));
        }
        None => {}
    }
    let modes = [options.check, options.record, options.against.is_some()];
    if modes.iter().filter(|&&on| on).count() > 1 {
        return Err(format!(
            "--check, --record and --against exclude each other\nusage: {USAGE}"
        ));
    }
    // A check is a trend gate on Viso: Makepad is measured when asked for,
    // and never against another commit, whose Makepad is the same.
    if (options.check && !explicit_makepad) || options.against.is_some() {
        options.makepad = None;
    }
    if let Some(dir) = &options.makepad
        && !dir.join("widgets/Cargo.toml").is_file()
    {
        if explicit_makepad {
            return Err(format!("{} is not a Makepad checkout", dir.display()));
        }
        eprintln!(
            "footprint: no Makepad checkout at {}; measuring Viso alone",
            dir.display()
        );
        options.makepad = None;
    }
    Ok(options)
}

pub(crate) fn footprint(args: &[String]) -> ExitCode {
    match parse(args).and_then(|options| run(&options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("footprint: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(options: &Options) -> Result<(), String> {
    let root = crate::workspace_root();
    let target = build::target_dir();
    let scratch = std::env::temp_dir().join(format!("viso-footprint-{}", std::process::id()));
    let makepad = options
        .makepad
        .as_deref()
        .map(|dir| dir.canonicalize().unwrap_or_else(|_| dir.to_owned()));
    let mut subjects = viso_subjects();
    if let Some(board) = &options.board {
        subjects.extend(board_subjects(&board.triple));
    }
    if let Some(dir) = &makepad {
        subjects.extend(makepad_subjects(dir));
    }
    let measured = match &options.against {
        Some(rev) => against(rev, &subjects, options, &root, &target, &scratch)
            .map(|(rows, base)| (rows, Some(base))),
        None => measure(&subjects, options, &root, &target, &scratch.join("tree"))
            .map(|rows| (rows, None)),
    };
    let _ = std::fs::remove_dir_all(&scratch);
    let (rows, base) = measured?;
    let mut meta = baseline::Meta::collect(makepad.as_deref(), options.runs);
    meta.board = options.board.as_ref().and_then(board::Board::describe);
    println!("{}", baseline::markdown(&meta, &rows));
    let dir = root.join("benches/footprint");
    if let (Some(rev), Some(base)) = (&options.against, base) {
        baseline::check_paired(rev, &base, &rows)?;
    } else if options.record {
        let (meta, rows) = if options.only.is_empty() {
            (meta, rows)
        } else {
            baseline::read(&dir.join("baseline.json"))?.merge(meta, rows)
        };
        let table = baseline::markdown(&meta, &rows);
        baseline::write(&dir, &meta, &rows, &table)?;
        println!(
            "footprint: recorded {}",
            dir.join("baseline.json").display()
        );
    } else if options.check {
        let recorded = baseline::read(&dir.join("baseline.json"))?;
        baseline::check(&meta, &recorded, &rows)?;
    }
    Ok(())
}

/// The rows of the work tree, and of the commit `rev` beside them: its
/// subjects built from a detached worktree of it, under a target directory
/// of their own, then the work tree's, on this machine one after the other.
fn against(
    rev: &str,
    subjects: &[Subject],
    options: &Options,
    root: &Path,
    target: &Path,
    scratch: &Path,
) -> Result<(Vec<Row>, Vec<Row>), String> {
    let dir = target.join("footprint-against");
    let tree = crate::worktree::Worktree::add(root, &dir.join("tree"), rev)?;
    eprintln!("footprint: measuring {rev}");
    let base = measure(
        subjects,
        options,
        tree.path(),
        &dir.join("target"),
        &scratch.join("base"),
    )?;
    drop(tree);
    eprintln!("footprint: measuring the work tree");
    let rows = measure(subjects, options, root, target, &scratch.join("tree"))?;
    Ok((rows, base))
}

/// Builds the wanted `subjects` of the workspace at `root` under `target`
/// and measures each of their scenes.
fn measure(
    subjects: &[Subject],
    options: &Options,
    root: &Path,
    target: &Path,
    scratch: &Path,
) -> Result<Vec<Row>, String> {
    std::fs::create_dir_all(scratch)
        .map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let wanted = |row: &str| {
        options.only.is_empty()
            || options
                .only
                .iter()
                .any(|only| row.starts_with(only.as_str()))
    };
    let mut rows = Vec::new();
    for subject in subjects {
        let ids: Vec<String> = Scene::ALL
            .iter()
            .map(|scene| format!("{}/{}/{}", subject.framework, subject.profile, scene.name()))
            .collect();
        if !ids.iter().any(|id| wanted(id)) {
            continue;
        }
        if subject.mode == Mode::Window && !cfg!(target_os = "macos") {
            eprintln!(
                "footprint: {} {} opens a window, which is measured on macOS only",
                subject.framework, subject.profile
            );
            continue;
        }
        eprintln!(
            "footprint: building {} {}",
            subject.framework, subject.profile
        );
        let built = build::build(&subject.build, root, target, scratch)?;
        for scene in Scene::ALL {
            let mut row = Row::new(subject.framework, subject.profile, scene.name());
            if !wanted(&row.id()) {
                continue;
            }
            let Some(binary) = built.binary(scene) else {
                continue;
            };
            build::sizes(binary, &mut row)?;
            eprintln!("footprint: measuring {}", row.id());
            match subject.mode {
                Mode::Size => {}
                Mode::Window => window::measure(binary, &built, scene, options.runs, &mut row)?,
                Mode::Headless => {
                    headless::measure(binary, scene, options.runs, scratch, &mut row)?
                }
                Mode::Board => {
                    let board = options.board.as_ref().expect("board subjects need a board");
                    board.measure(binary, scene, options.runs, &mut row)?
                }
            }
            rows.push(row);
            if subject.mode == Mode::Size {
                break;
            }
        }
    }
    Ok(rows)
}

/// The median of `values`, `None` when there are none.
pub(crate) fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    })
}

/// The `q` quantile (0 to 1) of `values` by nearest rank.
fn quantile(values: &mut [f64], q: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let rank = ((q * values.len() as f64).ceil() as usize).clamp(1, values.len());
    Some(values[rank - 1])
}

/// The medians over runs: each run's values for one row, metric by metric.
fn medians(runs: &[Values]) -> Values {
    (0..METRICS.len())
        .map(|at| {
            let mut taken: Vec<f64> = runs.iter().filter_map(|run| run[at]).collect();
            median(&mut taken)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Result<Options, String> {
        parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_its_arguments() {
        let o = args(&["--no-makepad"]).unwrap();
        assert!(!o.check && !o.record && o.makepad.is_none());
        assert_eq!(o.runs, 3);
        let o = args(&["--check", "--runs", "5", "--only", "viso/desktop"]).unwrap();
        assert!(o.check && o.makepad.is_none());
        assert_eq!(
            (o.runs, o.only.as_slice()),
            (5, &["viso/desktop".to_owned()][..])
        );
        assert!(args(&["--check", "--record"]).is_err());
        assert!(args(&["--check", "--against", "HEAD"]).is_err());
        let o = args(&["--against", "origin/nnn"]).unwrap();
        assert_eq!(o.against.as_deref(), Some("origin/nnn"));
        assert!(o.makepad.is_none());
        assert!(args(&["--runs", "0"]).is_err());
        assert!(args(&["--runs"]).is_err());
        assert!(args(&["--makepad", "/nonexistent/makepad"]).is_err());
        assert!(args(&["--fast"]).is_err());
        let o = args(&["--board", "pi", "--board-env", "VISO_KMS_MODE=800x480"]).unwrap();
        let board = o.board.unwrap();
        assert_eq!(
            (
                board.host.as_str(),
                board.triple.as_str(),
                board.shell.as_str()
            ),
            ("pi", "aarch64-unknown-linux-gnu", "sh -s")
        );
        assert_eq!(board.env, ["VISO_KMS_MODE=800x480"]);
        assert!(args(&["--board-triple", "armv7-unknown-linux-gnueabihf"]).is_err());
        assert!(args(&["--board", "pi", "--board-env", "MODE"]).is_err());
    }

    #[test]
    fn medians_and_quantiles() {
        assert_eq!(median(&mut []), None);
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&mut [4.0, 1.0, 2.0, 3.0]), Some(2.5));
        let mut frames: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(quantile(&mut frames, 0.5), Some(50.0));
        assert_eq!(quantile(&mut frames, 0.95), Some(95.0));
        assert_eq!(quantile(&mut [7.0], 0.95), Some(7.0));
        let runs = vec![
            vec![Some(1.0); METRICS.len()],
            vec![None; METRICS.len()],
            vec![Some(3.0); METRICS.len()],
        ];
        assert_eq!(medians(&runs)[0], Some(2.0));
    }

    #[test]
    fn every_metric_key_is_unique() {
        for (at, m) in METRICS.iter().enumerate() {
            assert_eq!(metric_index(m.key), Some(at), "{}", m.key);
        }
    }
}
