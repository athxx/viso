//! `Application::on_lifecycle` hears each OS lifecycle change on the launch
//! window's context, in order, and a `@persist` state written as the app is
//! suspended is durable before the app could be killed.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::Lifecycle;
use viso::platform::{RawEvent, WindowId};
use viso::prelude::*;
use viso::ui::EventCx;
use viso_view::persist::{MemoryStore, SharedStore};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
const KEY: &str = "draft.words";

viso::component! {
    Draft {
        @persist("draft.words")
        state words = 0;
        view {
            Text { width: 200dp; height: 40dp; text: "{words} words"; }
        }
    }
}

thread_local! {
    static HEARD: RefCell<Vec<Lifecycle>> = const { RefCell::new(Vec::new()) };
    static WORDS: Cell<Option<State<i32>>> = const { Cell::new(None) };
    /// What the store held for the draft when the app came back.
    static STORED_BY_RESUME: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    static MEMORY: RefCell<Option<MemoryStore>> = const { RefCell::new(None) };
}

struct Writer {
    words: Option<State<i32>>,
}

impl Application for Writer {
    fn new(_cx: &mut AppCx) -> Self {
        Writer { words: None }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let (Draft { words }, _) = Draft::build(cx);
        let words = State::from_id(words);
        self.words = Some(words);
        WORDS.with(|w| w.set(Some(words)));
    }

    fn on_lifecycle(&mut self, cx: &mut EventCx<'_>, event: Lifecycle) {
        HEARD.with(|h| h.borrow_mut().push(event));
        match event {
            Lifecycle::Suspended => {
                if let Some(words) = self.words {
                    words.set(cx, 41);
                }
            }
            Lifecycle::Resumed => {
                let stored = MEMORY.with(|m| m.borrow().as_ref().and_then(|m| m.get(KEY)));
                STORED_BY_RESUME.with(|s| *s.borrow_mut() = stored);
            }
            Lifecycle::LowMemory => {}
        }
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

#[test]
fn each_change_reaches_the_app_in_order_and_a_suspend_write_is_durable() {
    let memory = MemoryStore::default();
    viso_view::install_persistence(Some(SharedStore::new(memory.clone())));
    MEMORY.with(|m| *m.borrow_mut() = Some(memory.clone()));
    HEARD.with(|h| h.borrow_mut().clear());

    drive_scripted::<Writer>(
        vec![
            redraw(),
            RawEvent::Suspended,
            RawEvent::Resumed,
            redraw(),
            RawEvent::LowMemory,
            redraw(),
        ],
        STEP,
    );
    assert_eq!(
        HEARD.with(|h| h.borrow().clone()),
        [
            Lifecycle::Suspended,
            Lifecycle::Resumed,
            Lifecycle::LowMemory
        ]
    );

    // A later launch loads 41: what the store holds now, and held when the
    // app came back, so the suspend itself stored the hook's write.
    let app = drive_scripted::<Writer>(vec![redraw()], STEP);
    let words = WORDS.with(Cell::get).expect("built");
    assert_eq!(app.states().get(words.id()), Some(StateValue::Int(41)));
    assert_eq!(
        STORED_BY_RESUME.with(|s| s.borrow().clone()),
        memory.get(KEY),
        "the suspend made the hook's write durable"
    );
    viso_view::install_persistence(None);
}
