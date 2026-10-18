//! The recorded baseline: what was measured, on what, and the tables of it;
//! and the check of a measurement against it.

use std::path::Path;
use std::process::Command;

use viso_ende::{JsonValue, JsonWriter};

use super::{METRICS, MIB, Row, Unit, build, metric_index};

/// Where and how a measurement was taken.
pub(super) struct Meta {
    pub(super) date: String,
    pub(super) machine: String,
    pub(super) rustc: String,
    pub(super) makepad: Option<String>,
    pub(super) build: String,
    pub(super) runs: u32,
}

impl Meta {
    pub(super) fn collect(makepad: Option<&Path>, runs: u32) -> Meta {
        Meta {
            date: today(),
            machine: machine(),
            rustc: output(
                Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into())).arg("-V"),
            )
            .unwrap_or_else(|| "unknown".into()),
            makepad: makepad.and_then(|dir| {
                let rev = output(Command::new("git").arg("-C").arg(dir).args([
                    "rev-parse",
                    "--short=12",
                    "HEAD",
                ]))?;
                let date = output(Command::new("git").arg("-C").arg(dir).args([
                    "log",
                    "-1",
                    "--format=%cs",
                ]))
                .unwrap_or_default();
                Some(format!("{rev} ({date})"))
            }),
            build: build::profile_settings(),
            runs,
        }
    }
}

/// A command's trimmed stdout, when it succeeds.
fn output(command: &mut Command) -> Option<String> {
    let out = command.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// The processor, memory and OS this runs on.
fn machine() -> String {
    if cfg!(target_os = "macos") {
        let sysctl = |name: &str| output(Command::new("sysctl").args(["-n", name]));
        let cpu = sysctl("machdep.cpu.brand_string").unwrap_or_else(|| "unknown CPU".into());
        let memory = sysctl("hw.memsize")
            .and_then(|b| b.parse::<f64>().ok())
            .map(|b| format!("{:.0} GiB", b / (1024.0 * MIB)))
            .unwrap_or_default();
        let version = output(Command::new("sw_vers").arg("-productVersion")).unwrap_or_default();
        let build = output(Command::new("sw_vers").arg("-buildVersion")).unwrap_or_default();
        format!("{cpu}, {memory}, macOS {version} ({build})")
    } else {
        let cpu = std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|l| l.strip_prefix("model name"))
                    .map(|l| l.trim_start_matches([' ', '\t', ':']).to_owned())
            })
            .unwrap_or_else(|| "unknown CPU".into());
        let os = output(Command::new("uname").arg("-sr")).unwrap_or_default();
        format!("{cpu}, {os}")
    }
}

