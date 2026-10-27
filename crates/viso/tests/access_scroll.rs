//! An assistive technology scrolls through the facade: a page turned on a
//! 10,000-row virtual list mounts and publishes rows it had not mounted,
//! the list says where it stands and which ways it can still turn, and a
//! row asked into view comes into view.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted_with_caps, semantics_id};
use viso::platform::{
    AccessAction, AccessNode, AccessRequest, PlatformCaps, RawEvent, SemanticsId, SemanticsRole,
    WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, FlexStyle, Sizing, WindowConfig};
use viso::widgets::{VirtualListViewStyle, label, virtual_list};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
const ROWS: usize = 10_000;
const ROW: f32 = 40.0;

/// A 300×390 virtual list of ten thousand 40-tall rows: the tenth shows
/// cut off at its end.
struct Long;

impl Application for Long {
    fn new(_cx: &mut AppCx) -> Self {
        Long
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            size: (300.0, 400.0),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(FlexStyle::default(), |cx| {
            virtual_list(VirtualListViewStyle {
                size: Sizing::fixed(300.0, 390.0),
                estimated_row: ROW,
                ..VirtualListViewStyle::default()
            })
            .items(ROWS, |i, cx| {
                cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        size: Sizing::fixed(300.0, ROW),
                        ..FlexStyle::default()
                    },
                    |cx| {
                        label(format!("Row {i}")).build(cx);
                    },
                );
            })
            .build(cx);
        });
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn access(request: AccessRequest) -> RawEvent {
    RawEvent::Accessibility {
        window: WIN,
        request,
    }
}

fn act(target: SemanticsId, action: AccessAction) -> Vec<RawEvent> {
    vec![
        access(AccessRequest::Action { target, action }),
        redraw(),
        redraw(),
    ]
}

/// The list driven with an assistive technology listening, after `steps`.
fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<Long> {
    let mut script = vec![redraw(), access(AccessRequest::Activated)];
    script.extend(std::iter::repeat_n(redraw(), 6));
    for step in steps {
        script.extend(step.iter().cloned());
    }
    drive_scripted_with_caps::<Long>(script, STEP, PlatformCaps::DESKTOP)
}

fn list(published: &[AccessNode]) -> &AccessNode {
    published
        .iter()
        .find(|n| n.role == SemanticsRole::List)
        .expect("the list is published")
}

/// The 1-based places of the published rows, in order.
fn rows(published: &[AccessNode]) -> Vec<u32> {
    let mut rows: Vec<u32> = published
        .iter()
        .filter(|n| n.role == SemanticsRole::ListItem)
        .filter_map(|n| n.state.position)
        .inspect(|&(_, count)| assert_eq!(count, ROWS as u32))
        .map(|(place, _)| place)
        .collect();
    rows.sort_unstable();
    rows
}

#[test]
fn the_list_says_where_it_stands_and_turns_only_onward_at_its_start() {
    let app = run(&[]);
    let published = app.published().expect("an assistive technology listens");
    let list = list(&published);
    let y = list.state.scroll_y.expect("the list scrolls up and down");
    assert_eq!((y.at, y.least), (0.0, 0.0));
    assert!(y.most > 399_000.0, "{y:?}");
    assert!(list.state.scroll_x.is_none());
    assert!(list.actions.contains(AccessAction::ScrollDown));
    assert!(!list.actions.contains(AccessAction::ScrollUp));
    let shown = rows(&published);
    assert!(
        shown.starts_with(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]),
        "{shown:?}"
    );
    assert!(
        shown.len() < 40,
        "only a window of rows mounts: {}",
        shown.len()
    );
    let row = published
        .iter()
        .find(|n| n.role == SemanticsRole::ListItem)
        .unwrap();
    assert!(row.actions.contains(AccessAction::ScrollIntoView));
}

#[test]
fn a_page_turned_by_the_technology_reaches_rows_never_mounted() {
    let before = rows(&run(&[]).published().unwrap());
    let last = *before.last().unwrap();
    let target = list(&run(&[]).published().unwrap()).id;
    let pages: Vec<Vec<RawEvent>> = (0..3)
        .map(|_| act(target, AccessAction::ScrollDown))
        .collect();
    let app = run(&pages);
    let published = app.published().unwrap();
    let after = rows(&published);
    assert!(
        after.iter().any(|&place| place > last),
        "rows past {last} are published: {after:?}"
    );
    assert!(
        after.len() < 40,
        "the window stays a window: {}",
        after.len()
    );
    let list = list(&published);
    let y = list.state.scroll_y.unwrap();
    assert!(y.at > 3.0 * 300.0, "three pages down: {y:?}");
    assert!(list.actions.contains(AccessAction::ScrollUp));
    // The rows it shows are the ones its offset reaches.
    let top = (y.at / ROW) as u32;
    assert!(
        after.contains(&(top + 1)),
        "row {} shows: {after:?}",
        top + 1
    );

    // And a page back up turns back.
    let mut there_and_back = pages.clone();
    there_and_back.push(act(target, AccessAction::ScrollUp));
    let back = run(&there_and_back).published().unwrap();
    assert!(self::list(&back).state.scroll_y.unwrap().at < y.at);
}

#[test]
fn a_row_asked_into_view_comes_into_view() {
    let app = run(&[]);
    let published = app.published().unwrap();
    let view = list(&published).bounds;
    // A row cut off by the viewport's end.
    let below = published
        .iter()
        .filter(|n| n.role == SemanticsRole::ListItem)
        .find(|n| n.bounds.y + n.bounds.height > view.y + view.height)
        .expect("a row reaches past the viewport's end");
    let app = run(&[act(below.id, AccessAction::ScrollIntoView)]);
    let published = app.published().unwrap();
    let row = published
        .iter()
        .find(|n| n.id == below.id)
        .expect("the row stays");
    assert!(
        row.bounds.y + row.bounds.height <= view.y + view.height + 0.5 && row.bounds.y >= view.y,
        "{:?} within {view:?}",
        row.bounds
    );
    assert!(list(&published).state.scroll_y.unwrap().at > 0.0);
    // The published id is the one the facade gives the node.
    let root = app.root().unwrap();
    assert!(published.iter().any(|n| n.id == semantics_id(root)));
}

#[test]
fn the_first_frame_mounts_every_row_the_viewport_covers() {
    let app = drive_scripted_with_caps::<Long>(vec![redraw()], STEP, PlatformCaps::DESKTOP);
    let store = app.store();
    let mut places = Vec::new();
    let mut stack = vec![app.root().unwrap()];
    while let Some(n) = stack.pop() {
        if let Some((place, _)) = store.semantic_state(n).and_then(|s| s.position) {
            places.push(place);
        }
        stack.extend(store.arena().children(n));
    }
    places.sort_unstable();
    // 390 points of 40-point rows: ten rows, the tenth cut off.
    assert!(
        places.starts_with(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]),
        "{places:?}"
    );
}
