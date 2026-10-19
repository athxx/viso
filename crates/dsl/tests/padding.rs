//! A container's `padding` (§U3.3), from source to layout: every form of a
//! constant `EdgeInsets` pads the content under hot reload and the release
//! package, and a padding the runtime cannot lay out is a diagnostic, never
//! dropped.

mod support;

use support::{Rt, origin};

const PADDED: &str = r#"
component Padded {
    state taps = 0;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            padding: 8dp;
            Column {
                width: 100dp;
                height: 40dp;
                padding: EdgeInsets::axes(inline: 6dp, block: 2dp);
                Text { width: 10dp; height: 10dp; on click { taps += 1; } }
            }
            Row {
                height: 40dp;
                padding: EdgeInsets { start: 1em, bottom: 4dp };
                Text { width: 10dp; height: 10dp; }
            }
            Stack {
                height: 40dp;
                padding: EdgeInsets::all(3dp);
                Text { width: 10dp; height: 10dp; }
            }
        }
    }
}
"#;

#[test]
fn a_constant_padding_pads_the_content_in_every_target() {
    for rt in [Rt::mount(PADDED), Rt::packaged(PADDED)] {
        let root = rt.root.expect("mounted");
        let regions = rt.children(root);
        let at = |node| {
            let b = rt.store.bounds(node);
            (b.x, b.y)
        };
        let text_in = |region| at(rt.children(region)[0]);
        assert_eq!(at(regions[0]), (8.0, 8.0));
        assert_eq!(text_in(regions[0]), (14.0, 10.0), "inline 6dp, block 2dp");
        let row = at(regions[1]);
        assert_eq!(
            text_in(regions[1]),
            (row.0 + 14.0, row.1),
            "start 1em of 14sp"
        );
        let stack = at(regions[2]);
        assert_eq!(text_in(regions[2]), (stack.0 + 3.0, stack.1 + 3.0));
    }
}

fn errors(source: &str) -> Vec<(String, String)> {
    viso_dsl::frontend::compile_file(source, &origin())
        .errors()
        .map(|d| (d.code.to_string(), d.message.clone()))
        .collect()
}

#[test]
fn a_padding_the_runtime_cannot_lay_out_is_reported() {
    let view = |node: &str| {
        format!("export component A {{ state wide = false; view {{ Column {{ {node} }} }} }}")
    };
    for (node, says) in [
        (
            "Column { padding: if wide { 8dp } else { 4dp }; }",
            "reads a value",
        ),
        ("Text { padding: 4dp; }", "no content box"),
        (
            "Column { margin: if wide { 8dp } else { 4dp }; }",
            "a `margin` that reads a value",
        ),
        (
            "Column { max_width: if wide { 8dp } else { 4dp }; }",
            "a `max_width` that reads a value",
        ),
    ] {
        let errors = errors(&view(node));
        assert!(
            matches!(errors.as_slice(), [(code, message)] if code == "E3711" && message.contains(says)),
            "{node}: {errors:?}"
        );
    }
    for node in [
        "Column { padding: 4dp + 2sp; }",
        "Column { padding: EdgeInsets::zero(); }",
        "Column { padding: EdgeInsets { top: 1dp }; }",
        "Column { padding: 10%; }",
        "Column { padding: EdgeInsets::axes(inline: 5% + 2dp, block: 1sp); }",
        "Column { margin: -4dp; }",
        "Text { text: \"a\"; margin: EdgeInsets { start: 1sp }; min_width: 10%; }",
        "Column { min_height: 20dp; max_height: Option::None; max_width: 640dp; }",
    ] {
        assert_eq!(errors(&view(node)), [], "{node}");
    }
    let errors = errors(&view("Column { padding: true; }"));
    assert!(
        errors
            .iter()
            .any(|(code, message)| code == "E2103" && message.contains("`EdgeInsets`")),
        "{errors:?}"
    );
}
