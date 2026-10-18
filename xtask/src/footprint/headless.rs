//! Running a headless build through its scenario.
//!
//! Each run plays one scenario on the build's software raster with its
//! fixed clock: the click one presses the button again and again, so each
//! step changes the count's text (a reshape, relayout and repaint of one
//! label); the scroll one turns the wheel over the list, each step a row and
//! a half. Input that changes nothing runs no frame at all, so there is no
//! idle frame to time: an idle app's cost is the window subject's idle CPU
//! and wakeups. The build writes every frame's samples
//! (`VISO_HEADLESS_PROFILE`) and, at the end, its first window's detail
//! (`VISO_HEADLESS_DETAIL`), whose `budget` group holds the counted heap and
//! the GPU bytes. A frame's time is the UI thread's on it, the raster
//! included; only frames from the first input on count.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use viso_dev_protocol::headless::Reported;
use viso_dev_protocol::inspect::WindowDetail;
use viso_dev_protocol::profile::{Count, FrameSample, Span, Trace};

use super::window::{Running, rusage};
use super::{Row, Scene, Values, medians, quantile};

/// How long a scenario may take.
const LIMIT: Duration = Duration::from_secs(120);
/// The steps of each scenario.
const STEPS: u32 = 120;
/// How far one wheel step scrolls, in logical pixels.
const SCROLL_STEP: u32 = 36;

/// The surface the scene's window gets, as its desktop window is sized.
fn surface(scene: Scene) -> &'static str {
    match scene {
        Scene::Counter => "420x220@1",
        Scene::List => "420x600@1",
    }
}

/// The click scenario: `STEPS` presses of the button.
fn click_tape(scene: Scene) -> String {
    let mut tape = format!("surface {}\nexpect \"Increment\"\n", surface(scene));
    for _ in 0..STEPS {
        tape.push_str("click \"Increment\"\n");
    }
    tape
}

/// The scroll scenario: `STEPS` wheel steps down the list.
fn scroll_tape() -> String {
    let mut tape = format!("surface {}\nexpect \"Rows\"\n", surface(Scene::List));
    for _ in 0..STEPS {
        tape.push_str(&format!("scroll \"Rows\" 0 {SCROLL_STEP}\n"));
    }
    tape
}

/// Measures `binary` showing `scene` `runs` times, after a warm-up run.
pub(super) fn measure(
    binary: &Path,
    scene: Scene,
    runs: u32,
    scratch: &Path,
    row: &mut Row,
) -> Result<(), String> {
    let mut taken = Vec::new();
    for run in 0..=runs {
        let mut values = Row::new("", "", "");
        let click = play(binary, scene, &click_tape(scene), scratch, "click")?;
        values.set("startup_ms", click.startup_ms);
        let (p50, p95) = frame_times(&click.frames);
        values.set("click_frame_us_p50", p50);
        values.set("click_frame_us_p95", p95);
        let last = if scene == Scene::List {
            let scroll = play(binary, scene, &scroll_tape(), scratch, "scroll")?;
            let moved: u64 = scroll
                .frames
                .iter()
                .filter_map(|f| f.count(Count::DamagePx))
                .sum();
            if moved == 0 {
                return Err("the scroll scenario redrew nothing: the list did not scroll".into());
            }
            let (p50, p95) = frame_times(&scroll.frames);
            values.set("scroll_frame_us_p50", p50);
            values.set("scroll_frame_us_p95", p95);
            scroll
        } else {
            click
        };
        values.set("peak_rss_bytes", last.peak_rss);
        values.set("peak_footprint_bytes", last.peak_footprint);
        for (name, key) in [
            ("heap", "heap_live_bytes"),
            ("heap_peak", "heap_peak_bytes"),
            ("gpu", "gpu_bytes"),
        ] {
            values.set(key, budget(&last.detail, name));
        }
        if run > 0 {
            taken.push(values.values);
        }
    }
    let medians: Values = medians(&taken);
    for (at, value) in medians.into_iter().enumerate() {
        if value.is_some() {
            row.values[at] = value;
        }
    }
    Ok(())
}

/// What one scenario's run reported.
struct Played {
    startup_ms: Option<f64>,
    frames: Vec<FrameSample>,
    detail: WindowDetail,
    peak_rss: Option<f64>,
    peak_footprint: Option<f64>,
}

