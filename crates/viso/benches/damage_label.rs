//! `damage_single_label_update`: a steady frame of an app where one label's
//! text changes, through the headless frame loop `run` drives.
//!
//! Each tick is a click on a plain text cell whose handler bumps a count,
//! shown by the label beside it. The click's frame runs the handler and
//! changes nothing on screen; the frame it requests flushes the binding,
//! reshapes and re-measures the label, lays out, repaints, uploads and
//! presents the damage to the software raster. Nothing else on the page
//! changes. The time is that of one tick, both frames, read between the
//! submits of consecutive updates in a run of them.
//!
//! Before timing it asserts each tick submits those two frames and only the
//! second uploads, and prints the draws of that frame and the pixels it
//! changed on the surface.
//!
//! Run release (`cargo bench -p viso --features headless --bench damage_label`).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use viso::__test_support::{DrivenApp, drive_scripted, probe_frames};
use viso::platform::{Modifiers, PointerButtons, PointerPhase, RawEvent, RawPointer, WindowId};
use viso::prelude::*;

const STEP: Duration = Duration::from_millis(16);
const FONT: &[u8] = include_bytes!("../fixtures/DejaVuSans-subset.ttf");
/// Ticks run before the timed ones, so the label's glyphs are resident.
const WARM: u64 = 8;

struct Ticker;

impl Application for Ticker {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(FONT);
        Ticker
    }

    /// No caption band, so the view's cells sit at the surface origin.
    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("fixtures/ticker.vs")(cx);
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested {
        window: WindowId(1),
    }
}

/// The app launched, then `ticks` frames that each click the tick cell.
fn drive(ticks: u64) -> DrivenApp<Ticker> {
    let mut script = vec![redraw()];
    for _ in 0..ticks {
        for (buttons, phase) in [
            (PointerButtons::PRIMARY, PointerPhase::Down),
            (PointerButtons::default(), PointerPhase::Up),
        ] {
            script.push(RawEvent::Pointer(RawPointer::mouse(
                WindowId(1),
                50.0,
                20.0,
                buttons,
                Modifiers::default(),
                phase,
            )));
        }
        script.push(redraw());
    }
    drive_scripted::<Ticker>(script, STEP)
}

/// What a frame probe saw: when each frame was submitted, and the renderer
/// counters and whether it uploaded.
type Seen = Rc<RefCell<Vec<(Instant, viso::render::FrameStats, bool)>>>;

fn probed(
    ticks: u64,
) -> (
    DrivenApp<Ticker>,
    Vec<(Instant, viso::render::FrameStats, bool)>,
) {
    let seen: Seen = Rc::new(RefCell::new(Vec::with_capacity(ticks as usize + 2)));
    let sink = Rc::clone(&seen);
    probe_frames(Some(Box::new(move |stats, uploaded| {
        sink.borrow_mut().push((Instant::now(), *stats, uploaded));
    })));
    let app = drive(ticks);
    probe_frames(None);
    let seen = seen.take();
    (app, seen)
}

/// The box `[x0, y0, x1, y1)` of the pixels that differ between two frames
/// of the same surface, and how many.
fn changed(a: &[u8], b: &[u8], w: u32) -> Option<([u32; 4], usize)> {
    let mut bounds: Option<[u32; 4]> = None;
    let mut count = 0;
    for (i, (pa, pb)) in a
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0)
        .enumerate()
    {
        if pa == pb {
            continue;
        }
        count += 1;
        let (x, y) = (i as u32 % w, i as u32 / w);
        let r = bounds.get_or_insert([x, y, x + 1, y + 1]);
        *r = [r[0].min(x), r[1].min(y), r[2].max(x + 1), r[3].max(y + 1)];
    }
    bounds.map(|r| (r, count))
}

fn check() {
    let (before, _) = probed(WARM);
    let (after, seen) = probed(WARM + 1);
    assert_eq!(
        seen.len() as u64,
        2 * (WARM + 1) + 1,
        "the launch and two frames a tick are submitted"
    );
    assert!(
        seen[1..]
            .iter()
            .enumerate()
            .all(|(i, &(_, _, uploaded))| uploaded == (i % 2 == 1)),
        "a tick's update uploads, its click does not"
    );
    let (w, h, a) = before.pixels().expect("the window has a surface");
    let (_, _, b) = after.pixels().expect("the window has a surface");
    let ([x0, y0, x1, y1], count) = changed(&a, &b, w).expect("the tick changes the label");
    let area = (x1 - x0) * (y1 - y0);
    assert!(
        area * 20 < w * h,
        "the changed box {x0},{y0}..{x1},{y1} is most of the {w}x{h} surface"
    );
    let stats = seen.last().unwrap().1;
    eprintln!(
        "damage_single_label_update: {} nodes; the update frame draws {} draws of {} \
         instances, {} dirty primitives, {} upload bytes in {} ranges; it changes {count} \
         pixels in {x0},{y0}..{x1},{y1} ({area} of {} px)",
        after.store().arena().live_count(),
        stats.draw_calls,
        stats.instances,
        stats.dirty_primitives,
        stats.gpu_upload_bytes,
        stats.uploaded_ranges,
        w * h,
    );
}

fn bench_damage_label(c: &mut Criterion) {
    check();
    c.bench_function("damage_single_label_update", |b| {
        b.iter_custom(|iters| {
            let (app, seen) = probed(WARM + iters);
            black_box(app);
            // From the update of the last warm tick to that of the last.
            assert_eq!(seen.len() as u64, 2 * (WARM + iters) + 1);
            seen.last().unwrap().0 - seen[2 * WARM as usize].0
        });
    });
}

criterion_group!(benches, bench_damage_label);
criterion_main!(benches);
