//! Markdown and HTML documents driven through the facade: their blocks lay
//! out one under another, a click on a link reaches `on_link`, and a source
//! a click appends to grows the document by its new block.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{
    Modifiers as RawModifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent,
    RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, Component, FlexStyle, LeafStyle, NodeId, PointerButtons, PointerPhase, Size};
use viso::widgets::{DocumentText, html, markdown};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

const DOC: &str = "# Title\n\nSee [docs](/docs) now.\n\n```\nlet x = 1;\nlet y = 2;\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n\n---\n";

const PAGE: &str = "<h2>Title</h2><p>Some <b>bold</b> text.</p>\
    <pre>let x = 1;\nlet y = 2;</pre><ol><li>one<li>two</ol>\
    <table><tr><th>a<th>b<tr><td>1<td>2</table><hr>";

thread_local! {
    static FOLLOWED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static SOURCE: DocumentText = DocumentText::new("First.");
}

/// The document over a 20 by 20 box a click on which appends to `SOURCE`.
struct DocApp;

impl Application for DocApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        DocApp
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
                let more = cx.leaf(LeafStyle {
                    size: Size::fixed(20.0, 20.0),
                    ..LeafStyle::default()
                });
                cx.on_pointer(more, |ev| {
                    if ev.pointer().is_some_and(|p| {
                        p.phase == PointerPhase::Up && p.buttons.contains(PointerButtons::PRIMARY)
                    }) {
                        SOURCE.with(|s| s.push_str(ev, "\n\n- one\n- two"));
                    }
                });
                markdown(DOC)
                    .on_link(|_ev, url| FOLLOWED.with(|f| f.borrow_mut().push(url.to_owned())))
                    .build(cx);
                SOURCE.with(|s| markdown(s).build(cx));
                html(PAGE).build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f32, y: f32, phase: RawPhase) -> RawEvent {
    // A release carries the button it releases.
    let buttons = if phase == RawPhase::Moved {
        RawButtons::NONE
    } else {
        RawButtons::PRIMARY
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

/// The root's children: the button, the document, the streamed document,
/// the HTML page.
fn parts(app: &viso::__test_support::DrivenApp<DocApp>) -> Vec<NodeId> {
    let store = app.store();
    let root = app.root().expect("a root");
    store.arena().children(root).collect()
}

#[test]
fn the_blocks_lay_out_one_under_another_each_drawing_something() {
    let app = drive_scripted::<DocApp>(vec![redraw(), redraw()], STEP);
    let store = app.store();
    let doc = parts(&app)[1];
    let blocks: Vec<NodeId> = store.arena().children(doc).collect();
    assert_eq!(
        blocks.len(),
        6,
        "heading, paragraph, code, table, list, rule"
    );
    let mut bottom = f32::MIN;
    for &block in &blocks {
        let r = store.world(block);
        assert!(r.w > 0.0 && r.h > 0.0, "block {block:?} draws: {r:?}");
        assert!(r.y >= bottom, "below the one before");
        bottom = r.y + r.h;
    }
    let code = store.world(blocks[2]);
    assert!(
        code.h > 2.0 * 14.0 * 0.875,
        "both code lines show: {code:?}"
    );
    let grid = store.arena().children(blocks[3]).next().unwrap();
    let grid = store.world(grid);
    assert!(
        grid.w > 20.0 && grid.h > 2.0 * 14.0,
        "both rows show: {grid:?}"
    );
    let list = blocks[4];
    let item = store.arena().children(list).next().unwrap();
    let check = store.arena().children(item).next().unwrap();
    let check = store.world(check);
    assert!(
        check.w >= 14.0 && check.h >= 14.0,
        "the box shows: {check:?}"
    );
}

#[test]
fn a_click_on_a_link_reaches_on_link() {
    let app = drive_scripted::<DocApp>(vec![redraw(), redraw()], STEP);
    let store = app.store();
    let doc = parts(&app)[1];
    let paragraph = store.arena().children(doc).nth(1).unwrap();
    let docs = store.link_boxes(paragraph, 0).next().expect("a link box");
    let (x, y) = (docs.x + docs.w / 2.0, docs.y + docs.h / 2.0);
    drive_scripted::<DocApp>(
        vec![
            redraw(),
            redraw(),
            mouse(x, y, RawPhase::Down),
            mouse(x, y, RawPhase::Up),
            redraw(),
        ],
        STEP,
    );
    assert_eq!(FOLLOWED.with(|f| f.take()), ["/docs"]);
}

#[test]
fn an_append_shows_its_new_block_under_the_others() {
    let app = drive_scripted::<DocApp>(
        vec![
            redraw(),
            redraw(),
            mouse(10.0, 10.0, RawPhase::Down),
            mouse(10.0, 10.0, RawPhase::Up),
            redraw(),
            redraw(),
        ],
        STEP,
    );
    let store = app.store();
    let doc = parts(&app)[2];
    let blocks: Vec<NodeId> = store.arena().children(doc).collect();
    assert_eq!(blocks.len(), 2, "the paragraph and the appended list");
    let (first, list) = (store.world(blocks[0]), store.world(blocks[1]));
    assert!(
        list.h > 0.0 && list.y >= first.y + first.h,
        "{first:?} {list:?}"
    );
    assert_eq!(store.arena().children(blocks[1]).count(), 2);
}

#[test]
fn an_html_page_lays_out_its_blocks_like_a_markdown_one() {
    let app = drive_scripted::<DocApp>(vec![redraw(), redraw()], STEP);
    let store = app.store();
    let page = parts(&app)[3];
    let blocks: Vec<NodeId> = store.arena().children(page).collect();
    assert_eq!(
        blocks.len(),
        6,
        "heading, paragraph, code, list, table, rule"
    );
    let mut bottom = f32::MIN;
    for &block in &blocks {
        let r = store.world(block);
        assert!(r.w > 0.0 && r.h > 0.0, "block {block:?} draws: {r:?}");
        assert!(r.y >= bottom, "below the one before");
        bottom = r.y + r.h;
    }
    assert_eq!(store.arena().children(blocks[3]).count(), 2, "two items");
}
