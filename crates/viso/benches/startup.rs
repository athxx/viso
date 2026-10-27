//! From nothing to the first presented frame, through the headless frame loop
//! `run` drives: the application built, its window opened and its tree built,
//! text shaped against the app's font, laid out, painted, uploaded and
//! submitted to the software raster. The pump stops after that frame.
//!
//! - `startup_minimal`: one label.
//! - `startup_complex_app`: a `view!` of a toolbar (text, a text input, a
//!   toggle, a slider, a button) over a scrolled list of 40 rows of a user
//!   component (a check box, a label and a button each).
//!
//! Each case asserts once that a frame reached the surface and prints what it
//! drew. Teardown is not timed.
//!
//! Run release (`cargo bench -p viso --features headless --bench startup`).

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use viso::__test_support::{DrivenApp, drive_scripted_frames, probe_frames};
use viso::prelude::*;

const STEP: Duration = Duration::from_millis(16);
const FONT: &[u8] = include_bytes!("../fixtures/DejaVuSans-subset.ttf");

struct Minimal;

impl Application for Minimal {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(FONT);
        Minimal
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        label("Hello, world").build(cx);
    }
}

struct Complex;

impl Application for Complex {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(FONT);
        Complex
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("fixtures/complex_app.vs")(cx);
    }
}

/// The pump bounded to one beat: the launch window's first redraw.
fn start<A: Application>() -> DrivenApp<A> {
    drive_scripted_frames::<A>(Vec::new(), STEP, 1)
}

/// Starts `A` once with a probe on its frames: exactly one is submitted,
/// with draws, to pixels that are not all the clear color.
fn check<A: Application>(name: &str) {
    let submits = Rc::new(Cell::new(0u32));
    let draws = Rc::new(Cell::new((0usize, 0usize)));
    let (s, d) = (Rc::clone(&submits), Rc::clone(&draws));
    probe_frames(Some(Box::new(move |stats, _| {
        if s.get() == 0 {
            d.set((stats.draw_calls, stats.instances));
        }
        s.set(s.get() + 1);
    })));
    let app = start::<A>();
    probe_frames(None);
    let (draw_calls, instances) = draws.get();
    assert_eq!(submits.get(), 1, "{name}: one frame is submitted");
    assert!(draw_calls > 0, "{name}: the first frame draws nothing");
    let (w, h, pixels) = app.pixels().expect("the window has a surface");
    let (px, _) = pixels.as_chunks::<4>();
    assert!(
        px.iter().any(|p| p != &px[0]),
        "{name}: the surface is one color"
    );
    eprintln!(
        "{name}: {} nodes, the first frame in {draw_calls} draws of {instances} instances \
         to {w}x{h}",
        app.store().arena().live_count(),
    );
}

fn bench_startup(c: &mut Criterion) {
    check::<Minimal>("startup_minimal");
    c.bench_function("startup_minimal", |b| {
        b.iter_with_large_drop(|| black_box(start::<Minimal>()))
    });
    check::<Complex>("startup_complex_app");
    c.bench_function("startup_complex_app", |b| {
        b.iter_with_large_drop(|| black_box(start::<Complex>()))
    });
}

criterion_group!(benches, bench_startup);
criterion_main!(benches);
