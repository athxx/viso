//! What a profiled frame cost: the time each of its spans took on the UI
//! thread, the time the GPU spent on it, and the work it counted; how a run
//! of them summarizes; and the trace they are kept in.
//!
//! A runtime samples only while a tool captures: the dev session asks over
//! the dev channel (`viso profile`, `viso run --profile-frame`), a headless
//! run writes its samples to the file [`PROFILE_ENV`] names. The tool, the
//! CLI and Studio alike, summarizes them with [`summarize`] and keeps them as
//! a [`Trace`]: its Ende form is the canonical trace, [`chrome_trace`] the
//! Chrome trace-event JSON other viewers read.

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder, JsonWriter, ProtocolTag};

/// The environment variable a path is passed in where a headless run writes
/// the samples of every frame it draws, as a [`Trace`].
pub const PROFILE_ENV: &str = "VISO_HEADLESS_PROFILE";

/// The most frames a capture keeps.
pub const MAX_FRAMES: usize = 100_000;

/// A stretch of a frame on the UI thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Span {
    /// Input dispatched since the frame before: routing and handlers.
    Input,
    /// The state flush: reloads, animations, timers, tasks, the settle of
    /// states through their bindings and effects, and assets served.
    Flush,
    /// Text committed, edited, shaped and reflowed.
    Text,
    /// Measure and layout of the dirty subtrees, with the adaptive settle.
    Layout,
    /// Interaction styles resolved.
    Style,
    /// Paint of the dirty subtrees into primitives.
    Paint,
    /// The semantics tree updated for assistive technology.
    Semantics,
    /// Batching the primitives and uploading what changed to the GPU.
    Upload,
    /// Encoding the frame and presenting it, waiting for a drawable included.
    Submit,
}

impl Span {
    pub const COUNT: usize = 9;

    /// Every span, in the order a frame runs them.
    pub const ALL: [Span; Span::COUNT] = [
        Span::Input,
        Span::Flush,
        Span::Text,
        Span::Layout,
        Span::Style,
        Span::Paint,
        Span::Semantics,
        Span::Upload,
        Span::Submit,
    ];

    /// The span as metrics and traces name it.
    pub fn name(self) -> &'static str {
        match self {
            Span::Input => "input",
            Span::Flush => "flush",
            Span::Text => "text",
            Span::Layout => "layout",
            Span::Style => "style",
            Span::Paint => "paint",
            Span::Semantics => "semantics",
            Span::Upload => "upload",
            Span::Submit => "submit",
        }
    }
}

/// Work a frame counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Count {
    /// Nodes mounted, in every window.
    Nodes,
    /// Nodes some change invalidated.
    DirtyNodes,
    /// Nodes re-measured.
    Measured,
    /// Nodes re-placed.
    LaidOut,
    /// Primitives the paint rebuilt.
    Painted,
    /// Static binding edges evaluated.
    ReactiveEvals,
    /// Dynamic binding edges evaluated.
    DynamicEvals,
    /// Nodes that took a dynamic binding edge.
    DynamicFallbacks,
    /// Primitives the renderer drew.
    Primitives,
    /// Draw commands encoded.
    DrawCalls,
    /// Batches the primitives merged into.
    Batches,
    /// Instances drawn.
    Instances,
    /// Bytes uploaded to the GPU.
    UploadBytes,
    /// Text runs shaped.
    ShapedRuns,
    /// Glyphs rasterized.
    GlyphRasters,
    /// Glyph bytes uploaded to the atlases.
    GlyphUploadBytes,
    /// Glyph bytes resident in the atlases.
    GlyphCacheBytes,
    /// Atlas pages holding them.
    AtlasPages,
    /// Assets decoded.
    ResourceLoads,
}

impl Count {
    pub const COUNT: usize = 19;

