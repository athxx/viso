//! A window whose tree outgrows its node budget, as an embedded profile
//! fixes one: the store is built at that capacity, the nodes past it are
//! refused rather than the store growing or the app panicking, and the
//! window lays out and draws what fit.
//!
//! The budget is the session's, fixed once a process, so this file holds
//! one test.

use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{RawEvent, WindowId};
use viso::prelude::*;
use viso::ui::FlexStyle;
use viso::{MemoryBudget, widgets::text_input};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
const NODES: u32 = 48;

/// More rows of controls than the budget holds.
struct Crowded;

impl Application for Crowded {
    fn new(_cx: &mut AppCx) -> Self {
        Crowded
    }

    fn memory_budget() -> MemoryBudget {
        MemoryBudget {
            nodes: Some(NODES),
            ..MemoryBudget::of_build()
        }
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(
            FlexStyle {
                size: Sizing::fill(),
                axis: Axis::Column,
                ..FlexStyle::default()
            },
            |cx| {
                for row in 0..40 {
                    label(format!("Row {row}")).build(cx);
                    button("Open").build(cx);
                    text_input("Note").build(cx);
                }
            },
        );
    }
}

#[test]
fn a_tree_past_the_node_budget_keeps_what_fit_and_runs_on() {
    let script = vec![RawEvent::RedrawRequested { window: WIN }; 3];
    let app = drive_scripted::<Crowded>(script, STEP);
    let store = app.store();
    assert_eq!(store.arena().fixed_capacity(), Some(NODES));
    assert_eq!(
        store.arena().live_count(),
        NODES as usize,
        "the store is full"
    );
    let root = app.root().expect("the root fit");
    assert!(
        store.arena().children(root).count() > 0,
        "the first rows fit"
    );
    assert!(
        store.bounds(root).w > 0.0,
        "what fit is laid out: {:?}",
        store.bounds(root)
    );
}
