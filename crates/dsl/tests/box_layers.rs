//! A box's look in layers: a background of colors, gradients and images, any
//! number of shadows, a text's shadows and a button's or field's padding,
//! from the dev runtime and the release package alike.

mod support;

use support::Rt;
use viso_ui::background::FillLayer;
use viso_ui::{NodeId, Primitive};

const CARDS: &str = r##"
export component Cards {
    view {
        Column {
            Row {
                width: 100dp; height: 40dp; corner_radius: 8dp
                background: [#ffffff, Fill::linear { angle: 90deg, colors: [#ff0000, #0000ff] }]
                shadow: [Shadow { y: 1dp, blur: 2dp, color: #00000040 },
                         Shadow { blur: 4dp, color: #00000020, inset: true }]
            }
            Row { width: 100dp; height: 20dp; background: #ff0000 }
            Row {
                width: 100dp; height: 20dp
                background: Fill::radial { x: 0.0, colors: [#ffffff] }
                shadow: Shadow { color: #000000 }
            }
            Text("Lit") { text_shadow: TextShadow { color: #00000080 } }
            Button("Go") { padding: EdgeInsets { start: 12dp, top: 6dp, end: 12dp, bottom: 6dp } }
            TextInput { padding: 4dp }
        }
    }
}
"##;

fn rows(rt: &Rt) -> Vec<NodeId> {
    rt.children(rt.root.expect("mounted"))
}

#[test]
fn a_box_paints_its_layers_and_casts_its_shadows_in_every_target() {
    for mut rt in [Rt::mount(CARDS), Rt::packaged(CARDS)] {
        assert_eq!(rt.fault(), None);
        rt.relayout();
        let rows = rows(&rt);
        // The first color is the fill; the gradient paints over it.
        let card = rows[0];
        assert_eq!(rt.store.style(card).fill.a, 1.0);
        let [FillLayer::Linear { angle, stops }] = rt.store.background_layers(card) else {
            panic!("{:?}", rt.store.background_layers(card));
        };
        assert!((angle - core::f32::consts::FRAC_PI_2).abs() < 1e-4);
        assert_eq!(stops.len(), 2);
        let shadows = rt.store.shadows(card);
        assert_eq!(shadows.len(), 2);
        assert!(!shadows[0].inset && shadows[1].inset);

        let mut out = Vec::new();
        viso_ui::paint::paint_tree(&rt.store, card, &mut out);
        let kinds: Vec<&str> = out
            .iter()
            .map(|p| match p {
                Primitive::AnalyticShadow(s) if s.inner => "inset",
                Primitive::AnalyticShadow(_) => "outer",
                Primitive::Gradient(_) => "gradient",
                Primitive::Quad(_) => "box",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["outer", "box", "gradient", "inset"], "{out:?}");

        // A lone color, a lone fill and a lone shadow stand for lists of one.
        assert!(rt.store.background_layers(rows[1]).is_empty());
        assert!(matches!(
            rt.store.background_layers(rows[2]),
            [FillLayer::Radial { .. }]
        ));
        assert_eq!(rt.store.shadows(rows[2]).len(), 1);
        assert_eq!(rt.store.text_shadows(rows[3]).len(), 1);
    }
}

#[test]
fn a_button_and_a_field_keep_their_text_from_their_edges() {
    for mut rt in [Rt::mount(CARDS), Rt::packaged(CARDS)] {
        assert_eq!(rt.fault(), None);
        rt.relayout();
        let rows = rows(&rt);
        let (button, field) = (rows[4], rows[5]);
        let inset = rt.store.content_inset(button);
        assert_eq!(
            (inset.left, inset.top, inset.right, inset.bottom),
            (12.0, 6.0, 12.0, 6.0)
        );
        let world = rt.store.world(button);
        assert!(world.w >= 24.0 && world.h >= 12.0, "{world:?}");
        let inner = rt.store.content_box(button);
        assert_eq!((inner.x - world.x, inner.y - world.y), (12.0, 6.0));
        assert_eq!(rt.store.content_inset(field).left, 4.0);
    }
}
