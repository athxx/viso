//! Watching a windowed app from outside (macOS).
//!
//! The app runs under `/usr/bin/time -l`, which reports its peak RSS and peak
//! memory footprint when it exits. From spawn to the line it writes when it
//! submits its first frame is its startup. Two seconds later it is idle:
//! `top` samples its CPU time and idle wakeups across three seconds, then
//! `ps` its RSS and `footprint` its memory footprint and the GPU memory
//! (`IOAccelerator`, `IOSurface`) charged to it. An app that answers on
//! stdin is then asked for its heap, and ended by closing stdin.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::build::Built;
use super::{Row, Scene, Values, medians};

/// How long a first frame may take.
const FIRST_FRAME: Duration = Duration::from_secs(30);
/// From the first frame to the idle samples.
const SETTLE: Duration = Duration::from_secs(2);
/// The idle stretch CPU time and wakeups are counted over, in seconds.
const IDLE_SECONDS: u32 = 3;

/// Measures `binary` showing `scene` `runs` times, after a warm-up run, into
/// `row`.
pub(super) fn measure(
    binary: &Path,
    built: &Built,
    scene: Scene,
    runs: u32,
    row: &mut Row,
) -> Result<(), String> {
    let mut taken = Vec::new();
    for run in 0..=runs {
        let values = once(binary, built, scene)?;
        if run > 0 {
            taken.push(values);
        }
    }
    for (at, value) in medians(&taken).into_iter().enumerate() {
        if value.is_some() {
            row.values[at] = value;
        }
    }
    Ok(())
}

/// A line the app wrote, when it was read.
struct Line {
    at: Instant,
    text: String,
}

/// A spawned app and the lines it writes.
pub(super) struct Running {
    child: Child,
    spawned: Instant,
    lines: Receiver<Line>,
    readers: Vec<JoinHandle<()>>,
    seen: Vec<String>,
    /// Whether it was given a stdin, which it ends with.
    reads_stdin: bool,
}

