//! The layout and presentation values a view's nodes take, from source to the
//! retained node under hot reload and the release package: a flex
//! container's `justify` and `align`, a qualified `Axis`, `Sizing`, a
//! `const` standing for its value, `visible`, `focusable`, and a text's
//! `color`, `font_size` and `soft_wrap`. A binding no path applies yet is
//! `E3711`, never dropped.

mod support;

use support::{Rt, origin};
use viso_dsl::frontend::compile_file;
use viso_ui::{Rgba, Srgb};

const PLACED: &str = r#"
const WIDE: MixedLength = 200dp;
const MIDDLE: Justify = Justify::center;
export component Placed {
    state taps = 0;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Row {
                width: WIDE;
                height: 40dp;
                justify: MIDDLE;
                align: Align::end;
                Text { width: 20dp; height: 10dp; on click { taps += 1; } }
            }
            Flex {
                axis: Axis::column;
                width: Sizing::fixed(100dp);
                height: Sizing::fit;
                Text { width: 10dp; height: 10dp; }
                Text { width: 10dp; height: 10dp; }
            }
            Row {
                width: 400dp;
                height: 20dp;
                Column { width: Sizing::fill { weight: 3.0 }; height: 10dp; }
                Column { width: Sizing::fill; height: 10dp; }
            }
        }
    }
}
"#;

#[test]
fn constant_arrangement_and_sizing_lay_out_in_every_target() {
    for rt in [Rt::mount(PLACED), Rt::packaged(PLACED)] {
        let root = rt.root.expect("mounted");
        let [row, flex, weighted] = rt.children(root)[..] else {
            panic!("three regions");
        };
        let b = |node| rt.store.bounds(node);
        assert_eq!(b(row).w, 200.0, "a `const` width");
        let text = b(rt.children(row)[0]);
        assert_eq!(text.x - b(row).x, 90.0, "centered along the row");
        assert_eq!(text.y - b(row).y, 30.0, "at the end across it");
        let [first, second] = rt.children(flex)[..] else {
            panic!("two texts");
        };
        assert_eq!(b(flex).w, 100.0, "`Sizing::fixed`");
        assert_eq!(b(flex).h, 20.0, "`Sizing::fit` around a column");
        assert_eq!(b(second).y - b(first).y, 10.0, "`Axis::column` stacks");
        let [three, one] = rt.children(weighted)[..] else {
            panic!("two columns");
        };
        assert_eq!(
            (b(three).w, b(one).w),
            (300.0, 100.0),
            "`Sizing::fill` by weights 3 and 1"
        );
    }
}

#[test]
fn a_hot_reload_rearranges_the_kept_container() {
    let mut rt = Rt::mount(PLACED);
    let row = rt.children(rt.root.unwrap())[0];
    rt.reload(&PLACED.replace("Justify::center", "Justify::end"));
    assert_eq!(rt.children(rt.root.unwrap())[0], row, "the row is kept");
    let text = rt.store.bounds(rt.children(row)[0]);
    assert_eq!(text.x - rt.store.bounds(row).x, 180.0);
}

const SHOWN: &str = r#"
export component Shown {
    state gone = false;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Text { width: 20dp; height: 20dp; on click { gone = !gone; } }
            Column {
                width: 100dp;
                height: 20dp;
                visible: !gone;
                focusable: gone;
                semantics.label: Option::Some("panel");
            }
            Text {
                text: "a";
                color: #ff0000;
                font_size: 20sp;
                soft_wrap: true;
            }
        }
    }
}
"#;

#[test]
fn visibility_focus_and_text_style_reach_the_node() {
    for mut rt in [Rt::mount(SHOWN), Rt::packaged(SHOWN)] {
        let root = rt.root.expect("mounted");
        let [_, panel, text] = rt.children(root)[..] else {
            panic!("three nodes");
        };
        assert!(!rt.store.hidden(panel) && !rt.store.focusable(panel));
        let request = rt.store.text_request(text).expect("requested");
        let red: Rgba = Srgb::from_rgba32(0xff00_00ff).into_linear_straight();
        assert_eq!(request.color, red);
        assert_eq!(request.font_size, viso_ui::FontSize::Inherited);
        assert_eq!(rt.store.text_sizing(text, request).font_size, 20.0);
        assert!(request.soft_wrap);
        let read = |rt: &Rt| {
            rt.store
                .derive_semantics(root)
                .nodes
                .iter()
                .any(|n| n.label.as_deref() == Some("panel"))
        };
        assert!(read(&rt));
        rt.click(0);
        assert!(rt.store.hidden(panel), "`visible: false` folds it out");
        assert!(rt.store.focusable(panel));
        assert!(!read(&rt), "and out of the semantics tree");
    }
}

/// The errors `view` raises inside a component, code and message.
fn errors(view: &str) -> Vec<(&'static str, String)> {
    let source = format!(
        "style Dim for Text {{ when disabled {{ opacity: 0.5f32; }} }}\n\
         export component C {{\n    state n = 0;\n    view {{ {view} }}\n}}\n"
    );
    compile_file(&source, &origin())
        .errors()
        .map(|d| (d.code, d.message.clone()))
        .collect()
}

#[test]
fn a_binding_no_path_applies_is_reported() {
    for (view, says) in [
        (
            "TextInput { max_lines: Option::Some(1); }",
            "`max_lines` does not reach a `TextInput` yet",
        ),
        (
            "TextInput { overflow: TextOverflow::ellipsis; }",
            "`overflow` does not reach a `TextInput` yet",
        ),
        (
            "TextInput { invalid: n > 0; }",
            "`invalid` does not reach a `TextInput` yet",
        ),
        (
            "Column { width: Sizing::min_content; }",
            "`Sizing::min_content` does not lay out yet",
        ),
        (
            "Text { text: \"a\"; enabled: n > 0; }",
            "`enabled` does not reach a `Text` yet",
        ),
        (
            "Text { text: \"a\"; clip: true; }",
            "`clip` does not reach a `Text` yet",
        ),
    ] {
        let found = errors(view);
        assert_eq!(found.len(), 1, "{view}: {found:#?}");
        assert_eq!(found[0].0, "E3711", "{view}");
        assert!(found[0].1.contains(says), "{view}: {}", found[0].1);
    }
    assert_eq!(
        errors("Text { text: \"a\"; styles: [Dim]; enabled: n > 0; }"),
        [],
        "a style's selector reads `enabled`"
    );
    for view in [
        "Column { width: if n > 0 { 10dp } else { 20dp }; }",
        "Column { justify: if n > 0 { Justify::start } else { Justify::end }; }",
    ] {
        assert_eq!(
            errors(view),
            [],
            "a layout value lays out as it changes: {view}"
        );
    }
}
