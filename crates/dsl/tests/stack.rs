//! A `Stack`'s `content_align` and its children's `stack.*`, and an
//! `Absolute`'s children's `absolute.*` insets (§U3.6), from source to layout:
//! each lays out under hot reload and from the release package, a stack paints
//! and hits its children by layer, an inset that reads the environment folds
//! against it at layout, and a reload rewrites the alignments and the insets
//! in place.

mod support;

use support::{Rt, origin};
use viso_dsl::frontend::compile_file;
use viso_ui::Primitive;

fn layers(align: &str, bottom: &str) -> String {
    format!(
        r#"
component Badge {{
    view {{
        Text {{ width: 10dp; height: 10dp; background: #ff0000; }}
    }}
}}

export component Layers {{
    state taps = 0;
    view {{
        Column {{
            Stack {{
                content_align: Alignment2D::center;
                padding: 10dp;
                Text {{ width: 100dp; height: 60dp; background: #00ff00; stack.layer: 1; on click {{ taps += 1; }} }}
                Text {{ width: 40dp; height: 20dp; stack.align: Option::Some(Alignment2D::{align}); }}
                Badge {{ stack.align: Alignment2D::top_end; stack.layer: -1; }}
            }}
            Absolute {{
                width: 200dp;
                height: 100dp;
                Text {{ width: 20dp; height: 10dp; absolute.start: 5dp; absolute.top: 6dp; }}
                Text {{ width: 20dp; height: 10dp; absolute.end: 10%; absolute.bottom: {bottom}; }}
                Text {{ height: 8dp; absolute.start: 10dp; absolute.end: Option::Some(30dp); }}
            }}
        }}
    }}
}}
"#
    )
}

/// Each child's `(x, y, width)` within its container, container by container.
fn placed(rt: &Rt) -> Vec<Vec<(f32, f32, f32)>> {
    let root = rt.root.expect("mounted");
    rt.children(root)
        .into_iter()
        .map(|container| {
            let at = rt.store.bounds(container);
            rt.children(container)
                .into_iter()
                .map(|child| {
                    let b = rt.store.bounds(child);
                    (b.x - at.x, b.y - at.y, b.w)
                })
                .collect()
        })
        .collect()
}

#[test]
fn alignments_layers_and_insets_lay_out_in_every_target() {
    let source = layers("bottom_end", "4dp");
    for rt in [Rt::mount(&source), Rt::packaged(&source)] {
        let root = rt.root.expect("mounted");
        let stack = rt.children(root)[0];
        let size = rt.store.bounds(stack);
        assert_eq!((size.w, size.h), (120.0, 80.0), "the largest, padded");
        let placed = placed(&rt);
        assert_eq!(placed[0][0], (10.0, 10.0, 100.0), "the stack's alignment");
        assert_eq!(placed[0][1], (70.0, 50.0, 40.0), "its own");
        assert_eq!(placed[0][2], (100.0, 10.0, 10.0), "a component's call site");
        assert_eq!(placed[1][0], (5.0, 6.0, 20.0));
        assert_eq!(placed[1][1], (160.0, 86.0, 20.0), "from the end, by 10%");
        assert_eq!(placed[1][2], (10.0, 0.0, 160.0), "a fit pinned to both");

        // The badge, last in source, paints first and is not hit where the
        // layer-1 text covers it.
        let mut out = Vec::new();
        viso_ui::paint_tree(&rt.store, root, &mut out);
        let fills: Vec<_> = out
            .iter()
            .filter_map(|p| match p {
                Primitive::Quad(q) => Some((q.color.r, q.color.g)),
                _ => None,
            })
            .collect();
        assert_eq!(fills, [(1.0, 0.0), (0.0, 1.0)], "by layer");
        let text = rt.children(stack)[0];
        assert_eq!(viso_ui::hit_test(&rt.store, root, 105.0, 15.0), Some(text));
    }
}

#[test]
fn a_reload_rewrites_the_alignments_and_the_insets_in_place() {
    let mut rt = Rt::mount(&layers("bottom_end", "4dp"));
    let root = rt.root;
    rt.reload(&layers("top_start", "1em"));
    assert_eq!(rt.root, root);
    let placed_now = placed(&rt);
    assert_eq!(placed_now[0][1], (10.0, 10.0, 40.0));
    assert_eq!(placed_now[1][1], (160.0, 76.0, 20.0), "1em of 14sp up");
    rt.update_env(|env| env.text_scale = 2.0);
    rt.relayout();
    assert_eq!(
        placed(&rt)[1][1],
        (160.0, 62.0, 20.0),
        "an inset of 1em follows the text scale"
    );
}

/// The errors `view` raises inside a component, code and message.
fn errors(view: &str) -> Vec<(&'static str, String)> {
    let source = format!("export component C {{\n    state n = 0;\n    view {{ {view} }}\n}}\n");
    compile_file(&source, &origin())
        .errors()
        .map(|d| (d.code, d.message.clone()))
        .collect()
}

#[test]
fn a_placement_that_reads_a_value_lays_out() {
    for view in [
        "Stack { content_align: if n > 0 { Alignment2D::top } else { Alignment2D::center }; }",
        "Stack { Text { text: \"a\"; stack.layer: if n > 0 { 1i32 } else { 2i32 }; } }",
        "Absolute { Text { text: \"a\"; absolute.top: if n > 0 { 1dp } else { 2dp }; } }",
    ] {
        assert_eq!(errors(view), [], "{view}");
    }
    assert_eq!(
        errors(
            "Absolute { padding: 4dp; Text { text: \"a\"; absolute.bottom: Option::None; absolute.end: 50% - 2dp; } }"
        ),
        []
    );
}
