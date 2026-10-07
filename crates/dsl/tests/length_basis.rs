//! The `em` and `%` basis of a length (§19.4), from source to layout: in
//! `font_size` they read the parent's resolved font size, in every other
//! property the node's own — including a `font_size` the same node sets.

mod support;

use support::Rt;

#[test]
fn em_reads_the_parent_font_in_font_size_and_the_own_font_elsewhere() {
    let rt = Rt::mount(
        r#"
component Lengths {
    state taps = 0;
    view {
        Column {
            width: 400dp;
            height: 100dp;
            Text { font_size: 20dp; width: 2em; height: 10dp; on click { taps += 1; } }
            Text { width: 2em; height: 11dp; }
            Text { font_size: 2em; width: 1em; height: 12dp; }
            Text { font_size: 150%; width: 2em; height: 13dp; }
        }
    }
}
"#,
    );
    let width = |height: f32| {
        let at = rt
            .nodes
            .iter()
            .map(|&(_, id)| rt.store.bounds(id))
            .find(|b| b.h == height)
            .unwrap_or_else(|| panic!("no node {height} high"));
        at.w
    };
    // Its own 20dp font: 2em is 40.
    assert_eq!(width(10.0), 40.0);
    // No font of its own: the root's 14sp.
    assert_eq!(width(11.0), 28.0);
    // `font_size: 2em` doubles the parent's 14 to 28; `width: 1em` reads 28.
    assert_eq!(width(12.0), 28.0);
    // `150%` of the parent's 14 is 21; `2em` of that is 42.
    assert_eq!(width(13.0), 42.0);
}
