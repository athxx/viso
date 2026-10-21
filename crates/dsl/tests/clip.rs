//! A node's `clip` (§U4): its children are seen and hit only inside its box,
//! rounded by its `corner_radius`, until it stops clipping — under hot reload
//! and from the release package.

mod support;

use support::Rt;
use viso_ui::Primitive;

const WINDOW: &str = r#"
export component Window {
    state on = true;
    state taps = 0;
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { on = !on; } }
            Column {
                width: 20dp;
                height: 20dp;
                clip: on;
                corner_radius: 10dp;
                Text { width: 40dp; height: 40dp; on click { taps += 1; } }
            }
        }
    }
}
"#;

/// The rounded clips the scene paints.
fn rounded(rt: &Rt) -> usize {
    let root = rt.root.expect("mounted");
    let mut out = Vec::new();
    viso_ui::paint_tree(&rt.store, root, &mut out);
    out.iter()
        .filter(|p| matches!(p, Primitive::ClipCorners(_)))
        .count()
}

#[test]
fn a_clipping_node_cuts_its_children_to_its_rounded_box_in_every_target() {
    for mut rt in [Rt::mount(WINDOW), Rt::packaged(WINDOW)] {
        assert_eq!(rt.fault(), None);
        rt.relayout();
        assert_eq!(rounded(&rt), 1);

        rt.click_at(10.0, 30.0);
        assert_eq!(rt.int("taps"), Some(1), "inside the box");
        rt.click_at(30.0, 30.0);
        rt.click_at(1.0, 21.0);
        assert_eq!(rt.int("taps"), Some(1), "past the edge and the corner");

        rt.click(0);
        assert_eq!(rounded(&rt), 0);
        rt.click_at(30.0, 30.0);
        rt.click_at(1.0, 21.0);
        assert_eq!(
            rt.int("taps"),
            Some(3),
            "the overflow is hit once unclipped"
        );
        assert_eq!(rt.fault(), None);
    }
}

#[test]
fn a_reload_clips_as_its_source_now_says() {
    let mut rt = Rt::mount(WINDOW);
    rt.relayout();
    let sharp = WINDOW.replace("corner_radius: 10dp;", "");
    rt.reload(&sharp);
    assert_eq!(rounded(&rt), 0, "a sharp clip needs no mask");
    rt.click_at(1.0, 21.0);
    rt.click_at(30.0, 30.0);
    assert_eq!(
        rt.int("taps"),
        Some(1),
        "the corner is hit, the overflow is not"
    );

    rt.reload(&sharp.replace("clip: on;", ""));
    rt.click_at(30.0, 30.0);
    assert_eq!(rt.int("taps"), Some(2), "dropped, the clip is gone");
    assert_eq!(rt.fault(), None);
}