fn play(
    binary: &Path,
    scene: Scene,
    tape: &str,
    scratch: &Path,
    name: &str,
) -> Result<Played, String> {
    let file = |ext: &str| scratch.join(format!("{}-{name}.{ext}", scene.name()));
    let (script, report, trace, detail) =
        (file("tape"), file("report"), file("trace"), file("detail"));
    std::fs::write(&script, tape).map_err(|e| format!("cannot write the scenario: {e}"))?;
    let mut command = Command::new(if cfg!(target_os = "macos") {
        "/usr/bin/time"
    } else {
        "time"
    });
    command
        .arg(if cfg!(target_os = "macos") {
            "-l"
        } else {
            "-v"
        })
        .arg(binary)
        .arg(scene.name())
        .env("VISO_DEBUG", "1")
        .env("VISO_HEADLESS_SURFACE", surface(scene))
        .env("VISO_HEADLESS_SCRIPT", &script)
        .env("VISO_HEADLESS_REPORT", &report)
        .env("VISO_HEADLESS_PROFILE", &trace)
        .env("VISO_HEADLESS_DETAIL", &detail);
    let mut running = Running::spawn(command, false)?;
    let startup = running.wait_for("viso: first frame submitting", LIMIT);
    let lines = running.finish(None, LIMIT)?;
    let reported: Vec<Reported> = std::fs::read_to_string(&report)
        .unwrap_or_default()
        .lines()
        .filter_map(Reported::from_line)
        .collect();
    let failed = reported.iter().find_map(|r| match r {
        Reported::Failed { line, message } => Some(format!("line {line}: {message}")),
        _ => None,
    });
    let done = reported.iter().any(|r| matches!(r, Reported::Done { .. }));
    if failed.is_some() || !done {
        return Err(format!(
            "the {name} scenario of {} did not play through ({})\n{}",
            scene.name(),
            failed.unwrap_or_else(|| format!("{} steps reported", reported.len())),
            lines[lines.len().saturating_sub(12)..].join("\n")
        ));
    }
    let read = |path: &Path| {
        std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
    };
    let samples =
        Trace::from_bytes(&read(&trace)?).map_err(|e| format!("the trace does not decode: {e}"))?;
    let window = WindowDetail::from_bytes(&read(&detail)?)
        .map_err(|e| format!("the detail does not decode: {e}"))?;
    let (peak_rss, peak_footprint) = rusage(&lines);
    let peak_rss = peak_rss.or_else(|| gnu_peak_rss(&lines));
    Ok(Played {
        startup_ms: startup.map(|d| d.as_secs_f64() * 1e3),
        frames: from_first_input(samples.frames),
        detail: window,
        peak_rss,
        peak_footprint,
    })
}

/// The frames from the first that dispatched input on: the scenario's.
fn from_first_input(frames: Vec<FrameSample>) -> Vec<FrameSample> {
    let first = frames
        .iter()
        .position(|f| f.span(Span::Input).is_some())
        .unwrap_or(frames.len());
    frames[first..].to_vec()
}

/// The median and 95th percentile of the frames' UI-thread time, in µs.
fn frame_times(frames: &[FrameSample]) -> (Option<f64>, Option<f64>) {
    let mut micros: Vec<f64> = frames.iter().map(|f| f.nanos as f64 / 1e3).collect();
    (quantile(&mut micros, 0.5), quantile(&mut micros, 0.95))
}

/// The bytes of the `budget` resource `name` in the detail.
fn budget(detail: &WindowDetail, name: &str) -> Option<f64> {
    detail
        .frame
        .resources
        .iter()
        .find(|r| r.group == "budget" && r.name == name)
        .and_then(|r| r.bytes)
        .map(|b| b as f64)
}

/// The peak RSS GNU `time -v` reports, in bytes.
fn gnu_peak_rss(lines: &[String]) -> Option<f64> {
    lines.iter().find_map(|line| {
        let kib = line
            .trim()
            .strip_prefix("Maximum resident set size (kbytes):")?;
        Some(kib.trim().parse::<f64>().ok()? * 1024.0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_dev_protocol::headless::Script;
    use viso_dev_protocol::inspect::ResourceDetail;
    use viso_dev_protocol::profile::Timed;

    #[test]
    fn the_scenarios_parse() {
        for tape in [
            click_tape(Scene::Counter),
            click_tape(Scene::List),
            scroll_tape(),
        ] {
            let script = Script::parse(&tape).unwrap();
            assert_eq!(script.steps.len(), STEPS as usize + 1);
        }
    }

    #[test]
    fn only_frames_from_the_first_input_count() {
        let frame = |nanos: u64, input: bool| {
            let mut f = FrameSample {
                nanos,
                ..FrameSample::default()
            };
            if input {
                f.spans[Span::Input as usize] = Some(Timed { start: 0, nanos: 1 });
            }
            f
        };
        let frames = from_first_input(vec![
            frame(9_000_000, false),
            frame(2_000, true),
            frame(4_000, false),
            frame(3_000, true),
        ]);
        assert_eq!(frames.len(), 3);
        assert_eq!(frame_times(&frames), (Some(3.0), Some(4.0)));
        assert!(from_first_input(vec![frame(1, false)]).is_empty());
    }

    #[test]
    fn reads_the_budget_group() {
        let mut detail = WindowDetail::default();
        detail.frame.resources.push(ResourceDetail {
            group: "budget".into(),
            name: "heap".into(),
            count: 1,
            bytes: Some(4096),
            budget: None,
            note: None,
        });
        assert_eq!(budget(&detail, "heap"), Some(4096.0));
        assert_eq!(budget(&detail, "gpu"), None);
        let lines = vec!["\tMaximum resident set size (kbytes): 2048".to_owned()];
        assert_eq!(gnu_peak_rss(&lines), Some(2048.0 * 1024.0));
    }
}
