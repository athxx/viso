//! A text's `spans` and `font_style`, from source to the text request
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

const LINKED: &str = r#"
export component Doc {
    state followed = 0;
    state caught = 0;
    view {
        Column {
            on link(e) { caught += 1; }
            Text {
                spans: [
                    Span { text: "Read " },
                    Span { text: "the docs", link: Option::Some("/docs") },
                    Span { text: " or " },
                    Span { text: "the site", link: Option::Some("/site"), underline: false },
                ];
                on link(e) { followed = if e.url == "/docs" { 1 } else { 2 }; }
            }
        }
    }
}
"#;

#[test]
fn a_span_s_link_leads_where_it_says_and_its_activation_bubbles_to_on_link() {
    for mut rt in [Rt::mount(LINKED), Rt::packaged(LINKED)] {
        assert_eq!(rt.fault(), None);
        let [request] = &declared(&mut rt)[..] else {
            panic!("one linked text");
        };
        let links: Vec<_> = request
            .spans
            .iter()
            .map(|s| (s.range.clone(), s.style.link.as_deref(), s.style.underline))
            .collect();
        assert_eq!(
            links,
            [(5..13, Some("/docs"), true), (17..25, Some("/site"), true),],
            "a link is underlined"
        );
        let text = rt.children(rt.root.expect("mounted"))[0];
        assert!(rt.store.focusable(text), "its links take the focus");
        for (target, followed) in [("/site", 2), ("/docs", 1)] {
            viso_ui::KeyRouter::route_link(
                &mut rt.store,
                &mut rt.states,
                &rt.bindings,
                &mut rt.text_edits,
                text,
                target,
            );
            rt.frame();
            assert_eq!(rt.int("followed"), Some(followed));
        }
        assert_eq!(rt.int("caught"), Some(2), "the column hears each too");
    }
}

const SELECTABLE: &str = r#"
export component Note {
    state pick = true;
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { pick = !pick; } }
            Text { text: "Copy me"; selectable: pick; }
        }
    }
}
"#;

#[test]
fn a_selectable_text_is_a_read_only_field_until_it_is_not() {
    for mut rt in [Rt::mount(SELECTABLE), Rt::packaged(SELECTABLE)] {
        assert_eq!(rt.fault(), None);
        let text = rt.children(rt.root.expect("mounted"))[1];
        let reconcile = |rt: &mut Rt| {
            viso_ui::text_edit::reconcile(&mut rt.store, &mut rt.text_edits, None);
        };
        reconcile(&mut rt);
        let buffer = rt.text_edits.get(text).expect("a field's buffer");
        assert!(buffer.look.read_only);
        assert_eq!(buffer.text, "Copy me");
        assert!(rt.store.field_marks(text).is_some());
        assert!(rt.store.focusable(text), "it takes the focus");
        assert!(rt.store.has_key_handler(text), "its keys select");

        rt.click(0);
        rt.frame();
        reconcile(&mut rt);
        assert!(rt.text_edits.get(text).is_none(), "its buffer goes");
        assert!(rt.store.field_marks(text).is_none());
        assert!(!rt.store.focusable(text));
        assert_eq!(
            rt.store.text_request(text).map(|r| r.text.as_str()),
            Some("Copy me"),
            "a plain text again"
        );

        rt.click(0);
        rt.frame();
        reconcile(&mut rt);
        assert!(rt.text_edits.get(text).is_some_and(|b| b.look.read_only));
    }
}