    /// Every count, in the order metrics list them.
    pub const ALL: [Count; Count::COUNT] = [
        Count::Nodes,
        Count::DirtyNodes,
        Count::Measured,
        Count::LaidOut,
        Count::Painted,
        Count::ReactiveEvals,
        Count::DynamicEvals,
        Count::DynamicFallbacks,
        Count::Primitives,
        Count::DrawCalls,
        Count::Batches,
        Count::Instances,
        Count::UploadBytes,
        Count::ShapedRuns,
        Count::GlyphRasters,
        Count::GlyphUploadBytes,
        Count::GlyphCacheBytes,
        Count::AtlasPages,
        Count::ResourceLoads,
    ];

    /// The count as metrics and traces name it.
    pub fn name(self) -> &'static str {
        match self {
            Count::Nodes => "nodes",
            Count::DirtyNodes => "dirty_nodes",
            Count::Measured => "measured",
            Count::LaidOut => "laid_out",
            Count::Painted => "painted",
            Count::ReactiveEvals => "reactive_evals",
            Count::DynamicEvals => "dynamic_evals",
            Count::DynamicFallbacks => "dynamic_fallbacks",
            Count::Primitives => "primitives",
            Count::DrawCalls => "draw_calls",
            Count::Batches => "batches",
            Count::Instances => "instances",
            Count::UploadBytes => "upload_bytes",
            Count::ShapedRuns => "shaped_runs",
            Count::GlyphRasters => "glyph_rasters",
            Count::GlyphUploadBytes => "glyph_upload_bytes",
            Count::GlyphCacheBytes => "glyph_cache_bytes",
            Count::AtlasPages => "atlas_pages",
            Count::ResourceLoads => "resource_loads",
        }
    }
}

/// When a span began, in nanoseconds after its frame did, and how long it
/// took in all: a span a frame enters more than once (once per window) sums.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timed {
    pub start: u64,
    pub nanos: u64,
}

/// One frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameSample {
    /// Its number among the frames the capture sampled, from 0.
    pub frame: u64,
    /// When it began, in nanoseconds after the capture did: its first input
    /// dispatched, or its first phase.
    pub at: u64,
    /// The UI thread's time on it: its input and its phases.
    pub nanos: u64,
    /// Each span it ran, by [`Span`].
    pub spans: [Option<Timed>; Span::COUNT],
    /// The GPU's time on it, when the backend times frames.
    pub gpu: Option<u64>,
    /// Each count it took, by [`Count`].
    pub counts: [Option<u64>; Count::COUNT],
}

impl FrameSample {
    pub fn span(&self, span: Span) -> Option<Timed> {
        self.spans[span as usize]
    }

    pub fn count(&self, count: Count) -> Option<u64> {
        self.counts[count as usize]
    }
}

/// Spans and counts take a bit each in a mask ahead of their values.
const _: () = assert!(Span::COUNT <= 64 && Count::COUNT <= 64);

impl Encode for FrameSample {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_varint(self.frame);
        enc.write_varint(self.at);
        enc.write_varint(self.nanos);
        let mask = self
            .spans
            .iter()
            .enumerate()
            .filter(|(_, span)| span.is_some())
            .fold(0u64, |mask, (at, _)| mask | 1 << at);
        enc.write_varint(mask);
        for span in self.spans.iter().flatten() {
            enc.write_varint(span.start);
            enc.write_varint(span.nanos);
        }
        match self.gpu {
            Some(nanos) => enc.write_varint(nanos + 1),
            None => enc.write_varint(0),
        }
        let mask = self
            .counts
            .iter()
            .enumerate()
            .filter(|(_, count)| count.is_some())
            .fold(0u64, |mask, (at, _)| mask | 1 << at);
        enc.write_varint(mask);
        for count in self.counts.iter().flatten() {
            enc.write_varint(*count);
        }
    }
}

