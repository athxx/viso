//! Press feedback: a `ripple` spreads ink from where a press lands, and a
//! control a finger changes plays its haptic, from the dev runtime and the
//! release package alike.

mod support;

use std::time::Duration;

use support::Rt;
use viso_ui::{Haptic, NodeId, PointerPhase, Primitive};

const CONTROLS: &str = r##"
export component Controls {
    state on = false;
    state plain = 0;
    view {
        Column {
            Button { width: 100dp; height: 20dp; ripple: true; text: "Ink"; }
            Toggle { width: 40dp; height: 20dp; bind checked <=> on; }
            Button { width: 100dp; height: 20dp; text: "Plain"; on click { plain += 1; } }
        }
    }
}
"##;

fn nodes(rt: &Rt) -> [NodeId; 3] {
    let root = rt.root.expect("mounted");
    rt.children(root).try_into().expect("three children")
}

fn ink(rt: &Rt) -> usize {
    let mut out = Vec::new();
    viso_ui::paint::paint_tree(&rt.store, rt.root.expect("mounted"), &mut out);
    out.iter()
        .filter(|p| matches!(p, Primitive::AnalyticEllipse(_)))
        .count()
}

#[test]
fn a_rippling_button_spreads_ink_while_pressed_in_every_target() {
    for mut rt in [Rt::mount(CONTROLS), Rt::packaged(CONTROLS)] {
        assert_eq!(rt.fault(), None);
        rt.relayout();
        let [ink_button, _, plain] = nodes(&rt);
        assert!(rt.store.ripples(ink_button));
        assert!(!rt.store.ripples(plain), "off unless the platform's way");

        rt.touch_at(30.0, 10.0, &[PointerPhase::Down]);
        assert!(rt.store.is_rippling());
        rt.store.tick_ripples(Duration::from_millis(100));
        assert_eq!(ink(&rt), 1, "the ink shows while held");

        rt.touch_at(30.0, 10.0, &[PointerPhase::Up]);
        for _ in 0..4 {
            rt.store.tick_ripples(Duration::from_millis(100));
        }
        assert!(!rt.store.is_rippling(), "let go, it fades out");
        assert_eq!(ink(&rt), 0);

        rt.touch_at(30.0, 50.0, &[PointerPhase::Down, PointerPhase::Up]);
        assert!(!rt.store.is_rippling(), "a plain button shows none");
        assert_eq!(rt.fault(), None);
    }
}

#[test]
fn a_finger_feels_a_switch_flip_and_a_mouse_does_not() {
    for mut rt in [Rt::mount(CONTROLS), Rt::packaged(CONTROLS)] {
        rt.relayout();
        rt.click(1);
        assert_eq!(rt.int("on"), Some(1));
        assert_eq!(rt.store.take_haptic(), None, "a mouse plays none");

        rt.touch_at(10.0, 30.0, &[PointerPhase::Down, PointerPhase::Up]);
        rt.frame();
        assert_eq!(rt.int("on"), Some(0));
        assert_eq!(rt.store.take_haptic(), Some(Haptic::Selection));

        rt.touch_at(10.0, 50.0, &[PointerPhase::Down, PointerPhase::Up]);
        rt.frame();
        assert_eq!(rt.int("plain"), Some(1));
        assert_eq!(rt.store.take_haptic(), None, "a button's click plays none");
        assert_eq!(rt.fault(), None);
    }
}
