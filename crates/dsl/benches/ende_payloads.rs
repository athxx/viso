//! The codec on the payloads its users carry, built by their own crates:
//!
//! - `ende_profiler_trace_encode/frames_1k`    — 1k profiled frames, every span and count
//!   taken, into one reused encoder as a profile batch writes them;
//! - `ende_aot_package_validate/leaves_1k`     — a 1k-leaf AOT package validated in place;
//! - `ende_dev_patch_roundtrip/fresh_views`    — a patch mounting two compiled views fresh,
//!   with a resource section, framed and read back;
//! - `ende_hot_reload_patch_roundtrip/edit`    — the patch one edit of a compiled view plans
//!   (structural ops, node carries, state plans), framed and read back;
//! - `ende_game_packet_roundtrip/steady`       — a rollback session's steady packet written
//!   into a reused encoder and taken by its peer.
//!
//! Before timing, each checks the bytes read back to what was written and
//! prints the bytes a message takes and the heap allocations (and bytes) one
//! costs once warmed.
//!
//! Run release (`cargo bench -p viso-dsl --bench ende_payloads`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use viso_behavior::game::{RollbackSession, Scheduler, SessionConfig};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Vm};
use viso_dev_protocol::profile::{Count, FrameSample, Span, Timed, read_frames, write_frames};
use viso_dev_protocol::wire::{
    BuildId, DevSessionId, FileId, HostMessage, Message, PatchBundle, PatchSection, ResourceAsset,
    ResourcePatch, Revision, RuntimeSessionId, UiPatch, content_hash, frame_into,
};
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::{Determinism, TargetProfile};
use viso_dsl::hotreload::{CandidatePlan, plan_view, view_patch};
use viso_ende::{Decoder, Encode, Encoder};
use viso_schema::aot::{AotEdge, AotNode, AotNodeKind, AotPackage, AotStyle, AotView};
use viso_schema::layout::{BoxRules, GridPlacement, Placement, SpanInset};
use viso_schema::{DirtyClass, LengthTerms, NodeLengths, StateKey};

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

/// The `i`th frame of a capture at 60 Hz: every span timed, the GPU timed,
/// every count taken, the values varying as a real run's do.
fn frame(i: u64) -> FrameSample {
    let mut spans = [None; Span::COUNT];
    let mut start = 0;
    for (k, slot) in spans.iter_mut().enumerate() {
        let nanos = 20_000 + (i * 7_919 + k as u64 * 104_729) % 900_000;
        *slot = Some(Timed { start, nanos });
        start += nanos + 1_000;
    }
    let mut counts = [None; Count::COUNT];
    for (k, slot) in counts.iter_mut().enumerate() {
        *slot = Some((i * 31 + k as u64 * 977) % (1 << (4 + k % 16)));
    }
    FrameSample {
        frame: i,
        at: i * 16_666_667,
        nanos: start,
        spans,
        gpu: Some(3_000_000 + i % 500_000),
        counts,
    }
}

fn profiler_trace(c: &mut Criterion) {
    const N: u64 = 1000;
    let frames: Vec<FrameSample> = (0..N).map(frame).collect();
    let mut enc = Encoder::new();
    write_frames(&mut enc, &frames);
    let len = enc.len();
    let read = read_frames(&mut Decoder::new(enc.as_bytes())).unwrap();
    assert_eq!(read, frames, "the batch reads back every frame");

    let (allocs, alloc_bytes) = per_run(16, || {
        enc.clear();
        write_frames(&mut enc, black_box(&frames));
    });
    eprintln!(
        "ende_profiler_trace_encode: {:.1} bytes/frame ({len} bytes), {} allocations/frame ({alloc_bytes} bytes per batch), \
         1k frames of {} spans and {} counts into one reused encoder",
        len as f64 / N as f64,
        allocs / N as f64,
        Span::COUNT,
        Count::COUNT,
    );
    let mut g = c.benchmark_group("ende_profiler_trace_encode");
    g.throughput(Throughput::Elements(N));
    g.bench_function("frames_1k", |b| {
        b.iter(|| {
            enc.clear();
            write_frames(&mut enc, black_box(&frames));
            enc.len()
        })
    });
    g.finish();
}

