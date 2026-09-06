//! Section 66 facade integration pack for the transform-only animation clock —
//! the frame-loop wiring the Sheet control (and every future slide/settle) rides
//! on. Unlike the widget packs, which drive a `NodeStore` by hand, this pack
//! drives the *real* [`AppDriver`] frame loop end to end through a scripted
//! headless pump with a self-stepping deterministic clock, so it exercises the
//! same path `viso::run` takes:
//!
//! - **a translate animation reaches its target through the loop** — a scripted
//!   pointer press asks a handler to start a slide; after the pump drains, the
//!   node's world rect has moved by the full translate (sign rule
//!   `world = bounds − translate`), proving the animation ticked to completion;
//! - **the loop halts when the slide settles** — `wants_animation` is false once
//!   the registry empties, so the driver stops self-rescheduling beats (the
//!   zero-CPU-when-idle contract; a stuck-open animation would burn 100% CPU);
//! - **a pure TRANSFORM frame moves world through the facade loop** — the
//!   section-C gap regression: an animation frame marks only TRANSFORM (never
//!   MEASURE/LAYOUT), yet the settled world moved. World is derived only by
//!   `resolve_transforms`, and the only loop path that runs it on a
//!   layout-clean frame is the TRANSFORM-gated call in `relayout_and_paint`, so
//!   a moved world with no relayout is the proof that call fired. The last
//!   animation frame's `recompute` also confirms `laid_out == 0` while world
//!   still moved.
//!
//! The clock is a `FixedStepClock` (injected via the hidden `__test_support`
//! seam): the pump loops internally while `wants_animation` holds, so the test
//! cannot advance a manual cursor between beats — a fixed step per frame drives
//! the slide to completion at a known rate.

use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{
    Modifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent, RawPointer,
    WindowId,
};
use viso::prelude::*;
use viso::ui::{
    BuildCx, Easing, LeafStyle, PointerButtons, PointerPhase, Size, TranslateAnim, Vec2,
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

/// An app whose root is a surface-filling leaf. A primary pointer-down on it
/// requests a translate slide from rest to `(0, -SLIDE)` — the same
/// `EventCx::request_animation` seam the Sheet control will use. Nothing else
/// moves, so the whole tree is layout-clean once built and every animation
/// frame is a pure TRANSFORM frame.
struct SlideApp;

impl Application for SlideApp {
    fn new(_cx: &mut AppCx) -> Self {
        SlideApp
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let root = cx.leaf(LeafStyle {
            size: Size::fill(),
            ..LeafStyle::default()
        });
        cx.focusable(root, true);
        let node = root.id();
        cx.on_pointer(root, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if p.phase == PointerPhase::Down && p.buttons.contains(PointerButtons::PRIMARY) {
                ev.request_animation(TranslateAnim::new(
                    node,
                    Vec2::ZERO,
                    Vec2 { x: 0.0, y: -SLIDE },
                    Duration::from_millis(SLIDE_MS),
                    Easing::EaseOut,
                ));
            }
        });
    }
}

/// One priming redraw to run the first layout (so the fill root gets a world
/// box the pointer can hit — `on_launch` only builds and marks dirty; the
/// initial measure/layout runs on the first frame), then a primary pointer-down
/// at the center of the surface, then a beat to run the frame that starts the
/// slide. Once the handler starts the slide the driver `wants_animation` and
/// self-reschedules its own beats until the registry empties, so these priming
/// beats are enough — the loop drives the rest of the slide itself.
fn slide_script() -> Vec<RawEvent> {
    vec![
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
        RawEvent::Pointer(RawPointer {
            window: WindowId(1),
            x: SURFACE_W as f64 / 2.0,
            y: SURFACE_H as f64 / 2.0,
            buttons: RawButtons::PRIMARY,
            modifiers: Modifiers::default(),
            phase: RawPhase::Down,
        }),
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
    ]
}

#[test]
fn slide_reaches_target_through_the_facade_loop() {
    let app = drive_scripted::<SlideApp>(slide_script(), STEP);
    let root = app.root().expect("SlideApp declares a root");
    let store = app.store();

    // The root fills the surface, so its resting bounds start at the origin.
    let bounds = store.bounds(root);
    assert!(
        bounds.y.abs() < 0.5,
        "the fill root rests at the surface origin (y ~= 0), got {}",
        bounds.y
    );

    // The slide translated the node up by SLIDE. World = bounds − translate, so
    // an upward translate (`to.y = -SLIDE`) moves world *down* by SLIDE relative
    // to bounds: world.y = bounds.y − (−SLIDE) = bounds.y + SLIDE. Reaching it
    // proves the animation ticked to t=1 through the loop.
    let world = store.world(root);
    assert!(
        (world.y - (bounds.y + SLIDE)).abs() < 0.5,
        "the settled world moved by the full slide: expected world.y ~= {}, got {}",
        bounds.y + SLIDE,
        world.y
    );

    // The registry emptied when the slide finished, so the driver no longer
    // wants animation frames — the loop is free to idle (frame halt).
    assert!(
        !app.wants_animation(),
        "the loop halts once the slide settles (zero-CPU-when-idle)"
    );
}

#[test]
fn animation_frame_moves_world_without_relayout() {
    // Section-C gap regression. The tree is layout-clean after build (a fill
    // leaf), and the slide marks only TRANSFORM|HIT_TEST|PAINT — never
    // MEASURE/LAYOUT. World is produced solely by `resolve_transforms`, and on a
    // layout-clean frame the only loop path that runs it is the TRANSFORM-gated
    // call in `relayout_and_paint`. So if world moved, that call fired.
    let app = drive_scripted::<SlideApp>(slide_script(), STEP);
    let root = app.root().expect("SlideApp declares a root");
    let store = app.store();

    // World moved (proved in the sibling test) — here we additionally assert the
    // final frame relaid *nothing*: the animation advanced world with zero layout
    // work, which is exactly the section 8.7 transform/layout split.
    let recompute = app.recompute();
    assert_eq!(
        recompute.laid_out, 0,
        "the last animation frame relaid no node (pure TRANSFORM frame)"
    );

    // And world did move off its bounds, so the transform-only frame was not a
    // no-op: paint/hit-test read world, and world advanced without a relayout.
    let bounds = store.bounds(root);
    let world = store.world(root);
    assert!(
        (world.y - bounds.y).abs() > 1.0,
        "world advanced away from bounds on a layout-clean frame (moved by ~{})",
        world.y - bounds.y
    );
}
