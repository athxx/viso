//! The frames the session's runtimes draw, sampled for `viso profile` and
//! `viso run --profile-frame`.
//!
//! A runtime runs one capture at a time, which every consumer of its frames
//! shares: each `viso profile` waiting on it, for its frames or until its
//! time is up, and the stream `--profile-frame` reports frame by frame. The
//! session ends the capture once nobody consumes it; a `viso profile` alone
//! on it ends it at its deadline and is answered with what the runtime sent
//! up to its end.

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use viso_dev_protocol::profile::{Count, FrameSample, MAX_FRAMES, Span, Trace, Unit, summarize};
use viso_dev_protocol::wire::HostMessage;
use viso_ende::JsonWriter;

use super::DevSession;
use super::control::{self, ControlReply};
use crate::report::Reporter;

/// A capture of a runtime's frames.
pub(super) struct Capture {
    id: u32,
    /// Whether `--profile-frame` streams it.
    stream: bool,
    /// The `viso profile` calls waiting on it.
    waiting: Vec<Wait>,
    /// Whether it was ended, its last samples still to come.
    ending: bool,
    /// The frames of the stream's current second, for its human line.
    second: Vec<FrameSample>,
}

/// A `viso profile` waiting for frames.
struct Wait {
    reply: Sender<ControlReply>,
    /// The frames it wants, 0 for as many as its time takes.
    frames: u32,
    /// When its time is up.
    until: Option<Instant>,
    /// Whether its capture ends at its deadline, which it waits for.
    closing: bool,
    started: Instant,
    started_ms: u64,
    samples: Vec<FrameSample>,
    dropped: u32,
}

impl DevSession {
    /// Starts streaming the frames of runtime `at`, the frame time drawn over
    /// its windows.
    pub(super) fn stream_frames(&mut self, at: usize) {
        let id = self.query_id();
        let runtime = &mut self.runtimes[at];
        if runtime
            .writer
            .send(HostMessage::Profile { id, hud: true })
            .is_ok()
        {
            runtime.capture = Some(Capture {
                id,
                stream: true,
                waiting: Vec::new(),
                ending: false,
                second: Vec::new(),
            });
        }
    }

    /// Waits for the next `frames` frames the first runtime draws, or those
    /// it draws in the next `millis`, to answer `reply` with.
    pub(super) fn profile(&mut self, frames: u32, millis: u64, reply: Sender<ControlReply>) {
        let refuse = |reply: Sender<ControlReply>| {
            let _ = reply.send(ControlReply::refused(
                control::ENV_NO_DEV_SESSION,
                "the app is not connected to `viso run`",
            ));
        };
        if self.runtimes.is_empty() {
            return refuse(reply);
        }
        let now = Instant::now();
        let wait = Wait {
            reply,
            frames,
            until: (millis > 0).then(|| now + Duration::from_millis(millis)),
            closing: false,
            started: now,
            started_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
            samples: Vec::new(),
            dropped: 0,
        };
        // A capture already ending is not joined: a new one ends it.
        let running = self.runtimes[0]
            .capture
            .as_ref()
            .is_some_and(|capture| !capture.ending);
        if !running {
            let id = self.query_id();
            let runtime = &mut self.runtimes[0];
            if runtime
                .writer
                .send(HostMessage::Profile { id, hud: false })
                .is_err()
            {
                return refuse(wait.reply);
            }
            if let Some(old) = runtime.capture.take() {
                runtime.ended.push(old);
            }
            runtime.capture = Some(Capture {
                id,
                stream: false,
                waiting: Vec::new(),
                ending: false,
                second: Vec::new(),
            });
        }
        if let Some(capture) = &mut self.runtimes[0].capture {
            capture.waiting.push(wait);
        }
    }

    /// Takes the frames capture `id` of runtime `at` sent: each consumer's
    /// share, answering those it completes.
    pub(super) fn profiled(
        &mut self,
        at: usize,
        id: u32,
        frames: Vec<FrameSample>,
        dropped: u32,
        done: bool,
        out: &mut dyn Reporter,
    ) {
        let target = self.runtimes[at].hello.target.as_str();
        let runtime = &mut self.runtimes[at];
        let capture = if runtime.capture.as_ref().is_some_and(|c| c.id == id) {
            runtime.capture.as_mut()
        } else {
            runtime.ended.iter_mut().find(|c| c.id == id)
        };
        let Some(capture) = capture else {
            return;
        };
        if capture.stream {
            for frame in &frames {
                report_frame(frame, out);
                if capture
                    .second
                    .first()
                    .is_some_and(|first| frame.at >= first.at + 1_000_000_000)
                {
                    if out.human() {
                        eprintln!("[viso] {}", line(&capture.second));
                    }
                    capture.second.clear();
                }
                capture.second.push(*frame);
            }
        }
        let mut finished = Vec::new();
        for (index, wait) in capture.waiting.iter_mut().enumerate() {
            wait.dropped = wait.dropped.saturating_add(dropped);
            let room = if wait.frames == 0 {
                MAX_FRAMES
            } else {
                wait.frames as usize
            }
            .saturating_sub(wait.samples.len());
            wait.samples.extend(frames.iter().take(room));
            if done || (wait.frames > 0 && wait.samples.len() >= wait.frames as usize) {
                finished.push(index);
            }
        }
        for index in finished.into_iter().rev() {
            let wait = capture.waiting.swap_remove(index);
            answer(wait, target);
        }
        if done {
            runtime.ended.retain(|c| c.id != id);
            if runtime.capture.as_ref().is_some_and(|c| c.id == id) {
                runtime.capture = None;
            }
            return;
        }
        Self::release(runtime);
    }

