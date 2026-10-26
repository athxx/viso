//! The reference app of `cargo xtask footprint`.
//!
//! `counter` is a label and a button that increments the count it shows;
//! `list` adds a 10 000-row virtual list under them, which the headless
//! scenario scrolls. The scene is the first argument (`counter` when none).
//!
//! The heap is counted ([`viso::alloc::Counting`]). With
//! `VISO_FOOTPRINT_CONTROL` set, a thread reads commands on stdin while the
//! app runs: `heap` writes `footprint: heap live=<bytes> peak=<bytes>` to
//! stderr, and the end of stdin ends the process. The thread blocks on the
//! read, so it adds no wakeup to an idle app.

use std::io::BufRead;

use viso::prelude::*;
use viso::ui::{Axis, Component, Length, Sizing};

#[global_allocator]
static ALLOCATOR: viso::alloc::Counting = viso::alloc::Counting::system();

static FONTS: viso::fonts::PackagedFonts = viso::packaged_fonts!("fonts");

/// The rows of the `list` scene.
const ROWS: usize = 10_000;

/// The height of one row, in logical pixels.
const ROW_HEIGHT: f32 = 24.0;

/// The list's viewport: the window's height less the caption and the counter.
const LIST_HEIGHT: f32 = 460.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scene {
    Counter,
    List,
}

impl Scene {
    fn from_args() -> Scene {
        match std::env::args().nth(1).as_deref() {
            Some("list") => Scene::List,
            _ => Scene::Counter,
        }
    }

    /// The window: the counter's is the size of the Makepad counter's.
    fn size(self) -> (f64, f64) {
        match self {
            Scene::Counter => (420.0, 220.0),
            Scene::List => (420.0, 600.0),
        }
    }
}

struct Footprint {
    scene: Scene,
}

impl Application for Footprint {
    fn new(cx: &mut AppCx) -> Self {
        cx.use_packaged_fonts(FONTS);
        Footprint {
            scene: Scene::from_args(),
        }
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            title: "footprint".into(),
            size: self.scene.size(),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let scene = self.scene;
        cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: 12.0,
                size: Sizing::fill(),
                ..Default::default()
            },
            move |cx| {
                viso::view!("counter.vs")(cx);
                if scene == Scene::List {
                    virtual_list(VirtualListViewStyle {
                        axis: Axis::Column,
                        size: Sizing::fixed(420.0, LIST_HEIGHT),
                        estimated_row: ROW_HEIGHT,
                        ..Default::default()
                    })
                    .items(ROWS, |index, cx| {
                        label(format!("Row {index}"))
                            .size(Sizing {
                                width: Length::fill(),
                                height: Length::Fixed(ROW_HEIGHT),
                            })
                            .build(cx);
                    })
                    .label("Rows")
                    .build(cx);
                }
            },
        );
    }
}

/// Answers the measuring tool on stdin, when it asks to be answered.
fn listen() {
    if std::env::var_os("VISO_FOOTPRINT_CONTROL").is_none() {
        return;
    }
    std::thread::spawn(|| {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim() == "heap" {
                let heap = viso::alloc::heap().unwrap_or(viso::alloc::Heap { live: 0, peak: 0 });
                eprintln!("footprint: heap live={} peak={}", heap.live, heap.peak);
            }
        }
        std::process::exit(0);
    });
}

fn main() {
    listen();
    viso::run::<Footprint>();
}