/// Today's date (UTC), `yyyy-mm-dd`.
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil(secs / 86_400);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The proleptic Gregorian date `days` after 1970-01-01.
fn civil(days: u64) -> (i64, u32, u32) {
    let z = days as i64 + 719_468;
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

/// `value` in its unit, for a table.
fn shown(value: Option<f64>, unit: Unit) -> String {
    let Some(v) = value else {
        return "—".into();
    };
    match unit {
        Unit::Bytes if v >= MIB => format!("{:.2} MiB", v / MIB),
        Unit::Bytes => format!("{:.1} KiB", v / 1024.0),
        Unit::Millis | Unit::PerSecond => format!("{v:.1}"),
        Unit::Micros => format!("{v:.0}"),
    }
}

/// `value` as the baseline keeps it.
fn kept(value: f64, unit: Unit) -> f64 {
    match unit {
        Unit::Bytes | Unit::Micros => value.round(),
        Unit::Millis | Unit::PerSecond => (value * 10.0).round() / 10.0,
    }
}

/// The tables: Viso beside Makepad for each scene on the desktop, then
/// every subject.
pub(super) fn markdown(meta: &Meta, rows: &[Row]) -> String {
    let mut out = String::from("# Footprint baseline\n\n");
    out.push_str(
        "Written by `cargo xtask footprint --record`; the method is Architecture §50A.5.\n\n",
    );
    out.push_str(&format!("- Date: {}\n", meta.date));
    out.push_str(&format!("- Machine: {}\n", meta.machine));
    out.push_str(&format!("- Toolchain: {}\n", meta.rustc));
    if let Some(makepad) = &meta.makepad {
        out.push_str(&format!("- Makepad: {makepad}\n"));
    }
    out.push_str(&format!("- Build: {}\n", meta.build));
    out.push_str(&format!(
        "- Each value: the median of {} runs after one warm-up\n",
        meta.runs
    ));
    let find = |framework: &str, profile: &str, scene: &str| {
        rows.iter()
            .find(|r| r.framework == framework && r.profile == profile && r.scene == scene)
    };
    let columns: Vec<(&str, &Row)> = [
        ("Viso counter", find("viso", "desktop", "counter")),
        ("Makepad counter", find("makepad", "desktop", "counter")),
        ("Viso list", find("viso", "desktop", "list")),
        ("Makepad list", find("makepad", "desktop", "list")),
    ]
    .into_iter()
    .filter_map(|(title, row)| Some((title, row?)))
    .collect();
    if columns.iter().any(|(_, r)| r.framework == "makepad") {
        out.push_str("\n## Desktop window: Viso and Makepad\n\n| metric |");
        for (title, _) in &columns {
            out.push_str(&format!(" {title} |"));
        }
        out.push_str("\n|---|");
        out.push_str(&"---:|".repeat(columns.len()));
        out.push('\n');
        for (at, metric) in METRICS.iter().enumerate() {
            if columns.iter().all(|(_, r)| r.values[at].is_none()) {
                continue;
            }
            out.push_str(&format!("| {} |", metric.title));
            for (_, row) in &columns {
                out.push_str(&format!(" {} |", shown(row.values[at], metric.unit)));
            }
            out.push('\n');
        }
    }
    out.push_str("\n## Every subject\n\n| subject |");
    let taken: Vec<usize> = (0..METRICS.len())
        .filter(|&at| rows.iter().any(|r| r.values[at].is_some()))
        .collect();
    for &at in &taken {
        out.push_str(&format!(" {} |", METRICS[at].title));
    }
    out.push_str("\n|---|");
    out.push_str(&"---:|".repeat(taken.len()));
    out.push('\n');
    for row in rows {
        out.push_str(&format!("| {} |", row.id()));
        for &at in &taken {
            out.push_str(&format!(" {} |", shown(row.values[at], METRICS[at].unit)));
        }
        out.push('\n');
    }
    out
}

fn quoted(text: &str) -> String {
    let mut w = JsonWriter::new();
    w.string(text);
    w.into_string()
}

/// The baseline file: one member a line, so a change reads as a diff.
fn json(meta: &Meta, rows: &[Row]) -> String {
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"date\": {},\n", quoted(&meta.date)));
    out.push_str(&format!("  \"machine\": {},\n", quoted(&meta.machine)));
    out.push_str(&format!("  \"rustc\": {},\n", quoted(&meta.rustc)));
    if let Some(makepad) = &meta.makepad {
        out.push_str(&format!("  \"makepad\": {},\n", quoted(makepad)));
    }
    out.push_str(&format!("  \"build\": {},\n", quoted(&meta.build)));
    out.push_str(&format!("  \"runs\": {},\n", meta.runs));
    out.push_str("  \"rows\": [");
    for (n, row) in rows.iter().enumerate() {
        out.push_str(if n == 0 { "\n" } else { ",\n" });
        out.push_str("    {\n");
        let mut members = vec![
            format!("      \"framework\": {}", quoted(&row.framework)),
            format!("      \"profile\": {}", quoted(&row.profile)),
            format!("      \"scene\": {}", quoted(&row.scene)),
        ];
        for (at, metric) in METRICS.iter().enumerate() {
            if let Some(value) = row.values[at] {
                members.push(format!(
                    "      \"{}\": {}",
                    metric.key,
                    kept(value, metric.unit)
                ));
            }
        }
        out.push_str(&members.join(",\n"));
        out.push_str("\n    }");
    }
    out.push_str("\n  ]\n}\n");
    out
}

