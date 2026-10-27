//! The binary codec and the JSON emitter on their own: one small message
//! encoded and decoded, a stream of 100k of them, the steady encode into a
//! reused buffer, an owned and a borrowed decode, and a run of 1k compiler
//! diagnostics written and read back as JSON.
//!
//! - `ende_bin_small_message/event`        — `encode_to_vec` + `decode_from_slice` of one event;
//! - `ende_bin_100k_messages/stream`       — 100k events into one reused encoder, read back borrowed;
//! - `ende_bin_encode_reused_buffer/event` — one event into an encoder cleared each time;
//! - `ende_bin_decode_owned/event`         — one event decoded into its owned form;
//! - `ende_bin_decode_borrowed/event`      — one event read in place, borrowing the input;
//! - `ende_json_diagnostic_1k/write`       — 1k diagnostic objects through a `JsonWriter`;
//! - `ende_json_diagnostic_1k/parse`       — the same document read by `JsonValue::parse`.
//!
//! Before timing, each prints the bytes a message takes and the heap
//! allocations (and bytes) one costs once warmed.
//!
//! Run release (`cargo bench -p viso-ende --bench codec`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder, JsonValue, JsonWriter};

struct CountingAlloc;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

// SAFETY: forwards every call to the system allocator unchanged; the only added
// behavior is relaxed counter increments on allocation while armed.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: `layout` is the caller's, valid for `alloc`; the system allocator gets it unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was allocated by `System` with `layout`, through this allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(new_size, Ordering::Relaxed);
        }
        // SAFETY: `ptr` was allocated by `System` with `layout`, and the caller upholds `realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

/// The heap allocations and bytes `f` makes over `n` runs, per run, after one
/// unmeasured run warms what it reuses.
fn per_run(n: usize, mut f: impl FnMut()) -> (f64, f64) {
    f();
    ALLOCS.store(0, Ordering::Relaxed);
    ALLOC_BYTES.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
    for _ in 0..n {
        f();
    }
    ARMED.store(false, Ordering::Relaxed);
    let allocs = ALLOCS.load(Ordering::Relaxed) as f64 / n as f64;
    let bytes = ALLOC_BYTES.load(Ordering::Relaxed) as f64 / n as f64;
    (allocs, bytes)
}

/// A pointer event as a dev tool records it: fixed-width fields, a varint
/// timestamp, the target's name and a small opaque payload.
#[derive(Debug, Clone, PartialEq)]
struct Event {
    node: u32,
    kind: u8,
    x: f32,
    y: f32,
    time: u64,
    target: String,
    payload: Vec<u8>,
}

impl Encode for Event {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u32(self.node);
        enc.write_u8(self.kind);
        enc.write_f32(self.x);
        enc.write_f32(self.y);
        enc.write_varint(self.time);
        enc.write_str(&self.target);
        enc.write_bytes(&self.payload);
    }
}

impl Decode for Event {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(Event {
            node: dec.read_u32()?,
            kind: dec.read_u8()?,
            x: dec.read_f32()?,
            y: dec.read_f32()?,
            time: dec.read_varint()?,
            target: dec.read_str()?.to_owned(),
            payload: dec.read_bytes()?.to_vec(),
        })
    }
}

/// An [`Event`] read in place: its name and payload borrow the input.
#[derive(Debug, Clone, Copy, PartialEq)]
struct EventRef<'a> {
    node: u32,
    kind: u8,
    x: f32,
    y: f32,
    time: u64,
    target: &'a str,
    payload: &'a [u8],
}

