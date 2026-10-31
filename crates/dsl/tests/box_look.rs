//! A box's `corner_radius`, `border` and `shadow`: constant or reading state, a
//! `%` radius a share of the shorter side of the box as it paints, an `sp` one
//! following the text scale, and a style's `when` switching both, under hot
//! reload and from the release package.

mod support;

use support::Rt;
use viso_ui::{
    Border, LengthEnv, NodeId, PointerButtons, PointerEvent, PointerPhase, PointerRouter, Rgba,
    Shadow, Srgb, Vec2,
};

const CARD: &str = r##"
style Ring for Column {
    border: Option::Some(Border { width: 1dp, color: #00ff00 });
    when hover {
        corner_radius: 6dp;
        border: Option::Some(Border { width: 3dp, color: #0000ff });
        shadow: Shadow { y: 1dp, blur: 4dp, color: #000000, inset: true };
    }
}

export component Card {
    state round = false;
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { round = !round; } }
            Column {
                width: 100dp;
                height: 40dp;
                background: #ff0000;
                corner_radius: if round { 50% } else { 4dp };
                border: if round { Option::Some(Border { width: 2dp, color: #000000 }) } else { Option::None };
                shadow: if round { [Shadow { x: 1dp, y: 4dp, blur: 12dp, spread: -2dp, color: #00000040 }] } else { [] };
            }
            Column { width: 10dp; height: 10dp; corner_radius: 2sp; }
            Column { width: 20dp; height: 20dp; styles: [Ring]; }
        }
    }
}
"##;

fn rgba(srgb: u32) -> Rgba {
    Srgb::from_rgba32(srgb).into_linear_straight()
}

/// The root's children: the toggle, the card, the scaled box and the
/// styled one.
fn nodes(rt: &Rt) -> [NodeId; 4] {
    let root = rt.root.expect("mounted");
    rt.children(root).try_into().expect("four children")
}

/// A node's radius in dp, its share of the shorter side, and its border.
fn shape(rt: &Rt, node: NodeId) -> (f32, f32, Border) {
    let style = rt.store.style(node);
    (style.radius, style.radius_pct, style.border)
}

fn border(width: f32, srgb: u32) -> Border {
    Border {
        width,
        color: rgba(srgb),
    }
}

/// A pointer moving to the middle of the styled box, then a frame.
fn hover(rt: &mut Rt) {
    let root = rt.root.expect("mounted");
    let event = PointerEvent {
        x: 5.0,
        y: 80.0,
        phase: PointerPhase::Move,
        buttons: PointerButtons::NONE,
        modifiers: Default::default(),
    };
    PointerRouter::route(
        &mut rt.store,
        &mut rt.states,
        &rt.bindings,
        root,
        event,
        &mut Vec::new(),
    );
    rt.frame();
}

#[test]
fn a_box_rounds_and_strokes_as_its_values_say_in_every_target() {
    for mut rt in [Rt::mount(CARD), Rt::packaged(CARD)] {
        assert_eq!(rt.fault(), None);
        rt.relayout();
        let [_, card, scaled, styled] = nodes(&rt);
        assert_eq!(shape(&rt, card), (4.0, 0.0, Border::NONE));
        assert_eq!(rt.store.shadow(card), None);
        assert_eq!(shape(&rt, scaled).0, 2.0);
        assert_eq!(shape(&rt, styled), (0.0, 0.0, border(1.0, 0x00ff00ff)));

        rt.click(0);
        assert_eq!(shape(&rt, card), (0.0, 0.5, border(2.0, 0x000000ff)));
        assert_eq!(
            rt.store.shadow(card),
            Some(Shadow {
                color: rgba(0x00000040),
                offset: Vec2 { x: 1.0, y: 4.0 },
                blur: 12.0,
                spread: -2.0,
                inset: false,
            })
        );

        hover(&mut rt);
        assert_eq!(
            shape(&rt, styled),
            (6.0, 0.0, border(3.0, 0x0000ffff)),
            "the `when` switches both"
        );
        assert_eq!(
            rt.store.shadow(styled),
            Some(Shadow::below(rgba(0x000000ff), 1.0, 4.0).inside())
        );
        assert_eq!(rt.fault(), None);
    }
}

#[test]
fn an_sp_radius_follows_the_text_scale_and_a_reload_reshapes_the_box() {
    let mut rt = Rt::mount(CARD);
    rt.relayout();
    let [_, card, scaled, _] = nodes(&rt);
    rt.store.set_length_env(LengthEnv {
        text_scale: 2.0,
        ..rt.store.length_env()
    });
    rt.relayout();
    assert_eq!(shape(&rt, scaled).0, 4.0);

    rt.reload(&CARD.replace("{ 4dp }", "{ 8dp }"));
    let [_, card_again, ..] = nodes(&rt);
    assert_eq!(card_again, card, "kept");
    assert_eq!(shape(&rt, card).0, 8.0);
}
