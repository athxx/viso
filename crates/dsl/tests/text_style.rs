//! A text's `font_weight`, `font_family`, `line_height`, `max_lines`,
//! `overflow` and `align` (§U5), from source to the text request its node
//! declares: constant or reading state, under hot reload and from the release
//! package, on a `Text`, a `Button` and a `TextInput`.

mod support;

use std::num::NonZeroU32;
use std::sync::Arc;

use support::{Rt, origin};
use viso_dsl::frontend::compile_file;
use viso_ui::{
    FieldSeed, FontFamily, FontSize, FontWeight, LengthTerms, NodeId, TextAlign, TextOverflow,
    TextRequest, TextSizing, TextStyle,
};

const STYLED: &str = r#"
export component Styled {
    state bold = false;
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { bold = !bold; } }
            Text {
                text: "Title";
                font_weight: if bold { FontWeight::bold } else { FontWeight::regular };
                font_family: Option::Some(FontFamily::named("Inter"));
                line_height: Option::Some(150%);
                max_lines: Option::Some(2);
                overflow: TextOverflow::ellipsis;
                align: TextAlign::center;
            }
            Button {
                text: "Go";
                font_family: Option::Some(FontFamily::monospace);
                line_height: if bold { Option::Some(2em) } else { Option::None };
            }
            TextInput { value: "x"; align: TextAlign::end; font_weight: FontWeight::semibold; }
        }
    }
}
"#;

/// The requests declared since the last call, by text, each with the line
/// height it resolves to.
fn declared(rt: &mut Rt) -> Vec<(String, TextStyle, Option<f32>)> {
    let mut requests: Vec<(NodeId, Box<TextRequest>)> = Vec::new();
    rt.store.take_text_requests(&mut requests);
    let mut seeds: Vec<(NodeId, FieldSeed)> = Vec::new();
    rt.store.take_text_seeds(&mut seeds);
    let seeds = seeds.into_iter().map(|(id, seed)| (id, seed.request));
    let store = &rt.store;
    let mut out: Vec<_> = requests
        .into_iter()
        .map(|(id, r)| (id, *r))
        .chain(seeds)
        .filter(|(_, r)| !r.text.is_empty())
        .map(|(id, r)| {
            assert_eq!(r.font_size, FontSize::Inherited);
            let TextSizing {
                font_size,
                line_height,
            } = store.text_sizing(id, &r);
            assert_eq!(font_size, 14.0);
            (r.text, r.style, line_height)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn title(weight: FontWeight) -> TextStyle {
    TextStyle {
        weight,
        family: Some(FontFamily::Named(Arc::from("Inter"))),
        line_height: Some(LengthTerms::pct(150.0)),
        max_lines: NonZeroU32::new(2),
        overflow: TextOverflow::Ellipsis,
        align: TextAlign::Center,
    }
}

fn button(line_height: Option<LengthTerms>) -> TextStyle {
    TextStyle {
        family: Some(FontFamily::Monospace),
        line_height,
        ..TextStyle::default()
    }
}

#[test]
fn a_text_style_reaches_the_request_in_every_target() {
    for mut rt in [Rt::mount(STYLED), Rt::packaged(STYLED)] {
        assert_eq!(rt.fault(), None);
        let field = TextStyle {
            weight: FontWeight(600),
            align: TextAlign::End,
            ..TextStyle::default()
        };
        assert_eq!(
            declared(&mut rt),
            [
                ("Go".to_string(), button(None), None),
                ("Title".to_string(), title(FontWeight::REGULAR), Some(21.0)),
                ("x".to_string(), field, None),
            ]
        );
        rt.click(0);
        assert_eq!(
            declared(&mut rt),
            [
                (
                    "Go".to_string(),
                    button(Some(LengthTerms::em(2.0))),
                    Some(28.0)
                ),
                ("Title".to_string(), title(FontWeight::BOLD), Some(21.0)),
            ],
            "a state the style reads declares the text again"
        );
        rt.click(0);
        assert_eq!(declared(&mut rt).len(), 2, "and back");
    }
}

#[test]
fn a_reload_restyles_the_text_in_place() {
    let mut rt = Rt::mount(STYLED);
    let root = rt.root;
    declared(&mut rt);
    rt.reload(&STYLED.replace("TextAlign::center", "TextAlign::start"));
    assert_eq!(rt.root, root);
    let restyled = declared(&mut rt);
    let title = restyled
        .iter()
        .find(|(text, ..)| text == "Title")
        .expect("Title");
    assert_eq!(title.1.align, TextAlign::Start);
}

#[test]
fn every_text_style_property_compiles_for_text_button_and_input() {
    let view = |node: &str| {
        format!("export component A {{ state n = 0; view {{ Column {{ {node} }} }} }}")
    };
    for node in [
        "Text { text: \"a\"; font_weight: FontWeight::bold; align: TextAlign::center; }",
        "Text { text: \"a\"; max_lines: if n > 0 { Option::Some(1) } else { Option::None }; overflow: TextOverflow::ellipsis; }",
        "Button { text: \"a\"; font_weight: FontWeight::medium; max_lines: Option::Some(1); }",
        "TextInput { font_family: Option::Some(FontFamily::serif); line_height: Option::Some(20dp); }",
    ] {
        let errors: Vec<_> = compile_file(&view(node), &origin())
            .errors()
            .map(|d| (d.code.to_string(), d.message.clone()))
            .collect();
        assert_eq!(errors, [], "{node}");
    }
}

const SIZED: &str = r#"
export component Sized {
    state big = false;
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { big = !big; } }
            Text {
                text: "Body";
                font_size: if big { 2em } else { 1em };
                line_height: Option::Some(1.5em);
            }
            Text { text: "Fixed"; font_size: 10dp; }
        }
    }
}
"#;

#[test]
fn a_mounted_text_follows_the_text_scale_and_its_font_size() {
    for mut rt in [Rt::mount(SIZED), Rt::packaged(SIZED)] {
        assert_eq!(rt.fault(), None);
        let mut requests: Vec<(NodeId, Box<TextRequest>)> = Vec::new();
        rt.store.take_text_requests(&mut requests);
        let node = |text: &str| {
            let (id, request) = requests
                .iter()
                .find(|(_, r)| r.text == text)
                .expect("declared");
            (*id, (**request).clone())
        };
        let (body, fixed) = (node("Body"), node("Fixed"));
        let sizing = |rt: &Rt, (id, request): &(NodeId, TextRequest)| {
            let s = rt.store.text_sizing(*id, request);
            (s.font_size, s.line_height)
        };
        assert_eq!(sizing(&rt, &body), (14.0, Some(21.0)));
        assert_eq!(sizing(&rt, &fixed), (10.0, None));
        rt.store.take_text_resized();

        rt.update_env(|env| env.text_scale = 2.0);
        assert!(rt.store.take_text_resized(), "the text scale moves sizes");
        assert_eq!(sizing(&rt, &body), (28.0, Some(42.0)));
        assert_eq!(sizing(&rt, &fixed), (10.0, None), "a dp size holds");

        rt.click(0);
        assert!(rt.store.take_text_resized(), "its font size moved");
        assert_eq!(sizing(&rt, &body), (56.0, Some(84.0)));
        assert_eq!(
            declared(&mut rt),
            [],
            "the texts are not declared again: the shaper places them anew"
        );
    }
}