pub(super) fn write(dir: &Path, meta: &Meta, rows: &[Row], table: &str) -> Result<(), String> {
    for (name, text) in [
        ("baseline.json", json(meta, rows)),
        ("baseline.md", table.to_owned()),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// A baseline as read back.
pub(super) struct Recorded {
    machine: String,
    rustc: String,
    makepad: Option<String>,
    rows: Vec<Row>,
}

impl Recorded {
    /// The baseline with `rows` measured anew: each replaces the row of its
    /// subject in place, a new one goes last. Makepad's revision is kept for
    /// its rows when this run measured none of them.
    pub(super) fn merge(self, mut meta: Meta, rows: Vec<Row>) -> (Meta, Vec<Row>) {
        let mut merged = self.rows;
        for row in rows {
            match merged.iter_mut().find(|r| r.id() == row.id()) {
                Some(old) => *old = row,
                None => merged.push(row),
            }
        }
        if meta.makepad.is_none() && merged.iter().any(|r| r.framework == "makepad") {
            meta.makepad = self.makepad;
        }
        (meta, merged)
    }
}

pub(super) fn read(path: &Path) -> Result<Recorded, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read the baseline {}: {e}", path.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn parse(text: &str) -> Result<Recorded, String> {
    let value = JsonValue::parse(text).map_err(|e| e.to_string())?;
    let string =
        |v: &JsonValue, key: &str| v.get(key).and_then(JsonValue::as_str).map(str::to_owned);
    let mut rows = Vec::new();
    for item in value
        .get("rows")
        .and_then(JsonValue::as_array)
        .ok_or("no `rows`")?
    {
        let (Some(framework), Some(profile), Some(scene)) = (
            string(item, "framework"),
            string(item, "profile"),
            string(item, "scene"),
        ) else {
            return Err("a row without its framework, profile or scene".into());
        };
        let mut row = Row::new(&framework, &profile, &scene);
        if let JsonValue::Object(members) = item {
            for (key, value) in members {
                if let (Some(at), Some(n)) = (metric_index(key), value.as_f64()) {
                    row.values[at] = Some(n);
                }
            }
        }
        rows.push(row);
    }
    Ok(Recorded {
        machine: string(&value, "machine").unwrap_or_default(),
        rustc: string(&value, "rustc").unwrap_or_default(),
        makepad: string(&value, "makepad"),
        rows,
    })
}

/// A metric of `row` worse than its baseline by more than its tolerance.
fn regressions(recorded: &Row, row: &Row) -> Vec<String> {
    let mut out = Vec::new();
    for (at, metric) in METRICS.iter().enumerate() {
        let (Some(old), Some(new)) = (recorded.values[at], row.values[at]) else {
            continue;
        };
        let limit = old * (1.0 + metric.tolerance) + metric.floor;
        if new > limit {
            out.push(format!(
                "{} {}: {} against {} (at most {})",
                row.id(),
                metric.key,
                shown(Some(new), metric.unit),
                shown(Some(old), metric.unit),
                shown(Some(limit), metric.unit)
            ));
        }
    }
    out
}

/// Holds `rows` to `recorded`: every metric within its tolerance.
pub(super) fn check(meta: &Meta, recorded: &Recorded, rows: &[Row]) -> Result<(), String> {
    if recorded.machine != meta.machine || recorded.rustc != meta.rustc {
        println!(
            "footprint: the baseline was taken on {} with {}; this run is on {} with {}, so it reads as a trend",
            recorded.machine, recorded.rustc, meta.machine, meta.rustc
        );
    }
    let mut worse = Vec::new();
    for row in rows {
        match recorded.rows.iter().find(|r| r.id() == row.id()) {
            Some(old) => worse.extend(regressions(old, row)),
            None => println!("footprint: {} has no baseline", row.id()),
        }
    }
    if worse.is_empty() {
        println!("footprint: within the baseline ({} subjects)", rows.len());
        Ok(())
    } else {
        Err(format!(
            "{} metric(s) over the baseline:\n  {}",
            worse.len(),
            worse.join("\n  ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> Meta {
        Meta {
            date: "2026-10-18".into(),
            machine: "Test CPU".into(),
            rustc: "rustc 1.0".into(),
            makepad: Some("abc (2026-09-12)".into()),
            build: build::profile_settings(),
            runs: 3,
        }
    }

    fn row(framework: &str, binary: f64, startup: f64) -> Row {
        let mut row = Row::new(framework, "desktop", "counter");
        row.set("binary_bytes", Some(binary));
        row.set("startup_ms", Some(startup));
        row
    }

    #[test]
    fn a_partial_record_replaces_only_its_rows() {
        let rows = vec![
            row("viso", 5_187_104.0, 61.25),
            row("makepad", 12_239_984.0, 300.0),
        ];
        let recorded = parse(&json(&meta(), &rows)).unwrap();
        let mut fresh = Row::new("viso", "headless", "list");
        fresh.set("binary_bytes", Some(1.0));
        let this_run = Meta {
            makepad: None,
            ..meta()
        };
        let (meta, merged) = recorded.merge(this_run, vec![row("viso", 4.0, 2.0), fresh]);
        let ids: Vec<String> = merged.iter().map(Row::id).collect();
        assert_eq!(
            ids,
            [
                "viso/desktop/counter",
                "makepad/desktop/counter",
                "viso/headless/list"
            ]
        );
        assert_eq!(
            merged[0].values[metric_index("startup_ms").unwrap()],
            Some(2.0)
        );
        assert_eq!(
            merged[1].values[metric_index("startup_ms").unwrap()],
            Some(300.0)
        );
        assert_eq!(meta.makepad.as_deref(), Some("abc (2026-09-12)"));
    }

    #[test]
    fn the_baseline_reads_back_as_written() {
        let rows = vec![
            row("viso", 5_187_104.0, 61.25),
            row("makepad", 12_239_984.0, 300.0),
        ];
        let text = json(&meta(), &rows);
        let recorded = parse(&text).unwrap();
        assert_eq!(recorded.machine, "Test CPU");
        assert_eq!(recorded.rows.len(), 2);
        assert_eq!(recorded.rows[0].get("binary_bytes"), Some(5_187_104.0));
        assert_eq!(recorded.rows[0].get("startup_ms"), Some(61.3));
        assert_eq!(recorded.rows[1].id(), "makepad/desktop/counter");
    }

    #[test]
    fn a_metric_over_its_tolerance_is_a_regression() {
        let old = row("viso", 5_000_000.0, 60.0);
        assert!(regressions(&old, &row("viso", 5_100_000.0, 100.0)).is_empty());
        let worse = regressions(&old, &row("viso", 5_400_000.0, 200.0));
        assert_eq!(worse.len(), 2, "{worse:?}");
        assert!(worse[0].contains("binary_bytes"));
        assert!(regressions(&old, &row("viso", 1.0, 1.0)).is_empty());
    }

    #[test]
    fn the_tables_set_viso_beside_makepad() {
        let rows = vec![
            row("viso", 5_187_104.0, 61.0),
            row("makepad", 12_239_984.0, 300.0),
        ];
        let table = markdown(&meta(), &rows);
        assert!(
            table.contains("| metric | Viso counter | Makepad counter |"),
            "{table}"
        );
        assert!(
            table.contains("| binary | 4.95 MiB | 11.67 MiB |"),
            "{table}"
        );
        assert!(table.contains("| viso/desktop/counter |"), "{table}");
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_744), (2026, 10, 18));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }
}
