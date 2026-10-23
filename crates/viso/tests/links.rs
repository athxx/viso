//! A text's links driven through the facade: the hand shows over one, a click
//! on it reaches the label's `on_link`, and Tab then Enter activates the
//! focused one.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{drive_scripted, drive_scripted_with_services};
use viso::platform::{
    CursorIcon, KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, Component, Content, FlexStyle, Rect, Size};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    /// Where each activated link led, in order.
    static FOLLOWED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// "see docs or site", its "docs" and "site" links.
struct LinksApp;

impl Application for LinksApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        LinksApp
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
                size: Size::fill(),
                axis: Axis::Column,
                ..FlexStyle::default()
            },
            |cx| {
                rich_text([
                    ("see ", SpanStyle::default()),
                    ("docs", SpanStyle::default().link("/docs")),
                    (" or ", SpanStyle::default()),
                    ("site", SpanStyle::default().link("/site")),
                ])
                .font_size(20.0)
                .on_link(|_ev, target| FOLLOWED.with(|f| f.borrow_mut().push(target.to_owned())))
                .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f32, y: f32, phase: RawPhase) -> RawEvent {
    let buttons = if phase == RawPhase::Down {
        RawButtons::PRIMARY
    } else {
        RawButtons::NONE
    };
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        f64::from(x),
        f64::from(y),
        buttons,
        RawModifiers::default(),
        phase,
    ))
}

fn key(code: Key) -> [RawEvent; 2] {
    let raw = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers: RawModifiers::default(),
        })
    };
    [raw(true), raw(false)]
}

fn followed() -> Vec<String> {
    FOLLOWED.with(|f| std::mem::take(&mut *f.borrow_mut()))
}

/// Where each of the text's links draws, in the window.
fn link_boxes() -> Vec<(u16, Rect)> {
    let app = drive_scripted::<LinksApp>(vec![redraw(), redraw()], STEP);
    let store = app.store();
    let text = app.root().and_then(|c| store.arena().children(c).next());
    let text = text.expect("the text");
    let Some(Content::Text { ink: Some(ink), .. }) = store.content_payload(text) else {
        panic!("a text of links inks");
    };
    let world = store.world(text);
    ink.links
        .iter()
        .map(|b| {
            let r = b.rect;
            (b.link, Rect::new(world.x + r.x, world.y + r.y, r.w, r.h))
        })
        .collect()
}

#[test]
fn each_link_draws_one_box_over_its_own_words() {
    let boxes = link_boxes();
    let [(0, docs), (1, site)] = boxes[..] else {
        panic!("a box for each link: {boxes:?}");
    };
    assert!(docs.w > 0.0 && docs.h > 0.0);
    assert!(docs.x > 0.0, "\"see \" comes before it");
    assert!(site.x > docs.x + docs.w, "\" or \" comes between them");
    assert_eq!(docs.y, site.y, "on one line");
}

#[test]
fn the_hand_shows_over_a_link_and_a_click_on_it_follows_it() {
    followed();
    let boxes = link_boxes();
    let (_, docs) = boxes[0];
    let (x, y) = (docs.x + docs.w / 2.0, docs.y + docs.h / 2.0);
    let hover = drive_scripted::<LinksApp>(
        vec![redraw(), redraw(), mouse(x, y, RawPhase::Moved), redraw()],
        STEP,
    );
    assert_eq!(hover.cursor(), CursorIcon::Pointer);
    drive_scripted::<LinksApp>(
        vec![
            redraw(),
            redraw(),
            mouse(x, y, RawPhase::Down),
            mouse(x + 1.0, y, RawPhase::Up),
            redraw(),
        ],
        STEP,
    );
    assert_eq!(followed(), ["/docs"]);
    drive_scripted::<LinksApp>(
        vec![
            redraw(),
            redraw(),
            mouse(x, y, RawPhase::Down),
            mouse(1.0, y, RawPhase::Up),
            redraw(),
        ],
        STEP,
    );
    assert!(followed().is_empty(), "a release off the link follows none");
}

#[test]
fn tab_reaches_each_link_in_turn_and_enter_follows_the_focused_one() {
    followed();
    let mut script = vec![redraw(), redraw()];
    script.extend(key(Key::Tab));
    script.extend(key(Key::Enter));
    script.extend(key(Key::Tab));
    script.extend(key(Key::Enter));
    script.push(redraw());
    drive_scripted::<LinksApp>(script, STEP);
    assert_eq!(followed(), ["/docs", "/site"]);
}

/// A link label with no `on_link`, under a column that keeps nothing.
struct LabelApp;

impl Application for LabelApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        LabelApp
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
                size: Size::fill(),
                axis: Axis::Column,
                ..FlexStyle::default()
            },
            |cx| {
                link_label("docs", "https://example.org/docs")
                    .font_size(20.0)
                    .build(cx);
            },
        );
    }
}

#[test]
fn a_link_no_handler_keeps_opens_with_the_system_and_one_kept_does_not() {
    let mock = viso::services::Mock::new();
    let mut script = vec![redraw(), redraw()];
    script.extend(key(Key::Tab));
    script.extend(key(Key::Enter));
    script.push(redraw());
    drive_scripted_with_services::<LabelApp>(script.clone(), STEP, mock.services());
    assert_eq!(
        mock.calls(),
        [viso::services::Call::OpenUrl(
            "https://example.org/docs".into()
        )]
    );

    followed();
    let kept = viso::services::Mock::new();
    let mut script = vec![redraw(), redraw()];
    script.extend(key(Key::Tab));
    script.extend(key(Key::Enter));
    script.push(redraw());
    drive_scripted_with_services::<LinksApp>(script, STEP, kept.services());
    assert_eq!(followed(), ["/docs"]);
    assert!(kept.calls().is_empty(), "the label's on_link kept it");
}

viso::component! {
    DocsLink {
        view {
            Column {
                width: 400dp;
                height: 100dp;
                LinkLabel {
                    text: "docs";
                    url: "https://example.org/dsl";
                    font_size: 20dp;
                }
            }
        }
    }
}

/// A view's `LinkLabel`.
struct DslApp;

impl Application for DslApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        DslApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        DocsLink::build(cx);
    }
}

#[test]
fn a_view_s_link_label_opens_its_url_with_the_system() {
    let mock = viso::services::Mock::new();
    let mut script = vec![redraw(), redraw()];
    script.extend(key(Key::Tab));
    script.extend(key(Key::Enter));
    script.push(redraw());
    drive_scripted_with_services::<DslApp>(script, STEP, mock.services());
    assert_eq!(
        mock.calls(),
        [viso::services::Call::OpenUrl(
            "https://example.org/dsl".into()
        )]
    );
}
