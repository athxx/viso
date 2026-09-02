//! The grid frame seam: a grid authored through the facade `BuildCx`, laid out
//! headlessly against a synthetic surface — the same way the flex/scroll facade
//! tests drive the ui stores directly through `viso::ui`, no window or GPU.

use viso::prelude::*;
use viso::render::Rect;
use viso::ui::{BuildCx, LeafStyle, NodeStore, Size};

#[test]
fn a_facade_built_grid_lays_children_into_a_two_by_two() {
    let mut store = NodeStore::new();
    let mut ids = Vec::new();
    let grid = {
        let mut cx = BuildCx::new(&mut store);
        cx.grid(
            GridStyle {
                columns: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
                rows: vec![TrackSizing::Fr(1.0), TrackSizing::Fr(1.0)],
                size: Size::fixed(200.0, 200.0),
                ..Default::default()
            },
            |cx| {
                for _ in 0..4 {
                    ids.push(
                        cx.leaf(LeafStyle {
                            size: Size::fill(),
                            ..Default::default()
                        })
                        .id(),
                    );
                }
            },
        )
        .id()
    };
    let mut scratch = Vec::new();
    store.layout(
        grid,
        Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 200.0,
        },
        &mut scratch,
    );
    assert_eq!(
        store.bounds(ids[0]),
        Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0
        }
    );
    assert_eq!(
        store.bounds(ids[3]),
        Rect {
            x: 100.0,
            y: 100.0,
            w: 100.0,
            h: 100.0
        }
    );
}