impl Running {
    /// Spawns `command`, the app under `time`, reading both its outputs; with
    /// a piped stdin when `stdin`.
    pub(super) fn spawn(mut command: Command, stdin: bool) -> Result<Running, String> {
        command
            .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let spawned = Instant::now();
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot run {command:?}: {e}"))?;
        let (send, lines) = channel();
        let mut readers = Vec::new();
        let out: [Box<dyn std::io::Read + Send>; 2] = [
            Box::new(child.stdout.take().expect("piped stdout")),
            Box::new(child.stderr.take().expect("piped stderr")),
        ];
        for stream in out {
            let send = send.clone();
            readers.push(std::thread::spawn(move || {
                for text in BufReader::new(stream).lines().map_while(Result::ok) {
                    if send
                        .send(Line {
                            at: Instant::now(),
                            text,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
        Ok(Running {
            child,
            spawned,
            lines,
            readers,
            seen: Vec::new(),
            reads_stdin: stdin,
        })
    }

    /// Waits up to `limit` for a line containing `needle`; the time from
    /// spawn to it.
    pub(super) fn wait_for(&mut self, needle: &str, limit: Duration) -> Option<Duration> {
        let deadline = Instant::now() + limit;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(left).ok()?;
            let found = line.text.contains(needle);
            self.seen.push(line.text);
            if found {
                return Some(line.at.saturating_duration_since(self.spawned));
            }
        }
    }

    /// The last lines read, for a failure's message.
    pub(super) fn tail(&self) -> String {
        let from = self.seen.len().saturating_sub(12);
        self.seen[from..].join("\n")
    }

    /// The app's process: the child of `/usr/bin/time`.
    fn app_pid(&self) -> Option<u32> {
        for _ in 0..50 {
            let output = Command::new("pgrep")
                .arg("-P")
                .arg(self.child.id().to_string())
                .output()
                .ok()?;
            if let Some(pid) = String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .next()
                .and_then(|p| p.parse().ok())
            {
                return Some(pid);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    /// Ends the app: by closing its stdin when it reads it, else by a
    /// signal; then everything it wrote.
    pub(super) fn finish(
        mut self,
        pid: Option<u32>,
        limit: Duration,
    ) -> Result<Vec<String>, String> {
        drop(self.child.stdin.take());
        if !self.reads_stdin
            && let Some(pid) = pid
        {
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status();
        }
        let deadline = Instant::now() + limit;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return Err(format!("the app did not end in time\n{}", self.tail()));
                }
            }
        }
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        self.seen
            .extend(self.lines.try_iter().map(|line| line.text));
        Ok(self.seen)
    }
}

/// One run.
fn once(binary: &Path, built: &Built, scene: Scene) -> Result<Values, String> {
    let mut row = Row::new("", "", "");
    let mut command = Command::new("/usr/bin/time");
    command.arg("-l").arg(binary);
    if built.scene_argument {
        command.arg(scene.name());
    }
    command.env(built.first_frame.0, "1");
    if built.control {
        command.env("VISO_FOOTPRINT_CONTROL", "1");
    }
    let mut running = Running::spawn(command, built.control)?;
    let pid = running.app_pid();
    let Some(startup) = running.wait_for(built.first_frame.1, FIRST_FRAME) else {
        let tail = running.tail();
        let _ = running.finish(pid, Duration::from_secs(5));
        return Err(format!("{} drew no first frame\n{tail}", binary.display()));
    };
    row.set("startup_ms", Some(startup.as_secs_f64() * 1e3));
    let pid = pid.ok_or("cannot find the app's process")?;
    std::thread::sleep(SETTLE);
    if let Some((cpu_ms, wakeups)) = idle(pid) {
        row.set("idle_cpu_ms_per_s", Some(cpu_ms / f64::from(IDLE_SECONDS)));
        row.set(
            "idle_wakeups_per_s",
            Some(wakeups / f64::from(IDLE_SECONDS)),
        );
    }
    row.set("idle_rss_bytes", rss(pid));
    if let Some((total, gpu)) = footprint(pid) {
        row.set("idle_footprint_bytes", Some(total));
        row.set("os_gpu_bytes", Some(gpu));
    }
    if built.control
        && let Some(stdin) = running.child.stdin.as_mut()
    {
        let _ = stdin.write_all(b"heap\n");
        let _ = stdin.flush();
        if running
            .wait_for("footprint: heap", Duration::from_secs(5))
            .is_some()
            && let Some(line) = running.seen.last()
        {
            let (live, peak) = heap(line);
            row.set("heap_live_bytes", live);
            row.set("heap_peak_bytes", peak);
        }
    }
    let lines = running.finish(Some(pid), Duration::from_secs(10))?;
    let (peak_rss, peak_footprint) = rusage(&lines);
    row.set("peak_rss_bytes", peak_rss);
    row.set("peak_footprint_bytes", peak_footprint);
    Ok(row.values)
}

/// The CPU milliseconds and the idle wakeups of `pid` across
/// [`IDLE_SECONDS`], as `top` counts them.
fn idle(pid: u32) -> Option<(f64, f64)> {
    let output = Command::new("top")
        .args(["-l", "2", "-s", &IDLE_SECONDS.to_string(), "-pid"])
        .arg(pid.to_string())
        .args(["-stats", "pid,idlew,time"])
        .output()
        .ok()?;
    top_samples(&String::from_utf8_lossy(&output.stdout), pid)
}

/// The change between the two samples `top` printed for `pid`: CPU
/// milliseconds and idle wakeups.
fn top_samples(text: &str, pid: u32) -> Option<(f64, f64)> {
    let samples: Vec<(f64, f64)> = text
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            if words.next()?.parse::<u32>().ok()? != pid {
                return None;
            }
            let wakeups: f64 = words
                .next()?
                .trim_end_matches(|c: char| !c.is_ascii_digit())
                .parse()
                .ok()?;
            let time = clock_seconds(words.next()?.trim_end_matches('+'))?;
            Some((time, wakeups))
        })
        .collect();
    let (first, last) = (samples.first()?, samples.last()?);
    (samples.len() >= 2).then_some(((last.0 - first.0) * 1e3, last.1 - first.1))
}

/// `[[hh:]mm:]ss[.ff]` in seconds.
fn clock_seconds(text: &str) -> Option<f64> {
    text.split(':').try_fold(0.0, |total, part| {
        Some(total * 60.0 + part.parse::<f64>().ok()?)
    })
}

/// The resident set of `pid`, in bytes.
fn rss(pid: u32) -> Option<f64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p"])
        .arg(pid.to_string())
        .output()
        .ok()?;
    let kib: f64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()?;
    Some(kib * 1024.0)
}

/// The memory footprint of `pid` and the part of it that is GPU memory, in
/// bytes.
fn footprint(pid: u32) -> Option<(f64, f64)> {
    let output = Command::new("footprint")
        .args(["-f", "bytes", "-p"])
        .arg(pid.to_string())
        .output()
        .ok()?;
    footprint_report(&String::from_utf8_lossy(&output.stdout))
}

/// The total `footprint -f bytes` reports and the dirty bytes of its GPU
/// categories.
fn footprint_report(text: &str) -> Option<(f64, f64)> {
    let total = text.lines().find_map(|line| {
        let rest = &line[line.find("Footprint: ")? + "Footprint: ".len()..];
        rest.split_whitespace().next()?.parse::<f64>().ok()
    })?;
    let gpu = text
        .lines()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            // `<dirty> B <clean> B <reclaimable> B <regions> <category…>`
            let category = words.get(7..)?.join(" ");
            let gpu = category.starts_with("IOAccelerator") || category.starts_with("IOSurface");
            (gpu && words.get(1) == Some(&"B"))
                .then(|| words[0].parse::<f64>().ok())
                .flatten()
        })
        .sum();
    Some((total, gpu))
}

/// The live and peak heap of a `footprint: heap live=<n> peak=<n>` line.
fn heap(line: &str) -> (Option<f64>, Option<f64>) {
    let field = |name: &str| {
        line.split_whitespace()
            .find_map(|word| word.strip_prefix(name))
            .and_then(|n| n.parse().ok())
    };
    (field("live="), field("peak="))
}

/// The peak RSS and the peak memory footprint `/usr/bin/time -l` reported.
pub(super) fn rusage(lines: &[String]) -> (Option<f64>, Option<f64>) {
    let field = |name: &str| {
        lines.iter().rev().find_map(|line| {
            let line = line.trim();
            line.strip_suffix(name)?.trim().parse::<f64>().ok()
        })
    };
    (
        field("maximum resident set size"),
        field("peak memory footprint"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_top_samples() {
        let text = "Processes: 1 total\nPID    IDLEW TIME    \n61852  13    00:00.01\n\
                    Disks: 1/2 read\nPID    IDLEW TIME    \n61852  139+  00:01.51\n";
        assert_eq!(top_samples(text, 61852), Some((1500.0, 126.0)));
        assert_eq!(top_samples(text, 1), None);
        assert_eq!(clock_seconds("01:02.5"), Some(62.5));
        assert_eq!(clock_seconds("1:00:00"), Some(3600.0));
    }

    #[test]
    fn reads_the_footprint_and_its_gpu_part() {
        let text = "app [1]: 64-bit    Footprint: 29360128 B (16384 bytes per page)\n\
                    \n      Dirty         Clean   Reclaimable    Regions    Category\n\
                    ---\n 3080192 B           0 B           0 B          3    IOSurface\n\
                    2490368 B           0 B     2342912 B         32    IOAccelerator (graphics)\n\
                    65536 B           0 B           0 B          2    IOAccelerator\n\
                    425984 B           0 B           0 B          9    Malloc Metadata\n";
        assert_eq!(
            footprint_report(text),
            Some((29360128.0, (3080192 + 2490368 + 65536) as f64))
        );
    }

    #[test]
    fn reads_the_heap_and_the_rusage() {
        assert_eq!(
            heap("footprint: heap live=5712097 peak=6000000"),
            (Some(5712097.0), Some(6000000.0))
        );
        let lines: Vec<String> = [
            "        4.53 real         0.08 user         0.17 sys",
            "            90963968  maximum resident set size",
            "             7356944  peak memory footprint",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(rusage(&lines), (Some(90963968.0), Some(7356944.0)));
    }
}
