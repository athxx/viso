//! Size bounds, `margin` and a `%` padding (§U3.3), from source to layout:
//! each reads the parent's content box under hot reload and from the release
//! package, a rule that reads the environment folds against it at layout, and
//! a reload rebinds the rules in place.

mod support;

use support::Rt;

fn boxed(max_width: &str) -> String {
    format!(
        r#"
component Bordered {{
    view {{
        Text {{ width: 300dp; max_width: 120dp; height: 10dp; }}
    }}
}}

export component Boxed {{
    state taps = 0;
    view {{
        Column {{
            Column {{
                width: 400dp;
                height: 300dp;
                Column {{
                    width: 200dp;
                    height: 100dp;
                    padding: 10%;
                    Text {{ width: 10dp; height: 10dp; on click {{ taps += 1; }} }}
                }}
                Text {{ width: 500dp; {max_width} height: 10dp; }}
                Text {{
                    width: 10dp;
                    min_width: 30dp;
                    height: 10dp;
                    margin: EdgeInsets {{ start: 6dp, top: 4dp }};
                }}
                Text {{ width: 10dp; height: 10dp; margin: EdgeInsets {{ start: 1em }}; }}
                Bordered {{ max_width: Option::None; }}
            }}
        }}
    }}
}}
"#
    )
}

/// A child's `x`, `y` and width.
type Placed = (f32, f32, f32);

/// Each child's place, and the padded column's text's `(x, y)`.
fn placed(rt: &Rt) -> (Vec<Placed>, (f32, f32)) {
    let root = rt.root.expect("mounted");
    let children = rt.children(rt.children(root)[0]);
    let at = |node| {
        let b = rt.store.bounds(node);
        (b.x, b.y, b.w)
    };
    let text = rt.store.bounds(rt.children(children[0])[0]);
    (children.into_iter().map(at).collect(), (text.x, text.y))
}

#[test]
fn bounds_margins_and_a_percentage_padding_read_the_parent_content_box_in_every_target() {
    let source = boxed("max_width: 50%;");
    for rt in [Rt::mount(&source), Rt::packaged(&source)] {
        let (children, text) = placed(&rt);
        assert_eq!(text, (40.0, 30.0), "10% of the 400×300 content box");
        assert_eq!(children[1], (0.0, 100.0, 200.0), "at most half of 400dp");
        assert_eq!(
            children[2],
            (6.0, 114.0, 30.0),
            "at least 30dp, past its margin"
        );
        assert_eq!(children[3].0, 14.0, "a margin of 1em at 14sp");
        assert_eq!(
            children[4].2, 300.0,
            "a forwarded `Option::None` unbounds it"
        );
    }
}

#[test]
fn a_reload_rebinds_the_rules_in_place() {
    let mut rt = Rt::mount(&boxed("max_width: 50%;"));
    let root = rt.root;
    rt.reload(&boxed("max_width: 25%;"));
    assert_eq!(rt.root, root);
    assert_eq!(placed(&rt).0[1].2, 100.0);
    rt.reload(&boxed("max_width: 4em;"));
    assert_eq!(placed(&rt).0[1].2, 56.0, "a bound that reads the font size");
    rt.reload(&boxed(""));
    assert_eq!(rt.root, root);
    assert_eq!(placed(&rt).0[1].2, 500.0, "no bound left");
}
