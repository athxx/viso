//! The `viso::clipboard` natives through the facade on the headless
//! backend: a handler's `write_text` reaches the platform clipboard at the
//! end of its frame, and a task's `read_text` waits for the platform's answer
//! and reads what is there — the view's own write, then a handler's copy.
//! The `Viso.toml` beside this file grants the clipboard.

use std::cell::Cell;
use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::clipboard::ClipboardData;
use viso::platform::{
    Modifiers as RawModifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent,
    RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{FlexStyle, PointerPhase, Sizing, StackStyle, StateId, StateValue};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

viso::component! {
    import viso::clipboard;

    Clip {
        state got = 0;
        task paste() -> I64 {
            match await clipboard::read_text() {
                Option::Some(text) => if text == "from the view" { 1 } else if text == "from a handler" { 2 } else { 3 },
                Option::None => 4,
            }
        }
        view {
            Column {
                width: 200dp;
                height: 80dp;
                Text { width: 200dp; height: 40dp; on click { clipboard::write_text("from the view"); } }
                Text {
                    width: 200dp;
                    height: 40dp;
                    on click { start paste() { success(text) { got = text; } }; }
                }
            }
        }
    }
}

thread_local! {
    static GOT: Cell<Option<StateId>> = const { Cell::new(None) };
}

/// The view at the top, a strip below it copying through a handler.
struct Board;

impl Application for Board {
    fn new(_cx: &mut AppCx) -> Self {
        Board
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(
            FlexStyle {
                size: Sizing::fill(),
                axis: viso::ui::Axis::Column,
                ..FlexStyle::default()
            },
            |cx| {
                let (Clip { got }, _) = Clip::build(cx);
                GOT.with(|cell| cell.set(Some(got)));
                let copier = cx.stack(
                    StackStyle {
                        size: Sizing::fixed(200.0, 40.0),
                        ..StackStyle::default()
                    },
                    |_| {},
                );
                cx.on_pointer(copier, |ev| {
                    if ev.pointer().is_some_and(|p| p.phase == PointerPhase::Down) {
                        ev.copy(ClipboardData::text("from a handler"));
                    }
                });
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn click(y: f64) -> [RawEvent; 3] {
    let mouse = |phase, buttons| {
        RawEvent::Pointer(RawPointer::mouse(
            WIN,
            20.0,
            y,
            buttons,
            RawModifiers::default(),
            phase,
        ))
    };
    [
        mouse(RawPhase::Down, RawButtons::PRIMARY),
        mouse(RawPhase::Up, RawButtons::NONE),
        redraw(),
    ]
}

fn got(script: Vec<RawEvent>) -> Option<StateValue> {
    let app = drive_scripted::<Board>(script, STEP);
    let id = GOT.with(Cell::get).expect("built");
    app.states().get(id)
}

#[test]
fn a_read_waits_for_the_platform_and_sees_the_views_write_then_a_handlers_copy() {
    let mut script = vec![redraw()];
    script.extend(click(20.0));
    script.extend(click(60.0));
    script.extend([redraw(), redraw()]);
    assert_eq!(got(script), Some(StateValue::Int(1)), "the view's write");

    let mut script = vec![redraw()];
    script.extend(click(20.0));
    script.extend(click(100.0));
    script.extend(click(60.0));
    script.extend([redraw(), redraw()]);
    assert_eq!(got(script), Some(StateValue::Int(2)), "the handler's copy");
}