/// A row of `leaves` leaves, each with lengths and an edge, every other one
/// with box rules and every third one placed.
fn tree(leaves: u32) -> AotPackage {
    let mut nodes = vec![AotNode {
        kind: AotNodeKind::Flex,
        style: AotStyle {
            gap: Some(4.0),
            ..AotStyle::default()
        },
        lengths: None,
        rules: None,
        grid: None,
        child_count: leaves,
    }];
    let mut edges = Vec::new();
    for i in 0..leaves {
        nodes.push(AotNode {
            kind: AotNodeKind::Leaf,
            style: AotStyle {
                place: i
                    .is_multiple_of(3)
                    .then_some(Placement::Grid(GridPlacement {
                        column: Some(i as u16),
                        column_span: 2,
                        ..GridPlacement::default()
                    })),
                ..AotStyle::default()
            },
            lengths: Some(Box::new(NodeLengths {
                width: Some(LengthTerms {
                    dp: i as f32,
                    ..LengthTerms::default()
                }),
                ..NodeLengths::default()
            })),
            rules: i.is_multiple_of(2).then(|| {
                Box::new(BoxRules {
                    min_width: viso_schema::layout::Span::dp(i as f32),
                    margin: SpanInset {
                        top: viso_schema::layout::Span::pct(5.0),
                        ..SpanInset::ZERO
                    },
                    ..BoxRules::NONE
                })
            }),
            grid: None,
            child_count: 0,
        });
        edges.push(AotEdge {
            state: StateKey::from_parts(1, u64::from(i)),
            node: i + 1,
            class: DirtyClass::PAINT.bits(),
        });
    }
    AotPackage { nodes, edges }
}

fn aot_package(c: &mut Criterion) {
    const LEAVES: u32 = 1000;
    let package = tree(LEAVES);
    let blob = package.encode_to_vec();
    let view = AotView::new(&blob).expect("a valid package");
    assert_eq!(view.node_count(), LEAVES as usize + 1);
    assert_eq!(view.edge_count(), LEAVES as usize);
    assert_eq!(view.to_package(), package, "the owned form is the same");

    let (allocs, alloc_bytes) = per_run(16, || {
        black_box(AotView::new(black_box(&blob)).unwrap());
    });
    eprintln!(
        "ende_aot_package_validate: {} bytes/package ({:.1} bytes/node), {allocs} allocations/package ({alloc_bytes} bytes), \
         header, section table, every node record, the pre-order forest and every edge of {} nodes checked in place",
        blob.len(),
        blob.len() as f64 / f64::from(LEAVES + 1),
        LEAVES + 1,
    );
    let mut g = c.benchmark_group("ende_aot_package_validate");
    g.throughput(Throughput::Bytes(blob.len() as u64));
    g.bench_function("leaves_1k", |b| {
        b.iter(|| AotView::new(black_box(&blob)).unwrap().node_count())
    });
    g.finish();
}

const COUNTER: &str = r#"
component Counter {
    state count = 0;
    state enabled = true;
    view {
        Column {
            width: 120dp;
            Text("{count}")
            Text { visible: enabled; }
        }
    }
}
"#;

/// A list page: rows mounted by a keyed `for`, a toggled region and a
/// `match` over a state, each row a component with an event.
fn todo(extra: &str, edited: bool) -> String {
    let header = if edited {
        "Text { width: 20dp; height: 20dp; on click { show = !show; } }\n\
         Text(\"{done}\") { width: 40dp; height: 20dp }"
    } else {
        "Text { width: 20dp; height: 20dp; on click { show = !show; } }"
    };
    let width = if edited { 420 } else { 400 };
    format!(
        r#"
component TodoItem {{
    input id: I64;
    event toggled(id: I64);
    view {{
        Row {{
            width: 400dp;
            height: 20dp;
            Text {{ width: 20dp; height: 20dp; on click {{ emit toggled(id); }} }}
        }}
    }}
}}

export component TodoApp {{
    state items = [1, 2, 3];
    state done = 0;
    state show = true;
    {extra}
    view {{
        Column {{
            width: {width}dp;
            height: 300dp;
            Row {{
                width: 400dp;
                height: 20dp;
                Text {{ width: 20dp; height: 20dp; on click {{ items = [3, 1, 2]; }} }}
                Text {{ width: 20dp; height: 20dp; on click {{ items = [3, 1, 2, 4]; }} }}
                Text {{ width: 20dp; height: 20dp; on click {{ items = [2, 4]; }} }}
                {header}
            }}
            Column {{
                width: 400dp;
                height: 80dp;
                for item in items key item {{
                    TodoItem {{ id: item; on toggled(event) {{ done = event.id; }} }}
                }}
            }}
            Column {{
                width: 400dp;
                height: 20dp;
                if show {{
                    Text {{ width: 30dp; height: 20dp; }}
                }} else {{
                    Text {{ width: 10dp; height: 10dp; }}
                    Text {{ width: 10dp; height: 10dp; }}
                }}
            }}
            Column {{
                width: 400dp;
                height: 20dp;
                match done {{
                    0 => {{ }},
                    2 => {{ Text {{ width: 50dp; height: 20dp; }} }},
                    _ => {{ Text {{ width: 5dp; height: 5dp; }} }},
                }}
            }}
        }}
    }}
}}
"#
    )
}