    /// When a `viso profile` is next due, if any waits on a deadline.
    pub(super) fn profile_deadline(&self) -> Option<Instant> {
        self.runtimes
            .iter()
            .filter_map(|r| r.capture.as_ref())
            .flat_map(|c| &c.waiting)
            .filter(|wait| !wait.closing)
            .filter_map(|wait| wait.until)
            .min()
    }

    /// Ends the waits whose time is up: one alone on its capture ends the
    /// capture and is answered at its end; one sharing it is answered now.
    pub(super) fn expire_profiles(&mut self) {
        let now = Instant::now();
        for at in 0..self.runtimes.len() {
            let target = self.runtimes[at].hello.target.as_str();
            let runtime = &mut self.runtimes[at];
            let Some(capture) = &mut runtime.capture else {
                continue;
            };
            let alone = !capture.stream && capture.waiting.len() == 1;
            let mut index = 0;
            while index < capture.waiting.len() {
                let wait = &mut capture.waiting[index];
                if wait.closing || wait.until.is_none_or(|until| until > now) {
                    index += 1;
                } else if alone {
                    wait.closing = true;
                    index += 1;
                } else {
                    answer(capture.waiting.swap_remove(index), target);
                }
            }
            if alone && capture.waiting.iter().all(|w| w.closing) {
                Self::end(runtime);
            } else {
                Self::release(runtime);
            }
        }
    }

    /// Ends the capture of `runtime` once nobody consumes it.
    fn release(runtime: &mut super::Runtime) {
        if runtime
            .capture
            .as_ref()
            .is_some_and(|c| !c.stream && c.waiting.is_empty() && !c.ending)
        {
            Self::end(runtime);
        }
    }

    /// Asks `runtime` to end its capture, whose last samples come with its
    /// end.
    fn end(runtime: &mut super::Runtime) {
        let Some(capture) = &mut runtime.capture else {
            return;
        };
        if !capture.ending {
            capture.ending = true;
            let _ = runtime
                .writer
                .send(HostMessage::ProfileEnd { id: capture.id });
        }
    }

    /// The id the next question to a runtime takes.
    fn query_id(&mut self) -> u32 {
        let id = self.next_query;
        self.next_query = self.next_query.wrapping_add(1);
        id
    }
}

/// Refuses every `viso profile` waiting on `runtime`, which left.
pub(super) fn gone(runtime: &super::Runtime) {
    for capture in runtime.capture.iter().chain(&runtime.ended) {
        for wait in &capture.waiting {
            let _ = wait.reply.send(ControlReply::refused(
                control::PROFILE_APP_GONE,
                "the app left `viso run` while it was profiled",
            ));
        }
    }
}

/// Answers `wait` with its frames, renumbered from its first, as a trace
/// of `target`.
fn answer(wait: Wait, target: &str) {
    let mut frames = wait.samples;
    if let Some(first) = frames.first().copied() {
        for frame in &mut frames {
            frame.frame = frame.frame.saturating_sub(first.frame);
            frame.at = frame.at.saturating_sub(first.at);
        }
    }
    let trace = Trace {
        app: String::new(),
        target: target.to_owned(),
        device: None,
        started_ms: wait.started_ms,
        nanos: u64::try_from(wait.started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        dropped: wait.dropped,
        frames,
    };
    let _ = wait.reply.send(ControlReply::Profiled {
        trace: trace.to_bytes(),
    });
}

/// Reports `frame` as a `profile` event under `--json`.
fn report_frame(frame: &FrameSample, out: &mut dyn Reporter) {
    out.profile(
        &mut |w| {
            w.begin_object();
            w.name("frame");
            w.uint(frame.frame);
            w.name("at_ms");
            w.number(frame.at as f64 / 1e6);
            w.name("metrics");
            frame_metrics(w, frame);
            w.end_object();
        },
        &mut || None,
    );
}

/// The metrics of one frame as an object: times in milliseconds, counts as
/// numbers; what it did not take is left out.
pub fn frame_metrics(w: &mut JsonWriter, frame: &FrameSample) {
    let ms = |nanos: u64| nanos as f64 / 1e6;
    w.begin_object();
    w.name("frame");
    w.number(ms(frame.nanos));
    for span in Span::ALL {
        if let Some(t) = frame.span(span) {
            w.name(span.name());
            w.number(ms(t.nanos));
        }
    }
    if let Some(gpu) = frame.gpu {
        w.name("gpu");
        w.number(ms(gpu));
    }
    for count in Count::ALL {
        if let Some(value) = frame.count(count) {
            w.name(count.name());
            w.uint(value);
        }
    }
    w.end_object();
}

/// One line on `frames`: how many, and the frame and GPU times at p50 and
/// p95.
pub fn line(frames: &[FrameSample]) -> String {
    let (metrics, _) = summarize(frames);
    let mut line = format!("profile  {} frames", frames.len());
    for metric in metrics.iter().filter(|m| m.unit == Unit::Millis) {
        if metric.name == "frame" || metric.name == "gpu" {
            line.push_str(&format!(
                "  {} {:.2}/{:.2} ms",
                metric.name, metric.stat.p50, metric.stat.p95
            ));
        }
    }
    line
}
