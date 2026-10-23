//! The focus ring driven through the facade: Tab brings it round the focused
//! control in the system's style and accent.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    Appearance, KeyCode as Key, Modifiers as RawModifiers, RawEvent, RawKey, WindowId,
};
use viso::prelude::*;
use viso::render::{Primitive, Quad, Srgb};
use viso::ui::{Axis, Component, FlexStyle, FocusRingStyle, Inset, NodeId, Size, paint_tree};
use viso::widgets::{ButtonStyle, button};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

/// Two buttons in a padded column.
struct Buttons;

impl Application for Buttons {
    fn new(_cx: &mut AppCx) -> Self {
        Buttons
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
                axis: Axis::Column,
                gap: 20.0,
                padding: Inset::all(20.0),
                size: Size::fill(),
                ..FlexStyle::default()
            },
            |cx| {
                for _ in 0..2 {
                    button("Go")
                        .style(ButtonStyle {
                            size: Size::fixed(80.0, 30.0),
                            ..ButtonStyle::default()
                        })
                        .build(cx);
                }
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn tab() -> Vec<RawEvent> {
    let raw = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code: Key::Tab,
            pressed,
            repeat: false,
            modifiers: RawModifiers::default(),
        })
    };
    vec![raw(true), raw(false), redraw()]
}

fn run(steps: &[Vec<RawEvent>], settle: usize) -> DrivenApp<Buttons> {
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.extend(std::iter::repeat_n(redraw(), settle));
    drive_scripted::<Buttons>(script, STEP)
}

/// The buttons, in order.
fn buttons(app: &DrivenApp<Buttons>) -> Vec<NodeId> {
    let store = app.store();
    let column = app.root().unwrap();
    store.arena().children(column).collect()
}

/// The ring strokes the scene draws: the bordered quads that fill nothing.
fn rings(app: &DrivenApp<Buttons>) -> Vec<Quad> {
    let mut out = Vec::new();
    paint_tree(app.store(), app.root().unwrap(), &mut out);
    out.into_iter()
        .filter_map(|p| match p {
            Primitive::Quad(q) if q.border.width > 0.0 && q.color.a == 0.0 => Some(q),
            _ => None,
        })
        .collect()
}

#[test]
fn tab_brings_the_ring_round_the_focused_control() {
    let app = run(&[tab()], 30);
    let first = buttons(&app)[0];
    assert_eq!(app.store().focused(), Some(first));
    let style = app.store().focus_ring_style();
    assert_eq!(style, FocusRingStyle::native(None, false));
    let ring = rings(&app);
    assert!(!ring.is_empty(), "the ring goes round it");
    let world = app.store().world(first);
    assert!(ring[0].rect.x < world.x && ring[0].rect.y < world.y);
    assert_eq!(ring[0].border.color, style.color);
}

#[test]
fn the_ring_takes_the_system_s_accent() {
    let accent = Some([1.0, 0.5, 0.0]);
    let app = run(
        &[
            vec![RawEvent::AppearanceChanged(Appearance {
                accent,
                ..Appearance::default()
            })],
            tab(),
        ],
        30,
    );
    let orange = Srgb {
        r: 1.0,
        g: 0.5,
        b: 0.0,
        a: 1.0,
    }
    .into_linear_straight();
    assert_eq!(
        app.store().focus_ring_style(),
        FocusRingStyle::native(Some(orange), false)
    );
}