fn origin(module: &str) -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec![module.to_owned()],
        language: None,
        ..Default::default()
    }
}

fn planned(source: &str, module: &str) -> CandidatePlan {
    plan_view(source, &origin(module)).expect("the view compiles")
}

fn bundle(sections: Vec<PatchSection>) -> HostMessage {
    HostMessage::Patch(Box::new(PatchBundle {
        dev_session: DevSessionId(0x0123_4567_89ab_cdef_0011_2233_4455_6677),
        target_runtime: RuntimeSessionId(0x7766_5544_3322_1100_fedc_ba98_7654_3210),
        base_revision: Revision(41),
        next_revision: Revision(42),
        build_id: BuildId(0xdead_beef_0000_0000_0000_0000_cafe_f00d),
        sections,
    }))
}

/// Frames `message` into `enc` and reads it back from the frame's body.
fn roundtrip(message: &HostMessage, enc: &mut Encoder) -> HostMessage {
    let frame = frame_into(message, enc).expect("under the frame cap");
    HostMessage::from_frame(&frame[4..]).expect("a frame reads back")
}

fn report_patch(name: &str, what: &str, message: &HostMessage, enc: &mut Encoder) -> usize {
    assert_eq!(
        &roundtrip(message, enc),
        message,
        "{name}: reads back equal"
    );
    let len = enc.len();
    let (allocs, alloc_bytes) = per_run(16, || {
        black_box(roundtrip(black_box(message), enc));
    });
    eprintln!(
        "{name}: {len} bytes/message, {allocs} allocations/message ({alloc_bytes} bytes), {what}"
    );
    len
}

fn dev_patch(c: &mut Criterion) {
    let todo = planned(&todo("", false), "todo");
    let counter = planned(COUNTER, "counter");
    let views = vec![
        view_patch(FileId(1), None, &todo, &[]).expect("no shaders to build"),
        view_patch(FileId(2), None, &counter, &[]).expect("no shaders to build"),
    ];
    let icon = vec![0x5au8; 1536];
    let message = bundle(vec![
        PatchSection::Ui(UiPatch { views }),
        PatchSection::Resource(ResourcePatch {
            assets: vec![
                ResourceAsset {
                    path: "icons/check.png".to_owned(),
                    hash: content_hash(&icon),
                    len: icon.len() as u32,
                },
                ResourceAsset {
                    path: "strings/en.toml".to_owned(),
                    hash: content_hash(b"title = \"Todo\""),
                    len: 14,
                },
            ],
        }),
    ]);
    let mut enc = Encoder::new();
    let len = report_patch(
        "ende_dev_patch_roundtrip",
        "two compiled views mounted fresh and two assets, framed and read back",
        &message,
        &mut enc,
    );
    let mut g = c.benchmark_group("ende_dev_patch_roundtrip");
    g.throughput(Throughput::Bytes(len as u64));
    g.bench_function("fresh_views", |b| {
        b.iter(|| roundtrip(black_box(&message), &mut enc))
    });
    g.finish();
}

