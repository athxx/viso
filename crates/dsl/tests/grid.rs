//! A `Grid`'s tracks, gaps, cell alignment, adaptive columns and template
//! areas, and its children's `grid.*` cells, from source to layout:
//! each lays out under hot reload and from the release package, a gap that
//! reads the environment folds against it at layout, and a reload rewrites
//! the template and the cells in place.

mod support;

use support::{Rt, origin};
use viso_dsl::frontend::compile_file;

fn board(columns: &str, area: &str) -> String {
    format!(
        r#"
const SIDE: Track = Track::fixed(100dp);

component Tile {{
    view {{
        Text {{ width: 10dp; height: 10dp; }}
    }}
}}

export component Board {{
    state taps = 0;
    view {{
        Column {{
            Grid {{
                width: 300dp;
                height: 200dp;
                columns: {columns};
                rows: tracks(2, Track::fixed(40dp));
                column_gap: 10dp;
                row_gap: 1em;
                align_items: GridAlign::center;
                areas: ["head head head", "side main main", "side main main"];
                Text {{ width: 10dp; height: 10dp; grid.area: "{area}"; on click {{ taps += 1; }} }}
                Text {{ width: 10dp; height: 10dp; grid.column: 0; grid.row: Option::Some(2); }}
                Tile {{ grid.column: 2; }}
            }}
            Grid {{
                width: 300dp;
                height: 100dp;
                adaptive_columns: AdaptiveColumns {{ min: 90dp }};
                auto_rows: Track::fixed(30dp);
                Text {{ width: 10dp; height: 10dp; }}
                Text {{ width: 10dp; height: 10dp; }}
                Text {{ width: 10dp; height: 10dp; }}
                Text {{ width: 10dp; height: 10dp; }}
            }}
        }}
    }}
}}
"#
    )
}

/// Each child's `(x, y)` within its grid, grid by grid.
fn cells(rt: &Rt) -> Vec<Vec<(f32, f32)>> {
    let root = rt.root.expect("mounted");
    rt.children(root)
        .into_iter()
        .map(|grid| {
            let at = rt.store.bounds(grid);
            rt.children(grid)
                .into_iter()
                .map(|child| {
                    let b = rt.store.bounds(child);
                    (b.x - at.x, b.y - at.y)
                })
                .collect()
        })
        .collect()
}

const COLUMNS: &str = "[SIDE, Track::fr(1.0), Track::fr(2.0)]";

#[test]
fn tracks_gaps_areas_and_cells_lay_out_in_every_target() {
    let source = board(COLUMNS, "main");
    for rt in [Rt::mount(&source), Rt::packaged(&source)] {
        let cells = cells(&rt);
        // Columns 100, 60 and 120 apart by 10; rows 40, 40 and the 10 of the
        // implicit third, apart by 1em (14).
        assert_eq!(
            cells[0][0],
            (110.0, 81.0),
            "`main` from column 1 over two rows, centered"
        );
        assert_eq!(cells[0][1], (0.0, 108.0), "an implicit third row");
        assert_eq!(
            cells[0][2],
            (180.0, 15.0),
            "a component placed by its call site"
        );
        assert_eq!(
            cells[1],
            [(0.0, 0.0), (100.0, 0.0), (200.0, 0.0), (0.0, 30.0)],
            "three adaptive columns over 300dp, an auto row of 30dp"
        );
    }
}

#[test]
fn a_reload_rewrites_the_template_and_the_cells_in_place() {
    let mut rt = Rt::mount(&board(COLUMNS, "main"));
    let root = rt.root;
    rt.reload(&board("tracks(4, Track::fr(1.0))", "head"));
    assert_eq!(rt.root, root);
    let cells = cells(&rt);
    // Four columns of 67.5 apart by 10.
    assert_eq!(cells[0][0], (0.0, 15.0), "`head` from column 0, row 0");
    assert_eq!(
        cells[0][2],
        (155.0, 69.0),
        "column 2 flows past the full first row"
    );
    rt.update_env(|env| env.text_scale = 2.0);
    rt.relayout();
    assert_eq!(
        cells_of(&rt, 1),
        (0.0, 108.0 + 14.0 * 2.0),
        "a row gap of 1em follows the text scale"
    );
}

