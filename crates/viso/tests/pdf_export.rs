//! A view written as a PDF through the facade: laid out on pages of the
//! paper asked for, cut between lines, each page with its footer, its text
//! embedded as fonts and its shadow as a raster.
#![cfg(all(feature = "headless", feature = "pdf"))]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::drive_scripted_frames;
use viso::pdf::{self, Margins, PageSize, Pdf};
use viso::platform::{RawEvent, WindowId};
use viso::prelude::*;
use viso::services::{Reply, ServiceResult};
use viso::ui::{BoxStyle, Component, Rgba, Shadow, Sizing};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
const LINES: usize = 90;

thread_local! {
    static REPLY: RefCell<Option<Reply<Vec<u8>>>> = const { RefCell::new(None) };
}

fn report(cx: &mut BuildCx<'_>) {
    let card = cx.leaf(LeafStyle {
        size: Sizing::fixed(200.0, 80.0),
        style: BoxStyle::solid(Rgba::new(0.2, 0.4, 0.9, 1.0)).with_radius(12.0),
    });
    cx.shadow(card, Shadow::FLOATING);
    for line in 0..LINES {
        label(format!("Line {line} of the report"))
            .font_size(16.0)
            .build(cx);
    }
    pdf::page_break(cx);
    label("After the break").font_size(16.0).build(cx);
}

struct ExportApp;

impl Application for ExportApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        ExportApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.leaf(LeafStyle {
            size: Sizing::fill(),
            style: BoxStyle::NONE,
        });
        let reply = pdf::export(
            Pdf::new(report)
                .size(PageSize::A5)
                .margins(Margins::all(36.0))
                .title("Report")
                .footer(|cx, page| {
                    label(format!("Page {} of {}", page.number, page.count))
                        .font_size(10.0)
                        .build(cx);
                }),
        );
        REPLY.with(|r| *r.borrow_mut() = Some(reply));
    }
}

fn exported() -> Vec<u8> {
    let _app = drive_scripted_frames::<ExportApp>(
        vec![RawEvent::RedrawRequested { window: WIN }],
        STEP,
        30,
    );
    let result: Option<ServiceResult<Vec<u8>>> =
        REPLY.with(|r| r.borrow_mut().as_mut().and_then(Reply::try_take));
    let bytes = result.expect("written within the frames").expect("written");
    if let Some(path) = std::env::var_os("VISO_PDF_OUT") {
        std::fs::write(path, &bytes).unwrap();
    }
    bytes
}

fn count(bytes: &[u8], what: &[u8]) -> usize {
    bytes.windows(what.len()).filter(|w| *w == what).count()
}

#[test]
fn a_view_is_written_across_pages_with_its_text_and_shadow() {
    let bytes = exported();
    assert!(bytes.starts_with(b"%PDF-1.7"));
    assert!(bytes.ends_with(b"%%EOF\n"));
    // 90 lines of about 19 px on A5 pages of (595 - 72) / 0.75 = 697 px
    // fill three pages; the break starts a fourth.
    let pages = count(&bytes, b"/Type/Page/");
    assert_eq!(pages, 4, "pages");
    assert_eq!(count(&bytes, b"/MediaBox[0 0 419.53 595.28]"), pages);
    assert!(count(&bytes, b"/Subtype/Type3") >= 1, "text is embedded");
    assert!(count(&bytes, b"/Subtype/Image") >= 1, "the shadow is drawn");
    assert!(count(&bytes, b"/Title(Report)") == 1);
}
