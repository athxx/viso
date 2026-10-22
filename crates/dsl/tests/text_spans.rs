//! A text's `spans` and `font_style` (§U5), from source to the text request
//! its node declares: its pieces joined into its text, each drawn its own
//! way, constant or reading state, under hot reload and from the release
//! package.

mod support;

use support::Rt;
use viso_ui::{FontSlant, FontWeight, LengthTerms, NodeId, Rgba, SpanStyle, TextRequest, TextSpan};

const SPANNED: &str = r#"
export component Note {
    state loud = false;
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { loud = !loud; } }
            Text {
                text: "ignored";
                font_style: FontStyle::italic;
                spans: [
                    Span { text: "Saved " },
                    Span {
                        text: "3 files",
                        font_weight: Option::Some(FontWeight::bold),
                        color: if loud { Option::Some(#ff0000) } else { Option::None },
                        underline: loud,
                    },
                    Span { text: "²", font_size: Option::Some(0.75em), raise: Option::Some(0.5em) },

                ];
            }
        }
    }
}
"#;

/// The requests declared since the last call with any spans.
fn declared(rt: &mut Rt) -> Vec<TextRequest> {
    let mut requests: Vec<(NodeId, Box<TextRequest>)> = Vec::new();
    rt.store.take_text_requests(&mut requests);
    requests
        .into_iter()
        .map(|(_, r)| *r)

        .filter(|r| !r.spans.is_empty())
        .collect()
}

fn bold(color: Option<Rgba>, underline: bool) -> TextSpan {
    TextSpan {
        range: 6..13,
        style: SpanStyle {
            weight: Some(FontWeight::BOLD),
            color,
            underline,
            ..SpanStyle::default()
        },
    }
}

fn raised() -> TextSpan {
    TextSpan {
        range: 13..15,
        style: SpanStyle {
            size: Some(LengthTerms::em(0.75)),
            raise: Some(LengthTerms::em(0.5)),
            ..SpanStyle::default()
        },
    }
}

#[test]
fn spans_join_into_the_text_and_draw_each_its_way_in_every_target() {
    for mut rt in [Rt::mount(SPANNED), Rt::packaged(SPANNED)] {
        assert_eq!(rt.fault(), None);
        let [request] = &declared(&mut rt)[..] else {
            panic!("one spanned text");
        };
        assert_eq!(request.text, "Saved 3 files²");
        assert_eq!(request.style.slant, FontSlant::Italic);
        assert_eq!(request.spans, [bold(None, false), raised()]);
        rt.click(0);
        let [request] = &declared(&mut rt)[..] else {
            panic!("the state the spans read declares the text again");
        };
        let red = request.spans[0].style.color.expect("red");
        assert!(red.r > 0.9 && red.g < 0.01, "{red:?}");
        assert_eq!(request.spans[0], bold(Some(red), true));
    }
}

#[test]
fn a_reload_respans_the_text_in_place() {
    let mut rt = Rt::mount(SPANNED);
    let root = rt.root;
    declared(&mut rt);
    rt.reload(&SPANNED.replace("Saved ", "Kept "));
    assert_eq!(rt.root, root);
    let [request] = &declared(&mut rt)[..] else {
        panic!("one spanned text");
    };
    assert_eq!(request.text, "Kept 3 files²");
    assert_eq!(request.spans[0].range, 5..12);
}
