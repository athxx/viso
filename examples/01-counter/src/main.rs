//! The first interactive Viso app: a counter.
//!
//! Clicking the button increments a reactive state cell. The cell is bound to
//! one node, a "bar" leaf, so a click drives exactly a targeted update — no
//! full-tree rebuild: the bar carries a `Label` role, bound `SEMANTICS`, so the
//! count is exposed to assistive technology and re-derived when it changes, and
//! bound `PAINT`, a visual proxy that repaints on each change. Everything below
//! is the prelude.

use viso::prelude::*;

struct Counter {
    // Allocated in `build`, where the state store is reachable — not in `new`
    // (the `AppCx` marker cannot borrow the store). `None` until `build` runs;
    // the handlers below capture the cell directly, so they never read this.
    count: Option<State<i32>>,
}

impl Application for Counter {
    fn new(_cx: &mut AppCx) -> Self {
        Counter { count: None }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let count = cx.state_of(0);
        self.count = Some(count);

        cx.flex(row_style(), |cx| {
            // The button: clicking it increments the count.
            cx.component(
                &button("Count")
                    .size(Sizing::fixed(96.0, 48.0))
                    .on_click(move |ev| {
                        count.update(ev, |n| n + 1);
                    }),
            );

            // The bar carries the count as an accessible label, re-derived
            // when it changes, and repaints with it.
            let bar = cx.leaf(bar_style());
            let bar = cx.semantics(bar, Semantics::role(Role::Label).with_label("Count"));
            cx.bind(count, bar, DirtyClass::SEMANTICS);
            cx.bind(count, bar, DirtyClass::PAINT);
        });
    }
}

/// The container: a padded, cross-centered Row filling the window.
fn row_style() -> FlexStyle {
    FlexStyle {
        axis: Axis::Row,
        gap: 12.0,
        padding: Inset::all(16.0),
        size: Sizing::fill(),
        style: BoxStyle::solid(Rgba {
            r: 0.12,
            g: 0.13,
            b: 0.16,
            a: 1.0,
        }),
        ..Default::default()
    }
}

/// The count's visual proxy: a bar that repaints when the count changes.
fn bar_style() -> LeafStyle {
    LeafStyle {
        size: Sizing::fixed(160.0, 24.0),
        style: BoxStyle::solid(Rgba {
            r: 0.15,
            g: 0.75,
            b: 0.45,
            a: 1.0,
        })
        .with_radius(4.0),
    }
}

fn main() {
    viso::run::<Counter>();
}
