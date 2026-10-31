//! A glass box: a `BlurView` showing the standard glass, a `backdrop` of its
//! own reading state, and any node made glass, from the dev runtime and the
//! release package alike.

mod support;

use support::Rt;
use viso_ui::{Backdrop, NodeId, Primitive, Rgba, Srgb};

const PANELS: &str = r##"
export component Panels {
    state deep = false;
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { deep = !deep; } }
            BlurView { width: 100dp; height: 40dp; corner_radius: 12dp; }
            BlurView {
                width: 100dp;
                height: 40dp;
                backdrop: Option::Some(Backdrop {
                    blur: if deep { 60dp } else { 10dp },
                    saturation: 1.2,
                    tint: #1c1c1e80,
                });
            }
            Column { width: 10dp; height: 10dp; backdrop: Option::Some(Backdrop {}); }
            Column { width: 10dp; height: 10dp; }
        }
    }
}
"##;

fn rgba(srgb: u32) -> Rgba {
    Srgb::from_rgba32(srgb).into_linear_straight()
}

/// The root's children: the toggle, the standard glass, the tuned glass, the
/// glass column and the plain one.
fn nodes(rt: &Rt) -> [NodeId; 5] {
    let root = rt.root.expect("mounted");
    rt.children(root).try_into().expect("five children")
}

#[test]
fn glass_boxes_frost_what_lies_behind_them_in_every_target() {
    for mut rt in [Rt::mount(PANELS), Rt::packaged(PANELS)] {
        assert_eq!(rt.fault(), None);
        rt.relayout();
        let [_, standard, tuned, column, plain] = nodes(&rt);
        assert_eq!(rt.store.backdrop(standard), Some(Backdrop::GLASS));
        let tuned_glass = |blur| Backdrop {
            blur,
            saturation: 1.2,
            tint: rgba(0x1c1c_1e80),
            ..Backdrop::GLASS
        };
        assert_eq!(rt.store.backdrop(tuned), Some(tuned_glass(10.0)));
        assert_eq!(
            rt.store.backdrop(column),
            Some(Backdrop::GLASS),
            "the record's defaults are the standard glass"
        );
        assert_eq!(rt.store.backdrop(plain), None);

        rt.click(0);
        assert_eq!(rt.store.backdrop(tuned), Some(tuned_glass(60.0)));

        let root = rt.root.expect("mounted");
        let mut out = Vec::new();
        viso_ui::paint::paint_tree(&rt.store, root, &mut out);
        let surfaces: Vec<_> = out
            .iter()
            .filter_map(|p| match p {
                Primitive::Frosted(m) => Some(m),
                _ => None,
            })
            .collect();
        assert_eq!(surfaces.len(), 3, "one surface a glass box");
        assert_eq!(surfaces[0].rect, rt.store.world(standard));
        assert_eq!(surfaces[0].radius.left_top, 12.0, "the box's corners");
        assert_eq!(surfaces[1].backdrop_sigma, 30.0, "half the blur radius");
        assert_eq!(rt.fault(), None);
    }
}

const LIQUID: &str = r##"
export component Liquid {
    view {
        Column {
            width: 120dp;
            height: 48dp;
            corner_radius: 24dp;
            backdrop: Option::Some(Backdrop {
                blur: 4dp,
                tint: #ffffff20,
                luminosity: 0.5,
                refraction: 12dp,
                bevel: 16dp,
                dispersion: 0.2,
                highlight: 0.8,
                light: 0deg,
            });
        }
    }
}
"##;

#[test]
fn liquid_glass_bends_glints_and_evens_its_light_in_every_target() {
    for mut rt in [Rt::mount(LIQUID), Rt::packaged(LIQUID)] {
        assert_eq!(rt.fault(), None);
        rt.relayout();
        let root = rt.root.expect("mounted");
        let glass = rt.store.backdrop(root).expect("glass");
        assert_eq!(
            (glass.refraction, glass.bevel, glass.dispersion),
            (12.0, 16.0, 0.2)
        );
        assert_eq!((glass.highlight, glass.light), (0.8, 0.0));
        assert_eq!(glass.luminosity, 0.5);

        let mut out = Vec::new();
        viso_ui::paint::paint_tree(&rt.store, root, &mut out);
        let Some(Primitive::Frosted(surface)) = out.first() else {
            panic!("the glass first: {out:?}");
        };
        assert_eq!(surface.lens.refraction, 12.0);
        assert_eq!(surface.lens.bevel, 16.0);
        assert_eq!(surface.glint.strength, 0.8);
        assert!(surface.glint.width > 0.0, "a thin line");
        assert_eq!(surface.luminosity.amount, 0.5);
        // Its light gives way to the tint's: white.
        assert!((surface.luminosity.luminance - 1.0).abs() < 1e-4);
    }
}

#[test]
fn the_standard_glass_is_flat_and_unlit() {
    let glass = Backdrop::GLASS;
    assert_eq!(
        (glass.refraction, glass.highlight, glass.luminosity),
        (0.0, 0.0, 0.0)
    );
}
