//! Watching an app on a board, over ssh.
//!
//! The executable, cross-built for the board's triple, is written to the
//! board's `/tmp` through `ssh <board> 'cat > …'`. A POSIX shell script
//! ([`PROBE`], busybox is enough) then runs there through the board's shell
//! command (`--board-shell`: `sh -s` by default; `sudo -n sh -s` where the
//! display needs root, or a container's shell). It starts the app on the
//! board's display and reads `/proc`:
//!
//! - the startup, from its start to the line it writes when it submits its
//!   first frame, both read off `/proc/uptime`;
//! - two seconds later, its CPU ticks and the voluntary context switches of
//!   all its threads across three seconds, a switch being a thread that
//!   slept and woke;
//! - its resident set then (`VmRSS`), and its peak (`VmHWM`) once it has
//!   reported its heap.
//!
//! It ends the app by closing the stdin it reads commands on.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use super::{Row, Scene, Values, medians};

/// The shell script run on the board, `@…@` filled in.
const PROBE: &str = r#"set -u
bin=@BIN@
out=/tmp/viso-footprint.out
ctl=/tmp/viso-footprint.ctl
rm -f "$out" "$ctl"
mkfifo "$ctl"
up() { read -r u _ < /proc/uptime; echo "$u"; }
t0=$(up)
env VISO_DEBUG=1 VISO_FOOTPRINT_CONTROL=1 @ENV@ "$bin" @SCENE@ < "$ctl" > "$out" 2>&1 &
pid=$!
exec 3> "$ctl"
n=0
until grep -q "viso: first frame submitting" "$out" 2>/dev/null; do
  if ! kill -0 "$pid" 2>/dev/null; then echo "error: the app exited"; tail -n 20 "$out"; exit 1; fi
  n=$((n + 1))
  if [ "$n" -gt 3000 ]; then echo "error: no first frame"; tail -n 20 "$out"; kill "$pid"; exit 1; fi
  sleep 0.01