impl Decode for FrameSample {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let mut sample = FrameSample {
            frame: dec.read_varint()?,
            at: dec.read_varint()?,
            nanos: dec.read_varint()?,
            ..FrameSample::default()
        };
        // A bit past the spans and counts this build knows is a newer
        // runtime's: its value is read and left out.
        let mask = dec.read_varint()?;
        for bit in 0..64 {
            if mask & 1 << bit != 0 {
                let timed = Timed {
                    start: dec.read_varint()?,
                    nanos: dec.read_varint()?,
                };
                if let Some(slot) = sample.spans.get_mut(bit) {
                    *slot = Some(timed);
                }
            }
        }
        sample.gpu = dec.read_varint()?.checked_sub(1);
        let mask = dec.read_varint()?;
        for bit in 0..64 {
            if mask & 1 << bit != 0 {
                let count = dec.read_varint()?;
                if let Some(slot) = sample.counts.get_mut(bit) {
                    *slot = Some(count);
                }
            }
        }
        Ok(sample)
    }
}

/// A capture: what ran, and every frame it sampled.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Trace {
    /// The app, as its manifest names it.
    pub app: String,
    /// Where it ran: the runtime target and, when there is one, the device.
    pub target: String,
    pub device: Option<String>,
    /// When the capture began, in milliseconds since the Unix epoch.
    pub started_ms: u64,
    /// How long it lasted, in nanoseconds.
    pub nanos: u64,
    /// Frames the runtime sampled but could not hand over in time.
    pub dropped: u32,
    pub frames: Vec<FrameSample>,
}

/// What follows the stream tag of a trace file, then its version.
const TRACE_KIND: &[u8; 5] = b"trace";
const TRACE_VERSION: u16 = 1;

impl Trace {
    /// The trace as its `.ende` file holds it.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::with_capacity(64 + self.frames.len() * 64);
        ProtocolTag::current().encode(&mut enc);
        enc.write_raw(TRACE_KIND);
        enc.write_u16(TRACE_VERSION);
        enc.write_str(&self.app);
        enc.write_str(&self.target);
        match &self.device {
            Some(device) => {
                enc.write_u8(1);
                enc.write_str(device);
            }
            None => enc.write_u8(0),
        }
        enc.write_varint(self.started_ms);
        enc.write_varint(self.nanos);
        enc.write_varint(u64::from(self.dropped));
        write_frames(&mut enc, &self.frames);
        enc.into_bytes()
    }

    /// The trace `bytes` spell.
    ///
    /// # Errors
    ///
    /// Bytes that are not a trace of this version.
    pub fn from_bytes(bytes: &[u8]) -> Result<Trace, DecodeError> {
        let mut dec = Decoder::new(bytes);
        let tag = ProtocolTag::decode(&mut dec)?;
        let at = dec.position();
        if !tag.is_compatible()
            || dec.read_raw(TRACE_KIND.len())? != TRACE_KIND
            || dec.read_u16()? != TRACE_VERSION
        {
            return Err(DecodeError::Malformed { offset: at });
        }
        let app = dec.read_str()?.to_owned();
        let target = dec.read_str()?.to_owned();
        let device = match dec.read_u8()? {
            0 => None,
            _ => Some(dec.read_str()?.to_owned()),
        };
        let trace = Trace {
            app,
            target,
            device,
            started_ms: dec.read_varint()?,
            nanos: dec.read_varint()?,
            dropped: u32::try_from(dec.read_varint()?).unwrap_or(u32::MAX),
            frames: read_frames(&mut dec)?,
        };
        dec.finish()?;
        Ok(trace)
    }
}

/// Writes `frames`, counted.
pub fn write_frames(enc: &mut Encoder, frames: &[FrameSample]) {
    enc.write_varint(frames.len() as u64);
    for frame in frames {
        frame.encode(enc);
    }
}

/// Reads frames [`write_frames`] wrote.
///
/// # Errors
///
/// Bytes that are not frames, or more than [`MAX_FRAMES`].
pub fn read_frames(dec: &mut Decoder<'_>) -> Result<Vec<FrameSample>, DecodeError> {
    let at = dec.position();
    let count = dec.read_varint()?;
    if count > MAX_FRAMES as u64 {
        return Err(DecodeError::Malformed { offset: at });
    }
    (0..count).map(|_| FrameSample::decode(dec)).collect()
}

/// How a metric spread over the frames that took it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stat {
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
}

