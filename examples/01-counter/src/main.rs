//! The first interactive Viso app: a counter.
//!
//! Clicking the button increments a reactive state cell. The cell is bound to
//! two nodes, so a click drives exactly a targeted update — no full-tree rebuild:
//!
//! - the button carries a `Label` role, bound `SEMANTICS`, so the count is
//!   exposed to assistive technology and re-derived when it changes;
//! - a "bar" leaf is bound `PAINT`, a visual proxy that repaints on each change.
//!
//! The count's label is accessible semantics plus a paint proxy, not rendered
//! text: there is no text control yet, so nothing here fakes glyphs. When a text
//! control lands, the label becomes real text (gaining measure/layout/paint on
//! top of semantics). Everything below is the public facade — no internal crates.

use viso::prelude::*;
use viso::render::Rgba;
use viso::ui::{Axis, BoxStyle, Inset, Size};

struct Counter {
    // Allocated in `build`, where the state store is reachable — not in `new`
    // (the `AppCx` marker cannot borrow the store). `None` until `build` runs;
    // the handlers below capture the `StateId` directly, so they never read this.
    count: Option<StateId>,
}

impl Application for Counter {
    fn new(_cx: &mut AppCx) -> Self {
        Counter { count: None }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let count = cx.state(StateValue::Int(0));
        self.count = Some(count);

        cx.flex(row_style(), |cx| {
            // The button: clicking it increments the count.
            let button = cx.leaf(button_style());
            let button = cx.on_pointer(button, move |ev| {
                let now = match ev.get(count) {
                    Some(StateValue::Int(n)) => n,
                    _ => 0,
                };
                ev.set(count, StateValue::Int(now + 1));
            });
            // The button carries the count as an accessible label; bound
            // SEMANTICS so a change re-derives just this node's semantics.
            let button = cx.semantics(button, Semantics::role(Role::Label).with_label("Count"));
            cx.bind(count, button, DirtyClass::SEMANTICS);

            // The visual proxy: a bar that repaints when the count changes.
            let bar = cx.leaf(bar_style());
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
        size: Size::fill(),
        style: BoxStyle::solid(Rgba {
            r: 0.12,
            g: 0.13,
            b: 0.16,
            a: 1.0,
        }),
        ..Default::default()
    }
}

/// The button: a fixed, rounded, clickable box.
fn button_style() -> LeafStyle {
    LeafStyle {
        size: Size::fixed(96.0, 48.0),
        style: BoxStyle::solid(Rgba {
            r: 0.20,
            g: 0.45,
            b: 0.95,
            a: 1.0,
        })
        .with_radius(8.0),
    }
}

/// The count's visual proxy: a bar that repaints when the count changes.
fn bar_style() -> LeafStyle {
    LeafStyle {
        size: Size::fixed(160.0, 24.0),
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
