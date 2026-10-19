//! Layout properties bound to values (§U3): a node's size, rules, padding,
//! gaps, arrangement, grid tracks, stack alignment and absolute insets each
//! lay out by the value they read, under hot reload and from the release
//! package, follow a state change and the environment, and lay out again
//! after a reload.

mod support;

use support::{Rt, origin};
use viso_dsl::frontend::compile_file;

const BOUND: &str = r#"
export component Bound {
    state wide = false;
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { wide = !wide; } }
            Text { width: if wide { 200dp } else { 100dp }; height: 4dp; }
            Row {
                padding: if wide { 2sp } else { 4dp };
                gap: if wide { 6dp } else { 0dp };
                Text { width: 10dp; height: 10dp; }
                Text { width: 10dp; height: 10dp; }
            }
            Text { width: 300dp; height: 4dp; max_width: if wide { Option::Some(50%) } else { Option::None }; }
            Grid {
                columns: if wide { [Track::fixed(30dp), Track::fixed(40dp)] } else { [Track::fixed(10dp), Track::fixed(20dp)] };
                Text { height: 10dp; }
                Text { height: 10dp; }
            }
            Stack {
                width: 50dp;
                height: 12dp;
                Text { width: 10dp; height: 10dp; stack.align: if wide { Option::Some(Alignment2D::bottom_end) } else { Option::None }; }
            }
            Absolute {
                width: 100dp;
                height: 12dp;
                Text { width: 10dp; height: 10dp; absolute.top: if wide { Option::Some(10dp) } else { Option::None }; absolute.start: if wide { 50% } else { 0dp }; }
            }
            Row {
                width: 100dp;
                height: 10dp;
                justify: if wide { Justify::end } else { Justify::start };
                Text { width: 10dp; height: 10dp; }
            }
        }
    }
}
"#;

/// What each bound node lays out to: the sized text's width, the padded
/// row's second child's x and its first's y, the bounded text's width, the
/// grid's second cell's x, the stacked and the absolute child's
/// offsets, and the justified child's x.
#[derive(Debug, PartialEq)]
struct Laid {
    sized: f32,
    padded: (f32, f32),
    bounded: f32,
    cell: f32,
    stacked: (f32, f32),
    pinned: (f32, f32),
    justified: f32,
}

fn laid(rt: &Rt) -> Laid {
    let root = rt.root.expect("mounted");
    let nodes = rt.children(root);
    let child = |container: usize, n: usize| {
        let at = rt.store.bounds(nodes[container]);
        let b = rt.store.bounds(rt.children(nodes[container])[n]);
        (b.x - at.x, b.y - at.y, b.w)
    };
    let (x, _, _) = child(2, 1);
    let (_, y, _) = child(2, 0);
    let (cell, _, _) = child(4, 1);
    let (sx, sy, _) = child(5, 0);
    let (ax, ay, _) = child(6, 0);
    Laid {
        sized: rt.store.bounds(nodes[1]).w,
        padded: (x, y),
        bounded: rt.store.bounds(nodes[3]).w,
        cell,
        stacked: (sx, sy),
        pinned: (ax, ay),
        justified: child(7, 0).0,
    }
}

const NARROW: Laid = Laid {
    sized: 100.0,
    padded: (14.0, 4.0),
    bounded: 300.0,
    cell: 10.0,
    stacked: (0.0, 0.0),
    pinned: (0.0, 0.0),
    justified: 0.0,
};

const WIDE: Laid = Laid {
    sized: 200.0,
    padded: (18.0, 2.0),
    bounded: 200.0,
    cell: 30.0,
    stacked: (40.0, 2.0),
    pinned: (50.0, 10.0),
    justified: 90.0,
};

#[test]
fn bound_layout_follows_state_in_every_target() {
    for mut rt in [Rt::mount(BOUND), Rt::packaged(BOUND)] {
        assert_eq!(rt.fault(), None);
        assert_eq!(laid(&rt), NARROW);
        rt.click(0);
        assert_eq!(laid(&rt), WIDE, "a state change lays out again");
        rt.click(0);
        assert_eq!(laid(&rt), NARROW, "and back");
    }
}

#[test]
fn a_bound_length_follows_the_environment() {
    let mut rt = Rt::mount(BOUND);
    rt.click(0);
    rt.update_env(|env| env.text_scale = 2.0);
    rt.frame();
    let laid = laid(&rt);
    assert_eq!(laid.padded, (20.0, 4.0), "2sp at twice the text scale");
}

#[test]
fn a_reload_delivers_the_bound_layout_again() {
    let mut rt = Rt::mount(BOUND);
    rt.click(0);
    rt.reload(&BOUND.replace("200dp", "250dp"));
    rt.frame();
    assert_eq!(
        laid(&rt),
        Laid {
            sized: 250.0,
            ..WIDE
        },
        "the state survives, every value is delivered again"
    );
}

#[test]
fn a_component_root_takes_a_bound_size_from_its_call_site() {
    let source = r#"
component Bar {
    view { Text { height: 4dp; } }
}

export component Sized {
    state wide = false;
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { wide = !wide; } }
            Bar { width: if wide { 80dp } else { 40dp }; }
        }
    }
}
"#;
    for mut rt in [Rt::mount(source), Rt::packaged(source)] {
        let root = rt.root.expect("mounted");
        let bar = rt.children(root)[1];
        assert_eq!(rt.store.bounds(bar).w, 40.0);
        rt.click(0);
        let bar = rt.children(rt.root.expect("mounted"))[1];
        assert_eq!(rt.store.bounds(bar).w, 80.0);
    }
}

#[test]
fn a_sizing_mode_without_layout_faults() {
    let source = r#"
export component Content {
    state wide = false;
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { wide = !wide; } }
            Text { width: if wide { Sizing::min_content } else { Sizing::fixed(10dp) }; height: 4dp; }
        }
    }
}
"#;
    let mut rt = Rt::mount(source);
    assert_eq!(rt.fault(), None);
    rt.click(0);
    let fault = rt.fault().expect("min_content does not lay out yet");
    assert!(fault.to_string().contains("min_content"), "{fault}");
}

#[test]
fn bound_grid_areas_are_not_laid_out_yet() {
    let source = r#"
export component Areas {
    state wide = false;
    view {
        Grid {
            columns: [Track::fr(1.0)];
            areas: if wide { ["a"] } else { ["b"] };
            Text {}
        }
    }
}
"#;
    let codes: Vec<_> = compile_file(source, &origin())
        .diagnostics
        .iter()
        .map(|d| d.code.to_string())
        .collect();
    assert_eq!(codes, ["E3711"]);
}
