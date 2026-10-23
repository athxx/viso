//! Drag and drop driven through the facade: a mouse carries a card to the
//! zone that takes cards, one let go over nothing is put back, a finger lifts
//! it by a long press, and the keyboard carries it too.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode, Modifiers, PointerButtons, PointerId, PointerKind, PointerPhase, RawEvent, RawKey,
    RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{
    Axis, BoxStyle, Component, DragAccept, DragData, DragKind, DropOperation, DropPhase, FlexStyle,
    LeafStyle, Size, WindowConfig,
};
use viso::widgets::{DropZoneStyle, draggable, drop_zone};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

#[derive(Debug, PartialEq)]
struct Card(u32);

thread_local! {
    /// What was heard: a zone's phase by its number, a card's end by `9`.
    static HEARD: RefCell<Vec<(u32, String)>> = const { RefCell::new(Vec::new()) };
}

fn hear(who: u32, what: String) {
    HEARD.with(|h| h.borrow_mut().push((who, what)));
}

fn take_heard() -> Vec<(u32, String)> {
    HEARD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

/// A 300×300 column: a 100×40 card carrying `Card(7)` and its text, a
/// 300×100 zone taking cards, then one taking links.
struct Board;

impl Application for Board {
    fn new(_cx: &mut AppCx) -> Self {
        Board
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            size: (300.0, 300.0),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(
            FlexStyle {
                axis: Axis::Column,
                size: Size::fixed(300.0, 300.0),
                ..FlexStyle::default()
            },
            |cx| {
                draggable(DragData::value(Card(7)).with(viso::ui::DragItem::Text("Card 7".into())))
                    .content(|cx| {
                        cx.leaf(LeafStyle {
                            size: Size::fixed(100.0, 40.0),
                            style: BoxStyle::NONE,
                        });
                    })
                    .label("Card 7")
                    .on_end(|_, end| hear(9, format!("{:?}", end.operation)))
                    .build(cx);
                for (n, accept) in [
                    (1, DragAccept::Value(DragKind::of::<Card>())),
                    (2, DragAccept::Uri),
                ] {
                    drop_zone([accept])
                        .style(DropZoneStyle {
                            size: Size::fixed(300.0, 100.0),
                            ..DropZoneStyle::default()
                        })
                        .operation(DropOperation::Copy)
                        .on_enter(move |_, _| hear(n, "enter".into()))
                        .on_leave(move |_, _| hear(n, "leave".into()))
                        .on_drop(move |_, drop| {
                            assert_eq!(drop.phase, DropPhase::Drop);
                            hear(n, format!("drop {:?}", drop.data.get::<Card>()));
                        })
                        .build(cx);
                }
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn pointer(kind: PointerKind, x: f64, y: f64, down: bool, phase: PointerPhase) -> RawEvent {
    let buttons = if down {
        PointerButtons::PRIMARY
    } else {
        PointerButtons::NONE
    };
    RawEvent::Pointer(RawPointer {
        pointer: if kind == PointerKind::Mouse {
            PointerId::MOUSE
        } else {
            PointerId(5)
        },
        kind,
        ..RawPointer::mouse(WIN, x, y, buttons, Modifiers::default(), phase)
    })
}

/// A press at `from` carried to `to` in ten steps, held `hold` frames
/// first, then lifted. A waiting timer sets no redraw reason, so each held
/// frame wakes the loop first.
fn carry(kind: PointerKind, from: (f64, f64), to: (f64, f64), hold: usize) -> Vec<RawEvent> {
    let mut out = vec![
        pointer(kind, from.0, from.1, false, PointerPhase::Moved),
        pointer(kind, from.0, from.1, true, PointerPhase::Down),
    ];
    for _ in 0..hold {
        out.extend([RawEvent::Wakeup, redraw()]);
    }
    for k in 1..=10 {
        let t = f64::from(k) / 10.0;
        let (x, y) = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
        out.push(pointer(kind, x, y, true, PointerPhase::Moved));
        out.push(redraw());
    }
    out.push(pointer(kind, to.0, to.1, false, PointerPhase::Up));
    out.push(redraw());
    out
}

fn key(code: KeyCode) -> Vec<RawEvent> {
    let at = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers: Modifiers::default(),
        })
    };
    vec![at(true), at(false), redraw()]
}

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<Board> {
    take_heard();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
        script.extend(std::iter::repeat_n(redraw(), 60));
    }
    drive_scripted::<Board>(script, STEP)
}

fn heard() -> Vec<(u32, String)> {
    take_heard()
}

#[test]
fn a_mouse_carries_a_card_to_the_zone_that_takes_cards() {
    let _ = run(&[carry(PointerKind::Mouse, (50.0, 20.0), (150.0, 220.0), 0)]);
    assert_eq!(
        heard(),
        [
            (1, "enter".to_owned()),
            (1, "leave".to_owned()),
            (9, "None".to_owned()),
        ],
        "past the card zone, over one taking no card, it is put back"
    );
    let _ = run(&[carry(PointerKind::Mouse, (50.0, 20.0), (150.0, 100.0), 0)]);
    assert_eq!(
        heard(),
        [
            (1, "enter".to_owned()),
            (1, "drop Some(Card(7))".to_owned()),
            (9, "Some(Copy)".to_owned()),
        ]
    );
}

#[test]
fn let_go_over_nothing_the_card_is_put_back() {
    let app = run(&[carry(PointerKind::Mouse, (50.0, 20.0), (250.0, 20.0), 0)]);
    assert_eq!(heard(), [(9, "None".to_owned())]);
    let store = app.store();
    assert!(!store.drag_drop_running());
}

#[test]
fn a_finger_lifts_the_card_by_holding_it() {
    let _ = run(&[carry(PointerKind::Touch, (50.0, 20.0), (150.0, 100.0), 40)]);
    let heard = heard();
    assert!(
        heard.contains(&(1, "drop Some(Card(7))".to_owned())),
        "a long press lifted it: {heard:?}"
    );
    assert_eq!(heard.last(), Some(&(9, "Some(Copy)".to_owned())));
}

#[test]
fn a_brisk_finger_does_not_lift_it() {
    let _ = run(&[carry(PointerKind::Touch, (50.0, 20.0), (150.0, 100.0), 0)]);
    assert_eq!(heard(), []);
}

#[test]
fn the_keyboard_carries_the_card() {
    let _ = run(&[key(KeyCode::Tab), key(KeyCode::Space), key(KeyCode::Enter)]);
    assert_eq!(
        heard(),
        [
            (1, "enter".to_owned()),
            (1, "drop Some(Card(7))".to_owned()),
            (9, "Some(Copy)".to_owned()),
        ]
    );
}

#[test]
fn escape_puts_the_keyboard_s_card_back() {
    let _ = run(&[key(KeyCode::Tab), key(KeyCode::Space), key(KeyCode::Escape)]);
    assert_eq!(
        heard(),
        [
            (1, "enter".to_owned()),
            (1, "leave".to_owned()),
            (9, "None".to_owned()),
        ]
    );
}

fn outside(
    phase: viso::platform::DropPhase,
    x: f64,
    y: f64,
    reply: &viso::platform::DropReply,
) -> RawEvent {
    use viso::platform::{DropKinds, DropOffer, RawDrop};
    let dropped = phase == viso::platform::DropPhase::Drop;
    RawEvent::Drop(RawDrop {
        window: WIN,
        phase,
        x,
        y,
        offer: DropOffer {
            kinds: DropKinds {
                text: true,
                uri: true,
                files: false,
            },
            text: dropped.then(|| "https://example.org".to_owned()),
            uri: dropped.then(|| "https://example.org".to_owned()),
            files: Vec::new(),
        },
        reply: reply.clone(),
    })
}

#[test]
fn a_link_dragged_in_from_outside_drops_on_the_zone_taking_links() {
    use viso::platform::{DropAction, DropPhase, DropReply};
    let reply = DropReply::new();
    let mut steps = vec![outside(DropPhase::Over, 150.0, 100.0, &reply), redraw()];
    // Over the card zone, which takes no link, nothing answers.
    let _ = run(&[steps.clone()]);
    assert_eq!(reply.get(), None);
    assert_eq!(heard(), []);
    steps = [150.0, 190.0]
        .into_iter()
        .flat_map(|y| [outside(DropPhase::Over, 150.0, y, &reply), redraw()])
        .collect();
    let mut dropped = steps.clone();
    dropped.extend([outside(DropPhase::Drop, 150.0, 190.0, &reply), redraw()]);
    let _ = run(&[dropped]);
    assert_eq!(
        reply.get(),
        Some(DropAction::Copy),
        "the link zone copies it"
    );
    assert_eq!(
        heard(),
        [(2, "enter".to_owned()), (2, "drop None".to_owned())]
    );
    steps.extend([outside(DropPhase::Leave, 0.0, 0.0, &reply), redraw()]);
    let _ = run(&[steps]);
    assert_eq!(heard(), [(2, "enter".to_owned()), (2, "leave".to_owned())]);
}