done
t1=$(up)
echo "startup $t0 $t1"
sleep 2
ticks() { awk '{ print $14 + $15 }' "/proc/$pid/stat"; }
woke() { cat /proc/"$pid"/task/*/status | awk '/^voluntary_ctxt_switches/ { s += $2 } END { print s }'; }
c0=$(ticks); w0=$(woke)
sleep 3
c1=$(ticks); w1=$(woke)
echo "idle $((c1 - c0)) $((w1 - w0)) $(getconf CLK_TCK 2>/dev/null || echo 100)"
echo "rss $(awk '/^VmRSS/ { print $2 }' "/proc/$pid/status")"
echo heap >&3
n=0
until grep -q "footprint: heap" "$out"; do
  n=$((n + 1)); [ "$n" -gt 500 ] && break
  sleep 0.01
done
grep "footprint: heap" "$out" | tail -n 1
echo "peak $(awk '/^VmHWM/ { print $2 }' "/proc/$pid/status")"
exec 3>&-
n=0
while kill -0 "$pid" 2>/dev/null; do
  n=$((n + 1)); [ "$n" -gt 1000 ] && { kill -9 "$pid"; break; }
  sleep 0.01
done
rm -f "$ctl"
"#;

/// Where the subjects run.
pub(super) struct Board {
    /// The ssh destination.
    pub(super) host: String,
    /// The command that runs a script read on stdin there.
    pub(super) shell: String,
    /// The Rust triple the board runs.
    pub(super) triple: String,
    /// Variables the app runs with there, as `NAME=value`.
    pub(super) env: Vec<String>,
}

impl Board {
    /// `ssh` to the board: `FOOTPRINT_SSH` (a command and its options, such
    /// as `ssh -p 2222`), else `ssh`.
    fn ssh(&self) -> Command {
        let line = std::env::var("FOOTPRINT_SSH").unwrap_or_else(|_| "ssh".to_owned());
        let mut words = line.split_whitespace();
        let mut command = Command::new(words.next().unwrap_or("ssh"));
        command.args(words).arg(&self.host);
        command
    }

    /// The board's model (or architecture), processors, memory and kernel,
    /// read over ssh outside its shell: the machine, not the userland the
    /// app runs in.
    pub(super) fn describe(&self) -> Option<String> {
        let out = self
            .ssh()
            .arg(
                "tr -d '\\0' 2>/dev/null < /proc/device-tree/model || uname -m; echo; nproc; \
                 awk '/^MemTotal/ { print $2 }' /proc/meminfo; uname -sr",
            )
            .stdin(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let text = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let [model, cpus, kib, kernel] = lines[..] else {
            return None;
        };
        let gib = kib.parse::<f64>().ok()? / (1024.0 * 1024.0);
        Some(format!(
            "{model}, {cpus} CPUs, {gib:.1} GiB, {kernel} ({})",
            self.triple
        ))
    }

    /// Writes `binary` to the board; the path it has there.
    fn upload(&self, binary: &Path) -> Result<String, String> {
        let name = binary
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("an executable without a name")?;
        let path = format!("/tmp/{name}");
        let bytes =
            std::fs::read(binary).map_err(|e| format!("cannot read {}: {e}", binary.display()))?;
        let mut child = self
            .ssh()
            .arg(format!("cat > {path} && chmod +x {path}"))
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run ssh: {e}"))?;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(&bytes)
            .map_err(|e| format!("cannot send {name} to {}: {e}", self.host))?;
        let status = child.wait().map_err(|e| format!("ssh: {e}"))?;
        if !status.success() {
            return Err(format!("cannot write {path} on {} ({status})", self.host));
        }
        Ok(path)
    }

    /// One run of the app at `path` showing `scene`: what the probe read.
    fn once(&self, path: &str, scene: Scene) -> Result<Values, String> {
        let script = PROBE
            .replace("@BIN@", &quote(path))
            .replace("@SCENE@", scene.name())
            .replace(
                "@ENV@",
                &self
                    .env
                    .iter()
                    .map(|v| quote(v))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        let mut child = self
            .ssh()
            .arg(&self.shell)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("cannot run ssh: {e}"))?;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(script.as_bytes())
            .map_err(|e| format!("cannot send the probe to {}: {e}", self.host))?;
        let output = child.wait_with_output().map_err(|e| format!("ssh: {e}"))?;
        let text = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() || text.lines().any(|l| l.starts_with("error:")) {
            return Err(format!("the run on {} failed:\n{text}", self.host));
        }
        Ok(read(&text).values)
    }

    /// Measures `binary` showing `scene` on the board `runs` times, after a
    /// warm-up run, into `row`.
    pub(super) fn measure(
        &self,
        binary: &Path,
        scene: Scene,
        runs: u32,
        row: &mut Row,
    ) -> Result<(), String> {
        let path = self.upload(binary)?;
        let mut taken = Vec::new();
        for run in 0..=runs {
            let values = self.once(&path, scene)?;
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
}

/// `text` as one shell word.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The row the probe's lines give.
fn read(text: &str) -> Row {
    let mut row = Row::new("", "", "");
    let numbers = |rest: &str| -> Vec<f64> {
        rest.split_whitespace()
            .filter_map(|w| w.parse().ok())
            .collect()
    };
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(' ') else {
            continue;
        };
        let n = numbers(rest);
        match (key, n.as_slice()) {
            ("startup", [t0, t1]) => row.set("startup_ms", Some((t1 - t0) * 1e3)),
            ("idle", [ticks, woke, hz]) if *hz > 0.0 => {
                row.set("idle_cpu_ms_per_s", Some(ticks / hz * 1e3 / 3.0));
                row.set("idle_wakeups_per_s", Some(woke / 3.0));
            }
            ("rss", [kib]) => row.set("idle_rss_bytes", Some(kib * 1024.0)),
            ("peak", [kib]) => row.set("peak_rss_bytes", Some(kib * 1024.0)),
            ("footprint:", _) => {
                let (live, peak) = super::window::heap(line);
                row.set("heap_live_bytes", live);
                row.set("heap_peak_bytes", peak);
            }
            _ => {}
        }
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_probe_writes() {
        let row = read(
            "startup 1203.41 1203.66\n\
             idle 3 12 100\n\
             rss 5120\n\
             footprint: heap live=1048576 peak=2097152\n\
             peak 6144\n",
        );
        assert!((row.get("startup_ms").unwrap() - 250.0).abs() < 1e-6);
        assert_eq!(row.get("idle_cpu_ms_per_s"), Some(10.0));
        assert_eq!(row.get("idle_wakeups_per_s"), Some(4.0));
        assert_eq!(row.get("idle_rss_bytes"), Some(5120.0 * 1024.0));
        assert_eq!(row.get("peak_rss_bytes"), Some(6144.0 * 1024.0));
        assert_eq!(row.get("heap_live_bytes"), Some(1048576.0));
        assert_eq!(row.get("heap_peak_bytes"), Some(2097152.0));
    }

    #[test]
    fn a_word_is_quoted_whole() {
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), r"'it'\''s'");
    }
}
