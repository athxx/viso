//! Facade integration pack for the transform-only animation clock —
//! the frame-loop wiring the Sheet control (and every future slide/settle) rides
//! on. Unlike the widget packs, which drive a `NodeStore` by hand, this pack
//! drives the *real* [`AppDriver`] frame loop end to end through a scripted
//! headless pump with a self-stepping deterministic clock, so it exercises the
//! same path `viso::run` takes:
//!
//! - **a translate animation runs its course through the loop** — a scripted
//!   pointer press asks a handler to animate a node's translate; after the
//!   pump drains, the node's watcher has heard the animation end finished at
//!   its target, and the node shows its model translate again;
//! - **the loop halts when the slide settles** — `wants_animation` is false once
//!   the animation ends, so the driver stops self-rescheduling beats (the
//!   zero-CPU-when-idle contract; a stuck-open animation would burn 100% CPU);
//! - **a pure TRANSFORM frame moves world through the facade loop** — an
//!   animation frame marks only TRANSFORM (never MEASURE/LAYOUT), yet world
//!   moves mid-slide. World is derived only by `resolve_transforms`, and the
//!   only loop path that runs it on a layout-clean frame is the
//!   TRANSFORM-gated call in `relayout_and_paint`, so a moved world with no
//!   relayout (`laid_out == 0`) is the proof that call fired.
//!
//! - **a view's look transition plays through the loop** — a click on a
//!   `component!` node whose `opacity` has a `transition.opacity` moves the
//!   opacity in over the following frames, and the loop halts once it arrives.
//!
//! The clock is a `FixedStepClock` (injected via the hidden `__test_support`
//! seam): the pump loops internally while `wants_animation` holds, so the test
//! cannot advance a manual cursor between beats — a fixed step per frame drives
//! the slide to completion at a known rate.

use std::cell::Cell;
use std::time::Duration;

use viso::__test_support::{drive_scripted, drive_scripted_frames};
use viso::platform::{
    Modifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent, RawPointer,
    WindowId,
};
use viso::prelude::*;
use viso::ui::{
    BuildCx, Easing, LeafStyle, LookValue, Notice, NoticeKinds, PointerButtons, PointerPhase,
    Sizing, Timing, Vec2, WindowConfig,
};

/// The surface a launch window opens at (`WindowConfig::default` logical size,
/// scale 1.0). A center-of-surface pointer sample lands on the fill root.
const SURFACE_W: f32 = 800.0;
const SURFACE_H: f32 = 600.0;

/// How far the node slides, in world-space pixels (upward: `to.y` negative).
const SLIDE: f32 = 100.0;

/// A fixed-step deterministic frame delta. The slide runs `SLIDE_MS`, so a
/// handful of `STEP`-sized frames carries it past completion.
const STEP: Duration = Duration::from_millis(16);
const SLIDE_MS: u64 = 80;

thread_local! {
    /// How the slide ended, as the node's watcher heard it: its target's
    /// `y` and whether it ran its course.
    static ENDED: Cell<Option<(f32, bool)>> = const { Cell::new(None) };
}

/// An app whose root is a surface-filling leaf. A primary pointer-down on it
/// animates its translate from rest to `(0, -SLIDE)` through
/// `EventCx::animate`. Nothing else
/// moves, so the whole tree is layout-clean once built and every animation
/// frame is a pure TRANSFORM frame.
struct SlideApp;

impl Application for SlideApp {
    fn new(_cx: &mut AppCx) -> Self {
        SlideApp
    }

    /// Opt out of the default caption wrap: this pack exercises the animation
    /// frame loop against the app's own fill root, so it wants that root *as* the
    /// window root (unwrapped), not nested under a caption band. The wrap itself
    /// is covered by `caption_default_wrap.rs`.
    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let root = cx.leaf(LeafStyle {
            size: Sizing::fill(),
            ..LeafStyle::default()
        });
        cx.focusable(root, true);
        let node = root.id();
        cx.watch(
            root,
            NoticeKinds::ANIMATION_END,
            Box::new(|notice, _| {
                if let Notice::AnimationEnd {
                    target: LookValue::Translate(to),
                    finished,
                } = notice
                {
                    ENDED.set(Some((to.y, *finished)));
                }
            }),
        );
        cx.on_pointer(root, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if p.phase == PointerPhase::Down && p.buttons.contains(PointerButtons::PRIMARY) {
                ev.animate(
                    node,
                    LookValue::Translate(Vec2 { x: 0.0, y: -SLIDE }),
                    Timing::eased(Duration::from_millis(SLIDE_MS), Easing::EASE_OUT),
                );
            }
        });
    }
}