impl<'a> EventRef<'a> {
    fn read(dec: &mut Decoder<'a>) -> Result<EventRef<'a>, DecodeError> {
        Ok(EventRef {
            node: dec.read_u32()?,
            kind: dec.read_u8()?,
            x: dec.read_f32()?,
            y: dec.read_f32()?,
            time: dec.read_varint()?,
            target: dec.read_str()?,
            payload: dec.read_bytes()?,
        })
    }

    fn is(&self, event: &Event) -> bool {
        (self.node, self.kind, self.x, self.y, self.time)
            == (event.node, event.kind, event.x, event.y, event.time)
            && self.target == event.target
            && self.payload == event.payload
    }
}

const TARGETS: [&str; 4] = ["counter.button", "list.row", "search.field", "nav.tab"];

/// The `i`th event of a session: a press, move or release over one of a few
/// targets, a millisecond-ish apart.
fn event(i: u32) -> Event {
    Event {
        node: 1 + i % 997,
        kind: (i % 3) as u8,
        x: (i % 800) as f32 + 0.5,
        y: (i % 480) as f32 + 0.25,
        time: 1_000_000 + u64::from(i) * 1_013,
        target: TARGETS[i as usize % TARGETS.len()].to_owned(),
        payload: vec![(i % 251) as u8; 4 + i as usize % 13],
    }
}

/// The most events a stream holds.
const MAX_EVENTS: usize = 1 << 20;

fn write_stream(enc: &mut Encoder, events: &[Event]) {
    enc.write_varint(events.len() as u64);
    for event in events {
        event.encode(enc);
    }
}

/// Reads every event of a stream in place and folds them, so none is
/// optimized away.
fn read_stream(bytes: &[u8]) -> Result<u64, DecodeError> {
    let mut dec = Decoder::new(bytes);
    let count = dec.read_count(MAX_EVENTS)?;
    let mut sum = 0u64;
    for _ in 0..count {
        let event = EventRef::read(&mut dec)?;
        sum = sum
            .wrapping_add(u64::from(event.node))
            .wrapping_add(event.time)
            .wrapping_add(event.target.len() as u64 + event.payload.len() as u64);
    }
    dec.finish()?;
    Ok(sum)
}

fn small_message(c: &mut Criterion) {
    let msg = event(7);
    let bytes = msg.encode_to_vec();
    assert_eq!(Event::decode_from_slice(&bytes).as_ref(), Ok(&msg));
    let mut dec = Decoder::new(&bytes);
    assert!(EventRef::read(&mut dec).unwrap().is(&msg));
    assert_eq!(dec.finish(), Ok(()));

    let (allocs, alloc_bytes) = per_run(1000, || {
        let bytes = black_box(&msg).encode_to_vec();
        black_box(Event::decode_from_slice(&bytes).unwrap());
    });
    eprintln!(
        "ende_bin_small_message: {} bytes/message, {allocs} allocations/message ({alloc_bytes} bytes), \
         encode_to_vec + decode_from_slice of one owned event",
        bytes.len()
    );
    let mut g = c.benchmark_group("ende_bin_small_message");
    g.throughput(Throughput::Bytes(bytes.len() as u64));
    g.bench_function("event", |b| {
        b.iter(|| {
            let bytes = black_box(&msg).encode_to_vec();
            black_box(Event::decode_from_slice(&bytes).unwrap())
        })
    });
    g.finish();

    let mut enc = Encoder::new();
    let (allocs, alloc_bytes) = per_run(1000, || {
        enc.clear();
        black_box(&msg).encode(&mut enc);
        black_box(enc.as_bytes());
    });
    assert_eq!(enc.as_bytes(), bytes);
    eprintln!(
        "ende_bin_encode_reused_buffer: {} bytes/message, {allocs} allocations/message ({alloc_bytes} bytes), \
         one event into an encoder cleared each time",
        bytes.len()
    );
    let mut g = c.benchmark_group("ende_bin_encode_reused_buffer");
    g.throughput(Throughput::Bytes(bytes.len() as u64));
    g.bench_function("event", |b| {
        b.iter(|| {
            enc.clear();
            black_box(&msg).encode(&mut enc);
            black_box(enc.len())
        })
    });
    g.finish();

    let (allocs, alloc_bytes) = per_run(1000, || {
        black_box(Event::decode_from_slice(black_box(&bytes)).unwrap());
    });
    eprintln!(
        "ende_bin_decode_owned: {} bytes/message, {allocs} allocations/message ({alloc_bytes} bytes), \
         one event decoded into its owned String and Vec",
        bytes.len()
    );
    let mut g = c.benchmark_group("ende_bin_decode_owned");
    g.throughput(Throughput::Bytes(bytes.len() as u64));
    g.bench_function("event", |b| {
        b.iter(|| Event::decode_from_slice(black_box(&bytes)).unwrap())
    });
    g.finish();

    let read = |bytes: &[u8]| {
        let mut dec = Decoder::new(bytes);
        let event = EventRef::read(&mut dec).unwrap();
        dec.finish().unwrap();
        black_box(event.node) ^ black_box(event.target.len() as u32)
    };
    let (allocs, alloc_bytes) = per_run(1000, || {
        black_box(read(black_box(&bytes)));
    });
    eprintln!(
        "ende_bin_decode_borrowed: {} bytes/message, {allocs} allocations/message ({alloc_bytes} bytes), \
         one event read in place, its name and payload borrowing the input",
        bytes.len()
    );
    let mut g = c.benchmark_group("ende_bin_decode_borrowed");
    g.throughput(Throughput::Bytes(bytes.len() as u64));
    g.bench_function("event", |b| b.iter(|| read(black_box(&bytes))));
    g.finish();
}

fn many_messages(c: &mut Criterion) {
    const N: u32 = 100_000;
    let events: Vec<Event> = (0..N).map(event).collect();
    let mut enc = Encoder::new();
    write_stream(&mut enc, &events);
    let len = enc.len();
    let expected = read_stream(enc.as_bytes()).unwrap();
    {
        let mut dec = Decoder::new(enc.as_bytes());
        let decoded = dec.read_list(MAX_EVENTS, |dec| Event::decode(dec)).unwrap();
        assert_eq!(decoded, events, "the stream reads back every event");
    }

    let (allocs, alloc_bytes) = per_run(4, || {
        enc.clear();
        write_stream(&mut enc, black_box(&events));
        black_box(read_stream(enc.as_bytes()).unwrap());
    });
    eprintln!(
        "ende_bin_100k_messages: {:.1} bytes/message ({len} bytes), {} allocations/message ({alloc_bytes} bytes per stream), \
         100k events into one reused encoder, then read back in place",
        len as f64 / f64::from(N),
        allocs / f64::from(N)
    );
    let mut g = c.benchmark_group("ende_bin_100k_messages");
    g.throughput(Throughput::Bytes(len as u64));
    g.sample_size(20);
    g.bench_function("stream", |b| {
        b.iter(|| {
            enc.clear();
            write_stream(&mut enc, black_box(&events));
            let sum = read_stream(enc.as_bytes()).unwrap();
            debug_assert_eq!(sum, expected);
            sum
        })
    });
    g.finish();
}

/// A compiler diagnostic as the CLI reports it: what the JSON object holds.
struct Diagnostic {
    severity: &'static str,
    code: String,
    message: String,
    file: String,
    start: u64,
    end: u64,
    line: u64,
    column: u64,
    related: Option<(u64, u64, &'static str)>,
    notes: &'static [&'static str],
    fix: Option<(String, u64, u64, &'static str)>,
}

fn diagnostic(i: u64) -> Diagnostic {
    let start = 40 + i * 37;
    Diagnostic {
        severity: if i.is_multiple_of(5) {
            "warning"
        } else {
            "error"
        },
        code: format!("E{}", 2000 + i % 40),
        message: format!("unresolved \"Badge{i}\" in `view` (did you mean \"Badge\"?)"),
        file: format!("src/screens/page_{}.vs", i % 12),
        start,
        end: start + 6,
        line: 3 + i,
        column: 9 + i % 30,
        related: i
            .is_multiple_of(3)
            .then_some((start / 2, start / 2 + 5, "declared here")),
        notes: if i.is_multiple_of(4) {
            &["a component is in scope once its module is imported"]
        } else {
            &[]
        },
        fix: i
            .is_multiple_of(2)
            .then(|| ("import `Badge`".to_owned(), 0, 0, "import ui::Badge;\n")),
    }
}

fn location(w: &mut JsonWriter, d: &Diagnostic, start: u64, end: u64) {
    w.name("file");
    w.string(&d.file);
    w.name("byte_start");
    w.uint(start);
    w.name("byte_end");
    w.uint(end);
    w.name("line");
    w.uint(d.line);
    w.name("column_utf16");
    w.uint(d.column);
    w.name("end_line");
    w.uint(d.line);
    w.name("end_column_utf16");
    w.uint(d.column + (end - start));
}

/// The diagnostics as one JSON array of diagnostic objects.
fn write_diagnostics(diagnostics: &[Diagnostic]) -> String {
    let mut w = JsonWriter::new();
    w.begin_array();
    for d in diagnostics {
        w.begin_object();
        w.name("schema_version");
        w.string("1");
        w.name("severity");
        w.string(d.severity);
        w.name("code");
        w.string(&d.code);
        w.name("message");
        w.string(&d.message);
        w.name("primary");
        w.begin_object();
        location(&mut w, d, d.start, d.end);
        w.end_object();
        w.name("related");
        w.begin_array();
        if let Some((start, end, label)) = d.related {
            w.begin_object();
            location(&mut w, d, start, end);
            w.name("message");
            w.string(label);
            w.end_object();
        }
        w.end_array();
        w.name("expected");
        w.begin_array();
        w.end_array();
        w.name("actual");
        w.null();
        w.name("notes");
        w.begin_array();
        for note in d.notes {
            w.string(note);
        }
        w.end_array();
        w.name("fixes");
        w.begin_array();
        if let Some((title, start, end, replacement)) = &d.fix {
            w.begin_object();
            w.name("title");
            w.string(title);
            w.name("applicability");
            w.string("machine");
            w.name("edits");
            w.begin_array();
            w.begin_object();
            w.name("file");
            w.string(&d.file);
            w.name("byte_start");
            w.uint(*start);
            w.name("byte_end");
            w.uint(*end);
            w.name("replacement");
            w.string(replacement);
            w.end_object();
            w.end_array();
            w.end_object();
        }
        w.end_array();
        w.end_object();
    }
    w.end_array();
    w.into_string()
}

fn json_diagnostics(c: &mut Criterion) {
    const N: u64 = 1000;
    let diagnostics: Vec<Diagnostic> = (0..N).map(diagnostic).collect();
    let text = write_diagnostics(&diagnostics);
    let parsed = JsonValue::parse(&text).expect("the writer emits valid JSON");
    let items = parsed.as_array().unwrap();
    assert_eq!(items.len(), N as usize);
    for (item, d) in items.iter().zip(&diagnostics) {
        assert_eq!(item.get("code").and_then(JsonValue::as_str), Some(&*d.code));
        assert_eq!(
            item.get("message").and_then(JsonValue::as_str),
            Some(&*d.message)
        );
        let fixes = item.get("fixes").and_then(JsonValue::as_array).unwrap();
        assert_eq!(fixes.len(), usize::from(d.fix.is_some()));
    }

    let (write_allocs, write_bytes) = per_run(4, || {
        black_box(write_diagnostics(black_box(&diagnostics)));
    });
    let (parse_allocs, parse_bytes) = per_run(4, || {
        black_box(JsonValue::parse(black_box(&text)).unwrap());
    });
    eprintln!(
        "ende_json_diagnostic_1k: {:.1} bytes/diagnostic ({} bytes); write {:.2} allocations/diagnostic ({write_bytes} bytes), \
         parse {:.2} allocations/diagnostic ({parse_bytes} bytes); 1k CLI diagnostic objects as one array",
        text.len() as f64 / N as f64,
        text.len(),
        write_allocs / N as f64,
        parse_allocs / N as f64,
    );
    let mut g = c.benchmark_group("ende_json_diagnostic_1k");
    g.throughput(Throughput::Bytes(text.len() as u64));
    g.bench_function("write", |b| {
        b.iter(|| write_diagnostics(black_box(&diagnostics)))
    });
    g.bench_function("parse", |b| {
        b.iter(|| JsonValue::parse(black_box(&text)).unwrap())
    });
    g.finish();
}

criterion_group!(benches, small_message, many_messages, json_diagnostics);
criterion_main!(benches);
