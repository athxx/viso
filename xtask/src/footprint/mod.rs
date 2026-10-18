//! `cargo xtask footprint`: what the reference app costs in each footprint
//! profile, beside Makepad's equivalent apps (Architecture §50A.5).
//!
//! ```text
//! cargo xtask footprint [--check | --record] [--runs <n>]
//!     [--makepad <dir> | --no-makepad] [--only <subject>]...
//! ```
//!
//! Every subject is built in release with the same settings
//! ([`build::PROFILE`]: opt-level 3, thin LTO, one codegen unit, stripped)
//! under the shared target directory, then run `runs` times (default 3,
//! after one unrecorded warm-up) and each metric's median kept.
//!
//! - A window subject opens its window: the time from spawn to the first
//!   frame it submits, its RSS and memory footprint at the peak and after it
//!   settles idle, the CPU time and idle wakeups of three idle seconds, the
//!   GPU memory the OS charges to it (`GPU (OS)`), and the counted heap
//!   ([`window`]).
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
//! than its tolerance ([`METRICS`]), for CI trend reporting.

mod baseline;
mod build;
mod headless;
mod window;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "cargo xtask footprint [--check | --record] [--runs <n>] \
                     [--makepad <dir> | --no-makepad] [--only <subject>]...";

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
/// baseline value plus `floor`, in its unit.
struct Metric {
    key: &'static str,
    title: &'static str,
    unit: Unit,
    tolerance: f64,
    floor: f64,
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
    }
}

/// Every metric, in the order the tables list them. Sizes are exact for a
/// toolchain and so held tightly; times and memory vary from run to run, the
/// memory footprint most (the system's share of it, around a third of a
/// window app's, moves by tens of MiB between runs while the heap holds).
const METRICS: &[Metric] = &[
    metric("binary_bytes", "binary", Unit::Bytes, 0.03, 16.0 * KIB),
    metric("text_bytes", "__TEXT", Unit::Bytes, 0.03, 16.0 * KIB),
    metric("data_bytes", "data", Unit::Bytes, 0.05, 4.0 * KIB),
    metric("bss_bytes", "bss", Unit::Bytes, 0.05, 4.0 * KIB),
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
    ),
    metric(
        "heap_peak_bytes",
        "heap peak",
        Unit::Bytes,
        0.15,
        256.0 * KIB,
    ),
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

const RELEASE_ENV: &[(&str, &str)] = &[("VISO_PROFILE", "release")];

/// The Viso subjects: each profile `viso build` makes for this host, with a
/// window and headless.
fn viso_subjects() -> Vec<Subject> {
    let app = |features: &'static [&'static str], defaults: bool, env| build::Build::Viso {
        features,
        default_features: defaults,
        env,
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
            build: app(EMBEDDED_CPU, false, EMBEDDED_CPU_ENV),
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
    runs: u32,
    makepad: Option<PathBuf>,
    only: Vec<String>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let root = crate::workspace_root();
    let mut options = Options {
        check: false,
        record: false,
        runs: 3,
        makepad: std::env::var_os("MAKEPAD_DIR")
            .map(PathBuf::from)
            .or_else(|| Some(root.join("../makepad"))),
        only: Vec::new(),
    };
    let mut explicit_makepad = false;
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
            other => return Err(format!("unknown argument {other:?}\nusage: {USAGE}")),
        }
    }
    if options.check && options.record {
        return Err(format!(
            "--check and --record exclude each other\nusage: {USAGE}"
        ));
    }
    // A check is a trend gate on Viso: Makepad is measured when asked for.
    if options.check && !explicit_makepad {
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
    std::fs::create_dir_all(&scratch)
        .map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let mut subjects = viso_subjects();
    let makepad = options
        .makepad
        .as_deref()
        .map(|dir| dir.canonicalize().unwrap_or_else(|_| dir.to_owned()));
    if let Some(dir) = &makepad {
        subjects.extend(makepad_subjects(dir));
    }
    let wanted = |row: &str| {
        options.only.is_empty()
            || options
                .only
                .iter()
                .any(|only| row.starts_with(only.as_str()))
    };
    let mut rows = Vec::new();
    for subject in &subjects {
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
        let built = build::build(&subject.build, &root, &target, &scratch)?;
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
                    headless::measure(binary, scene, options.runs, &scratch, &mut row)?
                }
            }
            rows.push(row);
            if subject.mode == Mode::Size {
                break;
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    let meta = baseline::Meta::collect(makepad.as_deref(), options.runs);
    println!("{}", baseline::markdown(&meta, &rows));
    let dir = root.join("benches/footprint");
    if options.record {
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

/// The median of `values`, `None` when there are none.
fn median(values: &mut [f64]) -> Option<f64> {
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
        assert!(args(&["--runs", "0"]).is_err());
        assert!(args(&["--runs"]).is_err());
        assert!(args(&["--makepad", "/nonexistent/makepad"]).is_err());
        assert!(args(&["--fast"]).is_err());
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