/// One priming redraw to run the first layout (so the fill root gets a world
/// box the pointer can hit — `on_launch` only builds and marks dirty; the
/// initial measure/layout runs on the first frame), then a primary pointer-down
/// at the center of the surface, then a beat to run the frame that starts the
/// slide. Once the handler starts the slide the driver `wants_animation` and
/// self-reschedules its own beats until the slide ends, so these priming
/// beats are enough — the loop drives the rest of the slide itself.
fn slide_script() -> Vec<RawEvent> {
    vec![
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
        RawEvent::Pointer(RawPointer::mouse(
            WindowId(1),
            SURFACE_W as f64 / 2.0,
            SURFACE_H as f64 / 2.0,
            RawButtons::PRIMARY,
            Modifiers::default(),
            RawPhase::Down,
        )),
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
    ]
}

#[test]
fn a_slide_runs_its_course_through_the_facade_loop() {
    ENDED.set(None);
    let app = drive_scripted::<SlideApp>(slide_script(), STEP);
    let root = app.root().expect("SlideApp declares a root");
    let store = app.store();

    assert_eq!(
        ENDED.get(),
        Some((-SLIDE, true)),
        "the watcher heard the slide end finished at its target"
    );
    // Once it ends the node shows its model translate (none) again.
    let (bounds, world) = (store.bounds(root), store.world(root));
    assert!(
        bounds.y.abs() < 0.5 && (world.y - bounds.y).abs() < 0.5,
        "the fill root rests at the surface origin again: bounds.y {}, world.y {}",
        bounds.y,
        world.y
    );
    assert!(
        !app.wants_animation(),
        "the loop halts once the slide settles (zero-CPU-when-idle)"
    );
}

#[test]
fn an_animation_frame_moves_world_without_relayout() {
    // The tree is layout-clean after build (a fill leaf), and the slide marks
    // only TRANSFORM|HIT_TEST|PAINT — never MEASURE/LAYOUT. World is produced
    // solely by `resolve_transforms`, and on a layout-clean frame the only loop
    // path that runs it is the TRANSFORM-gated call in `relayout_and_paint`. So
    // if world moved mid-slide with nothing relaid, that call fired.
    // Three frames: the priming layout, the press, and the slide's first tick.
    let app = drive_scripted_frames::<SlideApp>(slide_script(), STEP, 3);
    let root = app.root().expect("SlideApp declares a root");
    let store = app.store();
    assert!(app.wants_animation(), "stopped mid-slide");
    assert_eq!(
        app.recompute().laid_out,
        0,
        "the animation frame relaid no node (pure TRANSFORM frame)"
    );
    let moved = store.bounds(root).y - store.world(root).y;
    assert!(
        moved > 1.0 && moved < SLIDE,
        "world is part way up the slide on a layout-clean frame (moved {moved})"
    );
}

viso::component! {
    Fade {
        state lit = false;
        view {
            Column {
                width: 800dp;
                height: 600dp;
                opacity: if lit { 1.0f32 } else { 0.2f32 };
                transition.opacity: Transition { duration: 80ms, easing: Easing::linear };
                on click { lit = true; }
            }
        }
    }
}

/// An app showing one `Fade` as its window root.
struct FadeApp;

impl Application for FadeApp {
    fn new(_cx: &mut AppCx) -> Self {
        FadeApp
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let build = viso::ui! { Fade {} };
        build(cx);
    }
}

#[test]
fn a_look_transition_plays_through_the_facade_loop() {
    let (x, y) = (SURFACE_W as f64 / 2.0, SURFACE_H as f64 / 2.0);
    let press = |phase| {
        RawEvent::Pointer(RawPointer::mouse(
            WindowId(1),
            x,
            y,
            RawButtons::PRIMARY,
            Modifiers::default(),
            phase,
        ))
    };
    let redraw = RawEvent::RedrawRequested {
        window: WindowId(1),
    };
    let app = drive_scripted::<FadeApp>(
        vec![
            redraw.clone(),
            press(RawPhase::Down),
            press(RawPhase::Up),
            redraw,
        ],
        STEP,
    );
    let store = app.store();
    let mut opacities = Vec::new();
    let mut pending = vec![app.root().expect("FadeApp declares a root")];
    while let Some(node) = pending.pop() {
        opacities.push(store.opacity(node));
        for c in store.arena().children(node) {
            pending.push(c);
        }
    }
    assert!(
        opacities.iter().all(|&o| o == 1.0),
        "the opacity arrived at its target through the loop: {opacities:?}"
    );
    assert!(!store.is_transitioning());
    assert!(
        !app.wants_animation(),
        "the loop halts once the look settles (zero-CPU-when-idle)"
    );
}