fn hot_reload_patch(c: &mut Criterion) {
    let last_good = planned(&todo("", false), "todo");
    let candidate = planned(&todo("state filter = \"all\";", true), "todo");
    let view = view_patch(FileId(1), Some(&last_good), &candidate, &[]).expect("no shaders");
    let plan = &view.plan;
    assert!(
        !plan.preserving && !plan.states.is_empty() && !plan.nodes.is_empty(),
        "the edit adds a node and a state: {} structural ops, {} node carries, {} state plans",
        plan.structural.len(),
        plan.nodes.len(),
        plan.states.len()
    );
    let what = format!(
        "one edit of a compiled view ({} structural ops, {} node carries, {} state plans), framed and read back",
        plan.structural.len(),
        plan.nodes.len(),
        plan.states.len()
    );
    let message = bundle(vec![PatchSection::Ui(UiPatch { views: vec![view] })]);
    let mut enc = Encoder::new();
    let len = report_patch("ende_hot_reload_patch_roundtrip", &what, &message, &mut enc);
    let mut g = c.benchmark_group("ende_hot_reload_patch_roundtrip");
    g.throughput(Throughput::Bytes(len as u64));
    g.bench_function("edit", |b| {
        b.iter(|| roundtrip(black_box(&message), &mut enc))
    });
    g.finish();
}

const GAME: &str = r#"
import viso::game::{FixedUpdate, FixedFrame};

export system Idle implements FixedUpdate {
    state ticks = 0;

    action fixed_update(frame: FixedFrame) {
        ticks += 1;
    }
}
"#;

/// Player `local` of a two-player rollback session over the compiled game.
fn peer(local: u32) -> RollbackSession {
    let profile = TargetProfile {
        determinism: Determinism::CrossPlatform,
        ..TargetProfile::default()
    };
    let compiled = compile_file_for(GAME, &origin("main"), Natives::standard(), profile);
    assert!(compiled.errors().next().is_none());
    let module = Rc::new(compiled.behavior.bytecode().expect("verified bytecode"));
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    let game = Scheduler::with_seed(vm, 7).expect("a started game");
    RollbackSession::new(game, SessionConfig::new(2, local)).expect("a session")
}

fn game_packet(c: &mut Criterion) {
    let (mut a, mut b) = (peer(0), peer(1));
    // Past the first hash exchanges: the packets are the steady ones, their
    // hashes at the most they carry.
    for _ in 0..120 {
        a.advance(1);
        b.advance(1);
        b.receive(&a.packet_for(1)).expect("a packet");
        a.receive(&b.packet_for(0)).expect("a packet");
    }
    // One tick on, the packet carries the input the peer has not seen.
    let acked = a.packet_for(1).len();
    a.advance(1);
    b.advance(1);
    let step = |a: &RollbackSession, b: &mut RollbackSession, enc: &mut Encoder| {
        enc.clear();
        a.write_packet_for(1, enc);
        b.receive(enc.as_bytes()).expect("a packet");
    };
    let mut enc = Encoder::new();
    step(&a, &mut b, &mut enc);
    let len = enc.len();
    assert!(len > acked, "the packet carries the new input");
    assert_eq!(enc.as_bytes(), a.packet_for(1));
    // The same packet again, as a lossy transport's duplicate: decoded whole,
    // nothing new in it.
    let before = (b.confirmed_tick(), b.stats(), b.desyncs().len());
    step(&a, &mut b, &mut enc);
    assert_eq!(
        (b.confirmed_tick(), b.stats(), b.desyncs().len()),
        before,
        "a packet taken again leaves the peer as it was"
    );
    assert!(b.confirmed_tick() > 100, "the peers confirm as they go");

    let (allocs, alloc_bytes) = per_run(1000, || step(&a, &mut b, &mut enc));
    eprintln!(
        "ende_game_packet_roundtrip: {len} bytes/packet, {allocs} allocations/packet ({alloc_bytes} bytes), \
         a steady rollback packet (one new input, recent hashes) written into a reused encoder and taken by its peer"
    );
    let mut g = c.benchmark_group("ende_game_packet_roundtrip");
    g.throughput(Throughput::Bytes(len as u64));
    g.bench_function("steady", |bench| {
        bench.iter(|| step(black_box(&a), &mut b, &mut enc))
    });
    g.finish();
}

criterion_group!(
    benches,
    profiler_trace,
    aot_package,
    dev_patch,
    hot_reload_patch,
    game_packet
);
criterion_main!(benches);
