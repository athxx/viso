//! A popover driven through the facade: a click on its anchor opens it below
//! the anchor with its arrow at the anchor's middle, the focus goes into it,
//! a press outside closes it and Escape does too, giving the focus back.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{
    Axis, BoxStyle, FlexStyle, Inset, LeafStyle, NodeId, PointerButtons, PointerPhase, Rect, Side,
    Sizing,
};
use viso::widgets::{PopupHandle, popover};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

/// An 80 by 30 anchor at 100, 100 opening a popover of a 120 by 60 box.
struct PopApp;

impl Application for PopApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        PopApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let popup: PopupHandle = cx.handle();
        cx.flex(
            FlexStyle {
                size: Sizing::fill(),
                axis: Axis::Column,
                padding: Inset {
                    left: 100.0,
                    top: 100.0,
                    ..Inset::default()
                },
                ..FlexStyle::default()
            },
            |cx| {
                popover()
                    .anchor(move |cx| {
                        let button = cx.leaf(LeafStyle {
                            size: Sizing::fixed(80.0, 30.0),
                            style: BoxStyle::NONE,
                        });
                        cx.focusable(button, true);
                        cx.on_pointer(button, move |ev| {
                            let up = ev.pointer().is_some_and(|p| {
                                p.phase == PointerPhase::Up
                                    && p.buttons.contains(PointerButtons::PRIMARY)
                            });
                            if up {
                                popup.toggle(ev);
                            }
                        });
                    })
                    .content(|cx| {
                        cx.leaf(LeafStyle {
                            size: Sizing::fixed(120.0, 60.0),
                            style: BoxStyle::NONE,
                        });
                    })
                    .handle(popup)
                    .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f32, y: f32, phase: RawPhase) -> RawEvent {
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        f64::from(x),
        f64::from(y),
        RawButtons::PRIMARY,
        RawModifiers::default(),
        phase,
    ))
}

fn click(x: f32, y: f32) -> [RawEvent; 2] {
    [mouse(x, y, RawPhase::Down), mouse(x, y, RawPhase::Up)]
}

fn escape() -> [RawEvent; 2] {
    let raw = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code: Key::Escape,
            pressed,
            repeat: false,
            modifiers: RawModifiers::default(),
        })
    };
    [raw(true), raw(false)]
}

/// The popup's anchor box and its content.
fn parts(app: &DrivenApp<PopApp>) -> (NodeId, NodeId) {
    let store = app.store();
    let popup = store.arena().children(app.root().unwrap()).next().unwrap();
    let mut kids = store.arena().children(popup);
    (kids.next().unwrap(), kids.next().unwrap())
}

fn opened() -> Vec<RawEvent> {
    let mut script = vec![redraw(), redraw()];
    script.extend(click(140.0, 115.0));
    script.push(redraw());
    script
}

#[test]
fn a_click_on_the_anchor_opens_the_popover_below_it_with_the_focus_inside() {
    let app = drive_scripted::<PopApp>(opened(), STEP);
    let store = app.store();
    let (anchor, content) = parts(&app);
    assert!(!store.hidden(content), "open");
    let a = store.world(anchor);
    assert_eq!(a, Rect::new(100.0, 100.0, 80.0, 30.0));
    let c = store.world(content);
    assert_eq!((c.w, c.h), (144.0, 84.0), "the box and its padding");
    assert_eq!(c.y, 130.0 + 9.0, "below the anchor, the arrow's room away");
    assert_eq!(c.x, 140.0 - 72.0, "centred on it");
    let placed = store.anchor_placement(content).expect("placed");
    assert_eq!(placed.side, Side::Bottom);
    assert_eq!(placed.arrow, 72.0, "the arrow at the anchor's middle");
    assert_eq!(store.focused(), Some(content), "the focus went in");
    let holder = store.arena().children(content).next().unwrap();
    let arrow = store.world(store.arena().children(holder).next().unwrap());
    let middle = (arrow.x + arrow.w / 2.0, arrow.y + arrow.h / 2.0);
    assert!(
        (middle.0 - 140.0).abs() < 0.01,
        "the arrow points at the anchor: {arrow:?}"
    );
    assert!(
        (middle.1 - c.y).abs() < 0.01,
        "on the edge facing it: {arrow:?}"
    );
}

#[test]
fn a_press_outside_closes_it_and_one_on_the_anchor_toggles_it() {
    let mut script = opened();
    script.extend(click(400.0, 400.0));
    script.push(redraw());
    let app = drive_scripted::<PopApp>(script, STEP);
    let (_, content) = parts(&app);
    assert!(app.store().hidden(content), "a press outside closes it");

    let mut script = opened();
    script.extend(click(140.0, 115.0));
    script.push(redraw());
    let app = drive_scripted::<PopApp>(script, STEP);
    let (_, content) = parts(&app);
    assert!(
        app.store().hidden(content),
        "the anchor's own click closes it"
    );
}

#[test]
fn escape_closes_it_and_gives_the_focus_back() {
    let mut script = opened();
    script.extend(escape());
    script.push(redraw());
    let app = drive_scripted::<PopApp>(script, STEP);
    let store = app.store();
    let (anchor, content) = parts(&app);
    assert!(store.hidden(content));
    let button = store.arena().children(anchor).next().unwrap();
    assert_eq!(store.focused(), Some(button), "back on the anchor");
}
