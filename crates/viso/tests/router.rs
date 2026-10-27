//! A typed router in a running app: deep links navigate it, the system back
//! action pops it, and the page below keeps its state through every turn.

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{KeyCode as Key, Modifiers as RawModifiers, RawEvent, RawKey, WindowId};
use viso::prelude::*;
use viso::ui::{EventCx, PageTransition};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

#[derive(Debug, Clone, PartialEq)]
enum Route {
    Inbox,
    Message(u32),
    Settings,
}

impl Route {
    /// The route a `mail://` link names.
    fn parse(url: &str) -> Option<Route> {
        let path = url.strip_prefix("mail://")?;
        Some(match path.split_once('/') {
            Some(("message", id)) => Route::Message(id.parse().ok()?),
            None if path == "settings" => Route::Settings,
            None if path == "inbox" => Route::Inbox,
            _ => return None,
        })
    }
}

thread_local! {
    /// The inbox page's draft counter, each time the page is built.
    static INBOX: RefCell<Vec<State<i32>>> = const { RefCell::new(Vec::new()) };
    /// The back stack each time a `mail://routes` link asked.
    static ROUTES: RefCell<Vec<Vec<Route>>> = const { RefCell::new(Vec::new()) };
}

struct Mail {
    nav: Option<Navigator<Route>>,
}

fn inbox(cx: &mut BuildCx<'_>) {
    let drafts = cx.state_of(0);
    INBOX.with(|i| i.borrow_mut().push(drafts));
    cx.leaf(LeafStyle::default());
}

fn page(cx: &mut BuildCx<'_>) {
    cx.leaf(LeafStyle::default());
}

impl Application for Mail {
    fn new(_cx: &mut AppCx) -> Self {
        Mail { nav: None }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let nav = router_view(
            Route::Inbox,
            routes! {
                Route::Inbox => inbox,
                Route::Message(_) => page,
                Route::Settings => page,
            },
        )
        .transition(PageTransition::None)
        .build(cx);
        self.nav = Some(nav);
    }

    fn on_open_url(&mut self, cx: &mut EventCx<'_>, url: &str) {
        let Some(nav) = self.nav else { return };
        if url == "mail://draft" {
            if let Some(drafts) = INBOX.with(|i| i.borrow().last().copied()) {
                drafts.update(cx, |n| n + 1);
            }
        } else if url == "mail://routes" {
            ROUTES.with(|r| r.borrow_mut().push(nav.routes(cx)));
        } else if let Some(route) = Route::parse(url) {
            // A deep link opens on top of the inbox, whatever showed before.
            nav.reset(cx, Route::Inbox);
            if route != Route::Inbox {
                nav.push(cx, route);
            }
        }
    }
}

fn open(url: &str) -> RawEvent {
    RawEvent::OpenUrl {
        url: url.to_owned(),
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn key(code: Key, pressed: bool) -> RawEvent {
    RawEvent::Key(RawKey {
        window: WIN,
        code,
        pressed,
        repeat: false,
        modifiers: RawModifiers::default(),
    })
}

#[test]
fn deep_links_and_system_back_keep_the_inbox_as_it_was() {
    INBOX.with(|i| i.borrow_mut().clear());
    ROUTES.with(|r| r.borrow_mut().clear());
    let app = drive_scripted::<Mail>(
        vec![
            redraw(),
            open("mail://draft"),
            redraw(),
            open("mail://message/5"),
            redraw(),
            key(Key::Back, true),
            key(Key::Back, false),
            redraw(),
            open("mail://routes"),
            open("mail://settings"),
            redraw(),
            open("mail://message/9"),
            redraw(),
            open("mail://routes"),
            redraw(),
        ],
        STEP,
    );
    assert_eq!(
        ROUTES.with(|r| r.borrow().clone()),
        [vec![Route::Inbox], vec![Route::Inbox, Route::Message(9)]],
        "the back action popped the message; the settings page gave way to the link"
    );
    let built = INBOX.with(|i| i.borrow().clone());
    assert_eq!(built.len(), 1, "the inbox was built once");
    assert_eq!(
        app.states().get(built[0].id()),
        Some(StateValue::Int(1)),
        "its draft survived the back action and both links"
    );
}

#[test]
fn a_back_action_at_the_inbox_is_the_hosts() {
    INBOX.with(|i| i.borrow_mut().clear());
    ROUTES.with(|r| r.borrow_mut().clear());
    let app = drive_scripted::<Mail>(
        vec![
            redraw(),
            key(Key::Back, true),
            key(Key::Back, false),
            redraw(),
            open("mail://routes"),
            redraw(),
        ],
        STEP,
    );
    assert_eq!(ROUTES.with(|r| r.borrow().clone()), [vec![Route::Inbox]]);
    assert_eq!(app.store().back_target(), None, "nothing to turn back");
}