/// A grid of a `dp + %` column, an `em` column and a share, apart by a `%`
/// gap, its rows a `sp` one and a `%` one apart by a `%` gap.
const LENGTHS: &str = r#"
export component Lengths {
    state taps = 0;
    view {
        Column {
            Grid {
                width: 400dp;
                height: 200dp;
                columns: [Track::fixed(10dp + 25%), Track::fixed(2em), Track::fr(1.0)];
                rows: [Track::fixed(20sp), Track::fixed(10% + 5dp)];
                column_gap: 5%;
                row_gap: 10%;
                Text { width: 1dp; height: 1dp; on click { taps += 1; } }
                Text { width: 1dp; height: 1dp; }
                Text { width: 1dp; height: 1dp; }
                Text { width: 1dp; height: 1dp; }
            }
        }
    }
}
"#;

#[test]
fn tracks_and_gaps_of_any_length_fold_and_follow_the_environment() {
    for mut rt in [Rt::mount(LENGTHS), Rt::packaged(LENGTHS)] {
        // Columns 110 (10 + 25% of 400) and 28 (2em of 14) apart by 20 (5%);
        // rows 20 and 25 (10% of 200, plus 5) apart by 20 (10% of 200).
        assert_eq!(
            cells(&rt)[0],
            [(0.0, 0.0), (130.0, 0.0), (178.0, 0.0), (0.0, 40.0)]
        );
        rt.update_env(|env| env.text_scale = 2.0);
        rt.relayout();
        // The em column and the sp row double.
        assert_eq!(
            cells(&rt)[0],
            [(0.0, 0.0), (130.0, 0.0), (206.0, 0.0), (0.0, 60.0)]
        );
    }
}

/// Grid 0's child `child`'s `(x, y)`.
fn cells_of(rt: &Rt, child: usize) -> (f32, f32) {
    cells(rt)[0][child]
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
fn a_template_or_cell_layout_does_not_take_is_reported() {
    for (view, code, says) in [
        (
            "Grid { areas: if n > 0 { [\"a\"] } else { [\"b\"] }; }",
            "E3711",
            "a `areas` that reads a value does not lay out yet",
        ),
        (
            "Grid { areas: [\"a b\", \"b a\"]; }",
            "E3108",
            "the area `a` of `areas` is not a rectangle",
        ),
        (
            "Grid { areas: [\"a a\", \"b\"]; }",
            "E3108",
            "every row of `areas` names the same number of cells",
        ),
        (
            "Grid { areas: [\"a\"]; Text { text: \"x\"; grid.area: \"b\"; } }",
            "E3108",
            "`b` names no area of this grid's `areas`",
        ),
        (
            "Grid { if n > 0 { Text { text: \"x\"; grid.area: \"b\"; } } }",
            "E3108",
            "this grid declares no `areas`",
        ),
        (
            "Grid { areas: [\"a\"]; Text { text: \"x\"; grid.area: \"a\"; grid.column: 0; } }",
            "E3108",
            "a `grid.area` places the node by itself",
        ),
        (
            "Grid { Text { text: \"x\"; grid.row_span: 0; } }",
            "E3108",
            "a `grid.row_span` spans at least one track",
        ),
        (
            "Grid { columns: tracks(3); }",
            "E2103",
            "`tracks` takes a count and a track",
        ),
    ] {
        let found = errors(view);
        assert_eq!(found.len(), 1, "{view}: {found:#?}");
        assert_eq!(found[0].0, code, "{view}");
        assert!(found[0].1.contains(says), "{view}: {}", found[0].1);
    }
    assert_eq!(
        errors(
            "Grid { columns: tracks(2, Track::minmax(40dp, TrackMax::fr(1.0))); \
             rows: [Track::fit_content(80dp), Track::fixed(25%)]; \
             adaptive_columns: Option::Some(AdaptiveColumns { mode: AutoRepeat::fit, min: 60dp, max: TrackMax::length(120dp) }); }"
        ),
        []
    );
}
