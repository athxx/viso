//! A link the app is asked to open reaches `Application::on_open_url` with
//! the launch window's context, in order, and what the app writes there
//! lands as a handler's write does.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{RawEvent, WindowId};
use viso::prelude::*;
use viso::ui::{BoxStyle, EventCx, Size};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static OPENED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static ITEM: Cell<Option<StateId>> = const { Cell::new(None) };
}

struct Notes {
    item: Option<StateId>,
}

impl Application for Notes {
    fn new(_cx: &mut AppCx) -> Self {
        Notes { item: None }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let item = cx.state(StateValue::Int(0));
        self.item = Some(item);
        ITEM.with(|i| i.set(Some(item)));
        let leaf = cx.leaf(LeafStyle {
            size: Size::fill(),
            style: BoxStyle::NONE,
        });
        cx.bind(item, leaf, DirtyClass::PAINT);
    }

    fn on_open_url(&mut self, cx: &mut EventCx<'_>, url: &str) {
        OPENED.with(|o| o.borrow_mut().push(url.to_owned()));
        let number = url
            .strip_prefix("notes://item/")
            .and_then(|n| n.parse().ok());
        if let (Some(item), Some(number)) = (self.item, number) {
            cx.set(item, StateValue::Int(number));
        }
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn open(url: &str) -> RawEvent {
    RawEvent::OpenUrl {
        url: url.to_owned(),
    }
}

#[test]
fn each_link_reaches_the_app_in_order_and_writes_its_state() {
    OPENED.with(|o| o.borrow_mut().clear());
    let app = drive_scripted::<Notes>(
        vec![
            open("notes://item/7"),
            redraw(),
            redraw(),
            open("notes://item/12"),
            open("notes://about"),
            redraw(),
        ],
        STEP,
    );
    assert_eq!(
        OPENED.with(|o| o.borrow().clone()),
        ["notes://item/7", "notes://item/12", "notes://about"]
    );
    let item = ITEM.with(Cell::get).unwrap();
    assert_eq!(app.states().get(item), Some(StateValue::Int(12)));
}