/// What a metric measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Millis,
    Count,
}

/// A metric of a summary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metric {
    pub name: &'static str,
    pub unit: Unit,
    pub stat: Stat,
}

/// The metrics of `frames`: the frame and each span over every frame, the
/// GPU and each count over the frames that took them; and the names of
/// those no frame took.
pub fn summarize(frames: &[FrameSample]) -> (Vec<Metric>, Vec<&'static str>) {
    let mut metrics = Vec::new();
    let mut missing = Vec::new();
    let mut values = Vec::with_capacity(frames.len());
    let mut add = |name, unit, values: &mut Vec<f64>| {
        match stat(values) {
            Some(stat) => metrics.push(Metric { name, unit, stat }),
            None => missing.push(name),
        }
        values.clear();
    };
    let millis = |nanos: u64| nanos as f64 / 1e6;
    values.extend(frames.iter().map(|f| millis(f.nanos)));
    add("frame", Unit::Millis, &mut values);
    // A frame that did not run a span spent nothing on it.
    for span in Span::ALL {
        values.extend(
            frames
                .iter()
                .map(|f| f.span(span).map_or(0.0, |t| millis(t.nanos))),
        );
        add(span.name(), Unit::Millis, &mut values);
    }
    values.extend(frames.iter().filter_map(|f| f.gpu).map(millis));
    add("gpu", Unit::Millis, &mut values);
    for count in Count::ALL {
        values.extend(
            frames
                .iter()
                .filter_map(|f| f.count(count))
                .map(|c| c as f64),
        );
        add(count.name(), Unit::Count, &mut values);
    }
    (metrics, missing)
}

/// The p50, p95 and maximum of `values`, by nearest rank; `None` when empty.
fn stat(values: &mut [f64]) -> Option<Stat> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let rank = |p: f64| {
        let at = (p * values.len() as f64).ceil() as usize;
        values[at.clamp(1, values.len()) - 1]
    };
    Some(Stat {
        p50: rank(0.50),
        p95: rank(0.95),
        max: values[values.len() - 1],
    })
}

