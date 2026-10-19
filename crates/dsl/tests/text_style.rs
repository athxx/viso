//! A text's `font_weight`, `font_family`, `line_height`, `max_lines`,
//! `overflow` and `align` (§U5), from source to the text request its node
//! declares: constant or reading state, under hot reload and from the release
//! package, on a `Text`, a `Button` and a `TextInput`.

mod support;

use std::num::NonZeroU32;
use std::sync::Arc;

use support::{Rt, origin};
use viso_dsl::frontend::compile_file;
use viso_ui::{FontFamily, FontWeight, NodeId, TextAlign, TextOverflow, TextRequest, TextStyle};

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

/// The requests declared since the last call, by text.
fn declared(rt: &mut Rt) -> Vec<(String, TextStyle)> {
    let mut requests: Vec<(NodeId, Box<TextRequest>)> = Vec::new();
    rt.store.take_text_requests(&mut requests);
    let mut seeds: Vec<(NodeId, TextRequest)> = Vec::new();
    rt.store.take_text_seeds(&mut seeds);
    let mut out: Vec<(String, TextStyle)> = requests
        .into_iter()
        .map(|(_, r)| (r.text.clone(), r.style.clone()))
        .chain(seeds.into_iter().map(|(_, r)| (r.text, r.style)))
        .filter(|(text, _)| !text.is_empty())
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn title(weight: FontWeight) -> TextStyle {
    TextStyle {
        weight,
        family: FontFamily::Named(Arc::from("Inter")),
        line_height: Some(21.0),
        max_lines: NonZeroU32::new(2),
        overflow: TextOverflow::Ellipsis,
        align: TextAlign::Center,
    }
}

fn button(line_height: Option<f32>) -> TextStyle {
    TextStyle {
        family: FontFamily::Monospace,
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
                ("Go".to_string(), button(None)),
                ("Title".to_string(), title(FontWeight::REGULAR)),
                ("x".to_string(), field),
            ]
        );
        rt.click(0);
        assert_eq!(
            declared(&mut rt),
            [
                ("Go".to_string(), button(Some(28.0))),
                ("Title".to_string(), title(FontWeight::BOLD)),
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
        .find(|(text, _)| text == "Title")
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