/// `trace` as Chrome trace-event JSON: each frame and its spans as complete
/// events on the UI thread, the GPU's time and the counts as counters.
pub fn chrome_trace(trace: &Trace) -> String {
    let mut w = JsonWriter::new();
    let micros = |nanos: u64| nanos as f64 / 1e3;
    w.begin_object();
    w.name("displayTimeUnit");
    w.string("ms");
    w.name("traceEvents");
    w.begin_array();
    let meta = |w: &mut JsonWriter, name: &str, tid: Option<u64>, value: &str| {
        w.begin_object();
        w.name("name");
        w.string(name);
        w.name("ph");
        w.string("M");
        w.name("pid");
        w.uint(1);
        if let Some(tid) = tid {
            w.name("tid");
            w.uint(tid);
        }
        w.name("args");
        w.begin_object();
        w.name("name");
        w.string(value);
        w.end_object();
        w.end_object();
    };
    let process = match &trace.device {
        Some(device) => format!("{} ({}, {device})", trace.app, trace.target),
        None => format!("{} ({})", trace.app, trace.target),
    };
    meta(&mut w, "process_name", None, &process);
    meta(&mut w, "thread_name", Some(1), "UI");
    let complete = |w: &mut JsonWriter, name: &str, at: u64, nanos: u64, frame: u64| {
        w.begin_object();
        w.name("name");
        w.string(name);
        w.name("cat");
        w.string(if name == "frame" { "frame" } else { "span" });
        w.name("ph");
        w.string("X");
        w.name("ts");
        w.number(micros(at));
        w.name("dur");
        w.number(micros(nanos));
        w.name("pid");
        w.uint(1);
        w.name("tid");
        w.uint(1);
        w.name("args");
        w.begin_object();
        w.name("frame");
        w.uint(frame);
        w.end_object();
        w.end_object();
    };
    for frame in &trace.frames {
        let end = frame
            .spans
            .iter()
            .flatten()
            .map(|t| t.start + t.nanos)
            .max()
            .unwrap_or(0)
            .max(frame.nanos);
        complete(&mut w, "frame", frame.at, end, frame.frame);
        for span in Span::ALL {
            if let Some(t) = frame.span(span) {
                complete(
                    &mut w,
                    span.name(),
                    frame.at + t.start,
                    t.nanos,
                    frame.frame,
                );
            }
        }
        w.begin_object();
        w.name("name");
        w.string("counts");
        w.name("ph");
        w.string("C");
        w.name("ts");
        w.number(micros(frame.at));
        w.name("pid");
        w.uint(1);
        w.name("args");
        w.begin_object();
        if let Some(gpu) = frame.gpu {
            w.name("gpu_ms");
            w.number(gpu as f64 / 1e6);
        }
        for count in Count::ALL {
            if let Some(value) = frame.count(count) {
                w.name(count.name());
                w.uint(value);
            }
        }
        w.end_object();
        w.end_object();
    }
    w.end_array();
    w.end_object();
    w.into_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(frame: u64, nanos: u64) -> FrameSample {
        let mut sample = FrameSample {
            frame,
            at: frame * 16_000_000,
            nanos,
            ..FrameSample::default()
        };
        sample.spans[Span::Layout as usize] = Some(Timed {
            start: 1_000,
            nanos: nanos / 2,
        });
        sample.counts[Count::Nodes as usize] = Some(40 + frame);
        sample
    }

    #[test]
    fn a_trace_round_trips_and_refuses_what_is_not_one() {
        let mut gpu = sample(1, 3_000_000);
        gpu.gpu = Some(0);
        let trace = Trace {
            app: "demo".into(),
            target: "host".into(),
            device: Some("mac".into()),
            started_ms: 1_700_000_000_000,
            nanos: 5_000_000_000,
            dropped: 2,
            frames: vec![sample(0, 2_000_000), gpu],
        };
        let bytes = trace.to_bytes();
        assert_eq!(Trace::from_bytes(&bytes).unwrap(), trace);
        assert!(Trace::from_bytes(&bytes[..bytes.len() - 1]).is_err());
        let mut other = bytes.clone();
        other[4] = b'X';
        assert!(Trace::from_bytes(&other).is_err());
    }

    #[test]
    fn a_summary_takes_each_metric_over_the_frames_that_took_it() {
        let frames: Vec<FrameSample> = (0..20).map(|n| sample(n, (n + 1) * 1_000_000)).collect();
        let (metrics, missing) = summarize(&frames);
        let frame = metrics.iter().find(|m| m.name == "frame").unwrap();
        assert_eq!(frame.unit, Unit::Millis);
        assert_eq!(
            frame.stat,
            Stat {
                p50: 10.0,
                p95: 19.0,
                max: 20.0
            }
        );
        let nodes = metrics.iter().find(|m| m.name == "nodes").unwrap();
        assert_eq!(nodes.stat.max, 59.0);
        assert_eq!(missing.first(), Some(&"gpu"));
        let input = metrics.iter().find(|m| m.name == "input").unwrap();
        assert_eq!(input.stat.max, 0.0);
    }

    #[test]
    fn the_chrome_trace_lays_each_span_inside_its_frame() {
        let trace = Trace {
            app: "demo".into(),
            target: "headless".into(),
            frames: vec![sample(0, 2_000_000)],
            ..Trace::default()
        };
        let json = chrome_trace(&trace);
        let value = viso_ende::JsonValue::parse(&json).unwrap();
        let events = value.get("traceEvents").unwrap().as_array().unwrap();
        let names: Vec<&str> = events
            .iter()
            .filter_map(|e| e.get("name")?.as_str())
            .collect();
        assert_eq!(
            names,
            ["process_name", "thread_name", "frame", "layout", "counts"]
        );
        let layout = &events[3];
        assert_eq!(layout.get("ts").unwrap().as_f64(), Some(1.0));
        assert_eq!(layout.get("dur").unwrap().as_f64(), Some(1000.0));
    }
}
